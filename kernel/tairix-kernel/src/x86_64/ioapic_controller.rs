//! Production [`IrqController`] implementation backed by the x86_64
//! IO-APIC.
//!
//! Stage 4.D Item 2-tail.2. The kernel binary builds one
//! [`IoApicController`] per boot during its post-MADT wiring phase
//! (see `crate::x86_64::boot::try_boot`, bare-metal only). The controller owns every
//! IO-APIC the firmware advertises through MADT and exposes a single
//! [`IrqController::mask`] method that the kernel-neutral
//! [`tairix_kernel_irq::IrqTable::fire`] path invokes *before* it
//! sets a wait-handle's `ready` flag — the mask-before-wake
//! invariant documented in `docs/src/security/irq.md`.
//!
//! # Mask-before-wake
//!
//! [`tairix_kernel_irq::IrqTable::fire`]'s ordering contract
//! requires the controller's `mask` call to complete (and be
//! globally observable) before the `ready` flag flips. This
//! controller honours the contract by:
//!
//! 1. Re-writing the IO-APIC redirection entry's low half through the
//!    audited [`IoApic::write_redirection_low`] driver, which uses
//!    volatile MMIO (`VolatileIoApicMmio`), so the mask bit lands
//!    on the CPU's write-combining store buffer before the function
//!    returns.
//! 2. Emitting a [`core::sync::atomic::fence`] with
//!    [`Ordering::SeqCst`] after the mask write so a subsequent
//!    waker observing `ready = true` is guaranteed to also observe
//!    the masked line.
//!
//! Step 2 is the load-bearing barrier: the IO-APIC's MMIO write is
//! globally ordered with respect to memory operations only after
//! a memory fence on Intel/AMD x86_64. The
//! `ioapic_controller_mask_before_wake_ordering` host test pins the
//! ordering against a `RecordingMmio` mock that captures every MMIO
//! write in observed order.
//!
//! # Multi-IO-APIC layout
//!
//! A modern x86_64 platform can advertise more than one IO-APIC.
//! Each `MadtEntry::IoApic` carries an `address`, an `id`, and a
//! `gsi_base` — the GSI range this IO-APIC owns is
//! `gsi_base .. gsi_base + max_redirection_entry + 1`. The
//! controller stores one block per IO-APIC the MADT names and routes the
//! kernel-neutral "line" parameter (a GSI) by linear scan.

extern crate alloc;
use alloc::vec::Vec;

use core::sync::atomic::{fence, Ordering};

use tairix_arch_x86_64::apic::{
    compatibility_entry, IoApic, IoApicMmio, PinWiring, IOAPIC_EOI_VERSION, REDIRECTION_MASKED,
};
use tairix_arch_x86_64::irq::Routing;
use tairix_arch_x86_64::msr::halves;
use tairix_kernel_core::iommu::InterruptTarget;
use tairix_kernel_irq::{ActivationError, IrqController, MaskError};
use tairix_sync::{InterruptControl, IrqSafeSpinLock, SpinLock};

use crate::x86_64::vectors::VectorPool;

/// Interrupt remapping as a pin's activation meets it.
pub trait PinRemapping {
    /// The redirection entry raising `target` from the IO-APIC whose APIC id
    /// is `ioapic`, its remapping entry made for the boot; [`None`] while
    /// remapping is off, when a pin raises its vector in compatibility
    /// format.
    ///
    /// # Errors
    ///
    /// [`ActivationError::Unroutable`] where remapping is on and makes the pin no
    /// entry.
    fn entry(&self, ioapic: u8, target: InterruptTarget) -> Result<Option<u64>, ActivationError>;
}

/// Set-once typed publication of the production
/// `IoApicController<VolatileIoApicMmio>` constructed by
/// [`crate::x86_64::boot::try_boot`].
///
/// Bare-metal only because the `VolatileIoApicMmio` type used in the
/// slot's `'static` reference is itself gated to `target_os = "none"`
/// — host tests have no IO-APIC MMIO window to publish.
///
/// Published alongside the [`crate::x86_64::arch_wrapper`] controller slot
/// (which carries the same controller as a `dyn IrqController` trait
/// object). The typed slot exposes [`IoApicController::unmask`] and
/// [`IoApicController::read_pin_low`], which have no analogue on every port:
/// the `irq_qemu_x86_64` and `ps2_input_qemu_x86_64` verticals unmask a
/// activated pin through it and re-read its mask after
/// [`tairix_kernel_irq::IrqTable::fire`] runs.
#[cfg(freestanding)]
static PUBLISHED_TYPED: tairix_sync::once::OnceCell<
    &'static IoApicController<tairix_arch_x86_64::apic::VolatileIoApicMmio>,
> = tairix_sync::once::OnceCell::new();

/// Publish the production controller into [`PUBLISHED_TYPED`].
///
/// Called once during [`crate::x86_64::boot::try_boot`]'s
/// `discover_and_program_io_apics` step. A second publish is silently
/// ignored — production code reaches this path exactly once.
#[cfg(freestanding)]
pub fn publish_typed(
    controller: &'static IoApicController<tairix_arch_x86_64::apic::VolatileIoApicMmio>,
) {
    let _ = PUBLISHED_TYPED.set(controller);
}

/// Read the controller published into [`PUBLISHED_TYPED`].
///
/// Returns `None` until [`publish_typed`] has run. The
/// `tests/integration/irq_qemu_x86_64` integration test calls this
/// from its `AuditEvent::BootCompleted` observer to obtain typed
/// access to the production [`IoApicController`].
#[cfg(freestanding)]
#[must_use]
pub fn published_typed(
) -> Option<&'static IoApicController<tairix_arch_x86_64::apic::VolatileIoApicMmio>> {
    match PUBLISHED_TYPED.get() {
        Ok(slot) => slot.copied(),
        Err(_) => None,
    }
}

/// One programmed pin: its entry with the mask clear, whether it is masked,
/// and the APIC id it delivers to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PinSettings {
    entry: u64,
    masked: bool,
    destination: u32,
}

impl PinSettings {
    const fn written(self) -> u64 {
        if self.masked {
            self.entry | REDIRECTION_MASKED
        } else {
            self.entry
        }
    }
}

/// One IO-APIC the MADT names: its id, its first global system interrupt,
/// the controller, and how each of its pins is wired.
pub struct IoApicBlock<M: IoApicMmio> {
    /// Its APIC id.
    pub id: u8,
    /// The global system interrupt of its pin 0.
    pub gsi_base: u32,
    /// The controller.
    pub ioapic: IoApic<M>,
    /// Each pin's wiring, one per pin.
    pub wiring: Vec<PinWiring>,
}

impl<M: IoApicMmio> IoApicBlock<M> {
    /// How many pins it has.
    #[must_use]
    pub fn pins(&self) -> u32 {
        u32::try_from(self.wiring.len()).unwrap_or(u32::MAX)
    }
}

