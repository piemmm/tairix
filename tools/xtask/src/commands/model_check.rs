//! `cargo xtask model-check` — the Silver model checker.
//!
//! Silver requires "a TLA+ (or equivalent) model of the
//! capability + IPC state machine that is kept in sync with the code". The
//! charter forbids trusting external code where a
//! first-party implementation is feasible, and already established the
//! "equivalent in-tree harness" precedent over an external runner. TLA+'s
//! TLC checker is an external Java tool, so the *equivalent* here is an
//! in-tree, exhaustive **explicit-state model checker**: it enumerates every
//! reachable state of a finite abstract state machine by breadth-first search
//! and verifies a set of safety invariants at every state and on every
//! transition. The verifier — not the model — is the oracle: a
//! reachable state (or transition) that violates an invariant fails the
//! command, fail-closed.
//!
//! The modelled state machine is the combined **capability + IPC** core:
//!
//! - a *subject* task whose authority evolves under `derive` /
//!   `delegate` / `revoke` (mirroring `kernel/sec::TaskCapabilities`), and
//! - a capability-checked *port* with a lane per sender under `send` / `recv`
//!   / `destroy` (mirroring `kernel/ipc::Port`): two senders, each bounded by
//!   its own depth and served in turn.
//!
//! The formal narrative — the state, the transition relation, and the
//! invariants, in TLA+-style pseudocode — lives in
//! `docs/src/security/model/capability_ipc.md` and is kept in sync with this
//! executable model and with the production code it abstracts.
//!
//! Adding a model means adding a [`NamedModel`] to [`MODELS`], never teaching
//! `ci` about it directly.

use std::collections::{HashSet, VecDeque};
use std::hash::Hash;

/// The result of exhaustively checking one model: how much of the abstract
/// state space was proven invariant-respecting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Report {
    /// Distinct reachable states explored (every one satisfies the invariants).
    pub states: usize,
    /// Transitions taken between reachable states.
    pub transitions: usize,
}

/// A counterexample: the first invariant violation the checker found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Counterexample {
    /// Human-readable description of the violated invariant.
    pub invariant: String,
    /// The shortest action trace from an initial state to the violation.
    pub trace: Vec<String>,
}

/// A finite-state machine whose entire reachable state space can be
/// enumerated and checked. This is the in-tree analogue of a TLA+ `Spec`:
/// [`initial`](Model::initial) is `Init`, [`successors`](Model::successors)
/// is `Next`, and the two invariant hooks are the `Inv` conjuncts.
pub trait Model {
    /// The abstract state. Must be hashable so the checker can dedupe the
    /// visited set, and finite so the search terminates.
    type State: Clone + Eq + Hash;

    /// Every initial state (`Init`).
    fn initial(&self) -> Vec<Self::State>;

    /// Every `(action label, successor)` reachable from `state` in one step
    /// (`Next`). The label is used only to build a readable counterexample
    /// trace.
    fn successors(&self, state: &Self::State) -> Vec<(String, Self::State)>;

    /// State invariant: must hold in every reachable state. Returns the name
    /// of the violated property, or `Ok(())`.
    fn state_invariant(&self, state: &Self::State) -> Result<(), String>;

