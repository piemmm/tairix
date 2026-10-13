# Link-local service discovery (`discoveryd`)

`discoveryd` is the multicast DNS / DNS-SD service, installed at
`/System/Services/discoveryd.app/Run` and started by PID 1 at boot: a segment
takes time to answer, so the service should already be listening when a program
first asks. It requires `network-up`, so it starts once the network stack
answers, and since its sockets live in the stack, a stack relaunch stops it and
starts it again against the new one. The staged design is `plans/ZEROCONF.md`. Programs reach it through
[`lib/discovery`](../lib/discovery.md); the protocol engine is
`tairix_net::mdns` ([`lib/net`](../lib/net.md)); the containment is the
supervised session of [`lib/sandbox`](../security/sandbox.md).

It asks and answers questions for its clients. It publishes nothing of its own
yet.

## The authority split

A multicast DNS datagram is written by whoever shares the segment — a café's
network, a compromised printer. The process that parses it therefore holds
nothing worth taking.

| | Front | Decoder |
|---|---|---|
| Is | the service as started | the same binary, respawned in the kernel's sandbox spawn mode |
| Holds | the two multicast DNS sockets, the discovery endpoint, `CAP_NET`, `CAP_SANDBOX_SPAWN`, `CAP_IPC_BIND_PRIVILEGED`, `CAP_FS_ACCESS` (the grant store, read once at start), `CAP_LOG_EMIT` | two pipe ends; an empty capability record and the sandbox syscall allow-list |
| Parses | nothing a peer sent; only fixed fields from its decoder, each bounds-checked | every datagram, through one engine per interface |
| Has a clock | yes | no: each frame that moves time carries the instant |
| Has randomness | the kernel CSPRNG | none of its own: its cache key and CSPRNG key arrive in its first frame |

The network stack reserves the multicast DNS port and groups to the service's
account (`DISCOVERYD_UID`), so no other process can hear the segment's answers
or send a query that draws unicast replies from every responder on it.

## Clients

A client opens a **session** on the reserved `DISCOVERY_ENDPOINT`, naming a
delivery port its own process owns, and starts typed requests in it
(`tairix_abi::discovery_ipc`): browse a service type, resolve an instance, look
up a host's addresses, name a link-local address, or enumerate every type. The
kernel attests the port's owner
([`call_peer_port`](../architecture/syscalls.md)); any other port, bound or
not, is refused alike, so no caller can aim the service's doorbell at another
process's mailbox. Each request is answered continuously until it is stopped.
Answers queue in the session and are taken with a call that never waits; the
service rings the port once when answers are waiting and not again until the
client has collected them all, so a slow reader costs one doorbell however many
answers queue. A doorbell a full port refuses is owed, not lost, and posted
once the port has room. A client believes a doorbell only when its
kernel-attested sender is the service's account and it names one of the
client's own sessions.

The service derives every name it asks the segment from the request's fields,
so the type a browse is scoped to is a field it reads, never a name the caller
spelled. Every answer names the interface it was learned on, and nothing merges
two links.

### Admission

Decided from the caller's kernel-attested `Origin`, before any state is
touched:

| Request | Needs |
|---|---|
| any | `CAP_NET` |
| host, reverse | nothing more |
| browse, resolve | a grant for the type, or `CAP_NET_DISCOVER_ALL` |
| enumerate types | `CAP_NET_DISCOVER_ALL` |

Grants live in `/System/Security/Policy/Discovery`. The image builder writes
them from each signed manifest's `browses` list, keyed on the bundle id and
publisher the load gate attests, so a grant is never more than a verified
manifest asked for. The service reads the store once at start and takes it
whole or not at all: a store that does not parse grants nothing, and says why.
A refusal names what the caller lacked and the type it asked for, never whether
another principal holds that type.

### Bounds

Fixed containment bounds, since each is state the service holds for a client,
sized so an inventory of a segment of hundreds of hosts fits one account
(`netmap` holds up to 577 requests in one session):

