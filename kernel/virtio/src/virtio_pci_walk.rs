//! Ring-0 virtio-PCI provisioning (Stage 4.D Item 4).
//!
//! A modern virtio PCI device is driven through four kernel-mapped register
//! windows (common / notify / ISR / device configuration) plus the
//! notification capability's `notify_off_multiplier`
//! ([`PciTransportWindows`]). Provisioning maps a window over each of the
//! virtio capabilities of the one function its caller names — the function
//! its bound node describes — through the frozen [`VirtioPciBus`] seam and the
//! [`MmioMapper`], whose capability check authorises every window; ring 0
//! names no `drivers/bus/*` type and synthesises no pointer of its own.

use tairix_abi::driver::virtio_pci::{
    VirtioPciBus, VIRTIO_PCI_CFG_COMMON, VIRTIO_PCI_CFG_DEVICE, VIRTIO_PCI_CFG_ISR,
    VIRTIO_PCI_CFG_NOTIFY,
};
use tairix_abi::{DriverError, MmioMapper};
use tairix_virtio::{PciTransportWindows, VirtioError};

/// Why [`provision_virtio_pci`] could not produce a transport.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VirtioPciWalkError {
    /// Mapping one of the device's virtio register windows failed
    /// (propagated verbatim; e.g. the caller lacks `CAP_MMIO_MAP`, or the
    /// function is no virtio device).
    MapWindow(DriverError),
    /// The mapped windows did not form a valid transport (e.g. a malformed
    /// common-configuration capability).
    Transport(VirtioError),
}

/// A provisioned virtio-PCI device: the transport `T` the caller's builder
/// produced over the four kernel-mapped register windows, and the function it
/// was built from, which its MSI-X routing is keyed by.
#[derive(Debug)]
pub struct VirtioProvision<T> {
    /// Transport the builder constructed over the kernel-mapped virtio
    /// register windows.
    pub transport: T,
    /// Bus-local address of the provisioned virtio function.
    pub bdf: u64,
}

/// Map the four virtio register windows of the function at `bdf` through
/// `mapper` and hand the assembled [`PciTransportWindows`] to `build`, which
/// constructs the caller's transport (in production
/// `tairix_drv_bus_virtio::PciTransport::new`), so ring 0 depends on `lib/*`
/// alone.
///
/// # Errors
///
/// [`VirtioPciWalkError`]; nothing here touches device state, and any init
/// `build` drives surfaces as [`VirtioPciWalkError::Transport`].
pub fn provision_virtio_pci<T, B>(
    bus: &dyn VirtioPciBus,
    bdf: u64,
    mapper: &dyn MmioMapper,
    build: B,
) -> Result<VirtioProvision<T>, VirtioPciWalkError>
where
    B: FnOnce(PciTransportWindows) -> Result<T, VirtioError>,
{
    let windows = PciTransportWindows {
        common: map(bus, bdf, VIRTIO_PCI_CFG_COMMON, mapper)?,
        notify: map(bus, bdf, VIRTIO_PCI_CFG_NOTIFY, mapper)?,
        isr: map(bus, bdf, VIRTIO_PCI_CFG_ISR, mapper)?,
        device: map(bus, bdf, VIRTIO_PCI_CFG_DEVICE, mapper)?,
        notify_off_multiplier: bus
            .notify_off_multiplier(bdf)
            .map_err(VirtioPciWalkError::MapWindow)?,
    };
    let transport = build(windows).map_err(VirtioPciWalkError::Transport)?;
    Ok(VirtioProvision { transport, bdf })
}

