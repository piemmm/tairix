//! The PCI hierarchy as its configuration-space owner walks it once: every
//! function, the bridges between them, and what a DMA translation unit needs
//! of them — the requester ids a function's DMA arrives under, and which
//! functions the fabric cannot keep apart (`plans/IOMMU.md` IOM8).
//!
//! Aliasing follows the PCI Express to PCI/PCI-X Bridge Specification rev. 1.0
//! §2.3. Isolation follows ACS (PCI Express Base Specification rev. 5.0
//! §6.12) the way Linux's IOMMU grouping applies it, with no device-specific
//! exceptions: a function the rules cannot prove isolated is grouped.

use alloc::vec::Vec;

use tairix_abi::driver::bus::BusDevice;
use tairix_abi::driver::pci::{config_address, function_of, requester_id, Quiesced};
use tairix_abi::DriverError;

/// Buses one PCI segment holds: every value of a bus number.
pub const BUSES: usize = u8::MAX as usize + 1;

/// The base class and subclass of a host bridge (PCI Code and ID Assignment
/// Specification rev. 1.11 §1.7).
const HOST_BRIDGE: u32 = 0x06_00;

/// What a function's header says it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Header {
    /// A type-0 header: an endpoint, which may master DMA.
    Endpoint,
    /// A PCI-to-PCI or `CardBus` bridge forwarding to buses `secondary` through
    /// `subordinate`. A `secondary` of zero was never assigned and forwards to
    /// nothing.
    Bridge {
        /// The bus directly below the bridge.
        secondary: u8,
        /// The last bus below the bridge.
        subordinate: u8,
        /// The port forwards to every function number of the one device
        /// below it, so the whole secondary bus is that device's (ARI
        /// Forwarding Enable, PCI Express Base 5.0 §7.5.3.16).
        ari: bool,
    },
}

/// A PCI Express function's device or port type (PCI Express Base 5.0
/// §7.5.3.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PortType {
    /// An endpoint.
    Endpoint,
    /// A legacy endpoint.
    LegacyEndpoint,
    /// A root port of the root complex.
    RootPort,
    /// A switch's upstream port.
    UpstreamPort,
    /// A switch's downstream port.
    DownstreamPort,
    /// A bridge to conventional PCI or PCI-X, which takes ownership of the
    /// requests below it.
    PcieToPci,
    /// A bridge from conventional PCI or PCI-X.
    PciToPcie,
    /// An endpoint integrated into the root complex.
    IntegratedEndpoint,
    /// A root complex event collector.
    EventCollector,
    /// A value the specification reserves.
    Reserved(u8),
}

impl PortType {
    /// The type the PCI Express Capabilities register's Device/Port Type
    /// field (bits 7:4) encodes.
    #[must_use]
    pub const fn from_field(field: u8) -> Self {
        match field {
            0x0 => Self::Endpoint,
            0x1 => Self::LegacyEndpoint,
            0x4 => Self::RootPort,
            0x5 => Self::UpstreamPort,
            0x6 => Self::DownstreamPort,
            0x7 => Self::PcieToPci,
            0x8 => Self::PciToPcie,
            0x9 => Self::IntegratedEndpoint,
            0xA => Self::EventCollector,
            other => Self::Reserved(other),
        }
    }
}

/// A function's ACS Capability and ACS Control registers (PCI Express Base
/// 5.0 §7.7.8).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Acs {
    /// The controls the function implements.
    pub capable: u16,
    /// The controls turned on.
    pub enabled: u16,
}

impl Acs {
    /// Source Validation: a downstream port checks each request's requester
    /// id against the buses below it.
    pub const SOURCE_VALIDATION: u16 = 1 << 0;
    /// P2P Request Redirect: a request for a peer goes up, through the unit.
    pub const REQUEST_REDIRECT: u16 = 1 << 2;
    /// P2P Completion Redirect.
    pub const COMPLETION_REDIRECT: u16 = 1 << 3;
    /// Upstream Forwarding: a request that came up is not turned back down.
    pub const UPSTREAM_FORWARDING: u16 = 1 << 4;
    /// The controls without which peers below a port reach each other below
    /// the unit.
    pub const ISOLATING: u16 = Self::SOURCE_VALIDATION
        | Self::REQUEST_REDIRECT
        | Self::COMPLETION_REDIRECT
        | Self::UPSTREAM_FORWARDING;