| Bound | Value |
|---|---|
| Sessions per account | 32 |
| Requests per account, across its sessions | 1024 |
| Queued answers per session | 256 KiB |
| Queued answers per account, across its sessions | 1 MiB |
| Queued answers in all | 16 MiB |
| Sessions in all | 4096 |
| Requests in one session asking one question | 2 |
| Fresh questions an account's requests raise | a burst of 64, then 16 a second |
| The same, every account together | a burst of 256, then 64 a second |
| Replays an account's joins cost the decoder | a burst of 32, then 8 a second |
| The same, every account together | a burst of 256, then 64 a second |
| Distinct questions, every client together | 4096 (`mdns::MAX_QUESTIONS`) |

Past any queue bound a request's updates are dropped and it is told `Lost`
once, after everything queued before. Requests are counted per account so
opening sessions cannot multiply them. An account's queues together hold a
sixteenth of the whole, so readers that never collect fill the service only
from sixteen accounts. An account is charged a question slot for each
subscription its requests hold and each question it left that the decoder has
not yet said it stopped, and is held to half the questions every client
shares. An account charged its floor of 256 raises a fresh question only while
more than 1024 slots remain free, so accounts asking all they may can together
fill no more than three quarters of the table, and four floors' worth is
always left for the rest. A request is admitted only after its authority is checked, so a caller
at its bound asking what it may not ask is refused and audited as
unauthorised. A drained queue gives back its storage. A session, a request,
an answer, a grant, and a request's place in a question it shares are each
found by key, an exited owner's sessions by the owner, and the doorbells owed
and the losses a session owes by their own lists, so no call or answer costs a
scan of the tables. Only a link edge and a replaced decoder, both rare, visit
every question.

A session ends when its owner exits: the service watches every principal it
holds a session for through the kernel's peer-exit watch
([`peer_watch`](../architecture/syscalls.md)), so a client that dies takes its
questions with it.

## Questions

Each distinct question — a form and a name — is asked once, however many
requests share it, on every interface whose link is up. A request that joins a
question already asked is brought up to date by a replay from the decoder's
cache rather than from a second copy in the front.

A question replays to one group of requests at a time: those joining while a
replay waits to be sent share it, and those joining while one is in flight
share the next. A replay walks everything the question holds, so a join that
needs a fresh one is charged to the account's replay budget and refused with
`Busy` once it is spent; a request starting and stopping in a loop costs no
more than that. A fresh question is queried on every link out of the group
budget every client shares, so it is charged to the account's fresh-question
budget the same way: one client churning names cannot starve another's first
query. A session may hold two requests asking one question — a
replacement started before the request it replaces stops — and no third, since
each copy costs every answer's fan-out and buys nothing.

A question keeps its requests in runs: those told every edge, those owed the
replay in flight, those owed the next, and those whose queue lost answers and
take nothing more. An edge is offered to the first run alone, a replayed answer
to the second, so a crowd of stalled or replaying requests costs an answer
nothing. An answer renewed again within a second of the last renewal told —
copies of one record, which an honest responder never sends faster — is not
told again.

A question's id is its slot in the front's table. The decoder acknowledges
every stop, and the id is given to another question only after that word:
frames cross the pipe in order, so no answer to the old question can follow
it, and ids are reused rather than run out.

The decoder owes an acknowledgement for every stop, link edge, and replay it
is sent, in the order they were sent. The front holds what it is owed in that
order with the instant each was sent: an acknowledgement out of order, of
something never sent, or withheld for 30 seconds condemns the decoder, so one
that stops answering cannot keep an id, a link's answers, or a late joiner's
replay from moving. A replaced decoder owes nothing, and every id it was
stopping is free at once.

