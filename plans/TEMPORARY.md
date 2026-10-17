# TEMPORARY.md — Open work from the Raspberry Pi 4 USB-storage investigation

## Ledger

| ID | Item | Status |
|---|---|---|
| TMP-0 | Decisions D-1 to D-4 put to the user, answered and recorded under each | done |
| TMP-9 | A reissuable answer carries its driver's word on when to reissue (D-5): at once when the device's own recovery has made the unit ready | done |
| TMP-1 | The USB host controller's serve loop never waits: every control transfer, controller command, port reset and recovery is an operation driven by events and one-shot timers (`plans/OPEN-DEFECTS.md` D1125) | done |
| TMP-1.1 | Each device's EP0 ring, control buffer and input context are resident in its own region; the engine-wide active/resting control cursor is deleted | done |
| TMP-1.2 | Control transfers are queued and reaped as bulk TDs are; a control URB carries a deadline and is held, never answered inline | done |
| TMP-1.3 | Controller commands are posted and routed by command-TRB address; a hung command is aborted, failing only its owner | done |
| TMP-1.4 | Endpoint recovery, deadline abandonment, `ClearHalt`, `SetInterface` and stream stop are operations | done |
| TMP-1.5 | Enumeration, hub port service, root-port reset, slot teardown and the boot walk are one per-port operation | done |
| TMP-1.6 | The engine's synchronous primitives are deleted and a host test proves the loop never waits | done |
| TMP-2 | Mass-storage readiness and deadlines follow the detected medium | done |
| TMP-2.1 | The medium is detected, with an explicit Unknown that every consumer reads conservatively; first, since every budget derives from it | done |
| TMP-2.2 | Readiness is a timed state machine: spin-up, `START STOP UNIT`, no medium | done |
| TMP-2.3 | Budgets derive from the medium and the transfer; a spin-up in service is ridden out | done |
| TMP-2.4 | Consumers pace their reissue on the unit's health | done |
| TMP-2.5 | An abandoned transfer's log names the endpoint's state | done |
| TMP-3 | Attached-volume names come from the volume's own label, never read as a format, and resolve collisions deterministically | done |
| TMP-4 | Removable media honour their on-disk ownership, and a refusal is stated to the user | done |
| TMP-5 | DESK-21 — window-channel requests are posted, never awaited (`plans/FIX-DESKTOP.md`) | done |
| TMP-6 | Raspberry Pi 4 acceptance of TMP-1 to TMP-5 and TMP-7 to TMP-9 on metal | blocked: the operator's Pi 4 run |
| TMP-7 | Dialogs, forms and progress are owned windows of their own; a modal panel drawn inside an application's window is forbidden | done |
| TMP-7.1 | The owned dialog window: protocol, session placement, owner modality shared with alerts, earned focus | done |
| TMP-7.2 | TextEdit's Go to line, Paint's forms and Settings' credential sheet become dialog windows; the in-window placement helpers are deleted | done |
| TMP-7.3 | Each file operation's progress is a modeless window of its own; the file manager runs a bounded, ordered set of operations | done |
| TMP-8 | Alerts and dialog windows size to their content; nothing is elided (`plans/OPEN-DEFECTS.md` D1129) | done |

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
it lands before or with TMP-7.1, and TMP-7.3 follows TMP-7.1. TMP-6 comes
last.

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

*Answer (the user, 2026-10-17):* the volume's own label is the primary name
source, as on other systems, and collisions are handled sensibly.

What the answer means for the design (TMP-3):

- **The label is the name**, read where each format keeps it
  authoritatively, and sanitised so it stays recognisable: a run of characters
  the alias grammar refuses becomes one `-` rather than vanishing, so
  `My Backup Disk` is `my-backup-disk`. A format's placeholder for "no label"
  is not a label.
- **An unlabelled volume is named from the volume alone**, never from its
  format or its ordinal: the device's transport and the volume's short
  fingerprint (`usb_25cm`). No two unlabelled volumes collide by construction,
  so their names do not depend on attach order.
- **A collision is resolved deterministically.** A label already in use takes
  `_<fingerprint>` of the volume's own identity, lengthened per further
  collision. A given volume always derives the same suffix; the bare label
  belongs to whichever volume holds it. There is no persistent record.