    /// Whether every isolating control the function implements is on. One it
    /// does not implement is one its hardware has no path for.
    #[must_use]
    pub const fn isolates(self) -> bool {
        let required = Self::ISOLATING & self.capable;
        self.enabled & required == required
    }
}

/// A function's Address Translation Services capability (PCI Express Base 5.0
/// §10.5.1): with it on, the function asks the unit for translations and
/// presents the addresses it cached as already translated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ats {
    /// Translation requests are on.
    pub enabled: bool,
}

/// A function's Page Request Interface (PCI Express Base 5.0 §10.5.2): with it
/// on, the function asks for pages to be made present.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Pri {
    /// Page requests are on.
    pub enabled: bool,
}

/// A function's PASID capability (PCI Express Base 5.0 §7.8.8): with it on, the
/// function tags its requests with a process address space id.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Pasid {
    /// Tagged requests are on.
    pub enabled: bool,
    /// Bits of PASID the function can carry.
    pub width: u8,
}

/// A physical function's SR-IOV capability (PCI Express Base 5.0 §9.3.3): its
/// virtual functions answer no configuration scan of their own, yet master
/// DMA under requester ids of their own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SrIov {
    /// The virtual functions exist: VF Enable is on.
    pub enabled: bool,
    /// How many there are (`NumVFs`).
    pub count: u16,
    /// The first one's requester id, less the physical function's.
    pub offset: u16,
    /// The distance between consecutive ones' requester ids.
    pub stride: u16,
}

impl SrIov {
    /// The requester ids the virtual functions of the physical function
    /// `physical` master DMA under; none while they are disabled. An id past
    /// the 16-bit space is never one a function carries, so a capability that
    /// numbers one ends there.
    pub fn requesters(self, physical: u16) -> impl Iterator<Item = u16> {
        let first = u32::from(physical) + u32::from(self.offset);
        let count = if self.enabled { self.count } else { 0 };
        (0..u32::from(count))
            .map(move |n| first + n * u32::from(self.stride))
            .map_while(|id| u16::try_from(id).ok())
    }
}

/// What a walk does to the controls deciding what each function's DMA can
/// reach and be told apart from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Confinement {
    /// What a segment behind a translation unit wants: every isolating ACS
    /// control turned on, so its groups are as fine as the hardware allows,
    /// and every function's address translation services and virtual
    /// functions turned off, since no owner is handed either; each read back
    /// for what the hardware kept.
    Confine,
    /// Read them as firmware left them.
    Leave,
}

/// One function, as a walk read it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Its configuration address (`bus << 16 | device << 11 | function << 8`).
    pub address: u64,
    /// Its vendor id.
    pub vendor: u16,
    /// Its device id.
    pub device: u16,
    /// Its 24-bit class code: base class, subclass and programming interface.
    pub class: u32,
    /// What its header says it is.
    pub header: Header,
    /// Its slot holds more than one function.
    pub multifunction: bool,
    /// Its PCI Express device or port type; [`None`] for a conventional PCI
    /// function.
    pub express: Option<PortType>,
    /// A bridge below which nothing is trusted: hardware can arrive there
    /// after boot (its slot is hot-plug capable), or the platform describes
    /// it as external-facing.
    pub external_facing: bool,
    /// Its ACS registers, where it has the capability.
    pub acs: Option<Acs>,
    /// Its address translation services, where it has the capability.
    pub ats: Option<Ats>,
    /// Its page request interface, where it has the capability.
    pub pri: Option<Pri>,
    /// Its PASID capability, where it has one.
    pub pasid: Option<Pasid>,
    /// Its SR-IOV capability, where it is a physical function.
    pub sriov: Option<SrIov>,
}

impl Function {
    /// The requester id its own requests carry.
    #[must_use]
    pub fn requester_id(&self) -> u16 {
        requester_id(self.address)
    }

    /// The bus it sits on.
    #[must_use]
    pub const fn bus(&self) -> u8 {
        function_of(self.address).0
    }

    /// Whether it is a bridge, which forwards for the buses below it rather
    /// than mastering DMA of its own.
    #[must_use]
    pub const fn is_bridge(&self) -> bool {
        matches!(self.header, Header::Bridge { .. })
    }

