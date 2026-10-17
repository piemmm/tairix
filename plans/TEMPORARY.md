# TEMPORARY.md — Open work from the Raspberry Pi 4 USB-storage investigation

## Ledger

| ID | Item | Status |
|---|---|---|
| TMP-0 | Decisions D-1, D-2 and D-4 put to the user, answered and recorded under each; D-3 is answered | blocked: the user's answers to D-1, D-2 and D-4 |
| TMP-1 | The USB host controller's serve loop never waits: every control transfer, controller command, port reset and recovery is an operation driven by events and one-shot timers (`plans/OPEN-DEFECTS.md` D1125) | planned |
| TMP-1.1 | Each device's EP0 ring, control buffer and input context are resident in its own region; the engine-wide active/resting control cursor is deleted | planned |
| TMP-1.2 | Control transfers are queued and reaped as bulk TDs are; a control URB carries a deadline and is held, never answered inline | planned |
| TMP-1.3 | Controller commands are posted and routed by command-TRB address; a hung command is aborted, failing only its owner | planned |
| TMP-1.4 | Endpoint recovery, deadline abandonment, `ClearHalt`, `SetInterface` and stream stop are operations | planned |
| TMP-1.5 | Enumeration, hub port service, root-port reset, slot teardown and the boot walk are one per-port operation | planned |
| TMP-1.6 | The engine's synchronous primitives are deleted and a host test proves the loop never waits | planned |
| TMP-2 | Mass-storage readiness and deadlines follow the detected medium | planned |
| TMP-2.1 | The medium is detected, with an explicit Unknown that every consumer reads conservatively; first, since every budget derives from it | planned |
| TMP-2.2 | Readiness is a timed state machine: spin-up, `START STOP UNIT`, no medium | planned |
| TMP-2.3 | Budgets derive from the medium and the transfer; a spin-up in service is ridden out | planned |
| TMP-2.4 | Consumers pace their reissue on the unit's health | planned |
| TMP-2.5 | An abandoned transfer's log names the endpoint's state | planned |
| TMP-3 | Attached-volume names are a stable property of the volume and never read as a format | blocked: D-1 |
| TMP-4 | Removable media are owned according to a per-volume policy | blocked: D-2 |
| TMP-5 | DESK-21 — window-channel requests are posted, never awaited (`plans/FIX-DESKTOP.md`) | planned |
| TMP-6 | Raspberry Pi 4 acceptance of TMP-1 to TMP-5, TMP-7 and TMP-8 on metal | blocked: TMP-1 to TMP-5, TMP-7, TMP-8 |
| TMP-7 | Dialogs, forms and progress are owned windows of their own; a modal panel drawn inside an application's window is forbidden | planned |
| TMP-7.1 | The owned dialog window: protocol, session placement, owner modality shared with alerts, earned focus | planned |
| TMP-7.2 | TextEdit's Go to line, Paint's forms and Settings' credential sheet become dialog windows; the in-window placement helpers are deleted | planned |
| TMP-7.3 | The file manager's operation progress is its own window | blocked: D-4 |
| TMP-8 | Alerts and dialog windows size to their content; nothing is elided (`plans/OPEN-DEFECTS.md` D1129) | planned |

Binding under `AGENTS.md`. This plan collects the decisions and the work the
Raspberry Pi 4 USB-storage logs of 2026-10-16 and 2026-10-17 left open once
their defects (D1113–D1124, D1126–D1128) were fixed. It also holds the desktop
dialog changes the user asked for on 2026-10-17 (TMP-7, TMP-8). It is
**temporary**. Each item's
durable design and guarantees move into the owning plan and `docs/` page in
the change that finishes it (USB.md, DEVICES.md, FIX-IO.md, DRIVES.md,
ALIAS.md, FIX-DESKTOP.md). The change that finishes the last item deletes
this file and its rows in `AGENTS.md` §15.18 and `PLAN.md`.

## The standard every item is held to

The work must pass review by a maintainer of Linus Torvalds's standard: a
reviewer who reads the design before the diff and rejects anything that is
merely made to work. Doing it properly comes first, even when that costs more
work. In practice:

- **Design first, on paper.** Each item's design is settled here, or in its
  owning plan, before code is written. A design that needs changing
  mid-implementation is changed here first, not worked around in code.
- **Correct and complete, never minimal.** Implement the abstraction, not the
  first caller's slice (§27). Make no knowingly partial step (§2.19). Add no
  shim, second path or compatibility layer: `abi-v1` is not frozen, so an
  interface changes in place with every caller (§2.13). What an item
  supersedes is deleted in that change (§2.14).
- **Prerequisites are part of the item.** If one is missing, it is built here.
  If it conflicts with the charter, another plan or a decision recorded
  below, stop and ask the user (§15.7) rather than choosing a compromise.
- **Proven, not asserted.** Every defect fixed gets a regression test that
  fails before and passes after. Every behaviour gets host tests over the
  existing doubles (the xHCI mock in `lib/usb/src/tests.rs`, the scripted
  mass-storage devices in `drivers/storage/usb_msd/src/testutil.rs`, the
  block mocks), a QEMU vertical where QEMU models the hardware, and TMP-6 on
  metal where it does not.
