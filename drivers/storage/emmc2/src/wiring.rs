//! Driver-host wiring: discovered EMMC2 register window → [`Emmc2`].
//!
//! The aarch64 `FdtDiscovery` emits the `brcm,bcm2711-emmc2` node as a
//! Storage-class device whose resources are the SDHCI register window (read
//! from the device tree's `reg`, translated through the ancestor buses'
//! `ranges`) and the DMA window its bus translates through. This is the only
//! seam that maps memory: [`open_discovered`] checks
//! [`CapabilityId::MMIO_MAP`], maps the window through the host's
//! [`MmioMapper`], carves the ADMA2 staging through the host's [`DmaHost`]
//! when it has one, and brings the card up over the register seam.

use tairix_abi::driver::dma::{DmaHost, DmaSlab};
use tairix_abi::driver::mmio::MmioMapError;
use tairix_abi::{CapabilityId, DriverError, DriverHost, MmioMapper};

use crate::{
    regs, Board, BringUpFault, BringUpStage, CompletionWait, Emmc2, IrqSdhci, DMA_DATA_BYTES,
    DMA_TABLE_BYTES,
};

/// Map the discovered EMMC2 register window and bring the card online.
///
/// `regs_phys` is the CPU-physical base of the SDHCI register block the
/// hardware-tree node carries. `waiter` is the completion and timed-wait
/// seam the engine parks on, and `board` the platform's base clock and card
/// supplies, borrowed for the bring-up alone.
///
/// With a host [`DmaHost`] the engine moves transfers by ADMA2 through a
/// [`DMA_DATA_BYTES`] data slab and a [`DMA_TABLE_BYTES`] descriptor slab; a
/// host without one, or a refused carve, leaves it on programmed I/O. Once the
/// bring-up has reset the controller it is declared quiesced to the DMA host,
/// so memory an earlier instance left with it can be released.
///
/// # Errors
///
/// A [`BringUpFault`]: [`BringUpStage::MapWindow`] with
/// [`DriverError::PermissionDenied`] without [`CapabilityId::MMIO_MAP`],
/// [`DriverError::Unsupported`] with no [`MmioMapper`], or the mapper's own
/// refusal; otherwise whatever [`Emmc2::open`] reports.
///
/// # Capabilities
///
/// Requires [`CapabilityId::MMIO_MAP`], plus the load-time
/// [`CapabilityId::DRV_LOAD`] [`crate::register`] checked. The DMA carve uses
/// [`CapabilityId::MEM_DMA`] through the host; without it the engine falls
/// back to programmed I/O rather than failing the bring-up.
pub fn open_discovered<W: CompletionWait>(
    host: &dyn DriverHost,
    regs_phys: u64,
    waiter: W,
    board: Board<'_>,
) -> Result<Emmc2<IrqSdhci<W>>, BringUpFault> {
    let map_fault = |error| BringUpFault {
        stage: BringUpStage::MapWindow,
        error,
    };
    if !host.has_capability(CapabilityId::MMIO_MAP) {
        return Err(map_fault(DriverError::PermissionDenied));
    }
    let mapper: &dyn MmioMapper = host
        .mmio_mapper()
        .ok_or(map_fault(DriverError::Unsupported))?;
    let window = mapper
        .map_window(regs_phys, regs::REGS_LEN_BYTES)
        .map_err(|e| map_fault(MmioMapError::as_driver_error(e)))?;

    let engine_host = match host.dma_host().and_then(carve_staging) {
        Some((data, table)) => IrqSdhci::with_dma(window, waiter, data, table),
        None => IrqSdhci::new(window, waiter),
    };
    let device = Emmc2::open(engine_host, board)?;
    // Bring-up reset the whole host controller, its ADMA engine included.
    if let Some(dma) = host.dma_host() {
        dma.device_quiesced();
    }
    Ok(device)
}

/// Carve the ADMA2 data and descriptor slabs, or neither.
fn carve_staging(dma: &dyn DmaHost) -> Option<(DmaSlab, DmaSlab)> {
    let data = dma.alloc_dma_zeroed(DMA_DATA_BYTES).ok()?;
    let table = dma.alloc_dma_zeroed(DMA_TABLE_BYTES).ok()?;
    Some((data, table))
}
