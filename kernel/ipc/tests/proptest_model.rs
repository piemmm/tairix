//! Stateful property models for the `kernel/ipc` port and call endpoint
//! (Bronze).
//!
//! [`Port`] and the call endpoint are the capability-checked IPC dispatch
//! endpoints. The charter requires each to carry a `proptest`-style stateful
//! model alongside its unit tests and the fuzz harness (`tests/fuzz_port.rs`);
//! the call endpoint's is `call_endpoint_lifecycle_tracks_reference_model`,
//! the port's is described below. Where the fuzz harness hammers
//! raw `(caps, payload)` bytes for crashes, this model generates a *structured*
//! sequence of `send` / `recv` / `admit` / `destroy` commands from several
//! sending instances and replays it against an independent reference model,
//! letting proptest **shrink** any counterexample to a minimal failing program.
//! The invariants checked after every command:
//!
//! * `send` is **fail-closed** in the exact `Port::send` precedence — closed
//!   port, then capabilities, then size, then the admitted sender, then the
//!   sender's own depth — checked against a mirror that never consults the
//!   live port.
//! * a sender is refused for room only when *its own* lane is full: no other
//!   sender's messages ever take its room, and the room peek a parked sender
//!   waits on agrees with what `send` would decide.
//! * senders are served in turn, each one's messages byte-for-byte and oldest
//!   first, so a sender cannot mutate an accepted payload and a deep backlog
//!   waits behind one message of every other sender's.
//! * once a sender is admitted, no other instance's message is taken, a
//!   refused one occupies no room, and whatever another instance had queued
//!   is discarded with the admission.
//! * occupancy equals the model and no lane ever exceeds the declared depth.
//! * once destroyed, every send fails closed regardless of authority.
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

use std::collections::{BTreeMap, VecDeque};

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use tairix_abi::ipc::IPC_MESSAGE_MAX_PAYLOAD_LEN;
use tairix_abi::{CapabilityId, Errno, ProcId, PROC_ID_LEN, RLIMIT_INFINITY};
use tairix_caps::CapabilitySet;
use tairix_kernel_ipc::{EndpointId, Port};
use tairix_kernel_sec::{ProcessId, TaskCapabilities, UserId};
use tairix_log::{set_max_level, Event, Level, Sink};

/// Sequences run by a plain `cargo test` (no budget set).
const SMOKE_CASES: u32 = 256;
/// Sequences per batch under a wall-clock budget.
const BUDGET_BATCH_CASES: u32 = 256;
/// Port payload bound; well under the global ABI cap so the port-local
/// check is the binding one and oversize payloads are reached often.
const MAX_PAYLOAD: u32 = 16;
/// Each sender's lane depth.
const DEPTH: usize = 3;
/// Capability every sender must hold.
const REQUIRED_SEND_CAP: CapabilityId = CapabilityId::NET_RAW;

/// Capabilities a sender may be drawn with; includes the required cap plus
/// unrelated ones so both the satisfied and denied branches are exercised.
const CAP_UNIVERSE: &[CapabilityId] = &[
    CapabilityId::NET_RAW,
    CapabilityId::AUDIT_READ,
    CapabilityId::FS_MOUNT,
];

struct NullSink;
impl Sink for NullSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

fn caps_of(items: &[CapabilityId]) -> CapabilitySet {
    let mut set = CapabilitySet::empty();
    for c in items {
        set.insert(*c);
    }
    set
}

fn task_with(task_id: u64, caps: &CapabilitySet) -> TaskCapabilities {
    TaskCapabilities::derive(ProcessId(task_id), UserId(1), *caps, *caps, &NullSink)
}

/// Process instances a sender may be, and the one an `Admit` may name.
const INSTANCES: u8 = 3;

fn instance(index: u8) -> ProcId {
    ProcId::from_raw([0xA0 + index; PROC_ID_LEN])
}