/// Whether an IO-APIC with `pins` pins from global system interrupt
/// `gsi_base` can take them: all below `ceiling`, where the line space
/// stops being GSIs, and none held by one of `blocks`, so no GSI reaches
/// two pins.
#[must_use]
pub fn gsis_free<M: IoApicMmio>(
    blocks: &[IoApicBlock<M>],
    gsi_base: u32,
    pins: u32,
    ceiling: u32,
) -> bool {
    let Some(end) = gsi_base.checked_add(pins) else {
        return false;
    };
    end <= ceiling
        && blocks.iter().all(|block| {
            end <= block.gsi_base || block.gsi_base.saturating_add(block.pins()) <= gsi_base
        })
}

/// What a block's lock masks while held: the interrupt path masks a pin
/// under the same lock task context re-arms one under, so an interrupt taken
/// while it is held would spin on it for ever.
#[cfg(freestanding)]
type BlockIrqs = tairix_arch_x86_64::irqmask::RflagsIrqControl;
#[cfg(not(freestanding))]
type BlockIrqs = tairix_sync::NopInterruptControl;

struct Block<M: IoApicMmio + Send, I: InterruptControl> {
    id: u8,
    gsi_base: u32,
    pin_count: u32,
    /// It has an EOI register.
    eoi_register: bool,
    inner: IrqSafeSpinLock<BlockInner<M>, I>,
}

struct BlockInner<M: IoApicMmio + Send> {
    ioapic: IoApic<M>,
    wiring: Vec<PinWiring>,
    pin_cache: Vec<Option<PinSettings>>,
}

/// One programmed pin, as interrupt remapping takes it over.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ProgrammedPin {
    /// Its global system interrupt.
    pub gsi: u32,
    /// The APIC id of the IO-APIC it is on.
    pub ioapic: u8,
    /// The vector it delivers.
    pub vector: u8,
    /// The APIC id of the CPU it delivers to.
    pub destination: u32,
    /// Whether it is level-triggered.
    pub level: bool,
}

/// The production x86_64 [`IrqController`]: every IO-APIC the firmware
/// advertises, addressed by global system interrupt.
pub struct IoApicController<M: IoApicMmio + Send + 'static, I: InterruptControl = BlockIrqs> {
    blocks: Vec<Block<M, I>>,
    /// Held across an activation, so two never give one pin two vectors; a block's
    /// own lock is held only to read and write it, never across a remapping
    /// unit's work.
    activating: SpinLock<()>,
}

// SAFETY: every block's state is reached only under its lock, and `IoApic<M>`
// carries no thread affinity, so `M: Send` is all sharing needs.
unsafe impl<M: IoApicMmio + Send + 'static, I: InterruptControl> Sync for IoApicController<M, I> {}

/// Why a pin could not be programmed.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProgramError {
    /// No block owns the global system interrupt, or it was never
    /// programmed.
    GsiOutOfRange,
}

