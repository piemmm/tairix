//! The DMA translation topology a device tree describes (`plans/IOMMU.md`
//! IOM14), past what the shared walk records on each node: the isolation
//! group of every platform master behind a unit, the firmware windows its
//! reserved-memory regions keep, and how each generic PCI host's requester
//! ids reach units, which the shared PCI probe reads as a [`UnitTopology`].

use alloc::vec::Vec;

use tairix_abi::{
    HwNode, HwProperty, HwResource, HwResourceKind, IommuGroup, IommuReservedWindow, IommuStreams,
    ReservedAccess, HW_NODE_MAX_RESOURCES,
};
use tairix_arch_api::fdtwalk::{emitted, provider, Provider};
use tairix_arch_api::HwNodeSink;
use tairix_fdt::iommu::{each_iommu_address, iommu_cells};
use tairix_fdt::pci::each_pci_host;
use tairix_fdt::{Fdt, IdMap, IdMapEntry};
use tairix_log::{Event, Field, FieldValue, Level, Sink};
use tairix_pci::topology::Topology;

use crate::boot_hwtree::CollectingHwNodeSink;
use crate::pci_probe::{log_discovery, Unconfined, UnitTopology};

/// How one host's requester ids reach units.
struct HostMap {
    segment: u16,
    mask: u32,
    /// Each entry with its target resolved; [`None`] for a map naming a
    /// target no stream can be read from, or one that does not decode.
    entries: Option<Vec<Entry>>,
}

/// One map entry, its target resolved.
#[derive(Copy, Clone)]
struct Entry {
    map: IdMapEntry,
    /// The unit node translating the ids; [`None`] for a unit translating
    /// nothing it names (one no consumer may use, or one whose specifiers
    /// carry no stream id), whose ids are untranslated.
    unit: Option<u32>,
}

/// Where a map entry sends an id it maps.
#[derive(Copy, Clone)]
enum Mapped {
    /// Through no unit: the id is untranslated.
    Untranslated,
    /// To this stream on the unit with this node id.
    Stream(u32, u32),
}

impl Entry {
    /// Where the entry sends `id`; [`None`] for an id it does not map.
    fn map(&self, id: u32) -> Option<Mapped> {
        let stream = self.map.map(id)?;
        Some(
            self.unit
                .map_or(Mapped::Untranslated, |unit| Mapped::Stream(unit, stream)),
        )
    }
}

/// Why a host's map names no unit for its ids.
enum Unresolved {
    /// It does not decode, or names a target that is no unit.
    Undescribed,
    /// Its entries could not be held.
    Exhausted,
}

/// Who masters DMA as a range of streams.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Owner {
    /// The functions of a PCI segment.
    Host(u16),
    /// A platform master, by its node's position among the collected ones.
    Platform(usize),
}

/// Streams `[first, last]` on unit node `unit`, mastered by `owner`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Claim {
    unit: u32,
    first: u32,
    last: u32,
    owner: Owner,
}

/// The translation topology a device tree describes.
pub struct FdtUnits {
    hosts: Vec<HostMap>,
    /// Every stream range any owner masters DMA as, by unit then first
    /// stream.
    claims: Vec<Claim>,
    /// For each claim, the highest stream it or any claim before it on its
    /// unit reaches: a backward scan for a stream stops below it.
    reach: Vec<u32>,
    /// The topology could not be recorded, so no segment's translation can
    /// be described.
    unrecorded: bool,
}

impl FdtUnits {
    /// Read the topology `fdt` describes and complete the platform masters
    /// the walk collected into `sink`: each joins one isolation group with
    /// every master it shares a stream with, named by their least stream,
    /// and its unit keeps the firmware windows its reserved-memory regions
    /// ask for. A master is confined by no group — and the refusal audited —
    /// whose group spans units, shares a stream with a host's devices, or
    /// asks for a window its domain cannot keep. Where the topology cannot be
    /// held, every master stays ungrouped and every segment a host maps is
    /// left undescribed.
    pub fn read(fdt: &Fdt<'_>, sink: &mut CollectingHwNodeSink, log: &dyn Sink) -> Self {
        record_fault_interrupts(fdt, sink.nodes_mut());
        log_undescribed_masters(fdt, sink.nodes(), log);
        Self::record(fdt, sink, log).unwrap_or_else(|| {
            log_discovery(
                log,
                Level::Error,
                "dma translation topology unrecorded; nothing behind a unit confined",
            );
            Self {
                hosts: Vec::new(),
                claims: Vec::new(),
                reach: Vec::new(),
                unrecorded: true,
            }
        })
    }

