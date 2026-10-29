# Block drivers

Block drivers expose a fixed-size logical-block array to higher layers
(filesystems, swap, dump). They implement
[`tairix_abi::driver::block::Block`](../abi/driver_traits.md) and are
loaded as user-space drivers unless their manifest declares
`kind = "in-kernel"` (in which case they require `CAP_DRV_KERNEL`).

## Class trait

`Block` exposes these core method families:

| Method                         | Purpose                                       | Capability gate                |
|--------------------------------|-----------------------------------------------|--------------------------------|
| `geometry`                     | report `BlockGeometry { block_size, block_count }` | `DriverHandle` ownership |
| `medium`                       | report the `BlkMedium` the device is, which its class and I/O budget follow from | `DriverHandle` ownership |
| `device_name`                  | report the device's own short name, for a consumer that lists devices | `DriverHandle` ownership |
| `backing_availability`         | report what the device can promise a background consumer | `DriverHandle` ownership |
| `read_blocks` / `write_blocks` | bulk transfer (multiple of `block_size`)      | `DriverHandle` ownership       |
| `read_blocks_with_class` / `write_blocks_with_class` | classed transfer (see below) | `DriverHandle` ownership |

All methods return `Result<_, DriverError>`. Per `AGENTS.md` §2.9 the
class trait never panics — buffer length errors map to
`DriverError::BufferTooSmall` and out-of-range LBAs map to
`DriverError::LengthOutOfRange`.

Device-reported outcomes carry a **health axis** rather than collapsing to a
single fault. A block-service completion leads with a `blkio::BlkStatus`
word, and every consumer of a served device — the kernel-side `BlkClient`
and the volume manager's probe alike — classifies it through the one shared
`DriverError::from_errno` mapping (`lib/abi`), never a per-consumer copy
(`AGENTS.md` §2.2). The health classes stay distinct so the filesystem layer
can act on them: a permanent bad sector is `DriverError::MediumError`, a
present-but-unresponsive or surprise-removed device is
`DriverError::DeviceOffline` — including a `virtio_blk` request whose
per-request deadline elapsed with no completion, or whose wait could not be
made at all, and every later request until the device hands that request's
staging back, since it may still write the abandoned request's data into it —
a transient stall or a device/hub reset is a reissuable `DriverError::Busy`,
and a timed-out or vanished endpoint, a wake-storm on a stuck device line, a
`virtio_blk` read whose completion does not cover its data, and any
unclassified failure all fail closed as `DriverError::DeviceFault`. Because the
mapping is per-consumer-agnostic, a fault on one device surfaces only to that
device's callers while every other mount keeps running (`plans/FIX-IO.md`
IO1/IO2).

## The shared data window has exactly one user at a time

A served block device is reached over **two** things: a call endpoint that
carries the fixed-size `BlkRequest`/`BlkCompletion` frames, and a **shared
data window** that the payload bytes themselves are staged in. The request
frame names an offset and a length; the driver writes the read bytes into
that window, or reads the write bytes out of it. The window is a property of
the *device*, not of a consumer: one node, one window.

That makes the window a **critical section**. Two transfers in flight on one
device would each stage bytes in the same buffer, so one would overwrite the
other's payload and a reader would be handed bytes belonging to a different
extent — silent corruption rather than a fault, and exactly the kind of defect
a checksum-less filesystem would never notice. The rule is therefore absolute:

> A device's data window has one user at a time, and every consumer of that
> device drives it through one client.

It is upheld structurally, never by convention:

- **In the kernel**, the runtime volume service connects a device **once per
  block-service endpoint** and shares that one client, read through one block
  cache, behind `SharedBlock`'s sleeping lock, so every volume on a disk — each
  partition's served volume, which its user-space filesystem driver reaches it
  through, and the detach-time device-cache flush — serialises whole operations
  onto it. An attach, recovery, serve or detach holds only its own device's
  lock, so a disk that stops answering holds up no other disk's volumes. A
  served volume holds an *owned* window (`OwnedBlockWindow`) because it long
  outlives the attach call that built it; the last
  window dropped closes the client and releases its window hold. When the
  device vanishes, every volume on it goes through the removal transition,
  and its cache drops what it held as soon as nothing holds the device.
  Sharing the client also gives a disk **one** health fold and one set of I/O
  counters rather than a divergent copy per mount, which is the honest reading
  anyway: health is a property of the device.
- **Across processes**, the volume manager and the kernel's mounts are ordered
  rather than interleaved. The manager probes the whole device, **drops** its
  transport, and only then asks the kernel to attach the volumes it found.
  Dropping the client consumes the window borrow, so it is the compiler — not
  a comment — that stops a later probe read from racing the mounts it just
  created.

A serving driver correspondingly resolves the window a request names and may
refuse a request naming a window it was never granted (fail closed); it never
assumes the requester is alone.

## What a device is, and how patiently it is served

How patient a consumer should be with a device is a property of the *device*: a
spinning disk may legitimately take tens of seconds to spin up or finish an
internal reset, while a paravirtual device that has not answered in seconds is
wedged rather than busy. Only the driver that binds the hardware knows what it
is, so it declares it:

- `Block::medium` reports a `blkio::BlkMedium`: the `BlkTransport` the device is
  reached over (`Ata`, `Nvme`, `Usb`, `Sd`, `Virtual`) and, as separate facts,
  whether its medium rotates (`BlkRotation`: `Unknown`, `NonRotating`, or
  `Rotating` at a nominal rate) and whether it can leave the unit
  (`BlkRemovability`: `Unknown`, `Fixed`, `Removable`). Each established fact
  carries the evidence that established it: the transport itself
  (`BlkMedium::of_transport` — an `NVMe` namespace neither rotates nor loses its
  medium, an SD card does not rotate), the Block Device Characteristics VPD
  page, IDENTIFY DEVICE word 217 through ATA PASS-THROUGH, INQUIRY's RMB bit,
  or a medium-not-present answer. A fact nothing established stays `Unknown`;
  nothing downstream is told more than the hardware reported. The trait default
  is `None`, a driver that says nothing about its device. Reporting the medium
  is a pure observation: it touches no hardware and cannot fail.
- Two things follow from the medium, each reading an unknown fact in its own
  conservative direction:
  - **Patience**, `BlkMedium::class`: a `Virtual` transport is `Virtual`; a
    medium that rotates, *or is not known not to*, is `Rotational`, the most
    patient class, so a disk spinning up is never failed for a guess; a
    non-rotating medium is `Removable` on USB and SD and `SolidState` on a
    direct bus. `BlkDeviceClass::of` (behind `BlockClass::device_class`) serves
    an undeclared device the bounded `Virtual` envelope, so a driver that says
    nothing still fails closed promptly when it wedges.
  - **Command cost**, `BlkMedium::command_cost`: what one command costs, which a
    filesystem's write-back window amortises (`lib/writeback`). A USB or SD unit
    is `Bridged` whatever its medium, because the bridge's fixed cost dominates
    every command; a rotating or unknown medium on a direct bus is `Seek`; a
    non-rotating one, or a paravirtual device, is `Cheap`. An undeclared device
    is `Cheap`, the window holding the least unpublished work.
- `BlkDeviceClass::budget` gives the class's `IoBudget`, the one policy the
  serving driver and every consumer read. Only its `CommandBudget` — the
  allowance of a media command, of a query the unit answers without its medium,
  and of a flush, the time a unit may take to come ready, and the slowest
  throughput a healthy device sustains — and its retry count and queue depth are
  chosen. The rest derive from them: a driver bounds each exchange by
  `CommandBudget::exchange_ns` (the command kind's allowance plus its transfer
  at that throughput); the recovery grace window is twice the longest command,
  so a command that fails and its reissue both fit; and a consumer's
  per-request deadline is the ready budget plus one grace window for every
  answer the request may wait behind — the whole queue for a class served one
  request at a time, one for a class served in parallel.

  | Class        | Media | Query | Flush | Ready | Throughput | Retries | Depth | Deadline |
  |--------------|-------|-------|-------|-------|------------|---------|-------|----------|
  | `Rotational` | 30 s  | 10 s  | 60 s  | 60 s  | 1 MiB/s    | 3       | 2 (serial)   | 300 s |
  | `Removable`  | 10 s  | 5 s   | 20 s  | 10 s  | 1 MiB/s    | 3       | 2 (serial)   | 90 s  |
  | `SolidState` | 5 s   | 2 s   | 10 s  | 10 s  | 32 MiB/s   | 2       | 32 (parallel) | 30 s |
  | `Virtual`    | 10 s  | 10 s  | 10 s  | 10 s  | 8 MiB/s    | 2       | 16 (parallel) | ≈30 s |

