//! x86_64 message-signalled-interrupt (MSI/MSI-X) routing.
//!
//! An MSI is an *edge* interrupt a device delivers as a memory write to the
//! local-APIC doorbell, carrying the target vector in its data word. Unlike
//! a legacy `INTx` line it is **not** wired to an IO-APIC pin, so it has no
//! redirection entry to mask and no level to re-assert: a single edge is
//! delivered once, and the waiter taking the fire in
//! [`tairix_kernel_irq::IrqTable::try_wait_step`] is the whole re-arm
//! interlock.
//!
//! Modelling an MSI as if it were an IO-APIC pin is wrong and was the cause
//! of the x86_64 root-disk bring-up hang (`plans/OPEN-DEFECTS.md` D7): the
//! bring-up reused the interrupt-line pin's vector for the device's MSI-X
//! message, so the one edge source and a level IO-APIC redirection entry
//! shared a vector, and the fire/mask/re-arm path drove the wrong (pinned,
//! level) controller for an edge source. Linux avoids this by allocating a
//! **dedicated** vector for each MSI from a separate vector domain with an
//! MSI `irq_chip` whose mask/unmask never touch the IO-APIC; the aarch64
//! port does the same with its virtual `MSI_LINE_BASE` range and
//! `CompositeIrqController`. This module is the x86_64 equivalent.
//!
//! Three pieces:
//!
//! * [`MSI_LINE_BASE`] — the base of a virtual interrupt-line range that sits
//!   far above any real IO-APIC GSI, so an MSI line and a GSI can never
//!   alias. A driver binds an MSI line in the kernel
//!   [`tairix_kernel_irq::IrqTable`] exactly as it would a GSI.
//! * [`CompositeIrqController`] — the one line→controller fan-out
//!   [`tairix_kernel_irq::IrqTable::fire`] and the `irq_wait`/completion re-arm
//!   path drive: a real GSI masks/unmasks the [`IoApicController`] redirection
//!   entry; an MSI line is an edge source with **no** hardware line to mask, so
//!   its mask/re-arm are honest no-ops (the waiter taking the fire is the
//!   interlock, and the runaway-interrupt safety net in
//!   [`tairix_kernel_irq::IrqTable`] still contains a storming vector). This is
//!   published as the routing controller so the kernel core and the device-IRQ
//!   dispatch both reach it.
//! * The allocator (`install_msi_lines` + `allocate`, over the published
//!   [`MsiPool`]) — claims a dedicated [`MsiVector`] for a device that will
//!   deliver MSI/MSI-X. The free external vectors above the IO-APIC pins are
//!   pre-installed in the IDT and pre-published in the arch routing table as
//!   MSI lines at boot (`install_msi_lines`), so a runtime allocation is a
//!   lock-free bitmap claim that never mutates the IDT, and the vector
//!   delivers the instant a device is programmed with it.

use core::sync::atomic::{AtomicU64, Ordering};
use tairix_arch_x86_64::apic::IoApicMmio;
use tairix_kernel_irq::{IrqController, MaskError};

use crate::x86_64::ioapic_controller::IoApicController;

/// Base of the virtual MSI interrupt-line range.
///
/// Chosen far above any plausible IO-APIC GSI (a PC IO-APIC owns 24 pins;
/// even a large multi-IO-APIC server stays in the low hundreds), so an MSI
/// line and a real GSI can never collide. `install_msi_lines` fails closed
/// if the discovered IO-APIC GSI ceiling ever reaches this base.
pub const MSI_LINE_BASE: u32 = 4096;

/// The most MSI vectors there can be: x86's whole vector space. The pool
/// holds every external vector the IO-APIC pins left free.
pub const MAX_MSI_VECTORS: u32 = 256;

/// The MSI-X table entry each interrupt-driven PCI function's vector is
/// routed into: every virtqueue of a virtio function shares it, so one bound
/// line covers the whole device.
pub const MSIX_ENTRY: u16 = 0;

/// The MSI line a vector index names.
#[must_use]
pub const fn msi_line_for_index(index: u32) -> u32 {
    MSI_LINE_BASE + index
}

/// The MSI vector index a line names, or [`None`] if `line` is a real GSI
/// (below [`MSI_LINE_BASE`]) or above the allocatable range.
#[must_use]
pub fn msi_index_of_line(line: u32) -> Option<u32> {
    if (MSI_LINE_BASE..MSI_LINE_BASE + MAX_MSI_VECTORS).contains(&line) {
        Some(line - MSI_LINE_BASE)
    } else {
        None
    }
}