- **Fast by construction.** Nothing on a hot path allocates, copies, locks or
  repeats work that could be hoisted (§2.16). Bounds that are capacities
  derive from discovered hardware (§24.1); bounds that are protocol or
  containment limits stay fixed and are defined once (§24.4, §2.2).
- **Sources.** Implement from the published specifications: xHCI 1.2, USB 2.0
  and 3.2, USB MSC BOT 1.0, CBI 1.1, UAS, SPC-5, SBC-4. Linux and macOS
  behaviour may be cited as precedent, but GPL-2.0-only source is not read.
- **The gate.** The whole gate runs green on the final tree: `cargo xtask ci`,
  `cargo xtask fuzz --secs 5` and `tools/ci/soak.sh both --secs 20`. The
  completion report declares the adversarial (§23) and performance reviews
  with their findings, even when there are none, and states which oracles
  looked (§19.11).

## Order of work

TMP-2.1 comes first (the user, 2026-10-17): every mass-storage budget
derives from the detected medium, so nothing in TMP-2 can be sized correctly
before it. The rest of TMP-2 follows it in order. TMP-1 and TMP-5 depend on
nothing here. TMP-8's layout is the one TMP-7's dialogs size themselves by, so
it lands before or with TMP-7.1. TMP-3, TMP-4 and TMP-7.3 wait for D-1, D-2
and D-4, and TMP-6 comes last.

## TMP-0 — Decisions for the user

Each decision is put to the user before its item starts. The answer is
recorded beneath the decision, and the item's ledger row is unblocked in the
same change.

### D-1 — How an attached volume is named (gates TMP-3)

What the code does (`drivers/storage/volmgr/src/{name.rs,plan.rs,program.rs}`):

- A usable label is lowercased and kept to `a-z0-9-_`, at most 32 bytes.
  Otherwise the name is `<fstype><n>`, where `ext` stands for the whole ext
  family and `n` counts volumes of that type **on that device only**.
- If the kernel answers `AlreadyExists`, the volume-identity fingerprint is
  appended as `-<4|8|26 chars>`.

Consequences:

1. Every unlabelled ext stick is `ext1`, and `ext1` reads as the name of a
   filesystem format.
2. Whichever volume attaches first takes the bare name. A volume's name
   therefore depends on what else is attached and in what order, so DEVICES.md
   D3c's "Re-inserting the same volume re-derives the same name" is false. The
   2026-10-16 log showed `ext1-25cm`, `ext1-cksm`, `ext1-yeka` and
   `ext1-aq7b`. The 2026-10-17 boot of nine USB-enclosure disks named seven
   ext volumes `ext1`, `ext1-2s5d`, `ext1-cksm`, `ext1-25cm`, `ext1-ym7q`,
   `ext1-aq7b` and `ext1-yeka`, in the order they attached.
3. The code disagrees with the specs: ALIAS.md §10.1 derives the fingerprint
   from `resource_kind || canonical_identity || identity_version` in an
   uppercase alphabet, while the code encodes the raw identity in lowercase.
   DRIVES.md's examples use `CameraCard-1:`.

Options:

- **(a) A pure function of the volume.** The label if usable, else
  `<medium>-<fp4>`, for example `usb-25cm` or `sd-k3x9`, with the medium
  taken from `BlkMedium`. A label already in use takes `-<fp4>`. This is
  stable per volume except when two attached volumes share a label.
- **(b) (a) plus a persistent identity → name record.** The first attach
  assigns the name and every later attach reuses it. A user rename becomes an
  alias recorded the same way. The record is bounded by a fixed containment
  limit, evicting least-recently-attached entries, and lives in machine
  settings. It is fully order-independent, and costs a store with an owner
  and a write path.
- **(c) Friendly names:** the label, else `untitled`, `untitled-2`, and so
  on. This is macOS's choice, and it is order-dependent by construction.

Recommendation: **(b).** A volume name is a user-facing contract that scripts,
desktop shortcuts and recent-file lists depend on, and only a persistent
record makes it independent of attach order. Whatever is chosen, the code and
ALIAS.md §10, DEVICES.md D3c and DRIVES.md are brought into agreement.

*Answer:* —

### D-2 — Who owns the files on removable media (gates TMP-4)

What the code does:

- **ext2/3/4** are served with their on-disk uid, gid, mode and ACL as they
  are (`drivers/filesystem/ext4/src/lib.rs` `security`; no wrapper in
  `kernel/tairix-kernel/src/volume_policy.rs`). A stick made by `mke2fs`
  elsewhere has a root-owned `0755` root, so a local user cannot create
  anything at its top level. That is the log's `mkdir … errno=6`: the user
  copied a folder named `files.app` into `/Storage/ext1-25cm` and was refused.
  The file manager keeps no per-volume state.
- **FAT32** is wrapped in `GroupMappedFs` when a `storage` group exists: uid 0,
  gid `storage`, `0775`/`0664`, and security changes refused
  (`volume_policy.rs`).
- **ARXFS** is mounted unwrapped.
- No mount option maps or ignores ownership: `MountFlags` is `READ_ONLY` and
  `NOEXEC` only. `CAP_FS_MOUNT_RELAX` is named in DRIVES.md and
  NEW-NAMESPACE.md but does not exist.