fn authorised_port() -> Port {
    let creator = task_with(
        1,
        &caps_of(&[CapabilityId::IPC_BIND_PRIVILEGED, REQUIRED_SEND_CAP]),
    );
    Port::create(
        EndpointId(0x4242),
        &creator,
        caps_of(&[REQUIRED_SEND_CAP]),
        CapabilitySet::empty(),
        MAX_PAYLOAD,
        DEPTH,
        RLIMIT_INFINITY,
        &NullSink,
    )
    .expect("authorised creator binds the port")
}

/// One operation on the port under test.
#[derive(Clone, Debug)]
enum Cmd {
    /// Send a payload; `cap_mask` selects the sender's capabilities and
    /// `len` is the payload length (deterministically filled).
    Send {
        cap_mask: u8,
        len: usize,
        sender: u8,
    },
    Recv,
    /// Admit instance `sender` as the port's only sender.
    Admit {
        sender: u8,
    },
    Destroy,
}

fn command() -> impl Strategy<Value = Cmd> {
    prop_oneof![
        // Weight sends heavily so the lanes actually fill.
        6 => (0u8..8u8, 0usize..=24, 0..INSTANCES)
            .prop_map(|(cap_mask, len, sender)| Cmd::Send { cap_mask, len, sender }),
        3 => Just(Cmd::Recv),
        1 => (0..INSTANCES).prop_map(|sender| Cmd::Admit { sender }),
        1 => Just(Cmd::Destroy),
    ]
}

fn program() -> impl Strategy<Value = Vec<Cmd>> {
    prop::collection::vec(command(), 0..=48)
}

/// The reference port: each sender's lane, oldest first, and the senders with
/// a message waiting, the next served first.
#[derive(Default)]
struct Model {
    lanes: BTreeMap<u8, VecDeque<Vec<u8>>>,
    turn: VecDeque<u8>,
    closed: bool,
    admitted: Option<u8>,
}

impl Model {
    fn depth_of(&self, sender: u8) -> usize {
        self.lanes.get(&sender).map_or(0, VecDeque::len)
    }

    fn queued(&self) -> usize {
        self.lanes.values().map(VecDeque::len).sum()
    }

    fn push(&mut self, sender: u8, payload: Vec<u8>) {
        let lane = self.lanes.entry(sender).or_default();
        if lane.is_empty() {
            self.turn.push_back(sender);
        }
        lane.push_back(payload);
    }

    fn pop(&mut self) -> Option<(u8, Vec<u8>)> {
        let sender = self.turn.pop_front()?;
        let lane = self.lanes.get_mut(&sender)?;
        let payload = lane.pop_front()?;
        if lane.is_empty() {
            self.lanes.remove(&sender);
        } else {
            self.turn.push_back(sender);
        }
        Some((sender, payload))
    }

    fn admit(&mut self, sender: u8) -> bool {
        let before = self.queued();
        self.lanes.retain(|from, _| *from == sender);
        self.turn.retain(|from| *from == sender);
        self.admitted = Some(sender);
        self.queued() < before
    }

    fn destroy(&mut self) {
        self.closed = true;
        self.lanes.clear();
        self.turn.clear();
    }
}

