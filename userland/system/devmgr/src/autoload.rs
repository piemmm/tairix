//! Reactive match-and-load over the read-only `/System` driver store
//! (Design D D2b-2c — `plans/PI.md`).
//!
//! The device manager owns *policy*: it resolves each
//! discovered hardware-tree node against the kernel-decoded driver
//! catalogue with the shared [`tairix_devmatch`] policy,
//! and — for each winning node — asks the kernel to load the matched bundle
//! for that node ([`crate::load_driver`]). The kernel keeps the *mechanism*
//! (signature verification, bundle bytes, grant minting, spawn) in its
//! trusted base; this module supplies no bytes and no grants.
//!
//! Every matched node gets its **own** loaded driver instance: the kernel
//! mints a fresh process per load with exactly that node's resource grants,
//! so one loaded process can never see a sibling node's registers — two
//! identical devices (e.g. a virtio keyboard and a virtio mouse, both
//! device id 18) each need their own instance or the second is bound in
//! name only and never driven. An unmatched node is left unbound and
//! logged — never an error; a load refusal fails only
//! that node, closed, and the walk continues. Every
//! outcome is audited through [`tairix_log`] with the stable
//! [`crate::events`] identifiers, so this is the IPC-loader sibling of the
//! kernel-side `DeviceManager::autoload` walk over the same `resolve`
//! definition.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::hwlink::LinkRole;
use tairix_abi::{Errno, HwNode};
use tairix_devmatch::{resolve, DriverCandidate, MatchResolution};
use tairix_log::{Field, Level, Sink};
use tairix_util::fmt::{format_hex_u64, format_i32};
use tairix_util::retry::RestartPacer;

use crate::events::{audit_node, NodeEvent};
use crate::store::{load_driver, unload_driver, CatalogueDriver, DriverStoreCall};

/// The last match decision reported for a node, so an unchanged decision is
/// **not** re-logged when the reactive loop re-evaluates the tree.
///
/// The device manager re-runs [`match_and_load`] over the whole snapshot on
/// every hardware-tree generation advance. Without this
/// memory each re-evaluation would re-emit the same `NODE_UNBOUND` /
/// `NODE_BOUND` audit line for every node, flooding the (slow, serial)
/// diagnostic log with identical records and starving the boot — a
/// progress-spam / redundant-work defect. A node is logged only the
/// first time it reaches a decision and again only when that decision
/// *changes* (e.g. `Unbound` → `Bound` once the late-bound catalogue
/// arrives).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeReport {
    /// The node's winning driver is loaded ([`crate::events::NODE_BOUND`]).
    Bound,
    /// The node matched no driver bind table ([`crate::events::NODE_UNBOUND`]).
    Unbound,
    /// Two drivers tied at the highest priority ([`crate::events::NODE_TIE_REJECTED`]).
    TieRejected,
    /// The node's driver loaded and then ended, leaving it undriven
    /// ([`crate::events::NODE_DRIVER_ENDED`]). Never re-attempted.
    Ended,
    /// The node's driver ended for a failure a fresh instance may not meet,
    /// and is reloaded once its restart falls due
    /// ([`crate::events::NODE_DRIVER_RESTARTING`]).
    Restarting,
    /// The winning driver did not load: the gate refused it
    /// ([`crate::events::NODE_LOAD_FAILED`]), the node left the tree first
    /// ([`crate::events::NODE_LOAD_RACED_REMOVAL`]), or another live driver holds it
    /// ([`crate::events::NODE_ALREADY_DRIVEN`]). Never re-attempted.
    LoadFailed,
    /// The node waits for the supplier its links name
    /// ([`crate::events::NODE_HELD`]).
    Held(u32),
}

/// The decision last reported per node id, the dedup memory the reactive
/// loop carries across re-evaluations (see [`NodeReport`]).
pub type ReportedNodes = BTreeMap<u32, NodeReport>;

/// One bound node's driver: the opaque `bundle_id` it matched and the
/// `handle` the kernel returned for the loaded driver instance.
///
/// Every load spawns a fresh driver process holding exactly its node's
/// grants, so each binding names its own instance and `handle`; the
/// unload-on-removal diff tears an instance down when its node vanishes
/// ([`unload_vanished`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeDriver {
    /// The opaque `bundle_id` the node matched.
    pub bundle_id: u32,
    /// The loaded driver's handle the kernel returned.
    pub handle: u64,
    /// The generation of the snapshot the driver was loaded against, which
    /// predates the kernel marking the node driven.
    pub bound_at: u64,
    /// When the driver was loaded, on the monotonic clock.
    pub loaded_ns: u64,
}

impl NodeDriver {
    /// Whether `node`, as the snapshot of `generation` shows it, says this
    /// driver ended: undriven in a snapshot taken after it was loaded.
    #[must_use]
    pub const fn ended(&self, node: &HwNode, generation: u64) -> bool {
        !node.is_driven() && generation > self.bound_at
    }
}

/// The restarts a node's driver may take before the device manager gives up
/// on it.
pub const RESTART_LIMIT: u32 = 3;

/// The delay before a node's first restart, doubling with each after it, so a
/// device given a moment to recover gets it and a crash loop stays slow.
pub const RESTART_DELAY_NS: u64 = 500_000_000;

/// The longest a restart waits.
const RESTART_DELAY_CAP_NS: u64 = 30_000_000_000;

/// How long a driver serves before its node's restarts are forgiven, so a
/// fault once in a long while is recovered from every time.
pub const RESTART_QUIET_NS: u64 = 60_000_000_000;

/// A node whose driver ended for a failure a fresh instance may not meet,
/// its restarts paced by the one restart schedule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeRestart {
    pacer: RestartPacer,
    /// Restarts taken since the node's driver last served
    /// [`RESTART_QUIET_NS`].
    pub attempts: u32,
    /// When the next reload falls due, while one is owed.
    pub due_ns: Option<u64>,
}

impl NodeRestart {
    const fn new() -> Self {
        Self {
            pacer: RestartPacer::new(RESTART_DELAY_NS, RESTART_DELAY_CAP_NS, RESTART_QUIET_NS),
            attempts: 0,
            due_ns: None,
        }
    }

    /// Owe a restart for a driver loaded at `loaded_ns` that ended at
    /// `now_ns`, answering whether the budget allows one.
    fn failed(&mut self, loaded_ns: u64, now_ns: u64) -> bool {
        self.pacer.started(loaded_ns);
        let restart = self.pacer.failed(now_ns);
        if restart.taken >= RESTART_LIMIT {
            return false;
        }
        self.attempts = restart.taken + 1;
        self.due_ns = Some(restart.at);
        true
    }
}

/// The driver bound to each node id, the hot-removal memory the reactive loop
/// carries across re-evaluations: when a generation bump drops a node from
/// the tree, its binding here names the driver to unload ([`unload_vanished`]).
pub type NodeBindings = BTreeMap<u32, NodeDriver>;