    /// Transition invariant: a relation between a state and each of its
    /// successors that must hold for every reachable transition. The default
    /// imposes no transition constraint.
    fn transition_invariant(
        &self,
        _pre: &Self::State,
        _action: &str,
        _post: &Self::State,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Exhaustively explore `model` by breadth-first search, checking the state
/// invariant on every reachable state and the transition invariant on every
/// reachable transition.
///
/// # Errors
/// Returns a [`Counterexample`] with the shortest action trace to the first
/// violating state or transition. Fail-closed: any violation aborts.
pub fn check<M: Model>(model: &M) -> Result<Report, Counterexample> {
    let mut visited: HashSet<M::State> = HashSet::new();
    // Each queue entry carries the trace that first reached the state, so a
    // violation reports a minimal-length reproduction.
    let mut queue: VecDeque<(M::State, Vec<String>)> = VecDeque::new();
    let mut transitions = 0usize;

    for init in model.initial() {
        if let Err(name) = model.state_invariant(&init) {
            return Err(Counterexample {
                invariant: name,
                trace: vec!["<init>".to_string()],
            });
        }
        if visited.insert(init.clone()) {
            queue.push_back((init, Vec::new()));
        }
    }

    while let Some((state, trace)) = queue.pop_front() {
        for (action, next) in model.successors(&state) {
            transitions += 1;
            if let Err(name) = model.transition_invariant(&state, &action, &next) {
                let mut t = trace.clone();
                t.push(action);
                return Err(Counterexample {
                    invariant: name,
                    trace: t,
                });
            }
            if let Err(name) = model.state_invariant(&next) {
                let mut t = trace.clone();
                t.push(action);
                return Err(Counterexample {
                    invariant: name,
                    trace: t,
                });
            }
            if visited.insert(next.clone()) {
                let mut t = trace.clone();
                t.push(action);
                queue.push_back((next, t));
            }
        }
    }

    Ok(Report {
        states: visited.len(),
        transitions,
    })
}

// ---------------------------------------------------------------------------
// The capability + IPC model (the Silver subject).
// ---------------------------------------------------------------------------

/// Capability universe size: capabilities are the bits `0..NCAPS` of a `u8`
/// mask. Two is enough to exercise every subset relation (`∅`, each
/// singleton, the full set) while keeping the reachable state space small
/// enough to enumerate exhaustively.
const NCAPS: u8 = 2;
/// Every capability subset, as a `u8` mask (`0..2^NCAPS`).
const SUBSET_COUNT: u8 = 1 << NCAPS;
/// The capability a sender must hold for the port to accept its message,
/// mirroring `kernel/ipc`'s `REQUIRED_SEND_CAP`.
const REQUIRED_SEND: u8 = 0b01;
/// The port's payload-size bound: a draw `len <= MAX_LEN` is `size_ok`.
const MAX_LEN: u8 = 2;
/// The largest payload length the model draws (one past `MAX_LEN`, so the
/// oversize/`size_ok == false` branch is always reached).
const MAX_LEN_DRAWN: u8 = 3;
/// The senders the model drives: two are enough to show that one sender's
/// traffic never costs another its room or its turn.
const SENDERS: usize = 2;
/// Each sender's lane depth at the port, mirroring `Port`'s per-sender bound.
const DEPTH: usize = 2;

/// One accepted message, recording the *genuine* authorisation facts that
/// held when the port accepted it — independently of the policy decision, so
/// the invariant can catch a policy that accepted a message it should not
/// have.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct Msg {
    /// The sender held the required capability.
    caps_ok: bool,
    /// The payload was within the size bound.
    size_ok: bool,
}

/// The combined abstract state: the subject task's authority plus the port.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CapIpcState {
    /// The subject's user grant (fixed at derive; a static upper bound).
    user_grant: u8,
    /// The subject's manifest request (fixed at derive; a static upper bound).
    manifest: u8,
    /// The subject's effective set; evolves under delegate/revoke.
    effective: u8,
    /// Whether the port has been destroyed.
    closed: bool,
    /// Each sender's lane, oldest message first.
    lanes: [Vec<Msg>; SENDERS],
    /// The senders with a message waiting, the next to be served first.
    turn: Vec<usize>,
}

/// The capability + IPC model. The production instance has every fault
/// switch off; the tests flip one at a time to prove the checker rejects a
/// model that breaks an invariant (the verifier is the oracle).
pub struct CapIpcModel {
    /// Fault injection: make `delegate` *widen* the effective set (breaks
    /// delegate-never-widens). Off in production.
    widen_on_delegate: bool,
    /// Fault injection: make `send` ignore the capability check (breaks IPC
    /// fail-closed). Off in production.
    ignore_caps_on_send: bool,
    /// Fault injection: bound every lane by one total they share, as ports
    /// once did (breaks room-isolation). Off in production.
    shared_depth: bool,
}

impl CapIpcModel {
    /// The faithful model: every fault switch off.
    #[must_use]
    pub fn production() -> Self {
        Self {
            widen_on_delegate: false,
            ignore_caps_on_send: false,
            shared_depth: false,
        }
    }

    /// `send(sender, sender_caps, len)` from `state`: the fail-closed
    /// precedence of `Port::send` — closed → caps → size → the sender's own
    /// depth — and, once accepted, the sender joins the turn order.
    fn send(&self, state: &CapIpcState, sender: usize, sender_caps: u8, len: u8) -> CapIpcState {
        let caps_ok = REQUIRED_SEND & !sender_caps == 0;
        let size_ok = len <= MAX_LEN;
        let held = if self.shared_depth {
            state.lanes.iter().map(Vec::len).sum()
        } else {
            state.lanes[sender].len()
        };
        let policy_caps_ok = caps_ok || self.ignore_caps_on_send;
        let accept = !state.closed && policy_caps_ok && size_ok && held < DEPTH;
        let mut next = state.clone();
        if accept {
            next.lanes[sender].push(Msg { caps_ok, size_ok });
            if !next.turn.contains(&sender) {
                next.turn.push(sender);
            }
        }
        next
    }
}