- The record also says what **form** the medium takes (`BlkForm`:
  `Unspecified`, or `OpticalDisc` for a disc in a multimedia drive), which is
  how a consumer tells a disc from a disk: the volume manager attaches a disc
  whole, and the desktop draws it with the optical-disc icon.
- A disc's **layout** — where its last session starts and the last block it
  recorded, as far as the drive can say (`Block::disc_layout`,
  `DiscLayout`) — rides the geometry completion beside its size
  (`BlkCompletion::disc_layout`), validated against the block count on
  decode — a layout past the device's end, or a last session starting after
  the last block recorded, reads as none — so a relaying layer carries it as
  faithfully as the geometry. A geometry frame bearing a flag this build does
  not define, or layout fields in a frame that declares no layout, is
  decoded as a fatal completion rather than read as partly meant. The
  ISO 9660 and UDF readers start from the last session, and UDF finds a
  write-once disc's virtual allocation table at its last recorded block.
- The medium travels to the consumer in the **geometry completion**
  (`BlkCompletion::medium`) as one 8-byte record (`BlkMedium::encode`) — the
  same record the mount table (`MountRecord::medium`) and the hardware tree
  (`HwProperty::BlockMedium` on the node the driver publishes) carry — and the
  consumer adopts it for every later request. Until the device answers, the
  geometry probe runs on the bounded undeclared envelope, so an endpoint that
  never answers is not granted a spinning disk's patience on hope.
- The serving driver is untrusted, and the medium needs no trust: it selects
  only how patient a consumer is with that one device and which picture a reader
  is shown, grants no authority, and is bounded by the widest class budget. A
  record this ABI does not define, or an inconsistent one (a rotation the
  transport "established", a fixed medium an absent one "established"), decodes
  to the one undeclared `None` rather than being believed, so a relaying layer
  cannot be made to carry a value of the driver's choosing — and it never
  discards an otherwise well-formed completion.
- `Block::device_name` reports the device's own short name, declared by the
  driver for the same reason the medium is: a surface listing storage devices
  otherwise has only the volumes on one to name it by, which names a
  filesystem, not a disk. It travels in the same geometry completion
  (`BlkCompletion::name`) and reaches a reader on the ungated
  `VOLUME_IO_STATS` record. A `BlkDeviceName` admits only printable non-space
  ASCII, refuses an over-long declaration rather than truncating it (a
  truncation could give two devices one name), and resolves anything else to
  the unnamed device, so an untrusted driver can neither forge an identity that
  means anything nor put an escape sequence on a reader's screen.
- **A device that wraps another reports what it wraps.** A partition window, a
  block cache, the kernel's disk-sharing boundary, the retained-writes journal,
  and the remote block clients forward their inner device's medium and name, so
  the real hardware survives every layer above it. A device *composed* of
  several (a RAID array) declares the medium of the live member it must wait on
  longest (`BlkMedium::of_members`): a mirror of an SSD and a spinning disk
  reads as the spinning disk and is served its spin-up budget, and when that
  member declared nothing the array reads as undeclared rather than as a medium
  nobody declared.

### A unit coming ready is held, not failed

A unit spinning up, or one that needs an initializing command, is neither
faulty nor busy: it will serve once it is ready. A driver says so with
`DriverError::NotReady`, and the shared serving engine
(`serve_request_recovering`) answers `Answer::Hold` instead of framing a
completion: no health is folded and no reissue is spent. The driver keeps the
request, tests the unit on a one-shot timer, and serves the request once the
unit is ready — or answers it through the unit's health when the unit is given
up on. The consumer's deadline already covers the class's ready budget, so a
held request is answered inside it. Wherever `NotReady` reaches a caller
instead it reads as the reissuable `Errno::WouldBlock`.

### A changed medium is never served to the old consumer

A unit's endpoint outlives its medium: a card leaves a reader and another is
inserted, and the unit's channel stays where it was. So every publication of a
unit carries a **binding** (`BlkPublication::binding`), which the geometry
completion hands the consumer (`BlkCompletion::binding`) and every later
request repeats (`BlkRequest::binding`). The engine refuses every request but a
geometry query whose binding is not the current publication's as
`Errno::Stale`, before the device is touched, so a consumer of the old card can never be served the new
card's blocks. A driver that publishes once uses the zero binding; one that
re-publishes draws each binding from the monotonic clock, which no two
publications on the machine share — not even across a reloaded driver. The
retracted node also revokes the grants consumers inherited through it, and its
retraction tells the kernel's volume service, which withdraws the volume that
medium was attached as even though the unit's endpoint lives on; the next
medium is attached afresh under its own binding, through the data window its
own publication conferred. The service marks the retracted publication's
device slot — the endpoint and that window — rather than the volumes it can
see, so a volume an attach or a recovery was still building over it is
withdrawn the moment that operation lets go, however the two raced, and a new
attach naming the retracted window is refused `Errno::Stale` before its client
is asked anything.

## What a device can promise a background consumer

A layer that verifies, discards, or rebuilds its own storage in the background
faces a question no per-request status answers: *is the thing underneath me in
a state where discretionary I/O is appropriate at all?* A degraded or
rebuilding array's bandwidth belongs to its own rebuild, a device inside its
recovery grace window must not be handed reads it did not have to serve, and a
discard — destructive and irreversible — must never be issued to a fault domain
whose state is in doubt. **Restoring redundancy below outranks verifying
above**, and asking is the only way a layer can know.

`Block::backing_availability` is that question, answered in the shared
`MountAvailability` vocabulary rather than a second one:

- The **default is `Available`** — the honest answer for a plain device with
  nothing composed beneath it.
- A device that **wraps** another forwards the inner answer: a partition window
  (`tairix_partition::PartitionBlock`), the kernel block cache, the retention
  journal, a `&mut B` adapter. A window that inherited the default instead would
  tell a filesystem on a partition that its backing is whole while the composed
  device beneath is rebuilding.
- A device that **composes** others folds its own state with theirs through
  `MountAvailability::worse_of`, so the worst answer anywhere in the stack is
  what the top layer acts on. A RAID array folds its `ArrayHealth` with its live
  members' answers (`tairix_raid::health`): a member that is still in sync while
  *itself* degraded or riding out a recovery window leaves the array optimal, and
  that is exactly the case the query exists to surface.
- A **block-service client** (`BlkClient` kernel-side, `RemoteBlock` in
  userland) reflects the health status each completion carried, through the one
  shared `MountAvailability::from_block_status` mapping. There is no second state
  machine: the serving driver owns the sticky one and its grace window, and the
  client mirrors its per-request verdict.

The query governs *discretionary background I/O only*. A user's read or write is
unaffected, reporting the state touches no hardware and cannot fail, and it needs
no capability beyond the handle that already reaches the device — it reports a
state the caller's own device already has.

### Crossing the block-service seam

A composed array serves a read perfectly well while short of redundancy, so a
consumer told only "the transfer succeeded" would mount it and report a clean
bill on sand. The shared serve engine therefore asks the served device what it
can promise and reports that as the success status
(`BlkStatus::for_backing`): `Available` answers `BlkStatus::Ok` exactly as
before, and anything less answers `BlkStatus::Degraded` — the only honest word
for a serving-but-unwell device, since the data *is* valid and every other
non-`Ok` status either says the payload cannot be consumed or invites the
consumer to reissue a good answer. The consumer's mount overlay then reads the
volume as at-risk and audits the edge, with no extra plumbing.

## Health state machine and the recovery grace window

A device that stalls, resets, or has its bus glitch is far more often only
*briefly* unwell than terminally dead, so a serving driver rides such a blip
out for a bounded **grace window** before failing it closed rather than
punishing the first missed beat (`plans/FIX-IO.md` IO3, `AGENTS.md` §26.5).
The policy and mechanism live in one shared place both a serving driver and a
consumer read (`blkio::BlkHealth`), never a per-driver copy (`AGENTS.md`
§2.2):

