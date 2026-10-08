//! What the kernel's PCI probe makes of the segments it owns, whatever the
//! port (`plans/IOMMU.md` IOM7–IOM10): how a translation unit knows each
//! function's DMA, which functions the probe stops mastering before any unit
//! takes over, and the record of the functions whose configuration space the
//! kernel owns.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::bus::BusDevice;
use tairix_abi::driver::pci::{PciAddress, PciBus, BUS_MASTER_ENABLE, COMMAND_OFFSET};
use tairix_abi::driver::virtio_pci::VIRTIO_PCI_VENDOR_ID;
use tairix_abi::{
    DriverError, HwDeviceClass, HwNode, IommuGroup, IommuStreams, HW_NODE_MAX_RESOURCES,
};
use tairix_arch_api::HwNodeSink;
use tairix_inline::ArrayVec;
use tairix_kernel_core::iommu::InterruptSource;
use tairix_log::{Event, EventId, Field, FieldValue, Level, Sink};
use tairix_pci::topology::{
    Confinement, Function as PciFunction, Header, PciTopology, PortType, Topology,
};

use crate::boot_hwtree::CollectingHwNodeSink;
use crate::hwdiscovery::{DmaIdentity, PciSegment};
use crate::pci_host::{Bridge, Function, HostBus, HostSegment};

/// Stable id for the probe's DMA translation discovery: a segment or a unit
/// the kernel cannot describe leaves the DMA of every device behind it
/// unconfined, or unpublished.
pub const DISCOVERY_EVENT: EventId = EventId(4103);

/// The aliases one function can carry: its node's resources bound them.
pub type Aliases = ArrayVec<IommuStreams, HW_NODE_MAX_RESOURCES>;

/// How a translation unit knows one function's DMA.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionDma {
    /// Its own stream, on the unit translating it.
    pub stream: IommuStreams,
    /// The further streams the fabric delivers its DMA as.
    pub aliases: Aliases,
    /// Its isolation group, named on its unit by the least stream of its
    /// members; [`None`] where no one unit can confine it — its group's
    /// members sit behind more than one unit, the fabric tags its DMA with
    /// more streams than a node can name or on another unit, a stream it
    /// masters DMA as is another group's or another master's too, or it sits
    /// below an external-facing port that does not validate requester ids.
    pub group: Option<IommuGroup>,
    /// It sits below an external-facing port, where nothing is trusted.
    pub untrusted: bool,
    /// The requester ids its interrupt messages can carry, as far as the
    /// fabric lets its unit tell: what a remapping entry for it admits.
    pub interrupts: InterruptSource,
}

/// What the units behind one segment say of a requester id's DMA.
pub trait Streams {
    /// The unit node and the stream id `requester` masters DMA as, where a
    /// unit translates it.
    fn stream(&self, requester: u16) -> Option<(u32, u32)>;

    /// The requester id firmware says `requester`'s DMA arrives at its unit
    /// as, where it names another: an AMD-Vi IVRS alias.
    fn firmware_alias(&self, requester: u16) -> Option<u16>;

    /// Whether a master outside the segment — a function of another, or a
    /// platform device — masters DMA as stream `stream` on unit `unit` too:
    /// no function using it can be kept apart from that master.
    fn contested(&self, unit: u32, stream: u32) -> bool;
}

/// How the units behind one segment know each of its functions' DMA.
pub struct SegmentDma<'t> {
    topology: &'t Topology,
    /// Each function's, by its index in the topology.
    functions: Vec<Option<FunctionDma>>,
}

