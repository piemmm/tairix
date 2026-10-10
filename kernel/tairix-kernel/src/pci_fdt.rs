//! The kernel's ownership of every generic ECAM host a device tree describes
//! (`plans/IOMMU.md` IOM13): each mapped, its buses numbered and its BARs
//! placed where firmware set none, probed through the one shared probe
//! against the tree's translation topology ([`FdtUnits`]), and published with
//! each function's interrupt: a message where the port routes one and the
//! function's MSI-X takes it, else its INTx resolved through the host's
//! `interrupt-map`.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ops::Range;
use core::ptr::NonNull;

use tairix_abi::driver::bus::BusDevice;
use tairix_abi::driver::msix::MsiMessage;
use tairix_abi::{DmaCoherence, HwResource, MmioMapError, MmioMapper, RegisterWindow};
use tairix_fdt::pci::{each_pci_host, InterruptSpec, PciHost, PciSpace};
use tairix_fdt::Fdt;
use tairix_kernel_core::iommu::{InterruptRouting, InterruptSource, RemapError};
use tairix_log::{Level, Sink};
use tairix_pci::topology::Topology;
use tairix_pci::{Aperture, Apertures, EcamRegion, PciResources, Windows};

use crate::boot_hwtree::CollectingHwNodeSink;
use crate::hwdiscovery::{DeviceInterrupt, DmaIdentity, PciSegment, PciWalk};
use crate::iommu_fdt::FdtUnits;
use crate::pci_host::{HostBus, MSIX_ENTRY};
use crate::pci_probe::{log_discovery, ProbeSegment};

/// What an FDT port gives the generic host bring-up.
pub trait FdtPort: Sync {
    /// A kernel mapping of the device registers at `[base, base + len)`,
    /// uncached, for the kernel's life; [`None`] where the port maps none.
    fn registers(&self, base: u64, len: usize) -> Option<NonNull<u8>>;

    /// One past the highest CPU address the port maps device registers at:
    /// a window reaching above it is not used.
    fn reach(&self) -> u64;

    /// How a master's DMA meets the CPU's caches where no node of the tree
    /// says: the architecture's devicetree convention.
    fn dma_convention(&self) -> DmaCoherence;
}

/// How a port turns a function's INTx specifier into the line it binds, its
/// interrupt controller read from the tree once.
pub trait IntxLines {
    /// The interrupt line `spec` names, where its parent is the controller
    /// the port drives.
    fn line(&self, spec: &InterruptSpec) -> Option<u32>;
}

/// Record why the message interrupts a port routes through a unit will
/// raise nothing.
pub fn log_unrouted(log: &dyn Sink, reason: &'static str) {
    tairix_log::log(
        log,
        &tairix_log::Event {
            level: Level::Warn,
            id: crate::pci_probe::DISCOVERY_EVENT,
            message: "pci functions' message interrupts left unrouted",
            fields: &[tairix_log::Field {
                key: "reason",
                value: tairix_log::FieldValue::Str(reason),
            }],
        },
    );
}

/// A port's message routing refused for `reason`, recorded.
pub fn routing_refused(log: &dyn Sink, reason: &'static str) -> InterruptRouting {
    log_unrouted(log, reason);
    InterruptRouting::Refused(RemapError::Unsupported)
}

/// A function's interrupt raised by message: what its MSI-X entry is
/// programmed with, the line its driver binds, and the doorbell its domain
/// maps so the message arrives.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MessageRoute {
    /// What the function writes, and where.
    pub message: MsiMessage,
    /// The line it raises.
    pub line: u32,
    /// Its [`tairix_abi::HwResourceKind::MsiDoorbell`].
    pub doorbell: HwResource,
}

/// How a port raises a PCI function's interrupts by message.
pub trait MessageRouter {
    /// The route `node`'s function on `host` would raise its interrupts by,
    /// its messages reaching the fabric as requester id `requester`; [`None`]
    /// where the port gives it none. Offered, not recorded:
    /// [`Self::accept`] records the last one offered once its function took
    /// it, so a function that cannot holds nothing.
    fn offer(
        &mut self,
        fdt: &Fdt<'_>,
        host: &PciHost<'_>,
        node: u32,
        requester: u16,
    ) -> Option<MessageRoute>;

    /// Record the route last offered: its function now raises it.
    fn accept(&mut self);
}

/// A port that raises no interrupt by message.
pub struct NoMessages;