The front keeps keyed fingerprints of what each question holds on each link,
which is all it needs to hold the decoder to its word: an answer is added once,
renewed or retired only while held, and never beyond the most records an honest
engine's cache holds on one link — counted across every question there, since
each cached record answers one question at most. An answer that crossed a stop,
a flush, a link edge, or a replaced decoder in flight is stale and dropped; one
no honest decoder sends — about an id that holds no question, or a stop it was
never told — condemns it.

## The channel

Every frame is one fixed layout behind a one-byte tag, and both directions
refuse a frame whose tag, length, or any field is not exactly what an honest
peer sends. The encodings are canonical: a frame that decodes re-encodes to
exactly its bytes.

| Direction | Frame | Carries |
|---|---|---|
| front → decoder | `Configure` | the cache key and CSPRNG key; the first frame, sent once |
| front → decoder | `Datagram` | the instant, the arrival interface, the sender's address and port, whether it went to the group, the payload unread |
| front → decoder | `Tick` | the instant |
| front → decoder | `Link` | the instant, an interface, and whether its link came up — with its group budget as the front holds it — or went down |
| front → decoder | `Ask` | the instant, a question id below `MAX_QUESTIONS`, its form, and the name |
| front → decoder | `Stop` | a question id, which the decoder acknowledges with `Stopped` |
| front → decoder | `Replay` | a question id and a token naming the requests owed it |
| decoder → front | `Deadline` | the earliest instant any engine next needs time, or none |
| decoder → front | `Answer` | one edge in one question's answers, already typed |
| decoder → front | `Held` / `Replayed` | a replay's answers, then its end |
| decoder → front | `Transmit` | a datagram an engine built, its interface, the instant it was charged at, and its destination |
| decoder → front | `Linked` | the acknowledgement of one link edge |
| decoder → front | `Stopped` | the acknowledgement of one stop, after which nothing about the question follows |

The decoder acknowledges every link edge. The front believes an answer about a
link, and sends a datagram built for it, only once every edge it told the
decoder has been acknowledged, so nothing learned before a link's last edge
reaches a client after it. A link that goes down tells each request that held
something there one `Flush`, however many of its questions held answers on
it, and finding those requests needs no memory, so a flush is never lost to
an allocation.

A decoder that receives anything before its configuration, a second
configuration, or a frame it cannot decode ends its session: the front never
sends one, so guessing would only hide a bug.

## The front's rules

- **Only on-link senders are relayed.** The stack stamps every delivery with
  whether its sender is on the arrival interface's link. The front drops
  anything else unread, because reflected mDNS is an amplifier.
- **Relay admission is budgeted per sender, then shared.** A sender gets a
  burst of 64 at 32 per second; all senders together get 2048 at 1024 per
  second, across 32 tracked senders.
- **What it sends is bounded too.** A datagram goes to the group, within the
  interface's budget of a burst of 32 at 8 per second, or straight back to a
  peer the front relayed from on that interface within the last second, once
  per datagram relayed from it and at most four owed at once. It names its
  egress interface, so a message built from one link's state never leaves by
  another.
- **The decoder spends the group budget the front charges.** Each link's
  engine holds the budget as the front does — handed over with the link-up
  edge, so a replacement decoder inherits what its predecessor spent — and
  each datagram for the group names the instant it was charged at, which the
  front charges it at too. A query the budget cannot pay for stays due until
  it can, rather than being counted sent and dropped, and the decoder's
  scheduled sends leave the last four datagrams of the budget to an answer
  that defends a name this host owns. Since an honest decoder spends exactly
  as the front charges, a datagram past the budget, or charged at an instant
  the front has not reached, condemns it.
- **Back-pressure costs nothing per datagram.** The decoder's queue holds
  64 KiB. A datagram it has no room for is held, and the front stops draining
  its delivery port until the queue drains.
- **Time is paced by the front.** It ticks the decoder when the reported
  instant comes, never before the decoder has reported since the last tick, and
  never closer together than 10 ms. After every wake, the next wake it asks for
  is later than the instant it just acted on, so its loop parks and never
  spins.
