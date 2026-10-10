//! Stateful property model for `kernel/sec` (Bronze).
//!
//! the charter requires the capability-critical paths to carry a `proptest`-style
//! stateful model. `kernel/sec/tests/proptest_invariants.rs` already checks
//! the two lib-level delegation properties in isolation; this model lifts
//! them to the **registry** level: a randomised sequence of commands drives a
//! live [`CapTable`] of [`TaskCapabilities`] against an independent reference
//! model, asserting after every command that
//!
//! * a derived task's effective set is exactly `user_grant ∩ manifest_request`
//!   and therefore a subset of both (no ambient authority),
//! * [`TaskCapabilities::delegate`] never widens the effective set and a
//!   refused delegation leaves it untouched,
//! * [`TaskCapabilities::revoke`] only ever shrinks the effective set, and
//! * the registry's contents (membership, cardinality, per-task effective
//!   set) match the model.
//!
//! A second model drives the session bookkeeping — placement by every
//! selector, departure, and the exits an anchor's session holds — against a
//! naive reference that recomputes membership from each process's chain,
//! asserting after every command that no emptied session is kept, every
//! member is indexed under its session and each ancestor and nothing else,
//! nothing is admitted into an ending session or past the depth bound, and
//! every exit is retired exactly once.
//!
//! Unlike the fuzz harnesses this generates structured command
//! sequences and lets proptest **shrink** any counterexample.
//!
//! ## Wall-clock budget
//!
//! The shared `tairix_fuzzseed::prop::drive` runner owns the seed/budget
//! policy (one definition): a plain `cargo test` runs [`SMOKE_CASES`]
//! sequences **once** from a fresh, logged seed; `cargo xtask proptest --soak`
//! exports `TAIRIX_PROPTEST_BUDGET_SECS` and the runner repeats
//! [`BUDGET_BATCH_CASES`] batches off the same continuing RNG until the
//! deadline. The seed is logged at the start of each run (pinnable via
//! `--seed`), so a fresh-seed counterexample is still reproducible.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use tairix_abi::CapabilityId;
use tairix_caps::CapabilitySet;
use tairix_kernel_sec::{CapTable, ProcessId, TaskCapabilities, TaskId, UserId};
use tairix_log::{Event, Sink};

/// Sequences run by a plain `cargo test` (no budget set).
const SMOKE_CASES: u32 = 256;
/// Sequences per batch under a wall-clock budget.
const BUDGET_BATCH_CASES: u32 = 256;
/// Highest capability id drawn by the model.
const CAP_MAX: u16 = 12;
/// Number of distinct task slots the registry juggles.
const TASKS: u64 = 4;

/// Sink that discards events; this model checks invariants, not audit text
/// (the per-decision audit emission is covered by the unit tests).
struct NullSink;
impl Sink for NullSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

fn cap(id: u16) -> CapabilityId {
    CapabilityId::from_raw(id).expect("id within CAPABILITY_ID_MAX")
}

fn build(ids: &[u16]) -> CapabilitySet {
    let mut s = CapabilitySet::empty();
    for &id in ids {
        s.insert(cap(id));
    }
    s
}

fn to_model(set: &CapabilitySet) -> BTreeSet<u16> {
    set.iter().map(CapabilityId::as_u16).collect()
}

/// One operation on the registry under test.
#[derive(Clone, Debug)]
enum Cmd {
    /// Derive a fresh task and insert it (replacing any prior record).
    Insert {
        task: u64,
        user_grant: Vec<u16>,
        manifest: Vec<u16>,
    },
    Delegate {
        task: u64,
        requested: Vec<u16>,
    },
    Revoke {
        task: u64,
        cap: u16,
    },
    Remove {
        task: u64,
    },
}

/// Reference image of one task's authority.
struct TaskModel {
    user_grant: BTreeSet<u16>,
    manifest: BTreeSet<u16>,
    effective: BTreeSet<u16>,
}

fn id_vec() -> impl Strategy<Value = Vec<u16>> {
    prop::collection::vec(0u16..=CAP_MAX, 0..=6)
}

fn task_id() -> impl Strategy<Value = u64> {
    0u64..TASKS
}

fn command() -> impl Strategy<Value = Cmd> {
    prop_oneof![
        (task_id(), id_vec(), id_vec()).prop_map(|(task, user_grant, manifest)| Cmd::Insert {
            task,
            user_grant,
            manifest,
        }),
        (task_id(), id_vec()).prop_map(|(task, requested)| Cmd::Delegate { task, requested }),
        (task_id(), 0u16..=CAP_MAX).prop_map(|(task, cap)| Cmd::Revoke { task, cap }),
        task_id().prop_map(|task| Cmd::Remove { task }),
    ]
}