    fn record(fdt: &Fdt<'_>, sink: &mut CollectingHwNodeSink, log: &dyn Sink) -> Option<Self> {
        let hosts = host_maps(fdt)?;
        let masters = platform_masters(sink.nodes())?;
        let mut claims = Vec::new();
        for host in &hosts {
            for entry in host.entries.iter().flatten() {
                let (Some(unit), Some(targets)) = (entry.unit, entry.map.targets()) else {
                    continue;
                };
                claims.try_reserve(1).ok()?;
                claims.push(Claim {
                    unit,
                    first: *targets.start(),
                    last: *targets.end(),
                    owner: Owner::Host(host.segment),
                });
            }
        }
        for (at, &index) in masters.iter().enumerate() {
            for range in streams_of(&sink.nodes()[index]) {
                claims.try_reserve(1).ok()?;
                claims.push(Claim {
                    unit: range.unit(),
                    first: range.first(),
                    last: range.first() + (range.count() - 1),
                    owner: Owner::Platform(at),
                });
            }
        }
        claims.sort_unstable();
        let mut reach = Vec::new();
        reach.try_reserve_exact(claims.len()).ok()?;
        for (at, claim) in claims.iter().enumerate() {
            let before = at
                .checked_sub(1)
                .filter(|&before| claims[before].unit == claim.unit)
                .map_or(claim.last, |before| reach[before]);
            reach.push(before.max(claim.last));
        }
        let mut groups = Groups::of(&claims, masters.len())?;
        groups.keep_windows(fdt, sink, &masters)?;
        groups.apply(sink, &masters, log);
        Some(Self {
            hosts,
            claims,
            reach,
            unrecorded: false,
        })
    }

    fn host(&self, segment: u16) -> Option<&HostMap> {
        self.hosts.iter().find(|host| host.segment == segment)
    }
}

impl UnitTopology for FdtUnits {
    fn covers(&self, segment: u16) -> bool {
        self.unrecorded || self.host(segment).is_some()
    }

    /// The units are the walk's own nodes, already collected.
    fn emit(
        &mut self,
        _walks: &[(u16, &Topology)],
        _sink: &mut dyn HwNodeSink,
        _log: &dyn Sink,
    ) -> Result<(), Unconfined> {
        Ok(())
    }

    fn strands(&self, segment: u16) -> bool {
        self.unrecorded
            || self
                .host(segment)
                .is_some_and(|host| host.entries.is_none())
    }

    fn stream(&self, segment: u16, _walk: &Topology, requester: u16) -> Option<(u32, u32)> {
        let host = self.host(segment)?;
        let id = u32::from(requester) & host.mask;
        match host
            .entries
            .as_ref()?
            .iter()
            .find_map(|entry| entry.map(id))?
        {
            Mapped::Stream(unit, stream) => Some((unit, stream)),
            Mapped::Untranslated => None,
        }
    }

    fn firmware_alias(&self, _segment: u16, _requester: u16) -> Option<u16> {
        None
    }

    fn contested(&self, segment: u16, unit: u32, stream: u32) -> bool {
        let first = self.claims.partition_point(|claim| claim.unit < unit);
        let end = first
            + self.claims[first..]
                .partition_point(|claim| claim.unit == unit && claim.first <= stream);
        self.claims[first..end]
            .iter()
            .zip(&self.reach[first..end])
            .rev()
            .take_while(|&(_, &reach)| reach >= stream)
            .any(|(claim, _)| claim.last >= stream && claim.owner != Owner::Host(segment))
    }
}

/// Every operational host's map. A segment two hosts name is no one's: which
/// one the PCI bring-up reached could not be told, so its map is undescribed
/// whatever either says, and the segment is stranded.
fn host_maps(fdt: &Fdt<'_>) -> Option<Vec<HostMap>> {
    let mut hosts: Vec<HostMap> = Vec::new();
    let mut seen: Vec<u16> = Vec::new();
    let mut held = true;
    each_pci_host(fdt, |host| {
        if !held {
            return;
        }
        if seen.contains(&host.segment) {
            if let Some(known) = hosts.iter_mut().find(|known| known.segment == host.segment) {
                known.entries = None;
            } else {
                held = hosts.try_reserve(1).is_ok();
                if held {
                    hosts.push(HostMap {
                        segment: host.segment,
                        mask: u32::MAX,
                        entries: None,
                    });
                }
            }
            return;
        }
        held = seen.try_reserve(1).is_ok();
        if !held {
            return;
        }
        seen.push(host.segment);
        let (mask, resolved) = match host.iommu_map() {
            Ok(None) => return,
            Ok(Some(map)) => (map.mask(), resolve(fdt, &map)),
            Err(_) => (u32::MAX, Err(Unresolved::Undescribed)),
        };
        let entries = match resolved {
            Ok(entries) => Some(entries),
            Err(Unresolved::Undescribed) => None,
            Err(Unresolved::Exhausted) => {
                held = false;
                return;
            }
        };
        held = hosts.try_reserve(1).is_ok();
        if held {
            hosts.push(HostMap {
                segment: host.segment,
                mask,
                entries,
            });
        }
    });
    held.then_some(hosts)
}

