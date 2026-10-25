# `tairix-drv-storage-usb-msd` — USB mass-storage class driver

`plans/DEVICES.md` D2/D5. The autoloaded **user-space USB mass-storage
*class* driver** — the `Run` binary `devmgr` spawns when a mass-storage
**interface** node it serves is discovered (`AGENTS.md` §18): SCSI
transparent over Bulk-Only Transport (`08:06:50`, the ubiquitous stick/disk),
UFI floppies over BOT (`08:04:50`) and over Control/Bulk/Interrupt
(`08:04:00`, the classic USB floppy drive), USB Attached SCSI
(`08:06:62`), and optical drives — any of those carrying a multimedia unit,
and ATAPI drives over BOT (`08:02:50`). The crate is a `lib` (its `BIND_KEYS` bind table plus the
host-testable device logic: the configuration-descriptor reader, the
transport-neutral SCSI command layer, the three wire transports, and the
block-service state machine) **and** a `Run` binary (the process).

It is a pure class driver: it touches **no** controller register, owns **no**
controller DMA, holds **no** IRQ line. The host-controller driver
(`drivers/bus/usb/xhci`) owns the controller and serves this interface's
control and bulk transfers over the bus-agnostic URB transport. The same
binary works behind any host controller that speaks the URB transport
(`AGENTS.md` §2.20 / §17.4).

## What it does

`main` (a freestanding pure-Rust `tairix-rt` program):

1. Builds `tairix_drvrt::RtDriverHost::from_grants_query` over its granted
   resources (the interface node's two transport grants — no MMIO/DMA), reads
   the URB endpoint id and maps the shared URB data buffer.
2. Reads the device's own **configuration descriptor** over control-IN and
   derives — never assumes — the mass-storage interface number, its wire
   transport (the interface protocol byte), its command set (the sub-class
   byte), and that transport's endpoints: the bulk pair for BOT, bulk pair +
   completion interrupt for CBI, or the four Pipe-Usage-named bulk pipes for
   UAS (`src/desc.rs`, fail-closed on every hostile length).
3. Discovers the units (`GET MAX LUN` for BOT, `REPORT LUNS` for UAS,
   exactly one for CBI) and sends each `INQUIRY` (a direct-access disk or a
   multimedia drive is served, any other type skipped). Every unit then comes
   up on its own one-shot timers inside the
   serve loop (`src/ready.rs`), so one disk spinning up never holds back its
   siblings. `TEST UNIT READY`'s sense (consumed per failed attempt — UAS and
   CBI/UFI deliver it in-band, BOT via `REQUEST SENSE`) is classified by key,
   ASC and ASCQ: a unit coming ready is re-tested every second, one needing an
   initializing command is sent `START STOP UNIT` (IMMED) once, one needing a
   person is given up on, a unit attention is consumed and re-tested at once
   (a few in a row at most), and a unit that does not come ready within its
   class's ready budget is given up on with its reason logged. A ready unit
   is probed for what it is — the VPD pages it supports, Block Limits (the
   most one command moves), the Block Device Characteristics rotation rate,
   else IDENTIFY DEVICE word 217 through `ATA PASS-THROUGH(16)` — each probe
   sent once and a refusal reporting nothing. INQUIRY's RMB bit records only
   whether the medium can leave the unit (many bridges set it for a fixed
   disk), so it never decides the unit's patience. Then
   `READ CAPACITY(10)`/`(16)` (validated geometry; 16-byte form past the
   32-bit LBA horizon) and the write-protect bit (`MODE SENSE(6)` for the
   transparent set, `MODE SENSE(10)` for UFI, ATAPI and multimedia units); a
   unit ready to a test whose geometry cannot be read is re-tried within its
   ready budget, then given up on. A multimedia unit's disc is served as
   `BlkForm::OpticalDisc` and asked what it is instead of probed as a disk:
   `GET CONFIGURATION`'s Random Writable feature (written only when current),
   `READ TOC` format 1 (the last session's start), and `READ DISC INFORMATION`
   with `READ TRACK INFORMATION` (its last recorded block), all carried to the
   consumer as its `DiscLayout`. A drive refusing one, or breaking the
   exchange of one (which ends the queries), reads as a pressed,
   single-session disc; a disc not yet ready, or that left or changed, fails
   the bring-up so it is read again, and another unit attention is asked past
   once. A session starts within what the disc recorded, and a last recorded
   address outside its track is not believed. `MEDIUM ERROR`, `BLANK CHECK`
   and `HARDWARE ERROR` fail the one request, never the unit; `RECOVERED
   ERROR` completes the command, and `ABORTED COMMAND` or a check condition
   naming no cause is answered reissuably.
4. Publishes one **storage-class** hardware-tree node per ready LUN, carrying
   a `tairix,usb-msd-lun` compatible key, two grants — a block-service call
   endpoint and a 32 KiB shared data window (`tairix_abi::blkio` — the
   request/completion frames a consumer such as the D3 volume manager
   drives) — and the unit's medium record. A unit with no medium (an empty
   card slot) is present, not failed: it is tested for a medium every two
   seconds and published when one arrives. A removable unit whose medium was
   given up on waits for that medium to leave or change before it is brought
   up again; a fixed one is refused. Once every unit present at attach is
   published, empty, or given up on, the driver settles its subtree, so a
   unit published later is a hot arrival.