impl<M: IoApicMmio + Send + 'static, I: InterruptControl> IoApicController<M, I> {
    /// The controller over `blocks`, or [`None`] where its bookkeeping
    /// cannot be had.
    #[must_use]
    pub fn new(blocks: Vec<IoApicBlock<M>>) -> Option<Self> {
        let mut built = Vec::new();
        built.try_reserve_exact(blocks.len()).ok()?;
        for mut block in blocks {
            let pins = block.wiring.len();
            let mut pin_cache = Vec::new();
            pin_cache.try_reserve_exact(pins).ok()?;
            pin_cache.resize(pins, None);
            built.push(Block {
                id: block.id,
                gsi_base: block.gsi_base,
                pin_count: u32::try_from(pins).ok()?,
                eoi_register: block.ioapic.version() >= IOAPIC_EOI_VERSION,
                inner: IrqSafeSpinLock::new(BlockInner {
                    ioapic: block.ioapic,
                    wiring: block.wiring,
                    pin_cache,
                }),
            });
        }
        Some(Self {
            blocks: built,
            activating: SpinLock::new(()),
        })
    }

    /// Mask every pin no activation has programmed: firmware may have left
    /// one delivering, and none does until its activation gives it a vector.
    pub fn quiesce(&self) {
        for block in &self.blocks {
            let mut inner = block.inner.lock();
            for pin in 0..block.pin_count {
                let slot = pin as usize;
                if inner.pin_cache[slot].is_none() {
                    let entry = REDIRECTION_MASKED | inner.wiring[slot].bits();
                    // As `write_pin`.
                    #[allow(clippy::cast_possible_truncation)]
                    inner.ioapic.write_redirection_entry(pin as u8, entry);
                }
            }
        }
    }

    /// Give `gsi` a vector of its own from `vectors`, recorded in `routing`,
    /// and program it masked to raise it at the pool's CPU: through the entry
    /// `remapping` makes once remapping is on, else in compatibility format.
    /// A pin active already keeps the vector it has, for the boot: nothing
    /// proves an interrupt it raised has left every CPU, so its vector is
    /// never handed on.
    ///
    /// # Errors
    ///
    /// [`ActivationError::OutOfRange`] for a GSI no block owns,
    /// [`ActivationError::Exhausted`] with no vector free, and
    /// [`ActivationError::Unroutable`] where remapping makes the pin no entry or,
    /// remapping off, compatibility format cannot name the pool's CPU. The pin
    /// is then left inactive.
    pub fn activate_pin(
        &self,
        gsi: u32,
        vectors: &VectorPool,
        routing: &Routing,
        remapping: &dyn PinRemapping,
    ) -> Result<(), ActivationError> {
        let (idx, pin) = self.locate(gsi).ok_or(ActivationError::OutOfRange)?;
        let block = &self.blocks[idx];
        let _activating = self.activating.lock();
        let wiring = {
            let inner = block.inner.lock();
            if inner.pin_cache[pin as usize].is_some() {
                return Ok(());
            }
            inner.wiring[pin as usize]
        };
        let destination = vectors.destination();
        let vector = vectors.claim().ok_or(ActivationError::Exhausted)?;
        // A free vector's route is always unmapped, since a vector is given
        // back only after its route goes; one found otherwise stays claimed,
        // never handed on.
        routing
            .install(gsi, vector)
            .map_err(|_| ActivationError::Exhausted)?;
        let target = InterruptTarget {
            vector,
            destination,
            level: wiring.level,
        };
        let refuse = |refused| {
            routing.remove(gsi, vector);
            vectors.release(vector);
            refused
        };
        // Nothing past the remapping entry can fail, so none is ever made for
        // a pin left inactive.
        let programmed =
            if let Some(redirection) = remapping.entry(block.id, target).map_err(refuse)? {
                self.write_pin(
                    gsi,
                    |wiring, _| (remapped(redirection, wiring), destination),
                    Some(true),
                )
            } else {
                let destination = tairix_arch_x86_64::apic::xapic_id(destination)
                    .ok_or_else(|| refuse(ActivationError::Unroutable))?;
                self.program_pin(gsi, vector, destination)
            };
        programmed.map_err(|_| refuse(ActivationError::OutOfRange))
    }

    /// Program `gsi` masked to deliver `vector` to the APIC at `dest_apic_id`
    /// in compatibility format, wired as the firmware says: how a pin raises
    /// its vector while remapping is off.
    fn program_pin(&self, gsi: u32, vector: u8, dest_apic_id: u8) -> Result<(), ProgramError> {
        self.write_pin(
            gsi,
            |wiring, _| {
                (
                    compatibility_entry(vector, dest_apic_id, wiring),
                    u32::from(dest_apic_id),
                )
            },
            Some(true),
        )
    }

    /// Re-program `gsi` to raise the remapping entry `redirection` names,
    /// keeping its wiring and its mask: the entry's vector and trigger are
    /// the pin's own.
    ///
    /// # Errors
    ///
    /// [`ProgramError::GsiOutOfRange`] for a GSI no block owns or one never
    /// programmed.
    pub fn remap_pin(&self, gsi: u32, redirection: u64) -> Result<(), ProgramError> {
        self.write_pin(
            gsi,
            |wiring, current| {
                (
                    remapped(redirection, wiring),
                    current.map_or(0, |settings| settings.destination),
                )
            },
            None,
        )
    }

    /// Write `gsi`'s entry, and the APIC id it delivers to, as `entry` makes
    /// them from the pin's wiring and its current settings, masked as
    /// `masked` says or as it was.
    fn write_pin(
        &self,
        gsi: u32,
        entry: impl FnOnce(PinWiring, Option<PinSettings>) -> (u64, u32),
        masked: Option<bool>,
    ) -> Result<(), ProgramError> {
        let (idx, pin) = self.locate(gsi).ok_or(ProgramError::GsiOutOfRange)?;
        let mut inner = self.blocks[idx].inner.lock();
        let slot = pin as usize;
        let current = inner.pin_cache[slot];
        let masked = masked
            .or_else(|| current.map(|settings| settings.masked))
            .ok_or(ProgramError::GsiOutOfRange)?;
        let (entry, destination) = entry(inner.wiring[slot], current);
        let settings = PinSettings {
            entry,
            masked,
            destination,
        };
        // `pin < pin_count`, an IO-APIC's redirection count, which fits a
        // `u8`.
        #[allow(clippy::cast_possible_truncation)]
        inner
            .ioapic
            .write_redirection_entry(pin as u8, settings.written());
        inner.pin_cache[slot] = Some(settings);
        Ok(())
    }

    /// Mask or unmask `gsi`, writing only the half of its entry that holds
    /// the mask.
    fn set_masked(&self, gsi: u32, masked: bool) -> Result<(), ProgramError> {
        let (idx, pin) = self.locate(gsi).ok_or(ProgramError::GsiOutOfRange)?;
        let mut inner = self.blocks[idx].inner.lock();
        let slot = pin as usize;
        let mut settings = inner.pin_cache[slot].ok_or(ProgramError::GsiOutOfRange)?;
        if settings.masked == masked {
            return Ok(());
        }
        settings.masked = masked;
        // As `write_pin`.
        #[allow(clippy::cast_possible_truncation)]
        inner
            .ioapic
            .write_redirection_low(pin as u8, halves(settings.written()).0);
        inner.pin_cache[slot] = Some(settings);
        Ok(())
    }

    /// Unmask `gsi` after a completion that masked it. A level-triggered pin
    /// first has its remote IRR cleared: an interrupt it raised that reached
    /// its CPU edge-triggered, as one an AMD-Vi unit remaps does, sends no
    /// end of interrupt back to it, and the pin would never raise another.
    /// An unmasked pin is left as it is, so no end of interrupt lands while
    /// one may be in service.
    fn rearm_pin(&self, gsi: u32) -> Result<(), ProgramError> {
        let (idx, pin) = self.locate(gsi).ok_or(ProgramError::GsiOutOfRange)?;
        let block = &self.blocks[idx];
        let mut inner = block.inner.lock();
        let slot = pin as usize;
        let mut settings = inner.pin_cache[slot].ok_or(ProgramError::GsiOutOfRange)?;
        if !settings.masked {
            return Ok(());
        }
        // `pin < pin_count`, an IO-APIC's redirection count, which fits a
        // `u8`.
        #[allow(clippy::cast_possible_truncation)]
        let pin = pin as u8;
        if inner.wiring[slot].level {
            if block.eoi_register {
                inner
                    .ioapic
                    .end_of_interrupt(settings.entry.to_le_bytes()[0]);
            } else {
                // Taking the pin through edge triggering clears it.
                let masked = settings.entry | REDIRECTION_MASKED;
                let edge = masked
                    & !PinWiring {
                        level: true,
                        active_low: false,
                    }
                    .bits();
                inner.ioapic.write_redirection_low(pin, halves(edge).0);
                inner.ioapic.write_redirection_low(pin, halves(masked).0);
            }
        }
        settings.masked = false;
        inner
            .ioapic
            .write_redirection_low(pin, halves(settings.written()).0);
        inner.pin_cache[slot] = Some(settings);
        Ok(())
    }

    /// Every pin programmed so far, block by block.
    pub fn programmed(&self, visit: &mut dyn FnMut(ProgrammedPin)) {
        for block in &self.blocks {
            let inner = block.inner.lock();
            for (pin, settings) in inner.pin_cache.iter().enumerate() {
                let Some(settings) = settings else {
                    continue;
                };
                let offset = u32::try_from(pin).unwrap_or(u32::MAX);
                visit(ProgrammedPin {
                    gsi: block.gsi_base + offset,
                    ioapic: block.id,
                    vector: settings.entry.to_le_bytes()[0],
                    destination: settings.destination,
                    level: inner.wiring[pin].level,
                });
            }
        }
    }

    fn locate(&self, gsi: u32) -> Option<(usize, u32)> {
        self.blocks.iter().enumerate().find_map(|(idx, block)| {
            let pin = gsi.checked_sub(block.gsi_base)?;
            (pin < block.pin_count).then_some((idx, pin))
        })
    }

    /// IO-APICs the controller owns.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// The APIC id of every IO-APIC the controller owns.
    pub fn ioapic_ids(&self) -> impl Iterator<Item = u8> + '_ {
        self.blocks.iter().map(|block| block.id)
    }

    /// The highest GSI a pin owns, or [`None`] with no pins.
    #[must_use]
    pub fn last_gsi(&self) -> Option<u32> {
        self.blocks
            .iter()
            .filter_map(|block| block.gsi_base.checked_add(block.pin_count.checked_sub(1)?))
            .max()
    }

    /// Unmask `gsi`.
    ///
    /// # Errors
    ///
    /// [`ProgramError::GsiOutOfRange`] for a GSI no block owns or one never
    /// programmed.
    pub fn unmask(&self, gsi: u32) -> Result<(), ProgramError> {
        self.set_masked(gsi, false)
    }

    /// The low half of `gsi`'s redirection entry as the IO-APIC reads it, or
    /// [`None`] for a GSI no block owns.
    #[must_use]
    pub fn read_pin_low(&self, gsi: u32) -> Option<u32> {
        let (idx, pin) = self.locate(gsi)?;
        let mut inner = self.blocks[idx].inner.lock();
        // As `write_pin`.
        #[allow(clippy::cast_possible_truncation)]
        Some(inner.ioapic.read_redirection_entry_low(pin as u8))
    }
}