- **Generated names are unforgeable.** A label never yields `_`, so no label
  can spell a name generated for another volume.

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

*Answer (the user, 2026-10-17):* honour the on-disk ownership. A node owned
by a privileged user is simply unreachable to a lesser one, so honouring it
loses access and grants none. What a medium yields when moved to another
system is that system's concern, not TAIRiX's.

What the answer means for the design (TMP-4):

- **Formats that record ownership** (ext2/3/4, ARXFS) are served with their
  on-disk uid, gid, mode and ACL unchanged, removable or not. There is no
  ownership mapping, no per-volume ownership policy and no "take ownership"
  operation; changing an owner is `chown` under the authority it already
  needs on any volume.
- **Formats that record none** (FAT32) keep the `storage`-group mapping,
  since there is no on-disk owner to honour.
- **A refusal is an answer** (§2.24): the file manager tells the user that
  the volume's owner refuses the operation, rather than failing silently.
- No capability is introduced, since there is no policy to gate.

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

*Answer (the user, 2026-10-17):* modeless, one window per operation; owner
modality stays settable, not preferred, for where it is absolutely required.

What the answer means for the design (TMP-7.1, TMP-7.3):

- **Modality is the dialog's own property**, stated by the application when
  it opens one: modeless is the preferred form, and owner-modal is used only
  where the dialog's content absolutely requires the owner held (a form that
  edits the owner's document, a credential prompt).
- **Each file operation opens its own modeless progress window**, and the
  folder window stays usable while it runs.
- **The file manager runs a bounded set of operations** and orders those that
  touch the same tree, queueing a later one behind an earlier one it
  conflicts with.

### D-5 — How soon a consumer reissues after a recovered reset (gates TMP-9)

What the code does: a request a stalling unit answers reissuably is reissued
after `IoBudget::reissue_pacing_ns`, the grace window spread evenly over the
class's retries — 30 s for a rotating disk (120 s ÷ 4). The driver's
escalation ladder (`RecoveryLadder`) climbs one rung per reissue it receives:
a plain retry, then transport resets. The pacing is therefore also what keeps
a disk still spinning up from being reset again too soon.

The witness is the 2026-10-17 boot: one disk's exchange went unanswered for
its 30 s budget, the transport reset at 50.163 s worked, and the reissue that
succeeded came at 80.176 s — 30 s spent waiting on a device that was ready.
Linux retries a command at once after a successful reset.

Options:

- **(a) Keep the even spread.** Simple, and no reissue can catch a disk still
  spinning up; every recovered blip costs a full pacing interval.
- **(b) Back off from short.** The first reissue goes almost at once, each
  later one waits twice as long, the sum still spanning the grace window, and
  the ladder's first reset waits for the second reissue rather than the
  first.
- **(c) The driver says when to reissue.** A reissuable answer carries the
  driver's own readiness: once its reset has completed and the unit answers
  `TEST UNIT READY`, the next reissue is served at once; until then the
  consumer waits the paced interval. Most precise, and a change to the
  `blkio` answer the driver and both consumers read.

Recommendation: **(c)**, since only the driver knows whether its recovery has
worked; (b) if a protocol change is not wanted.

*Answer (the user, 2026-10-17):* (c), the driver says when to reissue.

What the answer means for the design (TMP-9):

- **The word is the driver's, and only brings a reissue forward.** A
  reissuable completion carries `BlkReissue::Now` or `Paced`; the consumer
  reissues at once on `Now` and waits out its pacing otherwise, and its retry
  budget bounds both, so an untrusted driver can neither stall nor storm it.
- **`Now` means the unit answered ready after the recovery.** A transport whose
  reset recovery resynced it says so; only then is the unit asked `TEST UNIT
  READY`. One coming ready is held as any not-ready unit is, and one without
  its medium is retracted.

## TMP-1 — The USB host controller's serve loop never waits (D1125)

Done. The design and what the engine now guarantees are `plans/USB.md` U14;
the engine and the HCD loop are described in `docs/src/lib/usb.md`,
`docs/src/drivers/bus.md` and `lib/usb/README.md`. The metal acceptance — a
mass-storage stick that stalls, halts or NAKs never freezes the keyboard — is
TMP-6's.

