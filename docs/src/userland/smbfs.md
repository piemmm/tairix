# Network shares (`smbfs`)

`smbfs` lists and mounts SMB 3 shares. It is installed at
`/System/Services/smbfs.app/Run` and enrolled at boot under its own `smbfs`
account. It does not require `network-up`: when the stack is relaunched it
loses its connections and makes them again, rather than taking every mounted
share down with it. The staged design is `plans/SMB.md`. Programs reach it
through [`lib/netshare`](../abi/netshare.md); the SMB engine is
[`lib/smb`](../lib/smb.md); its workers are sessions of
[`lib/sandbox`](../security/sandbox.md).

## The authority split

Everything a server sends is a stranger's, so the process that parses it holds
nothing.

| | Front | Worker |
|---|---|---|
| Is | the service as started | the same binary, respawned in the kernel's sandbox spawn mode, one per connection |
| Holds | the `netshare-v1` endpoint, the sockets, the offered volumes, `CAP_NET`, `CAP_FS_SERVE`, `CAP_IPC_BIND_PRIVILEGED`, `CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT` | two pipe ends; an empty capability record and the sandbox syscall allow-list |
| Parses | nothing a server sent; only its workers' fixed frames, each bounds-checked | every byte its connection's server sends: SMB, NTLM, SPNEGO, DCE/RPC and each decryption |
| Holds a secret | the key each connection's password derives (NTOWFv2), never the password | that key, and the session's keys |
| Has a clock and randomness | yes | neither: the front sends what each sign-in needs |

A worker reports facts — what the server negotiated, a share it listed, a tree
it connected, the error a request met — and the front judges what each means
for a caller, so a compromised worker can choose among refusals but never
widen what a caller is given. A worker whose frames are not what an honest
worker writes is ended, costing only its connection — which is made again,
with a new worker, for the shares mounted on it.

## Calls

The endpoint is reserved and bound requiring `CAP_NET` of a caller, so a
program that may not reach the network cannot reach a server through the
service. A caller's identity and login session are its kernel-attested origin.
Every call is one request and one reply; a call needing the network is held by
its ticket and answered when the work completes, fails, or outlives its
deadline. The request a call carries, password included, is erased as soon as
it has been read, and a request the service cannot read is answered
`Unreadable`.

## Connections

A connection is made for one principal, one endpoint — the host, its port and
interface, and the address the caller named, if any — and one account. It
serves only calls naming the same account with the same password, the keys
compared in constant time, so no call rides a session it did not prove it may
open. A host named without an address is resolved off the loop, on a worker
thread. Listing shares and then mounting two is therefore one connection and
one session, and a connection nothing needs is kept 30 seconds for the call
that follows.

A remembered credential is offered only with the pin it was remembered under:
the GUID the server negotiated with and the address it was reached at. The pin
is checked once negotiation reports it and before any credential leaves; a
call whose pin differs is answered `PinMismatch` with what the connection
found, and a connection no remaining call needs is closed unauthenticated.

Each connection has its own delivery port. While an order waits for room
toward its worker the front takes no further event from that port, so a worker
that has not caught up holds its server back through TCP rather than filling
the front. The front always reads its workers, so a worker writing never waits
on the front; bytes the socket has not taken wait in the connection's backlog,
bounded at 4 MiB, past which the worker is lying and is ended.

## Reconnecting

A connection mounted shares depend on is made again when it drops: its socket
closes, its worker ends or lies, or it goes quiet. While its mounts have a
call waiting on the server, a file held open or a directory leased, a
connection unheard for 10 seconds is asked whether the server still serves it
(an SMB2 `ECHO`, outside any session); left unanswered 10 seconds more, it is
taken for dropped — within a call's deadline, so its calls can still be
answered. A mount with none of these is not asked after, so an idle mount
costs no wakeups; its drop is found by the next call made on it.