/// The state the reactive match-and-load loop carries across re-evaluations:
/// the per-node decision memory ([`ReportedNodes`]), the per-node driver
/// bindings ([`NodeBindings`]), and the node ids the last pass ran over.
#[derive(Default)]
pub struct AutoloadState {
    /// Each node's last reported decision, so an unchanged one is not
    /// re-logged on re-evaluation (see [`NodeReport`]).
    pub reported: ReportedNodes,
    /// The driver bound to each node id, so the hot-removal diff can find the
    /// driver to unload when a bound node vanishes (see [`NodeBindings`]).
    pub bindings: NodeBindings,
    /// The ascending ids of the snapshot the last pass ran over, so a
    /// reaction that added or removed no node can be told apart.
    pub observed: Vec<u32>,
    /// The suppliers each bound node's links name, so a supplier that
    /// vanishes takes its consumers down first.
    pub suppliers: BTreeMap<u32, Vec<u32>>,
    /// The generation of the snapshot the passes run over.
    pub generation: u64,
    /// When the passes run, on the monotonic clock.
    pub now_ns: u64,
    /// Each node owed or recently given a restart.
    pub restarts: BTreeMap<u32, NodeRestart>,
}

impl AutoloadState {
    /// Whether any node of `nodes` bound to a driver says it ended.
    #[must_use]
    pub fn any_ended(&self, nodes: &[HwNode]) -> bool {
        nodes.iter().any(|node| {
            self.bindings
                .get(&node.id())
                .is_some_and(|binding| binding.ended(node, self.generation))
        })
    }

    /// Whether a restart owed has fallen due.
    #[must_use]
    pub fn restart_due(&self) -> bool {
        self.restarts
            .values()
            .any(|restart| restart.due_ns.is_some_and(|at| at <= self.now_ns))
    }

    /// When the soonest restart owed falls due.
    #[must_use]
    pub fn next_restart_ns(&self) -> Option<u64> {
        self.restarts
            .values()
            .filter_map(|restart| restart.due_ns)
            .min()
    }
}

/// Whether a node reported `report` will never serve: its load was refused,
/// or its driver ended.
fn never_serves(report: Option<&NodeReport>) -> bool {
    matches!(report, Some(NodeReport::LoadFailed | NodeReport::Ended))
}