impl<'t> SegmentDma<'t> {
    /// Each function of `topology`'s DMA identity, where `streams` names the
    /// unit translating a requester id, the stream it knows it by, and the id
    /// firmware says it arrives as. The fabric delivers a function's DMA on
    /// its own unit under every alias, so a group is one unit's exactly when
    /// every member behind a unit is behind that one.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when the identities cannot be held, and
    /// [`DriverError::DeviceFault`] for a function behind a unit whose streams
    /// cannot be named: it is never published as an untranslated one.
    pub fn new(topology: &'t Topology, streams: &dyn Streams) -> Result<Self, DriverError> {
        let count = topology.functions().len();
        let mut units: Vec<Option<(u32, u32)>> = Vec::new();
        let mut firmware: Vec<Option<u16>> = Vec::new();
        units
            .try_reserve_exact(count)
            .and_then(|()| firmware.try_reserve_exact(count))
            .map_err(|_| DriverError::NoSpace)?;
        units.extend(
            topology
                .functions()
                .iter()
                .map(|f| streams.stream(f.requester_id())),
        );
        firmware.extend(
            topology
                .functions()
                .iter()
                .map(|f| streams.firmware_alias(f.requester_id())),
        );
        let mut groups: Vec<(u16, u32)> = Vec::new();
        groups
            .try_reserve_exact(count)
            .map_err(|_| DriverError::NoSpace)?;
        groups.extend(
            units
                .iter()
                .enumerate()
                .filter_map(|(index, unit)| Some((topology.group(index), (*unit)?.0))),
        );
        groups.sort_unstable();
        groups.dedup();
        let spans = |group: u16| {
            let first = groups.partition_point(|&(at, _)| at < group);
            groups[first..]
                .iter()
                .take_while(|&&(at, _)| at == group)
                .count()
                > 1
        };
        let mut functions = Vec::new();
        functions
            .try_reserve_exact(count)
            .map_err(|_| DriverError::NoSpace)?;
        for (index, unit) in units.iter().enumerate() {
            let fabric = Fabric {
                streams,
                firmware: firmware[index],
            };
            functions.push(
                unit.map(|own| identity(topology, index, own, &fabric, &spans))
                    .transpose()?,
            );
        }
        settle_groups(topology, &mut functions, streams)?;
        Ok(Self {
            topology,
            functions,
        })
    }

    /// How a unit knows the DMA of the function at configuration `address`;
    /// [`None`] for one no unit translates.
    #[must_use]
    pub fn of(&self, address: u64) -> Option<&FunctionDma> {
        self.topology
            .index_of(address)
            .and_then(|index| self.functions[index].as_ref())
    }
}

/// What [`identity`] reads of the units and of firmware for one function.
struct Fabric<'f> {
    streams: &'f dyn Streams,
    /// The id firmware says its DMA arrives as.
    firmware: Option<u16>,
}

/// Settle every group across the segment once each function's streams are
/// known. A stream two groups' functions master DMA as, or one a master
/// outside the segment uses too, cannot keep them apart, so every function
/// using it is left unconfined — the one aliased to as well as the one
/// aliasing, as either could master into the other's domain. Every other
/// group is named on its unit by its members' least stream: unique there
/// however many segments and platform masters the unit translates, where a
/// requester id is unique only on its segment.
fn settle_groups(
    topology: &Topology,
    functions: &mut [Option<FunctionDma>],
    streams: &dyn Streams,
) -> Result<(), DriverError> {
    // (unit, stream, group, function) for every stream any function uses.
    let mut claims: Vec<(u32, u32, u16, usize)> = Vec::new();
    let mut least: Vec<(u16, u32)> = Vec::new();
    let total = functions
        .iter()
        .flatten()
        .try_fold(0usize, |total, identity| {
            identity
                .aliases
                .as_slice()
                .iter()
                .chain(core::iter::once(&identity.stream))
                .try_fold(total, |total, range| {
                    total.checked_add(usize::try_from(range.count()).ok()?)
                })
        });
    claims
        .try_reserve_exact(total.ok_or(DriverError::NoSpace)?)
        .and_then(|()| least.try_reserve_exact(functions.len()))
        .map_err(|_| DriverError::NoSpace)?;
    for (index, identity) in functions.iter().enumerate() {
        let Some(identity) = identity else {
            continue;
        };
        let group = topology.group(index);
        least.push((group, identity.stream.first()));
        for range in core::iter::once(&identity.stream).chain(identity.aliases.as_slice()) {
            let ids = range.first()..=range.first() + (range.count() - 1);
            claims.extend(ids.map(|stream| (range.unit(), stream, group, index)));
        }
    }
    claims.sort_unstable();
    least.sort_unstable();
    let mut unconfined = Vec::new();
    unconfined
        .try_reserve_exact(functions.len())
        .map_err(|_| DriverError::NoSpace)?;
    unconfined.resize(functions.len(), false);
    for run in claims.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)) {
        let (unit, stream, group, _) = run[0];
        if run.iter().any(|claim| claim.2 != group) || streams.contested(unit, stream) {
            for claim in run {
                unconfined[claim.3] = true;
            }
        }
    }
    for (index, identity) in functions.iter_mut().enumerate() {
        let Some(identity) = identity else {
            continue;
        };
        let group = topology.group(index);
        identity.group = match identity.group {
            Some(_) if unconfined[index] => None,
            Some(confined) => {
                let first = least.partition_point(|&(at, _)| at < group);
                least
                    .get(first)
                    .map(|&(_, stream)| IommuGroup::new(confined.unit(), stream))
            }
            None => None,
        };
    }
    Ok(())
}