/// A pin's entry raising the remapping entry `redirection` names, wired as
/// `wiring` says and unmasked.
fn remapped(redirection: u64, wiring: PinWiring) -> u64 {
    let wiring_bits = PinWiring {
        level: true,
        active_low: true,
    }
    .bits();
    (redirection & !wiring_bits & !REDIRECTION_MASKED) | wiring.bits()
}

impl<M: IoApicMmio + Send + 'static, I: InterruptControl> IrqController for IoApicController<M, I> {
    fn mask(&self, line: u32) -> Result<(), MaskError> {
        self.set_masked(line, true)
            .map_err(|_| MaskError::OutOfRange)?;
        // The lock's release orders the mask write before this fence, which
        // pairs with the SeqCst load `try_wait_step` performs on `ready`:
        // every CPU that sees `ready = true` sees the masked entry.
        fence(Ordering::SeqCst);
        Ok(())
    }

    /// [`tairix_kernel_irq::IrqTable::fire`] masks a line before its waiter
    /// sees the wake, and a user-space driver cannot reach the IO-APIC, so its
    /// `irq_wait` re-arms the line here once it has drained the completion.
    fn rearm(&self, line: u32) -> Result<(), MaskError> {
        self.rearm_pin(line).map_err(|_| MaskError::OutOfRange)
    }
}

/// The Arch HAL view of the IO-APIC controller
/// (`plans/WIRING.md` Stage W3).
///
/// `IoApicController` already implements the consumer-side
/// [`tairix_kernel_irq::IrqController`] the IRQ table calls during a
/// wake; this impl additionally exposes it through the HAL
/// [`tairix_arch_api::IrqController`] so the architecture-neutral kernel
/// can name one interrupt-controller surface across every port. Both `mask` and `unmask` delegate to the existing
/// IO-APIC logic; the only error the IO-APIC controller produces is
/// "no such addressable line", which maps to
/// [`tairix_arch_api::IrqControlError::OutOfRange`].
///
/// x86_64 is a **vectored** architecture — the IDT vector identifies the
/// source and end-of-interrupt is a single LAPIC write that names no
/// line — so it deliberately does **not** implement
/// [`tairix_arch_api::InterruptEntry`]; there is no claim register to
/// model, and faking one would be a fake primitive.
impl<M: IoApicMmio + Send + 'static, I: InterruptControl> tairix_arch_api::IrqController
    for IoApicController<M, I>
{
    fn mask(&self, line: u32) -> Result<(), tairix_arch_api::IrqControlError> {
        <Self as IrqController>::mask(self, line)
            .map_err(|_| tairix_arch_api::IrqControlError::OutOfRange)
    }

    fn unmask(&self, line: u32) -> Result<(), tairix_arch_api::IrqControlError> {
        self.unmask(line)
            .map_err(|_| tairix_arch_api::IrqControlError::OutOfRange)
    }
}

#[cfg(test)]
mod tests {
    /// The controller as boot builds it, its blocks' locks masking nothing on
    /// the host.
    type Controller<M> = IoApicController<M, BlockIrqs>;

    use super::*;
    use std::sync::{Arc, Mutex};
    use std::vec::Vec as StdVec;
    use tairix_arch_x86_64::apic::IoApicMmio;
    use tairix_arch_x86_64::irq::EXTERNAL_VECTOR_COUNT;

    /// Recording mock IO-APIC MMIO. Captures every write in a
    /// shared log so tests can assert the order of operations, and
    /// surfaces the last value written to a register on subsequent
    /// reads so [`IoApicController::read_pin_low`] tests can observe
    /// the redirection-entry state through the same MMIO seam the
    /// production driver uses.
    #[derive(Clone)]
    struct RecordingMmio {
        log: Arc<Mutex<StdVec<(u8, u32)>>>,
        last_writes: Arc<Mutex<std::collections::HashMap<u8, u32>>>,
        /// Each end of interrupt, with how many writes preceded it.
        eois: Arc<Mutex<StdVec<(usize, u8)>>>,
        /// Register accesses made while [`Masking`] held no CPU masked.
        unmasked: Arc<core::sync::atomic::AtomicUsize>,
    }

    std::thread_local! {
        /// How deep this thread's [`Masking`] nests.
        static MASKED: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
    }

    /// Interrupt control counting how deep a thread has masked, so a test can
    /// see whether a register was reached with interrupts masked.
    struct Masking;

    // SAFETY: a test double: it masks nothing, and its state is the
    // thread-local depth `restore` puts back.
    unsafe impl InterruptControl for Masking {
        type State = tairix_sync::NopIrqState;

        fn disable() -> Self::State {
            MASKED.with(|depth| depth.set(depth.get() + 1));
            tairix_sync::NopIrqState::default()
        }

        unsafe fn restore(_: Self::State) {
            MASKED.with(|depth| depth.set(depth.get() - 1));
        }
    }

