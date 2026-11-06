# Kernel IPC subsystem

`kernel/ipc` is the in-kernel home of the capability-checked primitives
every higher-level kernel and userland component uses to talk to
another address space:

1. **Message ports** — typed endpoints declared via `lib/abi`. Each port
   carries a `required_send_caps`, a `required_recv_caps`, and a
   `max_payload` cap. The kernel enforces the send-side capability set
   on **every** send; the receiver does not re-check (`AGENTS.md` §5.2
   final bullet).
2. **Shared-memory objects** — explicit, capability-gated allocations
   tracked through `kernel/mem`. Revocation tears down every live
   mapping atomically.
3. **Asynchronous notifications** — signal-like bitfields with the
   same capability gates as ports.

Per the Stage 2.5 brief, the crate depends only on `kernel/{sync, mem,
sched, sec}` and `lib/{abi, caps, log, util}`. The Stage 2.7 syscall
dispatcher is out of scope here and consumes this crate's public API.

## Payload confidentiality

Every kernel-owned payload copy — a `Port` message, a
[`SensitiveBuffer`](./memory.md#4-sensitive-region-api), and a `CallEndpoint`
call's room, a `SensitiveRoom` holding its request and then its reply — is
zeroed the moment the kernel releases it, whichever way the transfer ends:
delivered and drained, refused for capability, size, budget or depth, withdrawn
by its poster, reaped on a deadline, or cancelled by teardown. A call's room is
wiped as well when the server hands it back, so no request byte is left beside
the reply. The staging buffers the `ipc_send` / `ipc_call` / `call_post`
handlers copy user bytes into are `SensitiveBuffer`s, and `call_reply` copies
the answer straight into the call's room, so no stage of the path holds
plaintext in a plain allocation.

This is unconditional rather than a per-endpoint property. The kernel heap
is shared across every principal and `lib/kalloc` does not zero on free, so
an un-wiped release leaves the bytes readable by whatever allocates the
block next; and the endpoints that carry a secret are not knowable at bind
time — the session and elevation exchanges carry a passphrase, the app-data
vault carries an application's sealed secrets, and a delegation carries a
capability token. An opt-in "this endpoint carries secrets" bit would be
open-by-default for every endpoint whose author did not anticipate one.

The cost is one write pass over the bytes a payload held — a call's room wipes
up to its high-water mark, never its whole capacity — which the path already
copies at least twice, and every payload is allocated outside the endpoint's
spinlock. A post
whose room cannot be allocated is refused `Errno::OutOfMemory` to its own
caller, rather than aborting on a failed `Vec` growth, so no server is handed
a call without room for its answer.

`kernel/ipc/src/payload_wipe_tests.rs` holds the regression cover: a
test-only global allocator scans every released block for the payload
sentinel, and one of its cases leaks a payload deliberately so the scan
cannot pass vacuously.

## Per-sender lanes

Ports and call endpoints are reached by many senders at once, so each keeps
what every sending process has outstanding in a lane of its own
(`kernel/ipc/src/lanes.rs`):

- **A depth per sender, not a total.** The owner declares how many messages
  one sender may have queued, or calls one caller may have waiting on the
  server — posted or in service — the `per_sender` bound of `port_bind` and
  `call_create`, at most `IPC_SENDER_DEPTH_MAX`. A sender at its depth is
  refused `WouldBlock`, whatever slot a thread of it is recorded as holding,
  and nothing another sender holds counts against it, so no sender can make an
  endpoint refuse another. A call's room comes back when
  the server answers it: an answer the caller has not collected is held in its
  lane, at its expense, and never holds that room, but a caller holding
  `IPC_SENDER_DEPTH_MAX` answers it has not collected is refused
  `LimitExceeded`, at once and blocking or not, until it holds fewer: the
  calls it posted below that may all be answered, taking it up to its depth
  past it, so collecting one is not always enough. No search of a lane grows
  with the caller's budget.
- **The sender pays.** A lane is charged to its sender's `ipc-queue-bytes`
  limit for all the storage it holds, as it grows — growth past the limit is
  refused like the traffic itself — until it retires; a queued message for its
  payload, never less than its own record, and an outstanding call for the
  larger of its endpoint's two payload bounds, until taken or retired. The owner pays for the
  endpoint's
  bookkeeping, the few idle lanes it keeps for reuse, and — for a port — one
  full lane for senders that have ended (below).
- **Served in turn.** The owner takes one item from each sender with something
  waiting before it takes a second from any, each sender's own items oldest
  first: a sender's earlier request is always served before its later one, and
  a deep backlog waits behind one item of everyone else's.
- **Nothing grows with the number of senders.** Lanes are found by hash and
  the turn order is a list threaded through them, so opening, serving and
  retiring a lane cost the same at one sender as at thousands; a call in
  service is found by its ticket. Only a send or post grows anything, fallibly,
  reserving all the room its item will later need, so receive, reply,
  collection and teardown never allocate and cannot fail for memory. What
  leaves an endpoint is dropped after its lock is released.
- **Room is per sender, and a freed slot wakes one thread.** A sender parked
  for room — a `PortRoom` wait-set member, or a blocking `ipc_call` at its
  depth — waits as its own process, in its process's line, kept under the lock
  that frees the room, so freeing a slot and handing it on are one critical
  section (`kernel/ipc/src/lanes.rs`, `RoomWaiters`). A slot that frees goes
  to the first thread in line, which holds it — no sibling's send takes it —
  until it sends or leaves; one that leaves it unused, killed or refused,
  hands it to the next, and a thread waiting again keeps its place. No change
  to one sender's lane wakes another's waiters, and a receive or reply with
  nobody waiting looks at an empty map under the lock it already holds. At
  every release of that lock each free slot is held by one thread or waited
  for by none, and a thread parks only after looking and finding none it may
  take, so no wake is lost. At a port, the member a wait reports ready keeps
  its thread's slot, and any budget release the wait was woken for, until the
  thread's next send there, its next system call that is neither a send nor a
  wait watching the port again, or its end — the `EPOLLEXCLUSIVE` contract:
  only a thread running between its wait and its send holds a slot, and only
  from its own siblings, since any other call may block. The debt is kept on
  the thread's own gate, one at a time, so every system call and user fault
  learns its thread owes nothing with one load and no lock; a send that used
  the slot settles it as the send returns, and a wait the kernel refuses
  before it scans leaves it as it was. At a port the room is
  budget as well as depth: a `PortRoom` member waits while all the sender
  holds against its `ipc-queue-bytes` limit, wherever it is held, leaves too
  little for the port's largest message and any lane the send opens or grows.
  That budget frees wherever the sender's charges are released, so a wait a
  scan finds short of it also waits under its process on the kernel's budget
  queue — registered, then scanned again, before it parks. A receive of one
  of the process's messages, and the collection, withdrawal or timeout of one
  of its calls, wake one thread waiting under that process's key — the oldest
  no release has chosen, found in O(log n) — which passes the wake on should
  it wait on or leave without sending; one whose wait ended on the room keeps
  it, and passes it on once it has sent or at that next call, since what freed
  may be room for more. A change to the process's limit wakes every waiter
  under its key, as a call endpoint's destruction does for each caller whose
  calls it cancelled, and a port's teardown, which discards many senders'
  messages at once, wakes every budget waiter. The queue counts its waiters,
  so a release costs a
  fence and one load while nobody waits; the waiter's `SeqCst` fence between
  registering and looking again, and the releaser's between releasing and
  reading the count, mean one always sees the other.

Port lanes are keyed by the sending process *instance*, because a queued
message outlives its sender and a recycled process number must not inherit a
dead sender's lane; call lanes by process number, because a dead caller's
calls are cancelled before its number is reused.

A message outlives its sender at its receiver's expense, up to what the
receiver set aside: each port's owner pays at bind for one full lane of its
largest messages (`Port::ended_room`), and when a sender ends, what it left
queued at each port is held against that room while it fits and discarded
whole when it does not (`PORT_ENDED_SENDER_DISCARDED`). A sender that sends and
exits therefore has its last messages delivered, while senders that end in
numbers cannot make a port hold more than its owner chose.

## Message ports

```text
+--------+   send(payload)    +------------------+   recv()    +----------+
| sender |  -------------->   |      Port        |  -------->  | receiver |
+--------+   caps checked     | required_send,   |             +----------+
                              | required_recv,   |
                              | max_payload,     |
                              | a lane per sender|
                              +------------------+
```

`Port::create` performs the bind-time check (creator must hold every
capability in `required_recv_caps`; restricting senders additionally
requires `CAP_IPC_BIND_PRIVILEGED`). `Port::send` is the per-call
fast path:

1. **Lock-free closed-state check.** An `AtomicU32` is consulted before
   the mailbox lock; sends to destroyed ports return `Errno::NotFound`
   without acquiring the lock. `kernel/ipc/tests/loom.rs` models this
   state machine against `destroy` over every interleaving
   (`cargo xtask loom`).
2. **Capability check** against the sender's `TaskCapabilities`.
3. **Size check** against the per-port `max_payload`, additionally
   bounded by the global ABI cap `IPC_MESSAGE_MAX_PAYLOAD_LEN` (1 MiB
   in `abi-v1`), and against the sender's own `ipc-queue-bytes` limit:
   a message that, with the lane it opens, would pass that limit even
   with the sender holding nothing is one no release could admit.
   Either way the send returns `Errno::MessageTooLarge` (semantically
   `EMSGSIZE`), never the retryable `LimitExceeded`, so a sender never
   waits for room it can never have.
4. **Charge** of the message to the sender's own `ipc-queue-bytes` limit,
   then the **admission check** (below), then the **depth check** against
   the sender's own lane.

Every rejection emits exactly one audit event before returning.
`Port::destroy` drains in-flight messages and is idempotent.

The receive side offers two shapes. `Port::recv` takes the next message in
turn unconditionally. `Port::recv_with(f)` is a **peek/commit** variant for
receivers that must move the payload across a fallible boundary before they
can accept it: it holds the mailbox lock while it runs `f` against the next
message and dequeues it **only** when `f` returns `Ok`, handing the message
back so its payload is wiped and freed outside the lock, and beside it the
thread of its sender the slot it freed was handed to, for the receiver to
wake there too; `recv` hands back both as well. The wake never rides in the
queued message, whose own record is the least any message is charged. A
failure — for
example a faulting `copy_to_user` when the kernel delivers the payload into
the receiver's address space, or a receive buffer too small to hold it —
leaves the message at the head of its sender's lane, the sender keeping its
turn, to be re-delivered rather than dropping it on the floor (`AGENTS.md`
§5.4, fail closed). Like `recv` it performs no capability
check of its own; the **syscall layer** gates every receive — the
`ipc_recv` handler checks the caller against the port's
`required_recv_caps` *before* any message is observed or dequeued (the
same handler-side receive gate `call_recv` applies to a call endpoint),
refusing an under-capable caller with `Errno::PermissionDenied` while the
message stays queued and nothing about the mailbox — not even whether it
is empty — is revealed. Bind-time proof alone is not enough: the binder
holding the capabilities says nothing about who later names the endpoint
id in a receive. The `ipc_recv` syscall is built on this pair of checks
(see the syscall page).

## Named-port registry

A `Port` on its own is anonymous: `Port::create` proves bind authority
and returns an owned value, but a sender or receiver still needs to
reach it by the `EndpointId` carried in an `IpcMessageHeader`.
`PortRegistry` (`kernel/ipc/src/registry.rs`) is that map from
`EndpointId` to the live kernel-owned `Port`.

Like `kernel/sec`'s `CapTable`, the registry exposes a plain `&self` /
`&mut self` surface. `kernel/core`'s `KernelState` composes it with the
scheduler and capability table under one lock-ordering policy (`AGENTS.md`
§2.1 — no global mutable static). Lookups borrow `&self` so concurrent
senders share a read guard while each `Port::send` re-checks the per-send
capability. Registration notes each port as its owner's, so an exit tears
down the ports its process bound and reads no other port's owner. The one
lock the registry owns guards its index of the ports each sender instance
uses (`kernel/ipc/src/senders.rs`, the index the call registry keeps too),
which a port updates under the read guard the first time an instance sends
to it or waits for room at it, so an instance's end visits those ports and
no other.

* `register(port)` binds the port under its own `id`, fail-closed: a
  duplicate `EndpointId` is refused with `Errno::AlreadyExists`, and a
  registration the registry cannot hold, or note in its indexes — the sender
  index, made with the first registration, and its owner's ports — with
  `Errno::OutOfMemory` (`PORT_REGISTER_DENIED`); the supplied port is
  handed back whole, for the caller to drop once it has released the
  registry — a live binding is never overwritten. Success emits
  `PORT_REGISTERED`.
* `lookup(id)` / `contains(id)` resolve a binding; a miss is `None`
  (mapped to `Errno::NotFound` at the syscall boundary) and is not a
  security decision, so it is not audited.
* `unregister(id)` removes and `Port::destroy`s the binding (draining
  in-flight messages, emitting `PORT_DESTROYED`) and then emits
  `PORT_UNREGISTERED`; an unknown endpoint is `Errno::NotFound`.

The registry performs no capability check of its own — bind authority
was proven at `Port::create` time and send authority is re-checked on
every `Port::send` — so it is a pure ownership map. It is now composed
into `kernel/core`'s `KernelState` as `ipc: RwLock<PortRegistry>`
(mirroring `caps: RwLock<CapTable>`) and borrowed by the
`KernelDispatchHook`, so the `ipc_send` / `ipc_recv` handlers resolve
the syscall's endpoint against the live map: an unbound endpoint fails
closed with `Errno::NotFound`. A bound `ipc_send` is fully wired — it
copies the payload in through the kernel's `copy_from_user` boundary
([`tairix_kernel_mem::copy_in`](./memory.md#3a-user-memory-copy-uaccess))
and hands it to `Port::send`, returning `Errno::BadAddress` (the TAIRiX
`EFAULT`) for a faulting buffer or a caller with no registered address
space. A bound `ipc_recv` is now wired too: it copies the head message
out through the kernel's `copy_to_user` boundary
([`tairix_kernel_mem::copy_out`](./memory.md#3a-user-memory-copy-uaccess))
using `Port::recv_with` (below), returning `Errno::WouldBlock` (the
TAIRiX `EAGAIN`) when the bound mailbox is momentarily empty and
`Errno::BadAddress` for a faulting buffer. See
[the syscall handler-wiring table](./syscalls.md#handler-wiring-stage-27-follow-up-f3).

A port takes messages from any capable sender and its owner authenticates
each by its attested origin, which keeps a forged message from being believed
but not from being queued: another sender's lane can never take the room of
the one sender a port serves, but its messages still cost the owner a drain
and a discard. `port_admit` lets the owner name that sender, by the call
endpoint it serves — the kernel resolves the instance serving it now, so the
identity is attested rather than the owner's word. From then on `Port::send`
refuses any other process instance with `PermissionDenied`, checked under the
mailbox lock the send already takes, so a refused message is never queued and
an unrestricted port pays nothing. The admission itself discards every other
sender's lane — taken out under the same lock and wiped and freed after it,
the room the admitted lane is kept in made beforehand — so the owner never has
to drain a port to trust it, and wakes the senders parked for room in them,
who now find their sends refused. Only the first refusal under an admission is recorded; the rest
are counted into the port's destruction record, so a sender the port will
never take cannot flood the audit trail through it.

A port, like a call endpoint, charges its owner's `ipc-queue-bytes` limit for
its bookkeeping when it is created, and each queued message to its sender
([per-sender lanes](#per-sender-lanes)); every charge travels with its object,
so destroying it by any path returns it
([resource limits](./resource-limits.md)). `port_unbind` lets the owner tear a
port down before it exits, which is what a program opening a transport per
operation needs: `tairix_rt::PrivatePort` unbinds its port when dropped.

### Well-known names

A numeric `EndpointId` is an opaque handle a binder must already know.
So that a process can reach a *well-known* endpoint — a long-running
system service's rendezvous — by a stable name instead, the registry
keeps a second index from `PortName` (`lib/abi`, §9) to `EndpointId`.
(Desktop input is deliberately **not** a named port: a port's receive
gate is capability-only, so it cannot express "only the live seat-lease
holder may drain"; the pointer/keyboard streams flow through the seat
registry's owner-gated channels instead — see
[the seat page](../desktop/seat.md).)

* `publish_name(name, id)` binds a validated `PortName` to a
  currently-registered endpoint. It fails closed: a name already in use
  is `Errno::AlreadyExists`, an endpoint that is not registered is
  `Errno::NotFound`, and a name the index has no room for is
  `Errno::OutOfMemory` (all `PORT_NAME_PUBLISH_DENIED`), so a name can
  never resolve to a non-existent port and a live name is never silently
  re-pointed. Success emits `PORT_NAME_PUBLISHED`.
* `resolve(name)` / `resolve_port(name)` map a name back to its
  `EndpointId` (or directly to the live `Port`); a miss is `None` and is
  not audited, mirroring `lookup`.
* `withdraw_name(name)` removes a single name binding, leaving the
  endpoint registered, and emits `PORT_NAME_WITHDRAWN`; an unbound name
  is `Errno::NotFound`.

The index only ever points at a live binding: `unregister(id)` withdraws
every name that resolved to `id` (one `PORT_NAME_WITHDRAWN` each) before
destroying the port, so a resolution can never dangle. A name grants no
authority of its own; the per-send capability check is unchanged.

User space reaches the index through the `port_resolve` syscall
(`SyscallNumber::PORT_RESOLVE`): the kernel bounds the supplied length
against `PORT_NAME_MAX_LEN` before touching user memory, copies the name
bytes in through the validated `copy_from_user` boundary, validates them
with `PortName::from_ascii`, and resolves them against the live registry
— returning the bound `EndpointId` for `ipc_send` / `ipc_recv`, or
`Errno::NotFound` for an unpublished name. Like the other pure observers
(`cap_query`, `clock_get`) it is unprivileged and unaudited: resolving a
name grants nothing, publication is a kernel-side bind-authority-checked
operation, and every send to the resolved endpoint is still
capability-checked at the port. The aarch64 driver-spawn QEMU vertical
exercises the whole path live: the test kernel publishes its reply
endpoint under a well-known name and the spawned stub resolves it over
the syscall before replying.

A `PortName` is a non-empty, ≤ 31-byte ASCII string that starts with a
lowercase letter and continues with lowercase letters, digits, `'.'`, or
`'_'`, with no trailing `'.'` and no `".."`. The constrained alphabet
keeps a name canonical, log-printable, and free of separators a routing
layer might re-interpret; its `from_ascii` / `from_bytes` decoders reject
anything else and are exercised by the `lib/abi` fuzz harness (§19.6).

## Shared memory

`SharedMemory::create` allocates a kernel-tracked, zero-on-free
`SensitiveBuffer` (so any credential or capability-token bytes that
ever transited the region are wiped on revocation). `SharedMemory::map`
gates each mapping on the recipient's capabilities; the returned
`ShmemMapping` holds the buffer alive through an `Arc<Inner>` and
exposes `with_bytes` / `with_bytes_mut` accessors that yield `None`
once `SharedMemory::revoke` has run. Revocation thus invalidates every
live mapping atomically and the racing-mapper integration test in
`kernel/ipc/tests/integration.rs` confirms there is no torn-buffer
window between the two.

## Notifications

`NotificationChannel` is a lossless OR-accumulating signal: senders
raise one or more bits with `signal(flags)` and the bound receiver
takes-and-clears the pending set with `take_pending()` (one atomic
swap). The same bind-/send-time capability split as ports applies.

## Synchronous call/reply endpoints

A `Port` is fire-and-forget; the reactive driver-store file service
(Design D — `/System` file-read IPC service) and any future request/reply
system service need *synchronous* semantics instead. `CallEndpoint` is
that primitive: a caller `post`s a request and receives an opaque,
unforgeable `CallTicket`; the single bound server takes the next request in
turn with `recv_call` (moving it to an in-service table keyed by its ticket)
and answers with `reply(ticket, …)`; the caller claims the answer with
`take_reply(claimant, ticket)`. The bind-/send-time capability split and
size/closed-port checks mirror `Port` exactly; `create` takes its bounds as
one `CallEndpointLimits` value (`max_request`, `max_reply`, `per_sender`),
where `per_sender` is a fail-closed bound on the calls one caller may have
waiting on the server (`AGENTS.md` §24.4), not a scaling capacity, bounded by
`IPC_SENDER_DEPTH_MAX` because it reaches `create` straight from the syscall's
argument.

A caller's calls live in its lane ([per-sender lanes](#per-sender-lanes)) —
queued, in service, and answered — and its own operations search that lane
alone; the server's reply finds the caller of a call in service through an
index keyed by ticket, so the server's cost never grows with how many callers
it has or how many answers any of them has left uncollected. Every removed
payload is moved out and dropped *after* the endpoint lock is released, so the
receive and reply paths never descend into the kernel heap's own global,
IRQ-masking lock while holding this endpoint's lock.

A blocking `ipc_call` whose caller is at its depth does not fail: it parks until
the server answers one of its process's calls, or one times out or is
withdrawn, each of which hands the slot it frees to one thread waiting as that
process, and then posts into it. A woken thread looks for its slot before it
posts again, so one woken without one parks again, keeping its place, without
paying for a refused post. Its own uncollected answers never hold room, so the
wait is on the server and never on the caller. A posted call (`call_post`) is
answered `WouldBlock` instead, and its caller posts again once the server has
answered one of its own calls.

A call is given one room as it is posted (`CallPayload`), as large as the
larger of its request and the most of an answer its caller can take, never
more than `max_reply` (`post_within`: `ipc_call` passes its reply buffer's
length, and an in-kernel client or a posted call, which names its buffer only
when it collects, takes the endpoint's bound). The room is a `SensitiveRoom`,
reserved whole, initialised only where it is written and wiped on drop up to
its high-water mark, so a call costs writes in proportion to what it carried
however large its endpoint's bound; its charge pays for the bound. The request
reaches the server in it; the receiving side hands it back wiped once it has
copied the request out (`keep_room` — `call_recv` does so before it writes the
ticket out, so a sibling server thread answering at once finds the room, and
an in-kernel server answers through `reply_with` instead). The answer is
written into it with the endpoint's lock released (`reply`, and `reply_from`,
through which `call_reply` copies the server's buffer straight in) and
published under it, the call checked again as still in service: one withdrawn,
timed out or torn down while its answer is written is refused `NotFound` and
its room dropped. An answer longer than its caller can take completes the call
holding nothing, and the caller is answered `BufferTooSmall`. A reply into a
kept room therefore needs no memory, and is never refused for want of it. A
post the kernel has no memory for is refused to its own caller, so no server is
handed a call without room for its answer. A call whose room was not handed
back is answered in one made for it, the only way a reply can need memory, and
so the only way one can be refused `OutOfMemory`.

An answer wakes the thread awaiting the call (`call_await`) and its poster,
which may be watching for it another way; a call is awaited by one thread at a
time, so a second thread awaiting the same call is refused `Busy` rather than
left asleep when the first takes the answer. A call withdrawn with
`call_cancel` wakes its awaiter, which then finds it gone.

An awaited answer is its awaiting thread's alone: `call_await`'s, and a
blocking `ipc_call`'s, whose poster awaits it in the call (`Collector::Poster`).
It wakes no thread watching for its caller's answers and a `CallReply` member
never reports it, nor its deadline, so a loop parked beside a worker making
blocking calls on the same endpoint sleeps through them rather than waking to
nothing it may take and finding the member still ready. An await that ends
without the answer — the process being killed, a park refused — hands the
call back to the reapers (`CallEndpoint::abandon_await`), waking the watchers
if the answer is in already.

`CallEndpoint` is the request/reply *state machine* only and never
blocks — the caller parking until its ticket is replied and the server
parking until a request arrives are layered above through the same
cooperative yield/park seam the IRQ-wait and `wait` syscalls use
(`kernel/core`), so the primitive stays scheduler-free and host-testable
(`AGENTS.md` §2.2 / §17.4). A caller no scheduler can park is answered
`NotImplemented` rather than left to spin, and withdraws any call it posted.
Two security properties beyond `Port`:
`take_reply` takes the claiming process and searches that process's lane
alone, so a reply is claimable only by the process that posted it and
another's ticket reads as `Unknown`, never revealed (`AGENTS.md` §19.1); and `destroy`
cancels every in-flight ticket so a parked caller observes `Cancelled`
and abandons rather than waiting forever (fail closed, `AGENTS.md` §2.9).

A caller's calls die with it. When a task exits (cleanly, by fault, or
killed — e.g. a class driver unloaded by `devmgr`), the kernel's task
reclamation cancels every call that task still has in flight
(`cancel_posted_by`): queued requests are dropped before any server
receives them, in-service tickets are retired so the server's later
`reply` is refused fail-closed, and unclaimed replies are discarded.
Without this a dead caller's queued request would be handed to the server
as if live — serviced on a ticket whose reply goes nowhere and, on a
single-slot protocol such as the USB URB transport, wedging the endpoint
against the caller's replacement (the observed Pi 4 keyboard-recovery
defect). Each affected endpoint records one `CALL_POSTER_VANISHED` (3051)
event.

An exit visits only what its process used. Every endpoint bound notes its
callers in the call registry's sender index (`kernel/ipc/src/senders.rs`)
the first time each one's lane opens, and remembers under its own lock whom
it has noted, so a lane that retires with every collected answer and opens
again with the next call takes no other lock. A note goes when its caller
ends or its endpoint is destroyed, and a process number drawn again after
its teardown is noted afresh. The registry also indexes the endpoints each
process owns, so a server's exit tears down its own and reads no other's
owner; an endpoint id another party has unbound and bound again since is no
longer the dying owner's, and its teardown leaves it alone. Destroying an endpoint hands back the threads it leaves waiting —
the poster and awaiter of every call it cancelled unanswered, every
thread parked for room there, and every thread a wait-set records watching
for a caller's answers — and the teardown wakes those, never every caller
parked on the machine, and, once the cancelled calls' charges are back, every
thread waiting for the budget of a caller whose calls it cancelled. A wait-set
with a `CallReply` member resolves its endpoint once per wait and records its
thread there before every scan, a record made and forgotten at one cost
however many watch. An awaited answer wakes no watcher; of the others, one
whose poster watches for it wakes that poster alone, and one it does not
wakes every thread watching for its caller's answers, since a thread other
than the one that posted may collect it. A destruction forgets the callers its
endpoint noted under one acquisition of the index's lock. An endpoint the kernel
takes down under a live server, as a detached network volume's is
(`callreg::take_down`), wakes the server too, and the server's `Endpoint`
wait-set member reports ready, so its next receive answers `NotFound`
rather than leave it parked on an endpoint that is gone.

The locks order registry, endpoint, index, heap. An endpoint notes a caller
under its own lock, and nothing but the heap is taken under the index's. An
owner's teardown takes the address-space registry's write lock before the call
registry's, so the grants it revokes name exactly the endpoints the owner
holds. A
caller's end takes its endpoints from the index, and an endpoint's
destruction forgets its callers there, with neither the registry's nor an
endpoint's lock held; a lookup holds the registry's lock only to clone the
endpoint out.

Because a queued call can vanish this way, a wait-set's endpoint
readiness peek is a hint, not a guarantee. The `call_recv` syscall
therefore takes a `CallRecvFlags` word: `0` blocks until a request
arrives (the dedicated-server mode `sysinfod`/`journald`/`seatmgr` use),
while `NON_BLOCKING` answers an empty queue with `WouldBlock` instead of
parking — the mode every wait-set-driven event loop uses so a loop
serving several sources can never park on one of them. Reserved flag
bits are rejected fail-closed. `tairix_rt::call_recv_ready` is that take
for a woken loop: it answers the call, or `None` for one withdrawn since the
wake, which is simply nothing to serve.

A client that must not wait on a wedged server forever posts with a
deadline. `tairix_rt::call_within` is that call for a client with nothing
else to do meanwhile: it posts and awaits that one call
(`tairix_rt::call_await`, the `call_await` syscall), answering `TimedOut` once
the deadline passes unanswered. The await parks on the call itself rather than
on a wait-set's reply member, which is ready for any of the process's replies
on the endpoint that no thread awaits, so a client with several calls
outstanding there is never woken for another's answer. A timed-out call says
nothing of what the server
did: one still queued at its deadline was never received, while one in
service may have been carried out and its reply refused.

## Audit catalogue

Audit events live in the `kernel/ipc` reserved range `3_000..4_000`
(see `lib/log` event-id conventions; `kernel/sec` owns `1_000..2_000`,
`kernel/mem` owns `2_000..3_000`).

| ID   | Level | Name                          | When |
|-----:|-------|-------------------------------|------|
| 3000 | Info  | `PORT_CREATED`                | A capability-checked port was created. |
| 3001 | Error | `PORT_CREATE_DENIED`          | A port-creation request was refused. |
| 3002 | Info  | `PORT_DESTROYED`              | A port was destroyed. `drained` counts the messages discarded with it, `refused` the sends refused over its life because its owner admitted another sender. |
| 3003 | Info  | `PORT_REGISTERED`             | A port was bound into the named-port registry. |
| 3004 | Error | `PORT_REGISTER_DENIED`        | A registration was refused (the `EndpointId` was already bound, or the registry could not hold the binding or note the port in its indexes). |
| 3005 | Info  | `PORT_UNREGISTERED`           | A port was removed from the registry and destroyed. |
| 3006 | Info  | `PORT_NAME_PUBLISHED`         | A well-known name was bound to an endpoint. |
| 3007 | Error | `PORT_NAME_PUBLISH_DENIED`    | A name binding was refused (name already bound, its endpoint not registered, or no room in the name index). |
| 3008 | Info  | `PORT_NAME_WITHDRAWN`         | A well-known name binding was removed (explicitly, or because its endpoint was unregistered). |
| 3009 | Info  | `PORT_SENDER_ADMITTED`        | A port's owner admitted the one sender its port takes (`port_admit`); `discarded` counts the queued messages of other senders dropped with it. |
| 3010 | Debug | `MESSAGE_DELIVERED`           | A message was enqueued for delivery. |
| 3011 | Error | `MESSAGE_SEND_DENIED`         | A send was refused. `reason` discriminates: `missing_capability` (the sender lacks the port's required capabilities) or `not_admitted` (the port's owner admitted another sender — recorded once per admission, the rest counted into `PORT_DESTROYED`, so a sender the port will never take cannot flood the trail). |
| 3012 | Error | `MESSAGE_TOO_LARGE`           | Payload exceeded `max_payload`. |
| 3013 | Error | `MESSAGE_SEND_TO_CLOSED_PORT` | A send raced with destruction and lost. |
| 3014 | Debug | `MAILBOX_FULL`                | The sender's own lane at the port was at the port's per-sender depth; the receiver has not taken that sender's earlier messages yet. Another sender's traffic never causes it. |
| 3015 | Warn  | `PORT_ENDED_SENDER_DISCARDED` | A sender that ended left messages queued that did not fit the room the port's owner set aside for ended senders, and they were discarded whole. |
| 3020 | Info  | `SHMEM_CREATED`               | A shared-memory object was created. |
| 3021 | Info  | `SHMEM_MAPPED`                | A mapping was established. |
| 3022 | Error | `SHMEM_MAP_DENIED`            | A mapping request was refused. |
| 3023 | Info  | `SHMEM_REVOKED`               | A shared-memory object was revoked. |
| 3030 | Info  | `NOTIFY_BOUND`                | A receiver bound to a channel. |
| 3031 | Info  | `NOTIFY_SIGNALLED`            | A notification was delivered. |
| 3032 | Error | `NOTIFY_SIGNAL_DENIED`        | Sender lacks the channel's signal capabilities. |
| 3040 | Info  | `CALL_ENDPOINT_CREATED`       | A capability-checked synchronous call endpoint was created. |
| 3041 | Error | `CALL_ENDPOINT_CREATE_DENIED` | A call-endpoint creation request was refused. |
| 3042 | Info  | `CALL_ENDPOINT_DESTROYED`     | A call endpoint was destroyed (in-flight callers fail closed). |
| 3043 | Debug | `CALL_POSTED`                 | A request was posted to a call endpoint, awaiting a reply. `Debug`, not `Info`: the synchronous call path is the high-throughput RPC transport (e.g. the USB URB endpoint), so a successful post is routine throughput that would flood the log two records per round-trip; it stays available when the level is lowered for forensics. Its denials (3044–3047) stay at `Error`. |
| 3044 | Error | `CALL_POST_DENIED`            | Caller lacks the endpoint's required capabilities. |
| 3045 | Error | `CALL_REQUEST_TOO_LARGE`      | Request payload exceeded `max_request`. |
| 3046 | Error | `CALL_POST_TO_CLOSED_ENDPOINT`| A post raced with destruction and lost. |
| 3047 | Debug | `CALL_QUEUE_FULL`             | A posted call found its caller already at the endpoint's per-caller depth of calls waiting on the server; the server answering one makes room. Another caller's traffic never causes it. |
| 3048 | Debug | `CALL_REPLIED`                | A server delivered a reply to an in-flight call. `Debug` for the same reason as `CALL_POSTED` (3043): routine high-throughput RPC completion. Its denial (3049) stays at `Error`. |
| 3049 | Error | `CALL_REPLY_DENIED`           | A reply was refused. `reason` discriminates: `oversize_reply` (exceeded `max_reply`) or `unknown_ticket` (no such in-flight call — timed out, cancelled, or forged). A late reply after a missed deadline is preceded by its own `CALL_TIMED_OUT` (3053). |
| 3053 | Warn  | `CALL_TIMED_OUT`              | An in-flight call's deadline elapsed before the server replied; the ticket is retired and a late reply is refused. Recorded at the IPC layer because an in-kernel caller (the filesystem's block path) never reaches the syscall dispatcher's audit — without it, a device that stopped answering leaves no trace but the puzzling refusal of its own late reply. Edge-triggered: retiring the ticket means a later poll finds nothing. |
| 3050 | Error | `CALL_ENDPOINT_REGISTER_DENIED` | A registry bind was refused: the `EndpointId` was already bound, or the registry could not keep the binding; the freshly created endpoint is dropped (mirrors `PORT_REGISTER_DENIED`, 3004). |
| 3051 | Info  | `CALL_POSTER_VANISHED`        | A caller task exited with calls still in flight on this endpoint; the kernel cancelled them (queued requests dropped before service, in-service tickets retired so the server's reply fails closed, unclaimed replies discarded). |
| 3052 | Info  | `CALL_ENDPOINT_GRANTS_REVOKED` | A destroyed endpoint's delegated per-endpoint grants were revoked. |
| 3060 | Error | `PAYLOAD_ALLOC_FAILED`        | The kernel heap could not hold the wiped-on-drop copy of a port send or a call's room — at its post, or for a reply whose call kept none — so the transfer failed closed with `Errno::OutOfMemory`. Machine distress rather than back-pressure, hence `Error` where `MAILBOX_FULL` / `CALL_QUEUE_FULL` are `Debug`. |
| 3061 | Debug | `IPC_SENDER_LIMIT_REACHED`    | A send or post was refused `LimitExceeded`: it would have carried its sender past its own `ipc-queue-bytes` limit, its lane's growth included, or the caller already holds `IPC_SENDER_DEPTH_MAX` answers it has not collected. The sender's own limits refusing it, never the receiver's state; `Debug` like the other back-pressure events, so a flooding sender cannot turn each refusal into an alert. |

Adding a new event requires assigning the next free identifier in
`kernel/ipc/src/audit.rs` and appending a row to this table.

## Error mapping

| Errno                  | Cause                                                |
|------------------------|------------------------------------------------------|
| `PermissionDenied`     | Sender / binder / mapper lacks a required capability |
| `MessageTooLarge`      | Payload exceeded the port's `max_payload`, or the message with the lane it opens exceeds the sender's whole `ipc-queue-bytes` limit (EMSGSIZE) |
| `LengthOutOfRange`     | Configuration out of range                           |
| `WouldBlock`           | The sender's own lane at the port or endpoint is at its per-sender depth — back-pressure on that sender alone, retry once the receiver takes one of its messages or the server answers one of its calls |
| `LimitExceeded`        | The send or post would carry the sender past its own `ipc-queue-bytes` limit, or the caller holds `IPC_SENDER_DEPTH_MAX` answers it has not collected |
| `NotFound`             | Send to destroyed port, map of revoked shmem, unregister of an unbound endpoint, publish naming an unregistered endpoint, withdraw of an unbound name |
| `AlreadyExists`        | Register of an already-bound `EndpointId`, publish of an already-bound `PortName` |
| `OutOfMemory`          | A port message's wiped-on-drop copy, a call's room, the lane room either needs, a registry's binding or well-known name, or a sender's note in its index could not be allocated — never a reply into a kept room |

Every error path emits a matching audit event before returning, so
"fail closed" is observable in the security trail (`AGENTS.md` §5.4).

A sender holding a payload its receiver must not lose reads each refusal by
one rule, `tairix_abi::ipc::SendRefusal`, and a send's whole outcome as
`Sent`: `WouldBlock` and `LimitExceeded` wait on the destination's
`PortRoom` member, which the kernel wakes for a receive there and for any
release of the sender's budget; `OutOfMemory`, which no event reports
freeing, is offered again after `IPC_MEMORY_RETRY_NS`, timed by one
`MemoryPause` per loop or a `Reoffer` per held payload; `NotFound` means the
receiver is gone and nothing is lost; any other refusal is final, and the
loss is counted (`tairix_log::Doubling`) and stated. A loop watching one
port keeps its member with `tairix_rt::room::RoomMember`, on that port and
no other, since a level-triggered member left on a port owed nothing would
wake it for ever.