impl MessageRouter for NoMessages {
    fn offer(
        &mut self,
        _fdt: &Fdt<'_>,
        _host: &PciHost<'_>,
        _node: u32,
        _requester: u16,
    ) -> Option<MessageRoute> {
        None
    }

    fn accept(&mut self) {}
}

/// The registers the kernel reaches itself through its port, on no process's
/// behalf.
struct PortRegisters<'p>(&'p dyn FdtPort);

impl MmioMapper for PortRegisters<'_> {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        let base = self
            .0
            .registers(phys_base, len)
            .ok_or(MmioMapError::InvalidRegion)?;
        // SAFETY: the port maps `len` bytes at `phys_base` uncached for the
        // kernel's life, and the window stays with the kernel, which touches
        // those registers through no other.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
    }
}

/// Byte offset of the dword holding a function's Interrupt Pin.
const INTERRUPT_REGISTERS: u16 = 0x3C;

/// Read the translation topology `fdt` describes, completing the platform
/// masters already collected into `sink`, then take every generic ECAM host
/// it describes as the kernel's own: map it, set its resources out where
/// firmware set none, probe it, publish what the probe hands to drivers into
/// `sink`, each interrupt-driven function's raised by the message `router`
/// offers where it takes one and otherwise on the line `intx` resolves its
/// pin to, and publish the kernel's PCI host, which reaches the registers it
/// programs itself through `port`. A host that cannot be mapped or described
/// is left unprobed, logged.
pub fn seed(
    fdt: &Fdt<'_>,
    port: &'static dyn FdtPort,
    intx: &dyn IntxLines,
    router: &mut dyn MessageRouter,
    sink: &mut CollectingHwNodeSink,
    log: &dyn Sink,
) {
    let mut units = FdtUnits::read(fdt, sink, log);
    let Some(memory) = memory_of(fdt) else {
        log_discovery(log, Level::Error, "memory unrecorded; pci unprobed");
        return;
    };
    let mut hosts: Vec<PciHost<'_>> = Vec::new();
    let mut segments = Vec::new();
    each_pci_host(fdt, |host| {
        if hosts.try_reserve(1).is_err() || segments.try_reserve(1).is_err() {
            log_discovery(log, Level::Error, "pci hosts unrecorded; host unprobed");
            return;
        }
        if hosts.iter().any(|known| known.segment == host.segment) {
            log_discovery(log, Level::Error, "pci segment named twice; host unprobed");
            return;
        }
        if let Some(segment) = segment_of(&host, port, &memory, log) {
            hosts.push(host);
            segments.push(segment);
        }
    });
    if hosts.is_empty() {
        return;
    }
    let router = RefCell::new(router);
    let external = |segment: u16, address: u64| {
        hosts.iter().any(|host| {
            host.segment == segment
                && u32::try_from(address).is_ok_and(|address| host.external_facing(fdt, address))
        })
    };
    let mut publish = |segment: PciSegment,
                       bus: &dyn HostBus,
                       topology: &Topology,
                       functions: &[BusDevice],
                       dma: DmaIdentity<'_>,
                       sink: &mut CollectingHwNodeSink| {
        let Some(host) = hosts.iter().find(|host| host.segment == segment.number) else {
            return;
        };
        // A pin stays quiet until an owner binds its line: one a function
        // left raised would storm the line for whichever sharer binds first.
        for function in functions {
            let _ = bus.set_intx(function.address, false);
        }
        let pin_line = |bdf: u64| {
            let pin = bus
                .read_config(bdf, INTERRUPT_REGISTERS)
                .ok()?
                .to_le_bytes()[1];
            let line = intx_line(fdt, intx, host, topology, bdf, pin)?;
            Some(tairix_abi::HwResource::irq(u64::from(line), 1))
        };
        crate::hwdiscovery::describe_virtio_units(segment.number, bus, &pin_line, sink.nodes_mut());
        let irq = |bdf: u64| {
            let routable = segment.node_id(bdf).zip(message_requester(topology, bdf));
            match (routable, router.try_borrow_mut()) {
                (Some((node, requester)), Ok(mut router)) => interrupt_of(
                    &mut **router,
                    fdt,
                    host,
                    node,
                    requester,
                    |message| {
                        crate::pci_host::route_message(bus, bdf, message, &PortRegisters(port))
                            .is_ok()
                    },
                    || pin_line(bdf),
                ),
                _ => pin_line(bdf).map(DeviceInterrupt::line),
            }
        };
        let walk = PciWalk {
            segment,
            functions,
            bus,
            registers: &PortRegisters(port),
            dma,
            coherence: host_coherence(fdt, host).unwrap_or_else(|| port.dma_convention()),
        };
        // An enumeration error leaves that class undiscovered; whatever was
        // collected is seeded regardless.
        let _ = crate::hwdiscovery::observe_virtio_pci_block_devices(&walk, sink, log);
        let _ = crate::hwdiscovery::observe_virtio_pci_network_devices(&walk, &irq, sink, log);
        let _ = crate::hwdiscovery::observe_virtio_pci_audio_devices(&walk, &irq, sink, log);
        let _ = crate::hwdiscovery::observe_virtio_pci_input_devices(&walk, &irq, sink, log);
        for class in [
            crate::hwdiscovery::XHCI_CONTROLLERS,
            crate::hwdiscovery::HD_AUDIO_CONTROLLERS,
        ] {
            let _ =
                crate::hwdiscovery::observe_pci_class_functions(&walk, bus, class, &irq, sink, log);
        }
    };
    let owned = crate::pci_probe::probe(segments, &mut units, &external, &mut publish, sink, log);
    crate::pci_host::publish(owned, Box::new(PortRegisters(port)), log);
}