fn program() -> impl Strategy<Value = Vec<Cmd>> {
    prop::collection::vec(command(), 0..=48)
}

#[test]
fn captable_tracks_reference_model() {
    tairix_fuzzseed::prop::drive(
        "captable_tracks_reference_model",
        SMOKE_CASES,
        BUDGET_BATCH_CASES,
        program(),
        |cmds| check_captable(&cmds),
    );
}

/// The per-program check `drive` runs, split out so the `#[test]` wrapper
/// stays small (clippy `too_many_lines`).
fn check_captable(cmds: &[Cmd]) -> Result<(), TestCaseError> {
    let sink = NullSink;
    let mut table = CapTable::new();
    let mut model: BTreeMap<u64, TaskModel> = BTreeMap::new();

    for c in cmds {
        match c {
            Cmd::Insert {
                task,
                user_grant,
                manifest,
            } => {
                let ug = build(user_grant);
                let mf = build(manifest);
                let caps = TaskCapabilities::derive(ProcessId(*task), UserId(1), ug, mf, &sink);
                // Derive must intersect: effective ⊆ both inputs.
                prop_assert!(caps.effective().is_subset_of(&ug));
                prop_assert!(caps.effective().is_subset_of(&mf));
                table.insert(caps);

                let ug_m: BTreeSet<u16> = user_grant.iter().copied().collect();
                let mf_m: BTreeSet<u16> = manifest.iter().copied().collect();
                let eff_m: BTreeSet<u16> = ug_m.intersection(&mf_m).copied().collect();
                model.insert(
                    *task,
                    TaskModel {
                        user_grant: ug_m,
                        manifest: mf_m,
                        effective: eff_m,
                    },
                );
            }
            Cmd::Delegate { task, requested } => {
                let req = build(requested);
                let req_m: BTreeSet<u16> = requested.iter().copied().collect();
                let live = table.caps_for_mut(TaskId(*task));
                let entry = model.get_mut(task);
                match (live, entry) {
                    (Some(caps), Some(state)) => {
                        let before = to_model(caps.effective());
                        let res = caps.delegate(&req, &sink);
                        if req_m.is_subset(&state.effective) {
                            prop_assert!(res.is_ok());
                            prop_assert_eq!(to_model(caps.effective()), req_m.clone());
                            state.effective = req_m;
                        } else {
                            prop_assert!(res.is_err());
                            // Refused delegation leaves the set untouched.
                            prop_assert_eq!(to_model(caps.effective()), before);
                        }
                    }
                    (None, None) => {}
                    _ => return Err(TestCaseError::fail("registry/model membership diverged")),
                }
            }
            Cmd::Revoke { task, cap: c } => {
                let live = table.caps_for_mut(TaskId(*task));
                let entry = model.get_mut(task);
                match (live, entry) {
                    (Some(caps), Some(state)) => {
                        let before = to_model(caps.effective());
                        let was = caps.revoke(cap(*c), &sink);
                        prop_assert_eq!(was, state.effective.remove(c));
                        // Revoke only ever shrinks the effective set.
                        prop_assert!(caps
                            .effective()
                            .is_subset_of(&build(&before.iter().copied().collect::<Vec<_>>())));
                        prop_assert_eq!(to_model(caps.effective()), state.effective.clone());
                    }
                    (None, None) => {}
                    _ => return Err(TestCaseError::fail("registry/model membership diverged")),
                }
            }
            Cmd::Remove { task } => {
                let live = table.remove(ProcessId(*task)).is_some();
                let modelled = model.remove(task).is_some();
                prop_assert_eq!(live, modelled);
            }
        }

        // Registry-wide invariants after each command.
        prop_assert_eq!(table.len(), model.len());
        prop_assert_eq!(table.is_empty(), model.is_empty());
        for (task, state) in &model {
            let caps = table
                .caps_for(TaskId(*task))
                .ok_or_else(|| TestCaseError::fail("modelled task missing from registry"))?;
            prop_assert_eq!(to_model(caps.effective()), state.effective.clone());
            // The upstream bounds never change; effective stays within them.
            prop_assert_eq!(to_model(caps.user_grant()), state.user_grant.clone());
            prop_assert_eq!(to_model(caps.manifest_request()), state.manifest.clone());
        }
    }
    Ok(())
}

