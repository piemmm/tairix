//! x86_64 message-signalled-interrupt (MSI/MSI-X) routing.
//!
//! An MSI is an *edge* interrupt a device delivers as a memory write to the
//! local-APIC doorbell, carrying the target vector in its data word. Unlike
//! a legacy `INTx` line it is **not** wired to an IO-APIC pin, so it has no
//! redirection entry to mask and no level to re-assert: a single edge is
//! delivered once, and the waiter taking the fire in
//! [`tairix_kernel_irq::IrqTable::try_wait_step`] is the whole re-arm
//! interlock. Modelling one as a pin sharing its vector was the x86_64
//! root-disk hang (`plans/OPEN-DEFECTS.md` D7); like Linux's MSI domain, every
//! MSI has a vector of its own.
//!
//! * [`MSI_LINE_BASE`] — the base of the virtual interrupt lines MSIs raise,
//!   far above any IO-APIC GSI, so the two never alias. Each line names one
//!   external vector, so a driver binds an MSI line in the
//!   [`tairix_kernel_irq::IrqTable`] exactly as it would a GSI.
//! * [`CompositeIrqController`] — the one line→controller fan-out the IRQ
//!   table and the re-arm paths drive: a GSI reaches the [`IoApicController`],
//!   which gives a pin its vector when it is first activated; an MSI line is an
//!   edge source with **no** line to mask, so its mask and re-arm are no-ops.
//! * [`allocate_from`] and [`release_to`] — a dedicated [`MsiVector`] from a
//!   [`VectorPool`], its line recorded in the arch routing table for as long
//!   as it is held; the boot's own pool is reached through `allocate` and
//!   `release`.

use tairix_arch_x86_64::apic::IoApicMmio;
use tairix_arch_x86_64::irq::{Routing, EXTERNAL_VECTOR_FIRST, EXTERNAL_VECTOR_LAST};
use tairix_kernel_irq::{ActivationError, IrqController, MaskError};

use crate::x86_64::ioapic_controller::{IoApicController, PinRemapping};
use crate::x86_64::vectors::VectorPool;

/// Base of the virtual MSI interrupt-line range.
///
/// Far above any IO-APIC GSI (a PC IO-APIC owns 24 pins, and a large server's
/// several stay in the low hundreds); discovery refuses an IO-APIC whose GSIs
/// would reach it.
pub const MSI_LINE_BASE: u32 = 4096;

/// The last MSI line: the one the last external vector raises.
pub const LAST_MSI_LINE: u32 = msi_line_for_vector(EXTERNAL_VECTOR_LAST);

/// The MSI line `vector` raises.
#[must_use]
pub const fn msi_line_for_vector(vector: u8) -> u32 {
    MSI_LINE_BASE + vector.saturating_sub(EXTERNAL_VECTOR_FIRST) as u32
}

/// The vector the MSI line `line` names, or [`None`] for a GSI (below
/// [`MSI_LINE_BASE`]) or a line past the last vector.
#[must_use]
pub fn msi_vector_of_line(line: u32) -> Option<u8> {
    if !(MSI_LINE_BASE..=LAST_MSI_LINE).contains(&line) {
        return None;
    }
    u8::try_from(line - MSI_LINE_BASE)
        .ok()
        .and_then(|offset| EXTERNAL_VECTOR_FIRST.checked_add(offset))
}

/// A kernel-side [`IrqController`] routing a GSI to the [`IoApicController`]
/// and an MSI line to the edge no-op path.
///
/// The single line→controller fan-out the kernel IRQ core drives through
/// `IrqRouting.controller`, and the object the device-IRQ dispatch masks
/// through in [`tairix_kernel_irq::IrqTable::fire`]. Generic over the IO-APIC
/// MMIO backend so the host tests exercise the fan-out over a mock IO-APIC.
pub struct CompositeIrqController<M: IoApicMmio + Send + 'static> {
    ioapic: &'static IoApicController<M>,
    vectors: &'static VectorPool,
    routing: &'static Routing,
    remapping: &'static (dyn PinRemapping + Sync),
}

impl<M: IoApicMmio + Send + 'static> CompositeIrqController<M> {
    /// The fan-out over `ioapic`, its pins claiming their vectors from
    /// `vectors` and recording them in `routing`, through `remapping` once
    /// remapping is on.
    #[must_use]
    pub const fn new(
        ioapic: &'static IoApicController<M>,
        vectors: &'static VectorPool,
        routing: &'static Routing,
        remapping: &'static (dyn PinRemapping + Sync),
    ) -> Self {
        Self {
            ioapic,
            vectors,
            routing,
            remapping,
        }
    }
}

