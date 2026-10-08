//! The IOM15 vertical behind an `SMMUv3` offering both stages, of which the
//! kernel takes stage 2, on a GICv3 `virt` board with its ITS
//! (`plans/IOMMU.md` IOM18.1, IOM18.2): the `vertical` module's run, the
//! keyboard's MSI-X reaching the arbiter as an LPI through its own domain.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod vertical;

/// Where the run proves the unit keeps its translations: the tables of the
/// stage the kernel takes of a unit offering both.
#[cfg(itest_aarch64)]
const TABLES: tairix_itest_translation_witness::Tables =
    tairix_itest_translation_witness::Tables::Walked(
        tairix_itest_translation_witness::Stage::Second,
    );

/// How the board's interrupts arrive: each PCI function's MSI-X through the
/// ITS, confined to the LPIs its routes were given, its doorbell mapped in
/// its domain.
#[cfg(itest_aarch64)]
const INTERRUPTS: tairix_itest_translation_witness::Interrupts =
    tairix_itest_translation_witness::Interrupts::Remapped {
        extended: the_cpus_take_every_lpi,
    };

/// A GICv3 CPU interface takes every LPI the ITS raises: there is no mode it
/// must enter first.
#[cfg(itest_aarch64)]
fn the_cpus_take_every_lpi() -> bool {
    true
}

#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/gicv3_tree.rs"));
}

/// The symbol the arch crate's boot trampoline calls.
#[cfg(itest_aarch64)]
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    vertical::boot(tree::DTB_BLOB)
}

#[cfg(not(itest_aarch64))]
fn main() {}