    /// Whether it can master DMA of its own: neither a bridge, which forwards
    /// for the buses below it, nor a host bridge, the root complex's own
    /// function, whose Bus Master Enable chipsets commonly hardwire on.
    #[must_use]
    pub const fn masters_dma(&self) -> bool {
        !self.is_bridge() && self.class >> 8 != HOST_BRIDGE
    }

    /// Its record as [`tairix_abi::driver::bus::Bus::enumerate`] states it.
    #[must_use]
    pub fn bus_device(&self) -> BusDevice {
        BusDevice {
            vendor: u32::from(self.vendor),
            device: u32::from(self.device),
            class: u16::try_from(self.class >> 8).unwrap_or(u16::MAX),
            reserved0: 0,
            address: self.address,
        }
    }

    /// The buses it forwards to, when it is a bridge firmware assigned some.
    const fn forwards(&self) -> Option<(u8, u8)> {
        match self.header {
            Header::Bridge {
                secondary,
                subordinate,
                ..
            } if secondary != 0 => Some((secondary, subordinate)),
            _ => None,
        }
    }

    /// Whether ACS keeps its peers from reaching it, and it them, below the
    /// unit — Linux's `pci_acs_enabled`. A conventional function shares its
    /// bus with every other on it; bridges to and from conventional PCI and
    /// event collectors may never implement ACS; a port must enforce it; and
    /// any other function needs it only when it shares a slot.
    fn isolates(&self) -> bool {
        let enforced = self.acs.is_some_and(Acs::isolates);
        match self.express {
            Some(PortType::RootPort | PortType::DownstreamPort) => enforced,
            Some(
                PortType::Endpoint
                | PortType::LegacyEndpoint
                | PortType::UpstreamPort
                | PortType::IntegratedEndpoint,
            ) => !self.multifunction || enforced,
            Some(
                PortType::PcieToPci
                | PortType::PciToPcie
                | PortType::EventCollector
                | PortType::Reserved(_),
            )
            | None => false,
        }
    }
}

/// A function below an external-facing port ([`Topology::untrusted`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Untrusted {
    /// Every external-facing port above it validates requester ids and
    /// isolates: its DMA can be confined to its own streams.
    pub confinable: bool,
}

/// A requester id the fabric tags a function's DMA with besides its own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Alias {
    /// The requester id the unit sees.
    pub requester: u16,
    /// The index, in [`Topology::functions`], of the bridge that tags it.
    pub bridge: usize,
}

/// Why a walk's functions describe no PCI hierarchy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TopologyError {
    /// Two functions answer at one address.
    Duplicate,
    /// The bridges' bus numbers do not form a tree: a bridge forwarding to a
    /// bus not below its own, two bridges forwarding to one bus, or a bus
    /// whose bridges do not nest.
    Inconsistent,
    /// No memory to hold the walk.
    Exhausted,
}

impl From<TopologyError> for DriverError {
    fn from(err: TopologyError) -> Self {
        match err {
            TopologyError::Exhausted => Self::NoSpace,
            TopologyError::Duplicate | TopologyError::Inconsistent => Self::DeviceFault,
        }
    }
}

/// A walk of every function on one PCI segment, and the isolation it
/// implies.
#[derive(Debug)]
pub struct Topology {
    /// Every function, ascending by address.
    functions: Vec<Function>,
    /// For each bus, the index of the bridge forwarding to it; [`None`] for a
    /// root bus.
    parents: [Option<usize>; BUSES],
    /// Each function's isolation group, named by its members' least
    /// requester id.
    groups: Vec<u16>,
}

impl Topology {
    /// The hierarchy `functions` describe, with each one's isolation group.
    ///
    /// # Errors
    ///
    /// [`TopologyError`] for functions that describe no single hierarchy, or
    /// no memory to group them.
    pub fn new(mut functions: Vec<Function>) -> Result<Self, TopologyError> {
        functions.sort_unstable_by_key(|function| function.address);
        if functions
            .windows(2)
            .any(|pair| pair[0].address == pair[1].address)
        {
            return Err(TopologyError::Duplicate);
        }
        let parents = forwarding(&functions)?;
        let mut topology = Self {
            functions,
            parents,
            groups: Vec::new(),
        };
        topology.groups = topology.isolation_groups()?;
        Ok(topology)
    }

    /// Every function, ascending by address.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// The index of the function at configuration `address`.
    #[must_use]
    pub fn index_of(&self, address: u64) -> Option<usize> {
        self.functions
            .binary_search_by_key(&address, |function| function.address)
            .ok()
    }