/// Match every node of `nodes` against `catalogue` and load each winner's
/// bundle through `store` — **one instance per matched node** — recording
/// the bindings in `state.bindings` and auditing every outcome through
/// `sink`.
///
/// `candidates` is the borrowed [`DriverCandidate`] view of `catalogue`
/// (built once by the caller); `reply_buf` is the caller-owned buffer each
/// [`load_driver`] reply is received into. Idempotent across calls: a node
/// already bound in `state.bindings` keeps its instance and is reported
/// bound without a second load (hotplug re-match). A bundle matched by
/// several nodes loads once **per node**: the kernel spawns each load into
/// its own process holding exactly that node's resource grants, so a shared
/// instance would leave every node after the first granted to no one and
/// silently dead (two identical virtio-input devices — a keyboard and a
/// mouse — are the canonical case).
///
/// `state.reported` is the per-node dedup memory ([`ReportedNodes`]): each
/// node's decision is logged only the first time it is reached and again only
/// when it *changes*, so re-evaluating a settled tree (the common case after
/// each generation advance) emits no audit line at all — never re-flooding
/// the diagnostic log with identical records. A node already recorded
/// [`NodeReport::LoadFailed`] is **not** re-attempted: the static store's gate
/// would refuse it identically, a node that left the tree is gone, and a held
/// node stays held while its driver runs.
///
/// A node whose links name a supplier that is not yet serving is held until
/// it serves or is refused, and a supplier may sit later in the tree than its
/// consumer, so a pass that bound or refused something is followed by
/// another.
pub fn match_and_load<C: DriverStoreCall + ?Sized>(
    nodes: &[HwNode],
    catalogue: &[CatalogueDriver],
    candidates: &[DriverCandidate<'_>],
    store: &mut C,
    reply_buf: &mut [u8],
    state: &mut AutoloadState,
    sink: &dyn Sink,
) {
    loop {
        let before = decided(state);
        match_and_load_once(nodes, catalogue, candidates, store, reply_buf, state, sink);
        if decided(state) == before {
            return;
        }
    }
}

/// How many nodes the passes so far have bound, and how many will never
/// serve.
///
/// Kept apart because a driver that ended moves its node from the first to
/// the second, which releases its consumers as any other decision does.
fn decided(state: &AutoloadState) -> (usize, usize) {
    let never = state
        .reported
        .values()
        .filter(|&report| never_serves(Some(report)))
        .count();
    (state.bindings.len(), never)
}

/// One pass of [`match_and_load`] over every node.
fn match_and_load_once<C: DriverStoreCall + ?Sized>(
    nodes: &[HwNode],
    catalogue: &[CatalogueDriver],
    candidates: &[DriverCandidate<'_>],
    store: &mut C,
    reply_buf: &mut [u8],
    state: &mut AutoloadState,
    sink: &dyn Sink,
) {
    let pass = Pass::new(nodes, candidates, state);
    for (at, node) in nodes.iter().enumerate() {
        if node.is_root() {
            continue;
        }
        let id = node.id();
        match pass.resolved[at] {
            MatchResolution::Unmatched => {
                // An unmatched node is the routine, high-volume case: on a
                // real device tree most nodes (clocks, pinctrl, thermal, …)
                // have no driver, so emitting one record per unbound node at
                // `Info` floods the slow diagnostic UART and starves boot (a
                // progress-spam / defect — it once delayed the Pi's
                // `ARXFS passphrase:` prompt by tens of seconds). It is `Debug`
                // instead, so the default `Info` filter drops it in O(1)
                // before any `log_emit` syscall. A *binding* (`NODE_BOUND`), a
                // packaging tie, or a load refusal stays visible — those are
                // the actionable outcomes. Mirrors `events::NODE_OBSERVED`.
                if changed(&mut state.reported, id, NodeReport::Unbound) {
                    audit_node(sink, NodeEvent::Unbound, Level::Debug, id, &[]);
                }
            }
            MatchResolution::Tie { priority } => {
                if changed(&mut state.reported, id, NodeReport::TieRejected) {
                    let mut pbuf = [0u8; 12];
                    let priority_str = format_i32(i32::from(priority), &mut pbuf);
                    audit_node(
                        sink,
                        NodeEvent::TieRejected,
                        Level::Warn,
                        id,
                        &[Field {
                            key: "priority",
                            value: tairix_log::FieldValue::Str(priority_str),
                        }],
                    );
                }
            }
            MatchResolution::Winner { candidate, .. } => {
                let bundle_id = catalogue[candidate].bundle_id;
                let (handle, bound_at, loaded_ns) = if let Some(&existing) = state.bindings.get(&id)
                {
                    if existing.ended(node, state.generation) {
                        state.bindings.remove(&id);
                        state.suppliers.remove(&id);
                        driver_ended(node, &existing, state, sink);
                        continue;
                    }
                    (existing.handle, existing.bound_at, existing.loaded_ns)
                } else {
                    let Some(handle) =
                        load_unbound(&pass, at, bundle_id, store, reply_buf, state, sink)
                    else {
                        continue;
                    };
                    (handle, state.generation, state.now_ns)
                };
                // Record (or refresh) which driver this node is bound to, so
                // the hot-removal diff can tear it down if the node later
                // vanishes (see `unload_vanished`). A node re-matched on a
                // re-evaluation re-records the same binding; the entry is
                // dropped only when the node disappears or its driver unloads.
                state.bindings.insert(
                    id,
                    NodeDriver {
                        bundle_id,
                        handle,
                        bound_at,
                        loaded_ns,
                    },
                );
                let named: Vec<u32> = suppliers_of(node).collect();
                if !named.is_empty() {
                    state.suppliers.insert(id, named);
                }
                if changed(&mut state.reported, id, NodeReport::Bound) {
                    let mut hbuf = [0u8; 16];
                    let handle_str = format_hex_u64(handle, &mut hbuf);
                    audit_node(
                        sink,
                        NodeEvent::Bound,
                        Level::Info,
                        id,
                        &[Field {
                            key: "handle",
                            value: tairix_log::FieldValue::Str(handle_str),
                        }],
                    );
                }
            }
        }
    }
}

/// Load the winning driver `bundle_id` for the unbound node at `at`, unless
/// its node will never serve, its restart is not yet due, or a supplier holds
/// it, answering the loaded driver's handle.
fn load_unbound<C: DriverStoreCall + ?Sized>(
    pass: &Pass<'_>,
    at: usize,
    bundle_id: u32,
    store: &mut C,
    reply_buf: &mut [u8],
    state: &mut AutoloadState,
    sink: &dyn Sink,
) -> Option<u64> {
    let id = pass.nodes[at].id();
    // A changed device is republished under a new id, so a failed load or an
    // ended driver is never worth repeating for this one.
    if never_serves(state.reported.get(&id)) {
        return None;
    }
    if let Some(restart) = state.restarts.get_mut(&id) {
        if restart.due_ns.is_some_and(|due| state.now_ns < due) {
            return None;
        }
        // Taken now whatever this pass decides, so a held restart waits on
        // its supplier rather than on a deadline already passed.
        restart.due_ns = None;
    }
    if let Some(supplier) = holding_supplier(pass, at, state) {
        if changed(&mut state.reported, id, NodeReport::Held(supplier)) {
            let mut sbuf = [0u8; 16];
            let supplier_str = format_hex_u64(u64::from(supplier), &mut sbuf);
            audit_node(
                sink,
                NodeEvent::Held,
                Level::Debug,
                id,
                &[Field {
                    key: "supplier",
                    value: tairix_log::FieldValue::Str(supplier_str),
                }],
            );
        }
        return None;
    }
    match load_driver(store, bundle_id, id, reply_buf) {
        Ok(handle) => Some(handle),
        Err(errno) => {
            if changed(&mut state.reported, id, NodeReport::LoadFailed) {
                let (event, level) = load_refusal(errno);
                let mut ebuf = [0u8; 12];
                let errno_str = format_i32(errno.as_i32(), &mut ebuf);
                audit_node(
                    sink,
                    event,
                    level,
                    id,
                    &[Field {
                        key: "errno",
                        value: tairix_log::FieldValue::Str(errno_str),
                    }],
                );
            }
            None
        }
    }
}

/// Record that the driver `ended`, bound to `node`, ended as the kernel
/// recorded, and owe the node a restart where one may cure the ending.
fn driver_ended(node: &HwNode, ended: &NodeDriver, state: &mut AutoloadState, sink: &dyn Sink) {
    let id = node.id();
    let ending = node.driver_ending();
    let named = Field {
        key: "ending",
        value: tairix_log::FieldValue::Str(ending.name()),
    };
    let restart = if ending.merits_restart() {
        let entry = state.restarts.entry(id).or_insert_with(NodeRestart::new);
        entry
            .failed(ended.loaded_ns, state.now_ns)
            .then_some(*entry)
    } else {
        None
    };
    if let Some(restart) = restart {
        let delay_ms = restart
            .due_ns
            .map_or(0, |at| at.saturating_sub(state.now_ns) / 1_000_000);
        if changed(&mut state.reported, id, NodeReport::Restarting) {
            audit_node(
                sink,
                NodeEvent::DriverRestarting,
                Level::Warn,
                id,
                &[
                    named,
                    Field {
                        key: "attempt",
                        value: tairix_log::FieldValue::UnsignedInt(u64::from(restart.attempts)),
                    },
                    Field {
                        key: "delay_ms",
                        value: tairix_log::FieldValue::UnsignedInt(delay_ms),
                    },
                ],
            );
        }
    } else {
        let spent = state
            .restarts
            .remove(&id)
            .filter(|_| ending.merits_restart())
            .map(|restart| restart.attempts);
        if changed(&mut state.reported, id, NodeReport::Ended) {
            let restarts = Field {
                key: "restarts",
                value: tairix_log::FieldValue::UnsignedInt(u64::from(spent.unwrap_or(0))),
            };
            let fields = [named, restarts];
            let len = if spent.is_some() { 2 } else { 1 };
            audit_node(
                sink,
                NodeEvent::DriverEnded,
                Level::Info,
                id,
                &fields[..len],
            );
        }
    }
}

/// Unload every driver whose bound hardware-tree node has **vanished** from
/// the live tree (hot-removal), the symmetric partner of [`match_and_load`].
///
/// `present` answers whether a node id is in the snapshot just observed.
/// Every previously-bound node ([`AutoloadState::bindings`]) absent from it
/// is gone: its binding is dropped and the kernel is asked to tear its
/// driver instance down through [`unload_driver`] (each node owns its own
/// instance, so no other binding can share the handle). Every decision
/// recorded for an absent node is dropped too, bound or not: a node id is
/// never reissued, so a device re-attached later arrives as a fresh node and
/// the memory would otherwise grow with every re-plug.
///
/// A vanished node is gone for good, whatever its fault-domain owner is
/// doing: no bus driver drops its children across its own reset (the xHCI
/// controller keeps every child whose device comes back, `plans/FIX-IO.md`
/// IO4), and a node that does leave the tree never returns under its id,
/// so its driver has nothing left to serve.
///
/// A still-present node whose links name a vanished supplier is unloaded
/// first, since its driver was using it; it is then held, its supplier gone.
///
/// Idempotent and fail-soft: an unload that the kernel reports already gone
/// ([`Errno::NotFound`]) still drops the local binding; a transport failure
/// is logged and the binding dropped so the stale driver is never
/// re-derived. Every unload is audited ([`crate::events::NODE_UNLOADED`]).
pub fn unload_vanished<C: DriverStoreCall + ?Sized>(
    present: &dyn Fn(u32) -> bool,
    store: &mut C,
    reply_buf: &mut [u8],
    state: &mut AutoloadState,
    sink: &dyn Sink,
) {
    state.reported.retain(|&node_id, _| present(node_id));
    state.restarts.retain(|&node_id, _| present(node_id));
    let gone: Vec<u32> = state
        .bindings
        .keys()
        .copied()
        .filter(|&node_id| !present(node_id))
        .collect();
    // A consumer goes before the supplier it was using; with its supplier
    // gone for good it is held from then on.
    let consumers: Vec<(u32, u32)> = state
        .suppliers
        .iter()
        .filter(|&(&node_id, _)| present(node_id))
        .filter_map(|(&node_id, named)| {
            named
                .iter()
                .find(|supplier| gone.contains(supplier))
                .map(|&supplier| (node_id, supplier))
        })
        .collect();
    for (node_id, supplier) in consumers {
        if let Some(driver) = state.bindings.remove(&node_id) {
            unload_one(store, reply_buf, node_id, driver, Some(supplier), sink);
        }
        state.reported.remove(&node_id);
    }
    for node_id in gone {
        if let Some(driver) = state.bindings.remove(&node_id) {
            unload_one(store, reply_buf, node_id, driver, None, sink);
        }
    }
    state
        .suppliers
        .retain(|node_id, _| state.bindings.contains_key(node_id));
}

/// Unload `driver`, bound to `node_id`, auditing the outcome; `supplier` names
/// the vanished supplier a consumer is unloaded for.
///
/// An unload the kernel reports already gone, or one whose transport failed,
/// is recorded at `Warn`, and the caller drops the binding either way so the
/// stale driver is never re-derived.
fn unload_one<C: DriverStoreCall + ?Sized>(
    store: &mut C,
    reply_buf: &mut [u8],
    node_id: u32,
    driver: NodeDriver,
    supplier: Option<u32>,
    sink: &dyn Sink,
) {
    let outcome = unload_driver(store, driver.handle, reply_buf);
    let mut hbuf = [0u8; 16];
    let mut sbuf = [0u8; 16];
    let mut ebuf = [0u8; 12];
    let handle = Field {
        key: "handle",
        value: tairix_log::FieldValue::Str(format_hex_u64(driver.handle, &mut hbuf)),
    };
    let mut fields = [handle; 3];
    let mut len = 1;
    if let Some(supplier) = supplier {
        fields[len] = Field {
            key: "supplier",
            value: tairix_log::FieldValue::Str(format_hex_u64(u64::from(supplier), &mut sbuf)),
        };
        len += 1;
    }
    let level = match outcome {
        Ok(()) => Level::Info,
        Err(errno) => {
            fields[len] = Field {
                key: "errno",
                value: tairix_log::FieldValue::Str(format_i32(errno.as_i32(), &mut ebuf)),
            };
            len += 1;
            Level::Warn
        }
    };
    audit_node(sink, NodeEvent::Unloaded, level, node_id, &fields[..len]);
}

/// The node ids of the suppliers `node`'s links name.
fn suppliers_of(node: &HwNode) -> impl Iterator<Item = u32> + '_ {
    links_of(node).map(|(supplier, _)| supplier)
}

/// Each supplier `node`'s links name, with the role it serves them in.
fn links_of(node: &HwNode) -> impl Iterator<Item = (u32, LinkRole)> + '_ {
    node.resources().iter().filter_map(|resource| {
        let request = resource.link_request().ok()?;
        let role = request.role();
        Some((role.endpoints().node_of(request.endpoint())?, role))
    })
}