- **The grace window.** On a drop the mounts wait for their connection for the
  longest the server keeps any of their held files' opens, or MS-SMB2's
  default of 60 seconds where it keeps none. Meanwhile they are listed
  `Reconnecting`, a call waits (to its own deadline), and the volume tells the
  kernel it is working.
- **Attempts.** The first is made at once, the host resolved afresh unless the
  calls named an address; one that fails is tried again after 1 second,
  doubling to at most 30 seconds between attempts. A connection that drops
  again within 30 seconds of coming back waits as an attempt that failed
  would.
- **Made again.** The new connection offers the client GUID the old one did,
  and its session names the one it replaces, so the server lets the old one
  go and keeps what it held for it. Each mounted share's tree is connected
  again. Each held file's durable open is reclaimed under its lease key; an
  open whose answer the drop took is asked again under the name it was first
  asked by, with the replay flag, and the server answers with the open it
  made, if it made one; any other is opened afresh, and nothing cached of it
  is trusted. What held files hold written goes on, writes that may repeat
  ones the drop took flagged as replays.
- **Calls in flight.** A call the drop took the answer of is asked again —
  except a change by name (a create, a remove, a rename), which might already
  have been made and is answered offline, since the caller cannot be told
  whether it was.
- **Past the window.** The mounts are listed `Lost`: every call needing the
  server fails at once with the device error, and what held files held
  written is lost, owed to the next flush. Attempts go on; when one succeeds
  the mounts serve again.
- **Ended instead.** A server that refuses the account, or that answers as
  another server or at another address than its mounts are on, ends the
  connection and withdraws its mounts; it is not tried again.
- **Unmounting** a share whose server is gone waits up to a call's deadline
  for what it holds written, then lets the volume go: the bytes are recorded
  as lost, and every answer it gives says the volume held writes the server
  never took, so the kernel keeps the volume lost until its owner detaches it.

## Listing and mounting

A listing answers the server's disk shares, the special ones only when asked:
the first 256, with counts of the special shares left out, of the shares no
tree connect could name, and of those past the 256.

A mount connects the share's tree, offers its volume to the caller
(`fsserve_offer`, [network attachments](../filesystem/served.md)) and answers
the offer, which the caller attaches itself. A mount needs a login session to
end with: when the process anchoring the session ends, every mount of it is
withdrawn. A share's volume key is derived from the server's GUID and the
share's name, ASCII case folded, so a share keeps one volume identity across
mounts; a principal holding a share mounted is answered `AlreadyMounted` for it
again. Only a mount's owner can unmount it.

## The served volume

The front serves each mounted volume over `fsserve-v1`. A call on a node the
kernel does not hold open is answered by one compound on the share's tree —
open, act, close — so it sees the share as other writers left it; a listing
keeps its directory open between pages, and a file the kernel holds open is
kept open too (below).

- **Nodes.** A server keeps no ids a client may name, so the front names each
  path it hands the kernel. The volume declares ASCII case-insensitive name
  matching, as Windows and default Samba shares match names, so two spellings
  of one name are one node; on a share that tells case apart, names differing
  only in case reach one of them. The server's file id, where it keeps one, is
  recorded with the path, so a node another writer replaced is answered stale
  rather than mistaken for the one held. A pinned node is never forgotten; the
  least recently used of the rest is.
- **Bytes.** A read's data lands in its call's span as the worker reports it,
  never past the span and never once the call is answered or timed out, so
  the kernel may reuse the span at once. A write's bytes are staged to the
  worker from its span before the write is ordered, and a write missing any
  staged byte is refused unsent. A release is answered once no read in hand
  still writes into its span.
- **The driver contract.** A read from a file's end on reads nothing. A rename
  replaces a file it lands on; a directory moved over an empty one has it
  removed first, as a share replaces no directory, and a move into its own
  subtree is refused. A remove marks its node for deletion rather than opening
  it delete-on-close, so a directory holding anything is refused, not kept in
  silence. An operation on the wrong kind of node is unsupported before the
  server sees it.