/// Every memory range `fdt` names, which no BAR may decode over.
fn memory_of(fdt: &Fdt<'_>) -> Option<Vec<Range<u64>>> {
    let mut memory = Vec::new();
    let mut held = true;
    fdt.each_memory_region(|base, len| {
        held &= memory.try_reserve(1).is_ok();
        if held {
            memory.push(base..base.saturating_add(len));
        }
    })
    .ok()?;
    held.then_some(memory)
}

/// The probe's view of `host`: its configuration region mapped, its
/// resources set out unless firmware's are to be kept, its BARs resolved
/// through the windows the port reaches.
fn segment_of(
    host: &PciHost<'_>,
    port: &dyn FdtPort,
    memory: &[Range<u64>],
    log: &dyn Sink,
) -> Option<ProbeSegment> {
    let (base, len) = host.ecam;
    let over_memory = base
        .checked_add(len)
        .is_none_or(|end| memory.iter().any(|ram| base < ram.end && ram.start < end));
    if over_memory {
        log_discovery(
            log,
            Level::Error,
            "pci configuration region over memory; host unprobed",
        );
        return None;
    }
    // The kernel reaches the configuration region only through this window:
    // by the probe, then under its PCI host lock.
    let Some(registers) = usize::try_from(len)
        .ok()
        .and_then(|len| PortRegisters(port).map_window(base, len).ok())
    else {
        log_discovery(
            log,
            Level::Error,
            "pci configuration region unmappable; host unprobed",
        );
        return None;
    };
    let Some((windows, apertures)) = windows_of(host, port.reach(), memory) else {
        log_discovery(log, Level::Error, "pci windows unusable; host unprobed");
        return None;
    };
    let mut ram = Vec::new();
    let mut regions = Vec::new();
    if ram.try_reserve_exact(memory.len()).is_err() || regions.try_reserve_exact(1).is_err() {
        log_discovery(log, Level::Error, "pci host unrecorded; host unprobed");
        return None;
    }
    ram.extend_from_slice(memory);
    regions.push(EcamRegion::new(registers, host.buses.0..=host.buses.1));
    let bus = tairix_pci::mechanism_ecam(regions, Apertures::new(apertures, ram));
    if !host.probe_only {
        match bus.assign(host.buses.0..=host.buses.1, &windows) {
            Ok(assigned) if assigned.unplaced == 0 => {}
            Ok(_) => log_discovery(
                log,
                Level::Warn,
                "pci resources left unplaced; their functions decode nothing",
            ),
            Err(_) => {
                log_discovery(log, Level::Error, "pci resources unassigned; host unprobed");
                return None;
            }
        }
    }
    Some(ProbeSegment {
        number: host.segment,
        bus: Box::new(bus),
    })
}

/// How the DMA of `host`'s functions meets the CPU's caches, as its node
/// states or inherits it; [`None`] where no node on its path says.
fn host_coherence(fdt: &Fdt<'_>, host: &PciHost<'_>) -> Option<DmaCoherence> {
    tairix_arch_api::fdtwalk::emitted(fdt)
        .find(|(_, node, _)| node.offset() == host.offset())
        .and_then(|(_, _, coherence)| coherence)
}