/// `map`'s entries with each target resolved to the unit the walk numbered.
fn resolve(fdt: &Fdt<'_>, map: &IdMap<'_>) -> Result<Vec<Entry>, Unresolved> {
    let mut entries = Vec::new();
    for entry in map.entries() {
        // A map's specifier is one cell: a target taking any other number is
        // no unit the map can name.
        let unit = match provider(fdt, entry.target) {
            Some(Provider::Emitted(id, node)) if iommu_cells(&node) == Some(1) => Some(id),
            Some(Provider::Unusable(node)) if iommu_cells(&node).is_some() => None,
            _ => return Err(Unresolved::Undescribed),
        };
        entries.try_reserve(1).map_err(|_| Unresolved::Exhausted)?;
        entries.push(Entry { map: entry, unit });
    }
    Ok(entries)
}

/// The positions in `nodes` of the platform masters the walk collected: the
/// nodes naming streams and no group yet.
fn platform_masters(nodes: &[HwNode]) -> Option<Vec<usize>> {
    let mut masters = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let resources = node.resources();
        let streams = resources
            .iter()
            .any(|r| r.kind() == Some(HwResourceKind::IommuStream));
        let grouped = resources
            .iter()
            .any(|r| r.kind() == Some(HwResourceKind::IommuGroup));
        if streams && !grouped {
            masters.try_reserve(1).ok()?;
            masters.push(index);
        }
    }
    Some(masters)
}

fn streams_of(node: &HwNode) -> impl Iterator<Item = IommuStreams> + '_ {
    node.resources()
        .iter()
        .filter_map(|r| r.iommu_streams().ok())
}

/// Why a platform master's group confines nothing.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Refusal {
    /// It shares a stream with a host's devices, or masters through two units.
    Unconfinable,
    /// Firmware asks its domain for a window it cannot keep.
    UnkeptWindow,
}

impl Refusal {
    fn reason(self) -> &'static str {
        match self {
            Self::Unconfinable => "unconfinable",
            Self::UnkeptWindow => "unkept_window",
        }
    }
}

/// The platform masters' isolation groups: a union of the masters sharing a
/// stream, by their position among the masters.
struct Groups {
    parent: Vec<usize>,
    /// By root: the unit the group masters through, its least stream, why it
    /// confines nothing, and the firmware windows its unit keeps for it.
    unit: Vec<Option<u32>>,
    least: Vec<u32>,
    refusal: Vec<Option<Refusal>>,
    windows: Vec<Vec<IommuReservedWindow>>,
}

impl Groups {
    /// Join the masters `claims` names wherever their streams meet on a unit,
    /// sweeping each unit's claims in order of first stream: a run of claims
    /// each reaching into the one before is one group. A run holding a
    /// host's claim leaves every master in it unconfinable.
    fn of(claims: &[Claim], masters: usize) -> Option<Self> {
        let mut groups = Self {
            parent: Vec::new(),
            unit: Vec::new(),
            least: Vec::new(),
            refusal: Vec::new(),
            windows: Vec::new(),
        };
        groups.parent.try_reserve_exact(masters).ok()?;
        groups.unit.try_reserve_exact(masters).ok()?;
        groups.least.try_reserve_exact(masters).ok()?;
        groups.refusal.try_reserve_exact(masters).ok()?;
        groups.windows.try_reserve_exact(masters).ok()?;
        groups.parent.extend(0..masters);
        groups.unit.resize(masters, None);
        groups.least.resize(masters, u32::MAX);
        groups.refusal.resize(masters, None);
        groups.windows.resize_with(masters, Vec::new);
        let mut start = 0;
        while start < claims.len() {
            let unit = claims[start].unit;
            let mut last = claims[start].last;
            let mut end = start + 1;
            while end < claims.len() && claims[end].unit == unit && claims[end].first <= last {
                last = last.max(claims[end].last);
                end += 1;
            }
            groups.join(&claims[start..end]);
            start = end;
        }
        for claim in claims {
            let Owner::Platform(at) = claim.owner else {
                continue;
            };
            let root = groups.root(at);
            groups.least[root] = groups.least[root].min(claim.first);
            match groups.unit[root] {
                None => groups.unit[root] = Some(claim.unit),
                Some(unit) if unit != claim.unit => groups.refuse(root, Refusal::Unconfinable),
                Some(_) => {}
            }
        }
        Some(groups)
    }

