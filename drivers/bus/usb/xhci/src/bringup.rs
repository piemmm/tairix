//! Arch-neutral xHCI controller bring-up orchestration for the HCD process
//! (`plans/USB.md` U3b).
//!
//! This is the composition the **host-controller driver** runs at start-up: it
//! brings the controller up over the device-resource grants the kernel minted
//! for it (its already-assigned register BAR and a DMA constraint), powers its
//! ports and returns the [`UsbHost`] with the walk of every port started. The
//! HCD's serve loop runs that walk, as it runs everything the engine does; a
//! keyboard absent at boot is a first-class state, not a failure. The HCD then
//! serves each device's transfers over the URB transport seam
//! (`tairix_usb::transport`) to the autoloaded class driver — it does **not**
//! decode HID reports itself; that is the class driver's job.
//!
//! # What lives here, and what does not
//!
//! The HCD is **arch-neutral**: the PCIe root-complex bring-up and BAR
//! assignment stay in the separate bus drivers, which assign the controller's
//! BAR and emit the
//! enumerated controller as the `usb,xhci` node carrying the BAR + DMA + IRQ
//! grants. This HCD is autoloaded against that node, granted *only* the
//! resources it requested, and reaches them through the [`DriverHost`] its
//! runtime builds over those grants. So this orchestration knows nothing of
//! PCI or any board: it maps a register window by address,
//! carves a DMA region, and speaks the bus-agnostic xHCI protocol in
//! [`tairix_usb`].
//!
//! # Testing boundary
//!
//! QEMU models no Pi USB timing, so the host tests prove the composition and
//! its fail-closed paths up to the controller hand-off; over an inert mock
//! register window [`Xhci::open`] fails closed with
//! [`DriverError::DeviceFault`], which is exactly the on-metal boundary. The
//! live controller bring-up is the on-metal acceptance item.

use tairix_abi::hwtree::{HwResource, HwResourceKind};
use tairix_abi::{CapabilityId, DriverError, DriverHost, MmioMapper, RegisterWindow};
use tairix_usb::device::UsbHost;
use tairix_usb::{SlabBank, Xhci, XhciOpenStage, DEFAULT_POLL_BUDGET};

/// The brought-up controller the HCD serves: the engine over the mapped
/// register BAR and a growable [`SlabBank`] of DMA chunks minted from the
/// host's DMA seam, so per-device memory is allocated on attach and released
/// on detach, bounded by the controller's slots and genuine exhaustion. The
/// `'h` lifetime is the borrow of the [`DriverHost`] whose DMA seam backs the
/// bank.
pub type ControllerHost<'h> = UsbHost<'h, RegisterWindow, SlabBank<'h>>;

/// The concrete bring-up inputs the HCD derives from its kernel-issued
/// device-resource grants to drive [`bring_up_controller`].
///
/// A `devmgr`-autoloaded HCD is granted exactly the resources its matched
/// `usb,xhci` node requested — its already-assigned register BAR and a DMA
/// constraint — and no more. It does not know those addresses at build time
/// (they depend on the board's bus layout); it reads them from the grants the
/// kernel delivered through `resource_grants` and turns them into these values
/// with [`derive_controller_resources`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ControllerResources {
    /// Device-visible base address of the controller's assigned register BAR
    /// window — the address the HCD names the BAR by when mapping it through
    /// its [`DriverHost`] (the host resolves the covering grant and performs
    /// the bus→CPU translation).
    pub bar_base: u64,
    /// Length of that register window, in bytes.
    pub bar_len: usize,
    /// Exclusive upper bound, in the **device-visible** address space, of the
    /// inbound DMA aperture the controller may reach — the bound
    /// [`bring_up_controller`] checks the carved DMA region against — or
    /// [`None`] where the grant declares no constraint.
    pub dma_aperture_top: Option<u64>,
}