/// The windows `host`'s resources go in, the first of each space, and the
/// apertures its memory BARs resolve through: every window reaching no
/// higher than `reach`, over none of `memory` and not over the host's own
/// configuration region. A 64-bit window wholly below the 4 GiB line serves
/// as memory there. [`None`] for memory windows that overlap, or no room to
/// hold them.
fn windows_of(
    host: &PciHost<'_>,
    reach: u64,
    memory: &[Range<u64>],
) -> Option<(Windows, Vec<Aperture>)> {
    let mut windows = Windows::default();
    let mut apertures = Vec::new();
    apertures
        .try_reserve_exact(tairix_fdt::pci::MAX_PCI_WINDOWS)
        .ok()?;
    let overlap = |a: &Range<u64>, b: &Range<u64>| a.start < b.end && b.start < a.end;
    let ecam = host.ecam.0..host.ecam.0.saturating_add(host.ecam.1);
    let reached = host.windows().filter(|window| {
        let Some(end) = window.cpu.checked_add(window.size) else {
            return false;
        };
        let cpu = window.cpu..end;
        end <= reach && !overlap(&cpu, &ecam) && !memory.iter().any(|ram| overlap(&cpu, ram))
    });
    for window in reached {
        let span = window.pci..window.pci.checked_add(window.size)?;
        let slot = match window.space {
            PciSpace::Io => &mut windows.io,
            PciSpace::Memory64 if span.end > 1 << 32 => &mut windows.wide,
            PciSpace::Memory32 | PciSpace::Memory64 => &mut windows.memory,
        };
        slot.get_or_insert_with(|| span.clone());
        if window.space != PciSpace::Io {
            apertures.push(Aperture {
                pci: span,
                cpu: window.cpu,
            });
        }
    }
    // Two windows decoding one address would place two BARs on it.
    let cpu = |aperture: &Aperture| {
        let start = aperture.cpu;
        start..start.saturating_add(aperture.pci.end - aperture.pci.start)
    };
    for (index, first) in apertures.iter().enumerate() {
        for second in &apertures[index + 1..] {
            if overlap(&first.pci, &second.pci) || overlap(&cpu(first), &cpu(second)) {
                return None;
            }
        }
    }
    Some((windows, apertures))
}

/// The interrupt `node`'s function is granted: the message route `router`
/// offers, where its MSI-X entry takes it (`route`), else the INTx line
/// `intx` resolves.
fn interrupt_of(
    router: &mut dyn MessageRouter,
    fdt: &Fdt<'_>,
    host: &PciHost<'_>,
    node: u32,
    requester: u16,
    route: impl FnOnce(MsiMessage) -> bool,
    intx: impl FnOnce() -> Option<HwResource>,
) -> Option<DeviceInterrupt> {
    if let Some(offered) = router.offer(fdt, host, node, requester) {
        if route(offered.message) {
            router.accept();
            return Some(DeviceInterrupt {
                line: HwResource::message_irq(u64::from(offered.line), MSIX_ENTRY),
                doorbell: Some(offered.doorbell),
            });
        }
    }
    intx().map(DeviceInterrupt::line)
}

/// The requester id the function at `bdf`'s messages reach the fabric as: its
/// own, the topmost conventional bridge's above it, or, below a bridge from
/// PCI Express to conventional PCI, the bridge's secondary bus at function
/// zero, which it tags the requests it takes over with.
fn message_requester(topology: &Topology, bdf: u64) -> Option<u16> {
    let index = topology.index_of(bdf)?;
    match crate::pci_probe::interrupt_source(topology, index, None) {
        InterruptSource::Requester(requester) => Some(requester),
        InterruptSource::Buses { first, .. } => Some(u16::from(first) << 8),
    }
}

/// The interrupt line the function at `bdf` raises its INTx pin `pin` on:
/// swizzled to the root bus, through the host's map to the controller the
/// port drives.
fn intx_line(
    fdt: &Fdt<'_>,
    intx: &dyn IntxLines,
    host: &PciHost<'_>,
    topology: &Topology,
    bdf: u64,
    pin: u8,
) -> Option<u32> {
    let (device, pin) = topology.intx_at_root(topology.index_of(bdf)?, pin)?;
    intx.line(&host.intx(fdt, device, pin)?)
}

#[cfg(test)]
#[path = "pci_fdt_tests.rs"]
mod tests;