impl<M: IoApicMmio + Send + 'static> IrqController for CompositeIrqController<M> {
    // The message is delivered once and its fire taken by the waiter: an MSI
    // line has nothing to mask, re-arm or activate beyond the vector it was
    // allocated with.
    fn mask(&self, line: u32) -> Result<(), MaskError> {
        match msi_vector_of_line(line) {
            Some(_) => Ok(()),
            None => self.ioapic.mask(line),
        }
    }

    fn rearm(&self, line: u32) -> Result<(), MaskError> {
        match msi_vector_of_line(line) {
            Some(_) => Ok(()),
            None => self.ioapic.rearm(line),
        }
    }

    fn activate(&self, line: u32) -> Result<(), ActivationError> {
        match msi_vector_of_line(line) {
            Some(_) => Ok(()),
            None => self
                .ioapic
                .activate_pin(line, self.vectors, self.routing, self.remapping),
        }
    }
}

/// Set-once slot for the boot-built [`CompositeIrqController`]: the
/// `IrqRouting.controller` the kernel core installs, and the controller a
/// kernel-driven device's waiter re-arms through, so fire, mask and re-arm
/// all reach one definition.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
static COMPOSITE_CONTROLLER: tairix_sync::once::OnceCell<
    &'static CompositeIrqController<tairix_arch_x86_64::apic::VolatileIoApicMmio>,
> = tairix_sync::once::OnceCell::new();

/// Publish the boot-built composite controller. Called once during
/// `discover_and_program_io_apics`; a second publish is a benign no-op.
///
/// Stored as the concrete type so a caller can coerce it to whichever
/// trait-object auto-trait set it needs — the kernel-core
/// `IrqRouting.controller` wants `+ Send + Sync`, the in-kernel
/// [`IrqParkWaiter`](tairix_kernel_core::IrqParkWaiter) wants `+ Sync` —
/// from the one published pointer.
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

/// Failure modes of claiming a vector: [`allocate_from`], and the boot pool's
/// `allocate`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MsiAllocError {
    /// No external vector is free.
    Exhausted,
    /// The vectors were never installed (a boot with no IO-APIC never installs
    /// them): fail closed rather than fabricate a vector.
    Uninitialised,
}

/// Claim a vector of `vectors` for a message-signalled source, the line it
/// raises recorded in `routing`.
///
/// # Errors
///
/// [`MsiAllocError::Exhausted`] when no vector is free.
pub fn allocate_from(vectors: &VectorPool, routing: &Routing) -> Result<MsiVector, MsiAllocError> {
    let vector = vectors.claim().ok_or(MsiAllocError::Exhausted)?;
    let line = msi_line_for_vector(vector);
    // A free vector's route is always unmapped: a vector is given back only
    // after its route goes. One found otherwise stays claimed, never handed on.
    routing
        .install(line, vector)
        .map_err(|_| MsiAllocError::Exhausted)?;
    Ok(MsiVector {
        vector,
        line,
        destination: vectors.destination(),
    })
}

