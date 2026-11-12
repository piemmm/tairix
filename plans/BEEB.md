# BEEB — the BBC Micro emulator (`beeb.app`) and its clean-room ROMs

Binding under `AGENTS.md`. This plan owns `beeb.app`, an emulated BBC Micro
Model B delivered as an ordinary sandboxed desktop application, and the three
ROMs it ships: an operating system equivalent to MOS 1.20, an interpreter
equivalent to BBC BASIC II, and a disc filing system equivalent to Acorn DFS
1.20, each written for TAIRiX from published interfaces and black-box
observation.

Read first: `plans/APPWIN.md` (the window channel), `plans/SOUND.md` (the
`audio-v1` stream), `plans/CAPABILITY_USE.md` CU6 (the picker's one-shot
grants), `plans/NEW-MENUS.md`, `plans/ICONS.md`, `plans/SHADER.md` §0 item 5
(guest-processor code is authored through a Rust builder), and
`docs/src/security/sandbox.md`.

## Ledger

| Item | What it is | Status |
|---|---|---|
| BB0 | This plan, its jump-sheet row and its `PLAN.md` row | done |
| BB1 | The NMOS 6502: the one instruction table, every opcode with its exact bus cycles, decimal mode, interrupt sampling, and the conformance vectors | done |
| BB2 | The Model B bus: memory map, sideways ROM/RAM paging, 1 MHz cycle stretching, the floating bus, and the event scheduler | done |
| BB3 | System and user VIAs, the keyboard matrix and its links, the SN76489, the uPD7002, the 6850 ACIA and the serial ULA | done |
| BB4 | Video: the 6845, the video ULA, the SAA5050 and its glyph set, and the sync-locked beam canvas | done |
| BB5 | Discs: the 8271, two double-sided drives, and the SSD and DSD images, written back | done |
| BB6 | Tape: the cassette interface, UEF and CSW images, recording, and the motor relay | done |
| BB7 | Machine snapshots: the versioned container and every device's state | done |
| BB8 | The 6502 builder, `cargo xtask beeb --write-roms`, the committed images and their drift gate | in progress |
| BB9 | The clean-room MOS | in progress |
| BB10 | The clean-room BASIC | planned |
| BB11 | The clean-room DFS | planned |
| BB12 | The reference harness and the recorded observation corpus | planned |
| BB13 | The machine worker and its protocol | planned |
| BB14 | The front end: window, scaling, input mapping, audio pacing and accelerated tape loading, media, menus, settings, status | planned |
| BB15 | The OS facilities the front end needs: lock and ISO keys, menu symbols, disc and tape content types, gzip members in `lib/compress` | in progress |
| BB16 | The bundle: manifest, icon, help in every required locale, docs page, inventory pins | planned |
| BB17 | Fuzz harnesses over every untrusted decoder, and the QEMU vertical | in progress |
| BB18 | Software acceptance over a recorded corpus | planned |

Items are built in ledger order; each lands complete with its tests.

## 0. Binding decisions

1. **One machine: the 32K BBC Model B.** An NMOS 6502 at 2 MHz, 32 KiB of
   RAM, sixteen sideways slots, two VIAs, the 6845/ULA/SAA5050 display, the
   SN76489, an 8271 with two double-sided drives, cassette, the uPD7002 and
   the printer port. B+, Master, Electron, Tube, Econet and speech are not
   emulated; a probe for one finds it absent exactly as on an unexpanded
   machine.
2. **The guest is hostile.** Guest code, ROM images, disc and tape images and
   snapshots are untrusted input. All of it is decoded and executed only in
   the machine worker, a `lib/sandbox` session worker holding no capability.
   The front end validates every worker reply as it would any untrusted
   message, and survives a worker that crashes, stalls or lies.
3. **The ROMs are authored in Rust.** Each ROM is Rust code driving the
   crate's 6502 builder; `cargo xtask beeb --write-roms` emits the 16 KiB
   images into the bundle's `Resources/`, they are committed, and a static
   gate and a crate test rebuild them and compare bytes. No 6502 assembly
   text, Python or other tooling enters the tree.
4. **Compatibility is with the documented machine.** Each ROM implements its
   original's documented interface, keeps its documented workspace at the
   documented addresses, and places internal state where software is known
   to read it. Private routine addresses inside a ROM are not reproduced;
   documented entry points, vectors, tables and workspace are.