/// Derive the [`ControllerResources`] from the [`HwResource`] grants the
/// kernel minted for this HCD process.
///
/// Exactly one mappable register window — an [`HwResourceKind::Mmio`] window
/// (CPU/identity space) or an [`HwResourceKind::BusWindow`] (outbound
/// PCIe-bus space, addressed by its far-side `translated_base`) — supplies the
/// BAR `bar_base`/`bar_len`. Exactly one [`HwResourceKind::Dma`] constraint
/// supplies the aperture bound: its device-visible exclusive top is the
/// far-side base plus extent for a translated inbound viewport, or its
/// `addr_limit` for an untranslated constraint, where a zero `addr_limit`
/// declares none. Any other resource (an IRQ
/// line, the URB endpoint/shared-region grants the HCD itself mints later) is
/// ignored here — this derive maps only the BAR and carves only the DMA
/// region.
///
/// # Errors
///
/// Fails closed, never guessing a missing address:
///
/// * [`DriverError::NotFound`] if no register-window grant or no DMA grant is
///   present.
/// * [`DriverError::Unsupported`] if more than one register-window grant or
///   more than one DMA grant is present (an ambiguous delivery — a packaging
///   defect the HCD refuses rather than picking one).
/// * [`DriverError::OutOfRange`] for a zero-length BAR, a BAR length past
///   `usize`, or a translated DMA aperture whose far-side top overflows.
pub fn derive_controller_resources<'a, I>(resources: I) -> Result<ControllerResources, DriverError>
where
    I: IntoIterator<Item = &'a HwResource>,
{
    let mut bar: Option<(u64, u64)> = None;
    let mut aperture: Option<Option<u64>> = None;
    for resource in resources {
        // The register-window base (CPU `base` for an `Mmio` window, far-side
        // `translated_base` for a `BusWindow`) is the one definition in
        // `HwResource::register_window_base`, so this driver does not
        // re-decide it.
        if let Some(base) = resource.register_window_base() {
            if bar.is_some() {
                return Err(DriverError::Unsupported);
            }
            bar = Some((base, resource.length()));
            continue;
        }
        if resource.kind() == Some(HwResourceKind::Dma) {
            if aperture.is_some() {
                return Err(DriverError::Unsupported);
            }
            // A translated inbound viewport's device-visible window is
            // `[translated_base, translated_base + len)`, so its exclusive top
            // is the far-side base plus extent; an untranslated constraint's
            // `addr_limit` (stored as `base`) is already the device-visible
            // exclusive top.
            let top = if resource.is_translated_dma_window() {
                Some(
                    resource
                        .translated_base()
                        .checked_add(resource.length())
                        .ok_or(DriverError::OutOfRange)?,
                )
            } else {
                // `HwResource::dma(0, 0, ..)` is "no constraint declared", not
                // an aperture ending at address zero.
                (resource.base() != 0).then_some(resource.base())
            };
            aperture = Some(top);
        }
        // An IRQ line, an endpoint/shared grant, or an unknown kind is not part
        // of this derive (validate the kind).
    }
    let (bar_base, bar_len) = bar.ok_or(DriverError::NotFound)?;
    let bar_len = usize::try_from(bar_len).map_err(|_| DriverError::OutOfRange)?;
    if bar_len == 0 {
        return Err(DriverError::OutOfRange);
    }
    let dma_aperture_top = aperture.ok_or(DriverError::NotFound)?;
    Ok(ControllerResources {
        bar_base,
        bar_len,
        dma_aperture_top,
    })
}