## TMP-2 — Mass-storage readiness and deadlines follow the detected medium

Done. The medium record and the budgets derived from it are
`docs/src/drivers/block.md` and `plans/FIX-IO.md`; readiness, spin-up and the
held request are `drivers/storage/usb_msd/README.md` and `plans/DEVICES.md`;
the abandoned transfer's endpoint state in the HCD's log is `plans/USB.md`
U13. The metal acceptance is TMP-6's.

## TMP-9 — The driver says when a reissue may come back (D-5)

Done. The completion's reissue word, the engine asking the device
(`Block::reissue`) and the consumers' one decision
(`IoBudget::reissue_wait_ns`) are `docs/src/drivers/block.md` and
`plans/FIX-IO.md` IO3; the readiness test after a resynced reset is
`drivers/storage/usb_msd/README.md`. The metal witness is TMP-6's.

## TMP-3 — Attached-volume names come from the volume's label (D-1)

The volume manager names a volume from itself (`drivers/storage/volmgr/src/name.rs`):

- **The label**, read where the format keeps it (`lib/fsprobe`): ext's
  `s_volume_name`; FAT32's root-directory label entry, located through the
  shared `Fat32Bpb` reading the FAT32 driver also uses, else the boot
  sector's copy; `NO NAME` is no label. It is kebab-cased: ASCII letters
  lowercased, digits kept, each run of anything else one `-`, trimmed.
- **Unlabelled**: `<transport>_<fp4>` from the device's declared medium
  (`disk_` when it declared none).
- **Collisions and reserved names**: a label in use, or one spelling a
  root-view entry, a storage resolver (`tairix_path::RESOLVER_NAMES`) or a
  resource namespace (`tairix_resref::KnownNamespace`), takes `_<fp4>`, then
  `_<fp8>`, then the whole fingerprint.
- **Kernel grammar**: `validate_volume_name` admits lowercase `a-z0-9-_`,
  beginning and ending with a letter or digit, and nothing else.
- **Specs**: ALIAS.md §3.8/§10.1 now state the code's fingerprint (lowercase
  Crockford base32 of the raw identity, every bit encoded), and `lib/resref`
  accepts the grouping dash §10.2 describes; DEVICES.md D3c, DRIVES.md §7.5
  and `docs/src/filesystem/drives.md` §7 and §10 describe the catalog names
  the code produces, a catalog entry being distinct from an alias.

## TMP-4 — Removable media honour their on-disk ownership (D-2)

- **Proven.** `an_ext_volume_keeps_its_on_disk_ownership_scenario`
  (`kernel/tairix-kernel/src/volume_service_tests.rs`) attaches an e2fsprogs
  volume served as a removable USB unit: its root (`0:0 0755`) and an ACL'd
  file present their records unchanged and the ACL decides; a non-owner is
  refused at the top level; a node made there carries its creator; `chmod`
  and `chown` follow the owner and `CAP_FS_CHOWN` rules, and the medium
  stores the result. FAT32 still refuses both
  (`assert_a_member_creates_on_the_volume`). Proving it found D1130: the
  delegated VFS authorised a file's bytes against its directory, now the
  node itself.
- **Visible.** A refused New ▸ names the item and the reason
  (`item_refusal`, `create_refusal`); a listing refused on a rescan, a
  climb, after an operation, from the places rail or from Close's folder
  climb is stated in the window as well as on `stderr`.
- **Stated.** `docs/src/filesystem/drives.md` §20, DRIVES.md §20 and
  DEVICES.md D3d state the rule; no document claims the mount-relax
  capability exists.

## TMP-5 — DESK-21: window-channel requests are posted, never awaited

Delivered as `plans/FIX-DESKTOP.md` DESK-21, which records what it now
guarantees; the Pi 4 drag-resize witness is TMP-6.

## TMP-7 — Dialogs, forms and progress are owned windows of their own

Done. The durable statement is `plans/APPWIN.md` §0 and
`docs/src/desktop/wm.md` (Dialog windows); what each surface became is in its
own page.