use tairix_abi::{ProcId, SpawnSession};
use tairix_kernel_sec::{HeldExit, PlacementError, ROOT_SESSION};

/// Process numbers the session model juggles.
const SESSION_PIDS: u64 = 8;

/// How a spawn asks to be placed.
#[derive(Clone, Copy, Debug)]
enum Selector {
    Inherit,
    New,
    Anchored,
    Join(u64),
}

/// One operation on the session bookkeeping under test.
#[derive(Clone, Debug)]
enum SessionCmd {
    /// `spawner` (the kernel when `None`) admits `pid` as `selector` asks.
    Admit {
        spawner: Option<u64>,
        pid: u64,
        selector: Selector,
    },
    /// `pid` dies: its record goes, and its exit is held while it anchors a
    /// session with members.
    Remove { pid: u64 },
}

fn session_pid() -> impl Strategy<Value = u64> {
    1u64..=SESSION_PIDS
}

fn selector() -> impl Strategy<Value = Selector> {
    prop_oneof![
        Just(Selector::Inherit),
        Just(Selector::New),
        Just(Selector::Anchored),
        session_pid().prop_map(Selector::Join),
    ]
}

fn session_command() -> impl Strategy<Value = SessionCmd> {
    prop_oneof![
        3 => (prop::option::weighted(0.8, session_pid()), session_pid(), selector()).prop_map(
            |(spawner, pid, selector)| SessionCmd::Admit {
                spawner,
                pid,
                selector,
            }
        ),
        1 => session_pid().prop_map(|pid| SessionCmd::Remove { pid }),
    ]
}

fn session_program() -> impl Strategy<Value = Vec<SessionCmd>> {
    prop::collection::vec(session_command(), 0..=64)
}

fn instance_of(serial: u64) -> ProcId {
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&serial.to_le_bytes());
    raw[8] = 0x5E;
    ProcId::from_raw(raw)
}

/// Reference image of one live, non-root session.
#[derive(Clone, Copy, Debug)]
struct SessionModel {
    parent: ProcId,
    depth: u8,
    ending: bool,
    held: Option<u64>,
}

/// Reference image of the whole bookkeeping, deliberately naive: membership
/// is recomputed from each process's chain rather than indexed.
#[derive(Default)]
struct World {
    /// Live process → (instance, session).
    procs: BTreeMap<u64, (ProcId, ProcId)>,
    sessions: BTreeMap<ProcId, SessionModel>,
    serial: u64,
}

impl World {
    fn chain(&self, session: ProcId) -> Vec<ProcId> {
        let mut out = Vec::new();
        let mut at = session;
        while let Some(node) = self.sessions.get(&at) {
            out.push(at);
            at = node.parent;
        }
        out
    }

    fn members(&self, session: ProcId) -> Vec<u64> {
        self.procs
            .iter()
            .filter(|(_, (_, own))| self.chain(*own).contains(&session))
            .map(|(pid, _)| *pid)
            .collect()
    }

    fn closed(&self, session: ProcId) -> bool {
        let mut at = session;
        while at != ROOT_SESSION {
            match self.sessions.get(&at) {
                Some(node) if !node.ending => at = node.parent,
                _ => return true,
            }
        }
        false
    }

    fn depth(&self, session: ProcId) -> Option<u8> {
        if session == ROOT_SESSION {
            Some(0)
        } else {
            self.sessions.get(&session).map(|node| node.depth)
        }
    }

    /// The depth of the session anchored at `anchor`, founding it inside
    /// `parent` if it does not exist yet.
    fn container(
        &self,
        anchor: ProcId,
        parent: ProcId,
    ) -> Result<(u8, Option<Founding>), PlacementError> {
        if let Some(node) = self.sessions.get(&anchor) {
            if self.closed(anchor) {
                return Err(PlacementError::Ending);
            }
            return Ok((node.depth, None));
        }
        if self.closed(parent) {
            return Err(PlacementError::Ending);
        }
        match self.depth(parent) {
            Some(depth) if depth < tairix_kernel_sec::session::SESSION_DEPTH_MAX => {
                let depth = depth + 1;
                Ok((
                    depth,
                    Some(Founding {
                        anchor,
                        parent,
                        depth,
                    }),
                ))
            }
            _ => Err(PlacementError::TooDeep),
        }
    }

