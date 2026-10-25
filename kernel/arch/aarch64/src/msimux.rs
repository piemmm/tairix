//! The chained message-interrupt multiplexer the firmware tree describes, as
//! the Arch HAL [`MsiMultiplexer`] slice: the binary's one way to it, so the
//! binary never names the part behind it.

use tairix_arch_api::MsiMultiplexer;

use crate::fdt::Fdt;

/// Configure the multiplexer `fdt` describes and hand it over, or [`None`]
/// for a board without one. Once, on the boot CPU, with the MMU on.
#[must_use]
pub fn configure_from_fdt(fdt: &Fdt<'_>) -> Option<&'static dyn MsiMultiplexer> {
    crate::brcm_msi::configure_from_fdt(fdt)
}