/// The identity of the function at `index`, which masters DMA as stream
/// `own.1` on the unit at node `own.0`; its group is provisional until
/// [`settle_groups`] names it.
fn identity(
    topology: &Topology,
    index: usize,
    (unit, own): (u32, u32),
    fabric: &Fabric<'_>,
    spans: &dyn Fn(u16) -> bool,
) -> Result<FunctionDma, DriverError> {
    let stream = IommuStreams::new(unit, own, 1).map_err(|_| DriverError::DeviceFault)?;
    let mut aliases = Aliases::new();
    let untrusted = topology.untrusted(index);
    let mut confinable = untrusted.is_none_or(|untrusted| untrusted.confinable);
    let walked = topology.aliases(index).map(|alias| alias.requester);
    for requester in walked.chain(fabric.firmware) {
        match fabric.streams.stream(requester) {
            Some((alias_unit, alias_stream)) if alias_unit == unit => {
                if alias_stream == own
                    || aliases.as_slice().iter().any(|a| a.first() == alias_stream)
                {
                    continue;
                }
                let range = IommuStreams::new(unit, alias_stream, 1)
                    .map_err(|_| DriverError::DeviceFault)?;
                confinable &= aliases.try_push(range).is_ok();
            }
            // The fabric delivers the DMA under an id this unit does not
            // translate: no one domain can hold all of it.
            _ => confinable = false,
        }
    }
    let group = topology.group(index);
    Ok(FunctionDma {
        stream,
        aliases,
        group: (confinable && !spans(group)).then(|| IommuGroup::new(unit, u32::from(group))),
        untrusted: untrusted.is_some(),
        interrupts: interrupt_source(topology, index, fabric.firmware),
    })
}

/// The requester ids the function at `index`'s interrupt messages reach its
/// unit as. An interrupt message is a write like its DMA, so it arrives as
/// the id firmware says the fabric tags its requests with, where firmware
/// names one: firmware knows a fabric no walk of the topology sees.
/// Otherwise its own; the bus range below the topmost bridge to conventional
/// PCI above it, which tags them with its secondary bus; or the topmost
/// conventional bridge's own id — Linux's `set_msi_sid`, without
/// device-specific aliases.
pub(crate) fn interrupt_source(
    topology: &Topology,
    index: usize,
    firmware: Option<u16>,
) -> InterruptSource {
    if let Some(alias) = firmware {
        return InterruptSource::Requester(alias);
    }
    let function = &topology.functions()[index];
    let Some(top) = topology.aliases(index).last() else {
        return InterruptSource::Requester(function.requester_id());
    };
    let bridge = &topology.functions()[top.bridge];
    match (bridge.express, bridge.header) {
        (Some(PortType::PcieToPci), Header::Bridge { secondary, .. }) => InterruptSource::Buses {
            first: secondary,
            last: function.bus(),
        },
        _ => InterruptSource::Requester(top.requester),
    }
}