    /// The slot on the root bus, and the pin, that INTx pin `pin` (1 for
    /// INTA) of the function at `index` reaches the host as: each bridge on
    /// the way swizzles it by the device it was raised on (PCI-to-PCI Bridge
    /// Architecture 1.2 §9.1). [`None`] for a pin outside INTA to INTD.
    #[must_use]
    pub fn intx_at_root(&self, index: usize, pin: u8) -> Option<(u8, u8)> {
        if !(1..=4).contains(&pin) {
            return None;
        }
        let (mut bus, mut device, _) = function_of(self.functions.get(index)?.address);
        let mut pin = pin;
        while let Some(bridge) = self.above(bus) {
            pin = (pin - 1 + device) % 4 + 1;
            (bus, device, _) = function_of(self.functions.get(bridge)?.address);
        }
        Some((device, pin))
    }

    /// The requester ids the bridges above the function at `index` tag its
    /// DMA with, nearest first. A bridge to conventional PCI tags it with its
    /// secondary bus and function `00.0`; a conventional bridge, or one from
    /// conventional PCI, with its own id; a PCI Express port passes it on.
    ///
    /// # Panics
    ///
    /// Never for an `index` below `self.functions().len()`.
    #[must_use]
    pub fn aliases(&self, index: usize) -> Aliases<'_> {
        Aliases {
            topology: self,
            bus: self.functions[index].bus(),
        }
    }

    /// The isolation group of the function at `index`, named by its members'
    /// least requester id: two functions share one exactly when the fabric
    /// can deliver either's DMA as the other's, or let either reach the other
    /// below the unit.
    ///
    /// # Panics
    ///
    /// Never for an `index` below `self.functions().len()`.
    #[must_use]
    pub fn group(&self, index: usize) -> u16 {
        self.groups[index]
    }

    /// Whether the function at `index` sits below an external-facing port,
    /// and if so whether every such port above it validates the requester id
    /// of what it forwards up and isolates it ([`Acs::isolates`]), so a
    /// device there can be told apart from every other: [`None`] for a
    /// trusted function.
    ///
    /// # Panics
    ///
    /// Never for an `index` below `self.functions().len()`.
    #[must_use]
    pub fn untrusted(&self, index: usize) -> Option<Untrusted> {
        let mut found = None;
        let mut bus = self.functions[index].bus();
        while let Some(bridge) = self.above(bus) {
            let port = &self.functions[bridge];
            if port.external_facing {
                let validates = port.acs.is_some_and(|acs| {
                    acs.capable & acs.enabled & Acs::SOURCE_VALIDATION != 0 && acs.isolates()
                });
                found = Some(Untrusted {
                    confinable: validates && found.is_none_or(|below: Untrusted| below.confinable),
                });
            }
            bus = port.bus();
        }
        found
    }

    /// The bridge forwarding to `bus`, if it is not a root bus.
    fn above(&self, bus: u8) -> Option<usize> {
        self.parents[usize::from(bus)]
    }

    /// Each function joins, as Linux's `pci_device_group` has it, the
    /// furthest device its DMA cannot be told apart from — the topmost bridge
    /// that tags it, then every bridge above whose path to the root lacks
    /// ACS — and that device, sharing a slot without ACS, joins its
    /// siblings that lack it too. Then, beyond Linux, it joins every function
    /// below an open bus above it ([`Self::open_buses`]): a port that does not
    /// redirect its requests up lets them reach its siblings' windows, which
    /// grouping by the path alone would miss. A physical function joins every
    /// function one of its virtual functions' requester ids names: the unit
    /// cannot tell their DMA apart.
    fn isolation_groups(&self) -> Result<Vec<u16>, TopologyError> {
        let count = self.functions.len();
        let mut sets: Vec<usize> = Vec::new();
        sets.try_reserve_exact(count)
            .map_err(|_| TopologyError::Exhausted)?;
        sets.extend(0..count);
        let mut slot_joined: Vec<bool> = Vec::new();
        slot_joined
            .try_reserve_exact(count)
            .map_err(|_| TopologyError::Exhausted)?;
        slot_joined.resize(count, false);
        let reach = self.reach()?;
        for index in 0..count {
            let on = reach[usize::from(self.functions[index].bus())];
            let start = on.alias.unwrap_or(index);
            let root = reach[usize::from(self.functions[start].bus())]
                .climb
                .unwrap_or(start);
            union(&mut sets, index, root);
            let device = &self.functions[root];
            if device.multifunction && !device.isolates() && !slot_joined[root] {
                slot_joined[root] = true;
                for sibling in self.slot(root) {
                    if !self.functions[sibling].isolates() {
                        union(&mut sets, root, sibling);
                    }
                }
            }
            if let Some(bridge) = on.open {
                union(&mut sets, index, bridge);
            }
            if let Some(sriov) = self.functions[index].sriov {
                for requester in sriov.requesters(self.functions[index].requester_id()) {
                    if let Some(named) = self.index_of(config_address(requester)) {
                        union(&mut sets, index, named);
                    }
                }
            }
        }
        let mut least: Vec<u16> = Vec::new();
        least
            .try_reserve_exact(count)
            .map_err(|_| TopologyError::Exhausted)?;
        least.resize(count, u16::MAX);
        for index in 0..count {
            let set = find(&mut sets, index);
            least[set] = least[set].min(self.functions[index].requester_id());
        }
        let mut groups = Vec::new();
        groups
            .try_reserve_exact(count)
            .map_err(|_| TopologyError::Exhausted)?;
        for index in 0..count {
            let set = find(&mut sets, index);
            groups.push(least[set]);
        }
        Ok(groups)
    }

    /// What each bus's path to the root decides for every function on it. A
    /// bridge sits on a lower bus than it forwards to, so one ascending pass
    /// sees each parent first.
    fn reach(&self) -> Result<Vec<Reach>, TopologyError> {
        let open = self.open_buses();
        let mut reach: Vec<Reach> = Vec::new();
        reach
            .try_reserve_exact(BUSES)
            .map_err(|_| TopologyError::Exhausted)?;
        for (parent, opens) in self.parents.iter().zip(open) {
            let entry = parent.map_or_else(Reach::default, |bridge| {
                let function = &self.functions[bridge];
                let up = reach[usize::from(function.bus())];
                let upward = self.above(function.bus()).is_none() || up.isolated;
                let isolated = upward && function.isolates();
                Reach {
                    isolated,
                    alias: up.alias.or_else(|| alias_of(function).map(|_| bridge)),
                    climb: (!isolated).then(|| up.climb.unwrap_or(bridge)),
                    open: up.open.or(opens.then_some(bridge)),
                }
            });
            reach.push(entry);
        }
        Ok(reach)
    }

    /// For each bus, whether a request reaching it from below can go back
    /// down to another function on it: a bus below the root complex holding
    /// more than one function, one of them a bridge that does not isolate or
    /// an endpoint beside a bridge, whose requests enter the bus through no
    /// port at all. Peer requests between root ports are left to the root
    /// complex, which must implement ACS on any root port that routes them.
    fn open_buses(&self) -> [bool; BUSES] {
        let mut functions = [0u16; BUSES];
        let mut bridges = [false; BUSES];
        let mut endpoints = [false; BUSES];
        let mut leaky = [false; BUSES];
        for function in &self.functions {
            let bus = usize::from(function.bus());
            if self.parents[bus].is_none() {
                continue;
            }
            functions[bus] = functions[bus].saturating_add(1);
            if function.is_bridge() {
                bridges[bus] = true;
                leaky[bus] |= !function.isolates();
            } else {
                endpoints[bus] = true;
            }
        }
        core::array::from_fn(|bus| {
            functions[bus] > 1 && (leaky[bus] || bridges[bus] && endpoints[bus])
        })
    }

    /// The indices of the other functions in the device of the one at
    /// `index`: its slot, or its whole bus where the port above forwards
    /// every function number to one device.
    fn slot(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let function = &self.functions[index];
        let ari = self.above(function.bus()).is_some_and(|bridge| {
            matches!(
                self.functions[bridge].header,
                Header::Bridge { ari: true, .. }
            )
        });
        let shift = if ari { 16 } else { 11 };
        let slot = function.address >> shift;
        let first = self
            .functions
            .partition_point(|function| function.address >> shift < slot);
        (first..self.functions.len())
            .take_while(move |&other| self.functions[other].address >> shift == slot)
            .filter(move |&other| other != index)
    }
}