#[test]
fn port_lifecycle_tracks_reference_model() {
    set_max_level(Level::Error);
    let effective_max =
        usize::try_from(u64::from(MAX_PAYLOAD).min(u64::from(IPC_MESSAGE_MAX_PAYLOAD_LEN)))
            .expect("payload bound fits usize");

    tairix_fuzzseed::prop::drive(
        "port_lifecycle_tracks_reference_model",
        SMOKE_CASES,
        BUDGET_BATCH_CASES,
        program(),
        move |cmds| {
            let sink = NullSink;
            let port = authorised_port();
            let mut model = Model::default();

            for c in &cmds {
                match c {
                    Cmd::Send {
                        cap_mask,
                        len,
                        sender: from,
                    } => {
                        let mut sender_caps = CapabilitySet::empty();
                        for (bit, cap) in CAP_UNIVERSE.iter().enumerate() {
                            if cap_mask & (1 << bit) != 0 {
                                sender_caps.insert(*cap);
                            }
                        }
                        let sender = task_with(0x100 + u64::from(*from), &sender_caps)
                            .with_proc_id(instance(*from));
                        let payload: Vec<u8> = (0..*len)
                            .map(|i| u8::try_from(i % 251).unwrap_or(0))
                            .collect();

                        // Mirror of `Port::send`'s precedence, never touching the
                        // live port: closed → caps → size → admission → the
                        // sender's own depth.
                        let caps_ok =
                            caps_of(&[REQUIRED_SEND_CAP]).is_subset_of(sender.effective());
                        let size_ok = payload.len() <= effective_max;
                        let want = if model.closed {
                            Err(Errno::NotFound)
                        } else if !caps_ok {
                            Err(Errno::PermissionDenied)
                        } else if !size_ok {
                            Err(Errno::MessageTooLarge)
                        } else if model.admitted.is_some_and(|only| only != *from) {
                            Err(Errno::PermissionDenied)
                        } else if model.depth_of(*from) >= DEPTH {
                            Err(Errno::WouldBlock)
                        } else {
                            Ok(())
                        };

                        let got = port.send(&sender, &payload, RLIMIT_INFINITY, &sink);
                        prop_assert_eq!(got, want);
                        if want.is_ok() {
                            model.push(*from, payload);
                        }
                    }
                    Cmd::Recv => match (port.recv(), model.pop()) {
                        (Some(msg), Some((from, want))) => {
                            prop_assert_eq!(msg.origin.proc_id(), instance(from));
                            prop_assert_eq!(msg.payload.as_bytes(), want.as_slice());
                        }
                        (None, None) => {}
                        (Some(_), None) => {
                            return Err(TestCaseError::fail("recv returned an unmodelled message"))
                        }
                        (None, Some(_)) => {
                            return Err(TestCaseError::fail("recv empty but model was not"))
                        }
                    },
                    Cmd::Admit { sender } => {
                        let discarded = model.admit(*sender);
                        prop_assert_eq!(port.admit(instance(*sender), &sink), Ok(discarded));
                    }
                    Cmd::Destroy => {
                        port.destroy(&sink);
                        model.destroy();
                    }
                }

                prop_assert_eq!(port.len(), model.queued());
                prop_assert_eq!(port.has_pending(), !model.turn.is_empty());
                prop_assert_eq!(port.is_closed(), model.closed);
                for sender in 0..INSTANCES {
                    prop_assert!(model.depth_of(sender) <= DEPTH);
                    prop_assert_eq!(
                        port.has_room_for(instance(sender)),
                        model.depth_of(sender) < DEPTH,
                        "the room a sender waits on is exactly what send decides"
                    );
                }
            }
            Ok(())
        },
    );
}

/// Callers a call model draws from.
const CALLERS: u8 = 3;
/// Each caller's depth of calls waiting on the server.
const CALL_DEPTH: usize = 2;
/// Request and reply bound of the modelled endpoint.
const CALL_MAX: u32 = 8;

/// One operation on the call endpoint under test. A `pick` selects among the
/// tickets the model knows, or names one nobody holds.
#[derive(Clone, Debug)]
enum CallCmd {
    Post { caller: u8, len: usize },
    Recv,
    Reply { pick: u8 },
    Take { caller: u8, pick: u8 },
    Await { caller: u8, pick: u8, thread: u8 },
    Cancel { caller: u8, pick: u8 },
    Destroy,
}

fn call_command() -> impl Strategy<Value = CallCmd> {
    prop_oneof![
        5 => (0..CALLERS, 0usize..=10).prop_map(|(caller, len)| CallCmd::Post { caller, len }),
        3 => Just(CallCmd::Recv),
        3 => any::<u8>().prop_map(|pick| CallCmd::Reply { pick }),
        3 => (0..CALLERS, any::<u8>()).prop_map(|(caller, pick)| CallCmd::Take { caller, pick }),
        2 => (0..CALLERS, any::<u8>(), 0u8..2)
            .prop_map(|(caller, pick, thread)| CallCmd::Await { caller, pick, thread }),
        1 => (0..CALLERS, any::<u8>()).prop_map(|(caller, pick)| CallCmd::Cancel { caller, pick }),
        1 => Just(CallCmd::Destroy),
    ]
}