- `BlkHealth::observe(raw, now_ns)` folds each device-level outcome into an
  explicit `BlkHealthState` (`Healthy` → `Degraded` → `Recovering` →
  `{ Healthy | Faulted }` → `Offline`/`Removed` → `Failed`) and returns the
  `BlkStatus` the consumer is told. It is pure and event-timed: the caller
  supplies the monotonic `clock_get` reading, so there is no timer to spin on
  (`AGENTS.md` §2.23) and the whole machine is proven host-side.
- **Inside the window** a transient stall/reset is answered with a reissuable
  `BlkStatus::Reset` and the device is held `Recovering`, so a blip that
  resolves in milliseconds is invisible to the workload. The reply is
  reissuable *within its own per-request deadline* rather than parked, so one
  device's blip never stalls the serve loop's other units (head-of-line
  freedom, §26.1).
- **The driver says when to reissue.** A reissuable answer carries its serving
  driver's `BlkReissue`, which the engine asks of the device
  (`Block::reissue`) right after the failure: `Now` once the device's own
  recovery from it has completed and the unit answered ready, so the consumer
  reissues at once; `Paced` otherwise, and the consumer waits out the class's
  pacing. Both consumers make that one decision through
  `IoBudget::reissue_wait_ns`. The word only brings a reissue forward — it
  never lengthens a wait or buys one past the class's budget — so an untrusted
  driver can neither stall a consumer with it nor make one storm its device.
- **When the window elapses** without the device coming back the device goes
  `Faulted` and only *then* fails closed (`BlkStatus::Offline`). A device that
  has faulted stays quarantined until it *demonstrably* answers again, so a
  flapping disk cannot masquerade as healthy — yet a genuine return always
  recovers it to `Healthy` with no reboot (`AGENTS.md` §18.4).
- **A quiet device still expires its window.** `observe` advances the window
  when a request outcome arrives, but a device that stalls and then goes silent
  would otherwise sit `Recovering` forever. `BlkHealth::grace_deadline_ns`
  returns the absolute monotonic time the window closes (a driver arms a
  **one-shot** timer for it, never a busy-poll — `AGENTS.md` §2.23), and
  `BlkHealth::poll(now_ns)` is the pure, event-timed transition that fails a
  still-`Recovering` device closed to `Faulted` when that deadline passes with
  no further request. `observe` and `poll` share one `grace_elapsed` check so
  the request-driven and time-driven paths cannot diverge (`AGENTS.md` §2.2).
  A serving driver that parks between requests arms this from
  `tairix_abi::blkio::recovery_wait_timeout` — the soonest armed grace deadline
  across every unit it serves, relative to now — as its wait's one-shot
  timeout, so it wakes exactly when the nearest window is due and drives
  `poll`. The arithmetic is that one shared helper, never copied per driver
  (`AGENTS.md` §2.2).
- Only a *device-level* outcome drives health. A request-level rejection (a
  write to a read-only unit, an out-of-range LBA, a malformed frame) is
  classified `BlkStatus::for_driver_health(err) == None` and framed verbatim,
  so a hostile or malformed request can never drive a healthy device toward
  `Faulted`.
- The grace duration is **per-device-class policy** (`IoBudget::grace_ns` from
  `BlkDeviceClass::budget`), sized wider than the per-request deadline so a
  single reset/spin-up cannot exhaust it — a rotational disk's spin-up budget
  is not an SSD's. It is scaling policy, never one global `const` (`AGENTS.md`
  §24.1) and never a security/validation bound (§24.4).

The request engine is **one shared definition** every block driver reuses:
`tairix_abi::blkio::serve_request_recovering` decodes and validates a request,
drives the device through the `Block` trait, folds the outcome into a
`BlkHealth`, and frames the completion — so the validation, the fail-closed
refusals, the success paths, and the recovery grace window cannot diverge
between drivers (`AGENTS.md` §2.2, §27). It is pure and alloc-free, proven
host-side over in-memory `Block` doubles in `lib/abi`. `usb_msd` is the first
consumer: its wait-set serve loop hands each per-LUN request to the engine with
that LUN's `BlkHealth` (the `Removable` class) driven by the monotonic clock,
and arms its wait's timeout from `recovery_wait_timeout` so a LUN that stalls
and then goes quiet still has its grace window expired (`BlkHealth::poll`) and
fails closed on time — logged once, keeping the LUN's node and endpoint so its
consumer still receives typed fail-closed answers and a later genuine return
recovers it with no reboot (a health fault is not a surprise-removal, so the
node is not retracted). Only the usb_msd-specific block-service endpoint-id
derivation (`serve::blk_block_for`) lives in the driver crate. `virtio_blk` and
`emmc2` are currently consumed in-kernel (root-unlock) and expose only their
`Block` implementation; when either is brought up as a user-space serving
process it reuses the same engine and the same idle-timer helper rather than
copying them. Even in that in-kernel form each driver's `Block` already maps a
raw device status to the *honest* per-request health class rather than a blanket
`DeviceFault`: `virtio_blk` decodes a `virtio_blk_req` status byte through one
shared `status_to_result` (`VIRTIO_BLK_S_IOERR` → a per-request
`DriverError::MediumError` the consumer recovers around and repairs, not a
whole-device fault; `VIRTIO_BLK_S_UNSUPP` → `DriverError::Unsupported`; any
undefined status → a fail-closed `DriverError::DeviceFault`, never the benign
`Unsupported`), so the health axis is correct at the source and no consumer
drops a whole device over a single bad sector. Device-level health
observability through `lib/log`/`sysinfo` beyond the per-volume mount overlay is
the staged remainder (`plans/FIX-IO.md` IO3–IO6).

## The recovery-escalation ladder — what the *driver* does behind a blip

The grace window decides *what a consumer is told* while a device rides out a
blip; the **recovery ladder** decides *what the driver does to the hardware*
between the reissued attempts, so a stalling device is actively nudged back
rather than merely waited on. It is a second shared primitive,
`tairix_abi::blkio::RecoveryLadder`, owned per served logical unit alongside
that unit's `BlkHealth`:

- `RecoveryLadder::next_action(state)` is the single entry point mapping the
  unit's current `BlkHealthState` to the next `RecoveryAction`. An operational
  device (`Healthy`/`Degraded`) yields `None` and re-arms the ladder; a
  `Recovering` device **escalates** — the first attempt is a gentle `Retry`
  (a one-off comms glitch often clears itself, and a reset would only add
  latency), and each subsequent attempt is a data-path `Reset`; once the
  class's `IoBudget::max_retries` is spent it is `GiveUp` and the grace window
  is left to fail the device closed on time. A device already failed closed
  (`Faulted`/`Offline`/`Removed`/`Failed`) is `GiveUp`, so the driver stops
  escalating; a later genuine answer returns it to `Healthy` and re-arms the
  ladder.
- The ladder's cap is the **same** per-class `IoBudget::max_retries` the
  consumer's `IoBudget::should_reissue` reads, so the driver's escalation and
  the consumer's reissue budget derive from one policy and cannot drift apart
  (`AGENTS.md` §2.2). A device that keeps stalling therefore climbs a *finite*
  ladder and is never reset forever (`AGENTS.md`'s ban on
  retry-until-it-works).
- The ladder holds no clock or timer and never spins or parks: it advances one
  rung per reissued attempt, and reissued attempts are already spaced by the
  consumer's own reissue cadence — at once only after the driver found the unit
  ready — and per-request deadline. That is *stronger*
  than a driver-side backoff timer for head-of-line freedom (`AGENTS.md` §26.1),
  since the serve loop never sleeps on one recovering device while its siblings
  wait, and it keeps the whole ladder provable host-side.

`usb_msd` is the first consumer: after replying to each request its serve loop
consults the LUN's ladder from the just-folded `BlkHealth` state and, on a
`Reset`, resets the transport (`ScsiDevice::reset_transport`: BOT's reset
recovery, CBI's Command Block Reset with the bulk clears, UAS's four pipes
cleared) and logs an `MSD_RECOVERY_RESET` audit event. The reset is only ever issued for a unit already being answered
reissuably, so it cannot stall an unrelated LUN. Which concrete mechanism a
`Reset` maps to is per-driver (a virtio/NVMe driver re-inits its queue); an
action a driver's hardware cannot express is a no-op that still advances the
ladder, so the escalation is honest on every transport.

## Consumer-side bounded reissue

