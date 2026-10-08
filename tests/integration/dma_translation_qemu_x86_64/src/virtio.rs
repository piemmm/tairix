//! The MI3 vertical behind a `virtio-iommu-pci`, its topology read from the
//! ACPI VIOT: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

mod vertical;

/// A virtio-iommu remaps no interrupt and keeps its domains' translations
/// itself. QEMU's has no MSI-X, and its INTx is routed only through ACPI's
/// `_PRT`, which the port does not read, so nothing hears its faults.
#[cfg(itest_x86_64)]
const UNIT: vertical::Unit = (
    tairix_itest_translation_witness::Interrupts::Unremapped,
    tairix_itest_translation_witness::Tables::Kept,
    tairix_itest_translation_witness::Faults::Unheard,
);

#[cfg(not(itest_x86_64))]
fn main() {}
