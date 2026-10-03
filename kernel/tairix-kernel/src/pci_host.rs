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
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::driver::msix::MsixBus;
use tairix_abi::driver::pci::{PciBus, BUS_MASTER_ENABLE, COMMAND_OFFSET};
use tairix_abi::driver::virtio_pci::VirtioPciBus;
use tairix_abi::IommuStreams;
use tairix_kernel_core::iommu::{BusMastering, MasterChange, MasterTarget, Quiesced};
use tairix_sync::SpinLock;

/// The seams the kernel drives a PCI bus through: enumeration and virtio
/// provisioning, MSI-X routing, and the generic function seam.
pub trait HostBus: VirtioPciBus + MsixBus + PciBus {}

impl<B: VirtioPciBus + MsixBus + PciBus + ?Sized> HostBus for B {}

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

/// The kernel's one owner of PCI configuration space.
pub struct PciHost {
    state: SpinLock<HostState>,
    functions: Vec<Function>,
    /// The next ownership epoch handed out; every function's record starts
    /// below it.
    epochs: AtomicU64,
}

/// What every access to the bus is serialised over.
struct HostState {
    bus: Box<dyn HostBus + Send>,
    /// For each of the host's functions, the epoch of the latest owner that
    /// changed its bus mastering.
    changed_by: Vec<u64>,
}

impl PciHost {
    /// The owner of `bus`'s configuration space; `functions` are the ones it
    /// handed over, or stopped and kept.
    #[must_use]
    pub fn new(bus: Box<dyn HostBus + Send>, functions: Vec<Function>) -> Self {
        let changed_by = alloc::vec![0; functions.len()];
        Self {
            state: SpinLock::new(HostState { bus, changed_by }),
            functions,
            epochs: AtomicU64::new(1),
        }
    }

    /// Run `f` over the bus, alone: mechanism #1 reaches every function
    /// through one machine-wide pair of ports, and a command register is
    /// changed by a read and a write, so no two accesses may interleave.
    pub fn with<R>(&self, f: impl FnOnce(&dyn HostBus) -> R) -> R {
        f(&*self.state.lock().bus)
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
        let mut state = self.state.lock();
        let HostState { bus, changed_by } = &mut *state;
        let mut change = None;
        for (function, latest) in self.functions.iter().zip(changed_by.iter_mut()) {
            if !function.named_by(target) || epoch < *latest {
                continue;
            }
            let Some(was) = mastering(&**bus, function.address) else {
                continue;
            };
            *latest = epoch;
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

    fn quiesce(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> Quiesced {
        let state = self.state.lock();
        let mut quiesced = Quiesced::default();
        for function in &self.functions {
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
        quiesced
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use tairix_abi::driver::bus::{Bus, BusDevice};
    use tairix_abi::{DriverError, HwNode, MmioMapper, MsiMessage, RegisterWindow};

    use super::*;

    /// A bus holding one command/status dword per function, by address; a
    /// function listed in `stuck` ignores every write.
    struct CommandBus {
        commands: SpinLock<Vec<(u64, u32)>>,
        stuck: Vec<u64>,
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

        fn offered_features(&self, _bdf: u64) -> Result<u64, DriverError> {
            Err(DriverError::Unsupported)
        }
    }

    impl MsixBus for CommandBus {
        fn route_msix(
            &self,
            _bdf: u64,
            _entry: u16,
            _message: MsiMessage,
            _mapper: &dyn MmioMapper,
        ) -> Result<(), DriverError> {
            Err(DriverError::Unsupported)
        }
    }

    impl PciBus for CommandBus {
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
        };
        let functions = vec![
            Function {
                address: DEVICE,
                node: Some(7),
                stream: Some(stream(0x18, 1)),
            },
            Function {
                address: QUIET,
                node: None,
                stream: Some(stream(0x1F, 1)),
            },
            Function {
                address: STUCK,
                node: Some(9),
                stream: None,
            },
        ];
        PciHost::new(Box::new(bus), functions)
    }

    fn command(host: &PciHost, address: u64) -> u32 {
        host.with(|bus| bus.read_config(address, COMMAND_OFFSET))
            .unwrap()
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
            assert!(host.state.is_locked(), "reported before another change");
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
        };
        let host = PciHost::new(
            Box::new(bus),
            vec![Function {
                address: STUCK,
                node: None,
                stream: Some(stream(0x20, 1)),
            }],
        );
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
        let reached = host.with(|bus| {
            assert!(host.state.is_locked(), "held across the whole access");
            bus.read_config(DEVICE, COMMAND_OFFSET)
        });
        assert_eq!(reached, Ok(0));
        assert!(!host.state.is_locked());
    }
}