The grace window is the *serving* half of the reply-reissuable model; the
*consumer* half is a bounded reissue. When a serving driver rides a blip out it
answers within the request's own deadline with a reissuable status
(`BlkStatus::TransientError` / `Reset` / `Timeout`) rather than a hard fault, so
a consumer that simply surfaced the first such reply as an I/O error would
punish a device that was merely recovering. Instead every consumer of a served
block device — the kernel-side `BlkClient` (`kernel/core`) and the volume
manager's probe (`RemoteBlock`) — reissues a reissuable completion a bounded
number of times before failing closed:

- The retry count is the shared per-class policy `IoBudget::max_retries`, read
  through the one definition `IoBudget::should_reissue(status, attempts)` both
  consumers call, so they can never drift apart in when they retry versus fail
  closed (`AGENTS.md` §2.2). A device that keeps answering reissuably still
  fails closed deterministically at the budget rather than retrying forever
  (`AGENTS.md`'s ban on retry-until-it-works).
- Each reissue first parks the consumer out the class's pacing
  (`IoBudget::reissue_pacing_ns`: the grace window spread evenly over the
  class's reissues — `sleep::park_until` in the kernel, `tairix_rt::park_ns` in
  a driver), so the device has its whole window to recover in rather than
  answering every reissue at once, and is then a fresh post → park-on-reply
  exchange: event-driven, never a busy spin. The serving driver owns the
  recovery grace window and its timers; the consumer only honours the
  reissuable reply.
- A hard per-request deadline timeout (synthesised kernel-side when the driver
  never answers) and a torn-down endpoint fail closed with **no** reissue: a
  device that consumed its whole deadline without answering is treated as
  wedged, not retried. A non-retryable verdict — a `MediumError` bad sector, a
  gone `Offline`/`Removed` device, a `Stale` refusal because the medium it was
  reading has changed — is surfaced on the first attempt.

## Fault domains — one hub/controller blip is one recovery episode

A bus, hub, USB controller, SAS/JBOD expander, or PCIe root complex owns a
group of block devices beneath it. When such an *owner* resets or blips, the
symptom on every disk below it is the same stall — so treating it as N
independent disk failures is wrong: it is **one** fault-domain event
(`plans/FIX-IO.md` IO4). `blkio::FaultDomain` is the interior-node counterpart
of the per-device `BlkHealth`, and both drive their recovery window through the
one shared `GraceWindow` timer, so an interior node and a leaf device time
their grace window identically and the arithmetic cannot diverge (`AGENTS.md`
§2.2).

- Which nodes are children is read from the discovered hardware tree
  (`lib/abi::hwtree`), never hard-coded — a USB hub, a SAS expander, and a PCIe
  root complex are all just interior nodes (`AGENTS.md` §18.1, §2.20). A
  `FaultDomain` stores only the owner's opaque node id, so the type stays
  platform-neutral.
- `FaultDomain::quiesce(now_ns)` opens **one** shared grace window over the
  whole subtree: every child's in-flight request is answered reissuably
  (`FaultDomain::child_status` returns `BlkStatus::Reset`), so a hub reset that
  resolves in milliseconds is invisible to the workload.
- `FaultDomain::resume()` records a *demonstrated* owner return: the whole
  subtree recovers to `Healthy` at once and children resume on their own
  per-device health. This is the only transition that clears a failed subtree,
  so a returning hub always recovers without a reboot (`AGENTS.md` §18.4).
- `FaultDomain::poll(now_ns)` fails a `Recovering` subtree closed to `Offline`
  when the window elapses, driven by the one-shot timer
  `FaultDomain::grace_deadline_ns` names rather than a busy-poll (`AGENTS.md`
  §2.23). A subtree that has failed closed is sticky until a demonstrated
  return, so a flapping hub cannot masquerade as healthy.
- The grace duration is **policy** the caller derives from the owner's
  discovered class (e.g. the widest child `IoBudget::grace_ns`), never one
  global `const` (`AGENTS.md` §24.1).

The `FaultDomain` machine is pure and event-timed (the caller supplies the
monotonic reading and drives the children's own `BlkHealth`), so the whole
coherent quiesce/resume is proven host-side in `lib/abi`.

Which interior node a device blips *with* is resolved by the shared
`hwtree::fault_domain_owner(nodes, node_id)` helper: it walks the discovered
hardware tree upward and returns the nearest strict ancestor that owns a group
of devices — a bus/hub/controller/expander/PCIe-root-complex
(`HwDeviceClass::Bus`), or the synthetic `Root` as the domain of last resort for
a device attached directly to it. It skips non-owning ancestors and fails
closed on an absent node, a rootless node, or a broken/cyclic chain (the walk is
bounded by the node count, never an unbounded spin, `AGENTS.md` §2.9). It reads
the tree and hard-codes no board (`AGENTS.md` §18.1, §2.20), so it is the one
definition every serving/bus driver uses to build a child's `FaultDomain`.

A device usually blips with more than one interior node — a disk on a hub on a
controller shares a fault domain with the hub *and* the controller *and* the
root. The **full ordered chain** of those nested owners, nearest first, is the
shared lazy iterator `hwtree::fault_domain_chain(nodes, node_id)`, built by
re-applying `fault_domain_owner` to each owner in turn — so a serving driver
builds one `FaultDomain` per interior node in the chain without re-deriving the
walk itself (`AGENTS.md` §2.2). It is allocation-free (it holds only a borrow of
the tree, so no fixed-depth ceiling, `AGENTS.md` §24.1), inherits
`fault_domain_owner`'s fail-closed behaviour at every level, and is cycle-safe:
bounded to at most one step per node, so even a malformed tree terminates rather
than spins. The chain is exactly the `domains` argument the two composition
helpers below consume.

Two pure composition helpers let a serve loop use those fault domains exactly
as it already uses the per-device machinery, without re-deriving the rules
(`AGENTS.md` §2.2):

- `blkio::fault_domain_wait_timeout(domains, now_ns)` is the interior-node
  counterpart of the per-device `recovery_wait_timeout`: the soonest armed
  subtree grace window, relative to now, so a serve loop parks on the nearest
  event and never leaves a quiesced-but-quiet domain `Recovering` forever. Both
  delegate to one shared `nearest_relative_deadline` core, so a loop that owns
  *both* per-device and fault-domain windows takes the min of the two and
  cannot compute them by different rules (`Some(0)` = poll now, `None` = park
  with no timeout, matching the `waitset_wait` convention).
- `blkio::effective_child_status(device_status, domains, now_ns)` folds a
  child's own outcome with what each ancestor imposes into the one status its
  completion carries, using `BlkStatus::combine`'s total order
  (`BlkStatus::severity`). A hub mid-reset turns a child's `Ok` into a
  reissuable `Reset` (its aborted data is not consumed); an ancestor whose
  window has elapsed fails the child closed to `Offline`; and a device's own
  definitive `MediumError` still wins over a concurrent reset — a bad sector is
  real and must not be retried into. The fold is associative and commutative,
  so a deeper failing domain can never be masked by a shallower healthy one,
  whatever order the chain is walked in.

`BlkStatus::severity`/`combine` are the single, explicit definition of "which
health signal wins" when more than one applies to one request, kept independent
of the wire value `BlkStatus::as_u32` so the transport encoding and the recovery
precedence can never silently couple. All of these are pure and proven
host-side in `lib/abi`.

A `FaultDomain` owner need not be a *bus* node in the tree: a leaf driver's own
shared transport that fans out to several logical units is equally a
fault-domain owner of those units. `usb_msd` is the first live consumer — every
LUN behind one USB mass-storage device shares one Bulk-Only pipe pair, so the
data-path reset it escalates is a transport-wide event. Its serve loop owns one
`FaultDomain` for that shared transport (owner = the device's own discovered URB
transport grant), `quiesce`s it around the reset, drives each LUN through the
per-request engine and folds the domain's verdict with `effective_child_status`,
`resume`s the whole device when any unit completes a real transfer, arms its
wait from the min of `recovery_wait_timeout` and `fault_domain_wait_timeout`, and
audits the device-wide edges through `BlkHealthTransition::for_fault_domain`
(`drivers/storage/usb_msd/src/recover.rs`). So one shared-transport blip is one
recovery episode across the device, not N spurious LUN failures.

The first live *interior hardware-tree node* consumer is the **xHCI host
controller** (`drivers/bus/usb/xhci`). The controller is the interior node every
USB device below it hangs from, so a controller-wide fault — a latched Host
System Error / HCHalted, or the `HCRST` reset the driver performs to recover — is
one recovery episode over the whole subtree. Its pure, host-tested
`domain::ControllerHealth` coordinator wraps one `FaultDomain` (owner = the
controller's own discovered URB endpoint-block base; grace =
`CONTROLLER_GRACE_NS`, matching the removable-storage window it sits above) and
the freestanding serve loop drives it around the controller reset: it
`begin_recovery`s on the first fault, arms its wait from `wait_timeout` and
retries on the grace one-shot (the fix for a faulted controller raising no
further interrupt, xHCI §4.24.1, which previously parked the loop forever),
`note_reset`s each attempt (recovering on a demonstrated return, failing closed
once the window elapses, when every interface node is retracted), and audits
the device-wide edges through
`BlkHealthTransition::for_fault_domain` (`HCD_DOMAIN_RECOVERING` /
`HCD_DOMAIN_RECOVERED` / `HCD_DOMAIN_OFFLINE`). A controller failed closed stays
sticky-but-recoverable — a later successful reset clears it — and is not retried
against forever.

An interior node's fault-domain state reaches the leaf block consumers beneath
it through the discovered hardware tree itself: `HwNode::fault_health` carries a
`FaultDomainState` on the wire, and an interior-node driver publishes its *own*
node's health with the `hw_node_health` syscall (`CAP_HW_EMIT`, audited; the
kernel resolves the caller's own matched node, so a driver can never forge
another's health). Recording it bumps the hardware-tree generation, so the
reactive `hw_tree_wait` observers re-read — the same channel the hotplug
emit/remove path uses, but a *distinct* signal: the node stays present, only its
health changes, so a merely-recovering subtree is never torn down. The xHCI
controller is the live emitter (each `ControllerHealth` edge → a
`Recovering`/`Healthy`/`Offline` publish), and it keeps its children published
across its own reset, retracting only those whose device did not come back
(`docs/src/drivers/bus.md`, "Controller recovery keeps the devices that come
back"), so one controller reset is one recovery episode across the subtree
rather than N teardown/reload cycles. The device manager therefore holds
nothing: a node id is never reissued, so a child that vanishes is gone for good
and its driver is unloaded at once, whatever its owner's health. The affected
volumes already surface as `Recovering` through the kernel `BlkClient`'s
existing `MountAvailability` fold as their leaf transports blip.

The remaining live wiring is the deeper nested-owner chains a hub or SAS
expander adds (`fault_domain_chain` + `effective_child_status`) and the QEMU
vertical that exercises the whole subtree recovery (`plans/FIX-IO.md` IO4–IO6).

## `BufferClass` and zero-on-free

`*_with_class` accept a `BufferClass` (`NonSensitive` /
`Sensitive`). Per `AGENTS.md` §4 a driver that bounces payload
through an internal staging area **must** scrub that staging before
the method returns when `class == Sensitive`. The default
implementations of the `_with_class` methods delegate to the plain
methods and are only safe for drivers that DMA straight into the
caller-owned buffer; drivers that bounce-buffer (such as
`virtio_blk` over the Stage 4 host-side allocator) override them.

The trait makes no guarantee about scrubbing the caller-owned `buf`;
that remains the caller's responsibility once it has consumed the
payload.

## Sharing one device across windows

The boot path brings up exactly **one** bootstrap-floor block device, yet two
independent consumers must read it during bring-up — the read-only signed
`/System` driver-store mount and the root-unlock window — and, under
Design D, the `/System` store must stay reachable for on-demand and reactive
(hotplug) driver loads (`AGENTS.md` §18.3 / §18.4). One disk must therefore
back two concurrent partition windows.

The kernel block-sharing layer (`tairix_kernel::shared_block`) is that
primitive. A `SharedBlock<B>` owns the brought-up device behind a scheduler-
blocking `SleepLock` and hands out `SharedBlockHandle`s, each of which is
itself a `Block`. Every operation takes the lock for the duration of one
device call, so concurrent windows on different CPUs are serialised
(`AGENTS.md` §4 — SMP from day one). The device's `BlockGeometry` is immutable
for the life of a disk, so it is queried once at construction and cached:
`geometry()` is then lock-free (`AGENTS.md` §2.16). A geometry fault at
construction refuses to wrap the device, so no handle is ever handed out for
an unusable device (fail closed, §2.9).

A **sleeping** lock is required, not a spin lock: a device call parks its
caller while it waits for the completion interrupt, so the lock is held across
a park. A spinning contender would then burn its whole quantum waiting on a
holder that is not running — or deadlock a single CPU outright — whereas the
`SleepLock` parks the contender and wakes it when the holder releases. The
layer is generic over any `Block` and names no device or architecture, so every
port shares the one definition (§2.2 / §2.20). The shared root-unlock tail
(`finish_unlock`) wraps the floor device a port brought up in a `SharedBlock`
and drives both the `/System` autoload and the interactive unlock through
concurrent handles rather than borrowing then moving the one device.

The floor's disk is additionally wrapped in a `MeteredBlock`
(`tairix_kernel_core::fs::blkmeter`) *innermost* — below the whole-disk block
cache, which is itself below the sharing lock — so the counters the three
per-volume `sysinfo` queries report are folded for a device that has no
serving block-service endpoint to fold them at, and folded from what actually
reached the medium rather than from cache hits that never did. It reports its
readings under a reserved `blkio::kernel_block_device` identity, disjoint from
the endpoint-id space by construction, and hands them to the driver-store
service so both boot-floor mount registrations — the read-only `/System`
volume and the writable root — attach the one fold to their volume.

Because *every* in-kernel device operation funnels through this one
implementation, it is also where a **burst** of them is paced. A caller
issuing operation after operation — a filesystem read walking a large file, a
service kthread draining requests — stays inside a single dispatched body for
the whole burst, and on a device fast enough that no operation ever waits for
its completion (an emulated virtio queue, an NVMe namespace whose completion is
already in the ring at the first poll) that body never returns to the dispatch
loop at all: the loop's housekeeping and heartbeats stop and everything else
runnable on that CPU waits for the burst to finish, which the lockup watchdog
reports as an in-kernel stall. `SharedBlockHandle::with_device` therefore
offers the CPU back to the dispatcher — `preempt::yield_if_owed`, see
`docs/src/architecture/scheduler.md` — *before* it takes the device lock, so a
burst gives up the CPU at most one operation after its quantum expires and
never leaves the device held by a task that is not running. Nothing is given up
unless the scheduler is owed a turn, so an uncontended burst pays no context
switch. Suspending here is sound by construction: the operation it wraps can
already park the same body in the same place waiting for a slow device.

`MeteredBlock` records an operation the device took 250 ms or more to answer
(`4075` `DEVICE_OPERATION_SLOW`: the operation, its block and length, how long,
and whether it succeeded) from the two clock readings its fold takes anyway,
so the fast path pays nothing. A window kept waiting for the shared device is
the sleeping lock's own record (`4076` `LOCK_ACQUIRED_LATE`,
`docs/src/architecture/sync.md`), naming `with_device` as the site. A slow
operation is the device or its interrupt; a late lock is a holder, the wake, or
the scheduler.

## The persistent driver-store service

Design D needs the `/System` driver store reachable for the life of the system
(on-demand and reactive driver loads, `AGENTS.md` §18.3 / §18.4), not only
during boot. `DriverStoreService<B>` (`tairix_kernel::shared_block`) owns the
boot disk's `SharedBlock` and hands out a fresh read-only window
(`SharedBlockHandle`) for each `/System` read.

The disk is the system's for life, so its bring-up leaks the device's backing —
the register map, the DMA pool and host, the IRQ waiter — to `'static`, and the
root-unlock kthread is a *never-returning* kernel service (`AGENTS.md` §17.1 —
"a continuous service never returns"): `finish_unlock` takes the brought-up
device by value and never returns, so the device outlives every window handed
out over it.

After running the boot autoload and the root unlock through two
concurrent windows, logging the outcome, and releasing the console-0 gate to
`login`, the service calls `DriverStoreService::hold`, which **parks** the
kthread for life owning the `SharedBlock` — a real park, never a busy-yield
loop (`AGENTS.md` §2.1), so it consumes no CPU while idle. A later reader (the
D2b `driver_store_load` path) wakes this kthread to serve a `/System` read
through a window and then re-parks, reusing the one proven I/O path rather than
driving the device from an arbitrary caller's context.

## Shipped drivers

| Driver                                   | Crate                                | Supported buses     | Status                                   |
|------------------------------------------|--------------------------------------|---------------------|------------------------------------------|
| [virtio-blk](./virtio.md)                | `tairix-drv-storage-virtio-blk`      | virtio (PCI / MMIO) | host-side tests + mock transport only    |
| Raspberry Pi 4 EMMC2                      | `tairix-drv-storage-emmc2`           | Pi 4 SDHCI (MMIO)   | UHS-I DDR50 / High Speed negotiation, ADMA2 + PIO, host-tested; wired into root-unlock; metal acceptance pending (Pi 4) |
| USB mass storage (BOT / CBI / UAS)        | `tairix-drv-storage-usb-msd`         | any USB host via the URB transport | shared SCSI layer + three wire transports (incl. UFI floppies) host-tested over scripted doubles; metal acceptance pending (Pi 4) |

QEMU integration on real PCI / MMIO virtio devices depends on the
prerequisites enumerated in `plans/WIRING.md` (kernel
DMA, IRQ routing, bus-handle hand-off).

### Discovery and the bootstrap floor

Every shipped block driver publishes a canonical `BIND_KEYS` table
(`AGENTS.md` §18.3) so a discovered hardware-tree node binds them by
match, never by a kernel guess (§18.5):

| Driver       | `BIND_KEYS` match key                         | Discovered node source                          |
|--------------|-----------------------------------------------|-------------------------------------------------|
| virtio-blk   | virtio device id `2` (`HwMatchKey::virtio(2)`)| a probed virtio node (PCI or MMIO transport)    |
| EMMC2        | `compatible = "brcm,bcm2711-emmc2"`           | the aarch64 `FdtDiscovery` Storage node         |
| USB MSD      | USB class `08:06:50` (`HwMatchKey::usb(0, 0, 0x08_06_50)`) | the mass-storage interface node the xHCI HCD emits |

The virtio-blk and EMMC2 drivers are part of the **bootstrap floor** (`AGENTS.md`
§18.6): the storage path must be up before the signed driver store under
`/System/Drivers/` is reachable, so the volume that holds the store can be
read. A floor driver declares itself in a `Floor.toml` beside its crate's
`Cargo.toml`, one quoted string per key:

| Key        | Value |
|------------|-------|
| `bring-up` | `virtio`: the port provisions the transport and the kernel opens the device as a virtio disk. `host`: the kernel lends the driver a host over its node's own resources and calls the crate's `floor::open`. |
| `isas`     | The instruction sets whose kernel holds it, space-separated. |
| `bind`     | The crate's `BIND_KEYS` restated for the build to sign, space-separated: `compatible:<string>@<priority>`, `virtio:<id>@<priority>`, or `pci:`/`usb:<vendor>:<device>:<class>@<priority>` with the vendor, device and class in hex. |
| `reason`   | Why it sits below the store, in one line. |

The kernel's `build.rs` walks `drivers/` for the declarations and generates
each target's `driver_catalog::IN_KERNEL_DRIVERS` table, so the floor is listed
nowhere by hand. An entry takes the crate's own `FLOOR_PATH`, `BIND_KEYS` and
`register`, and a `host` driver's `floor::open` and `floor::DMA_CARVES`, beside
the build-signed manifest; the build fails when the manifest's bind table is
not the crate's. The floor binds by discovery-match through the same shared
`lib/devmatch` policy the user-space `devmgr` uses — the in-kernel match and
the user-space match can never diverge (§2.2) — and is signature-verified and
capability-gated alike (§18.6). The floor only ever shrinks toward the store,
never grows.

### Raspberry Pi 4 EMMC2 (SDHCI)

`tairix-drv-storage-emmc2` brings up the Pi 4 (BCM2711) EMMC2 controller — an
Arasan / SDHCI 3.00 SD host — at the fastest bus the controller, the card and
the board drive, and exposes the card through `Block`.

**Bus speed.** Bring-up reads the controller's version, capabilities and
maximum current, and divides every SD clock from the base clock actually
feeding it: the firmware's EMMC2 clock where the platform reports one, else
the capabilities register's. It then negotiates down a ladder, each rung
proven by reading block 0 at its own timing:

| Rung | Signalling | Mode | Bus rate |
|------|------------|------|----------|
| UHS-I | 1.8 V | DDR50 (else untuned SDR50, else SDR25) | 50 MB/s |
| High Speed | 3.3 V | High Speed (else Default Speed) | 25 MB/s |
| Default Speed | 3.3 V | Default Speed | 12.5 MB/s |

UHS-I is attempted only where the board can switch the card's I/O rail *and*
cycle its power, because a card that has switched to 1.8 V returns to 3.3 V
only by losing power. On the Pi 4 both rails are lines of the firmware's GPIO
expander (`VDD_SD_IO_SEL`, `SD_PWR_ON`): the aarch64 discovery reads them from
the EMMC2 node's `vqmmc-supply` and `vmmc-supply` and states them on the node
as its I/O and power rails (`HwNode::io_rail`, `HwNode::power_rail`), with the
levels and the settling, start-up and off-on times the regulators declare, and
the driver switches them over the firmware channel the kernel lends it, which
answers it those two lines alone. Bring-up first selects 3.3 V, as a card
powers up; a board whose supply refuses even that runs the card at 3.3 V. A
failure in a rung's own steps steps
down, power-cycling the card first when it had left 3.3 V; a card that never
answers identification is power-cycled once and retried. The BCM2711 offers
DDR50 without sampling-clock tuning, so a UHS-I card runs DDR50; the driver
performs no tuning, so a mode needing it is never
chosen.

**Transfers.** ADMA2 moves up to 256 KiB per command through a data staging
area and a separate table of 64 KiB descriptors; the kernel host carves both
inside the node's DMA window — the `/emmc2bus` `dma-ranges` the firmware sets
per `SoC` stepping — and hands the controller bus addresses translated through
it. Bring-up keeps ADMA2 only once a DMA read of block 0 into staging filled
with the inverse of what the data port read there matches it; otherwise the
card is served through the buffer data port. Multi-block commands announce
their length with Auto-`CMD23` when the card's SCR offers it, else end with
Auto-`CMD12`. A data command whose R1 reports an error fails, and every write
is followed by `CMD13`, where the card reports a block it could not program.
A `Sensitive` transfer's staging copy is zeroed before the call returns.

**Waits.** Completions park on the controller's GIC line
(`CompletionWait::await_irq`); the intervals the SD specification mandates —
supply ramps, the 10 ms clock gate across the voltage switch, `ACMD41` paced
every 10 ms for up to one second — park on a timer
(`tairix_kernel_core::park_until`). Only the controller's reset and
clock-stable handshakes spin, bounded, and every wait fails closed with
`DriverError::DeviceFault`.

**Recovery.** A failed transfer resets the command and data lines, halting
the ADMA2 engine, and a multi-block one is aborted with `CMD12`. A controller
whose line reset never confirms may still master the staging, so it is handed
none again and the staging is withheld for the kernel's DMA quarantine. A card
whose state recovery could not prove is asked with `CMD13` before its next
data command, and aborted or awaited back into `tran` within three rounds.

**Diagnostics.** QEMU models no EMMC2, so the UART log is the metal signal,
and the driver records it on the log the kernel hands `floor::open`. A failed
bring-up logs `emmc2: bring-up failed` (`EventId(24941)`) with the
`BringUpStage` it stalled at and the `DriverError` as its `stage` and `error`
fields. A successful one logs `emmc2: link` (`EventId(24940)`) with `mode`,
`clock_hz`, `base_clock_hz`, `signal_mv`, `cmd23`, `dma`, and the stage and
error of any speed or DMA fallback, at `Warn` when either fell back. The
engine also reports each step it takes as a
`trace::Trace` record; no kernel host records them, and the bring-up tests
read them to check the steps.

The controller-reset clears SD Bus Power and the register block gates every
command on it, so bring-up powers the bus before the first command. The CSD
is decoded as the controller presents the R2 response — CRC stripped and
right-aligned — so `CSD_STRUCTURE` is read at `RESP3[23:22]` and `C_SIZE` at
`RESP1[29:8]`. Only SDHC/SDXC (CSD v2) cards are supported.

The kernel opens the card through the driver's `floor::open`
(`floor_mmio::bring_up_host_floor`). Having admitted the driver at the signed
load gate, it claims the node, binds and arms the controller's one wired GIC
line, carves the staging inside the node's DMA window from a
`CAP_MEM_DMA`-gated pool, and lends the driver a host that maps only the
node's own register window under `CAP_MMIO_MAP`, with the firmware channel
for the bring-up alone; the card then feeds the shared `finish_unlock` tail.
The kernel is the mailbox's only user then — the `vcmailbox` service is
loaded from the store this bring-up reaches — and drops the channel before
that store is served.

### USB mass storage (BOT / CBI / UAS) — `drivers/storage/usb_msd`

`tairix-drv-storage-usb-msd` is the first **discovered-tier, user-space**
block driver (`plans/DEVICES.md` D2/D5): a pure USB *class* driver `devmgr`
autoloads against the mass-storage interface node the xHCI host-controller
driver emits. It owns no register window, no DMA, and no IRQ — every
transfer rides the bus-agnostic URB transport (`lib/usb`), so the same
binary serves a disk behind any host controller that speaks it.

The driver reads the device's own configuration descriptor to derive the
interface number, wire transport, command set, and endpoints (never
assumed), then drives one transport-neutral SCSI command layer
(`src/scsi.rs` — the transparent set, or UFI's 12-byte padded CDBs and
`MODE SENSE(10)` for floppies) over the transport the device speaks:

- **Bulk-Only Transport 1.0** (`08:06:50`, `08:04:50`): each command
  wrapped in a CBW on bulk-OUT, the data phase over the bulk pair in
  bounded chunks, and the CSW validated field by field (signature, tag
  match, residue bound, status) — the device is hostile input. A stalled
  data phase falls through to the CSW; a stalled CSW read is retried once;
  a transfer that fails or outruns the exchange, a tag mismatch, a corrupt
  CSW, or a phase error runs the spec's reset recovery — the Bulk-Only Mass
  Storage Reset, then both bulk pipes' halts cleared — and fails the
  command.
- **Control/Bulk/Interrupt 1.1** (`08:04:00`, the classic USB floppy):
  the 12-byte command block over the ADSC control-OUT data stage (a
  control STALL is the device's "command not accepted" answer, recovered
  in place by the URB layer), the data phase over the bulk pair, and the
  two-byte command-completion interrupt (UFI ASC/ASCQ, or the typed
  status spelling for non-UFI sets); a failed transfer or a malformed or
  out-of-step completion runs the spec's Command Block Reset and clears both
  bulk pipes. A UFI failure's
  ASC/ASCQ is read **in-band** from that completion interrupt (like UAS
  autosense), so the command layer never issues a separate `REQUEST
  SENSE` — a real UFI floppy does not answer one reliably, and depending
  on it aborted floppy bring-up on hardware.
- **USB Attached SCSI** (`08:06:62`): the four Pipe-Usage-named bulk
  pipes with tag-checked Command / Read-Ready / Write-Ready / Sense IU
  sequencing (USB 2.0 non-stream operation) and in-band autosense; every
  IU is validated fail-closed — a foreign tag, wrong-direction ready IU,
  or lying sense length refuses the exchange, and a broken exchange clears
  all four pipes. One command is in flight at
  a time (the block service is synchronous); queueing, task-management
  IUs, and SuperSpeed streams are the staged remainder (`plans/DEVICES.md`
  §3).

Every exchange is bounded by its own command budget
(`CommandBudget::exchange_ns`): a media command, a query, or a flush, plus its
transfer at the unit's slowest throughput, for the class the unit has been
learnt to be — so a disk spinning up on a read is given its spin-up, a TEST
UNIT READY is not, and a device that stops answering fails the command rather
than holding the driver. A command a reset ended is answered reissuably —
`TransientError` once the transport is back in step, `Timeout` when the device
would not answer its own reset — so the unit's health rides it out in its grace
window rather than failing it at once.

Per logical unit (`GET MAX LUN` for BOT, `REPORT LUNS` for UAS, exactly one for
CBI; up to 16) the driver runs `INQUIRY` (a direct-access disk or a multimedia
drive is served, any other type skipped), then
brings the unit up on its own one-shot timers inside the serve loop, so one disk
spinning up never holds back its siblings:

- **Readiness.** TEST UNIT READY's answer is classified by its sense (key,
  ASC, ASCQ): ready; coming ready (02/04/01, re-tested every second); needing
  an initializing command (02/04/02, sent START STOP UNIT with IMMED once);
  needing a person (02/04/03, given up on); no medium (02/3A); a medium change
  (06/28) or another unit attention (consumed, re-tested at once, a few in a
  row at most). A unit that does not come ready within its class's ready
  budget is given up on and logged with its reason.
- **What the unit is.** Once ready it is probed, each probe sent once per
  medium and a refusal reporting nothing: the supported VPD pages, Block Limits
  (which caps the bytes one command moves), the Block Device Characteristics
  page's rotation rate, else IDENTIFY DEVICE word 217 through ATA
  PASS-THROUGH(16). INQUIRY's RMB bit says only whether the medium can leave
  the unit — many bridges set it for a fixed disk — so it never decides how
  patiently the unit is served. The unit's command budget follows the medium
  learnt.
- **Geometry and write policy.** `READ CAPACITY(10)`/`(16)` with a fully
  validated geometry (power-of-two block size 512–4096; the 16-byte form
  covers units past the 32-bit LBA horizon), and the command set's
  write-protect bit — enforced driver-side (`DriverError::PermissionDenied`
  before any byte reaches the device), not merely reported. A unit ready to a
  test whose geometry or write policy cannot be read is re-tried within its
  ready budget, then given up on.
- **Optical drives.** A multimedia unit (peripheral type `05h`, over BOT, UAS,
  or an ATAPI drive's 12-byte packets, `08:02:50`) serves its disc as an
  `OpticalDisc` and asks the disc what it is rather than probing a disk's
  pages: GET CONFIGURATION's Random Writable feature decides whether it is
  written, READ TOC format 1 gives the last session's start, and READ DISC
  INFORMATION and READ TRACK INFORMATION its last recorded block. A drive that
  refuses any of these, or breaks the exchange of one (which ends the
  queries), reads as a pressed, single-session disc; a disc not yet ready, or
  that left or changed, fails the bring-up so it is read again, and another
  unit attention is asked past once. A session starts within what the disc
  recorded, and a last recorded address outside its own track is not
  believed. The mode page comes from MODE SENSE(10). MEDIUM ERROR, BLANK
  CHECK and HARDWARE ERROR fail the one request, never the unit.
- **Empty and rejected units.** A unit with no medium — a card reader's empty
  slot — is present, not failed: it is tested for a medium every two seconds
  and published the moment one arrives. A removable unit whose medium was
  given up on is not brought up again until that medium leaves or changes;
  a fixed one is refused. A device none of whose units answers INQUIRY as a
  disk fails its bring-up.

A media command the unit answers with a unit attention is sent once more; one
answered RECOVERED ERROR was carried out, and ABORTED COMMAND or a check
condition naming no cause is answered reissuably. A reset attention on a unit
whose medium can leave is read as a possible change of medium unless a reset
this driver's transport ran — for any command, a probe or disc query included —
is still owed. One
it answers "coming ready" is held (see above) while the unit is tested on its
timer: its endpoint leaves the serve set, so the calls queued behind it wait in
the kernel rather than waking the loop. One it answers with no medium, or a
changed one, retracts the unit's node — revoking the grants its consumers
inherited — and the next medium is published afresh under a new binding.

Each ready LUN is published as a **storage-class hardware-tree node**
(compatible `tairix,usb-msd-lun`) carrying two grants — a block-service
call endpoint and a 32 KiB shared data window — and its medium record.
Once every unit present at attach has been published, found empty, or given
up on, the driver settles its subtree, so a unit published later is a hot
arrival on a running machine. Consumers drive the unit
with the fixed-frame `tairix_abi::blkio` protocol (`BlkRequest`:
geometry / read / write / flush; completions carry the geometry and the
read-only flag) — the same request-reply IPC shape as the URB transport,
served by the driver's wait-set loop (never a busy-poll). Each LUN carries a
per-unit `blkio::BlkHealth` of its medium's class, so a transient
device stall or bus reset is ridden out through its recovery grace window —
answered reissuably while the unit is `Recovering` — and only a unit that
stays unwell past the window is failed closed to its consumers, while the
other LUNs and every other mount keep running (`plans/FIX-IO.md` IO3). A
hot-unplug surfaces as the URB endpoint vanishing: the driver retracts its
LUN nodes and exits cleanly so a re-plug re-enumerates and reloads it. A
serving removable unit is tested every two seconds (`MEDIA_POLL_NS`), so a disc
ejected while nothing reads it is retracted all the same. The engine,
descriptor reader, and block service are host-proven over scripted doubles;
the USB optical vertical (`tairix-test-optical-qemu-aarch64`) drives a QEMU
`usb-bot` + `scsi-cd` drive on its xHCI controller end to end — changing the
disc mid-run, so the second disc is read back through a fresh publication of
the unit — and the rest of the live path is Pi 4 metal acceptance.

### Volume manager (automount policy) — `drivers/storage/volmgr`

`tairix-drv-storage-volmgr` closes the hotplug loop (`plans/DEVICES.md`
D3c): it is the **policy driver** `devmgr` autoloads against each
block-service node — a disk's per-LUN node (compatible
`tairix,usb-msd-lun`) or a composed array's (`tairix,raid-array`), which
are indistinguishable in kind — one instance per node, spawned with
exactly that node's blkio endpoint + shared-window
grants — the same discovery/match/grant machinery every driver uses, so
no new kernel surface and no ambient authority (an instance can never
reach a sibling device's transport; the per-endpoint grant gates every
`ipc_call`).

The instance is a **read-only prober**: a fail-closed blkio `Block`
client (hostile geometry refused at connect, `write_blocks` refuses by
construction), the layout probe (whole-device filesystem signature first
— a superfloppy — else the GPT/MBR table via `lib/partition`, each
present partition's head probed by content through `lib/fsprobe`;
declared partition types are hints the probe ignores), and the naming
policy, which names a volume from itself: its own label in kebab case
(`My Backup Disk` is `my-backup-disk`; FAT32's read from the root-directory
label entry), else `<transport>_<fingerprint>` (`usb_25cm`); a label
already in use, or one spelling a root-view entry, a storage resolver or a
resource namespace, takes `_<fingerprint>` of the volume's own identity,
lengthened while taken. A label never yields `_`, so it cannot spell
another volume's generated name, and unlabelled volumes take the same names
whatever order they arrive in.

It runs in **two ordered phases**, because the probe and the mounts it
creates would otherwise be two concurrent users of the device's one staging
window (see "The shared data window has exactly one user at a time"). First
the whole device is probed and every recognised volume recorded; then the
blkio transport is *dropped*, which consumes the window borrow so no further
probe read can compile; only then is each recorded volume handed to the
kernel through the `CAP_FS_MOUNT`-gated, audited
`volume_attach` syscall — the kernel re-validates the grants, extent,
and name, opens the filesystem itself, mounts under `/Storage/<name>`,
and publishes the durable `id::` root. The instance then exits `0`
(run-to-completion; the kernel-held mount outlives it), logging every
outcome with stable event ids (28080–28084). Removal handling (surprise
removal, retained dirty state, force-unmount, verified re-insert) is the
staged D4 work.

The blkio client, probe plan, and naming policy are host-proven over
scripted devices and synthetic disk images; the live path is Pi 4 metal
acceptance, following the `usb_msd` precedent.

### RAID array composer — `drivers/storage/raid`

`tairix-drv-storage-raid` is the **policy driver** that turns discovered array
members into served arrays. It binds no hardware: `devmgr` matches it to the
kernel's one synthetic `tairix,virtual-bus` node, so a single instance runs per
machine whether or not a member exists. Each member disk reaches it through the
sibling per-disk agent (`drivers/storage/raid_member`), which delegates that
device's blkio endpoint and window to the composer's reserved rendezvous and
parks — the parked call *is* the membership.

The composer reads every offered device's superblock itself, assembles the
array its metadata describes, and publishes it as a `tairix,raid-array` node
carrying the array's own endpoint and window. The volume manager above binds
that node exactly as it binds a disk's, so an array's filesystems mount through
the unmodified path, and an array can itself be a member of another array. Each
live array is served through the same `serve_request_recovering` engine a leaf
device is served with, so it rides the same recovery grace window.

The assembly decisions, the degraded-start re-stamp that stops a returning
member masquerading as current, and the reserved-metadata offset that keeps a
member's superblock out of array data are documented with the composition
engines in the RAID library page.

#### Blank disks are held, not ignored

A whole device the volume manager probed **entirely empty** — no partition
table, no filesystem signature, no array metadata — is published as a
`tairix,raid-candidate` node, and its member agent offers it to the composer
down the same rendezvous a real member uses. The composer *holds* such a device
as an **unaffiliated candidate**: it is registered, offered to no array, and
excluded from the reassembly view entirely, so no assembly, late-join, or
rebuild can consume it. Only an explicit administrative request may claim it.
That asymmetry is deliberate — a blank disk plugged into a machine must never be
drawn into an array by accident, and metadata that is present but *damaged* is
not blank: it is refused rather than treated as a candidate, so a create can
never overwrite a member whose superblock merely failed to decode.

#### Administration and status endpoint

Alongside the rendezvous the composer binds the reserved
`RAID_CONTROL_ENDPOINT` (`lib/abi/src/raid_admin.rs`) on the **same wait-set**,
so one park serves both and there is no second thread and no poll. Each request
is judged in a fixed order: the caller's identity is read from the kernel's
attested call origin (never from the frame), the frame is decoded, and the
operation's required capability is checked **before any state is read or
written**; anything else is `PermissionDenied`.

| Operation | Authority | What it does |
| --- | --- | --- |
| `ListArrays` | `CAP_SYSINFO_HW` | Pages the live arrays: identity, level, width, active members, health, rebuild/scrub progress. |
| `ListMembers` | `CAP_SYSINFO_HW` | Pages every held device: an array member's slot and state, a device whose metadata names an unassembled array (`Held`), or an unaffiliated blank `Candidate`. |
| `Create` | `CAP_STORAGE_ADMIN` | Creates an array over named blank candidates. |
| `Add` | `CAP_STORAGE_ADMIN` | Admits a blank candidate into an absent slot and starts its rebuild. |
| `Remove` | `CAP_STORAGE_ADMIN` | Retires a **faulted** member, vacating its slot. |
| `Stop` | `CAP_STORAGE_ADMIN` | Retires the array's published node and releases every member. |

`Create` is the strictest path, because it is the only one that deliberately
destroys what is on a disk. Every named node must currently be a held
unaffiliated candidate; the width is checked against the level's own floor and
ceiling; a stripe unit is required exactly when the level is striped and refused
otherwise; and every member's geometry must agree and leave room past the
reserved metadata. Each device is then **re-read by the composer itself** — no
filesystem, no array metadata, no partition table — because the candidate node
is a pointer to look, never a claim to believe, and a disk can have been written
between the probe and the request. Only then is the array identity minted from
the kernel CSPRNG (a caller-supplied identity could collide with a live array
and leave two arrays indistinguishable to reassembly) and each member stamped at
generation 1. A stamp that fails rolls the whole create back, so no half-created
array is ever left claiming to be whole.

`Remove` refuses a live or rebuilding member — only a faulted one may be
retired, so a working copy is never dropped by a mistyped request. Retiring one
bumps the array's generation and re-stamps every survivor at it, so the removed
disk, which still carries a superblock naming its old slot, can never return
claiming to be current. `Stop` uses the kernel's **orderly** node removal, which
refuses with `Busy` while a volume is still attached on an endpoint the node
declares; that refusal reaches the administrator unchanged with the array left
running and nothing released, so an array cannot be stopped out from under a
mounted filesystem.

Every mutation is audited, allowed or refused, naming the operation and the
array or device but never a token: `28214` an allowed mutation, `28215` a refused
one (with the errno), `28216` a request whose origin the kernel could not attest
(refused unread), and `28217` a blank device taken in as a candidate. Reads are
not audited — a status poll would drown the trail.
