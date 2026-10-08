//! How a device the kernel drives itself — a bootstrap-floor disk — reaches
//! the memory it masters: through the kernel's own domain where a
//! translation unit stands in front of it, physical otherwise.

use tairix_abi::HwNode;
use tairix_kernel_core::iommu::{DmaPath, KERNEL_OWNER};
use tairix_kernel_core::InitSpawnCtx;
use tairix_kernel_mem::DmaTranslator;

/// Pages of the DMA window a floor virtio driver carves its rings and request
/// buffers from.
pub const WINDOW_PAGES: usize = 64;

/// How a floor device's DMA is confined.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Confinement {
    /// Through the unit standing in front of it, or, where none does, as it
    /// masters physical memory.
    WhereTranslated,
    /// Through the unit standing in front of it, or not at all.
    Translated,
}

/// The domain `node`'s device carves through when a unit translates it: the
/// kernel's own, which no driver can take. [`None`] for one no unit stands in
/// front of, where `confinement` allows it.
///
/// # Errors
///
/// A unit that translates nothing stands in front of it, or none does and
/// `confinement` demands one.
pub fn translator(
    ctx: &dyn InitSpawnCtx,
    node: &HwNode,
    confinement: Confinement,
) -> Result<Option<DmaTranslator>, &'static str> {
    let translation = ctx.dma_translation();
    match translation.map(|translation| (translation, translation.dma_path(node.id()))) {
        Some((translation, DmaPath::Translated { output_limit })) => Ok(Some(DmaTranslator {
            node: node.id(),
            generation: KERNEL_OWNER,
            domains: translation,
            output_limit,
        })),
        Some((_, DmaPath::Stranded { .. })) => {
            Err("floor: the device is behind a unit that translates nothing")
        }
        _ if confinement == Confinement::Translated => Err("floor: no unit confines the device"),
        _ => Ok(None),
    }
}