/// What one pass derives from the snapshot before it decides any node.
struct Pass<'n> {
    nodes: &'n [HwNode],
    /// Each node's match, in `nodes` order.
    resolved: Vec<MatchResolution>,
    /// Each node's position in `nodes`, by id.
    position: BTreeMap<u32, usize>,
    /// Each node's wait component ([`wait_components`]).
    component: Vec<usize>,
}

impl<'n> Pass<'n> {
    fn new(nodes: &'n [HwNode], candidates: &[DriverCandidate<'_>], state: &AutoloadState) -> Self {
        let resolved: Vec<MatchResolution> = nodes
            .iter()
            .map(|node| resolve(node.match_keys(), candidates))
            .collect();
        let position = nodes
            .iter()
            .enumerate()
            .map(|(at, node)| (node.id(), at))
            .collect();
        let mut pass = Self {
            nodes,
            resolved,
            position,
            component: Vec::new(),
        };
        pass.component = wait_components(&pass.waits(state));
        pass
    }

    /// The position of `supplier` when it is in the tree and holds back the
    /// consumers it serves in `role`: a driver matches it, it is not yet
    /// serving, and its load was not refused.
    fn holding_back(&self, supplier: u32, role: LinkRole, state: &AutoloadState) -> Option<usize> {
        if never_serves(state.reported.get(&supplier)) {
            return None;
        }
        let &found = self.position.get(&supplier)?;
        let undecided = !self.nodes[found].serves(role)
            && matches!(self.resolved[found], MatchResolution::Winner { .. });
        undecided.then_some(found)
    }

    /// The graph in which every node points at each present supplier holding
    /// it back, as each node's targets laid end to end and where each node's
    /// begin.
    ///
    /// A loaded node keeps its edges, so a cycle stays one after its first
    /// member loads rather than leaving the rest to wait on that member.
    fn waits(&self, state: &AutoloadState) -> Waits {
        let mut waits = Waits {
            starts: Vec::with_capacity(self.nodes.len() + 1),
            targets: Vec::new(),
        };
        for node in self.nodes {
            waits.starts.push(waits.targets.len());
            for (supplier, role) in links_of(node) {
                if let Some(found) = self.holding_back(supplier, role, state) {
                    waits.targets.push(found);
                }
            }
        }
        waits.starts.push(waits.targets.len());
        waits
    }
}

/// A directed graph over node positions, each node's out-edges contiguous.
struct Waits {
    starts: Vec<usize>,
    targets: Vec<usize>,
}

impl Waits {
    fn edges(&self, from: usize) -> &[usize] {
        &self.targets[self.starts[from]..self.starts[from + 1]]
    }
}