    impl RecordingMmio {
        fn new() -> Self {
            Self {
                log: Arc::new(Mutex::new(StdVec::new())),
                last_writes: Arc::new(Mutex::new(std::collections::HashMap::new())),
                eois: Arc::new(Mutex::new(StdVec::new())),
                unmasked: Arc::new(core::sync::atomic::AtomicUsize::new(0)),
            }
        }
        /// An IO-APIC reporting `version`.
        fn versioned(version: u8) -> Self {
            let mmio = Self::new();
            mmio.last_writes
                .lock()
                .unwrap()
                .insert(0x01, 0x0017_0000 | u32::from(version));
            mmio
        }
        fn snapshot(&self) -> StdVec<(u8, u32)> {
            self.log.lock().unwrap().clone()
        }
        fn eois(&self) -> StdVec<(usize, u8)> {
            self.eois.lock().unwrap().clone()
        }
        fn note_masking(&self) {
            if MASKED.with(core::cell::Cell::get) == 0 {
                self.unmasked
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    impl IoApicMmio for RecordingMmio {
        fn read(&mut self, reg: u8) -> u32 {
            self.note_masking();
            // Surface the last write to `reg` so `read_pin_low`
            // tests observe the mask state set by `program_pin` /
            // `IrqController::mask`. The IoApic driver reads
            // IOAPICID (reg 0x00) and IOAPICVER (reg 0x01) during
            // its metadata queries; both default to zero, which is
            // the correct sentinel here.
            *self.last_writes.lock().unwrap().get(&reg).unwrap_or(&0)
        }
        fn write(&mut self, reg: u8, value: u32) {
            self.note_masking();
            self.log.lock().unwrap().push((reg, value));
            self.last_writes.lock().unwrap().insert(reg, value);
        }
        fn end_of_interrupt(&mut self, vector: u8) {
            let at = self.log.lock().unwrap().len();
            self.eois.lock().unwrap().push((at, vector));
        }
    }

    fn block(
        id: u8,
        gsi_base: u32,
        pin_count: u32,
        mmio: &RecordingMmio,
    ) -> IoApicBlock<RecordingMmio> {
        IoApicBlock {
            id,
            gsi_base,
            ioapic: IoApic::new(mmio.clone()),
            wiring: (0..pin_count)
                .map(|pin| PinWiring::of(gsi_base + pin, None))
                .collect(),
        }
    }

    fn fresh_controller(
        gsi_base: u32,
        pin_count: u32,
    ) -> (IoApicController<RecordingMmio>, RecordingMmio) {
        let mmio = RecordingMmio::new();
        let controller =
            Controller::new(alloc::vec![block(0, gsi_base, pin_count, &mmio)]).unwrap();
        (controller, mmio)
    }

    /// The interrupt path masks a pin under the lock task context re-arms one
    /// under, so every register is reached with this CPU's interrupts masked:
    /// one taken while the lock is held would spin on it for ever.
    #[test]
    fn every_register_is_reached_with_interrupts_masked() {
        use core::sync::atomic::Ordering;
        let mmio = RecordingMmio::new();
        let mut wired = block(0, 0, 24, &mmio);
        wired.wiring[9] = PinWiring::of(9, Some(0b1111));
        let controller = IoApicController::<_, Masking>::new(alloc::vec![wired]).unwrap();
        mmio.unmasked.store(0, Ordering::Relaxed);
        controller.program_pin(9, 0x39, 0).unwrap();
        IrqController::mask(&controller, 9).unwrap();
        IrqController::rearm(&controller, 9).unwrap();
        controller.unmask(9).unwrap();
        let _ = controller.read_pin_low(9);
        assert_eq!(mmio.unmasked.load(Ordering::Relaxed), 0);
        assert!(!mmio.snapshot().is_empty(), "the pin was reached");
    }

    /// A pin is programmed as the firmware wired it: a level-triggered,
    /// active-low line is never delivered as an edge, whose second assertion
    /// would be lost.
    #[test]
    fn a_pin_is_programmed_with_its_firmware_wiring() {
        let mmio = RecordingMmio::new();
        let mut wired = block(0, 0, 24, &mmio);
        wired.wiring[9] = PinWiring::of(9, Some(0b1111));
        let controller = Controller::new(alloc::vec![wired]).unwrap();
        controller.program_pin(9, 0x39, 0).unwrap();
        controller.program_pin(4, 0x34, 0).unwrap();
        let low = |gsi| controller.read_pin_low(gsi).unwrap();
        assert_eq!(
            low(9) & (1 << 15 | 1 << 13),
            1 << 15 | 1 << 13,
            "level, active low"
        );
        assert_eq!(
            low(4) & (1 << 15 | 1 << 13),
            0,
            "an ISA line: edge, active high"
        );
    }

    /// Remapping replaces a pin's whole entry with the remappable one, its
    /// wiring and mask kept, and masking it afterwards keeps the format.
    #[test]
    fn a_remapped_pin_keeps_its_wiring_and_mask() {
        let mmio = RecordingMmio::new();
        let mut wired = block(2, 0, 24, &mmio);
        wired.wiring[9] = PinWiring::of(9, Some(0b1111));
        let controller = Controller::new(alloc::vec![wired]).unwrap();
        controller.program_pin(9, 0x39, 3).unwrap();
        let mut programmed = StdVec::new();
        controller.programmed(&mut |pin| programmed.push(pin));
        let pin = ProgrammedPin {
            gsi: 9,
            ioapic: 2,
            vector: 0x39,
            destination: 3,
            level: true,
        };
        assert_eq!(programmed, [pin]);
        let remappable = (5 << 49) | (1 << 48) | 0x39;
        controller.remap_pin(9, remappable).unwrap();
        programmed.clear();
        controller.programmed(&mut |pin| programmed.push(pin));
        assert_eq!(programmed, [pin], "the remapped pin still names its target");
        let entry = |gsi| {
            let low = u64::from(controller.read_pin_low(gsi).unwrap());
            let high = u64::from(*mmio.last_writes.lock().unwrap().get(&0x23).unwrap_or(&0));
            low | high << 32
        };
        let wiring = 1 << 15 | 1 << 13;
        assert_eq!(
            entry(9),
            remappable | wiring | REDIRECTION_MASKED,
            "still masked"
        );
        controller.unmask(9).unwrap();
        assert_eq!(entry(9), remappable | wiring);
        IrqController::mask(&controller, 9).unwrap();
        assert_eq!(entry(9), remappable | wiring | REDIRECTION_MASKED);
        assert_eq!(
            controller.remap_pin(10, remappable),
            Err(ProgramError::GsiOutOfRange),
            "a pin never programmed"
        );
    }

    #[test]
    fn locate_returns_none_for_gsi_above_range() {
        let (controller, _mmio) = fresh_controller(0, 24);
        // Borrow internal helper for the test.
        assert!(controller.locate(24).is_none());
        assert!(controller.locate(u32::MAX).is_none());
        assert!(controller.locate(0).is_some());
        assert!(controller.locate(23).is_some());
    }

    #[test]
    fn program_pin_records_settings_and_writes_low_then_high() {
        let (controller, mmio) = fresh_controller(0, 24);
        controller.program_pin(7, 0x30, 0xAB).expect("program");
        let writes = mmio.snapshot();
        // The IoApic driver writes low (reg 0x10 + 2*pin) then high
        // (reg 0x10 + 2*pin + 1). For pin 7 that's regs 0x1E and 0x1F.
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].0, 0x10 + 14);
        // Low: vector | (masked ? mask_bit : 0).
        assert_eq!(writes[0].1 & 0xFF, 0x30);
        assert!(writes[0].1 & (1 << 16) != 0);
        assert_eq!(writes[1].0, 0x10 + 15);
        // High: dest_apic_id in bits 24..32.
        assert_eq!(writes[1].1, (0xABu32) << 24);
    }

    #[test]
    fn program_pin_rejects_gsi_out_of_range() {
        let (controller, _mmio) = fresh_controller(0, 24);
        assert_eq!(
            controller.program_pin(24, 0x30, 0xAB),
            Err(ProgramError::GsiOutOfRange),
        );
    }

    #[test]
    fn mask_rewrites_redirection_entry_with_masked_bit_set() {
        let (controller, mmio) = fresh_controller(0, 24);
        // Program pin 7 initially *unmasked*.
        controller.program_pin(7, 0x30, 0xAB).expect("program");
        controller.unmask(7).expect("unmask");
        // Clear the install-time writes from the log so the assertions
        // below cover only the `mask` call's writes.
        mmio.log.lock().unwrap().clear();

        IrqController::mask(&controller, 7).expect("mask succeeds");

        let writes = mmio.snapshot();
        assert_eq!(writes.len(), 1, "a mask writes the half holding it");
        assert_eq!(writes[0].0, 0x10 + 14);
        assert_eq!(writes[0].1 & 0xFF, 0x30, "vector preserved");
        assert!(writes[0].1 & (1 << 16) != 0, "mask bit set in low half");
        assert_eq!(controller.read_pin_low(7).map(|low| low & 0xFF), Some(0x30));
    }

