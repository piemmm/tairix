//! The PCI configuration space the kernel owns (`plans/IOMMU.md` IOM7).
//!
//! Where the kernel enumerates PCI itself (the x86_64 boot probe) it is the
//! one owner of every function's configuration space: drivers are handed
//! register windows, never configuration access. [`PciHost`] is that owner —
//! the one bus every kernel access goes through, one at a time, and the
//! record of the functions it handed over, by which the kernel turns each
//! one's bus mastering on as its owner begins and off as it ends.
//!
//! The record is built from the probe's own emission, never from a node's
//! descriptive address, which any publisher may set: no process can steer
//! the kernel's configuration writes at a function it was not handed.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::driver::msix::{MsiMessage, MsixBus};
use tairix_abi::driver::pci::{PciAddress, PciBus, BUS_MASTER_ENABLE, COMMAND_OFFSET};
use tairix_abi::driver::virtio_pci::{
    VirtioPciBus, VirtioPciWindows, VIRTIO_PCI_CFG_COMMON, VIRTIO_PCI_CFG_DEVICE,
    VIRTIO_PCI_CFG_ISR, VIRTIO_PCI_CFG_NOTIFY,
};
use tairix_abi::driver::MmioMapper;
use tairix_abi::{DriverError, IommuStreams};
use tairix_kernel_core::iommu::{
    BusMastering, InterruptSource, MasterChange, MasterTarget, Quiesced,
};
use tairix_kernel_iommu_api::IommuError;
use tairix_sync::SpinLock;

/// The seams the kernel drives a PCI bus through: enumeration and virtio
/// provisioning, MSI-X routing, and the generic function seam.
pub trait HostBus: VirtioPciBus + MsixBus + PciBus {}

impl<B: VirtioPciBus + MsixBus + PciBus + ?Sized> HostBus for B {}

/// The MSI-X table entry each interrupt-driven PCI function's message is
/// routed into: every virtqueue of a virtio function shares it, so one bound
/// line covers the whole device.
pub const MSIX_ENTRY: u16 = 0;

/// Point the interrupt of the function at `bdf` on `bus` at `message`: its
/// MSI-X entry where it has that capability, else its MSI capability.
///
/// # Errors
///
/// [`DriverError::NotFound`] for a function with neither, or the bus's
/// refusal.
pub fn route_message(
    bus: &dyn HostBus,
    bdf: u64,
    message: MsiMessage,
    registers: &dyn MmioMapper,
) -> Result<(), DriverError> {
    match bus.route_msix(bdf, MSIX_ENTRY, message, registers) {
        Err(DriverError::NotFound) => bus.route_msi(bdf, message),
        routed => routed,
    }
}

/// A function whose configuration space the kernel owns.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Its configuration address.
    pub address: u64,
    /// The node it was published as; [`None`] for one the kernel published
    /// no node for.
    pub node: Option<u32>,
    /// The stream a translation unit knows it by as its own, if one does.
    pub stream: Option<IommuStreams>,
    /// The requester ids its interrupts reach that unit as, if one
    /// translates it.
    pub interrupts: Option<InterruptSource>,
}

impl Function {
    /// Whether `target` names this function. Streams name only a function
    /// that was handed over, whose own stream lies in one of them.
    fn named_by(&self, target: MasterTarget<'_>) -> bool {
        match target {
            MasterTarget::Node(node) => self.node == Some(node),
            MasterTarget::Streams(ranges) => {
                self.node.is_some()
                    && self
                        .stream
                        .is_some_and(|own| ranges.iter().any(|range| range.covers(own)))
            }
        }
    }
}

/// A bridge on a segment the kernel owns: it forwards its buses' DMA
/// upstream only once a function on one of them is granted mastering.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Bridge {
    /// Its configuration address.
    pub address: u64,
    /// The bus directly below it.
    pub secondary: u8,
    /// The last bus below it.
    pub subordinate: u8,
}

impl Bridge {
    /// Whether DMA from a function at configuration `address` passes it.
    const fn forwards(&self, address: u64) -> bool {
        let (bus, _, _) = tairix_abi::driver::pci::function_of(address);
        self.secondary <= bus && bus <= self.subordinate
    }
}

/// A function the kernel published a node for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Published {
    /// Its segment.
    pub segment: u16,
    /// Its configuration address on the segment.
    pub address: u64,
    /// The stream a translation unit knows it by, if one does.
    pub stream: Option<IommuStreams>,
    /// The requester ids its interrupts reach that unit as, if they are
    /// known.
    pub interrupts: Option<InterruptSource>,
}