Honouring foreign uids is not only inconvenient, it is unsafe on a multi-user
machine. A stick whose files belong to uid 1000 on its home system gives owner
rights to whichever local account happens to be uid 1000.

Options:

- **(a) Honour on-disk ownership on POSIX formats**, as today, and add an
  explicit, capability-gated "take ownership". This matches the Linux desktop.
- **(b) Map every removable volume through the `storage` group by default**,
  whatever its format, as FAT32 already is. Honouring on-disk ownership
  becomes a per-volume choice, recorded by volume identity and gated by the
  mount-policy authority. New nodes are stored with the volume root's on-disk
  owner, so the medium stays coherent for its home system. This matches
  macOS's default of ignoring ownership on external volumes.
- **(c) Ask at first attach** and remember the answer per volume.

Recommendation: **(b).** One rule then covers every removable format, access
depends on local group membership rather than on ids minted on another
machine, and the uid-coincidence hole is closed.

*Answer:* —

### D-3 — Mass-storage time budgets (gates TMP-2)

What the code does:

- Every USB unit is in the `Removable` class (`lib/abi/src/blkio.rs`:
  15 s deadline, 3 retries, depth 2, 20 s grace), whatever its medium.
  `lun_medium` yields only `UsbFlash` or `UsbDisk`, and both map there.
- The medium itself is a guess from one bit. `lun_medium` reads only
  INQUIRY's RMB: set gives `UsbFlash`, clear gives `UsbDisk`. USB-SATA bridges
  commonly set RMB for a fixed disk, so a disk in an enclosure can be
  described as a "USB flash drive" (`lib/procinfo/src/volume.rs`) and drawn
  with the USB-stick icon (`lib/icon/src/glyph.rs`). Nothing else is
  consulted.
- A command exchange gets 3.75 s (`exchange_budget_ns`).
- Bring-up makes 8 `TEST UNIT READY` attempts back to back (`READY_ATTEMPTS`,
  `ScsiDevice::ready_after_drain`).
- The sense is classified by key alone, so NOT READY sub-codes are never told
  apart. `START STOP UNIT` is never sent and no VPD page is read.
- Consumers (`lib/blkclient`, `kernel/core/src/fs/blkclient.rs`) reissue at
  once with no pacing, and neither reads `IoBudget::grace_ns`.

A USB hard disk that spun down therefore fails its first command and is reset,
and one still spinning up at plug-in can exhaust its ready attempts and be
skipped.

The witness is the 2026-10-17 boot. Nine 2 TB disks in USB enclosures
(`blocks_hex=00000000e8e088b0`) were each published after `TEST UNIT READY`
answered ready: their bridges do not report a stopped disk there. Eight of
them then outran the 3.75 s exchange on their first read while spinning up,
logged as `usb-hcd: URB failed … errno=13` (TimedOut) at 33.27–33.46 s
against probes started at 29.52–29.57 s. Each was reset (event 27376) and
"recovered" (27370) once it had spun up. Nothing was broken, so the reset was
the wrong answer; under a 30 s command budget none would have run.

*Answer (the user, 2026-10-17):* detect what the medium is, so the budgets
can be sized to it; where it cannot be detected, classify the unit as the
slowest media. Detecting the medium is the first thing to correct.

What the answer means for the design (TMP-2.1 and TMP-2.3):

- **Only positive evidence tightens a budget.** A unit is solid-state only
  when it says so, through VPD B1h or SAT. A reported rotation rate, or no
  report at all, gets the slowest media's budgets: the `Rotational` class's
  30 s deadline and 60 s grace are the most patient the tree defines.
- **Reference values.** Linux's `sd` driver uses 30 s per command, twice that
  for a cache flush, and up to 100 s for a disk to spin up. A spin-up budget
  of 60 s matches the Rotational grace.

### D-4 — Does an operation's progress window hold its folder window? (gates TMP-7.3)

What the code does: the file manager runs one operation at a time per window,
as `Overlays.operation` (`userland/apps/files/src/run.rs`). It draws the
progress as a panel painted into that window, and the app itself filters
every input but Cancel, Escape and close (`operation.rs` `operation_control`).
TMP-7.3 moves the progress into a window of its own; this decision is whether
that window holds its folder window modal.

Options:

- **(a) Owner-modal, as today.** The folder window waits while its operation
  runs, as it would under an alert. This is the smallest change to the file
  manager.
- **(b) Modeless, one owned window per operation.** The folder window stays
  usable while copies run, as Finder, Explorer and GNOME Files all work. The
  file manager then holds a bounded set of operations instead of one, and
  must order operations that touch the same tree, queueing the later one
  behind the earlier.

Recommendation: **(b).** A copy is not a question, and nothing about it needs
the folder window held; holding it is the older pattern that every current
desktop has left.

*Answer:* —

## TMP-1 — The USB host controller's serve loop never waits (D1125)

### Problem

The xHCI HCD (`drivers/bus/usb/xhci/src/main.rs` `serve_events`) is a single
loop serving every interface on its controller. Many paths reachable from it
after bring-up wait synchronously, on `await_event_for` (up to
`AWAIT_EVENT_BUDGET_US`, 5 s each) or on a parked `Delay`:

- a class driver's control URB (`drive_urb` → `DeviceEngine::control_*` →
  `device_control`);
- every pipe recovery: `recover_bulk_endpoint`, `clear_bulk_halt`,
  `reset_bulk_endpoint` and `readd_bulk_endpoint`, `resynchronise_bulk_pipe`,
  `abort_bulk` at a deadline, `recover_interrupt_endpoint`;
- `SetInterface` and `IsoStop`;
- hub status-change service: `GET_STATUS` per port, then `PORT_RESET` with up
  to `PORT_RESET_POLLS` × `PORT_RESET_POLL_US` (40 × 20 ms) of polls plus a
  settle, then `enumerate_on_port`, which issues 3–5 commands and 5–9 control
  transfers per device, up to 4 attempts;
- a root-port reset parked up to 800 ms;
- fault-detach and slot teardown;
- controller recovery, which re-runs the whole `bring_up`, connect window
  included.

During any of these waits the loop serves no other interface's submit,
delivers no captured report, honours no URB deadline and services no
hot-plug. A device that NAKs its control endpoint, a slow hub or a recovering
pipe on one device therefore freezes every device on the controller,
keyboard included: a halted bulk transfer's clear followed by its class reset
can hold input for 10 s. This contradicts `plans/USB.md` §1.1 ("never a
per-URB blocking server").

The limit is in software, not hardware. Every device already has its own EP0
ring and a 4 KiB control buffer in its own region (`DeviceRegion`), but the
engine keeps one `ControlCursor` and drives one control transfer at a time:
`activate_device_control`, `rest_active_context`, `active_device`,
`active_hub`. It also uses one engine-wide input context for every Address
Device, Evaluate Context and Configure Endpoint command.

### Design

The engine becomes an event-driven state machine; this is the same model the
bulk path already uses (`queue_bulk` / `poll_bulk`).

1. **Operations.** Every multi-step procedure is an explicit operation. Each
   operation has a typed state, an owner (a slot, a hub port, a root port or
   the controller), a deadline, and a cancellation path that releases every
   ring, chunk, slot and URB it holds. Operations advance only when a routed
   completion or a due timer arrives, and never wait inside a step.
2. **Routing, never awaiting.** Transfer events are routed by (slot, DCI) to
   the endpoint's owner, and command completions by command-TRB address to
   the pending-command table. An event nothing owns is dropped as stale, as
   today; the shared ring is not a security boundary.
3. **Per-device control.** Each device's EP0 ring, control buffer and input
   context live in its region. The pending control operations form a bounded
   per-device FIFO with one TD in flight, since the control pipe carries one
   message at a time (a message pipe, USB 2.0 §5.3.2.2). Its capacity is one slot per interface,
   because each interface holds at most one URB, plus a fixed internal
   reserve; overflow fails closed with `Busy`. A hub-downstream device needs
   no activation: each slot's EP0 context already carries its route.
4. **Commands posted.** Commands are posted to the command ring and their
   completions routed by TRB address. A per-command deadline aborts a hung
   command through the Command Ring Abort of xHCI 1.2 §4.6.1.2, which fails
   only that command's owner. A controller that will not abort is a
   `ControllerHealth` fault, recovered as today.
5. **Ring reuse is fenced.** A ring is rebuilt only after the controller has
   confirmed the endpoint stopped or reset, so no completion for an old TD
   can land in a slot of the new ring. This generalises the bulk rule that
   `abandon_bulk_tds` documents.
6. **Time is a timer.** The connect debounce, port-reset settle, `TRSTRCY`
   and the hub reset poll become one-shot timers. The hub's status-change
   endpoint is re-armed before a port reset is issued, so its `C_PORT_RESET`
   change (USB 2.0 §11.24.2.7.2) arrives as an event, with a timer bounding
   it. The comment above `await_hub_port_reset_complete` says a hub raises no
   interrupt for its own reset; that is true only because the engine services
   the hub synchronously. The root port's reset completes on its Port Status
   Change event, also with a bounding timer. The loop parks until the
   earliest operation, URB or timer deadline, so it stays tickless.
7. **Control URBs are held.** `UrbRequest.timeout_ms` becomes required for
   control transfers. The class driver states its budget and the HCD clamps
   it to the USB request ceiling: 5 s (USB 2.0 §9.2.6.4 and §9.2.6.5), a
   protocol bound defined once. `UrbService` holds a control URB as it holds a
   bulk one. At the deadline the TD is abandoned (Stop Endpoint, then the ring
   repositioned past it), and the device's control pipe recovers at its next
   SETUP (USB 2.0 §8.5.3.4). The ABI rule "a control transfer carries none"
   and the `transport.rs` claim that a control transfer "completes
   synchronously within the call" are deleted.
8. **One definition.** The boot walk, hot-plug and controller recovery all
   drive the same per-port operation, so there is no second enumeration.
   Bounded hardware handshakes inside a controller reset (`HCRST`, `CNR`)
   remain bounded MMIO polls that fail closed, the documented exception in
   §2.23.

### Stages

Each stage converts whole procedures and deletes what it replaces. No stage
adds a synchronous wait, a second implementation of a procedure, or a
synchronous wrapper over an asynchronous primitive. The old synchronous
primitives (`await_event_for`, `issue_command`, `control_transfer`,
`EventWait`, `Delay` in the engine) survive only while an unconverted
procedure still calls them, and 1.6 deletes them.

- **1.1** Per-device EP0 ring, control buffer and input context;
  `ControlCursor`, `active_device`, `active_hub`, `activate_device_control`,
  `rest_active_context` and `ep0_owner_index` deleted. A composite sibling
  shares its slot's EP0 state by reference, never by copy.
- **1.2** Control transfers are queued and reaped. The URB ABI, `UrbLink` and
  `UrbClient` control calls take a deadline. Every class driver states its
  control budget: `usb_hid`, `usb_msd` (its reset recovery is bounded by the
  exchange), `usb_uac` and any other `UrbClient` user. `UrbService` holds and
  expires control URBs.
- **1.3** The pending-command table, posted commands, Command Ring Abort, and
  per-command deadlines.
- **1.4** Pipe and endpoint recovery as operations: the bulk-halt path that
  `decode_bulk_event` runs today (Reset Endpoint, or Stop and a Configure
  Endpoint drop and add; Set TR Dequeue; `CLEAR_FEATURE(ENDPOINT_HALT)`),
  interrupt recovery and its `Ladder`, `abort_bulk`, `ClearHalt`,
  `SetInterface` and `IsoStop`. A URB whose pipe is recovering stays held,
  and is answered when the recovery finishes or its deadline passes.
- **1.5** The per-port operation: connect debounce, reset, Enable Slot,
  Address Device, the descriptor and EP0 max-packet sequence, configuration,
  hub configuration and port power, publication, detach and Disable Slot,
  with hot-unplug cancelling it at any step. The boot walk and controller
  recovery drive it.
- **1.6** Delete the synchronous primitives, so the engine has no `EventWait`
  or `Delay` and no path to block. Update `plans/USB.md` §1.1, §1.3 and the
  active/resting-context passages, `docs/src/lib/usb.md`,
  `docs/src/drivers/bus.md` and `lib/usb/README.md`. Move this design into
  USB.md as a done item and close D1125.

### Acceptance

- **The loop never waits.** A mock-controller host test shows that a device
  whose EP0 never completes (it NAKs for ever) has its control URB held
  without delaying another device's report delivery, bulk completion or URB
  deadline in the same step. The held URB expires at its deadline, and that
  device's EP0 serves its next request.
- **Enumeration interleaves and cancels.** Two devices enumerate at once and
  both publish. An unplug at every enumeration step leaks no slot, chunk,
  ring or held URB.
- **Hung commands are contained.** A hung command is aborted and fails only
  its owner. A controller reset mid-operation cancels every operation, and
  held URBs are answered as they are today.
- **Ring reuse is fenced.** A completion for an abandoned TD can never
  complete a later transfer, for every endpoint type.
- **Existing tests survive.** Every existing `lib/usb`, xHCI and class-driver
  test passes, rewritten where a synchronous assumption was the thing under
  test, never weakened. Every QEMU USB vertical passes.
- **Metal.** On the Pi, a mass-storage stick that stalls or NAKs does not
  freeze the keyboard (TMP-6).

## TMP-2 — Mass-storage readiness and deadlines follow the detected medium

### Problem

See D-3 above: what the code does, the 2026-10-17 witness and the user's
answer. In addition, `ScsiDevice::ready_after_drain` makes its attempts back to
back, so a disk that reports its spin-up can be skipped in milliseconds. A
command that meets a disk spinning up, whether at its first access or back
from standby, is treated as a transport fault and reset.

### TMP-2.1 — The medium is detected, first

Everything else in TMP-2 sizes itself from the medium, so this comes first.

1. **A discovered property with an explicit Unknown.** Rotation (rotating,
   with its rate when reported; non-rotating; unknown) and removability
   (removable medium; fixed; unknown) are separate facts, each recorded with
   the source that established it. `BlkMedium` is reshaped in place to carry
   them; `abi-v1` is not frozen, so no variants are added beside the old
   ones.
2. **Sources, strongest first.** Each probe is bounded by its exchange
   deadline, and a refusal means "not reported".
   - VPD page B1h, MEDIUM ROTATION RATE (SBC-4), once VPD page 00h lists it. A
     SAT bridge (SAT-3) derives B1h from the disk's IDENTIFY DEVICE word 217,
     so an enclosure usually reports its disk's rate.
   - ATA PASS-THROUGH IDENTIFY DEVICE (SAT-4), word 217, when B1h is absent
     and the bridge accepts pass-through.
   - INQUIRY RMB describes removability only, never rotation. A card reader's
     "medium not present" (02/3A) confirms a removable medium.
   Many USB bridges mishandle VPD requests, which is why Linux's usb-storage
   skips them by default. So probing is fail-safe: a bridge that hangs or
   resets on a probe is recovered by the transport's reset recovery, recorded
   as not reporting, and still served. Each probe runs once per attach and is
   never retried in a loop.
3. **Each consumer reads Unknown in its own conservative direction**, decided
   explicitly for that consumer rather than inherited from one shared class:
   - *Time budgets:* Unknown means the slowest media (D-3), and only positive
     non-rotating evidence tightens them.
   - *Write-back window:* `lib/writeback`'s `writeback_window_ns` keys on the
     same class but reads it as command cost ("widest where a device command
     is dearest"), and a removable medium can also be pulled mid-window.
     Separate the facts the window needs (command cost, exposure to surprise
     removal) from the budget class, and decide Unknown's window with
     `plans/ARXFS-WRITEBACK.md`; stop and ask if the two disagree.
   - *Description and icon:* Unknown is neither "flash drive" nor "disk"
     (`lib/procinfo/src/volume.rs`, `lib/icon/src/glyph.rs`). It gets a
     generic USB-drive picture and wording, never a claim the system cannot
     back.
4. **Visible.** The detected medium and its source are reported through the
   System Information API, so a misdetection is diagnosed from `sysinfo`
   rather than inferred from timing.

### TMP-2.2 — Readiness is a timed state machine, not a loop

- **Readiness is not readable from `TEST UNIT READY` alone.** A bridge may
  answer ready while its disk is stopped and spin it up on the first media
  access, so readiness is never the only defence against a spin-up (TMP-2.3).
- **Bring-up forces no spin-up.** Nine disks starting at once at boot is a
  power surge the system must not choose; a unit spins up when it is first
  used.
- **Paced and bounded.** `TEST UNIT READY` is paced by one-shot timers and
  bounded by the spin-up budget, as DEVICES.md already stages
  ("one-shot-timed, never a spin"). The sense is classified by (key, ASC,
  ASCQ):

  | Sense (key / ASC / ASCQ) | Meaning | Response |
  |---|---|---|
  | 02/04/01 | becoming ready | wait and retest |
  | 02/04/02 | initializing command required | `START STOP UNIT` with START set, its own budget, then retest |
  | 02/04/03 | manual intervention required | fail closed, stating why |
  | 02/3A | medium not present | the unit is present with no medium, which is not a fault (the media-change pacing DEVICES.md stages) |
  | 06/28 | medium changed | re-read capacity |
  | 06/29 | power-on or reset | retest |

### TMP-2.3 — Budgets derive from the medium and the transfer

- **Every command to a rotating or Unknown unit carries a budget that covers a
  spin-up.**
- **Spin-up in service is ridden out.** A command that meets 02/04/01 is held
  and retried on a timer within the spin-up budget. The unit reports a
  distinct becoming-ready health state rather than a transport fault, so no
  reset recovery runs and no retry is spent.
- **One exchange budget per command, not one constant.** A command's exchange
  budget is the class base latency plus its transfer length at the class's
  minimum throughput, replacing the single `exchange_budget_ns`.
  `SYNCHRONIZE CACHE` has its own budget. The maximum transfer honours VPD
  B0h (Block Limits) when it is reported.

### TMP-2.4 — Consumers pace their reissue

- **Reissue waits for the unit.** A reissuable answer from a recovering unit
  is reissued only on the unit's health edge, which is event-driven (the
  block health notices), or after the pacing interval the driver states. It
  is never reissued at once until the retries are gone.
- **`IoBudget::grace_ns` is used or removed.** Either the consumer reads it
  or the field goes; it is never left unread.
- **Designed with FIX-IO.** This touches the consumer contract that FIX-IO's
  IO3 and IO4 own, so design it with that plan, and stop and ask if the two
  disagree.

### TMP-2.5 — An abandoned transfer's log names the endpoint's state

When the HCD abandons a transfer at its deadline, its log also carries the
endpoint's EP State and the TD's progress, read from the output context before
the stop. A device still working (NAKing) can then be told from a completion
the host lost from the log alone, without guessing.

### Acceptance

The scripted-device tests use the fake clock.

- **Medium (TMP-2.1).** Each case resolves to the medium shown:

  | Unit reports | Detected medium |
  |---|---|
  | RMB set, B1h rate 7200 | rotating, not a stick |
  | RMB set, no VPD, SAT word 217 = 7200 | rotating |
  | B1h rate 0001h | non-rotating |
  | VPD and SAT both refused | Unknown, with the slowest budgets |
  | a probe that hangs | recovered and recorded as not reporting; the unit is still served |

  The budgets, the write-back window, the description and the icon each
  follow their stated reading of Unknown.
- **Readiness (TMP-2.2).**
  - A unit becoming ready after 20 s is served, with timer-paced retests and
    no back-to-back attempts.
  - 04/02 issues `START STOP UNIT`.
  - 04/03 fails closed with its reason.
  - 3A presents a unit with no medium.
- **Budgets (TMP-2.3).**
  - A unit that answers `TEST UNIT READY` at once but whose first read
    answers only after 8 s (spinning up on first access) is served with no
    reset and no retry spent.
  - A mid-service spin-up is ridden out the same way.
  - The flush budget is used for `SYNCHRONIZE CACHE`.
  - Per-command budgets scale with transfer length.
- **Consumers (TMP-2.4).** A recovering unit's reissue waits for its health
  edge.
- **Diagnostics (TMP-2.5).** An abandoned transfer's log carries its endpoint
  state.
- **Metal (TMP-6).**
  - The nine enclosure disks are detected as rotating, or Unknown, and none
    reads as a USB flash drive.
  - A disk spun down by its own standby timer serves its next read.

## TMP-3 — Attached-volume names are a stable property of the volume (D-1)

Implement the scheme D-1 settles, in `drivers/storage/volmgr`
(`name.rs`, `plan.rs`, `program.rs`) and wherever the kernel volume service
must take part (`kernel/tairix-kernel/src/volume_service.rs`).

- **A persistent record, if D-1 chooses one.** It needs an owner with
  authority to write it, a bounded size, eviction, and an audit trail. Design
  it with `plans/NEW-NAMESPACE.md` and the settings write path, introducing
  no capability without a live holder and an enforcement point (§5.2).
- **Labels are hostile input.** Sanitise as today, refuse names that are
  reserved or confusable in the alias grammar (ALIAS.md §5), and never let a
  label choose a path outside `/Storage`.
- **Code and specs agree.** ALIAS.md §10 (fingerprint input and alphabet),
  DEVICES.md D3c and DRIVES.md's examples match the code, whichever side
  D-1 says is wrong.

Acceptance:

- Two unlabelled sticks attached in either order get the same names each
  time.
- A re-plug keeps its name while other volumes come and go.
- A label collision resolves as specified.
- A hostile label cannot escape or spoof a name.
- Every name matches the alias grammar and the kernel's `VolumeName` checks.

## TMP-4 — Removable media are owned according to a per-volume policy (D-2)

Implement D-2's policy in the volume service and
`kernel/tairix-kernel/src/volume_policy.rs`, generalising `GroupMappedFs`
from FAT32 to every removable format rather than adding a second wrapper.
The policy is recorded per volume identity, and changing it is gated by the
mount-policy authority. Prefer `CAP_FS_MOUNT`; add `CAP_FS_MOUNT_RELAX` only
if §5.2's three tests demand it, with its holder and enforcement point in the
same change. Policy decisions are audited.

Acceptance:

- Under the default policy, a `storage` member can create at the root of an
  ext stick made elsewhere.
- A volume honouring ownership presents its on-disk ids unchanged.
- A local uid that matches a foreign one gains nothing under mapping.
- `chmod` and `chown` behave as the policy states: refused under mapping, as
  for FAT32 today, unless D-2 decides otherwise.
- New nodes are stored with the owner the policy names.
- The file manager surfaces a refusal to the user rather than failing
  silently (§2.24).
- DRIVES.md, `docs/src/filesystem/drives.md` and DEVICES.md D3/D4 describe
  the policy.

## TMP-5 — DESK-21: window-channel requests are posted, never awaited

The design, witness and acceptance live in `plans/FIX-DESKTOP.md` DESK-21; the
Pi 4 witness is the file manager blocked in `WindowClient::resize` →
`ipc_call` during a resize. Deliver DESK-21 there; this row closes when that
one does. Nothing here supersedes it.

## TMP-7 — Dialogs, forms and progress are owned windows of their own

### Problem

Four surfaces are modal panels that an application paints inside its own
window. Each has its modality enforced by that application:

- the file manager's operation progress (`lib/browse/src/render.rs`
  `draw_progress_dialog`, painted last into the window's surface;
  `userland/apps/files/src/operation.rs` filters the input);
