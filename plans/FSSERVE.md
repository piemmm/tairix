# FSSERVE.md — Serving a filesystem from user space

Staged plan for `fsserve-v1`: the protocol a user-space filesystem driver
serves a volume to the kernel's VFS over, the kernel's proxy for it, the
partition-scoped block grant the driver reaches its volume through, and the
removable-media drivers moved onto it. **Binding under `AGENTS.md`** (§4, §5,
§8, §17.4, §18, §19.5, §24, §26). It closes `plans/OPEN-DEFECTS.md` D1072
(`PLAN.md` FS-USERSPACE) and is the protocol `plans/SMB.md` SMB9a–SMB9e serve shares
over. It consumes `plans/DEVICES.md` D3–D4 (the attach path, retained writes,
surprise removal, verified re-insert), `plans/FIX-IO.md` (deadlines, health),
`plans/ARXFS-WRITEBACK.md` (the commit window) and
`docs/src/filesystem/drives.md` (§10 `Storage:`, §20 foreign filesystems).

## Ledger

| id | What it is | Status |
|---|---|---|
| FSS1 | The `fsserve-v1` ABI (`lib/abi/src/fsserve.rs`): the request/reply codec over a call endpoint and a shared data window, mirroring the filesystem driver traits; fuzzed in both directions | done |
| FSS2 | `lib/fsserve`: the server runtime a filesystem driver links — dispatch onto the driver traits, the in-process host and its single-shot commit deadline, the granted volume as a block device, the format trait, the driver program — and the conformance suite over the real codec | done |
| FSS3 | The kernel `ServedFs` proxy: a `KernelFs` over the protocol with liveness-bounded, killable calls, window quarantine, hold calls settled rather than withdrawn, releases reaped by the volume's own operations, fail-closed reply validation, and the server-health overlay | done |
| FSS4 | The volume block grant: `HwResourceKind::Volume` under `CAP_FS_SERVE`, the kernel's served-volume registry, and `volume_read` / `volume_write` / `volume_ctl` / `volume_serve` | done |
| FSS5 | Removable volumes served from user space: the kernel-published volume node, the attach, serve, refusal, detach, unplug, server-loss and re-insert flows, the six driver bundles (ext4, FAT32, ADFS, ISO 9660, UDF, ARXFS), and the in-kernel removable path removed | done |
| FSS6 | Network-served attachments, landing as `plans/SMB.md` SMB9a: the kernel-minted offer and its attach by the one principal it names, the presentations, owner and server detach, server-reported liveness, `MOUNT_LIST`'s owner and class, and the lanes that serve a multi-writer share concurrently and uncached | done |

## 0. Decisions (binding)

- **The kernel keeps the disk stack.** The block-service client, the shared
  device and its cache, the retained-write journal, the unplug transitions and
  the verified re-insert replay (`plans/DEVICES.md` D4) stay where they are. A
  filesystem driver reaches only its own volume's extent, through the volume
  block grant (§4): never a sibling partition, the partition table, or the
  device's shared window, which two processes could not share safely.
- **A filesystem driver is a discovered driver.** For each attached volume the
  kernel publishes a volume node under the block-service node `volmgr` was
  loaded for; `devmgr` matches it by `tairix,volume-<fstype>` and the kernel
  spawns that format's signed bundle (`drivers/filesystem/<fs>`'s `Run`)
  holding exactly the node's grants. A new format ships as a bundle with no
  kernel change. The driver holds `CAP_FS_SERVE`, the class capability every
  authority-conferring resource kind pairs with its per-object grant, and
  `CAP_LOG_EMIT`; the grant scopes `CAP_FS_SERVE` to one volume. The kernel
  creates its serving endpoint and data window for it at `volume_serve`, so
  it needs neither `CAP_IPC_BIND_PRIVILEGED` nor `CAP_SHM`.
- **Every removable format moves, ARXFS included.** The kernel keeps only the
  boot volumes' drivers: ARXFS for the root and `/System`, FAT32 for the boot
  partition it reads at boot.