impl Model for CapIpcModel {
    type State = CapIpcState;

    fn initial(&self) -> Vec<Self::State> {
        // `derive`: effective is exactly `user_grant ∩ manifest`. Every
        // bounds pairing is an initial state.
        let mut out = Vec::new();
        for user_grant in 0..SUBSET_COUNT {
            for manifest in 0..SUBSET_COUNT {
                out.push(CapIpcState {
                    user_grant,
                    manifest,
                    effective: user_grant & manifest,
                    closed: false,
                    lanes: [const { Vec::new() }; SENDERS],
                    turn: Vec::new(),
                });
            }
        }
        out
    }

    fn successors(&self, state: &Self::State) -> Vec<(String, Self::State)> {
        let mut out = Vec::new();

        // delegate(req): adopt `req` iff it is a subset of the current
        // effective set; otherwise the call is refused and the set is
        // untouched (mirrors `TaskCapabilities::delegate`).
        for req in 0..SUBSET_COUNT {
            let mut next = state.clone();
            let subset = req & !state.effective == 0;
            if self.widen_on_delegate {
                // Fault: union instead of the subset-gated replace.
                next.effective |= req;
            } else if subset {
                next.effective = req;
            }
            out.push((format!("delegate({req:02b})"), next));
        }

        // revoke(bit): clear one capability bit (mirrors
        // `TaskCapabilities::revoke`).
        for bit in 0..NCAPS {
            let mask = 1u8 << bit;
            let mut next = state.clone();
            next.effective &= !mask;
            out.push((format!("revoke({mask:02b})"), next));
        }

        for sender in 0..SENDERS {
            for sender_caps in 0..SUBSET_COUNT {
                for len in 0..=MAX_LEN_DRAWN {
                    out.push((
                        format!("send(sender={sender},caps={sender_caps:02b},len={len})"),
                        self.send(state, sender, sender_caps, len),
                    ));
                }
            }
        }

        // recv: take the oldest message of the sender whose turn it is; a
        // sender with more waiting goes to the back (mirrors `Port::recv`).
        {
            let mut next = state.clone();
            if let Some(&sender) = next.turn.first() {
                next.turn.remove(0);
                if !next.lanes[sender].is_empty() {
                    next.lanes[sender].remove(0);
                }
                if !next.lanes[sender].is_empty() {
                    next.turn.push(sender);
                }
            }
            out.push(("recv".to_string(), next));
        }

        // destroy: close the port and drain every lane (mirrors
        // `Port::destroy`).
        {
            let mut next = state.clone();
            next.closed = true;
            for lane in &mut next.lanes {
                lane.clear();
            }
            next.turn.clear();
            out.push(("destroy".to_string(), next));
        }

        out
    }

    fn state_invariant(&self, s: &Self::State) -> Result<(), String> {
        // No ambient authority / unforgeability: the effective set
        // never exceeds either derive-time bound.
        if s.effective & !s.user_grant != 0 || s.effective & !s.manifest != 0 {
            return Err("no-ambient-authority: effective ⊄ user_grant ∩ manifest".to_string());
        }
        // IPC fail-closed: a queued message was authorised and sized.
        if s.lanes.iter().flatten().any(|m| !m.caps_ok || !m.size_ok) {
            return Err("ipc-fail-closed: an unauthorised/oversize message was queued".to_string());
        }
        if s.lanes.iter().any(|lane| lane.len() > DEPTH) {
            return Err("lane-depth: a sender's lane exceeded the declared depth".to_string());
        }
        // A destroyed port holds no messages.
        if s.closed && (s.lanes.iter().any(|lane| !lane.is_empty()) || !s.turn.is_empty()) {
            return Err("closed-port-drained: a closed port still held messages".to_string());
        }
        // The turn order names exactly the senders with something waiting,
        // each once, so none is served twice in a round and none is skipped.
        let waiting = (0..SENDERS).filter(|&sender| !s.lanes[sender].is_empty());
        let mut turn = s.turn.clone();
        turn.sort_unstable();
        if !turn.iter().copied().eq(waiting) {
            return Err("turn-exact: the turn order and the waiting senders disagree".to_string());
        }
        // Room isolation: a capable sender below its own depth is admitted,
        // however full every other lane is.
        for sender in 0..SENDERS {
            if !s.closed && s.lanes[sender].len() < DEPTH {
                let next = self.send(s, sender, REQUIRED_SEND, 0);
                if next.lanes[sender].len() != s.lanes[sender].len() + 1 {
                    return Err(
                        "room-isolation: a sender below its depth was refused room".to_string()
                    );
                }
            }
        }
        Ok(())
    }