    fn root(&mut self, mut at: usize) -> usize {
        while self.parent[at] != at {
            self.parent[at] = self.parent[self.parent[at]];
            at = self.parent[at];
        }
        at
    }

    /// Join every master of `run`, claims that meet on one unit.
    fn join(&mut self, run: &[Claim]) {
        let mut joined = None;
        let mut hosted = false;
        for claim in run {
            match claim.owner {
                Owner::Host(_) => hosted = true,
                Owner::Platform(at) => {
                    let root = self.root(at);
                    let into = *joined.get_or_insert(root);
                    if root != into {
                        self.parent[root] = into;
                        let refusal = self.refusal[root];
                        if let Some(refusal) = refusal {
                            self.refuse(into, refusal);
                        }
                    }
                }
            }
        }
        if let (true, Some(root)) = (hosted, joined) {
            self.refuse(root, Refusal::Unconfinable);
        }
    }

    fn refuse(&mut self, root: usize, refusal: Refusal) {
        self.refusal[root].get_or_insert(refusal);
    }

    /// Gather the firmware windows each master's reserved-memory regions ask
    /// its domain to keep, on every stream it masters as: a group asking for
    /// one its domain cannot keep — mapping memory elsewhere than at its own
    /// address, keeping an I/O range out of use, not whole pages, or past
    /// what its unit's node holds — confines nothing.
    fn keep_windows(
        &mut self,
        fdt: &Fdt<'_>,
        sink: &CollectingHwNodeSink,
        masters: &[usize],
    ) -> Option<()> {
        if masters.is_empty() {
            return Some(());
        }
        let mut by_id = Vec::new();
        by_id.try_reserve_exact(masters.len()).ok()?;
        by_id.extend(
            masters
                .iter()
                .enumerate()
                .map(|(at, &index)| (sink.nodes()[index].id(), at)),
        );
        by_id.sort_unstable();
        for (id, node) in emitted(fdt) {
            let Ok(found) = by_id.binary_search_by_key(&id, |&(id, _)| id) else {
                continue;
            };
            let at = by_id[found].1;
            let root = self.root(at);
            let mut asked = Ok(());
            let mut windows = Vec::new();
            let read = each_iommu_address(fdt, &node, &mut |window| {
                if asked.is_err() {
                    return;
                }
                // A device tree names no access for a window: the device
                // reads and writes it.
                let kept = window
                    .is_identity()
                    .then(|| {
                        IommuReservedWindow::new(
                            0,
                            window.iova,
                            window.len,
                            ReservedAccess::ReadWrite,
                        )
                        .ok()
                    })
                    .flatten();
                match kept {
                    Some(_) if windows.try_reserve(1).is_ok() => windows.push(window),
                    Some(_) => asked = Err(None),
                    None => asked = Err(Some(Refusal::UnkeptWindow)),
                }
            });
            match (read, asked) {
                (Err(_), _) | (_, Err(Some(_))) => self.refuse(root, Refusal::UnkeptWindow),
                (_, Err(None)) => return None,
                (Ok(()), Ok(())) => {
                    for window in windows {
                        for range in streams_of(&sink.nodes()[masters[at]]) {
                            for stream in range.first()..=range.first() + (range.count() - 1) {
                                let kept = IommuReservedWindow::new(
                                    stream,
                                    window.iova,
                                    window.len,
                                    ReservedAccess::ReadWrite,
                                )
                                .ok()?;
                                self.windows[root].try_reserve(1).ok()?;
                                self.windows[root].push(kept);
                            }
                        }
                    }
                }
            }
        }
        Some(())
    }