/// One segment the kernel owns: its bus, the functions on it the kernel
/// handed over, or stopped and kept, and its bridges.
pub struct HostSegment {
    number: u16,
    bus: Box<dyn HostBus + Send>,
    functions: Vec<Function>,
    bridges: Vec<Bridge>,
    windows: Vec<Range<u64>>,
}

impl HostSegment {
    /// Segment `number`, reached through `bus`, decoding no memory.
    #[must_use]
    pub fn new(
        number: u16,
        bus: Box<dyn HostBus + Send>,
        functions: Vec<Function>,
        bridges: Vec<Bridge>,
    ) -> Self {
        Self {
            number,
            bus,
            functions,
            bridges,
            windows: Vec::new(),
        }
    }

    /// The segment, decoding the PCI addresses `windows` as memory.
    #[must_use]
    pub fn decoding(self, windows: Vec<Range<u64>>) -> Self {
        Self { windows, ..self }
    }
}

/// The kernel's one owner of PCI configuration space, which the port's boot
/// probe publishes.
static PUBLISHED: tairix_sync::Once<PciHost> = tairix_sync::Once::new();

/// Publish the segments the boot probe owns as the kernel's PCI host, once,
/// reaching the registers it programs itself — a unit function's MSI-X
/// table — through `registers`; a host that cannot be held is logged, and
/// then no function masters.
pub fn publish(
    segments: Vec<HostSegment>,
    registers: Box<dyn MmioMapper + Send + Sync>,
    log: &dyn tairix_log::Sink,
) {
    let owned = PciHost::new(segments, registers)
        .is_some_and(|host| PUBLISHED.call_once_infallible(move || host).is_ok());
    if !owned {
        crate::pci_probe::log_discovery(
            log,
            tairix_log::Level::Error,
            "pci configuration space unowned; no function masters",
        );
    }
}

/// The kernel's owner of PCI configuration space, once published: every
/// kernel access to a function's configuration space, and every change to
/// its bus mastering, goes through it.
#[must_use]
pub fn published() -> Option<&'static PciHost> {
    PUBLISHED.get().ok().flatten()
}

/// The published host, through which the translation facility stops and
/// grants a function's bus mastering: every port's.
#[must_use]
pub fn bus_mastering() -> Option<&'static (dyn tairix_kernel_core::iommu::BusMastering + 'static)> {
    published().map(|host| host as &'static (dyn tairix_kernel_core::iommu::BusMastering + 'static))
}

/// The published host, through which a unit that is a PCI function raises
/// its own MSI: every port's.
#[must_use]
pub fn unit_function() -> Option<&'static dyn tairix_kernel_iommu_api::UnitFunction> {
    published().map(|host| host as &'static dyn tairix_kernel_iommu_api::UnitFunction)
}

/// Hand `sink` the PCI address windows the published host's segments
/// decode as memory: every port's.
pub fn decoded_windows(sink: &mut dyn FnMut(Range<u64>)) {
    if let Some(host) = published() {
        host.decoded_windows(sink);
    }
}

/// The kernel's one owner of PCI configuration space.
pub struct PciHost {
    /// Ascending by segment number.
    segments: Vec<Segment>,
    /// The next ownership epoch handed out; every function's record starts
    /// below it.
    epochs: AtomicU64,
    /// The kernel's own reach of the device registers it programs.
    registers: Box<dyn MmioMapper + Send + Sync>,
}

struct Segment {
    number: u16,
    state: SpinLock<HostState>,
    functions: Vec<Function>,
    bridges: Vec<Bridge>,
    windows: Vec<Range<u64>>,
}

/// What every access to one segment's bus is serialised over.
struct HostState {
    bus: Box<dyn HostBus + Send>,
    /// For each of the segment's functions, the epoch of the latest owner
    /// that changed its bus mastering.
    changed_by: Vec<u64>,
}

impl PciHost {
    /// The owner of every segment in `segments`, a segment named twice
    /// keeping its first bus, reaching the registers it programs through
    /// `registers`; [`None`] where its records cannot be had.
    #[must_use]
    pub fn new(
        mut segments: Vec<HostSegment>,
        registers: Box<dyn MmioMapper + Send + Sync>,
    ) -> Option<Self> {
        segments.sort_by_key(|segment| segment.number);
        segments.dedup_by_key(|segment| segment.number);
        let mut owned = Vec::new();
        owned.try_reserve_exact(segments.len()).ok()?;
        for segment in segments {
            let mut changed_by = Vec::new();
            changed_by.try_reserve_exact(segment.functions.len()).ok()?;
            changed_by.resize(segment.functions.len(), 0);
            owned.push(Segment {
                number: segment.number,
                state: SpinLock::new(HostState {
                    changed_by,
                    bus: segment.bus,
                }),
                functions: segment.functions,
                bridges: segment.bridges,
                windows: segment.windows,
            });
        }
        Some(Self {
            segments: owned,
            epochs: AtomicU64::new(1),
            registers,
        })
    }