/// A kernel-side [`IrqController`] routing a real GSI to the [`IoApicController`]
/// and a virtual MSI line to the edge no-op path.
///
/// This is the single line→controller fan-out the kernel IRQ core drives
/// through `IrqRouting.controller`, and the same object the device-IRQ
/// dispatch masks through in [`tairix_kernel_irq::IrqTable::fire`]. It adds no
/// policy of its own: a GSI delegates to the range-checked, fence-ordered
/// [`IoApicController`]; an MSI line has no hardware line to mask (the edge
/// message is delivered once and its fire taken by the waiter), so its
/// mask/re-arm are no-ops that always succeed.
///
/// Generic over the IO-APIC MMIO backend so the host tests exercise the
/// exact fan-out over a mock IO-APIC.
pub struct CompositeIrqController<M: IoApicMmio + Send + 'static> {
    ioapic: &'static IoApicController<M>,
}

impl<M: IoApicMmio + Send + 'static> CompositeIrqController<M> {
    /// Wrap the boot-built IO-APIC controller.
    #[must_use]
    pub const fn new(ioapic: &'static IoApicController<M>) -> Self {
        Self { ioapic }
    }

    /// Borrow the wrapped IO-APIC controller (for the typed accessors the
    /// `irq_qemu_x86_64` vertical drives).
    #[must_use]
    pub const fn ioapic(&self) -> &'static IoApicController<M> {
        self.ioapic
    }
}

impl<M: IoApicMmio + Send + 'static> IrqController for CompositeIrqController<M> {
    fn mask(&self, line: u32) -> Result<(), MaskError> {
        match msi_index_of_line(line) {
            // An MSI is an edge source with no hardware line to mask before a
            // waiter observes the wake: the message is delivered once, the
            // line's fire count records it, and `try_wait_step` takes it. There
            // is nothing to mask, so this succeeds without touching hardware.
            Some(_) => Ok(()),
            None => self.ioapic.mask(line),
        }
    }

    fn rearm(&self, line: u32) -> Result<(), MaskError> {
        match msi_index_of_line(line) {
            // Symmetric with `mask`: an edge MSI has no masked line to
            // re-enable, so re-arm is a no-op. The next message delivers on
            // its own.
            Some(_) => Ok(()),
            None => self.ioapic.rearm(line),
        }
    }
}

/// Set-once slot for the boot-built [`CompositeIrqController`], published as
/// the trait object the [`IrqTable`](tairix_kernel_irq::IrqTable) masks
/// through and the in-kernel completion-wait re-arm path drives. The
/// bootstrap-floor root-unlock kthread reads it to build its device
/// waiter's controller; the same object is the `IrqRouting.controller` the
/// kernel core installs, so fire/mask/re-arm all reach one definition.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
static COMPOSITE_CONTROLLER: tairix_sync::once::OnceCell<
    &'static CompositeIrqController<tairix_arch_x86_64::apic::VolatileIoApicMmio>,
> = tairix_sync::once::OnceCell::new();

/// Publish the boot-built composite controller. Called once during
/// `discover_and_program_io_apics`; a second publish is a benign no-op.
///
/// Stored as the concrete `CompositeIrqController<VolatileIoApicMmio>` so a
/// caller can coerce it to whichever trait-object auto-trait set it needs —
/// the kernel-core `IrqRouting.controller` wants `+ Send + Sync`, the
/// in-kernel [`IrqParkWaiter`](tairix_kernel_core::IrqParkWaiter) wants
/// `+ Sync` — from the one published pointer.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub fn publish_composite(
    controller: &'static CompositeIrqController<tairix_arch_x86_64::apic::VolatileIoApicMmio>,
) {
    let _ = COMPOSITE_CONTROLLER.set(controller);
}

/// Read the published composite controller, or [`None`] before boot
/// published it (a headless / no-IO-APIC boot never does).
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
#[must_use]
pub fn published_composite(
) -> Option<&'static CompositeIrqController<tairix_arch_x86_64::apic::VolatileIoApicMmio>> {
    match COMPOSITE_CONTROLLER.get() {
        Ok(slot) => slot.copied(),
        Err(_) => None,
    }
}

/// A dedicated `(vector, line)` MSI allocation, and the CPU whose interrupt
/// table holds the vector.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MsiVector {
    /// The IDT vector the device's MSI message must carry in its data word.
    pub vector: u8,
    /// The virtual interrupt line the driver binds in the
    /// [`tairix_kernel_irq::IrqTable`].
    pub line: u32,
    /// The APIC id of the CPU the vector is installed on: the only one a
    /// message may name.
    pub destination: u32,
}