/// Map one virtio configuration window, tagging a failure as a
/// window-mapping error.
fn map(
    bus: &dyn VirtioPciBus,
    bdf: u64,
    cfg_type: u8,
    mapper: &dyn MmioMapper,
) -> Result<tairix_abi::RegisterWindow, VirtioPciWalkError> {
    bus.map_virtio_window(bdf, cfg_type, mapper)
        .map_err(VirtioPciWalkError::MapWindow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use tairix_abi::driver::mmio::MmioMapError;
    use tairix_abi::RegisterWindow;

    const TARGET_BDF: u64 = 0x0000_0800;

    /// Length the fake device advertises for each virtio config
    /// structure, keyed by `cfg_type`. The walk only maps the windows
    /// and assembles them; it does not inspect their contents (that is
    /// the builder's job), so any non-zero length exercises it.
    fn cfg_len(cfg_type: u8) -> usize {
        match cfg_type {
            VIRTIO_PCI_CFG_COMMON => 0x38,
            VIRTIO_PCI_CFG_NOTIFY => 0x10,
            VIRTIO_PCI_CFG_ISR => 0x4,
            VIRTIO_PCI_CFG_DEVICE => 0x8,
            _ => 0,
        }
    }

    /// Identity builder: keeps the assembled windows so the test can
    /// assert on them directly, standing in for a real transport
    /// constructor without depending on a `drivers/bus/*` crate.
    fn keep_windows() -> impl FnOnce(PciTransportWindows) -> Result<PciTransportWindows, VirtioError>
    {
        |windows| Ok(windows)
    }

    /// Mapper that hands out windows over freshly-leaked, aligned
    /// backing storage and records the `(phys, len)` of each request.
    struct RecordingMapper {
        grant: bool,
        requests: RefCell<alloc::vec::Vec<(u64, usize)>>,
    }

    impl RecordingMapper {
        fn new(grant: bool) -> Self {
            Self {
                grant,
                requests: RefCell::new(alloc::vec::Vec::new()),
            }
        }
    }

    impl MmioMapper for RecordingMapper {
        fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
            if !self.grant {
                return Err(MmioMapError::CapabilityMissing);
            }
            self.requests.borrow_mut().push((phys_base, len));
            let words = len.div_ceil(8).max(1);
            let boxed = alloc::vec![0u64; words].into_boxed_slice();
            let raw = alloc::boxed::Box::leak(boxed);
            let base = core::ptr::NonNull::new(raw.as_mut_ptr().cast::<u8>()).expect("non-null");
            // SAFETY: `base` covers `len` bytes of leaked storage that
            // lives for the rest of the test process; nothing else
            // aliases it and the window only performs volatile access.
            Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
        }
    }

    /// Fake bus resolving every virtio window of the function at
    /// [`TARGET_BDF`] by asking the mapper for a fixed length per `cfg_type`;
    /// a synthetic physical base encodes the `cfg_type` so a test can confirm
    /// the right window was mapped for the right structure. Any other
    /// function has no virtio capability.
    struct FakeBus;

    impl tairix_abi::driver::bus::Bus for FakeBus {
        fn enumerate(
            &self,
            _out: &mut [tairix_abi::driver::bus::BusDevice],
        ) -> Result<usize, DriverError> {
            Err(DriverError::Unsupported)
        }
    }

    impl VirtioPciBus for FakeBus {
        fn virtio_window_region(
            &self,
            bdf: u64,
            cfg_type: u8,
        ) -> Result<(u64, usize), DriverError> {
            let len = cfg_len(cfg_type);
            if bdf != TARGET_BDF || len == 0 {
                return Err(DriverError::NotFound);
            }
            Ok((0xC000_0000 + u64::from(cfg_type), len))
        }

        fn notify_off_multiplier(&self, _bdf: u64) -> Result<u32, DriverError> {
            Ok(4)
        }
    }

    #[test]
    fn provisions_the_named_function_s_transport() {
        let mapper = RecordingMapper::new(true);
        let provision =
            provision_virtio_pci(&FakeBus, TARGET_BDF, &mapper, keep_windows()).expect("transport");
        assert_eq!(provision.bdf, TARGET_BDF);
        assert_eq!(provision.transport.notify_off_multiplier, 4);
        let reqs = mapper.requests.borrow();
        assert_eq!(reqs.len(), 4);
        for cfg in [
            VIRTIO_PCI_CFG_COMMON,
            VIRTIO_PCI_CFG_NOTIFY,
            VIRTIO_PCI_CFG_ISR,
            VIRTIO_PCI_CFG_DEVICE,
        ] {
            let want = (0xC000_0000 + u64::from(cfg), cfg_len(cfg));
            assert!(reqs.contains(&want), "missing window for cfg {cfg}");
        }
    }

    #[test]
    fn a_function_with_no_virtio_windows_maps_nothing() {
        let mapper = RecordingMapper::new(true);
        assert_eq!(
            provision_virtio_pci(&FakeBus, TARGET_BDF + 0x100, &mapper, keep_windows())
                .unwrap_err(),
            VirtioPciWalkError::MapWindow(DriverError::NotFound)
        );
        assert!(mapper.requests.borrow().is_empty());
    }

    #[test]
    fn propagates_map_failure_as_permission_denied() {
        let mapper = RecordingMapper::new(false);
        assert_eq!(
            provision_virtio_pci(&FakeBus, TARGET_BDF, &mapper, keep_windows()).unwrap_err(),
            VirtioPciWalkError::MapWindow(DriverError::PermissionDenied)
        );
    }
}