/// Each node's strongly connected component in `waits`, numbered so that two
/// nodes share one exactly when each waits, through others, on the other.
///
/// Tarjan's algorithm, iterative so a long chain of links cannot exhaust the
/// stack.
fn wait_components(waits: &Waits) -> Vec<usize> {
    const UNVISITED: usize = usize::MAX;
    let count = waits.starts.len().saturating_sub(1);
    let mut order = vec![UNVISITED; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut component = vec![0; count];
    let mut stack = Vec::new();
    // Each frame is a node being explored and the next of its edges to take.
    let mut frames: Vec<(usize, usize)> = Vec::new();
    let (mut visited, mut components) = (0, 0);
    for root in 0..count {
        if order[root] != UNVISITED {
            continue;
        }
        order[root] = visited;
        low[root] = visited;
        visited += 1;
        stack.push(root);
        on_stack[root] = true;
        frames.push((root, 0));
        while let Some(frame) = frames.last_mut() {
            let node = frame.0;
            if let Some(&next) = waits.edges(node).get(frame.1) {
                frame.1 += 1;
                if order[next] == UNVISITED {
                    order[next] = visited;
                    low[next] = visited;
                    visited += 1;
                    stack.push(next);
                    on_stack[next] = true;
                    frames.push((next, 0));
                } else if on_stack[next] {
                    low[node] = low[node].min(order[next]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component[member] = components;
                    if member == node {
                        break;
                    }
                }
                components += 1;
            }
        }
    }
    component
}

/// The first supplier the links of the node at `at` name that holds it back:
/// one not in the tree, which may yet appear and never under an id a removed
/// node had, or one in it that a driver matches but that is not serving and
/// was not refused.
///
/// A supplier no installed driver serves — a clock the firmware owns — holds
/// nothing, or its consumers would wait forever. Nor does a link that closes
/// a cycle, a clock controller fed by a PHY whose own clocks it makes: every
/// member would wait on another, so as Linux's device links do, the links
/// within a cycle order nothing and its members load together.
fn holding_supplier(pass: &Pass<'_>, at: usize, state: &AutoloadState) -> Option<u32> {
    links_of(&pass.nodes[at])
        .find(|&(supplier, role)| {
            if never_serves(state.reported.get(&supplier)) {
                return false;
            }
            if !pass.position.contains_key(&supplier) {
                return true;
            }
            pass.holding_back(supplier, role, state)
                .is_some_and(|found| pass.component[found] != pass.component[at])
        })
        .map(|(supplier, _)| supplier)
}

/// The record a failed load is reported under: a node that left the tree
/// before its driver was admitted, or that another live driver already
/// holds, is the tree moving under the load rather than the gate refusing
/// the image.
fn load_refusal(errno: Errno) -> (NodeEvent, Level) {
    match errno {
        Errno::DeviceOffline => (NodeEvent::LoadRacedRemoval, Level::Info),
        Errno::Busy => (NodeEvent::AlreadyDriven, Level::Info),
        _ => (NodeEvent::LoadFailed, Level::Warn),
    }
}

/// Record `kind` as node `id`'s latest decision, returning `true` when it
/// differs from the previously reported one (or none was) — the signal that
/// the decision is worth logging (log a change, not every
/// re-evaluation).
fn changed(reported: &mut ReportedNodes, id: u32, kind: NodeReport) -> bool {
    reported.insert(id, kind) != Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events;
    use crate::testsink::RecordingSink;

    use alloc::vec;
    use core::cell::RefCell;

    use tairix_abi::driver::DriverEnding;
    use tairix_abi::driver_store::{encode_error_reply, encode_unload_reply, StoreRequest};
    use tairix_abi::hwtree::{HwDeviceClass, HwMatchKey};
    use tairix_abi::DriverBindKey;

    /// A driver-store seam that records every `Unload { handle }` and frames
    /// a success reply — so the diff's teardown decisions are observable.
    struct UnloadRecorder {
        unloads: RefCell<Vec<u64>>,
    }

    impl UnloadRecorder {
        fn new() -> Self {
            Self {
                unloads: RefCell::new(Vec::new()),
            }
        }
    }

    impl DriverStoreCall for UnloadRecorder {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            // The diff only ever issues `Unload`; any other opcode is a test
            // bug surfaced fail-closed rather than silently ignored.
            match StoreRequest::decode(request)? {
                StoreRequest::Unload { handle } => {
                    self.unloads.borrow_mut().push(handle);
                    encode_unload_reply(reply)
                }
                _ => Err(Errno::NotImplemented),
            }
        }
    }

    /// A sink that drops every record — these tests assert on the store's
    /// recorded unloads and the state, not the audit log.
    struct NullSink;
    impl Sink for NullSink {
        fn write_event(&self, _event: &tairix_log::Event<'_>) {}
    }

    fn bound(state: &mut AutoloadState, node: u32, bundle_id: u32, handle: u64) {
        state.bindings.insert(
            node,
            NodeDriver {
                bundle_id,
                handle,
                bound_at: 0,
                loaded_ns: 0,
            },
        );
        state.reported.insert(node, NodeReport::Bound);
    }

    #[test]
    fn a_vanished_bound_node_unloads_its_driver_instance() {
        let mut state = AutoloadState::default();
        bound(&mut state, 2, 7, 0x1007);
        let mut store = UnloadRecorder::new();
        let mut reply = [0u8; 64];

        // Node 2 is no longer present: tear its driver down.
        unload_vanished(&|_id| false, &mut store, &mut reply, &mut state, &NullSink);

        assert_eq!(store.unloads.borrow().as_slice(), &[0x1007]);
        assert!(state.bindings.is_empty(), "the binding is dropped");
        assert!(
            !state.reported.contains_key(&2),
            "the decision memory goes with the node"
        );
    }

    #[test]
    fn a_vanished_node_leaves_no_decision_behind_bound_or_not() {
        // Node ids are never reissued, so an entry kept for a node that left
        // grows the memory by one with every re-plug, for the life of the
        // service.
        let mut state = AutoloadState::default();
        state.reported.insert(3, NodeReport::Unbound);
        state.reported.insert(4, NodeReport::TieRejected);
        state.reported.insert(5, NodeReport::LoadFailed);
        state.reported.insert(6, NodeReport::Unbound);
        let mut store = UnloadRecorder::new();
        let mut reply = [0u8; 64];

        unload_vanished(&|id| id == 6, &mut store, &mut reply, &mut state, &NullSink);

        assert_eq!(
            state.reported.keys().copied().collect::<Vec<_>>(),
            [6],
            "only the node still present keeps its decision"
        );
        assert!(
            store.unloads.borrow().is_empty(),
            "nothing was bound, so nothing is unloaded"
        );
    }

    #[test]
    fn a_still_present_bound_node_is_not_unloaded() {
        let mut state = AutoloadState::default();
        bound(&mut state, 2, 7, 0x1007);
        let mut store = UnloadRecorder::new();
        let mut reply = [0u8; 64];

        // Node 2 is still present: nothing is torn down.
        unload_vanished(&|id| id == 2, &mut store, &mut reply, &mut state, &NullSink);

        assert!(store.unloads.borrow().is_empty());
        assert_eq!(state.bindings.len(), 1);
    }

    #[test]
    fn a_vanished_node_unloads_only_its_own_instance() {
        // One bundle, two nodes, two instances (each load spawns its own
        // process holding its node's grants). Losing one node tears down
        // only that node's instance; the sibling's keeps running.
        let mut state = AutoloadState::default();
        bound(&mut state, 2, 7, 0x1007);
        bound(&mut state, 3, 7, 0x2007);
        let mut store = UnloadRecorder::new();
        let mut reply = [0u8; 64];

        // Node 2 vanishes, node 3 stays: only node 2's instance is torn down.
        unload_vanished(&|id| id == 3, &mut store, &mut reply, &mut state, &NullSink);
        assert_eq!(
            store.unloads.borrow().as_slice(),
            &[0x1007],
            "only the vanished node's instance is torn down"
        );
        assert_eq!(state.bindings.len(), 1);

        // Now node 3 vanishes too — its own instance is unloaded.
        unload_vanished(&|_id| false, &mut store, &mut reply, &mut state, &NullSink);
        assert_eq!(store.unloads.borrow().as_slice(), &[0x1007, 0x2007]);
        assert!(state.bindings.is_empty());
    }

    #[test]
    fn an_already_gone_handle_still_drops_the_binding_fail_soft() {
        // The kernel reports the driver already gone; the diff still drops the
        // local binding so the stale driver is never re-derived (fail-soft).
        struct AlreadyGone;
        impl DriverStoreCall for AlreadyGone {
            fn call(&mut self, _request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
                tairix_abi::driver_store::encode_error_reply(reply, Errno::NotFound)
            }
        }

        let mut state = AutoloadState::default();
        bound(&mut state, 2, 7, 0x1007);
        let mut store = AlreadyGone;
        let mut reply = [0u8; 64];

        unload_vanished(&|_id| false, &mut store, &mut reply, &mut state, &NullSink);

        assert!(state.bindings.is_empty());
    }

    /// A store that refuses every load in band with the errno scripted for
    /// its node.
    struct RefusingStore(Vec<(u32, Errno)>);

    impl DriverStoreCall for RefusingStore {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            let StoreRequest::Load { node_id, .. } = StoreRequest::decode(request)? else {
                return Err(Errno::NotImplemented);
            };
            let errno = self
                .0
                .iter()
                .find(|(node, _)| *node == node_id)
                .map_or(Errno::NotFound, |(_, errno)| *errno);
            encode_error_reply(reply, errno)
        }
    }

    #[test]
    fn a_load_the_tree_overtook_is_not_reported_as_a_gate_refusal() {
        let key = HwMatchKey::virtio(0x1234);
        let input = |id| {
            let mut node = HwNode::new(id, 1, HwDeviceClass::Input);
            node.push_match_key(key).expect("key fits");
            node
        };
        let nodes = [input(2), input(3), input(4)];
        let catalogue = [CatalogueDriver {
            bundle_id: 7,
            bind_keys: vec![DriverBindKey::new(5, key)],
        }];
        let candidates: Vec<_> = catalogue.iter().map(CatalogueDriver::candidate).collect();
        let mut store = RefusingStore(vec![
            (2, Errno::DeviceOffline),
            (3, Errno::Busy),
            (4, Errno::PermissionDenied),
        ]);
        let sink = RecordingSink::new();
        let mut state = AutoloadState::default();
        let mut reply = [0u8; 64];

        match_and_load(
            &nodes,
            &catalogue,
            &candidates,
            &mut store,
            &mut reply,
            &mut state,
            &sink,
        );

        assert_eq!(
            sink.ids(),
            [
                events::NODE_LOAD_RACED_REMOVAL.0,
                events::NODE_ALREADY_DRIVEN.0,
                events::NODE_LOAD_FAILED.0,
            ]
        );
        assert_eq!(
            sink.level_of(events::NODE_LOAD_RACED_REMOVAL.0),
            Some(Level::Info)
        );
        assert_eq!(
            sink.level_of(events::NODE_ALREADY_DRIVEN.0),
            Some(Level::Info)
        );
        assert_eq!(sink.level_of(events::NODE_LOAD_FAILED.0), Some(Level::Warn));
        assert_eq!(
            sink.field_of(events::NODE_ALREADY_DRIVEN.0, "errno")
                .as_deref(),
            Some("41"),
            "the cause travels with the record"
        );
        assert!(
            state
                .reported
                .values()
                .all(|report| *report == NodeReport::LoadFailed),
            "none of the three is re-attempted"
        );
    }

    /// A store that loads every node it is asked to, as handle `node_id`,
    /// unless scripted to refuse it, and records each load and unload in
    /// order.
    struct LoadingStore {
        refuse: Vec<u32>,
        loads: Vec<u32>,
        unloads: Vec<u64>,
    }

    impl LoadingStore {
        fn refusing(refuse: &[u32]) -> Self {
            Self {
                refuse: refuse.to_vec(),
                loads: Vec::new(),
                unloads: Vec::new(),
            }
        }
    }

    impl DriverStoreCall for LoadingStore {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            match StoreRequest::decode(request)? {
                StoreRequest::Load { node_id, .. } => {
                    self.loads.push(node_id);
                    if self.refuse.contains(&node_id) {
                        return encode_error_reply(reply, Errno::PermissionDenied);
                    }
                    tairix_abi::driver_store::encode_load_reply(reply, u64::from(node_id))
                }
                StoreRequest::Unload { handle } => {
                    self.unloads.push(handle);
                    encode_unload_reply(reply)
                }
                _ => Err(Errno::NotImplemented),
            }
        }
    }

    const CONSUMER_KEY: HwMatchKey = HwMatchKey::virtio(0x0C0C);
    const SUPPLIER_KEY: HwMatchKey = HwMatchKey::virtio(0x5C5C);

    /// Node `id`, matched by `key`, whose clock links name each of
    /// `suppliers`.
    fn linked(id: u32, key: HwMatchKey, suppliers: &[u32]) -> HwNode {
        use tairix_abi::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
        use tairix_abi::hwlink::LinkRequest;
        let mut node = HwNode::new(id, 1, HwDeviceClass::Other);
        node.push_match_key(key).expect("key fits");
        for (index, &supplier) in (0u8..).zip(suppliers) {
            let request = LinkRequest::new(
                CLOCK_CONTROLLER_ENDPOINTS.endpoint(supplier),
                index,
                &[1],
                b"",
            )
            .expect("valid");
            node.push_resource(tairix_abi::HwResource::request(&request))
                .expect("room");
        }
        node
    }

    /// `node`, its clock endpoint live.
    fn serving(mut node: HwNode) -> HwNode {
        node.set_serves(tairix_abi::hwlink::LinkRole::Clock, true);
        node
    }

    /// `node`, as the kernel shows it while a live driver holds it.
    fn driven(mut node: HwNode) -> HwNode {
        node.set_driven(true);
        node
    }

    fn count(n: u32) -> usize {
        usize::try_from(n).expect("a small count")
    }

    /// `node`, its last driver having ended as `ending`.
    fn ended_as(mut node: HwNode, ending: DriverEnding) -> HwNode {
        node.set_driver_ending(ending);
        node
    }

    /// Run one reaction over `nodes`, at `generation` and `now_ns`.
    fn react_at(
        nodes: &[HwNode],
        served: &[HwMatchKey],
        store: &mut LoadingStore,
        state: &mut AutoloadState,
        sink: &RecordingSink,
        (generation, now_ns): (u64, u64),
    ) {
        state.generation = generation;
        state.now_ns = now_ns;
        react(nodes, served, store, state, sink);
    }

    /// A fresh state whose passes run over the snapshot of `generation`.
    fn at_generation(generation: u64) -> AutoloadState {
        AutoloadState {
            generation,
            ..AutoloadState::default()
        }
    }

    /// Run one reaction over `nodes` with drivers for the keys in `served`.
    fn react(
        nodes: &[HwNode],
        served: &[HwMatchKey],
        store: &mut LoadingStore,
        state: &mut AutoloadState,
        sink: &RecordingSink,
    ) {
        let catalogue: Vec<CatalogueDriver> = (1u32..)
            .zip(served)
            .map(|(bundle_id, &key)| CatalogueDriver {
                bundle_id,
                bind_keys: vec![DriverBindKey::new(5, key)],
            })
            .collect();
        let candidates: Vec<_> = catalogue.iter().map(CatalogueDriver::candidate).collect();
        let mut reply = [0u8; 64];
        tairix_log::set_max_level(Level::Trace);
        match_and_load(
            nodes,
            &catalogue,
            &candidates,
            store,
            &mut reply,
            state,
            sink,
        );
    }

    #[test]
    fn a_consumer_waits_until_its_supplier_serves_not_merely_until_it_is_loaded() {
        let nodes = [linked(2, CONSUMER_KEY, &[5]), linked(5, SUPPLIER_KEY, &[])];
        let drivers = [CONSUMER_KEY, SUPPLIER_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        let sink = RecordingSink::new();
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(store.loads, [5], "the supplier, though later in the tree");
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Held(5)));
        assert_eq!(sink.level_of(events::NODE_HELD.0), Some(Level::Debug));

        let nodes = [nodes[0], serving(nodes[1])];
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(store.loads, [5, 2], "the consumer once its supplier serves");
        assert_eq!(state.suppliers.get(&2), Some(&vec![5]));
    }

    #[test]
    fn nodes_that_wait_on_each_other_load_rather_than_wait_forever() {
        let nodes = [linked(2, CONSUMER_KEY, &[5]), linked(5, SUPPLIER_KEY, &[2])];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        let sink = RecordingSink::new();
        react(
            &nodes,
            &[CONSUMER_KEY, SUPPLIER_KEY],
            &mut store,
            &mut state,
            &sink,
        );
        assert_eq!(store.loads, [2, 5]);

        let nodes = [linked(2, CONSUMER_KEY, &[2])];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);
        assert_eq!(store.loads, [2], "a node naming itself");
    }

    #[test]
    fn a_member_of_a_cycle_still_waits_for_a_supplier_outside_it() {
        const OUTSIDE_KEY: HwMatchKey = HwMatchKey::virtio(0x0757);
        let nodes = [
            linked(2, CONSUMER_KEY, &[5]),
            linked(5, SUPPLIER_KEY, &[2, 7]),
            linked(7, OUTSIDE_KEY, &[]),
        ];
        let drivers = [CONSUMER_KEY, SUPPLIER_KEY, OUTSIDE_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        let sink = RecordingSink::new();
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(store.loads, [2, 7]);
        assert_eq!(state.reported.get(&5), Some(&NodeReport::Held(7)));

        let nodes = [nodes[0], nodes[1], serving(nodes[2])];
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(store.loads, [2, 7, 5]);
    }

    #[test]
    fn a_supplier_no_driver_serves_or_whose_load_was_refused_holds_nothing() {
        let nodes = [linked(2, CONSUMER_KEY, &[5]), linked(5, SUPPLIER_KEY, &[])];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        let sink = RecordingSink::new();
        react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);
        assert_eq!(store.loads, [2], "nothing serves the supplier");

        let mut store = LoadingStore::refusing(&[5]);
        let mut state = AutoloadState::default();
        react(
            &nodes,
            &[CONSUMER_KEY, SUPPLIER_KEY],
            &mut store,
            &mut state,
            &sink,
        );
        assert_eq!(
            store.loads,
            [5, 2],
            "a refused supplier releases its consumer"
        );
        assert!(state.bindings.contains_key(&2));
    }

    #[test]
    fn a_consumer_naming_a_supplier_not_in_the_tree_is_held() {
        let nodes = [linked(2, CONSUMER_KEY, &[9])];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        let sink = RecordingSink::new();
        react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);
        assert!(store.loads.is_empty());
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Held(9)));
        assert_eq!(
            sink.field_of(events::NODE_HELD.0, "supplier").as_deref(),
            Some("0000000000000009")
        );
    }

    #[test]
    fn a_vanished_supplier_takes_its_consumer_down_first_and_leaves_it_held() {
        let nodes = [linked(2, CONSUMER_KEY, &[5]), linked(5, SUPPLIER_KEY, &[])];
        let drivers = [CONSUMER_KEY, SUPPLIER_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = AutoloadState::default();
        let sink = RecordingSink::new();
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        let nodes = [nodes[0], serving(nodes[1])];
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(store.loads, [5, 2]);
        let mut reply = [0u8; 64];
        unload_vanished(&|id| id != 5, &mut store, &mut reply, &mut state, &sink);
        assert_eq!(store.unloads, [2, 5], "the consumer before its supplier");
        assert!(state.bindings.is_empty() && state.suppliers.is_empty());
        react(
            &nodes[..1],
            &[CONSUMER_KEY, SUPPLIER_KEY],
            &mut store,
            &mut state,
            &sink,
        );
        assert_eq!(store.loads, [5, 2], "no second load: its supplier is gone");
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Held(5)));
    }

    #[test]
    fn a_driver_that_ends_leaves_its_node_unbound_once_and_is_never_reloaded() {
        let nodes = [linked(2, CONSUMER_KEY, &[])];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = at_generation(1);
        let sink = RecordingSink::new();
        react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);

        state.generation = 2;
        react(
            &[driven(nodes[0])],
            &[CONSUMER_KEY],
            &mut store,
            &mut state,
            &sink,
        );
        assert!(
            state.bindings.contains_key(&2),
            "a driven node keeps its driver"
        );

        for generation in [3, 4] {
            state.generation = generation;
            react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);
        }
        assert_eq!(store.loads, [2]);
        assert!(
            store.unloads.is_empty(),
            "an ended driver has nothing to unload"
        );
        assert!(state.bindings.is_empty());
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Ended));
        let ended = events::NODE_DRIVER_ENDED.0;
        assert_eq!(sink.ids().iter().filter(|&&id| id == ended).count(), 1);
        assert_eq!(sink.level_of(ended), Some(Level::Info));
    }

    #[test]
    fn a_driver_that_fails_is_reloaded_when_due_until_its_budget_is_spent() {
        let fresh = linked(2, CONSUMER_KEY, &[]);
        let failed = [ended_as(fresh, DriverEnding::Failed)];
        let keys = [CONSUMER_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = at_generation(1);
        let sink = RecordingSink::new();
        react(&[fresh], &keys, &mut store, &mut state, &sink);
        let (mut generation, mut now) = (1, 0);
        for attempt in 0..RESTART_LIMIT {
            generation += 1;
            now += 1_000;
            react_at(
                &failed,
                &keys,
                &mut store,
                &mut state,
                &sink,
                (generation, now),
            );
            assert_eq!(state.reported.get(&2), Some(&NodeReport::Restarting));
            let due = state.next_restart_ns().expect("a restart is owed");
            assert_eq!(due - now, RESTART_DELAY_NS << attempt);
            react_at(
                &failed,
                &keys,
                &mut store,
                &mut state,
                &sink,
                (generation, due - 1),
            );
            assert_eq!(
                store.loads.len(),
                1 + count(attempt),
                "not before it is due"
            );
            now = due;
            react_at(
                &failed,
                &keys,
                &mut store,
                &mut state,
                &sink,
                (generation, now),
            );
            assert_eq!(store.loads.len(), 2 + count(attempt));
            assert_eq!(state.next_restart_ns(), None, "nothing more is owed");
        }
        react_at(
            &failed,
            &keys,
            &mut store,
            &mut state,
            &sink,
            (generation + 1, now + 1_000),
        );
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Ended));
        react_at(
            &failed,
            &keys,
            &mut store,
            &mut state,
            &sink,
            (generation + 2, u64::MAX),
        );
        assert_eq!(store.loads.len(), 1 + count(RESTART_LIMIT));
        let restarting = events::NODE_DRIVER_RESTARTING.0;
        assert_eq!(sink.level_of(restarting), Some(Level::Warn));
        assert_eq!(
            sink.field_of(restarting, "ending").as_deref(),
            Some("failed")
        );
        assert_eq!(
            sink.field_of(restarting, "delay_ms").as_deref(),
            Some("500")
        );
        let restarts = sink.field_of(events::NODE_DRIVER_ENDED.0, "restarts");
        assert_eq!(restarts.as_deref(), Some("3"));
    }

    #[test]
    fn a_driver_that_served_past_the_quiet_period_has_its_restarts_forgiven() {
        let fresh = linked(2, CONSUMER_KEY, &[]);
        let crashed = [ended_as(fresh, DriverEnding::Crashed)];
        let keys = [CONSUMER_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = at_generation(1);
        let sink = RecordingSink::new();
        react(&[fresh], &keys, &mut store, &mut state, &sink);
        react_at(&crashed, &keys, &mut store, &mut state, &sink, (2, 1_000));
        let due = state.next_restart_ns().expect("a restart is owed");
        react_at(&crashed, &keys, &mut store, &mut state, &sink, (2, due));
        assert_eq!(state.restarts.get(&2).map(|r| r.attempts), Some(1));

        react_at(
            &crashed,
            &keys,
            &mut store,
            &mut state,
            &sink,
            (3, due + RESTART_QUIET_NS),
        );
        let restart = state.restarts.get(&2).copied().expect("restarted again");
        assert_eq!(
            restart.attempts, 1,
            "a fault once in a long while is forgiven"
        );
        assert_eq!(
            restart.due_ns,
            Some(due + RESTART_QUIET_NS + RESTART_DELAY_NS)
        );
    }

    #[test]
    fn a_driver_that_declines_or_is_refused_is_never_reloaded() {
        for ending in [DriverEnding::Clean, DriverEnding::Refused] {
            let fresh = linked(2, CONSUMER_KEY, &[]);
            let mut store = LoadingStore::refusing(&[]);
            let mut state = at_generation(1);
            let sink = RecordingSink::new();
            react(&[fresh], &[CONSUMER_KEY], &mut store, &mut state, &sink);
            let ended = [ended_as(fresh, ending)];
            react_at(
                &ended,
                &[CONSUMER_KEY],
                &mut store,
                &mut state,
                &sink,
                (2, 0),
            );
            react_at(
                &ended,
                &[CONSUMER_KEY],
                &mut store,
                &mut state,
                &sink,
                (3, u64::MAX),
            );
            assert_eq!(store.loads, [2], "{ending:?}");
            assert_eq!(state.reported.get(&2), Some(&NodeReport::Ended));
            let named = sink.field_of(events::NODE_DRIVER_ENDED.0, "ending");
            assert_eq!(named.as_deref(), Some(ending.name()));
        }
    }

    #[test]
    fn a_supplier_owed_a_restart_holds_its_consumers_until_it_serves_again() {
        let nodes = [linked(2, CONSUMER_KEY, &[5]), linked(5, SUPPLIER_KEY, &[])];
        let drivers = [CONSUMER_KEY, SUPPLIER_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = at_generation(1);
        let sink = RecordingSink::new();
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        let failed = [nodes[0], ended_as(nodes[1], DriverEnding::Failed)];
        react_at(&failed, &drivers, &mut store, &mut state, &sink, (2, 0));
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Held(5)));
        let due = state.next_restart_ns().expect("a restart is owed");
        react_at(&failed, &drivers, &mut store, &mut state, &sink, (2, due));
        assert_eq!(
            store.loads,
            [5, 5],
            "the supplier, and still not its consumer"
        );
        let serving = [nodes[0], serving(nodes[1])];
        react_at(&serving, &drivers, &mut store, &mut state, &sink, (3, due));
        assert_eq!(store.loads, [5, 5, 2]);
    }

    #[test]
    fn the_snapshot_a_driver_was_loaded_against_does_not_say_it_ended() {
        let nodes = [linked(2, CONSUMER_KEY, &[])];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = at_generation(5);
        let sink = RecordingSink::new();
        react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);
        assert!(!state.any_ended(&nodes));
        react(&nodes, &[CONSUMER_KEY], &mut store, &mut state, &sink);
        assert_eq!(store.loads, [2]);
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Bound));
    }

    #[test]
    fn a_supplier_whose_driver_ends_releases_the_consumers_it_held() {
        // The consumer sits first, so only a further pass can release it.
        let nodes = [linked(2, CONSUMER_KEY, &[5]), linked(5, SUPPLIER_KEY, &[])];
        let drivers = [CONSUMER_KEY, SUPPLIER_KEY];
        let mut store = LoadingStore::refusing(&[]);
        let mut state = at_generation(1);
        let sink = RecordingSink::new();
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(state.reported.get(&2), Some(&NodeReport::Held(5)));

        state.generation = 2;
        assert!(state.any_ended(&nodes));
        react(&nodes, &drivers, &mut store, &mut state, &sink);
        assert_eq!(store.loads, [5, 2]);
        assert_eq!(state.reported.get(&5), Some(&NodeReport::Ended));
        assert!(state.bindings.contains_key(&2));
    }
}