5. **The clean-room boundary.** Implementation inputs are the published
   manuals (the User Guide, the Advanced User Guide), data sheets, and
   observations of genuine ROMs run as opaque programs: what they print,
   what they write to documented workspace, how they drive hardware. No
   original ROM's code, disassembly or annotated listing is read. Genuine
   ROM bytes never enter the tree, a fixture or a build.
6. **Genuine ROMs are the user's choice.** A user may install their own ROM
   images in any slot or as the OS; the built-in clean-room ROMs are the
   default, so the emulator works out of the box.
7. **Timing is cycle-exact.** Every CPU bus cycle, dummy accesses included,
   happens at its true cycle; devices are advanced to the cycle of each access
   that can observe them; video is produced at the beam position of the cycle
   that fetched it. Raster tricks, mid-line register writes and timer-to-raster
   synchronisation behave as on hardware.
8. **The audio clock paces the machine.** With an audio stream the machine
   runs exactly as fast as the device consumes its samples, so the two cannot
   drift; without one, the monotonic clock paces it. Video presents whichever
   frame is newest.
9. **Determinism.** The machine is a pure function of its ROMs, media and
   input events with their cycle stamps. Its power-on state is fixed (RAM
   clear, as the MOS leaves it at power-on anyway), so snapshots and
   recordings replay exactly.

## 1. Crate shape

One crate, `userland/apps/beeb` (`tairix-beeb`, bundle `beeb.app`, title
*BBC Micro*). An application crate may depend only on `lib/*`, so the machine,
the ROM sources and the front-end model are modules of its `[lib]`, and the
one `Run` binary is both the front end and, re-spawned through `@self` in its
sandbox role, the machine worker.

- `cpu` — the 6502: `isa` (the single opcode table: mnemonic and mode per
  opcode, from which the CPU dispatch, the disassembler and the builder's
  encoder are all derived), the core, `disasm`.
- `machine` — the Model B: the bus, `via`, `keyboard`, `sound`, `adc`,
  `acia`, `serial`, `crtc`, `ula`, `teletext`, `video`, `fdc` (the 8271 and
  its two drives), `cassette`.
- `snapshot` — the snapshot container and the codec every device's state
  goes through.
- `media` — the disc and tape image decoders.
- `rom` — host-only (`cfg(not(freestanding))`): the builder and the three ROM
  sources. The freestanding binary never compiles it; xtask and the crate's
  tests drive it.
- `proto` — the worker protocol, encoded and validated on both sides.
- `front` — the host-tested front end: key mapping, the presentation scaler,
  pacing, menus, settings, media handling.

## 2. The machine model

**Clock.** One `u64` count of 2 MHz cycles since power-on. 1MHzE is high in
odd cycles. An access to `&FC00–&FDFF`, `&FE00–&FE1F` or `&FE40–&FEDF`
stretches to the next coincident falling edge: one extra cycle when it begins
in an even cycle, two when it begins in an odd one. `&FE20–&FE3F` (video ULA,
ROM select) and `&FEE0–&FEFF` run at 2 MHz. An unmapped read returns the
floating bus: the last byte the data bus carried.

**Scheduler.** Each device states the next cycle at which its outputs can
change without a CPU access (a timer underflow, a scan-line boundary, a byte
under the disc head, a tape bit edge). The machine processes events when the
clock reaches the earliest, and before any access to a device it advances that
device to the access cycle. Interrupt lines are therefore exact at every cycle
the CPU samples them.

**CPU.** Every opcode, documented or not, performs the NMOS bus cycle
sequence, dummy reads and writes included. IRQ and NMI are sampled at the end
of every cycle and taken on the penultimate cycle's sample; a taken branch
that does not cross a page ignores an interrupt first seen in its second
cycle; NMI hijacks BRK and IRQ until the vector fetch; JAM halts until reset.
Decimal mode reproduces NMOS flags for every input, valid BCD or not.

**Video.** The 6845 is stepped per character clock; the ULA produces 16 MHz
pixel slots; the SAA5050 receives one byte per 1 MHz character with the
Model B's display delay and produces 12 half-dots per character with
character rounding, drawn into 16 slots by coverage. The beam is locked to
HSYNC and VSYNC, so a program moving sync moves the picture. The canvas is
896 × 600: 56 µs of each line from 8 µs after the HSYNC edge, and 300 lines
of each field from 16 lines after the VSYNC edge, fields woven for interlace.