/// What one bus's path to the root decides for every function on it.
#[derive(Copy, Clone, Default)]
struct Reach {
    /// The bridge forwarding to the bus and every bridge above it isolate:
    /// Linux's `pci_acs_path_enabled` from that bridge to the root.
    isolated: bool,
    /// The bridge nearest the root that tags the bus's DMA.
    alias: Option<usize>,
    /// The furthest bridge above the bus its DMA cannot be told apart from:
    /// up through every bus whose path does not isolate.
    climb: Option<usize>,
    /// The bridge forwarding to the open bus nearest the root above it:
    /// everything below that bridge is one group.
    open: Option<usize>,
}

/// The requester id `bridge` tags the DMA it forwards up with, if any.
fn alias_of(bridge: &Function) -> Option<u16> {
    match (bridge.express, bridge.header) {
        (Some(PortType::PcieToPci), Header::Bridge { secondary, .. }) => {
            Some(u16::from(secondary) << 8)
        }
        (Some(PortType::PciToPcie) | None, _) => Some(bridge.requester_id()),
        (Some(_), _) => None,
    }
}

/// The bridge each bus is forwarded from, once the bridges' bus numbers are
/// proven to form a tree: every bridge forwards below its own bus, no two to
/// one bus, and each bus in use lies in the ranges of exactly the bridges
/// above it. A bus no function answers on and no bridge forwards to directly
/// is one firmware held back, for a hot-plugged device.
fn forwarding(functions: &[Function]) -> Result<[Option<usize>; BUSES], TopologyError> {
    let mut parents = [None; BUSES];
    let mut claims = [0u16; BUSES];
    let mut populated = [false; BUSES];
    for (index, function) in functions.iter().enumerate() {
        populated[usize::from(function.bus())] = true;
        let Some((secondary, subordinate)) = function.forwards() else {
            continue;
        };
        if secondary <= function.bus() || subordinate < secondary {
            return Err(TopologyError::Inconsistent);
        }
        if parents[usize::from(secondary)].replace(index).is_some() {
            return Err(TopologyError::Inconsistent);
        }
        for bus in secondary..=subordinate {
            claims[usize::from(bus)] += 1;
        }
    }
    for (bus, &claimed) in claims.iter().enumerate() {
        if !populated[bus] && parents[bus].is_none() {
            continue;
        }
        let mut depth = 0u16;
        let mut at = bus;
        while let Some(bridge) = parents[at] {
            depth += 1;
            at = usize::from(functions[bridge].bus());
        }
        if claimed != depth {
            return Err(TopologyError::Inconsistent);
        }
    }
    Ok(parents)
}