- **The owned dialog window.** `WindowRequest::CreateDialog` (round-trip test,
  fuzzed fail-closed decode) opens a transient of one of the caller's own
  top-level windows through the engine's one transient path
  (`TransientKind::Dialog`): one level only, the per-client budget shared, a
  refused open committing nothing, closed with its owner. An alert may hold a
  dialog, and closing a window dismisses every alert holding it or a transient
  that closed with it. `WindowClient::create_dialog`, `WindowPane::open_dialog`.
- **Placed, named and fitted by the session.** `WindowFrame::dialog()`,
  centred over its owner within the work area, off the taskbar and picker; its
  band reads the bundle's attested name and then the application's title, and
  a retitle keeps the name first. One past the work area is fitted and told by
  `Resized`, on opening and on every resize.
- **One hold rule.** `hold::Holds` is the session's single record of which
  window is held by what, shared by alerts and owner-modal dialogs: only a
  release reaches a held window, a press on it raises and focuses its front
  holder, it takes no drop, and as a holder closes the keyboard goes to the
  holder behind it, else the window. Focus is earned as an alert's, spending
  the activation that earned it; a dialog takes no press until `ALERT_ARM_NS`
  after the frame that first shows its content.
- **Telling the system's prompts apart.** The session's credential and
  confirmation prompts are undecorated, centred on the screen and hold the
  whole seat; an application's dialog is framed, names its application first,
  sits over its own window and never holds the seat.
- **The surfaces moved.** TextEdit's Go to line (modeless) and Paint's forms
  (owner-modal: their rows are filled from the picture they act on) are drawn
  beside the document's area and shown in a dialog through the document host
  (`DocumentView::dialog_window`, `dialog_input`), which fits and scrolls a
  dialog the desktop shrank. Settings' credential question (owner-modal) has
  its own window in the settings loop. Each file operation has a modeless
  progress window of its own; a folder window may run several, bounded by the
  operations desk and run in the order asked, so one touching a tree an earlier
  one works in waits behind it and says so. None of the applications filters
  its own input for modality.
- **Deleted.** `Dialog::placed_over`, `CredentialSheet::centred_in`, the
  TextEdit and Paint veils, the file manager's `operation_control` and
  in-window progress panel, and the settings shell's modality filters.

## TMP-8 — Alerts and dialog windows size to their content; nothing is elided

`lib/controls/src/alert.rs` lays an alert out from its content (D1129 closed):

- **Width**: the text's widest line at a readable measure (`MEASURE_EMS`, 30
  ems of the body face, so it follows the density), plus the mark and pads,
  never below `ALERT_MIN_WIDTH` or the single-row button width, capped at the
  screen's share.
- **Height**: exactly the content — mark or text, whichever is taller, and the
  button rows — capped at the screen's share.
- **Past the cap**: the whole text, headline first, scrolls by line
  (`TextLines`), and the buttons stay at the foot. The headline cap and the
  4:3 shape are gone; the protocol's byte bounds stay.
- **Dialog windows** (TMP-7) take the same rule: their content states its
  extent, the session fits it to the screen, and what does not fit scrolls.

## TMP-6 — Raspberry Pi 4 acceptance on metal

The operator's run on the Pi 4, with the system log captured:

- a stick that stalls, halts or NAKs does not freeze the keyboard (TMP-1);
- a spun-down USB hard disk serves its next read without failing closed, and
  the nine USB-enclosure disks of the 2026-10-17 boot are detected as rotating
  or Unknown, none reading as a USB flash drive, each reported rotating drawn
  as a hard disk, and are published and probed with no exchange timing out
  and no transport reset (TMP-2);
- a disk whose exchange stalls holds up no other disk's volumes: the others
  publish while it recovers (`plans/OPEN-DEFECTS.md` D1134), and it is served
  again as soon as its reset finds it ready, not after a paced wait (TMP-9);
- unplugging and replugging several sticks in a different order keeps every
  name (TMP-3);
- an ext stick made on another system presents its own ownership, and a
  refused write is stated to the user (TMP-4);
- a drag-resize of a file window produces no stall report (TMP-5);
- a copy's progress, Go to line, a Paint form and the Settings credential
  sheet each open as a window of their own (TMP-7);
- an alert with a long headline shows all of it, and a short one is short
  (TMP-8).

Each witness is quoted in the completion report. Then this plan is deleted.