**Frame encoding.** One byte per canvas pixel: foreground colour (bits 0–2),
background colour (bits 3–5), foreground coverage 0–3 (bits 6–7). A bitmap
pixel is coverage 3. The front end expands through one 256-entry table, so
anti-aliased teletext costs nothing extra.

**Sound.** The SN76489 is clocked at 4 MHz, updating at 250 kHz; its exact
250 kHz output is resampled to 48 kHz by `tairix_audio::resample` at the exact
ratio 125:24. No second resampler exists.

**Discs.** The 8271 runs its command set against drives modelled by rotation:
300 rpm, ten 256-byte sectors per track of an FM single-density image, a byte
under the head every 64 µs and an NMI per byte. SSD and DSD images are read
and written back: a disc encodes to its image's own length, longer only as far
as a guest wrote past it, and reports any track holding what a sector dump
cannot record. The machine reports a disc written since it was last saved, and
the front end writes it back through its delegated handle.

**Tape.** UEF chunks and CSW pulse runs are turned into the cassette signal
the serial ULA demodulates; the motor relay gates the tape. With record down,
what the 6850 sends is recorded over the tape, which saves back as UEF.
Accelerated loading runs the machine unpaced while the motor is on, with
sound muted.

**Bus monitor.** While asked, the machine records every access to the I/O
pages, its cycle, address, value and direction, for the reference harness and
the debugger, keeping a bounded number and dropping the oldest; off, it costs
one test per access.