- TextEdit's Go to line (`userland/apps/textedit/src/view.rs`, a `Dialog`
  over a veiled window);
- Paint's `Modal::Form` family: New Picture, Page and Sprite, Scale, Canvas,
  Convert, Save As and Layer (`userland/apps/paint/src/{view.rs,dialog.rs,render.rs}`);
- Settings' `CredentialSheet` (`userland/apps/settings/src/shell.rs`).

`lib/controls` supports this through `Dialog::placed_over` and
`CredentialSheet::centred_in`. Such a panel is clipped to its window: the
progress panel in a small window "simply shows what fits". It cannot be moved,
or raised apart from its window. Every application re-implements modality.
`plans/APPWIN.md` AW8 permitted all of this ("Forms stay in their windows");
on 2026-10-17 the user reversed that rule, and APPWIN.md §0 now forbids it.

The only framed window a desktop app can own today is an alert, which the
session draws from a fixed spec. A form or a progress window needs content the
application draws itself, and the protocol has no such kind: `Create` has no
owner, `CreatePopup` is undecorated, and `CreateTool` is a floating palette.

### Design

1. **An owned dialog window.** A new transient kind in the AW6/AW7 family
   (`TransientSpec`, `WindowServer::open_transient`). It is framed by
   `WindowFrame::dialog()` and is a transient of one of the caller's own
   top-level windows, one level only. It stays off the taskbar and the
   window picker, rises and hides with its owner's family, and closes with
   its owner. The session places it, centred over its owner and clamped on
   screen.