    #[test]
    fn mask_returns_out_of_range_for_unprogrammed_pin() {
        let (controller, _mmio) = fresh_controller(0, 24);
        // Pin 7 was never programmed, so the cache slot is `None`.
        // The mask path fail-closes with `OutOfRange` (the
        // controller refuses to mask a line that was never
        // programmed at install time).
        assert_eq!(
            IrqController::mask(&controller, 7),
            Err(MaskError::OutOfRange),
        );
    }

    #[test]
    fn mask_returns_out_of_range_for_gsi_above_every_block() {
        let (controller, _mmio) = fresh_controller(0, 24);
        assert_eq!(
            IrqController::mask(&controller, 99),
            Err(MaskError::OutOfRange),
        );
    }

    /// A block whose global system interrupts wrap, run into the MSI lines,
    /// or overlap a block already taken is refused; one beside it is not.
    #[test]
    fn a_block_takes_only_global_system_interrupts_no_other_holds() {
        let mmio = RecordingMmio::new();
        let held = [block(0, 0, 24, &mmio), block(1, 40, 8, &mmio)];
        assert!(gsis_free(&held, 24, 16, 4096), "between the two");
        assert!(gsis_free(&held, 48, 24, 4096), "after both");
        assert!(!gsis_free(&held, 16, 16, 4096), "overlaps the first");
        assert!(!gsis_free(&held, 30, 16, 4096), "overlaps the second");
        assert!(!gsis_free(&held, 44, 2, 4096), "inside the second");
        assert!(!gsis_free(&held, 4090, 24, 4096), "runs into the MSI lines");
        assert!(!gsis_free(&held, u32::MAX - 4, 24, u32::MAX), "wraps");
        assert!(gsis_free::<RecordingMmio>(&[], 0, 24, 4096));
    }

    #[test]
    fn multi_ioapic_controller_routes_by_gsi_base() {
        let mmio0 = RecordingMmio::new();
        let mmio1 = RecordingMmio::new();
        let controller = Controller::new(alloc::vec![
            block(0, 0, 24, &mmio0),
            block(1, 24, 8, &mmio1)
        ])
        .unwrap();
        assert_eq!(controller.block_count(), 2);
        controller.program_pin(5, 0x40, 1).expect("block 0");
        controller.program_pin(27, 0x41, 2).expect("block 1");
        assert!(!mmio0.snapshot().is_empty(), "block 0 received writes");
        assert!(!mmio1.snapshot().is_empty(), "block 1 received writes");
    }

    /// / W3: the IO-APIC controller passes the shared Arch HAL
    /// interrupt-controller conformance vertical over its real handle
    /// (`plans/WIRING.md` Stage W3). GSI 7 is programmed (so it is an
    /// addressable, maskable line); GSI 99 is above the single block's
    /// range. x86_64 is vectored, so there is no `InterruptEntry` handle
    /// to drive.
    #[test]
    fn ioapic_controller_passes_arch_hal_irq_conformance() {
        let (controller, _mmio) = fresh_controller(0, 24);
        controller
            .program_pin(7, 0x30, 0xAB)
            .expect("program the line so it is addressable");
        tairix_arch_api::irq::conformance::run_controller(&controller, 7, 99);
    }

    /// Stage 4.D Item 2-tail.2 — the mask-before-wake regression
    /// probe. Drives [`IrqTable`] with this controller and asserts
    /// the controller's MMIO write log records the mask write
    /// *before* the [`IrqTable`] flips `ready = true`.
    ///
    /// The fire path on success returns `FireOutcome::Marked`; the
    /// test observes the MMIO write count snapshotted by an
    /// [`IrqController`] override that records the snapshot at the
    /// moment `mask` returns and compares it against the count
    /// observed immediately after `fire` returns.
    #[test]
    fn ioapic_controller_mask_before_wake_ordering() {
        use tairix_kernel_irq::{FireOutcome, IrqTable};
        use tairix_kernel_sec::{ProcessId, TaskId};

        let (controller, mmio) = fresh_controller(0, 24);
        controller.program_pin(7, 0x30, 0xAB).expect("prog");
        controller.unmask(7).expect("unmask");
        // Clear the install-time writes.
        mmio.log.lock().unwrap().clear();

        // Build a kernel-neutral IrqTable and bind line 7 to an
        // arbitrary task; the bind is necessary so `fire` walks
        // the `Marked` branch (the branch that mask-before-wake
        // covers).
        let table = IrqTable::new(23);
        let owner = TaskId(1);
        let _outcome = table.bind(7, ProcessId(owner.0)).expect("bind");
        // Snapshot the write count *before* the fire.
        let pre_fire_writes = mmio.snapshot().len();
        let outcome = table
            .fire(7, &controller as &dyn IrqController)
            .expect("fire");
        assert!(matches!(outcome, FireOutcome::Marked));
        // Snapshot the write count *after* the fire. The
        // controller must have issued its two-write mask sequence
        // (low + high half of the redirection entry) before
        // `IrqTable::fire` returned the marked outcome — and
        // therefore before `try_wait_step` could observe
        // `ready = true`.
        let post_fire_writes = mmio.snapshot().len();
        assert_eq!(
            post_fire_writes - pre_fire_writes,
            1,
            "controller.mask must complete before IrqTable::fire returns Marked"
        );
        // The write at offset 0x10 + 14 must carry the mask bit set.
        let writes = mmio.snapshot();
        assert!(
            writes[pre_fire_writes].1 & (1 << 16) != 0,
            "mask bit must be set in the low half of the redirection entry"
        );
    }

    /// Stage 4.D Item 2-tail.2 QEMU validation — [`read_pin_low`]
    /// returns `None` for an unowned GSI and the cached
    /// redirection-entry low half for an owned pin.
    ///
    /// Used by the QEMU integration test to re-read the IO-APIC
    /// redirection-entry mask bit after [`IrqTable::fire`] runs; this
    /// host probe pins the contract on the same MMIO read seam.
    #[test]
    fn read_pin_low_returns_low_half_after_program_pin() {
        let (controller, _mmio) = fresh_controller(0, 24);
        // Out-of-range GSI → None.
        assert!(controller.read_pin_low(99).is_none());

        // Programmed and unmasked, the low half carries the vector but not
        // the mask bit.
        controller.program_pin(7, 0x42, 0xAB).expect("prog");
        controller.unmask(7).expect("unmask");
        let low = controller.read_pin_low(7).expect("pin 7 readable");
        assert_eq!(low & 0xFF, 0x42, "vector preserved in low byte");
        assert_eq!(low & (1 << 16), 0, "mask bit clear after unmasked program");

        // After `IrqController::mask`, the low half re-reads with
        // the mask bit set — the QEMU integration test's evidence
        // path for the mask-before-wake invariant.
        IrqController::mask(&controller, 7).expect("mask");
        let low_after = controller.read_pin_low(7).expect("pin 7 readable");
        assert_eq!(low_after & 0xFF, 0x42, "vector still preserved");
        assert!(
            low_after & (1 << 16) != 0,
            "mask bit set after IrqController::mask"
        );
    }

