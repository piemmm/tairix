# The capability + IPC model (§19.7 Silver)

`AGENTS.md` §19.7 requires the capability-critical core to carry
machine-checked specifications in addition to unit and property tests. The
**Silver** tier asks for "a TLA+ (or equivalent) model of the capability +
IPC state machine [that] is kept in sync with the code", checked by `cargo
xtask ci` on every PR that touches the modelled subsystems.

TAIRiX supplies the *equivalent*: an in-tree, exhaustive **explicit-state
model checker**. The charter forbids leaning on external code where a clean
first-party implementation is feasible (§2.12), and §19.6 already set the
"equivalent in-tree harness" precedent over an external runner; TLA+'s TLC
checker is an external Java tool, so the model and its checker live in the
workspace instead. The verifier — not the model — is the only oracle
(§19.7): a reachable state or transition that breaks an invariant fails the
build, fail-closed.

The models and checker are in `tools/xtask/src/commands/model_check.rs`; the
command is `cargo xtask model-check` (run unconditionally inside `cargo xtask
ci` because the search is exhaustive and finishes in milliseconds). This page
is the formal narrative kept in sync with that code.

## What is modelled

The model abstracts the combined capability + IPC core that three production
crates implement together:

- `kernel/sec::TaskCapabilities` — a task's authority under `derive`,
  `delegate`, and `revoke`.
- `lib/caps::CapabilitySet` — the subset algebra those operations use.
- `kernel/ipc::Port` — the capability-checked mailbox under `send`, `recv`,
  and `destroy`, with a lane per sender: each bounded by its own depth and
  served in turn ([per-sender lanes](../../architecture/ipc.md#per-sender-lanes)).
  The model drives two senders, enough to show one sender's traffic never
  costs another its room or its turn.

Capabilities are abstracted to the bits `0..N` of a small mask (the model
uses `N = 2`, which exercises every subset relation: empty, each singleton,
and the full set). The numbers are kept small deliberately: the whole point
of a model checker is to enumerate *every* reachable state, so the abstract
universe is sized to stay finite and exhaustively searchable while still
covering each qualitative case.

## State (`TypeOK`)

```
Subset      == 0 .. (2^N - 1)          \* a capability mask
Senders     == 0 .. (SENDERS - 1)
Msg         == [ caps_ok: BOOLEAN, size_ok: BOOLEAN ]

State == [
  user_grant : Subset,                 \* fixed at derive (upper bound)
  manifest   : Subset,                 \* fixed at derive (upper bound)
  effective  : Subset,                 \* evolves under delegate / revoke
  closed     : BOOLEAN,                \* the port has been destroyed
  lanes      : [Senders -> Seq(Msg)],  \* each sender's lane, oldest first
  turn       : Seq(Senders)            \* senders with a message waiting, next first
]
```

`Msg` records the *genuine* authorisation facts that held when the port
accepted a message — independently of the policy decision — so an invariant
can catch a policy that accepted a message it should have refused.

## Initial states (`Init`)

`derive` sets the effective set to exactly `user_grant ∩ manifest` (§5.2 —
no ambient authority). Every bounds pairing is an initial state:

```
Init ==
  \E ug, mf \in Subset:
    /\ user_grant = ug
    /\ manifest   = mf
    /\ effective  = ug & mf          \* bitwise AND = set intersection
    /\ closed     = FALSE
    /\ lanes      = [s \in Senders |-> << >>]
    /\ turn       = << >>
```

## Transitions (`Next`)

```
Delegate(req) ==                      \* TaskCapabilities::delegate
  /\ IF req \subseteq effective       \* adopt only a subset; else refuse
       THEN effective' = req
       ELSE effective' = effective    \* refused: untouched
  /\ UNCHANGED << user_grant, manifest, closed, lanes, turn >>

Revoke(bit) ==                        \* TaskCapabilities::revoke
  /\ effective' = effective \ {bit}
  /\ UNCHANGED << user_grant, manifest, closed, lanes, turn >>

Send(s, sender_caps, len) ==          \* Port::send, fail-closed precedence
  LET caps_ok == REQUIRED_SEND \subseteq sender_caps
      size_ok == len <= MAX_LEN
      room    == Len(lanes[s]) < DEPTH  \* the sender's own lane, never a total
      accept  == ~closed /\ caps_ok /\ size_ok /\ room
  IN  /\ IF accept
           THEN /\ lanes' = [lanes EXCEPT ![s] = Append(@, [caps_ok |-> caps_ok,
                                                           size_ok |-> size_ok])]
                /\ turn'  = IF s \in Range(turn) THEN turn ELSE Append(turn, s)
           ELSE UNCHANGED << lanes, turn >>
      /\ UNCHANGED << user_grant, manifest, effective, closed >>

Recv ==                               \* Port::recv: the front sender's oldest
  IF turn = << >>
    THEN UNCHANGED << lanes, turn >>
    ELSE LET s == Head(turn) IN
         /\ lanes' = [lanes EXCEPT ![s] = Tail(@)]
         /\ turn'  = IF Tail(lanes[s]) = << >> THEN Tail(turn)
                     ELSE Append(Tail(turn), s)   \* more waiting: to the back
  /\ UNCHANGED << user_grant, manifest, effective, closed >>