    /// Record every group on the collected nodes: each confining group's
    /// windows on its unit, all of them or — past what the unit's node holds
    /// — none and the group refused, then its members' group; each refused
    /// group audited, member by member.
    fn apply(&mut self, sink: &mut CollectingHwNodeSink, masters: &[usize], log: &dyn Sink) {
        for root in 0..masters.len() {
            if self.root(root) != root || self.refusal[root].is_some() {
                continue;
            }
            let Some(unit) = self.unit[root] else {
                continue;
            };
            let Some(node) = sink.nodes_mut().iter_mut().find(|node| node.id() == unit) else {
                self.refuse(root, Refusal::UnkeptWindow);
                continue;
            };
            let windows = &self.windows[root];
            if HW_NODE_MAX_RESOURCES - node.resources().len() < windows.len() {
                self.refuse(root, Refusal::UnkeptWindow);
                continue;
            }
            for &window in windows {
                let _ = node.push_resource(HwResource::iommu_reserved_window(window));
            }
        }
        for (at, &index) in masters.iter().enumerate() {
            let root = self.root(at);
            let node = &mut sink.nodes_mut()[index];
            let refusal = match (self.refusal[root], self.unit[root]) {
                (None, Some(unit)) => {
                    let group = IommuGroup::new(unit, self.least[root]);
                    match node.push_resource(HwResource::iommu_group_member(group)) {
                        Ok(()) => continue,
                        Err(_) => Refusal::Unconfinable,
                    }
                }
                (refusal, _) => refusal.unwrap_or(Refusal::Unconfinable),
            };
            log_refused(
                log,
                node.id(),
                self.unit[root].unwrap_or(u32::MAX),
                refusal.reason(),
            );
        }
    }
}

/// Where among `nodes` the walk's node `id` is. The walk collects its nodes
/// first, numbered from the root at zero, so that is where to look first.
fn walked(nodes: &[HwNode], id: u32) -> Option<usize> {
    usize::try_from(id)
        .ok()
        .filter(|&at| nodes.get(at).is_some_and(|entry| entry.id() == id))
        .or_else(|| nodes.iter().position(|entry| entry.id() == id))
}

/// State on each unit the place of the wired line it raises its faults on:
/// as an `SMMUv3`'s binding names it in `interrupt-names`, and a RISC-V
/// IOMMU's family chooses it.
fn record_fault_interrupts(fdt: &Fdt<'_>, nodes: &mut [HwNode]) {
    for (id, node) in emitted(fdt) {
        if node.property("#iommu-cells").is_none() {
            continue;
        }
        let place = if node.is_compatible(tairix_kernel_iommu_smmuv3::COMPATIBLE) {
            node.property("interrupt-names").and_then(|names| {
                tairix_kernel_iommu_smmuv3::fault_interrupt(&names.iter_strings())
            })
        } else if node.is_compatible(tairix_kernel_iommu_riscv::COMPATIBLE) {
            Some(tairix_kernel_iommu_riscv::FAULT_INTERRUPT)
        } else {
            None
        };
        let Some(place) = place else {
            continue;
        };
        if let Some(at) = walked(nodes, id) {
            let fact = HwResource::property(HwProperty::FaultInterrupt, u64::from(place));
            let _ = nodes[at].push_resource(fact);
        }
    }
}

/// Audit every master whose `iommus` the walk could not describe: it was
/// given no DMA authority at all.
fn log_undescribed_masters(fdt: &Fdt<'_>, nodes: &[HwNode], log: &dyn Sink) {
    for (id, node) in emitted(fdt) {
        if node
            .property("iommus")
            .is_none_or(|iommus| iommus.value().is_empty())
        {
            continue;
        }
        let Some(entry) = walked(nodes, id).map(|at| &nodes[at]) else {
            continue;
        };
        let described = entry.resources().iter().any(|r| {
            matches!(
                r.kind(),
                Some(HwResourceKind::IommuStream | HwResourceKind::Dma)
            )
        });
        if !described {
            log_refused(log, id, u32::MAX, "undescribed");
        }
    }
}

fn log_refused(log: &dyn Sink, node: u32, unit: u32, reason: &'static str) {
    let event = tairix_kernel_core::AuditEvent::DmaTranslationBypass;
    tairix_log::log(
        log,
        &Event {
            level: Level::Warn,
            id: event.id(),
            message: event.message(),
            fields: &[
                Field {
                    key: "node",
                    value: FieldValue::UnsignedInt(u64::from(node)),
                },
                Field {
                    key: "unit",
                    value: FieldValue::UnsignedInt(u64::from(unit)),
                },
                Field {
                    key: "reason",
                    value: FieldValue::Str(reason),
                },
            ],
        },
    );
}

#[cfg(test)]
#[path = "iommu_fdt_tests.rs"]
mod tests;