/// Stop every function of `topology` that TAIRiX takes from firmware
/// mastering DMA, before any unit is taken over: TAIRiX makes a function a
/// bus master only as it hands it to an owner (`plans/IOMMU.md` IOM7).
///
/// That is every virtio function, which the probe hands to drivers, and every
/// function behind a unit (`dma` knows it), whose stream the unit blocks
/// anyway — whether or not its unit then comes up, since a function firmware
/// keeps no window for has no claim on DMA after the hand-off. A function
/// whose stream firmware keeps a window for (`keeps`) keeps mastering, as
/// firmware still uses it. A function that masters nothing of its own
/// ([`tairix_pci::topology::Function::masters_dma`]) is left alone, as is
/// every other function behind no unit: nothing would confine it. A bit
/// already clear is not written.
///
/// # Errors
///
/// The first configuration access that fails; the functions after it are not
/// reached.
pub fn stop_mastering(
    bus: &dyn PciBus,
    topology: &Topology,
    dma: &SegmentDma<'_>,
    keeps: &dyn Fn(IommuStreams) -> bool,
) -> Result<(), DriverError> {
    for function in topology.functions().iter().filter(|f| f.masters_dma()) {
        let taken = match dma.of(function.address) {
            Some(identity) => !keeps(identity.stream),
            None => function.vendor == VIRTIO_PCI_VENDOR_ID,
        };
        if !taken {
            continue;
        }
        let command = bus.read_config(function.address, COMMAND_OFFSET)?;
        if command != u32::MAX && command & BUS_MASTER_ENABLE != 0 {
            bus.set_bus_master(function.address, false)?;
        }
    }
    Ok(())
}

/// Whether a function found by a walk that formed no hierarchy is stopped
/// mastering. No unit's scope can be resolved without the hierarchy, so
/// where a unit covers the segment (`covered`) every function that masters
/// DMA of its own is taken to be behind one, no firmware window can be
/// vouched for, and every bridge is stopped too, so nothing below it reaches
/// memory through it however its buses were numbered; elsewhere only a
/// virtio function, which TAIRiX would have driven, is stopped, as
/// [`stop_mastering`] stops one behind no unit. A host bridge, the root
/// complex's own function, is never stopped.
#[must_use]
pub fn stopped_unresolved(function: &PciFunction, covered: bool) -> bool {
    if covered {
        function.masters_dma() || function.is_bridge()
    } else {
        function.masters_dma() && function.vendor == VIRTIO_PCI_VENDOR_ID
    }
}

/// The functions of segment `segment` whose bus mastering the kernel owns:
/// each one behind a unit that masters DMA of its own, published or not, and
/// each node the probe published on the segment (`published`), known by the
/// address the probe itself recorded.
///
/// # Errors
///
/// [`DriverError::NoSpace`] when the record cannot be held.
pub fn record_functions(
    segment: u16,
    topology: &Topology,
    dma: &SegmentDma<'_>,
    published: &[HwNode],
) -> Result<Vec<Function>, DriverError> {
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(topology.functions().len() + published.len())
        .map_err(|_| DriverError::NoSpace)?;
    functions.extend(
        topology
            .functions()
            .iter()
            .filter(|function| function.masters_dma())
            .filter_map(|function| {
                dma.of(function.address).map(|identity| Function {
                    address: function.address,
                    node: None,
                    stream: Some(identity.stream),
                    interrupts: Some(identity.interrupts),
                })
            }),
    );
    // Ascending by address, as the topology holds them.
    let behind = functions.len();
    for node in published {
        let function = PciAddress::from_node_address(node.address());
        if function.segment() != segment {
            continue;
        }
        let address = function.config_address();
        match functions[..behind].binary_search_by_key(&address, |function| function.address) {
            Ok(at) => functions[at].node = Some(node.id()),
            Err(_) => functions.push(Function {
                address,
                node: Some(node.id()),
                stream: None,
                interrupts: None,
            }),
        }
    }
    Ok(functions)
}