/// Bring up the granted xHCI controller and start the walk of its ports,
/// returning the [`ControllerHost`] whose serve loop runs it.
///
/// `host` is the HCD process's [`DriverHost`] over its kernel-issued
/// device-resource grants. `bar_base`/`bar_len` name the controller's
/// already-assigned register BAR window; `dma_aperture_top` is the exclusive
/// upper bound, in the device-visible address space, of the inbound window the
/// bridge lets the controller reach, or [`None`] where the platform imposes
/// none. The carved region's device-visible end must lie wholly below it or
/// the controller could not reach its own rings.
///
/// The DMA region is carved and aperture-checked before any register is
/// touched, so a region the controller could not reach is refused fail-closed
/// with nothing half-configured.
///
/// # Errors
///
/// As [`bring_up_controller_diagnostic`], but with the coarse
/// [`DriverError`] unwrapped from the diagnostic breadcrumb.
///
/// # Capabilities
///
/// Requires [`CapabilityId::MMIO_MAP`] (to map the register BAR); the DMA
/// carve is gated on the host's own DMA capability (`CAP_MEM_DMA`) at
/// allocation time. Both are re-checked kernel-side at each map/allocation.
pub fn bring_up_controller(
    host: &dyn DriverHost,
    bar_base: u64,
    bar_len: usize,
    dma_aperture_top: Option<u64>,
) -> Result<ControllerHost<'_>, DriverError> {
    bring_up_controller_diagnostic(host, bar_base, bar_len, dma_aperture_top)
        .map_err(|err| err.error)
}

/// The phase of [`bring_up_controller_diagnostic`] that failed, so the HCD's
/// one-shot diagnostic can name *where* a coarse [`DriverError`] came from
/// when the controller does not come up. QEMU models no Pi USB, so the live
/// bring-up is metal-only: this is the breadcrumb the on-metal capture
/// reports.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BringupPhase {
    /// Acquiring the host's capability / mapper / DMA seams (a missing
    /// `CAP_MMIO_MAP`, `MmioMapper`, or DMA host).
    Setup,
    /// Mapping the controller's register BAR (`mmio_map`).
    BarMap,
    /// Bringing the controller to the halted/reset state ([`Xhci::open`]).
    ControllerOpen,
    /// Growing and programming the controller's shared DMA structures and
    /// starting the controller ([`UsbHost::start`]). Covers the shared
    /// chunk's allocation and its aperture/alignment refusals (the bank
    /// fails closed on a chunk the controller could not reach).
    ControllerStart,
    /// Powering the root ports and starting their walk
    /// ([`UsbHost::bring_up`]). Only a fault of the **controller** reaches
    /// this phase: a device that will not enumerate is a counted skip the
    /// serve loop reports, never a bring-up failure.
    PortPower,
}

impl BringupPhase {
    /// Stable diagnostic name for the failing phase.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::BarMap => "bar_map",
            Self::ControllerOpen => "controller_open",
            Self::ControllerStart => "controller_start",
            Self::PortPower => "port_power",
        }
    }
}

/// A structured controller bring-up failure: the coarse [`DriverError`] plus
/// the breadcrumb a one-shot on-metal diagnostic needs to pin which controller
/// step stalled. The engine itself holds no logging dependency; the HCD wraps
/// the engine with its own diagnostics and emits this through `lib/log`.
///
/// [`BringupPhase::ControllerOpen`] sets [`Self::open_stage`], and it and
/// [`BringupPhase::PortPower`] the [`Self::usbcmd`]/[`Self::usbsts`] snapshot
/// the failing stage observed; the other phases carry only [`Self::phase`]
/// and [`Self::error`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ControllerBringupError {
    /// The phase that failed.
    pub phase: BringupPhase,
    /// The coarse driver error the phase returned.
    pub error: DriverError,
    /// The [`Xhci::open`] reset stage, set only for
    /// [`BringupPhase::ControllerOpen`].
    pub open_stage: Option<XhciOpenStage>,
    /// `USBCMD` observed at the failing stage, if readable.
    pub usbcmd: Option<u32>,
    /// `USBSTS` observed at the failing stage, if readable.
    pub usbsts: Option<u32>,
}

impl ControllerBringupError {
    /// A bare fault at `phase` with no captured controller state.
    const fn bare(phase: BringupPhase, error: DriverError) -> Self {
        Self {
            phase,
            error,
            open_stage: None,
            usbcmd: None,
            usbsts: None,
        }
    }
}