- **Node-handle protocol, mirroring the driver traits.** Requests are the
  operations of `FilesystemRead`, `FilesystemWrite`, `FilesystemSecurity`,
  `FilesystemStats` and `FilesystemAttrs`: one definition of what a
  filesystem does. A `NodeId` keeps the drivers' contract — a pinned one
  names its node until its last unpin, an unheld one lives as the trait says
  — because the VFS keeps ids across operations (file identities in `stat`
  and listings, advisory locks, listing cursors). `Stale` means what it means
  from an in-kernel driver. The holds live in the server and die with it: a
  descriptor held across a lost server answers the stand-in's device fault,
  then stale once the restarted server serves the volume under a fresh
  registration. Setting a node's times arrives
  with D1264, which adds the trait method; declared timestamp limits with
  D1048.
- **Every call is bounded by the server's liveness**, not by a guess at its
  work: a driver waiting on its device is working, and the device's own
  deadline bounds each transfer, so a call is abandoned only when the driver
  goes `SERVED_IDLE_NS` neither answering nor touching its volume, or when its
  caller is killed; no wait on a server is unkillable. An abandoned call
  leaves the window **quarantined**: no request stages data in it or reads
  data out of it until a `Release` round trip proves the server has finished
  with every earlier request. An abandoned call on the server's holds
  (`Pin`, `Unpin`, `ReleasePins`) is not withdrawn, since the kernel counts
  every hold: it is waited out before the next call is posted, a pin the
  server turns out to have granted is let go, and a server once found stopped
  fails each later call at once until it shows life.
- **A server is a peer, not a driver.** Every reply is validated as untrusted
  input: lengths, kinds, names against the path grammar, counts against the
  request, sizes and times against their ranges. A malformed reply fails that
  call and marks the attachment degraded; it never reaches a caller.
- **A local served volume has one writer, the kernel**, so it keeps the
  kernel's cache, the directory-watch feed and per-mount serialisation exactly
  as an in-kernel driver: the proxy is one more `KernelFs` in the registry,
  under `PresentedFs` for the ownerless formats and `CachedFs` for all. The
  concurrent uncached entry a multi-writer share needs is FSS6's.
- **Write-back stays in the driver's process.** A served volume never reports
  a deadline to the kernel's flusher, and its registration is reaped inline —
  a descriptor's last release is handed over by the next operation on the
  volume, the close that dropped it, or a sync before it flushes — so a
  stalled server can never hold
  the flusher: the driver's commit window runs on a one-shot timer in its own
  loop (`lib/fsserve`), and every reply says whether the server holds
  unpublished writes, which the unplug classification counts as lost.
- **Presentation narrows, never widens.** The kernel takes a volume's identity
  from its own probe (`lib/fsprobe`), never from the server. The server may
  ask for read-only, never for writes the device or the kernel refuses. A
  removable mount stays `noexec`, presented through the same template and
  `PresentedFs` map an in-kernel removable volume is.

## 1. The ABI (`lib/abi/src/fsserve.rs`) — FSS1

- One call endpoint per served volume plus one shared data window. Requests
  and replies travel inline; only file data uses the window: a `ReadAt` names
  the window span the server writes into, a `WriteAt` the span the kernel
  staged. Every frame carries a fixed header — version and operation, then a
  request's flags or a reply's server state and status — checked before any
  field is read; the call transport carries the length.
- Requests, numbered in the traits' order: `NodeInfo`, `Lookup`, `Parent`,
  `ReadAt`, `ReadLink`, `SeekRegion`, `ReadDir` (a batch from a cursor and
  the name it came from, bounded by the reply size), `Pin`, `Unpin`,
  `ReleasePins`, `Create` and `CreateLink` (each with the record the node is
  born with), `Link`, `WriteAt`, `Truncate` and `Punch` (each naming its
  node), `Remove`, `Rename`, `Flush`, `Security`, `SetSecurity`,
  `RecordsOwnership`, `Stats`, `GetAttr`, `SetAttr`, `ListAttr`,
  `RemoveAttr`, and `Release`. An allocating request carries its
  `AllocationClass`.