/// The bus a port hands the probe for one segment: what the kernel then owns
/// it through.
pub trait SegmentBus: HostBus + PciTopology + Send {}

impl<B: HostBus + PciTopology + Send + ?Sized> SegmentBus for B {}

/// One segment the port owns, as it hands it to the probe.
pub struct ProbeSegment {
    /// The segment's number.
    pub number: u16,
    /// The bus that reaches it.
    pub bus: Box<dyn SegmentBus>,
}

/// How a platform's DMA translation units cover the segments: the port's
/// reading of its firmware (a DMAR, an IVRS, a device tree's `iommu-map`).
pub trait UnitTopology {
    /// Whether a unit covers `segment`.
    fn covers(&self, segment: u16) -> bool;

    /// Emit one node per unit into `sink`, each keeping the firmware windows
    /// of the functions it translates, resolved through `walks` — every
    /// segment whose walk formed a hierarchy. A unit whose windows cannot be
    /// resolved comes up keeping none, so it blocks every stream.
    ///
    /// # Errors
    ///
    /// [`Unconfined`] when the units cannot be described: nothing behind any
    /// of them can be confined.
    fn emit(
        &mut self,
        walks: &[(u16, &Topology)],
        sink: &mut dyn HwNodeSink,
        log: &dyn Sink,
    ) -> Result<(), Unconfined>;

    /// Whether the translation of `segment` cannot be described — a unit
    /// covering it was left without a node, or firmware's map of its
    /// requester ids cannot be read — so nothing on it can be confined.
    fn strands(&self, segment: u16) -> bool;

    /// The unit node and the stream the function with requester id
    /// `requester` on `segment` masters DMA as, `walk` being the segment's
    /// hierarchy; [`None`] where no unit with a node translates it.
    fn stream(&self, segment: u16, walk: &Topology, requester: u16) -> Option<(u32, u32)>;

    /// The requester id firmware says the DMA of `requester` on `segment`
    /// arrives at its unit as, where it names another.
    fn firmware_alias(&self, segment: u16, requester: u16) -> Option<u16>;

    /// Whether a master off `segment` masters DMA as stream `stream` on unit
    /// `unit` too: a function of another segment, or a platform device.
    fn contested(&self, segment: u16, unit: u32, stream: u32) -> bool;
}

/// The units a malformed firmware table described: there may be units on
/// any segment, and none can be read, so every segment's translation is
/// undescribed and no function on it is published.
pub struct Undescribed;

impl UnitTopology for Undescribed {
    fn covers(&self, _segment: u16) -> bool {
        true
    }

    fn emit(
        &mut self,
        _walks: &[(u16, &Topology)],
        _sink: &mut dyn HwNodeSink,
        _log: &dyn Sink,
    ) -> Result<(), Unconfined> {
        Ok(())
    }

    fn strands(&self, _segment: u16) -> bool {
        true
    }

    fn stream(&self, _segment: u16, _walk: &Topology, _requester: u16) -> Option<(u32, u32)> {
        None
    }

    fn firmware_alias(&self, _segment: u16, _requester: u16) -> Option<u16> {
        None
    }

    fn contested(&self, _segment: u16, _unit: u32, _stream: u32) -> bool {
        false
    }
}

/// One segment's view of the units, as [`SegmentDma`] reads it.
struct SegmentStreams<'u> {
    units: &'u dyn UnitTopology,
    segment: u16,
    topology: &'u Topology,
}

impl Streams for SegmentStreams<'_> {
    fn stream(&self, requester: u16) -> Option<(u32, u32)> {
        self.units.stream(self.segment, self.topology, requester)
    }

    fn firmware_alias(&self, requester: u16) -> Option<u16> {
        self.units.firmware_alias(self.segment, requester)
    }

    fn contested(&self, unit: u32, stream: u32) -> bool {
        self.units.contested(self.segment, unit, stream)
    }
}

/// What every segment's probe reads of the units beside its own walk.
#[derive(Copy, Clone)]
struct Translation<'t> {
    units: &'t dyn UnitTopology,
    windows: &'t FirmwareWindows,
}