/// [`bring_up_controller`] with a structured [`ControllerBringupError`] on
/// failure, naming the phase that stalled and the controller state observed
/// there.
///
/// The HCD calls this (rather than the plain [`bring_up_controller`]) so it can
/// emit a one-shot diagnostic through `lib/log` when the controller does not
/// come up: a [`BringupPhase::ControllerOpen`] stall names the reset sub-stage
/// and its `USBCMD`/`USBSTS`. A device that fails to enumerate never lands
/// here — the walk skips it and the HCD warns with that port's own snapshot
/// while the controller keeps serving.
///
/// # Errors
///
/// As [`bring_up_controller`], but wrapped in [`ControllerBringupError`].
///
/// # Capabilities
///
/// As [`bring_up_controller`].
pub fn bring_up_controller_diagnostic(
    host: &dyn DriverHost,
    bar_base: u64,
    bar_len: usize,
    dma_aperture_top: Option<u64>,
) -> Result<ControllerHost<'_>, ControllerBringupError> {
    // Capability before state; the kernel re-checks at the map/allocation
    // traps regardless.
    if !host.has_capability(CapabilityId::MMIO_MAP) {
        return Err(ControllerBringupError::bare(
            BringupPhase::Setup,
            DriverError::PermissionDenied,
        ));
    }
    let mapper: &dyn MmioMapper = host.mmio_mapper().ok_or(ControllerBringupError::bare(
        BringupPhase::Setup,
        DriverError::Unsupported,
    ))?;
    // The controller allocates its xHCI DMA through the bus-neutral DMA seam,
    // not the virtio host.
    let dma_host = host.dma_host().ok_or(ControllerBringupError::bare(
        BringupPhase::Setup,
        DriverError::Unsupported,
    ))?;

    // Every DMA chunk the engine grows — the shared structures now, each
    // device's region as it attaches — is allocated through this bank and
    // verified to lie wholly below the discovered inbound-DMA aperture at
    // allocation time: a chunk the controller cannot reach is a fail-closed
    // refusal, never a silent truncation. The kernel maps each chunk
    // coherent (Normal Non-Cacheable on a non-I/O-coherent platform), so
    // the controller sees the rings the driver writes with no cache
    // maintenance.
    let dma = match dma_aperture_top {
        Some(top) => SlabBank::with_aperture(dma_host, top),
        None => SlabBank::new(dma_host),
    };

    // Map the controller's already-assigned register BAR. The host resolves
    // the grant covering `[bar_base, bar_base + bar_len)` and maps that window
    // once; a window no grant covers is refused.
    let window = mapper
        .map_window(bar_base, bar_len)
        .map_err(|e| ControllerBringupError::bare(BringupPhase::BarMap, e.as_driver_error()))?;

    // Bring the controller to the halted/reset state. `open_diagnostic` names
    // the failing reset sub-stage and the `USBCMD`/`USBSTS` it saw.
    let xhci = Xhci::open_diagnostic_with_budget(window, DEFAULT_POLL_BUDGET).map_err(|err| {
        let mut e = ControllerBringupError::bare(BringupPhase::ControllerOpen, err.error);
        e.open_stage = Some(err.stage);
        e.usbcmd = err.registers.usbcmd;
        e.usbsts = err.registers.usbsts;
        e
    })?;

    // Grow the shared chunk out of the bank, lay out and program the
    // device-shared structures, start the controller, and enable its
    // completion interrupter, which wakes the serve loop.
    let mut controller = UsbHost::start(xhci, dma, DEFAULT_POLL_BUDGET)
        .map_err(|e| ControllerBringupError::bare(BringupPhase::ControllerStart, e))?;

    // Power the ports and start their walk: each port, root or behind a hub,
    // is reconciled by its own operation as the serve loop runs.
    if let Err(error) = controller.bring_up() {
        let mut e = ControllerBringupError::bare(BringupPhase::PortPower, error);
        if let Ok(mut core) = controller.device() {
            e.usbcmd = core.read_usbcmd();
            e.usbsts = core.read_usbsts();
        }
        return Err(e);
    }
    Ok(controller)
}

#[cfg(test)]
#[path = "bringup_tests.rs"]
mod tests;