- **Containment is total.** A decoder is reaped and logged, and everything
  learned from it dropped — every client holding an answer is told the answers
  are void — if it crashes, breaks the framing, ends its stream, or sends a
  frame no decoder sends. A replacement starts after the supervisor's paced
  delay (100 ms, doubling to 30 s), is keyed afresh, and is told the links and
  asked the questions again.
- **Without entropy, no decoder.** If the random source refuses the keys a
  decoder needs, the service logs why and exits.

## The reactor

One wait-set holds the delivery port (registered only while the front takes
datagrams, and drained at most one mailbox's worth per wake), the decoder's two
pipe ends, the discovery endpoint, the thread's peer-exit feed, and room on any
client port a doorbell is owed to, with a timeout for the one instant the front
must next act by. A port is watched only while a doorbell is owed there, and the
watches change only for a port whose count crossed zero since the last turn. A
dead wait-set ends the service rather than degrading into a poll.

## Audit records

The service owns the stable `tairix_log::EventId` range **`25_000` …
`25_999`**. A decoder's crash is `lib/sandbox`'s own `6000`, and a refused
decoder launch is its `6001`.

| Id | Name | Level | Meaning |
|---|---|---|---|
| `25_001` | `SERVICE_STARTED` | Info | The front holds its multicast DNS sockets; carries each address family's outcome — joined, or the step the stack refused and why. |
| `25_002` | `DECODER_STARTED` | Info | A decoder started and was keyed; carries its generation. |
| `25_003` | `SERVICE_UNAVAILABLE` | Error | The service cannot serve and is exiting; carries the reason, and each family's outcome when no socket could be opened. |
| `25_004` | `REQUEST_DENIED` | Warn | A client request was refused for want of authority; carries what was lacking, the caller's uid, and the type asked for. |
| `25_005` | `GRANTS_REFUSED` | Warn | The grant store could not be opened or read, or was refused whole, so no application may browse; carries the reason. |

## Tests

The front, the decoder, the channel, the question table, the sessions, and the
query planner are host-tested, the front over in-process workers — a recording
one, a doomed one, one that lies, and the real decoder end to end. They cover
relay admission, the held datagram, the tick floor, crash replacement under
fresh keys, condemnation, admission and refusal, grants, replays to late
joiners, link flaps in flight, bounded queues and the `Lost` notice, peer exit,
every transmit rule, one session holding hundreds of requests through the real
decoder, ids freed only by the decoder's word and never run out, a withheld or
unprompted acknowledgement condemning the decoder, a doorbell port the caller
does not own refused, owed doorbells and the edge-driven watches on their
ports, a flush told once per request, replays shared and budgeted, a third
copy of a request refused, copies of a record renewing it once a second, a
request that lost answers offered nothing more, the runs kept in step through
random churn, a query the budget cannot pay for waiting due, scheduled sends
stopping at the reserve, overspending and a replacement inheriting what was
spent, a client churning fresh names leaving another's first query unharmed,
per-account request bounds, and id wrap that never names a live session or
request.

`fuzz_discoveryd` is enrolled in `cargo xtask fuzz`: both codecs canonical under
random frames and single-bit mutations, the decoder total over hostile and
well-formed datagrams with time running backwards, and the front against a
decoder saying anything at all while it holds its no-spin bound, configures each
decoder first and once, spaces its ticks, contains the decoder, and relays to
its replacement. It also drives random calls from several accounts and
processes against a real decoder — foreign doorbell ports, doorbells that block
or vanish, segment answers, link edges, and the decoder killed and replaced
mid-sweep — checking the front's tables after every step, and that nothing
any client asked for is left once every client has gone.

The live vertical `tairix-test-discovery-qemu-aarch64` boots the production
aarch64 image with a host-side multicast DNS responder on the wire and runs
`dns-sd` browse, resolve, and host lookups through the whole path; the peer
requires that the guest asked the wire for every record they need, and the
script that the guest printed each of the peer's answers.