/// The streams firmware keeps a window for, by unit node: what the unit
/// nodes the emission placed say, so a function whose stream is one keeps
/// mastering through the hand-off.
struct FirmwareWindows(Vec<(u32, u32)>);

impl FirmwareWindows {
    /// The windows every unit node among `nodes` keeps, or [`None`] when they
    /// cannot be held.
    fn of(nodes: &[HwNode]) -> Option<Self> {
        let mut windows = Vec::new();
        for node in nodes
            .iter()
            .filter(|node| node.class() == Some(HwDeviceClass::Iommu))
        {
            for window in node
                .resources()
                .iter()
                .filter_map(|r| r.iommu_reserved().ok())
            {
                windows.try_reserve(1).ok()?;
                windows.push((node.id(), window.stream()));
            }
        }
        Some(Self(windows))
    }

    fn keeps(&self, streams: IommuStreams) -> bool {
        self.0
            .iter()
            .any(|&(unit, stream)| unit == streams.unit() && streams.contains(stream))
    }
}

/// A segment whose functions cannot be confined: none is published, and
/// every one that could master past a unit is stopped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Unconfined;

/// What a port publishes of one segment: every function among `functions`
/// it hands to a driver, emitted into `sink`, the walk's `Topology` at hand
/// for what the hierarchy decides (a function's INTx, swizzled to the root).
pub type Publish<'a> = &'a mut dyn FnMut(
    PciSegment,
    &dyn HostBus,
    &Topology,
    &[BusDevice],
    DmaIdentity<'_>,
    &mut CollectingHwNodeSink,
);

/// Walk every segment in `segments` once — confining those a unit covers
/// and taking `external` (by segment and configuration address) for the
/// ports the platform describes as external-facing — emit the units, then
/// for each segment set out its functions' DMA identities, stop every
/// function TAIRiX takes from firmware, and `publish` it; and answer the
/// segments the kernel now owns, numbered by their position in `segments`.
///
/// A segment that cannot be confined publishes nothing, and a flat scan
/// stops every function and bridge on it that could master past a unit.
pub fn probe(
    segments: Vec<ProbeSegment>,
    units: &mut dyn UnitTopology,
    external: &dyn Fn(u16, u64) -> bool,
    publish: Publish<'_>,
    sink: &mut CollectingHwNodeSink,
    log: &dyn Sink,
) -> Vec<HostSegment> {
    let mut walks = Vec::new();
    let mut formed = Vec::new();
    let mut owned = Vec::new();
    if walks.try_reserve_exact(segments.len()).is_err()
        || formed.try_reserve_exact(segments.len()).is_err()
        || owned.try_reserve_exact(segments.len()).is_err()
    {
        log_discovery(log, Level::Error, "pci segments unrecorded; none published");
        return quiesce_all(segments, &*units, log);
    }
    for segment in &segments {
        let confinement = if units.covers(segment.number) {
            Confinement::Confine
        } else {
            Confinement::Leave
        };
        let number = segment.number;
        let walk = segment
            .bus
            .topology(confinement, &|address| external(number, address));
        if walk.is_err() {
            log_discovery(
                log,
                Level::Error,
                "pci hierarchy unreadable; none published",
            );
        }
        walks.push(walk.ok());
    }
    formed.extend(
        segments
            .iter()
            .zip(&walks)
            .filter_map(|(segment, walk)| Some((segment.number, walk.as_ref()?))),
    );
    let described = units
        .emit(&formed, sink, log)
        .and_then(|()| FirmwareWindows::of(sink.nodes()).ok_or(Unconfined));
    if described.is_err() {
        log_discovery(
            log,
            Level::Error,
            "dma translation units undiscovered; none published",
        );
    }
    let none = FirmwareWindows(Vec::new());
    let windows = described.as_ref().unwrap_or(&none);
    for (ordinal, (segment, walk)) in segments.into_iter().zip(&walks).enumerate() {
        let covered = units.covers(segment.number);
        let (probed, decoded) = match walk {
            Some(_) if covered && described.is_err() => (Err(Unconfined), Vec::new()),
            Some(_) if units.strands(segment.number) => {
                log_discovery(
                    log,
                    Level::Error,
                    "segment's dma translation undescribed; none published",
                );
                (Err(Unconfined), Vec::new())
            }
            Some(topology) => {
                let translation = Translation {
                    units: &*units,
                    windows,
                };
                probe_walked(&segment, ordinal, topology, translation, publish, sink, log)
            }
            None => (Err(Unconfined), Vec::new()),
        };
        let bridges = match (&probed, walk) {
            (Ok(_), Some(topology)) => bridges_of(topology).unwrap_or_else(|| {
                log_discovery(
                    log,
                    Level::Error,
                    "pci bridges unrecorded; nothing below one masters",
                );
                Vec::new()
            }),
            _ => Vec::new(),
        };
        let functions = probed.unwrap_or_else(|Unconfined| {
            log_quiesced(
                log,
                segment
                    .bus
                    .quiesce(&|function| stopped_unresolved(function, covered)),
            );
            Vec::new()
        });
        owned.push(
            HostSegment::new(segment.number, segment.bus, functions, bridges).decoding(decoded),
        );
    }
    owned
}