- Replies carry a `DriverError` code or the result, plus two state bits: the
  server holds unpublished writes, and the volume is in its reserve.
- The bind offer (`volume_serve`) carries the root, `NameMatching`,
  `NodeIdentity`, whether the driver keeps attributes, and whether it asks
  for read-only and why; the kernel's answer carries the endpoint, the window
  and its length.
- The C view of these records and the framing is `plans/CCOMPAT.md` CC1.2.

## 2. The server runtime (`lib/fsserve`) — FSS2

- Dispatch is pure: one request frame, the window, and a driver in, one reply
  frame out — host-testable and shared by every driver. Requests are
  validated as strictly as replies are kernel-side.
- The program (`program` feature): read the grant, describe the volume
  (`volume_ctl`) and open it as a block device over `volume_read` /
  `volume_write`, open the format (read-only when the kernel says so, its
  write-back bounded by the machine's memory and the process's pressure
  band), offer it through `volume_serve` — or refuse with the reason the format
  gave — then serve from one wait set: the endpoint, the memory-pressure
  notice, and the commit deadline the driver's `VolumeHost` reports as the
  wait's timeout, taken once when it fires so a failed publish cannot spin.
- A driver crate supplies only how its format opens (`ServedFormat`), so six
  drivers share one program; its `Run` binary is behind a `program` feature,
  because the kernel links two of the libraries for its boot volumes.
- The conformance suite runs the driver-trait contract over the real codec on
  the host against an in-memory volume and the real FAT32 driver, plus the
  hostile cases on both sides: oversized and truncated frames, wrong kinds,
  names outside the grammar, spans outside the window.

## 3. The proxy (`kernel/core/src/fs/served.rs`) — FSS3

- Implements the driver traits by encoding each call, posting it under a
  synthetic kernel identity, waking the server, parking on the reply until the
  deadline, and validating the answer. A read or write larger than the window
  is split; a short read is end of file. A listing replays the batch to the
  caller's visitor and asks for the next batch from where it stopped, keeping
  the stream contract.
- A kill while parked abandons the call (`DriverError::Interrupted`, which
  the VFS reports as `Errno::Interrupted`) and quarantines the window; a call
  on the holds is settled by the next call instead.
- Health: a deadline miss or a malformed reply marks the attachment degraded;
  the next answered call that needed no quarantine clears it.

## 4. The volume block grant — FSS4

- `HwResourceKind::Volume` names a kernel served-volume object: the volume's
  partition window over the shared device and its journal, its block size and
  count, and whether it may be written.
- `volume_read`, `volume_write` and `volume_ctl` read, write, flush, discard
  or describe the volume, synchronously in the caller's context, for a caller
  holding `CAP_FS_SERVE` whose grants cover the object: whole blocks within
  the volume, at most the window's length per call. Writes reach the journal
  like an in-kernel driver's.
- Retiring the object — detach, unplug, refusal — fails every later call
  closed.

## 5. Removable volumes — FSS5

- **Attach** (`volume_attach`, unchanged ABI): connect, window, recognise and
  re-insert as today; reserve the catalog name; create the served-volume
  object; publish the volume node. The mount appears when a driver serves it.
- **Serve** (`volume_serve`): the driver holding the node's grant offers the
  volume; the kernel creates the endpoint and window, builds the proxy stack,
  mounts it at `/Storage/<name>`, registers the device's health source and
  publishes the identity. A refusal retracts the attachment with the driver's
  reason audited.
- **Detach**: flush through the proxy, commit the device, retract the node,
  retire the object, unmount, unpublish — failing closed as today.
- **Unplug**: the D4 transitions, with the server's unpublished writes
  counted as lost; the node and object are retired with the device.
- **Server loss** (its endpoint vanished): the stand-in answers every call,
  the mount reports recovering, and the node stays published, so the driver
  `devmgr` restarts serves it again. A driver that refuses a conflicted
  re-insert leaves it mounted read-only behind the stand-in: its retained set
  is the audited force-unmount's to discard.
- **Re-insert**: the D4c replay at the block layer, then a fresh object and
  node; the mount keeps its stand-in until the new driver serves.