/// Failure modes of `allocate`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MsiAllocError {
    /// No free MSI vector remains (every pre-installed vector is claimed).
    Exhausted,
    /// The MSI vector pool was never installed (a boot with no IO-APIC never
    /// reaches `install_msi_lines`) — fail closed rather than fabricate a
    /// vector.
    Uninitialised,
}

/// The MSI vectors one boot installed, the CPU whose interrupt table holds
/// them, and which of them drivers have claimed: set-only, as a driver holds
/// its vector for life.
pub struct MsiPool {
    first: u8,
    count: u32,
    destination: u32,
    /// Bit `i` of word `i / 64`: slot `i` is claimed.
    claimed: [AtomicU64; MAX_MSI_VECTORS.div_ceil(64) as usize],
}

impl MsiPool {
    /// `count` vectors from `first`, at most [`MAX_MSI_VECTORS`], installed
    /// on the CPU whose APIC id is `destination`.
    #[must_use]
    pub const fn new(first: u8, count: u32, destination: u32) -> Self {
        Self {
            first,
            count: if count > MAX_MSI_VECTORS {
                MAX_MSI_VECTORS
            } else {
                count
            },
            destination,
            claimed: [const { AtomicU64::new(0) }; MAX_MSI_VECTORS.div_ceil(64) as usize],
        }
    }

    /// Claim the lowest free vector.
    ///
    /// # Errors
    ///
    /// [`MsiAllocError::Exhausted`] when every vector is claimed.
    pub fn allocate(&self) -> Result<MsiVector, MsiAllocError> {
        for (first, claimed) in (0..self.count).step_by(64).zip(&self.claimed) {
            let usable = (self.count - first).min(64);
            let mask = u64::MAX >> (64 - usable);
            let mut current = claimed.load(Ordering::Acquire);
            loop {
                let free = !current & mask;
                if free == 0 {
                    break;
                }
                let bit = free.trailing_zeros();
                let slot = first + bit;
                let vector = u8::try_from(u32::from(self.first) + slot)
                    .map_err(|_| MsiAllocError::Exhausted)?;
                match claimed.compare_exchange_weak(
                    current,
                    current | (1 << bit),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        return Ok(MsiVector {
                            vector,
                            line: msi_line_for_index(slot),
                            destination: self.destination,
                        });
                    }
                    Err(seen) => current = seen,
                }
            }
        }
        Err(MsiAllocError::Exhausted)
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
mod alloc_impl {
    use tairix_arch_x86_64::irq as arch_irq;
    use tairix_arch_x86_64::percpu;
    use tairix_sync::Once;

    use super::{msi_line_for_index, MsiAllocError, MsiPool, MsiVector, MAX_MSI_VECTORS};

    /// The boot's vectors, published once [`install_msi_lines`] installed
    /// them.
    static POOL: Once<MsiPool> = Once::new();

    /// Pre-install every free external vector above the IO-APIC pins as a
    /// dedicated MSI vector: install its per-CPU IDT entry (so a delivered
    /// message reaches [`arch_irq::global_routing`]'s dispatch) and publish
    /// `vector → msi_line` in the arch routing table. **No IO-APIC
    /// redirection entry is programmed** — an MSI never uses a pin.
    ///
    /// Returns the highest MSI line installed (`MSI_LINE_BASE + count - 1`),
    /// or `MSI_LINE_BASE - 1` when no vector was free (so the caller's
    /// `IrqRouting.max_line` still covers only the real GSIs). Once per boot:
    /// a second call is refused.
    ///
    /// `first_vector` is the first vector the IO-APIC-pin pass left unused,
    /// and `destination` the APIC id of the boot CPU, whose interrupt table
    /// alone holds the vectors. Failing to install a vector stops the pass at
    /// that point (the vectors installed so far remain usable), so a partial
    /// IDT is never published as a larger count.
    pub fn install_msi_lines(first_vector: u8, destination: u32) -> Result<u32, MsiAllocError> {
        if !matches!(POOL.get(), Ok(None)) {
            return Err(MsiAllocError::Uninitialised);
        }
        let routing = arch_irq::global_routing();
        let mut count: u32 = 0;
        let mut vector = first_vector;
        while count < MAX_MSI_VECTORS && vector <= arch_irq::EXTERNAL_VECTOR_LAST {
            let Some(isr_addr) = arch_irq::external_isr_addr(vector) else {
                break;
            };
            // SAFETY: the BSP finished `percpu::init(0)` before the boot
            // pipeline reaches IO-APIC programming, interrupts are masked,
            // and `vector` is in the reserved external range (never an
            // architectural exception vector). A second install of the same
            // vector is idempotent in the IDT.
            if unsafe { percpu::install_vector(0, vector, isr_addr) }.is_err() {
                break;
            }
            let line = msi_line_for_index(count);
            if routing.install(line, vector).is_err() {
                break;
            }
            count += 1;
            let Some(next) = vector.checked_add(1) else {
                break;
            };
            vector = next;
        }
        POOL.call_once_infallible(|| MsiPool::new(first_vector, count, destination))
            .map_err(|_| MsiAllocError::Uninitialised)?;
        // The inclusive top of the line space actually installed. With no
        // free vector the range is empty and the top is `MSI_LINE_BASE - 1`,
        // so a caller taking `max(max_gsi, top)` leaves `max_line` unchanged.
        Ok(msi_line_for_index(count).wrapping_sub(1))
    }