5. Parks on a kernel wait-set over the LUN endpoints and serves each
   `BlkRequest` (geometry / read / write / flush) through the shared SCSI
   command layer (`src/scsi.rs`) on the device's wire transport, each LUN
   carrying a per-unit `blkio::BlkHealth` of its medium's class so a
   transient stall or bus reset is ridden out through its recovery grace
   window — answered reissuably while `Recovering`, failed closed only once
   the window elapses without the unit returning (`plans/FIX-IO.md` IO3). A
   recovering unit also drives a per-LUN `blkio::RecoveryLadder`: after each
   reply the serve loop escalates the shared ladder from the unit's health —
   a gentle retry first, then a transport reset (`ScsiDevice::reset_transport`:
   the transport's own reset recovery) on the next attempt, bounded by the unit's
   class's `IoBudget::max_retries` so a wedged unit is never reset forever — nudging a
   stalling unit back before the grace window is left to fail it closed:
   - **BOT** (`src/bot.rs`): CBW/CSW framing with every device field
     validated, per-transfer halt recovery below (the HCD), CSW-stall retry,
     and the spec's reset recovery — a Bulk-Only Mass Storage Reset, then
     the halt of each bulk pipe cleared — whenever an exchange breaks: a
     transfer that fails or outruns its deadline, a tag mismatch, a corrupt
     CSW, a phase error.
   - **CBI** (`src/cbi.rs`): the 12-byte command block over the ADSC
     control-OUT channel (a control STALL is the "command not accepted"
     answer), the bulk data phase, the two-byte completion interrupt (UFI
     ASC/ASCQ or the typed status spelling), and the Command Block Reset
     with both bulk pipes cleared whenever an exchange breaks. A completion
     carries no tag, so every command first drops the completions the HCD
     has captured (`DiscardReports`): one an earlier command raised late is
     never read as the next command's. A UFI floppy reports the
     failing command's ASC/ASCQ **in its completion interrupt** (CBI 1.1
     §3.4.3.1.1), so the sense is read in-band through `take_sense` exactly
     as UAS delivers autosense — never over a separate `REQUEST SENSE`,
     which a real UFI floppy (e.g. the Mitsumi SmartDisk FDD) does not
     answer reliably and whose failure otherwise aborted bring-up. UFI
     floppies have no `SYNCHRONIZE CACHE`; a flush succeeds without a wire
     round trip.
   - **UAS** (`src/uas.rs`): tag-checked Command/Read-Ready/Write-Ready/
     Sense IU sequencing over the four pipes (USB 2.0 non-stream
     operation), with autosense delivered in-band, every hostile IU
     refused closed, and all four pipes cleared when an exchange breaks.

   Every command exchange is bounded by its own budget
   (`CommandBudget::exchange_ns`: a media command, a query, or a flush, plus
   its transfer at the class's slowest throughput), so a disk spinning up on
   a read is given its spin-up while a query is not, and a device that stops
   answering fails the command and has its transport reset, the failure
   answered reissuably for the unit's health to ride out. Once the reset has
   put the transport back in step the unit is asked `TEST UNIT READY`: one
   answering ready, or with a unit attention the test consumes, has its
   request answered `BlkReissue::Now`, so its consumer reissues at once rather
   than waiting out its pacing; one coming ready has the request held, and one
   without its medium is retracted. Every transfer of
   an exchange, its control requests included, runs within what the
   exchange has left; the reset recovery a broken exchange owes runs in an
   exchange of its own, as long as the command it serves, so a command that
   spent its exchange still gets its reset. Whatever command broke, its
   reset's attention is then owed by every unit, so a unit whose medium can
   leave reads any other reset attention as a possible change of medium. A
   media command answered with a unit attention is sent once more. One answered "coming
   ready" is held — `DriverError::NotReady`, which the shared engine answers
   `Answer::Hold` — with no health folded and no reissue spent: the unit's
   endpoint leaves the serve set, so the calls queued behind it wait in the
   kernel, and the request is served once the unit is ready, or answered
   through its health when the unit is given up on. One answered with no
   medium, or a changed medium, retracts the unit's node, revoking the grants
   its consumers inherited, and the next medium is published afresh under a
   new publication binding, so a request made for the old medium is refused
   `Stale` rather than served the new one's blocks.

   Every LUN behind one device shares the *same* bulk pipe pair, so a
   transport reset is a **transport-wide** event, not a per-LUN one. The serve
   loop therefore wraps the per-unit health in one shared-transport
   `blkio::FaultDomain` (owned by the device's own discovered URB transport
   grant, never a board constant): a reset opens a single recovery window over
   the whole device — every LUN's request is held reissuable under it, the
   device recovers the moment *any* unit completes a real transfer (or reports
   a medium error, which proves the transport delivered the command), and only
   a window that elapses with the transport still wedged fails the whole device
   closed. So one shared-transport blip is one recovery episode, not N spurious
   LUN failures (`src/recover.rs`, `plans/FIX-IO.md` IO4).

   Write-protected media refuse writes driver-side before any byte reaches
   the device. A vanished interface retracts every LUN node and exits `0` so
   `devmgr` reloads the driver on re-plug.

Failures exit with reserved fail-closed codes (`80` no host, `81` no
transport, `82` bring-up refused every unit, `83` block service could not be
stood up), leaving the volume absent rather than wedged (`AGENTS.md` §2.9).

## Least privilege (`AGENTS.md` §5.4)

`CAP_SHM` (map the granted URB buffer; create the per-LUN data windows),
`CAP_IPC_ENDPOINT` (submit URBs on its one interface's transport endpoint),
`CAP_IPC_BIND_PRIVILEGED` (bind the per-LUN block-service endpoints),
`CAP_HW_EMIT` (publish/retract the per-LUN storage nodes), `CAP_LOG_EMIT`
(diagnostics). A compromised disk driver cannot reprogram the controller,
reach another device's buffer, or touch the bus.

## Supported hardware / limitations

Any USB mass-storage device exposing an interface in the served set —
SCSI-transparent bulk-only (`08:06:50`, essentially every USB flash stick
and disk enclosure), UFI floppy drives over BOT (`08:04:50`) or CBI
(`08:04:00`), and USB Attached SCSI (`08:06:62`) — that a host-controller
driver enumerates and serves over the URB transport (USB-A ports via the
xHCI HCD). Up to 16 LUNs. Logical block sizes 512–4096 (powers of
two). Deliberately not covered (per `plans/DEVICES.md` §3/D5): the
interrupt-less CB variant (protocol `0x01`), tape, software eject and tray
locking (`plans/FOREIGN-MEDIA.md`), UAS command queueing / task-management
IUs / SuperSpeed bulk
streams (one command in flight serves the synchronous block service; a
protocol violation fails the exchange closed), and the BOT→UAS alternate-
setting switch (a dual BOT+UAS device runs its default BOT setting; UAS
serves devices exposing it as the default interface). Media are polled for
with `TEST UNIT READY` every two seconds — an empty
unit for a medium, a serving removable one for its medium leaving — and MMC's
media event notification is not used. Runtime unload is
driven by `devmgr` through the kernel driver-unload mechanism; the driver
also exits by itself when its interface vanishes.

## Tests

The device logic is host-proven in this crate over scripted doubles:
`src/ready_tests.rs` (readiness classification by key/ASC/ASCQ, paced
re-tests, the one start, the ready budget, attention storms, a unit ready but
unservable), `src/scsi_tests.rs` (per-set CDB spelling, the medium probe and
its refusals, per-command budgets, MODE SENSE forms, flush semantics, the
unit-attention retry, autosense vs `REQUEST SENSE`, LunBlock
validation/chunking/scrub), `src/bot_tests.rs` (CBW/CSW framing, tag
mismatch and corrupt-CSW reset recovery, CSW-stall retry, stalled data
phases, short reads, sense mapping, capacity validation including a
100 TB-class unit, multi-LUN), `src/cbi_tests.rs` (ADSC framing, the
stall-as-refusal answer, both completion spellings, Command Block Reset,
the floppy geometry, in-band UFI sense with no `REQUEST SENSE`, and a
not-ready floppy's readiness read in-band), `src/uas_tests.rs` (IU sequencing, tag checking,
autosense in both sense formats, hostile IUs, `REPORT LUNS`),
`src/desc_tests.rs` (transport classification and hostile descriptor
streams, including UAS Pipe Usage validation), `src/serve.rs` (the per-LUN
block-service endpoint-id derivation), and `src/recover.rs` (the
shared-transport fault-domain coordination over fault-injecting doubles: a
quiesced transport holds a stalling sibling reissuable, any unit's definitive
answer recovers the whole device, and an elapsed window fails a sibling
closed). The URB transport
is proven in `lib/usb` (bulk, the no-data and data-stage control-OUT, the
EP0 stall recovery, and the per-pipe bulk routing). `src/scsi_tests.rs` also
covers the multimedia set: a multisession disc, an open track recorded up to
its next writable address, a rewritable disc, refusing drives, a short track
answer, and per-request medium errors. The USB optical QEMU vertical
(`tairix-test-optical-qemu-aarch64`) runs a `usb-bot` + `scsi-cd` drive on
QEMU's xHCI controller end to end; the rest of the live path is the metal
acceptance item (`plans/PI.md` §0.4).