/// Probe `segment`, the `ordinal`th, over its `topology`: what
/// [`probe_segment`] publishes of it, and the windows its bridges decode,
/// where a translated device's IOVA must not land.
fn probe_walked(
    segment: &ProbeSegment,
    ordinal: usize,
    topology: &Topology,
    translation: Translation<'_>,
    publish: Publish<'_>,
    sink: &mut CollectingHwNodeSink,
    log: &dyn Sink,
) -> (Result<Vec<Function>, Unconfined>, Vec<Range<u64>>) {
    let Ok(decoded) = segment.bus.decoded_windows(topology) else {
        log_discovery(log, Level::Error, "pci windows unrecorded; none published");
        return (Err(Unconfined), Vec::new());
    };
    let at = PciSegment {
        number: segment.number,
        ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
    };
    let probed = probe_segment(&*segment.bus, at, topology, translation, publish, sink, log);
    (probed, decoded)
}

/// Every bridge `topology` gave buses, each to forward its buses' DMA once a
/// function on one of them masters; [`None`] where they cannot be held.
fn bridges_of(topology: &Topology) -> Option<Vec<Bridge>> {
    let numbered = |function: &PciFunction| match function.header {
        Header::Bridge {
            secondary,
            subordinate,
            ..
        } if secondary != 0 => Some(Bridge {
            address: function.address,
            secondary,
            subordinate,
        }),
        Header::Bridge { .. } | Header::Endpoint => None,
    };
    let mut bridges = Vec::new();
    bridges
        .try_reserve_exact(topology.functions().iter().filter_map(numbered).count())
        .ok()?;
    bridges.extend(topology.functions().iter().filter_map(numbered));
    Some(bridges)
}

/// [`probe`]'s answer when the walks cannot even be recorded: every segment
/// stopped over a flat scan, and owned with no function handed over where
/// the record can still be held.
fn quiesce_all(
    segments: Vec<ProbeSegment>,
    units: &dyn UnitTopology,
    log: &dyn Sink,
) -> Vec<HostSegment> {
    let mut owned = Vec::new();
    let room = owned.try_reserve_exact(segments.len()).is_ok();
    for segment in segments {
        let covered = units.covers(segment.number);
        log_quiesced(
            log,
            segment
                .bus
                .quiesce(&|function| stopped_unresolved(function, covered)),
        );
        if room {
            owned.push(HostSegment::new(
                segment.number,
                segment.bus,
                Vec::new(),
                Vec::new(),
            ));
        }
    }
    owned
}