    /// Claim the lowest free MSI vector.
    pub fn allocate() -> Result<MsiVector, MsiAllocError> {
        match POOL.get() {
            Ok(Some(pool)) => pool.allocate(),
            _ => Err(MsiAllocError::Uninitialised),
        }
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub use alloc_impl::{allocate, install_msi_lines};

/// The compatibility-format message raising `vector` at the CPU it is
/// installed on, or [`None`] where that CPU's APIC id is past the eight bits
/// the format names: what a source no remapping unit sees writes.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
#[must_use]
pub fn compatibility_message(vector: MsiVector) -> Option<tairix_abi::driver::msix::MsiMessage> {
    let destination = u8::try_from(vector.destination).ok()?;
    Some(tairix_arch_x86_64::irq::msi_message(
        vector.vector,
        destination,
    ))
}

/// The MSI producer for interrupts the kernel takes itself — a translation
/// unit's fault event, which the unit raises itself and remapping does not
/// translate: never handed to a process, so the vector space stays the
/// kernel's.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub struct KernelMsi;

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
impl tairix_kernel_core::MsiAllocFacility for KernelMsi {
    fn allocate(&self) -> Result<tairix_abi::MsiAllocation, tairix_abi::Errno> {
        let vector = allocate().map_err(|err| match err {
            MsiAllocError::Exhausted => tairix_abi::Errno::OutOfRange,
            MsiAllocError::Uninitialised => tairix_abi::Errno::NotImplemented,
        })?;
        let message = compatibility_message(vector).ok_or(tairix_abi::Errno::NotImplemented)?;
        Ok(tairix_abi::MsiAllocation::new(
            message.address,
            message.data,
            vector.line,
        ))
    }
}

/// The one [`KernelMsi`] the port hands the kernel.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub static KERNEL_MSI: KernelMsi = KernelMsi;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every vector names the CPU its interrupt table holds it on, never the
    /// caller's, and the pool hands each out once, past one word of slots.
    #[test]
    fn every_vector_names_the_cpu_holding_it_and_is_handed_out_once() {
        let pool = MsiPool::new(0x40, 70, 3);
        for slot in 0..70u8 {
            let vector = pool.allocate().expect("a free vector");
            assert_eq!(vector.vector, 0x40 + slot);
            assert_eq!(vector.destination, 3);
            assert_eq!(vector.line, msi_line_for_index(u32::from(slot)));
        }
        assert_eq!(pool.allocate(), Err(MsiAllocError::Exhausted));
        assert_eq!(
            MsiPool::new(0xF0, 0, 0).allocate(),
            Err(MsiAllocError::Exhausted)
        );
    }

    #[test]
    fn msi_lines_and_gsis_never_alias() {
        // A real GSI (below the base) is not an MSI line.
        assert_eq!(msi_index_of_line(0), None);
        assert_eq!(msi_index_of_line(23), None);
        assert_eq!(msi_index_of_line(MSI_LINE_BASE - 1), None);
        // The base and the top of the range map to slot 0 and the last slot.
        assert_eq!(msi_index_of_line(MSI_LINE_BASE), Some(0));
        assert_eq!(
            msi_index_of_line(MSI_LINE_BASE + MAX_MSI_VECTORS - 1),
            Some(MAX_MSI_VECTORS - 1),
        );
        // Above the range is not an MSI line.
        assert_eq!(msi_index_of_line(MSI_LINE_BASE + MAX_MSI_VECTORS), None);
    }

    #[test]
    fn line_for_index_round_trips() {
        for i in 0..MAX_MSI_VECTORS {
            assert_eq!(msi_index_of_line(msi_line_for_index(i)), Some(i));
        }
    }
}