/// Where a modelled call stands.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CallState {
    Pending(Vec<u8>),
    InService,
    Answered(Vec<u8>),
}

/// A modelled call: its caller, state, and the thread awaiting it.
#[derive(Clone, Debug)]
struct ModelCall {
    caller: u8,
    state: CallState,
    awaiter: Option<u64>,
}

/// The reference call endpoint: every outstanding call by ticket, each
/// caller's queued tickets oldest first, and the callers with a call
/// waiting, the next served first.
#[derive(Default)]
struct CallModel {
    calls: BTreeMap<u64, ModelCall>,
    queued: BTreeMap<u8, VecDeque<u64>>,
    turn: VecDeque<u8>,
    closed: bool,
}

impl CallModel {
    fn unanswered(&self, caller: u8) -> usize {
        self.calls
            .values()
            .filter(|call| call.caller == caller && !matches!(call.state, CallState::Answered(_)))
            .count()
    }

    /// A ticket the model knows, chosen by `pick`, or one no caller holds.
    fn ticket(&self, pick: u8, among: impl Fn(&ModelCall) -> bool) -> u64 {
        let known: Vec<u64> = self
            .calls
            .iter()
            .filter(|(_, call)| among(call))
            .map(|(&ticket, _)| ticket)
            .collect();
        if known.is_empty() || pick.is_multiple_of(5) {
            u64::MAX - u64::from(pick)
        } else {
            known[usize::from(pick) % known.len()]
        }
    }

    fn remove(&mut self, ticket: u64) -> Option<ModelCall> {
        let call = self.calls.remove(&ticket)?;
        if let Some(queue) = self.queued.get_mut(&call.caller) {
            queue.retain(|&queued| queued != ticket);
            if queue.is_empty() {
                self.turn.retain(|&waiting| waiting != call.caller);
            }
        }
        Some(call)
    }
}

/// The scheduler id each caller posts as.
fn poster_of(caller: u8) -> u64 {
    0x500 + u64::from(caller)
}

fn call_endpoint() -> tairix_kernel_ipc::CallEndpoint {
    let creator = task_with(1, &CapabilitySet::empty());
    tairix_kernel_ipc::CallEndpoint::create(
        EndpointId(0x4343),
        &creator,
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        tairix_kernel_ipc::CallEndpointLimits {
            max_request: CALL_MAX,
            max_reply: CALL_MAX,
            per_sender: CALL_DEPTH,
        },
        RLIMIT_INFINITY,
        &NullSink,
    )
    .expect("the endpoint binds")
}

fn sorted(tasks: impl Iterator<Item = u64>) -> Vec<u64> {
    let mut tasks: Vec<u64> = tasks.collect();
    tasks.sort_unstable();
    tasks.dedup();
    tasks
}

/// One run of the call endpoint against the reference model.
struct CallRun {
    ep: tairix_kernel_ipc::CallEndpoint,
    callers: Vec<TaskCapabilities>,
    model: CallModel,
}

/// The process number each caller posts as.
fn process(caller: u8) -> u64 {
    0x200 + u64::from(caller)
}

impl CallRun {
    fn new() -> Self {
        Self {
            ep: call_endpoint(),
            callers: (0..CALLERS)
                .map(|caller| task_with(process(caller), &CapabilitySet::empty()))
                .collect(),
            model: CallModel::default(),
        }
    }

    fn step(&mut self, cmd: &CallCmd) -> Result<(), TestCaseError> {
        match cmd {
            CallCmd::Post { caller, len } => self.post(*caller, *len)?,
            CallCmd::Recv => self.recv()?,
            CallCmd::Reply { pick } => self.reply(*pick)?,
            CallCmd::Take { caller, pick } => self.take(*caller, *pick)?,
            CallCmd::Await {
                caller,
                pick,
                thread,
            } => self.await_call(*caller, *pick, *thread)?,
            CallCmd::Cancel { caller, pick } => self.cancel(*caller, *pick)?,
            CallCmd::Destroy => {
                self.ep.destroy(&NullSink);
                self.model = CallModel {
                    closed: true,
                    ..CallModel::default()
                };
            }
        }
        prop_assert_eq!(self.ep.outstanding(), self.model.calls.len());
        prop_assert_eq!(self.ep.has_pending(), !self.model.turn.is_empty());
        prop_assert_eq!(self.ep.is_closed(), self.model.closed);
        for caller in 0..CALLERS {
            prop_assert!(self.model.unanswered(caller) <= CALL_DEPTH);
        }
        Ok(())
    }