    /// [`unmask`] clears the mask bit while preserving the cached vector and
    /// destination: what the QEMU integration test does to the legacy IRQ-0
    /// GSI once it has activated it.
    #[test]
    fn unmask_clears_mask_bit_and_preserves_vector() {
        let (controller, _mmio) = fresh_controller(0, 24);
        controller.program_pin(7, 0x55, 0xCD).expect("prog");
        let low_before = controller.read_pin_low(7).expect("readable");
        assert!(low_before & (1 << 16) != 0, "pre-unmask: masked");
        // Unmask.
        controller.unmask(7).expect("unmask");
        let low_after = controller.read_pin_low(7).expect("readable");
        assert_eq!(low_after & 0xFF, 0x55, "vector preserved");
        assert_eq!(low_after & (1 << 16), 0, "mask bit cleared");
    }

    /// `unmask` on a GSI outside every block fails fail-closed.
    #[test]
    fn unmask_rejects_gsi_out_of_range() {
        let (controller, _mmio) = fresh_controller(0, 24);
        assert_eq!(controller.unmask(99), Err(ProgramError::GsiOutOfRange));
    }

    /// `unmask` on a pin that was never programmed has no cached
    /// `(vector, dest)` to re-apply, so it surfaces
    /// `ProgramError::GsiOutOfRange` — symmetric with
    /// [`IrqController::mask`]'s posture
    /// (`mask_returns_out_of_range_for_unprogrammed_pin`).
    #[test]
    fn unmask_rejects_unprogrammed_pin() {
        let (controller, _mmio) = fresh_controller(0, 24);
        assert_eq!(controller.unmask(7), Err(ProgramError::GsiOutOfRange));
    }

    /// The [`IrqController::rearm`] override clears the mask bit through the
    /// controller — the counterpart of the mask-before-wake `mask` write the
    /// user-space `irq_wait` park path drives on a bound line's behalf. A
    /// line masked by `IrqController::mask` re-reads unmasked after `rearm`,
    /// with the cached vector + destination preserved.
    #[test]
    fn rearm_clears_the_mask_bit_after_a_masking_fire() {
        let (controller, _mmio) = fresh_controller(0, 24);
        controller.program_pin(7, 0x61, 0x0C).expect("prog");
        controller.unmask(7).expect("unmask");
        // A `fire`-time mask leaves the pin masked.
        IrqController::mask(&controller, 7).expect("mask");
        assert!(
            controller.read_pin_low(7).expect("readable") & (1 << 16) != 0,
            "post-mask: masked"
        );
        // `rearm` re-enables it for the next device interrupt.
        IrqController::rearm(&controller, 7).expect("rearm");
        let low = controller.read_pin_low(7).expect("readable");
        assert_eq!(low & 0xFF, 0x61, "vector preserved");
        assert_eq!(low & (1 << 16), 0, "mask bit cleared by rearm");
    }

    /// A level pin's remote IRR is cleared through the EOI register before
    /// it is unmasked, so a pin whose interrupt reached its CPU edge-triggered
    /// raises another; an edge pin, or one already unmasked, ends nothing.
    #[test]
    fn rearm_ends_a_level_pin_s_interrupt_before_unmasking() {
        let mmio = RecordingMmio::versioned(IOAPIC_EOI_VERSION);
        let controller = Controller::new(alloc::vec![block(0, 0, 24, &mmio)]).unwrap();
        controller.program_pin(20, 0x07, 0).unwrap();
        controller.program_pin(4, 0x08, 0).unwrap();
        controller.unmask(20).unwrap();
        controller.unmask(4).unwrap();
        IrqController::rearm(&controller, 20).unwrap();
        assert!(
            mmio.eois().is_empty(),
            "an unmasked pin may have one in service"
        );
        IrqController::mask(&controller, 20).unwrap();
        IrqController::mask(&controller, 4).unwrap();
        IrqController::rearm(&controller, 4).unwrap();
        assert!(mmio.eois().is_empty(), "an edge pin holds no remote IRR");
        let writes = mmio.snapshot().len();
        IrqController::rearm(&controller, 20).unwrap();
        assert_eq!(mmio.eois(), [(writes, 0x07)], "ended before the unmask");
        assert_eq!(controller.read_pin_low(20).unwrap() & (1 << 16), 0);
    }

    /// An IO-APIC older than the EOI register has the pin taken through edge
    /// triggering instead, masked throughout.
    #[test]
    fn rearm_takes_an_old_io_apic_s_level_pin_through_edge() {
        let mmio = RecordingMmio::versioned(0x11);
        let controller = Controller::new(alloc::vec![block(0, 0, 24, &mmio)]).unwrap();
        controller.program_pin(20, 0x07, 0).unwrap();
        let writes = mmio.snapshot().len();
        IrqController::rearm(&controller, 20).unwrap();
        assert!(mmio.eois().is_empty());
        let low = |entry: u32| (0x10 + 2 * 20, entry);
        let level = 0x07 | (1 << 15) | (1 << 13);
        assert_eq!(
            mmio.snapshot()[writes..],
            [
                low((level & !(1 << 15)) | (1 << 16)),
                low(level | (1 << 16)),
                low(level),
            ]
        );
    }

    /// `rearm` on a line no block owns fails closed with
    /// [`MaskError::OutOfRange`], mirroring `mask`.
    #[test]
    fn rearm_rejects_gsi_out_of_range() {
        let (controller, _mmio) = fresh_controller(0, 24);
        assert_eq!(
            IrqController::rearm(&controller, 99),
            Err(MaskError::OutOfRange)
        );
    }

    /// Remapping as a test sees it: off, refusing, or making `redirection`
    /// for every pin, recording each target it was asked for.
    struct Remapping {
        made: Option<Result<u64, ActivationError>>,
        asked: Mutex<StdVec<(u8, InterruptTarget)>>,
    }

    impl Remapping {
        fn off() -> Self {
            Self::making(None)
        }

        fn making(made: Option<Result<u64, ActivationError>>) -> Self {
            Self {
                made,
                asked: Mutex::new(StdVec::new()),
            }
        }
    }

    impl PinRemapping for Remapping {
        fn entry(
            &self,
            ioapic: u8,
            target: InterruptTarget,
        ) -> Result<Option<u64>, ActivationError> {
            self.asked.lock().unwrap().push((ioapic, target));
            self.made.map_or(Ok(None), |made| made.map(Some))
        }
    }