2. **Modality is the window manager's, one implementation for alerts and
   dialogs.** An owner-modal dialog holds its owner exactly as an AW8 alert
   does: only a release reaches the owner's client, a press there focuses the
   dialog, and the owner takes no drop. A modeless dialog holds nothing. The
   per-application input filters and veils are deleted.
3. **Identity.** The frame names the owning application from its attested
   identity, as alerts are titled, so no application's dialog can pass for
   the desktop's own prompts (the session's elevation prompt among them). A
   credential form is phishing-sensitive; the design states how a user tells
   an application's credential sheet from the system's, and the review
   checks it.
4. **Focus and keys follow AW8:** focus is earned, buttons arm after
   `ALERT_ARM_NS`, `Enter`/`Escape` take the default and cancel, and the
   focus hands back as the dialog closes.
5. **Sized from content (TMP-8), never clipped.** The application states its
   client extent from its content, and the session caps it to the screen. Past
   the cap, the content scrolls inside the dialog.
6. **The in-window path is removed, not left beside.** `Dialog::placed_over`,
   `CredentialSheet::centred_in` and the veils are deleted with their last
   caller. The shared controls then offer no way to place a dialog over an
   application's own content, so the APPWIN.md rule is enforced structurally,
   not only by review.

Stages:

- **TMP-7.1:** the protocol (with a round-trip test and a fuzzed fail-closed
  decode), the engine, session placement, shared modality and earned focus.