    fn transition_invariant(
        &self,
        pre: &Self::State,
        _action: &str,
        post: &Self::State,
    ) -> Result<(), String> {
        // Authority is monotone non-increasing across *every* transition: no
        // action may grant a capability the subject did not already hold.
        // This single relation subsumes delegate-never-widens,
        // revoke-only-shrinks, and unforgeability.
        if post.effective & !pre.effective != 0 {
            return Err("authority-monotone: a transition widened the effective set".to_string());
        }
        // Fail-closed admission: a message appended to a lane found the port
        // open, room in that sender's own lane, and the message genuinely
        // authorised.
        for sender in 0..SENDERS {
            if post.lanes[sender].len() > pre.lanes[sender].len() {
                if pre.closed {
                    return Err(
                        "fail-closed-admission: accepted a send on a closed port".to_string()
                    );
                }
                if pre.lanes[sender].len() >= DEPTH {
                    return Err("fail-closed-admission: accepted a send past its depth".to_string());
                }
                match post.lanes[sender].last() {
                    Some(m) if m.caps_ok && m.size_ok => {}
                    _ => {
                        return Err(
                            "fail-closed-admission: accepted an unauthorised/oversize send"
                                .to_string(),
                        )
                    }
                }
            }
        }
        // Served in turn: on an open port a message leaves only from the lane
        // whose turn it was, one at a time, and a sender with more waiting
        // goes to the back of the order.
        if !post.closed {
            let front = pre.turn.first().copied();
            let mut taken = 0;
            for sender in 0..SENDERS {
                let left = pre.lanes[sender]
                    .len()
                    .saturating_sub(post.lanes[sender].len());
                if left > 0 && Some(sender) != front {
                    return Err("served-in-turn: a message left out of turn".to_string());
                }
                taken += left;
            }
            if taken > 1 {
                return Err("served-in-turn: one receive took more than one message".to_string());
            }
            if let (1, Some(sender)) = (taken, front) {
                if !post.lanes[sender].is_empty() && post.turn.last() != Some(&sender) {
                    return Err(
                        "served-in-turn: a sender with more waiting kept the front".to_string()
                    );
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Command registry and entry point.
// ---------------------------------------------------------------------------

/// One named model the orchestrator knows how to check.
#[derive(Clone, Copy)]
pub struct NamedModel {
    /// Short, unique selector used by `--target`.
    pub name: &'static str,
    /// One-line description shown by `--list`.
    pub description: &'static str,
    /// Run the exhaustive check.
    pub run: fn() -> Result<Report, Counterexample>,
}

fn check_cap_ipc() -> Result<Report, Counterexample> {
    check(&CapIpcModel::production())
}

/// The closed set of Silver models, in run order.
pub const MODELS: &[NamedModel] = &[NamedModel {
    name: "cap-ipc",
    description: "capability + IPC state machine (derive/delegate/revoke + port send/recv/destroy)",
    run: check_cap_ipc,
}];

/// Parsed `model-check` invocation.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct Options {
    /// Optional model filter (`--target <name>`); checks all when `None`.
    pub only: Option<String>,
    /// `--list`: print the registry and exit without checking anything.
    pub list: bool,
}

/// Parse `model-check` arguments.
///
/// # Errors
/// Returns a usage error for an unknown flag or a missing `--target` value.
pub fn parse(args: &[std::ffi::OsString]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--list") => opts.list = true,
            Some("--target") => {
                let name = iter
                    .next()
                    .ok_or_else(|| "model-check: `--target` requires a model name".to_string())?;
                let name = name.to_str().ok_or_else(|| {
                    "model-check: `--target` value is not valid UTF-8".to_string()
                })?;
                opts.only = Some(name.to_string());
            }
            _ => {
                return Err(format!(
                    "model-check: unexpected argument {}; usage: \
                     cargo xtask model-check [--target NAME] [--list]",
                    arg.display()
                ));
            }
        }
    }
    Ok(opts)
}

/// Resolve the models an [`Options`] selects, preserving registry order.
///
/// # Errors
/// Returns an error if `--target` names a model that is not registered.
pub fn selected(opts: &Options) -> Result<Vec<NamedModel>, String> {
    let Some(name) = opts.only.as_deref() else {
        return Ok(MODELS.to_vec());
    };
    match MODELS.iter().find(|m| m.name == name) {
        Some(m) => Ok(vec![*m]),
        None => Err(format!(
            "model-check: unknown model `{name}`; known models: {}",
            MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Run the selected models, failing closed on the first counterexample.
///
/// # Errors
/// An unknown model, a `--list` stdout cannot take, or any model that finds a
/// state or transition violating an invariant.
pub fn run(opts: &Options) -> Result<(), String> {
    if opts.list {
        for m in MODELS {
            outln!("{:<10} {}", m.name, m.description)?;
        }
        return Ok(());
    }

    for m in &selected(opts)? {
        match (m.run)() {
            Ok(report) => {
                noteln!(
                    "xtask: [model-check] {}: {} states, {} transitions — all invariants hold",
                    m.name,
                    report.states,
                    report.transitions
                );
            }
            Err(cx) => {
                return Err(format!(
                    "model-check: model `{}` violated `{}`\n  trace: {}",
                    m.name,
                    cx.invariant,
                    cx.trace.join(" -> ")
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn production_model_holds_every_invariant() {
        let report = check(&CapIpcModel::production()).expect("no counterexample");
        // The search must actually explore a non-trivial state space.
        assert!(report.states > 50, "explored only {} states", report.states);
        assert!(report.transitions > report.states);
    }

    #[test]
    fn checker_catches_a_widening_delegate() {
        let broken = CapIpcModel {
            widen_on_delegate: true,
            ..CapIpcModel::production()
        };
        let cx = check(&broken).expect_err("widening delegate must be rejected");
        assert!(
            cx.invariant.starts_with("authority-monotone")
                || cx.invariant.starts_with("no-ambient-authority"),
            "unexpected invariant: {}",
            cx.invariant
        );
        assert!(!cx.trace.is_empty());
    }

    #[test]
    fn checker_catches_a_leaky_port() {
        let broken = CapIpcModel {
            ignore_caps_on_send: true,
            ..CapIpcModel::production()
        };
        let cx = check(&broken).expect_err("a port that skips the cap check must be rejected");
        assert!(
            cx.invariant.starts_with("ipc-fail-closed")
                || cx.invariant.starts_with("fail-closed-admission"),
            "unexpected invariant: {}",
            cx.invariant
        );
    }

    /// One total the lanes share is the lock-out the per-sender bound removed:
    /// a sender that fills the port refuses every other.
    #[test]
    fn checker_catches_a_port_whose_senders_share_one_depth() {
        let broken = CapIpcModel {
            shared_depth: true,
            ..CapIpcModel::production()
        };
        let cx = check(&broken).expect_err("a shared depth must be rejected");
        assert!(
            cx.invariant.starts_with("room-isolation"),
            "unexpected invariant: {}",
            cx.invariant
        );
        assert!(!cx.trace.is_empty());
    }

    #[test]
    fn run_checks_the_production_model() {
        run(&Options::default()).expect("production model-check passes");
    }

    #[test]
    fn parse_defaults_to_all_models() {
        let opts = parse(&argv(&[])).expect("empty args parse");
        assert!(opts.only.is_none());
        assert!(!opts.list);
        assert_eq!(selected(&opts).expect("all").len(), MODELS.len());
    }

    #[test]
    fn parse_list_flag() {
        let opts = parse(&argv(&["--list"])).expect("list parses");
        assert!(opts.list);
    }

    #[test]
    fn target_selects_one_known_model() {
        let opts = parse(&argv(&["--target", "cap-ipc"])).expect("target parses");
        let chosen = selected(&opts).expect("known model");
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].name, "cap-ipc");
    }

    #[test]
    fn unknown_target_fails_closed() {
        let opts = parse(&argv(&["--target", "nope"])).expect("flag parses");
        assert!(selected(&opts).is_err());
    }

    #[test]
    fn target_requires_a_value() {
        assert!(parse(&argv(&["--target"])).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse(&argv(&["--bogus"])).is_err());
    }

    #[test]
    fn every_registered_model_has_a_unique_name() {
        let mut names: Vec<&str> = MODELS.iter().map(|m| m.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate model name in registry");
    }
}