    /// A pin takes a vector the first time it is activated, routed to it and
    /// programmed masked at the pool's CPU, and keeps it however often it is
    /// activated again.
    #[test]
    fn a_pin_takes_a_vector_when_first_activated_and_keeps_it() {
        let (controller, mmio) = fresh_controller(0, 24);
        let (vectors, routing, off) = (VectorPool::new(0xAB), Routing::new(), Remapping::off());
        assert_eq!(
            IrqController::mask(&controller, 7),
            Err(MaskError::OutOfRange)
        );
        controller
            .activate_pin(7, &vectors, &routing, &off)
            .expect("activates");
        assert_eq!(routing.gsi_for_vector(0x30), Some(7));
        let low = controller.read_pin_low(7).expect("readable");
        assert_eq!(low & 0xFF, 0x30);
        assert_ne!(low & (1 << 16), 0, "masked until bound");
        assert_eq!(
            mmio.last_writes.lock().unwrap().get(&0x1F),
            Some(&(0xAB << 24))
        );
        controller
            .activate_pin(7, &vectors, &routing, &off)
            .expect("active already");
        controller
            .activate_pin(9, &vectors, &routing, &off)
            .expect("activates");
        assert_eq!(routing.gsi_for_vector(0x31), Some(9), "pin 7 kept its own");
        assert_eq!(routing.vector_for_gsi(7), Some(0x30));
    }

    /// The defect a boot-time vector per pin had: IO-APICs carrying more pins
    /// than there are vectors left nothing for message-signalled sources, and
    /// failed to boot. Pins now cost vectors only as they are activated.
    #[test]
    fn more_pins_than_vectors_cost_only_the_active_ones() {
        let mmios = [
            RecordingMmio::new(),
            RecordingMmio::new(),
            RecordingMmio::new(),
        ];
        let controller = Controller::new(
            (0u8..3)
                .zip(&mmios)
                .map(|(id, mmio)| block(id, u32::from(id) * 120, 120, mmio))
                .collect(),
        )
        .expect("360 pins");
        controller.quiesce();
        for mmio in &mmios {
            assert!(
                mmio.snapshot()
                    .iter()
                    .filter(|(reg, _)| reg % 2 == 0)
                    .all(|(_, low)| low & (1 << 16) != 0),
                "every pin masked"
            );
        }
        let (vectors, routing, off) = (VectorPool::new(0), Routing::new(), Remapping::off());
        for gsi in [0, 121, 359] {
            controller
                .activate_pin(gsi, &vectors, &routing, &off)
                .expect("activates");
        }
        let mut free = 0;
        while vectors.claim().is_some() {
            free += 1;
        }
        assert_eq!(free, EXTERNAL_VECTOR_COUNT - 3);
        assert_eq!(
            controller.activate_pin(200, &vectors, &routing, &off),
            Err(ActivationError::Exhausted)
        );
        assert_eq!(
            IrqController::mask(&controller, 200),
            Err(MaskError::OutOfRange),
            "unactivated"
        );
        assert_eq!(controller.last_gsi(), Some(359));
        assert_eq!(controller.ioapic_ids().collect::<StdVec<_>>(), [0, 1, 2]);
    }

    /// Quiescing leaves an active pin as its activation programmed it.
    #[test]
    fn quiescing_leaves_an_active_pin_alone() {
        let (controller, mmio) = fresh_controller(0, 24);
        let (vectors, routing, off) = (VectorPool::new(0), Routing::new(), Remapping::off());
        controller
            .activate_pin(4, &vectors, &routing, &off)
            .expect("activates");
        controller.unmask(4).expect("unmask");
        mmio.log.lock().unwrap().clear();
        controller.quiesce();
        assert_eq!(
            mmio.snapshot().len(),
            2 * 23,
            "every other pin, both halves"
        );
        assert_eq!(
            controller.read_pin_low(4).map(|low| low & (1 << 16)),
            Some(0)
        );
    }

    /// Once remapping is on, a pin raises the entry its unit made for the
    /// target of its own vector, CPU and trigger, masked.
    #[test]
    fn a_pin_activated_under_remapping_raises_its_remapping_entry() {
        let mmio = RecordingMmio::new();
        let mut wired = block(2, 0, 24, &mmio);
        wired.wiring[9] = PinWiring::of(9, Some(0b1111));
        let controller = Controller::new(alloc::vec![wired]).unwrap();
        let remappable = (5 << 49) | (1 << 48) | 0x30;
        let remapping = Remapping::making(Some(Ok(remappable)));
        let (vectors, routing) = (VectorPool::new(0x1_0000), Routing::new());
        controller
            .activate_pin(9, &vectors, &routing, &remapping)
            .expect("activates");
        let target = InterruptTarget {
            vector: 0x30,
            destination: 0x1_0000,
            level: true,
        };
        assert_eq!(*remapping.asked.lock().unwrap(), [(2, target)]);
        let low = u64::from(controller.read_pin_low(9).unwrap());
        let high = u64::from(*mmio.last_writes.lock().unwrap().get(&0x23).unwrap());
        let wiring = 1 << 15 | 1 << 13;
        assert_eq!(low | high << 32, remappable | wiring | REDIRECTION_MASKED);
        let mut programmed = StdVec::new();
        controller.programmed(&mut |pin| programmed.push(pin));
        assert_eq!(programmed[0].destination, 0x1_0000);
    }

    /// A pin no entry can be made for, or whose CPU compatibility format
    /// cannot name — past its eight bits, or the broadcast id, which would
    /// reach every CPU — is left inactive, its vector unrouted and free.
    #[test]
    fn a_pin_that_cannot_reach_its_cpu_is_left_inactive() {
        let (controller, _mmio) = fresh_controller(0, 24);
        let routing = Routing::new();
        let refusing = Remapping::making(Some(Err(ActivationError::Unroutable)));
        let off = Remapping::off();
        for (vectors, remapping) in [
            (VectorPool::new(0), &refusing),
            (VectorPool::new(0x100), &off),
            (VectorPool::new(0xFF), &off),
        ] {
            assert_eq!(
                controller.activate_pin(7, &vectors, &routing, remapping),
                Err(ActivationError::Unroutable)
            );
            assert_eq!(routing.gsi_for_vector(0x30), None);
            assert_eq!(vectors.claim(), Some(0x30), "free again");
            assert_eq!(
                IrqController::mask(&controller, 7),
                Err(MaskError::OutOfRange)
            );
        }
    }

    /// A vector whose route another line holds is never handed to a pin, nor
    /// given back: it stays claimed, and the pin inactive.
    #[test]
    fn a_vector_routed_elsewhere_is_never_a_pin_s() {
        let (controller, _mmio) = fresh_controller(0, 24);
        let (vectors, routing, off) = (VectorPool::new(0), Routing::new(), Remapping::off());
        routing.install(4096, 0x30).expect("a stray route");
        assert_eq!(
            controller.activate_pin(7, &vectors, &routing, &off),
            Err(ActivationError::Exhausted)
        );
        assert_eq!(routing.gsi_for_vector(0x30), Some(4096));
        assert_eq!(
            vectors.claim(),
            Some(0x31),
            "the stray vector stays claimed"
        );
        assert_eq!(off.asked.lock().unwrap().len(), 0, "nothing was remapped");
    }

    #[test]
    fn a_gsi_no_block_owns_activates_nothing() {
        let (controller, _mmio) = fresh_controller(0, 24);
        let (vectors, routing, off) = (VectorPool::new(0), Routing::new(), Remapping::off());
        assert_eq!(
            controller.activate_pin(24, &vectors, &routing, &off),
            Err(ActivationError::OutOfRange)
        );
        assert_eq!(vectors.claim(), Some(0x30));
    }
}