- The kernel no longer links ext4, ADFS, ISO 9660 or UDF.

## 6. Network attachments — FSS6

- **Offer.** `fsserve_offer` — `CAP_FS_SERVE` — takes a `NetworkOffer`: the
  `ServeOffer` (`FormatName::Smb3`), the uid it is for, a 16-byte volume key,
  the source the mount table shows (`//host/share`), and its lanes. As
  `volume_serve` does, the kernel mints the endpoint in the served range and
  the window, so the kernel is the endpoint's only caller, and answers the
  `ServeGrant` and an `OfferId`, never zero and never reused in a boot. An
  offer mounts nothing; it lives until attached, withdrawn, or its server
  exits. `netshare-v1`'s `Mount` answers it.
- **Lanes.** A multi-writer share is served by several *lanes*: each is the
  one-call proxy of §3 over the shared endpoint, owning its own span of the
  window, and the volume's `DriverLock` hands an operation any free lane, so
  no caller waits behind another's round trip. A local volume has one lane.
  The offer names the count and each lane's span, within the kernel's bounds
  (`SERVED_LANES_MAX`, a span a power of two up to `FSSERVE_WINDOW_LEN`), so
  the server sizes what one mount costs; detach takes every lane in order.
  `Release` names its span, since a concurrent server's answer to one span's
  release says nothing of another's.
- **Uncached.** Other writers make `CachedFs`'s one-writer model false, so a
  network attachment is registered without it: caching is the server's,
  under its own coherence protocol (SMB leases), and the volume has no
  directory-watch feed.
- **Liveness.** A network server touches no volume grant, so it shows life
  through `fsserve_ctl(offer, Alive)`, which it sends while calls it is
  working on are outstanding; a call is abandoned as in §0 once the server
  neither answers nor shows life for `SERVED_IDLE_NS`, and the caller stays
  killable throughout.
- **Attach.** `fsserve_attach(offer, attach)` is called by the principal the
  offer names, so every authority decision is made against the real caller;
  anyone else is answered `NotFound`. It names the volume (a base name the
  kernel suffixes through the shared volume-naming policy until one is free),
  the presentation, an optional narrowing to read-only, and an optional
  explicit target. The presentation is the identity map §0 applies to
  removable volumes, generalised: *private* for a caller without
  `CAP_FS_MOUNT` — owner the caller, its primary group, directories
  `rwx------`, files `rw-------` — and, for a holder, also *shared* (the
  storage group, `rwxrwx---`/`rw-rw----`) or *explicit*; in each every node is
  presented so whatever owner the server claims to record
  (`PresentedFs::whole`), and the write bits drop where the server marks a
  node read-only. Flags are `noexec` plus
  what the caller narrows. The volume appears at `/Storage/<name>`; an
  explicit target is a `view_bind` (`plans/NEW-NAMESPACE.md` NS-4a) made
  under the same call.
- **Identity.** A network volume's identity is derived by the kernel from the
  serving process's attested bundle identity and the offer's key, so a server
  can never claim a local volume's identity.
- **Detach.** `volume_detach` succeeds for the attachment's owner without
  `CAP_FS_MOUNT`, and takes the endpoint down, which is how the server learns
  the volume is gone; `fsserve_ctl(offer, Withdraw)` lets the server retire
  its own offer or attachment, after it has flushed what it holds.
- **Retirement.** A volume its server withdraws, or whose server dies, leaves
  the namespace without the kernel waiting on the server's own lanes, unless
  the server last said it held writes it had not published: that one keeps its
  mount behind the fail-closed stand-in, reporting `UnavailableLost`, until its
  owner detaches it with `force`. An attach whose explicit target is refused
  is undone, holds let go, and the offer stays to be attached again.
- The implementation is `kernel/tairix-kernel/src/network_volume.rs` (audit
  4300–4305) over `kernel/core`'s lanes (`ServedFs::lanes`,
  `DriverLock::lanes`), `LateFilesystem::register_network` and
  `volume_policy::PresentedFs`.
- `MountRecord` gains the owner uid and the volume's class
  (`VolumeClass::Network`).