    /// Admit `pid` under a fresh instance, or say why not.
    fn admit(
        &mut self,
        spawner: Option<u64>,
        pid: u64,
        selector: Selector,
    ) -> Result<ProcId, PlacementError> {
        let (parent, anchor) = match spawner {
            None => (ROOT_SESSION, ProcId::KERNEL),
            Some(spawner) => {
                let &(instance, session) =
                    self.procs.get(&spawner).ok_or(PlacementError::NotFound)?;
                (session, instance)
            }
        };
        let plan = match selector {
            Selector::Inherit => {
                if self.closed(parent) {
                    return Err(PlacementError::Ending);
                }
                (None, parent, None)
            }
            Selector::Join(target) => {
                let &(_, destination) = self
                    .procs
                    .get(&target)
                    .filter(|_| parent == ROOT_SESSION || self.members(parent).contains(&target))
                    .ok_or(PlacementError::NotFound)?;
                if self.closed(destination) {
                    return Err(PlacementError::Ending);
                }
                (None, destination, None)
            }
            Selector::Anchored => {
                if anchor == ProcId::KERNEL {
                    return Err(PlacementError::NotFound);
                }
                let (_, container) = self.container(anchor, parent)?;
                (container, anchor, None)
            }
            Selector::New => {
                let (session, depth, container) = if anchor == ProcId::KERNEL {
                    (ROOT_SESSION, 0, None)
                } else {
                    let (depth, container) = self.container(anchor, parent)?;
                    (anchor, depth, container)
                };
                if depth >= tairix_kernel_sec::session::SESSION_DEPTH_MAX {
                    return Err(PlacementError::TooDeep);
                }
                (container, session, Some(depth + 1))
            }
        };
        if self.procs.contains_key(&pid) {
            return Err(PlacementError::NotFound);
        }
        let (container, session, founds) = plan;
        self.serial += 1;
        let instance = instance_of(self.serial);
        if let Some(Founding {
            anchor,
            parent,
            depth,
        }) = container
        {
            self.sessions.insert(anchor, fresh(parent, depth));
        }
        let own = match founds {
            Some(depth) => {
                self.sessions.insert(instance, fresh(session, depth));
                instance
            }
            None => session,
        };
        self.procs.insert(pid, (instance, own));
        Ok(instance)
    }

    /// `pid` departs: the sessions it empties go, releasing what they held,
    /// and the session it anchored ends if members remain.
    fn depart(&mut self, pid: u64) -> Option<(ProcId, Vec<u64>)> {
        let (instance, session) = self.procs.remove(&pid)?;
        let mut released = Vec::new();
        let mut at = session;
        while let Some(node) = self.sessions.get(&at).copied() {
            if self.members(at).is_empty() {
                self.sessions.remove(&at);
                released.extend(node.held);
            }
            at = node.parent;
        }
        if let Some(node) = self.sessions.get_mut(&instance) {
            node.ending = true;
        }
        Some((instance, released))
    }
}

/// A session the model founds: its anchor, the session it nests in, and its
/// depth.
#[derive(Copy, Clone, Debug)]
struct Founding {
    anchor: ProcId,
    parent: ProcId,
    depth: u8,
}

fn fresh(parent: ProcId, depth: u8) -> SessionModel {
    SessionModel {
        parent,
        depth,
        ending: false,
        held: None,
    }
}

#[test]
fn sessions_track_reference_model() {
    tairix_fuzzseed::prop::drive(
        "sessions_track_reference_model",
        SMOKE_CASES,
        BUDGET_BATCH_CASES,
        session_program(),
        |cmds| check_sessions(&cmds),
    );
}

/// What a run has seen that the table alone does not say.
#[derive(Default)]
struct Ledger {
    /// Processes whose exit is held. A held exit keeps its number from the
    /// draw, so none of them is admitted again until it is released.
    held: BTreeSet<u64>,
    /// Every instance the table issued.
    issued: Vec<ProcId>,
}

fn check_sessions(cmds: &[SessionCmd]) -> Result<(), TestCaseError> {
    let mut table = CapTable::new();
    let mut world = World::default();
    let mut ledger = Ledger::default();
    for cmd in cmds {
        match *cmd {
            SessionCmd::Admit {
                spawner,
                pid,
                selector,
            } => admit_in_both(
                &mut table,
                &mut world,
                &mut ledger,
                (spawner, pid, selector),
            )?,
            SessionCmd::Remove { pid } => {
                remove_from_both(&mut table, &mut world, &mut ledger, pid)?;
            }
        }
        check_agreement(&table, &world, &ledger)?;
    }
    Ok(())
}