/// Give back the vector `line` raises, one nothing can raise any more: its
/// route goes before the vector is free, and a line not routed to the vector
/// it names — a pin's, or one never allocated — gives nothing back.
pub fn release_to(vectors: &VectorPool, routing: &Routing, line: u32) {
    if let Some(vector) = msi_vector_of_line(line) {
        if routing.remove(line, vector) {
            vectors.release(vector);
        }
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
mod alloc_impl {
    use tairix_arch_x86_64::irq as arch_irq;

    use super::{allocate_from, release_to, MsiAllocError, MsiVector};

    /// Claim a vector of the boot's pool for a message-signalled source.
    ///
    /// # Errors
    ///
    /// As [`allocate_from`], and [`MsiAllocError::Uninitialised`] before the
    /// vectors are installed.
    pub fn allocate() -> Result<MsiVector, MsiAllocError> {
        let vectors = crate::x86_64::vectors::published().ok_or(MsiAllocError::Uninitialised)?;
        allocate_from(vectors, arch_irq::global_routing())
    }

    /// Give back the vector `line` raises: one no device was told, or none
    /// told can raise any more.
    pub fn release(line: u32) {
        if let Some(vectors) = crate::x86_64::vectors::published() {
            release_to(vectors, arch_irq::global_routing(), line);
        }
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub use alloc_impl::{allocate, release};

/// The compatibility-format message raising `vector` at the CPU it is
/// installed on, or [`None`] where that CPU's APIC id is one the format's
/// eight bits cannot name: what a source no remapping unit sees writes.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
#[must_use]
pub fn compatibility_message(vector: MsiVector) -> Option<tairix_abi::driver::msix::MsiMessage> {
    let destination = tairix_arch_x86_64::apic::xapic_id(vector.destination)?;
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
impl tairix_kernel_core::KernelMsiFacility for KernelMsi {
    fn allocate(&self) -> Result<tairix_abi::MsiAllocation, tairix_abi::Errno> {
        let vector = allocate().map_err(|err| match err {
            MsiAllocError::Exhausted => tairix_abi::Errno::OutOfRange,
            MsiAllocError::Uninitialised => tairix_abi::Errno::NotImplemented,
        })?;
        let Some(message) = compatibility_message(vector) else {
            release(vector.line);
            return Err(tairix_abi::Errno::NotImplemented);
        };
        Ok(tairix_abi::MsiAllocation::new(
            message.address,
            message.data,
            vector.line,
        ))
    }

    fn release(&self, allocation: &tairix_abi::MsiAllocation) {
        release(allocation.line);
    }
}

/// The one [`KernelMsi`] the port hands the kernel.
#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub static KERNEL_MSI: KernelMsi = KernelMsi;

#[cfg(test)]
mod tests {
    use super::*;

    use tairix_arch_x86_64::irq::EXTERNAL_VECTOR_COUNT;

    /// Every vector names the CPU the pool installed it on, and its line the
    /// vector, routed while it is held.
    #[test]
    fn an_allocation_names_its_cpu_and_routes_its_line() {
        let (vectors, routing) = (VectorPool::new(3), Routing::new());
        let first = allocate_from(&vectors, &routing).expect("a free vector");
        assert_eq!(first.vector, EXTERNAL_VECTOR_FIRST);
        assert_eq!(first.destination, 3);
        assert_eq!(first.line, MSI_LINE_BASE);
        assert_eq!(routing.gsi_for_vector(first.vector), Some(first.line));
        let mut held = 1;
        while allocate_from(&vectors, &routing).is_ok() {
            held += 1;
        }
        assert_eq!(held, EXTERNAL_VECTOR_COUNT);
        assert_eq!(
            routing.gsi_for_vector(EXTERNAL_VECTOR_LAST),
            Some(LAST_MSI_LINE)
        );
    }

    /// A released vector loses its route before it is handed out again, so
    /// allocating and releasing cycles far past the vector space.
    #[test]
    fn a_released_vector_is_unrouted_and_handed_out_again() {
        let (vectors, routing) = (VectorPool::new(0), Routing::new());
        let kept = allocate_from(&vectors, &routing).expect("a free vector");
        for _ in 0..1000 {
            let vector = allocate_from(&vectors, &routing).expect("a free vector");
            assert_ne!(vector.line, kept.line);
            release_to(&vectors, &routing, vector.line);
            assert_eq!(routing.gsi_for_vector(vector.vector), None);
        }
        assert_eq!(routing.gsi_for_vector(kept.vector), Some(kept.line));
    }

    /// A pin's vector is never given back through the MSI line naming it, nor
    /// is a vector by a GSI.
    #[test]
    fn a_line_not_routed_to_its_vector_gives_nothing_back() {
        let (vectors, routing) = (VectorPool::new(0), Routing::new());
        let pin = vectors.claim().expect("a free vector");
        routing.install(9, pin).expect("routes the pin");
        release_to(&vectors, &routing, msi_line_for_vector(pin));
        release_to(&vectors, &routing, 9);
        assert_eq!(routing.gsi_for_vector(pin), Some(9));
        assert_ne!(
            allocate_from(&vectors, &routing).map(|vector| vector.vector),
            Ok(pin)
        );
    }

    #[test]
    fn msi_lines_and_gsis_never_alias() {
        assert_eq!(msi_vector_of_line(0), None);
        assert_eq!(msi_vector_of_line(23), None);
        assert_eq!(msi_vector_of_line(MSI_LINE_BASE - 1), None);
        assert_eq!(
            msi_vector_of_line(MSI_LINE_BASE),
            Some(EXTERNAL_VECTOR_FIRST)
        );
        assert_eq!(
            msi_vector_of_line(LAST_MSI_LINE),
            Some(EXTERNAL_VECTOR_LAST)
        );
        assert_eq!(msi_vector_of_line(LAST_MSI_LINE + 1), None);
    }

    #[test]
    fn a_line_names_the_vector_that_raises_it() {
        for vector in EXTERNAL_VECTOR_FIRST..=EXTERNAL_VECTOR_LAST {
            assert_eq!(
                msi_vector_of_line(msi_line_for_vector(vector)),
                Some(vector)
            );
        }
    }
}