    /// Hand `sink` every PCI address window a segment decodes as memory.
    pub fn decoded_windows(&self, sink: &mut dyn FnMut(Range<u64>)) {
        for window in self.segments.iter().flat_map(|segment| &segment.windows) {
            sink(window.clone());
        }
    }

    /// The function the probe published as `node`, as the probe recorded it:
    /// never from the node's own address, which any publisher may set.
    #[must_use]
    pub fn published(&self, node: u32) -> Option<Published> {
        self.segments.iter().find_map(|segment| {
            segment
                .functions
                .iter()
                .find(|function| function.node == Some(node))
                .map(|function| Published {
                    segment: segment.number,
                    address: function.address,
                    stream: function.stream,
                    interrupts: function.interrupts,
                })
        })
    }

    /// Run `f` over segment `segment`'s bus, alone: mechanism #1 reaches
    /// every function through one machine-wide pair of ports, and a command
    /// register is changed by a read and a write, so no two accesses may
    /// interleave. [`None`] for a segment the host does not own.
    pub fn with<R>(&self, segment: u16, f: impl FnOnce(&dyn HostBus) -> R) -> Option<R> {
        let at = self
            .segments
            .binary_search_by_key(&segment, |owned| owned.number)
            .ok()?;
        Some(f(&*self.segments[at].state.lock().bus))
    }
}

impl tairix_kernel_iommu_api::UnitFunction for PciHost {
    fn route_msi(&self, address: u32, message_address: u64, data: u32) -> Result<(), IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        let message = MsiMessage {
            address: message_address,
            data,
        };
        self.with(function.segment(), |bus| {
            PciBus::route_msi(bus, bdf, message)
        })
        .and_then(Result::ok)
        .ok_or(IommuError::Hardware)
    }

    fn route_msix(
        &self,
        address: u32,
        entry: u16,
        message_address: u64,
        data: u32,
    ) -> Result<(), IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        let message = MsiMessage {
            address: message_address,
            data,
        };
        self.with(function.segment(), |bus| {
            MsixBus::route_msix(bus, bdf, entry, message, &*self.registers)
        })
        .and_then(Result::ok)
        .ok_or(IommuError::Hardware)
    }

    fn msix_entries(&self, address: u32) -> Result<u16, IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        match self.with(function.segment(), |bus| bus.msix_entries(bdf)) {
            Some(Ok(entries)) => Ok(entries),
            Some(Err(DriverError::NotFound)) => Ok(0),
            Some(Err(_)) | None => Err(IommuError::Hardware),
        }
    }

    fn mask_msix(&self, address: u32, masked: bool) -> Result<(), IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        self.with(function.segment(), |bus| bus.mask_msix(bdf, masked))
            .and_then(Result::ok)
            .ok_or(IommuError::Hardware)
    }

    fn capability_header(&self, address: u32, id: u8) -> Result<Option<u32>, IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        match self.with(function.segment(), |bus| bus.capability_header(bdf, id)) {
            Some(Ok(header)) => Ok(Some(header)),
            Some(Err(DriverError::NotFound)) => Ok(None),
            Some(Err(_)) | None => Err(IommuError::Hardware),
        }
    }

    fn set_master(&self, address: u32, master: bool) -> Result<(), IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        let segment = self
            .segments
            .binary_search_by_key(&function.segment(), |owned| owned.number)
            .map(|at| &self.segments[at])
            .map_err(|_| IommuError::Hardware)?;
        let state = segment.state.lock();
        let bus = &*state.bus;
        if master {
            open_bridges(bus, &segment.bridges, bdf);
        }
        if mastering(bus, bdf) != Some(master) {
            let _ = bus.set_bus_master(bdf, master);
        }
        (mastering(bus, bdf) == Some(master))
            .then_some(())
            .ok_or(IommuError::Hardware)
    }

    fn set_intx(&self, address: u32, raise: bool) -> Result<(), IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        self.with(function.segment(), |bus| bus.set_intx(bdf, raise))
            .and_then(Result::ok)
            .ok_or(IommuError::Hardware)
    }

    fn virtio_windows(&self, address: u32) -> Result<VirtioPciWindows, IommuError> {
        let function = PciAddress::from_node_address(address);
        let bdf = function.config_address();
        let unknown = |err| match err {
            DriverError::NotFound => IommuError::OutOfRange,
            _ => IommuError::Hardware,
        };
        self.with(function.segment(), |bus| {
            let window = |role| bus.virtio_window_region(bdf, role).map_err(unknown);
            Ok(VirtioPciWindows {
                common: window(VIRTIO_PCI_CFG_COMMON)?,
                notify: window(VIRTIO_PCI_CFG_NOTIFY)?,
                isr: window(VIRTIO_PCI_CFG_ISR)?,
                device: window(VIRTIO_PCI_CFG_DEVICE)?,
                notify_off_multiplier: bus.notify_off_multiplier(bdf).map_err(unknown)?,
                msix_entry: None,
            })
        })
        .ok_or(IommuError::Hardware)?
    }
}