/// Set out segment `at`'s DMA identities from `topology`, its one walk of
/// `bus`, stop every function TAIRiX takes from firmware, publish it, and
/// answer the functions whose configuration space the kernel now owns.
///
/// Before anything is routed or published, every function TAIRiX takes from
/// firmware stops mastering DMA ([`stop_mastering`]), and nothing here makes
/// one a bus master again: its owner's attached domain, or its owner's first
/// carve, does.
fn probe_segment(
    bus: &dyn SegmentBus,
    at: PciSegment,
    topology: &Topology,
    translation: Translation<'_>,
    publish: Publish<'_>,
    sink: &mut CollectingHwNodeSink,
    log: &dyn Sink,
) -> Result<Vec<Function>, Unconfined> {
    let Translation { units, windows } = translation;
    let streams = SegmentStreams {
        units,
        segment: at.number,
        topology,
    };
    let Ok(identities) = SegmentDma::new(topology, &streams) else {
        log_discovery(
            log,
            Level::Error,
            "pci dma identities unrecorded; none published",
        );
        return Err(Unconfined);
    };
    if stop_mastering(bus, topology, &identities, &|stream| windows.keeps(stream)).is_err() {
        log_discovery(
            log,
            Level::Error,
            "pci functions left mastering; none published",
        );
        return Err(Unconfined);
    }
    if units.covers(at.number) {
        log_services_left_on(log, topology);
    }
    let mut functions = Vec::new();
    if functions
        .try_reserve_exact(topology.functions().len())
        .is_err()
    {
        log_discovery(
            log,
            Level::Error,
            "pci functions unrecorded; none published",
        );
        return Err(Unconfined);
    }
    functions.extend(topology.functions().iter().map(PciFunction::bus_device));
    let first = sink.nodes().len();
    let dma = |address: u64| identities.of(address).cloned();
    publish(at, bus, topology, &functions, &dma, sink);
    Ok(
        record_functions(at.number, topology, &identities, &sink.nodes()[first..]).unwrap_or_else(
            |_| {
                log_discovery(log, Level::Error, "pci functions unrecorded; none masters");
                Vec::new()
            },
        ),
    )
}

/// Record a discovery outcome under [`DISCOVERY_EVENT`].
pub fn log_discovery(log: &dyn Sink, level: Level, message: &'static str) {
    tairix_log::log(
        log,
        &Event {
            level,
            id: DISCOVERY_EVENT,
            message,
            fields: &[],
        },
    );
}

/// Record how many functions of a confined walk read back with an address
/// translation service or virtual functions still on: the unit refuses a
/// translated or PASID-tagged request either way, and blocks every stream
/// no owner holds, but a device that ignores the write is worth naming.
fn log_services_left_on(log: &dyn Sink, topology: &Topology) {
    let on = |function: &&PciFunction| {
        function.ats.is_some_and(|ats| ats.enabled)
            || function.pri.is_some_and(|pri| pri.enabled)
            || function.pasid.is_some_and(|pasid| pasid.enabled)
            || function.sriov.is_some_and(|sriov| sriov.enabled)
    };
    let count = topology.functions().iter().filter(on).count();
    if count == 0 {
        return;
    }
    tairix_log::log(
        log,
        &Event {
            level: Level::Warn,
            id: DISCOVERY_EVENT,
            message: "pci functions left translation services or virtual functions on",
            fields: &[Field {
                key: "functions",
                value: FieldValue::UnsignedInt(count as u64),
            }],
        },
    );
}

/// Record what the flat scan of an unconfined segment stopped, and what read
/// back mastering still.
fn log_quiesced(log: &dyn Sink, quiesced: tairix_abi::driver::pci::Quiesced) {
    let count = |count: usize| FieldValue::UnsignedInt(count as u64);
    tairix_log::log(
        log,
        &Event {
            level: if quiesced.refused == 0 {
                Level::Warn
            } else {
                Level::Error
            },
            id: DISCOVERY_EVENT,
            message: "pci functions and bridges stopped mastering over a flat scan",
            fields: &[
                Field {
                    key: "stopped",
                    value: count(quiesced.stopped),
                },
                Field {
                    key: "refused",
                    value: count(quiesced.refused),
                },
            ],
        },
    );
}

#[cfg(test)]
#[path = "pci_probe_tests.rs"]
pub(crate) mod tests;
