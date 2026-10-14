//! Set-once boot-state publication for the riscv64 `virt`-board boot
//! consumer.
//!
//! The riscv64 virtio-MMIO QEMU verticals run as freestanding bins that
//! re-use the [`crate::boot`] pipeline and hijack the boot hart on
//! `AuditEvent::BootCompleted`. At that point they need the
//! flattened-device-tree pointer the boot pipeline otherwise consumes, to
//! walk the `virtio_mmio` slots, the PLIC base, and each device's
//! `interrupts` cell. Their DMA draws on the kernel's own frame allocator,
//! never on a second one built over the firmware map.
//!
//! The pointer is exposed through a set-once slot ([`OnceCell`]): the boot
//! pipeline publishes once and any observer reads an immutable `'static`
//! view. A second publish is rejected by the `OnceCell` (one-shot publish);
//! the accessor exposes no writable surface.

use tairix_sync::once::OnceCell;

/// Set-once slot for the flattened-device-tree pointer (`a1`) OpenSBI
/// handed the boot hart.
static DTB_PTR_SLOT: OnceCell<u64> = OnceCell::new();

/// Publish the flattened-device-tree pointer into the set-once slot.
///
/// Called once from [`crate::boot::try_boot`] with the verbatim `a1`
/// hand-off value. A second call is a no-op (one-shot publish).
pub fn publish_dtb(dtb: u64) {
    let _ = DTB_PTR_SLOT.set(dtb);
}

/// Read the device-tree pointer published by [`publish_dtb`].
///
/// Returns `None` until the boot pipeline has published it. The pointer
/// is the raw physical address of the device-tree blob; callers walk it
/// through the boot identity map (read-only
/// accessor).
#[must_use]
pub fn published_dtb() -> Option<u64> {
    match DTB_PTR_SLOT.get() {
        Ok(slot) => slot.copied(),
        Err(_) => None,
    }
}