- **Opens.** A server may fail a compound's close for the failure before it;
  the open it left is then closed on its own. A listing let go while its open
  is in flight is closed once the open is answered. The worker names every
  open the front keeps by one name the front gives it, unique across the
  volumes a connection serves.
- **Liveness.** While calls are in hand the volume tells the kernel every
  2.5 seconds that it is working — a quarter of the kernel's idle bound — so a
  call is abandoned only by its own deadline or its caller.
- Links, punched holes, security records and named attributes are
  unsupported, and ownership is unrecorded.

## Caching

What the front caches, the server's leases let it cache; nothing else stands
longer than a second.

- **Held files.** The first read, write or resize of a file the kernel holds
  open opens it on the server under a lease asking reads, the handle and
  writes, and keeps it open until the kernel's last hold goes. One held file's
  calls run in the order they came, so no write overtakes a truncate and no
  read the write before it. A server that will not hold the file open leaves
  it reached by path.
- **Reads.** Under a lease with reads, what is read is kept in 64 KiB pages,
  in the process's one bounded, pressure-governed cache of clean file data,
  keyed by the lease's generation: a break that takes reads away, or a resize,
  makes every page of the file unreachable at once.
- **Writes.** Under a lease with writes, a write is answered once its bytes
  are held: in 4 KiB blocks, each knowing which of its bytes are held, so a
  write costs the blocks it spans however scattered the file's writes. They
  are sent, each stretch of held bytes as writes of at most 64 KiB, within a
  second of being written; sooner when the bound on what is held needs the
  room; before a truncate, a sync, a close or a break that takes writes away
  is acknowledged; and before an unmount or the end of the login session
  withdraws the volume, which waits up to a call's deadline for them. Every
  reply says whether the volume holds bytes the server does not have.
- **Losses.** A write the server takes in part has its rest sent again; one
  it takes none of, or refuses, once its call was answered, is lost, and so
  is what it was sending when it answers nothing for a call's deadline. A
  loss is recorded, owed to the volume's next flush — which is answered with
  its error, even after the file was closed — and leaves the file served
  uncached until it is opened again, as what the kernel was told of it no
  longer holds.
- **The bound.** What is held written is pinned memory — its blocks and the
  index over them — bounded by the machine's share of RAM every writable
  volume of the service draws on and narrowed by memory pressure. A volume
  holding nothing may always hold one write; under critical pressure, or with
  the pressure notice refused, nothing is held and every write is sent before
  it is answered.
- **Breaks.** The server breaking a lease is answered as the lease asks: the
  pages it no longer covers dropped, the writes it no longer covers sent, then
  the acknowledgment. A compound of the front's own on a file it holds carries
  the file's lease, so its own removes and renames break nothing of its own.
- **Removing an open file.** A file the kernel holds open and the front holds
  is moved aside to a hidden name beside it, then that name is removed: the
  name is free at once, and the open serves on until its last hold goes.
- **A rename over a held file** waits for the front to let its own open on
  that file go, as no share replaces a file that is open.
- **Attributes and listings.** What the server said of a node, and a
  directory's whole listing of up to 4096 entries, are kept a second — CIFS's
  `actimeo` default — in caches of file metadata, and the front's own changes
  forget what they make stale at once. On a server that leases directories, a
  listing opens its directory under a lease and keeps that open once the
  listing ends, so the listing and what it said of its entries stand until
  the lease breaks; a directory lease breaks whenever what a listing of it
  would show changes, a child's size or times included. A lease left 30
  seconds unused — CIFS's `dir_cache_timeout` default — is let go, so a mount
  left idle holds nothing open. Each volume keeps at most 16 directories
  leased, letting the least recently used go first.
- **Pressure.** The service follows the system's memory-pressure notice:
  each cache shrinks to what the band lets it keep, and with the notice
  refused it caches nothing.

## Bounds

