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

use tairix_abi::driver::msix::MsixBus;
use tairix_abi::driver::pci::{PciBus, BUS_MASTER_ENABLE, COMMAND_OFFSET};
use tairix_abi::driver::virtio_pci::VirtioPciBus;
use tairix_abi::IommuStreams;
use tairix_kernel_core::iommu::{BusMastering, MasterChange, MasterTarget, KERNEL_OWNER};
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
    /// The node it was published as; [`None`] for one the kernel stopped and
    /// handed to no driver.
    pub node: Option<u32>,
    /// The stream a translation unit knows it by, if one does.
    pub stream: Option<IommuStreams>,
}

impl Function {
    /// Whether `target` names this function. Streams name only a function
    /// that was handed over, whose stream lies in them.
    fn named_by(&self, target: MasterTarget) -> bool {
        match target {
            MasterTarget::Node(node) => self.node == Some(node),
            MasterTarget::Streams(streams) => {
                self.node.is_some()
                    && self.stream.is_some_and(|own| {
                        own.unit() == streams.unit()
                            && own
                                .first()
                                .checked_sub(streams.first())
                                .is_some_and(|offset| offset < streams.count())
                    })
            }
        }
    }
}

/// The kernel's one owner of PCI configuration space.
pub struct PciHost {
    state: SpinLock<HostState>,
    functions: Vec<Function>,
}

/// What every access to the bus is serialised over.
struct HostState {
    bus: Box<dyn HostBus + Send>,
    /// For each of the host's functions, the generation of the latest owner
    /// that changed its bus mastering.
    changed_by: Vec<u64>,
}

impl PciHost {
    /// The owner of `bus`'s configuration space; `functions` are the ones it
    /// handed over, or stopped and kept.
    #[must_use]
    pub fn new(bus: Box<dyn HostBus + Send>, functions: Vec<Function>) -> Self {
        let changed_by = alloc::vec![KERNEL_OWNER; functions.len()];
        Self {
            state: SpinLock::new(HostState { bus, changed_by }),
            functions,
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
    fn set_mastering(
        &self,
        target: MasterTarget,
        master: bool,
        generation: u64,
    ) -> Option<MasterChange> {
        let mut state = self.state.lock();
        let HostState { bus, changed_by } = &mut *state;
        let mut change = None;
        for (function, latest) in self.functions.iter().zip(changed_by.iter_mut()) {
            if !function.named_by(target) || generation < *latest {
                continue;
            }
            let Some(was) = mastering(&**bus, function.address) else {
                continue;
            };
            *latest = generation;
            let changed = was != master && bus.set_bus_master(function.address, master).is_ok();
            let refused = mastering(&**bus, function.address) != Some(master);
            let seen: &mut MasterChange = change.get_or_insert_default();
            seen.changed |= changed;
            seen.refused |= refused;
        }
        change
    }

    fn strays(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> usize {
        let state = self.state.lock();
        self.functions
            .iter()
            .filter(|function| {
                function
                    .stream
                    .is_some_and(|stream| stream.unit() == unit && !keeps(stream.first()))
            })
            .filter(|function| mastering(&*state.bus, function.address) == Some(true))
            .count()
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
    /// A user driver's generation: every one is later than the kernel's.
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

    #[test]
    fn a_node_s_function_is_granted_and_withdrawn_and_read_back() {
        let host = rig(0x0002, &[]);
        assert_eq!(
            host.set_mastering(MasterTarget::Node(7), true, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, DEVICE), 0x0006);
        assert_eq!(command(&host, QUIET), 0x0002, "only the named function");
        assert_eq!(
            host.set_mastering(MasterTarget::Node(7), true, OWNER),
            Some(MasterChange::default()),
            "a bit already on is not written again"
        );
        assert_eq!(
            host.set_mastering(MasterTarget::Node(7), false, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, DEVICE), 0x0002);
    }

    #[test]
    fn an_owner_that_ends_after_its_successor_began_leaves_the_successor_mastering() {
        let host = rig(0, &[]);
        let node = MasterTarget::Node(7);
        assert_eq!(host.set_mastering(node, true, OWNER), Some(APPLIED));
        assert_eq!(
            host.set_mastering(node, true, OWNER + 1),
            Some(MasterChange::default())
        );
        assert_eq!(
            host.set_mastering(node, false, OWNER),
            None,
            "the earlier owner's late end"
        );
        assert_eq!(command(&host, DEVICE), BUS_MASTER_ENABLE);
        assert_eq!(host.set_mastering(node, false, OWNER + 1), Some(APPLIED));
        assert_eq!(command(&host, DEVICE), 0);
    }

    #[test]
    fn streams_name_the_handed_over_function_inside_them() {
        let host = rig(0, &[]);
        // A child published for the same device carries a range holding its
        // parent's stream, and masters through the parent's function.
        assert_eq!(
            host.set_mastering(MasterTarget::Streams(stream(0x10, 0x10)), true, OWNER),
            Some(APPLIED)
        );
        assert_eq!(command(&host, DEVICE), BUS_MASTER_ENABLE);
        assert_eq!(command(&host, QUIET), 0, "a function handed to no driver");
        assert_eq!(
            host.set_mastering(MasterTarget::Streams(stream(0x19, 4)), true, OWNER),
            None,
            "a range missing the stream names nothing"
        );
        let elsewhere = IommuStreams::new(UNIT + 1, 0x18, 1).unwrap();
        assert_eq!(
            host.set_mastering(MasterTarget::Streams(elsewhere), true, OWNER),
            None
        );
    }

    #[test]
    fn a_node_the_host_never_handed_over_names_nothing() {
        assert_eq!(
            rig(0, &[]).set_mastering(MasterTarget::Node(8), true, OWNER),
            None
        );
    }

    #[test]
    fn a_function_that_ignores_the_write_is_refused() {
        assert_eq!(
            rig(0, &[]).set_mastering(MasterTarget::Node(9), true, OWNER),
            Some(MasterChange {
                changed: true,
                refused: true,
            })
        );
    }

    #[test]
    fn a_function_that_no_longer_answers_is_passed_over() {
        assert_eq!(
            rig(0, &[DEVICE]).set_mastering(MasterTarget::Node(7), false, OWNER),
            None
        );
    }

    #[test]
    fn strays_count_every_mastering_function_behind_the_unit_without_a_window() {
        let host = rig(BUS_MASTER_ENABLE, &[]);
        assert_eq!(host.strays(UNIT, &|_| false), 2, "handed over or not");
        assert_eq!(host.strays(UNIT, &|stream| stream == 0x1F), 1);
        assert_eq!(host.strays(UNIT + 1, &|_| false), 0);
        assert_eq!(rig(0, &[]).strays(UNIT, &|_| false), 0);
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