/// The requester ids above one function, nearest first ([`Topology::aliases`]).
pub struct Aliases<'t> {
    topology: &'t Topology,
    bus: u8,
}

impl Iterator for Aliases<'_> {
    type Item = Alias;

    fn next(&mut self) -> Option<Alias> {
        loop {
            let bridge = self.topology.above(self.bus)?;
            let above = &self.topology.functions[bridge];
            self.bus = above.bus();
            if let Some(requester) = alias_of(above) {
                return Some(Alias { requester, bridge });
            }
        }
    }
}

fn find(sets: &mut [usize], mut index: usize) -> usize {
    while sets[index] != index {
        let parent = sets[index];
        sets[index] = sets[parent];
        index = parent;
    }
    index
}

fn union(sets: &mut [usize], a: usize, b: usize) {
    let (a, b) = (find(sets, a), find(sets, b));
    if a != b {
        sets[a.max(b)] = a.min(b);
    }
}

/// A configuration-space owner's view of its whole hierarchy.
pub trait PciTopology {
    /// Walk every function once, applying `confinement` to each, and describe
    /// the hierarchy they form. `external` names the bridges, by
    /// configuration address, the platform describes as external-facing.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when the walk cannot be held, and
    /// [`DriverError::DeviceFault`] for functions that form no hierarchy.
    fn topology(
        &self,
        confinement: Confinement,
        external: &dyn Fn(u64) -> bool,
    ) -> Result<Topology, DriverError>;

    /// Stop every function `stopped` names from mastering DMA, walking the
    /// bus flat: it needs no hierarchy and holds nothing, so it reaches every
    /// function of a walk [`topology`](Self::topology) refuses. Answers how
    /// many of them were found mastering and stopped, and how many read back
    /// mastering still.
    fn quiesce(&self, stopped: &dyn Fn(&Function) -> bool) -> Quiesced;
}

#[cfg(test)]
#[path = "topology_tests.rs"]
mod tests;