- **TMP-7.2:** TextEdit, Paint and Settings move to dialog windows, and the
  helpers are deleted.
- **TMP-7.3:** the file manager's operation progress, as D-4 decides.

Each stage updates the docs it touches: `docs/src/desktop/apps.md` (the
in-window description) and `wm.md`. Two descriptions are already stale and are
corrected in passing:

- the terminal settings rustdoc still says "in-window", though it is a popup
  (`userland/apps/terminal/src/{settings.rs,lib.rs}`);
- `plans/NEW-FILEMANAGER.md` still describes Delete as a `Dialog`, though it
  is an alert.

### Acceptance

- **Library (`lib/window` loopback).** The owner must be the caller's own
  top-level window. A transient of a transient is refused. Dialogs share the
  per-client budget, close with their owner, and a refused request commits
  nothing.
- **Session.**
  - The dialog is framed, off the taskbar, and titled with the application's
    attested name.
  - It is placed over its owner and clamped on screen.
  - Owner modality behaves exactly as for an alert, and a modeless dialog
    holds nothing.
  - Focus is earned and buttons arm.
  - It is sized from its content.
- **Per application.** Each migrated surface opens its window. No veil
  remains. The application no longer filters its own input for modality.
- **Metal.** A copy's progress is its own window (TMP-6).