/// Let every bridge above the function at `address` forward its DMA. One
/// that refuses leaves the function's DMA stopped short of memory, which is
/// the side a failure belongs on; a bridge is never closed again by a
/// revocation, the function's own bit being what stops it.
fn open_bridges(bus: &dyn HostBus, bridges: &[Bridge], address: u64) {
    for bridge in bridges.iter().filter(|bridge| bridge.forwards(address)) {
        if mastering(bus, bridge.address) == Some(false) {
            let _ = bus.set_bus_master(bridge.address, true);
        }
    }
}

/// Whether the function at `address` masters DMA; [`None`] for one that no
/// longer answers.
fn mastering(bus: &dyn HostBus, address: u64) -> Option<bool> {
    let dword = bus.read_config(address, COMMAND_OFFSET).ok()?;
    (dword != u32::MAX).then_some(dword & BUS_MASTER_ENABLE != 0)
}

impl BusMastering for PciHost {
    fn begin(&self) -> u64 {
        // Only uniqueness and order matter, and the counter is the whole of
        // the state.
        self.epochs.fetch_add(1, Ordering::Relaxed)
    }

    fn set_mastering(
        &self,
        target: MasterTarget<'_>,
        master: bool,
        epoch: u64,
        report: &mut dyn FnMut(MasterChange),
    ) {
        // A function lies on one segment, so reporting under that segment's
        // lock keeps each function's records in the order its changes landed.
        for segment in &self.segments {
            let mut state = segment.state.lock();
            let HostState { bus, changed_by } = &mut *state;
            let mut change = None;
            for (function, latest) in segment.functions.iter().zip(changed_by.iter_mut()) {
                if !function.named_by(target) || epoch < *latest {
                    continue;
                }
                let Some(was) = mastering(&**bus, function.address) else {
                    continue;
                };
                *latest = epoch;
                if master && !was {
                    open_bridges(&**bus, &segment.bridges, function.address);
                }
                let changed = was != master && bus.set_bus_master(function.address, master).is_ok();
                let refused = mastering(&**bus, function.address) != Some(master);
                let seen: &mut MasterChange = change.get_or_insert_default();
                seen.changed |= changed;
                seen.refused |= refused;
            }
            if let Some(change) = change {
                report(change);
            }
        }
    }