/// Admit `pid` for `spawner` into the table and the model alike, requiring
/// the same answer from both.
fn admit_in_both(
    table: &mut CapTable,
    world: &mut World,
    ledger: &mut Ledger,
    (spawner, pid, selector): (Option<u64>, u64, Selector),
) -> Result<(), TestCaseError> {
    if ledger.held.contains(&pid) {
        return Ok(());
    }
    let request = match selector {
        Selector::Inherit => SpawnSession::Inherit,
        Selector::New => SpawnSession::New,
        Selector::Anchored => SpawnSession::Anchored,
        Selector::Join(target) => SpawnSession::Join(
            world
                .procs
                .get(&target)
                .map_or(instance_of(u64::MAX), |(instance, _)| *instance),
        ),
    };
    let spawner_id = spawner.map_or(ProcessId::KERNEL, ProcessId);
    let expected = world.admit(spawner, pid, selector);
    let instance = expected.unwrap_or_else(|_| instance_of(world.serial + 1));
    let record = TaskCapabilities::derive(
        ProcessId(pid),
        UserId(1),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        &NullSink,
    )
    .with_proc_id(instance);
    let placed = table
        .resolve_placement(spawner_id, request, false)
        .and_then(|placement| table.admit(record, placement));
    prop_assert_eq!(
        placed,
        expected.map(|_| ()),
        "{:?} admitting {} as {:?}",
        spawner,
        pid,
        selector
    );
    if expected.is_ok() {
        ledger.issued.push(instance);
    }
    Ok(())
}

/// `pid` departs the table and the model alike: both release the same held
/// exits, and both hold its own exit or both hand it back.
fn remove_from_both(
    table: &mut CapTable,
    world: &mut World,
    ledger: &mut Ledger,
    pid: u64,
) -> Result<(), TestCaseError> {
    let removed = table.remove(ProcessId(pid));
    let modelled = world.depart(pid);
    prop_assert_eq!(removed.is_some(), modelled.is_some());
    let (Some(removed), Some((instance, released))) = (removed, modelled) else {
        return Ok(());
    };
    let exits: Vec<u64> = removed.released.iter().map(|exit| exit.process.0).collect();
    prop_assert_eq!(&exits, &released, "released by {}", pid);
    for exit in exits {
        prop_assert!(
            ledger.held.remove(&exit),
            "{} released but never held",
            exit
        );
    }
    let own = HeldExit {
        process: ProcessId(pid),
        status: Some(137),
    };
    match (
        table.hold_exit(instance, own),
        world.sessions.get_mut(&instance),
    ) {
        (Ok(()), Some(node)) => {
            node.held = Some(pid);
            ledger.held.insert(pid);
        }
        (Err(back), None) => prop_assert_eq!(back, own),
        _ => return Err(TestCaseError::fail("held exit and model disagree")),
    }
    Ok(())
}

/// Every session the model keeps is the table's, member for member, and
/// nothing the model has let go of lingers in the table.
fn check_agreement(table: &CapTable, world: &World, ledger: &Ledger) -> Result<(), TestCaseError> {
    for (&session, node) in &world.sessions {
        let real: Vec<u64> = table
            .sessions()
            .members_after(session, None)
            .map(|member| member.0)
            .collect();
        prop_assert_eq!(real, world.members(session));
        prop_assert!(
            !world.members(session).is_empty(),
            "no empty session is kept"
        );
        prop_assert_eq!(table.sessions().is_ending(session), node.ending);
        let enclosing = world
            .chain(session)
            .iter()
            .skip(1)
            .any(|at| world.sessions[at].ending);
        prop_assert_eq!(table.sessions().enclosing_ending(session), enclosing);
        prop_assert!(node.depth <= tairix_kernel_sec::session::SESSION_DEPTH_MAX);
    }
    for (&pid, &(_, session)) in &world.procs {
        let record = table
            .caps_of_process(ProcessId(pid))
            .ok_or_else(|| TestCaseError::fail("modelled process missing"))?;
        prop_assert_eq!(record.session(), session);
    }
    for instance in &ledger.issued {
        if !world.sessions.contains_key(instance) {
            prop_assert_eq!(table.sessions().members_after(*instance, None).count(), 0);
            prop_assert!(!table.sessions().is_ending(*instance));
        }
    }
    let holding: BTreeSet<u64> = world
        .sessions
        .values()
        .filter_map(|node| node.held)
        .collect();
    prop_assert_eq!(
        &holding,
        &ledger.held,
        "an exit is held exactly while its session lasts"
    );
    Ok(())
}