## TMP-8 — Alerts and dialog windows size to their content; nothing is elided

### Problem

`lib/controls/src/alert.rs` forces every alert to 4:3:

- `client_extent` searches for the narrowest 4:3 width that fits;
  `four_thirds` and `client_of` force the height to three quarters of the
  width.
- A short message is padded out to that height.
- A headline past `HEADLINE_LINES` (6) lines is cut with an ellipsis
  (`lib/controls/src/paint.rs`), and `needed_height` measures that capped
  height. This contradicts APPWIN.md AW8's own "nothing is elided": a defect,
  `plans/OPEN-DEFECTS.md` D1129.

On 2026-10-17 the user asked that alerts and progress windows size vertically
to their content, with nothing hidden.

### Design

One content-sized layout serves alerts and TMP-7's dialogs (one definition,
§2.2):

- **Width.** The narrowest width at or above `ALERT_MIN_WIDTH` that keeps the
  text within a readable measure and seats the button row in as few rows as
  possible, capped at the screen's share. The measure is a line length set
  from the font's metrics and scale, not a pixel constant.
- **Height.** Exactly what the laid-out content needs, frame included,
  capped at the screen's usable height.
- **Past the cap.** The whole text area scrolls, headline included, and the
  button row stays at the foot. The headline cap and its ellipsis are
  deleted.
- **Bounds.** The protocol's containment bounds (headline, detail and label
  bytes) stay, since they are security bounds (§24.4). Within them, nothing
  is elided.
- **Scale.** Everything is laid out in logical pixels through
  `tairix_geometry::Scale`. A density change re-lays the window out.

### Acceptance

The `lib/controls` tests (`alert_tests.rs` asserts 4:3 today and is rewritten,
not weakened) cover:

- the height equals the content height;
- a one-line alert is short;
- a long headline wraps whole, with no ellipsis;
- past the screen cap the text scrolls and the buttons stay;
- the width honours the measure and the minimum;
- a density change re-lays the window out.

The session suite places the window by its new extent. APPWIN.md AW8,
`docs/src/desktop/wm.md` and the `lib/controls` rustdoc state the layout.

## TMP-6 — Raspberry Pi 4 acceptance on metal

The operator's run on the Pi 4, with the system log captured:

- a stick that stalls, halts or NAKs does not freeze the keyboard (TMP-1);
- a spun-down USB hard disk serves its next read without failing closed, and
  the nine USB-enclosure disks of the 2026-10-17 boot are published and
  probed with no exchange timing out and no transport reset (TMP-2);
- unplugging and replugging several sticks in a different order keeps every
  name (TMP-3);
- an ext stick made on another system is writable as D-2 decided (TMP-4);
- a drag-resize of a file window produces no stall report (TMP-5);
- a copy's progress, Go to line, a Paint form and the Settings credential
  sheet each open as a window of their own (TMP-7);
- an alert with a long headline shows all of it, and a short one is short
  (TMP-8).

Each witness is quoted in the completion report. Then this plan is deleted.