Destroy ==                            \* Port::destroy: close and drain
  /\ closed' = TRUE
  /\ lanes'  = [s \in Senders |-> << >>]
  /\ turn'   = << >>
  /\ UNCHANGED << user_grant, manifest, effective >>

Next == \/ \E r \in Subset:        Delegate(r)
        \/ \E b \in 0 .. (N-1):    Revoke(b)
        \/ \E s \in Senders, c \in Subset, l \in 0 .. MAX_LEN+1: Send(s, c, l)
        \/ Recv
        \/ Destroy
```

The `Send` precedence — `closed → caps → size → the sender's own depth` — is
identical to `Port::send`, and the model draws `len` up to `MAX_LEN + 1` so the
oversize (`size_ok = FALSE`) branch is always reachable.

## Invariants (`Inv`)

State invariants, checked at every reachable state:

- **`no-ambient-authority`** (§4, §5.2): `effective ⊆ user_grant` and
  `effective ⊆ manifest`. A task can never hold a capability outside its
  derive-time bounds — the unforgeability guarantee.
- **`ipc-fail-closed`** (§5.4): every queued `Msg` has `caps_ok ∧ size_ok`.
  No unauthorised or oversize message is ever in the mailbox.
- **`lane-depth`**: `∀ s: Len(lanes[s]) ≤ DEPTH`.
- **`closed-port-drained`**: `closed ⇒` every lane and the turn are empty.
- **`turn-exact`**: the turn order names exactly the senders whose lane is
  non-empty, each once — so none is served twice in a round and none skipped.
- **`room-isolation`**: on an open port, a capable sender whose own lane is
  below `DEPTH` is admitted, however full every other lane is. The property a
  shared total breaks: under one, a sender that filled the port refused every
  other.

Transition invariants, checked on every reachable step:

- **`authority-monotone`**: `effective' ⊆ effective` for *every* transition.
  This single relation subsumes delegate-never-widens, revoke-only-shrinks,
  and the impossibility of any action minting authority.
- **`fail-closed-admission`**: if a message was appended to a lane, the port
  was open, that sender's lane had room, and the message was genuinely
  authorised and sized.
- **`served-in-turn`**: on an open port a message leaves only the lane whose
  turn it was, one per receive, and a sender with more waiting goes to the
  back of the order.

## The room model

A second model checks that a slot freed in a sender's lane wakes one thread
waiting for it, never the line, and that nothing is lost when that thread
leaves its slot unused — `kernel/ipc`'s `RoomWaiters`, shared by ports and
call endpoints ([per-sender lanes](../../architecture/ipc.md#per-sender-lanes)).
Three threads of one sender contend for a lane of one slot:

```
Step   == {Idle, Looking, Parked, Ready, Owes}
Record == {Waiting, Designated, Owing}

State == [
  queued  : 0 .. DEPTH,
  steps   : [Threads -> Step],
  records : [Threads -> Record \cup {None}],
  line    : Seq(Threads),            \* Waiting threads, first in line first
  woken   : [Threads -> BOOLEAN]     \* wakes delivered, not yet taken
]

HandOut ==                           \* a slot no thread holds goes to the first
  IF Unheld /\ line # << >>
    THEN /\ records' = [records EXCEPT ![Head(line)] = Designated]
         /\ woken'   = [woken EXCEPT ![Head(line)] = TRUE]
         /\ line'    = Tail(line)
    ELSE UNCHANGED << records, woken, line >>
```

A thread begins a wait, looks under the lock (recording itself `Waiting` at
the back of the line unless it has a record, and taking a slot no thread holds
as `Designated`), parks, resumes on a wake — a spurious one too — sends into
its slot, keeps it as `Owing` when its wait reported the room ready, waits
again keeping that record, or leaves from any step, a slot it held handed out.
In the kernel a thread leaves an `Owing` slot at its next call that is neither
a send nor a wait watching the port again, since any other may block, and at
its end. A receive frees a slot and hands it out.

State invariants: **`room-held`** (no more slots held than free),
**`no-slot-unheld-while-waiting`** (a free slot nobody holds never coexists
with a waiting thread), **`line-exact`**, **`parked-recorded`** (no thread
parks with no record to wake it by) and **`no-lost-wake`** (a parked thread
handed a slot has its wake pending). Transition invariant:
**`one-wake-per-slot`** (no step wakes more than one thread). Fault
injections — a receive waking every waiter, a slot let go of and handed to
nobody — prove the checker rejects each.

## Keeping the model in sync

The model is faithful only as long as the production semantics it mirrors do
not drift. When `TaskCapabilities` or `Port` changes shape:

1. Update the transition relation above and in `model_check.rs` together.
2. Re-run `cargo xtask model-check`; a divergence between the policy and the
   recorded authorisation facts surfaces as an invariant counterexample with
   a minimal action trace.

The checker also carries fault-injection tests (a widening `delegate`, a
port that skips the capability check, and lanes bounded by one shared total)
that prove it *rejects* a model that breaks an invariant — the oracle has
teeth.

## Tiers beyond Silver

- **Bronze** (done) — the `proptest` stateful models; see
  [Stateful models for the capability core](../proptest_models.md).
- **Gold** (aspirational, tracked in `PLAN.md`) — Verus contracts on the
  `lib/caps` and `kernel/sec` capability-check paths, discharged by `cargo
  xtask verify`.