**Snapshots.** A versioned container (`BEEBSNAP`, a version, the body's
length, then the body compressed with `lib/compress`'s RLZ codec). The body is tagged,
length-prefixed little-endian sections: the ROM fitting, each ROM by its
`lib/crypto` fingerprint and each sideways RAM bank by its contents; the
machine's time; every register; RAM; and each device's state, the discs and
tape in the drives, the picture being drawn and the last one shown among it,
so a restored machine shows at once what it showed. Sound and output the
front end has not taken are not kept. A snapshot restores only onto the
fitting it records. Decoding checks every field against the range its device
can hold it in and every time against the machine's clock, refuses the whole
snapshot on any failure, and changes nothing until all of it has passed; a
snapshot never names a file.

## 3. The ROMs

**Builder.** `rom::asm::Asm` emits one image from its origin: typed operands
(`Zp`, `Abs`, `.x()`, `.y()`, `.ind_y()`, `imm`), labels with forward
fixups, branch-range checks, fixed-address placement that refuses overlap,
data directives, and a symbol map. Encoding comes from `cpu::isa`; only
documented opcodes are encodable. Hardware addresses come from `machine`, so
a register address is defined once for both the emulation and the firmware.

**MOS.** The whole MOS 1.20 interface: reset in its three forms, the
vectors and extended vectors, sideways ROM services, OSBYTE 0–255, OSWORD
0–13, OSCLI and its commands, the VDU drivers in all eight modes with text
and graphics windows, PLOT 0–31 and 64–95 with the rest offered to VDUV,
palette and flashing, cursor editing, the
keyboard with soft keys and auto-repeat, buffers and events, SOUND and
ENVELOPE, serial and printer streams, `*SPOOL` and `*EXEC`, the ADC, and the
cassette and ROM filing systems. Workspace follows the Advanced User Guide:
the vectors and OSBYTE variables of page &02, the VDU variables of page &03
as OSBYTE &A0 reads them, the documented zero page from &90, the buffers and
pages &08–&0D.

Behaviour pinned by observation, each with its test in `rom::mos::tests`:

- A line is drawn from whichever end comes first along its longer axis, so
  both directions plot the same points; a diagonal is drawn from the target
  unless it rises to the right. The dot pattern starts at that end and runs on
  through clipped points. An open line (PLOT 8–15, 24–31) plots its target
  once more, which only an inverting action shows.
- The external graphics cursor keeps the coordinates a PLOT was given; VDU 29
  keeps the cursor's pixel and recomputes its external form, and VDU 26 clears
  the origin and both cursors. A fill's run makes its start the previous point
  and its end the cursor, scaled back from pixels; a fill that fills nothing
  leaves the target a line up, its high byte unchanged, as the previous point.
- VDU 5 keeps its cursor in pixels: cursor controls 8–13, 30 and DEL move it a
  cell, a step that leaves the window wraps to the cell at its other edge and
  an x wrap carries y a line, and DEL fills the cell in the background colour
  whatever the action. VDU 31 still moves the text cursor.
- The keyboard interrupts on CA2 while no key is down; the interrupt stands
  down and sets the semaphore at &242, and once every key is up the tick arms
  it again only if the semaphore is still set. OSBYTE 178 is that semaphore.
- A numeric command (`*FX`, `*OPT`, `*TV`, `*TAPE`, `*MOTOR`, `*CODE`) takes
  decimal numbers 0–255, the first after spaces and each later one after a
  comma or spaces, and nothing but spaces after them; anything else is Bad
  command.
- KEYV is entered with C clear and V set for a key press, and with both set
  by the tick only while a key is down or the keyboard waits to rearm; a scan
  leaves CA2's flag clear, so rearming takes no second press.
- The soft keys lie at &B00 as MOS 1.20 keeps them: an offset from &B01 to
  each key's text, the text in the order it was defined, an empty key at the
  end's offset, 238 bytes in all. A definition takes the old text out first
  and is read straight into place; OSBYTE 18 and a hard reset set the whole
  page to &10. Any `*KEY` during an expansion is Key in use.
- GSREAD keeps spaces inside quotes, leaves Y at the next word after a
  string's end, and refuses `|` before the CR as Bad string.
- An OSBYTE or OSWORD nothing takes is refused: X &FF, N and V set, C clear,
  Y kept but 0 for an OSBYTE below &75. OSWORD &E0 on goes to USERV. INKEY
  (-256) is &FF. OSWORD 0 returns the line's length in Y; OSWORD 5 and 6
  leave the interrupt flag as they found it.
- Entering a language leaves the error pointer at its copyright string, so a
  REPORT before any error prints it; a hard reset keeps the RTI at &D00.
- The cassette's OSARGS and OSFIND keep X and Y. OPENOUT's prompt ends its
  line, a written file's first block shows straight under it, its last ends
  with the bell and a new line, and its addresses are &FFFFFFFF. A silent
  sound channel leaves REMV alone until a note is queued on it.

Deliberate divergences: the character set is this ROM's own design, so a
glyph's pixels differ from MOS 1.20's; and &31A, the VDU driver's scratch for
the last addressed pixel's line, is not reproduced for a line clipped at its
start or the extra plot of an open line, which MOS 1.20 takes from other
points of its own drawing order.

BB9 remains: the keyboard workspace as MOS 1.20 leaves it once keys are used
(&2CA–&2CE, &2EA–&2FF); sound and envelopes checked against recorded chip
writes, including the final silence MOS 1.20 writes three times; cursor
editing, the remaining OSBYTE results, and the cassette's one output channel
when `*SPOOL` holds it and OPENOUT asks for another, checked by comparison; and
speed
where it still falls short of MOS 1.20 — the line routine's minor step, which
costs a diagonal about a tenth more; the keyboard scan while a key is held,
which takes 11% of the machine where MOS 1.20 takes 7.5%, as sensing columns
through CA2 would cure; the text cursor's address, worked out afresh at each
character; a window's scroll and clear, moved a byte at a time; fills and VDU
5, which address each pixel afresh; and the glyph loop, which tests the mode
for every row. These need room the full image does not have, so each lands
with the space a rework frees.

**BASIC.** The whole BBC BASIC II language: every keyword, the tokenised
program format, the line editor and its commands, the five-byte floating
point and every function to the original's accuracy, `@%` formatting, the
inline assembler, `CALL`/`USR` conventions, errors with their numbers and
messages, and file access through the MOS. Workspace follows BASIC II: zero
page &00–&6F, the resident integers at &0400, the variable catalogue from
&0480, the control stacks of page &05, the string buffer at &0600 and the
input buffer at &0700.

**DFS.** The Acorn DFS 1.20 interface over the 8271: the catalogue format,
every `*` command, OSFILE, OSFIND, OSGBPB, OSARGS, OSBGET and OSBPUT on up to
five open files, OSWORD &7D–&7F, `!BOOT` and `*OPT 4`, and auto-boot on
SHIFT-BREAK. It claims the pages the original claims, so PAGE is &1900.

## 4. The worker and its protocol

The worker is the `Run` binary in its `--sandbox-session-worker` role over a
`SupervisedSession`, with one share holding the frame slot. The owner sends
requests; the worker answers each with one reply; nothing is unsolicited.

- **Requests:** configure (ROM images per slot, sideways RAM, keyboard
  links); insert or eject a disc or tape; key and joystick changes stamped in
  cycles from the slice start; run *n* cycles; reset or BREAK; take or
  restore a snapshot; collect the discs written since last collected.
- **Replies:** cycles run; whether a new frame is in the slot and which rows
  changed since the last frame reported; the slice's 48 kHz samples;
  keyboard and drive LEDs, motor and head activity; refusals with a reason.

Every message is length-bounded and validated on decode; a malformed reply
condemns the worker. A crashed worker is replaced by the supervisor and the
machine restarts from power-on, and the user is told why.

## 5. The front end

- **Window.** Resizable, opening at the canvas's 4:3 picture; the picture is
  letterboxed at its aspect. Scaling is *sharp* (integer prescale, then a
  bilinear remainder) or *smooth*; optional scan lines. Only rows the worker
  reported changed are expanded, scaled and presented.
- **Keyboard.** *Logical* mapping types the character the host key produces,
  pressing or lifting the BBC SHIFT as the BBC layout needs; *physical*
  mapping places the BBC keyboard on the host keys by position, for games.
  Held keys are released when focus is lost. Paste types text through the
  matrix at a rate the keyboard scan accepts.
- **Pacing.** The audio ring is kept between one and two device periods
  ahead; each `SpaceAvailable` asks for the cycles that refill it. Without
  audio, a deadline per 20 ms of machine time.
- **Media.** Discs and tapes arrive by the picker, at launch, by a drop on
  the window or its icon-bar icon, or through Open With. Reads and write-backs
  run on a worker thread, never the event loop.
- **Menus.** The window's menu (right click, or the icon-bar menu): Insert
  Disc into drive 0 or 1, Eject, Insert Tape, Rewind, BREAK, Reset, Pause,
  Speed, Fullscreen, Scaling, Keyboard mapping, Save and Restore Snapshot,
  Settings.
- **Settings.** In `lib/appdata`: ROM slots, sideways RAM, keyboard links,
  scaling, scan lines, keyboard mapping, volume, accelerated tape loading.
  User ROM images are app-data blobs. Writes leave the loop on a worker.
- **Status.** A strip beneath the picture shows the three keyboard LEDs and
  each drive's and the cassette motor's activity.

## 6. OS facilities this plan adds

- **Lock and ISO keys.** The input path delivers Caps Lock's press and
  release with its physical usage, and the two ISO keys (`#~`, `\|`) as
  physical keys, alongside the lock state it already keeps.
- **Menu symbols.** `Reset`, `Eject` and `Fullscreen` join the symbol
  vocabulary with their glyphs.
- **Content types.** Acorn DFS disc images (`.ssd`, `.dsd`) and UEF and CSW
  tape images enter the registry, so Files opens them with `beeb.app`.
- **gzip members.** A bounded gzip member decoder joins `lib/compress`; the
  archive driver and the tape decoder share it.

## 7. Verification

- **CPU.** A deterministic sample of the SingleStepTests 65x02 vectors
  (MIT-licensed, notice kept beside them) checks every opcode's registers,
  memory and bus cycles; unit tests pin decimal mode, interrupt timing and
  JAM.
- **Devices.** Each device has unit tests against its data sheet's
  behaviour; the machine has scenario tests on synthetic ROMs written with the
  builder.
- **ROMs.** Each ROM is tested on the emulated machine: typed input, captured
  VDU output and documented workspace, against expectations from the manuals
  and from the recorded corpus.
- **Reference harness.** `cargo xtask beeb --reference <dir>` runs the
  corpus scenarios against genuine ROMs a developer holds, outside the tree,
  and records their observations as fixtures; ci replays the fixtures against
  the clean-room ROMs. The harness refuses to write a fixture containing ROM
  bytes.
- **Fuzzing.** Every decoder of untrusted bytes — disc, tape, snapshot, the
  protocol in both directions — has a registered harness.
- **QEMU.** A desktop vertical launches `beeb.app`, boots the clean-room ROMs
  to the BASIC prompt, runs a program and screendumps the window.
- **Acceptance.** A recorded corpus of real software, named with its media
  hash, outcome and the ROM set it ran on.