    fn post(&mut self, caller: u8, len: usize) -> Result<(), TestCaseError> {
        let request: Vec<u8> = (0..len)
            .map(|i| u8::try_from(i).unwrap_or(0) ^ caller)
            .collect();
        let got = self.ep.post(
            &self.callers[usize::from(caller)],
            poster_of(caller),
            &request,
            u64::MAX,
            RLIMIT_INFINITY,
            &NullSink,
        );
        let want = if self.model.closed {
            Err(Errno::NotFound)
        } else if request.len() > CALL_MAX as usize {
            Err(Errno::MessageTooLarge)
        } else if self.model.unanswered(caller) >= CALL_DEPTH {
            Err(Errno::WouldBlock)
        } else {
            Ok(())
        };
        prop_assert_eq!(got.map(drop), want);
        if let Ok(tairix_kernel_ipc::CallTicket(ticket)) = got {
            let model = &mut self.model;
            prop_assert!(!model.calls.contains_key(&ticket), "a ticket is fresh");
            model.calls.insert(
                ticket,
                ModelCall {
                    caller,
                    state: CallState::Pending(request),
                    awaiter: None,
                },
            );
            let queue = model.queued.entry(caller).or_default();
            if queue.is_empty() {
                model.turn.push_back(caller);
            }
            queue.push_back(ticket);
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<(), TestCaseError> {
        use tairix_kernel_ipc::RecvCall;
        let model = &mut self.model;
        let next = model
            .turn
            .front()
            .copied()
            .and_then(|caller| Some((caller, *model.queued.get(&caller)?.front()?)));
        match (self.ep.recv_call(usize::MAX), next) {
            (RecvCall::Received(call), Some((caller, ticket))) => {
                prop_assert_eq!(call.ticket.0, ticket, "served in turn");
                prop_assert_eq!(call.sender, process(caller));
                let Some(model_call) = model.calls.get_mut(&ticket) else {
                    return Err(TestCaseError::fail("served an unmodelled call"));
                };
                let CallState::Pending(request) =
                    core::mem::replace(&mut model_call.state, CallState::InService)
                else {
                    return Err(TestCaseError::fail("served a call not queued"));
                };
                prop_assert_eq!(call.request.as_bytes(), request.as_slice());
                if let Some(queue) = model.queued.get_mut(&caller) {
                    queue.pop_front();
                    model.turn.pop_front();
                    if !queue.is_empty() {
                        model.turn.push_back(caller);
                    }
                }
                Ok(())
            }
            (RecvCall::Empty, None) => Ok(()),
            (got, want) => Err(TestCaseError::fail(format!(
                "recv {got:?} against the model's {want:?}"
            ))),
        }
    }

    fn reply(&mut self, pick: u8) -> Result<(), TestCaseError> {
        let ticket = self
            .model
            .ticket(pick, |call| call.state == CallState::InService);
        let reply = ticket.to_le_bytes()[..4].to_vec();
        let got = self
            .ep
            .reply(tairix_kernel_ipc::CallTicket(ticket), &reply, &NullSink);
        match self.model.calls.get_mut(&ticket) {
            Some(call) if call.state == CallState::InService => {
                let woken =
                    got.map_err(|err| TestCaseError::fail(format!("reply refused {err:?}")))?;
                let mut want = vec![poster_of(call.caller)];
                want.extend(call.awaiter);
                prop_assert_eq!(sorted(woken.tasks()), sorted(want.into_iter()));
                call.state = CallState::Answered(reply);
            }
            _ => prop_assert_eq!(got.map(drop), Err(Errno::NotFound)),
        }
        Ok(())
    }

    fn take(&mut self, caller: u8, pick: u8) -> Result<(), TestCaseError> {
        use tairix_kernel_ipc::ReplyOutcome;
        let ticket = self.model.ticket(pick, |call| call.caller == caller);
        let got = self.ep.take_reply(
            process(caller),
            tairix_kernel_ipc::CallTicket(ticket),
            0,
            &NullSink,
        );
        let held = self
            .model
            .calls
            .get(&ticket)
            .filter(|call| call.caller == caller)
            .map(|call| call.state.clone());
        match (got, held) {
            (ReplyOutcome::Ready(bytes), Some(CallState::Answered(want))) => {
                prop_assert_eq!(bytes.as_bytes(), want.as_slice());
                self.model.remove(ticket);
                Ok(())
            }
            (ReplyOutcome::Pending, Some(CallState::Pending(_) | CallState::InService))
            | (ReplyOutcome::Cancelled | ReplyOutcome::Unknown, None) => Ok(()),
            (got, held) => Err(TestCaseError::fail(format!(
                "take {got:?} of a call the model holds as {held:?}"
            ))),
        }
    }

    fn await_call(&mut self, caller: u8, pick: u8, thread: u8) -> Result<(), TestCaseError> {
        let ticket = self.model.ticket(pick, |call| call.caller == caller);
        let waiter = 0x900 + u64::from(caller) * 4 + u64::from(thread);
        let got = self.ep.await_by(
            process(caller),
            tairix_kernel_ipc::CallTicket(ticket),
            waiter,
        );
        match self
            .model
            .calls
            .get_mut(&ticket)
            .filter(|call| call.caller == caller)
        {
            Some(call) if !matches!(call.state, CallState::Answered(_)) => {
                if call.awaiter.is_some_and(|other| other != waiter) {
                    prop_assert_eq!(got, Err(Errno::Busy));
                } else {
                    prop_assert_eq!(got, Ok(u64::MAX));
                    call.awaiter = Some(waiter);
                }
            }
            _ => prop_assert_eq!(got, Ok(u64::MAX)),
        }
        Ok(())
    }

    fn cancel(&mut self, caller: u8, pick: u8) -> Result<(), TestCaseError> {
        let ticket = self.model.ticket(pick, |call| call.caller == caller);
        let got = self
            .ep
            .cancel_one(process(caller), tairix_kernel_ipc::CallTicket(ticket));
        let held = self
            .model
            .calls
            .get(&ticket)
            .is_some_and(|call| call.caller == caller);
        prop_assert_eq!(got.is_some(), held, "only its own call is withdrawn");
        if let (Some(call), Some(woken)) = (self.model.remove(ticket), got) {
            if !matches!(call.state, CallState::Answered(_)) {
                let named = sorted(woken.tasks());
                prop_assert!(
                    named.contains(&poster_of(call.caller)),
                    "its poster is woken"
                );
                if let Some(awaiter) = call.awaiter {
                    prop_assert!(named.contains(&awaiter), "its awaiter is woken");
                }
            }
        }
        Ok(())
    }
}

/// The call endpoint against an independent reference: a caller is refused
/// for room only at its own depth of calls waiting on the server, and its
/// uncollected answers never count; callers are served in turn, each oldest
/// first; an answer goes only to its caller and wakes its poster and the one
/// thread awaiting it; a second awaiter is refused `Busy`; a withdrawn call is
/// gone; and a destroyed endpoint cancels everything.
#[test]
fn call_endpoint_lifecycle_tracks_reference_model() {
    set_max_level(Level::Error);
    tairix_fuzzseed::prop::drive(
        "call_endpoint_lifecycle_tracks_reference_model",
        SMOKE_CASES,
        BUDGET_BATCH_CASES,
        prop::collection::vec(call_command(), 0..=48),
        move |cmds| {
            let mut run = CallRun::new();
            for cmd in &cmds {
                run.step(cmd)?;
            }
            Ok(())
        },
    );
}