    fn quiesce(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> Quiesced {
        let mut quiesced = Quiesced::default();
        for segment in &self.segments {
            let state = segment.state.lock();
            for function in &segment.functions {
                if !function
                    .stream
                    .is_some_and(|stream| stream.unit() == unit && !keeps(stream.first()))
                {
                    continue;
                }
                if mastering(&*state.bus, function.address) != Some(true) {
                    continue;
                }
                let _ = state.bus.set_bus_master(function.address, false);
                if mastering(&*state.bus, function.address) == Some(false) {
                    quiesced.stopped += 1;
                } else {
                    quiesced.refused += 1;
                }
            }
        }
        quiesced
    }

    fn set_wired_interrupt(&self, node: u32, raise: bool) {
        for segment in &self.segments {
            let state = segment.state.lock();
            for function in segment.functions.iter().filter(|f| f.node == Some(node)) {
                let _ = state.bus.set_intx(function.address, raise);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use tairix_abi::driver::bus::{Bus, BusDevice};
    use tairix_abi::{DriverError, HwNode, MmioMapper, MsiMessage, RegisterWindow};

    use super::*;

    /// A bus holding one command/status dword per function, by address; a
    /// function listed in `stuck` ignores every write, and one listed in
    /// `msix` has an MSI-X capability. Each routing is recorded with whether
    /// it was through MSI-X.
    struct CommandBus {
        commands: SpinLock<Vec<(u64, u32)>>,
        stuck: Vec<u64>,
        msix: Vec<u64>,
        routed: SpinLock<Vec<(u64, bool)>>,
    }

    impl Bus for CommandBus {
        fn enumerate(&self, _out: &mut [BusDevice]) -> Result<usize, DriverError> {
            Ok(0)
        }
    }

    impl VirtioPciBus for CommandBus {
        fn virtio_window_region(&self, _bdf: u64, _cfg: u8) -> Result<(u64, usize), DriverError> {
            Err(DriverError::Unsupported)
        }

        fn notify_off_multiplier(&self, _bdf: u64) -> Result<u32, DriverError> {
            Err(DriverError::Unsupported)
        }
    }

    impl MsixBus for CommandBus {
        fn route_msix(
            &self,
            bdf: u64,
            _entry: u16,
            _message: MsiMessage,
            _mapper: &dyn MmioMapper,
        ) -> Result<(), DriverError> {
            if !self.msix.contains(&bdf) {
                return Err(DriverError::NotFound);
            }
            self.routed.lock().push((bdf, true));
            Ok(())
        }

        fn msix_entries(&self, _bdf: u64) -> Result<u16, DriverError> {
            Err(DriverError::NotFound)
        }

        fn mask_msix(&self, _bdf: u64, _masked: bool) -> Result<(), DriverError> {
            Err(DriverError::NotFound)
        }
    }

    impl PciBus for CommandBus {
        fn route_msi(
            &self,
            bdf: u64,
            _message: tairix_abi::driver::msix::MsiMessage,
        ) -> Result<(), DriverError> {
            if self.commands.lock().iter().any(|(at, _)| *at == bdf) {
                self.routed.lock().push((bdf, false));
                Ok(())
            } else {
                Err(DriverError::NotFound)
            }
        }

        fn map_bar_window(
            &self,
            _bdf: u64,
            _bar_index: u8,
            _mapper: &dyn MmioMapper,
        ) -> Result<RegisterWindow, DriverError> {
            Err(DriverError::Unsupported)
        }

        fn enable_memory_space(&self, _bdf: u64) -> Result<(), DriverError> {
            Err(DriverError::Unsupported)
        }

        fn set_bus_master(&self, bdf: u64, master: bool) -> Result<(), DriverError> {
            if self.stuck.contains(&bdf) {
                return Ok(());
            }
            for (at, command) in self.commands.lock().iter_mut() {
                if *at == bdf {
                    *command = if master {
                        *command | BUS_MASTER_ENABLE
                    } else {
                        *command & !BUS_MASTER_ENABLE
                    };
                }
            }
            Ok(())
        }

        fn set_intx(&self, _bdf: u64, _raise: bool) -> Result<(), DriverError> {
            Ok(())
        }

        fn assign_bar(
            &self,
            _bdf: u64,
            _bar_index: u8,
            _window_base: u64,
            _window_size: u64,
        ) -> Result<u64, DriverError> {
            Err(DriverError::Unsupported)
        }

        fn read_config(&self, bdf: u64, offset: u16) -> Result<u32, DriverError> {
            assert_eq!(offset, COMMAND_OFFSET);
            Ok(self
                .commands
                .lock()
                .iter()
                .find(|(at, _)| *at == bdf)
                .map_or(u32::MAX, |(_, command)| *command))
        }

        fn capability_header(&self, _bdf: u64, _id: u8) -> Result<u32, DriverError> {
            Err(DriverError::NotFound)
        }

        fn describe_function(&self, _bdf: u64) -> Result<HwNode, DriverError> {
            Err(DriverError::Unsupported)
        }
    }

    const UNIT: u32 = 100;
    const DEVICE: u64 = 0x0000_1800;
    const QUIET: u64 = 0x0000_1F00;
    const STUCK: u64 = 0x0000_2000;
    /// An owner's epoch.
    const OWNER: u64 = 3;

    fn stream(first: u32, count: u32) -> IommuStreams {
        IommuStreams::new(UNIT, first, count).unwrap()
    }

    /// A host over three functions, each answering with `command`: `DEVICE`
    /// handed over as node 7 on stream 0x18, `QUIET` stopped and handed to no
    /// driver on stream 0x1F, and `STUCK` (node 9), whose bit ignores writes.
    /// The functions in `absent` are recorded but no longer answer.
    fn rig(command: u32, absent: &[u64]) -> PciHost {
        let commands = [DEVICE, QUIET, STUCK]
            .into_iter()
            .filter(|address| !absent.contains(address))
            .map(|address| (address, command))
            .collect();
        let bus = CommandBus {
            commands: SpinLock::new(commands),
            stuck: vec![STUCK],
            msix: Vec::new(),
            routed: SpinLock::new(Vec::new()),
        };
        let functions = vec![
            Function {
                address: DEVICE,
                node: Some(7),
                stream: Some(stream(0x18, 1)),
                interrupts: None,
            },
            Function {
                address: QUIET,
                node: None,
                stream: Some(stream(0x1F, 1)),
                interrupts: None,
            },
            Function {
                address: STUCK,
                node: Some(9),
                stream: None,
                interrupts: None,
            },
        ];
        PciHost::new(
            vec![HostSegment::new(0, Box::new(bus), functions, Vec::new())],
            Box::new(crate::test_support::NoRegisters),
        )
        .unwrap()
    }

    fn command(host: &PciHost, address: u64) -> u32 {
        host.with(0, |bus| bus.read_config(address, COMMAND_OFFSET))
            .unwrap()
            .unwrap()
    }

    fn locked(host: &PciHost) -> bool {
        host.segments
            .iter()
            .any(|segment| segment.state.is_locked())
    }

    const APPLIED: MasterChange = MasterChange {
        changed: true,
        refused: false,
    };

    /// What `host` reported for one change, if anything.
    fn change(
        host: &PciHost,
        target: MasterTarget<'_>,
        master: bool,
        epoch: u64,
    ) -> Option<MasterChange> {
        let mut reported = None;
        host.set_mastering(target, master, epoch, &mut |change| {
            assert!(locked(host), "reported before another change");
            assert!(reported.replace(change).is_none(), "reported once");
        });
        reported
    }

    #[test]
    fn a_node_s_function_is_granted_and_withdrawn_and_read_back() {
        let host = rig(0x0002, &[]);
        assert_eq!(
            change(&host, MasterTarget::Node(7), true, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, DEVICE), 0x0006);
        assert_eq!(command(&host, QUIET), 0x0002, "only the named function");
        assert_eq!(
            change(&host, MasterTarget::Node(7), true, OWNER),
            Some(MasterChange::default()),
            "a bit already on is not written again"
        );
        assert_eq!(
            change(&host, MasterTarget::Node(7), false, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, DEVICE), 0x0002);
    }

    #[test]
    fn an_owner_that_ends_after_its_successor_began_leaves_the_successor_mastering() {
        let host = rig(0, &[]);
        let node = MasterTarget::Node(7);
        assert_eq!(change(&host, node, true, OWNER), Some(APPLIED));
        assert_eq!(
            change(&host, node, true, OWNER + 1),
            Some(MasterChange::default())
        );
        assert_eq!(
            change(&host, node, false, OWNER),
            None,
            "the earlier owner's late end"
        );
        assert_eq!(command(&host, DEVICE), BUS_MASTER_ENABLE);
        assert_eq!(change(&host, node, false, OWNER + 1), Some(APPLIED));
        assert_eq!(command(&host, DEVICE), 0);
    }

    /// A unit's own function is given its fault interrupt's message and no
    /// bus mastering; one the host cannot reach is refused.
    #[test]
    fn a_unit_s_function_raises_its_msi_without_mastering() {
        use tairix_kernel_iommu_api::UnitFunction;

        let host = rig(0, &[]);
        let unit = u32::from(tairix_abi::driver::pci::requester_id(QUIET));
        host.route_msi(unit, 0xFEE0_0000, 0x41).unwrap();
        assert_eq!(command(&host, QUIET), 0);
        assert_eq!(
            host.route_msi(1 << 16 | unit, 0xFEE0_0000, 0x41),
            Err(tairix_kernel_iommu_api::IommuError::Hardware),
            "a segment the host does not own"
        );
        assert_eq!(
            host.route_msi(0x00FF, 0xFEE0_0000, 0x41),
            Err(tairix_kernel_iommu_api::IommuError::Hardware),
            "a function that does not answer"
        );
    }

    /// A node's function is found on its own segment as the probe recorded
    /// it; a function published as no node is never found.
    #[test]
    fn a_published_function_is_found_by_its_node_as_the_probe_recorded_it() {
        let bus = || CommandBus {
            commands: SpinLock::new(Vec::new()),
            stuck: Vec::new(),
            msix: Vec::new(),
            routed: SpinLock::new(Vec::new()),
        };
        let behind = InterruptSource::Buses { first: 2, last: 3 };
        let host = PciHost::new(
            vec![
                HostSegment::new(
                    4,
                    Box::new(bus()),
                    vec![Function {
                        address: DEVICE,
                        node: Some(7),
                        stream: Some(stream(0x18, 1)),
                        interrupts: Some(behind),
                    }],
                    Vec::new(),
                ),
                HostSegment::new(
                    0,
                    Box::new(bus()),
                    vec![
                        Function {
                            address: QUIET,
                            node: None,
                            stream: Some(stream(0x1F, 1)),
                            interrupts: None,
                        },
                        Function {
                            address: STUCK,
                            node: Some(9),
                            stream: None,
                            interrupts: None,
                        },
                    ],
                    Vec::new(),
                ),
            ],
            Box::new(crate::test_support::NoRegisters),
        )
        .unwrap();
        assert_eq!(
            host.published(7),
            Some(Published {
                segment: 4,
                address: DEVICE,
                stream: Some(stream(0x18, 1)),
                interrupts: Some(behind),
            })
        );
        assert_eq!(
            host.published(9),
            Some(Published {
                segment: 0,
                address: STUCK,
                stream: None,
                interrupts: None,
            })
        );
        assert_eq!(host.published(8), None);
    }

    #[test]
    fn a_grant_opens_the_bridges_above_its_function_and_no_others() {
        // BEHIND (18:00.0) lies below ROOT_PORT (00:01.0, buses 0x10 to 0x1F)
        // and SWITCH (10:00.0, bus 0x18); ASIDE (00:02.0) forwards others.
        const ROOT_PORT: u64 = 0x0000_0800;
        const SWITCH: u64 = 0x0010_0000;
        const ASIDE: u64 = 0x0000_1000;
        const BEHIND: u64 = 0x0018_0000;
        let bridge = |address, secondary, subordinate| Bridge {
            address,
            secondary,
            subordinate,
        };
        let bus = CommandBus {
            commands: SpinLock::new(vec![
                (ROOT_PORT, 0x0002),
                (SWITCH, 0x0002),
                (ASIDE, 0x0002),
                (BEHIND, 0x0002),
            ]),
            stuck: Vec::new(),
            msix: Vec::new(),
            routed: SpinLock::new(Vec::new()),
        };
        let host = PciHost::new(
            vec![HostSegment::new(
                0,
                Box::new(bus),
                vec![Function {
                    address: BEHIND,
                    node: Some(7),
                    stream: Some(stream(0x1800, 1)),
                    interrupts: None,
                }],
                vec![
                    bridge(ROOT_PORT, 0x10, 0x1F),
                    bridge(SWITCH, 0x18, 0x18),
                    bridge(ASIDE, 0x20, 0x2F),
                ],
            )],
            Box::new(crate::test_support::NoRegisters),
        )
        .unwrap();
        for bridge in [ROOT_PORT, SWITCH, ASIDE] {
            assert_eq!(command(&host, bridge), 0x0002, "nothing granted yet");
        }
        assert_eq!(
            change(&host, MasterTarget::Node(7), true, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, BEHIND), 0x0006);
        assert_eq!(command(&host, ROOT_PORT), 0x0006);
        assert_eq!(command(&host, SWITCH), 0x0006);
        assert_eq!(
            command(&host, ASIDE),
            0x0002,
            "forwards for no function granted"
        );
        assert_eq!(
            change(&host, MasterTarget::Node(7), false, OWNER + 1),
            Some(APPLIED)
        );
        assert_eq!(command(&host, BEHIND), 0x0002);
        assert_eq!(
            command(&host, SWITCH),
            0x0006,
            "a revocation stops the function, not its bridges"
        );
    }

    #[test]
    fn epochs_are_handed_out_in_order() {
        let host = rig(0, &[]);
        let first = host.begin();
        assert!(first > 0, "above every function's starting record");
        assert!(host.begin() > first);
    }

    #[test]
    fn streams_name_the_handed_over_function_inside_them() {
        let host = rig(0, &[]);
        // A child published for the same device carries a range holding its
        // parent's stream, and masters through the parent's function.
        let parent = [stream(0x10, 0x10)];
        assert_eq!(
            change(&host, MasterTarget::Streams(&parent), true, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, DEVICE), BUS_MASTER_ENABLE);
        assert_eq!(command(&host, QUIET), 0, "a function handed to no driver");
        let beside = [stream(0x19, 4), stream(0x0, 0x18)];
        assert_eq!(
            change(&host, MasterTarget::Streams(&beside), true, OWNER),
            None,
            "ranges missing the stream name nothing"
        );
        let elsewhere = [IommuStreams::new(UNIT + 1, 0x18, 1).unwrap()];
        assert_eq!(
            change(&host, MasterTarget::Streams(&elsewhere), true, OWNER),
            None
        );
    }

    #[test]
    fn a_node_the_host_never_handed_over_names_nothing() {
        assert_eq!(
            change(&rig(0, &[]), MasterTarget::Node(8), true, OWNER),
            None
        );
    }

    #[test]
    fn a_function_that_ignores_the_write_is_refused() {
        assert_eq!(
            change(&rig(0, &[]), MasterTarget::Node(9), true, OWNER),
            Some(MasterChange {
                changed: true,
                refused: true,
            })
        );
    }

    #[test]
    fn a_function_that_no_longer_answers_is_passed_over() {
        assert_eq!(
            change(&rig(0, &[DEVICE]), MasterTarget::Node(7), false, OWNER),
            None
        );
    }

    #[test]
    fn quiesce_stops_every_mastering_function_behind_the_unit_without_a_window() {
        let host = rig(BUS_MASTER_ENABLE, &[]);
        assert_eq!(
            host.quiesce(UNIT, &|stream| stream == 0x1F),
            Quiesced {
                stopped: 1,
                refused: 0,
            },
            "the one firmware keeps a window for keeps mastering"
        );
        assert_eq!(command(&host, DEVICE), 0);
        assert_eq!(command(&host, QUIET), BUS_MASTER_ENABLE);
        assert_eq!(
            host.quiesce(UNIT, &|_| false),
            Quiesced {
                stopped: 1,
                refused: 0,
            },
            "handed over or not"
        );
        assert_eq!(host.quiesce(UNIT + 1, &|_| false), Quiesced::default());
    }

    #[test]
    fn quiesce_reports_a_function_that_will_not_stop() {
        let bus = CommandBus {
            commands: SpinLock::new(vec![(STUCK, BUS_MASTER_ENABLE)]),
            stuck: vec![STUCK],
            msix: Vec::new(),
            routed: SpinLock::new(Vec::new()),
        };
        let host = PciHost::new(
            vec![HostSegment::new(
                0,
                Box::new(bus),
                vec![Function {
                    address: STUCK,
                    node: None,
                    stream: Some(stream(0x20, 1)),
                    interrupts: None,
                }],
                Vec::new(),
            )],
            Box::new(crate::test_support::NoRegisters),
        )
        .unwrap();
        assert_eq!(
            host.quiesce(UNIT, &|_| false),
            Quiesced {
                stopped: 0,
                refused: 1,
            }
        );
    }

    #[test]
    fn the_bus_is_reached_alone() {
        let host = rig(0, &[]);
        let reached = host.with(0, |bus| {
            assert!(locked(&host), "held across the whole access");
            bus.read_config(DEVICE, COMMAND_OFFSET)
        });
        assert_eq!(reached, Some(Ok(0)));
        assert!(!locked(&host));
        assert!(
            host.with(1, |_| ()).is_none(),
            "a segment the host does not own is reached by nothing"
        );
    }

    /// One requester id on two segments is two functions: a change names the
    /// node's own, and each segment is reached through its own bus.
    #[test]
    fn functions_on_two_segments_are_told_apart() {
        let bus = |command| CommandBus {
            commands: SpinLock::new(vec![(DEVICE, command)]),
            stuck: vec![],
            msix: Vec::new(),
            routed: SpinLock::new(Vec::new()),
        };
        let function = |node| Function {
            address: DEVICE,
            node: Some(node),
            stream: None,
            interrupts: None,
        };
        let host = PciHost::new(
            vec![
                HostSegment::new(1, Box::new(bus(0)), vec![function(11)], Vec::new()),
                HostSegment::new(0, Box::new(bus(0)), vec![function(10)], Vec::new()),
            ],
            Box::new(crate::test_support::NoRegisters),
        )
        .unwrap();
        assert_eq!(
            change(&host, MasterTarget::Node(11), true, OWNER),
            Some(APPLIED)
        );
        let on = |segment| {
            host.with(segment, |bus| bus.read_config(DEVICE, COMMAND_OFFSET))
                .unwrap()
                .unwrap()
                & BUS_MASTER_ENABLE
        };
        assert_eq!((on(0), on(1)), (0, BUS_MASTER_ENABLE));
    }

    /// A mapper that maps nothing: no routing here reaches a register window.
    struct NoMapper;

    impl MmioMapper for NoMapper {
        fn map_window(&self, _: u64, _: usize) -> Result<RegisterWindow, tairix_abi::MmioMapError> {
            Err(tairix_abi::MmioMapError::InvalidRegion)
        }
    }

    /// A function with MSI-X is routed through it; one with only MSI, as an
    /// HD Audio controller often is, through that; one with neither is left
    /// unrouted rather than handed a line that never fires.
    #[test]
    fn a_function_is_routed_through_msi_x_else_its_msi_capability() {
        const WITH_MSIX: u64 = 0x08;
        const MSI_ONLY: u64 = 0x10;
        const NEITHER: u64 = 0x18;
        let bus = CommandBus {
            commands: SpinLock::new(vec![(WITH_MSIX, 0), (MSI_ONLY, 0)]),
            stuck: Vec::new(),
            msix: vec![WITH_MSIX],
            routed: SpinLock::new(Vec::new()),
        };
        let message = MsiMessage {
            address: 0xFEE0_0000,
            data: 0x41,
        };
        assert_eq!(route_message(&bus, WITH_MSIX, message, &NoMapper), Ok(()));
        assert_eq!(route_message(&bus, MSI_ONLY, message, &NoMapper), Ok(()));
        assert_eq!(
            route_message(&bus, NEITHER, message, &NoMapper),
            Err(DriverError::NotFound)
        );
        assert_eq!(*bus.routed.lock(), [(WITH_MSIX, true), (MSI_ONLY, false)]);
    }
}