| Bound | Value |
|---|---|
| A call's deadline | 30 s |
| A connection's set-up | 30 s |
| An idle connection is kept | 30 s |
| Connections per principal | 16 |
| Calls waiting per principal | 16 |
| Mounts per principal | 32 |
| Shares in a listing | 256, the rest counted |
| A worker frame | one socket event's bytes, or one share or listing entry |
| One read or write | 64 KiB, a lane's span |
| Unpinned nodes remembered per mount | 16 384 |
| Listings open per volume | 8 |
| A listing nothing reads is kept | 30 s |
| A cached page | 64 KiB |
| A held block | 4 KiB |
| Written bytes wait at most | 1 s |
| Writes of one file in flight | 8 |
| Held work is given up after | 30 s without an answer |
| Attributes and listings stand | 1 s, or until their directory's lease breaks |
| A listing kept whole | 4096 entries |
| Directories leased per volume | 16 |
| A directory lease kept unused | 30 s |
| A connection something rests on, unheard before it is asked after | 10 s |
| The ask unanswered before the connection is taken for dropped | 10 s |
| A dropped connection's grace window | the longest durable timeout its files were granted; 60 s with none |
| Waits between attempts | 1 s, doubling to 30 s |

Each bound fails closed with a typed refusal.

## Events

| Id | Event |
|---|---|
| 35000 | serving |
| 35001 | cannot serve, and why |
| 35002 | a share was mounted |
| 35003 | a mount was withdrawn: unmounted, its session ended, or its connection ended |
| 35004 | a request was refused, and why |
| 35005 | a host is not the one a remembered sign-in is for |
| 35006 | a server refused the credentials |
| 35007 | a connection ended for a reason of its own |
| 35008 | a request could not be read |
| 35009 | a caller could not be attested |
| 35010 | lookups run on the service's own thread |
| 35011 | bytes written to a share, their write answered, were not taken |
| 35012 | the memory-pressure wake was refused, so nothing is cached |
| 35013 | a connection mounted shares need dropped, and its grace window |
| 35014 | a dropped connection was made again |
| 35015 | a dropped connection was not made again within its grace window |

A record names what it concerns — the principal, the host, the address it was
reached at, the share, the offer — and never a credential or an account name.

## Tests

The worker is tested against `lib/smb`'s scripted server; the front over an
in-process world whose sockets reach that server: listing, sharing and its
account rule, pins, mounts, ownership, session ends, deadlines, the linger and
every bound. The served volume is driven through that world as the kernel
drives it: the operations end to end, case, the rename and remove contract,
batched and resumed listings, held answers timing out with their late data
landing nowhere, a call timing out with no memory to spare, releases, holds,
whole-span writes, two lanes in flight, and a worker reporting more than a read
holds. The world serves one share to every connection, so the caching is driven
against a second client: pages read once, writes held and sent by their window,
a flush, a close, a truncate after writes, another client's open breaking a
lease, a refused write owed to the flush and a loss at close still owed, short
writes sent again until one takes nothing, scattered writes held to what they
pin, a wave answered slowly but steadily, renames and removes of open files,
the bound, memory pressure, unmounts and session ends sending what is held, the
second's attributes and listings, and directory leases, let go once idle and
the least recently used first past the most. Dropped connections are driven
through it too: a connection cut, gone quiet, or its worker lost; a quiet one
whose server answers each ask kept, and an idle mount left unasked until a call
rests on it; the held file's bytes carried across, and a file first held during
the drop served once it is back; a read asked again and a change answered
offline; an open whose answer the drop took found by its replay; a server
restarted; the window's end and the mount coming back; a server answering as
another, or refusing the account; attempts backing off, and a connection that
drops as soon as it is back; and an unmount whose server is gone. A quarter of
the fuzz harness's rounds run against a server that takes half of every write.
`fuzz_smbfs` holds both frame codecs canonical and total, and the front whole
against workers whose frames are dropped, repeated, corrupted or replaced and
servers whose answers are held back: every call, a client's or the kernel's,
answered once, nothing left behind.
