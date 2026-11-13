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
| BB8 | The 6502 builder, `cargo xtask beeb --write-roms`, the committed images and their drift gate | done |
| BB9 | The clean-room MOS | done |
| BB10a | BASIC: workspace, entry and BREAK, the prompt, tokeniser and LIST, line editing, NEW/OLD/RUN/END/STOP, errors with ON ERROR, REPORT, ERR and ERL | done |
| BB10b | BASIC: values and variables — integers, reals and strings, the catalogue and heap, indirection, arrays and DIM, the operators and their coercions, assignment, the string functions | done |
| BB10c | BASIC: control — IF/ELSE, FOR/NEXT, REPEAT/UNTIL, GOTO, GOSUB, ON, PROC/FN/LOCAL with array parameters, READ/DATA/RESTORE | done |
| BB10d | BASIC: numbers — the five-byte arithmetic, conversions, PRINT and `@%`, VAL and STR$, the transcendental functions to BASIC II's accuracy, RND | done |
| BB10e | BASIC: I/O and the machine — PRINT and INPUT forms, files, the MOS statements, CALL and USR, AUTO, DELETE, RENUMBER, LISTO, TRACE, WIDTH, CHAIN, LOAD and SAVE | done |
| BB10f | BASIC: the inline assembler | done |
| BB11 | The clean-room DFS | done |
| BB12 | The reference harness and the recorded observation corpus | done |
| BB13 | The machine worker and its protocol | done |
| BB14 | The front end: window, scaling, input mapping, audio pacing and accelerated tape loading, media, menus, settings, status | done |
| BB15 | The OS facilities the front end needs: lock and ISO keys, menu symbols, disc and tape content types, gzip members in `lib/compress` | done |
| BB16 | The bundle: manifest, icon, help in every required locale, docs page, inventory pins | done |
| BB17 | Fuzz harnesses over every untrusted decoder, and the QEMU vertical | done |
| BB18 | Software acceptance over a recorded corpus | blocked: needs a corpus of real software images a developer holds outside the tree |

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
   images into the crate's `firmware/`, they are committed and compiled into
   `Run`, and a static gate and a crate test rebuild them and compare bytes.
   No 6502 assembly text, Python or other tooling enters the tree.
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
- `worker` — the protocol, encoded and validated on both sides; the
  worker's `MachineService`; the owner's `MachineClient`.
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
code run on into a label only where the label is checked to lie next, data
directives, and a symbol map. Encoding comes from `cpu::isa`; only
documented opcodes are encodable. Hardware addresses come from `machine`, so
a register address is defined once for both the emulation and the firmware.

**Shipped images.** `firmware` carries each image `Run` ships, committed in
the crate's `firmware/` and compiled in with `include_bytes!`: an app reads
its own bundle only with `CAP_FS_ACCESS` (`plans/APPS.md` §16), which the
window does not hold. `rom::SHIPPED` pairs each name with its builder. `cargo
xtask beeb --write-roms` rebuilds them; the `beeb-roms` static gate and the
crate's `firmware` test refuse a committed image the tree does not build, the
test also that the carried bytes are the committed ones, and the gate refuses
an image file the crate does not ship.

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
  with the bell and a new line, and its addresses are &FFFFFFFF. OPENOUT
  while a file is open to write abandons that file unwritten and reopens
  the one channel, so a `*SPOOL` holding it carries on into the new file.
- A silent sound channel leaves REMV alone until a note is queued on it.
  Each channel counts twentieths of its own: a note that breaks silence, or
  ends a release past its duration, has a first twentieth a centisecond
  short, and a note queued behind another starts as it ends. An envelope
  steps from its note's start, the start's pitch written first; a step moves
  on one pitch section at most, and repeating sections start again from the
  note's pitch. Levels run 0–127; attack and decay end once the level is past
  their targets from the side it moves, a level that does not move counting
  as falling, the level then one above the target. A note's level carries on
  from the last while the channel sounds and is nothing once it is silent; an
  amplitude's is the top of its volume's eight. A held note's release steps
  on the tick it takes over.
- While copy editing, output lands at the input position, the cursor's cell
  followed through each exchange of the two cursors.
- *FX118 returns X 1, Y &7F and C clear; OSBYTE &79 and &7A leave Y &7F
  after a key's test and &EE after a scan; a negative INKEY's C says whether
  the key is down; OSBYTE &80 reading the fire buttons leaves C set.
- CAPS LOCK makes letters capitals with SHIFT or without. CAPS LOCK turns
  itself over and SHIFT LOCK off; with SHIFT it turns itself on with SHIFT
  reversing the locks (status bit 7), so SHIFT types lower case; SHIFT LOCK
  turns itself over and CAPS LOCK off, shifting every key that has a shifted
  form, function keys included. CTRL takes the code shifting gives. The lock
  keys act as they go down and never repeat, and each scan of the keyboard
  records SHIFT and CTRL in status bits 3 and 6. An input zone set to zero
  ignores its codes.
- OSCLI calls FSCV with A=8 before each command; *HELP offers the ROMs the
  call before showing its version; *EXEC of a file the filing system cannot
  open is File not found.

Deliberate divergences: the character set is this ROM's own design, so a
glyph's pixels differ from MOS 1.20's; and &31A, the VDU driver's scratch for
the last addressed pixel's line, is not reproduced for a line clipped at its
start or the extra plot of an open line, which MOS 1.20 takes from other
points of its own drawing order. The 16 KiB image has no room for the rest:
MOS 1.20 writes a silenced channel's attenuation twice more and steps a
finished release's pitch twice while silent, which nothing hears; the
two-key rollover takes a second key about 1.5 cs sooner; the workspace MOS
1.20 keeps for rollover, auto-repeat and sound (&2CA–&2CE, the sound
buffers' flags at &2D3–&2D6) and the &FF it leaves at &2EA–&2FF are this
ROM's own; and, against MOS 1.20's speed, text output is within a tenth and
a window's clear is faster, but a whole-screen clear takes twice as long,
VDU 5 half as long again, lines a tenth longer, and the scan while a key is
held 11% of the machine to its 7.5%.

**BASIC.** The whole BBC BASIC II language: every keyword, the tokenised
program format, the line editor and its commands, the five-byte floating
point and every function to the original's accuracy, `@%` formatting, the
inline assembler, `CALL`/`USR` conventions, errors with their numbers and
messages, and file access through the MOS. Workspace follows BASIC II: zero
page &00–&6F, the resident integers at &0400, the variable catalogue from
&0480, the control stacks of page &05, the string buffer at &0600 and the
input buffer at &0700.

BASIC is written afresh in `rom::basic`, laid out as BASIC II is. Workspace
facts, taken from the published guides and confirmed by observing BASIC II
run:

- Zero page: LOMEM &00, the top of the variables &02, the BASIC stack &04,
  HIMEM &06, ERL &08, the text pointer's offset &0A and base &0B, the RND
  seed &0D–&11, TOP &12, the print field and hex flags &14–&15, the ON ERROR
  pointer &16, PAGE's high byte &18, the second text pointer &19, the DATA
  pointer &1C, COUNT &1E, LISTO &1F, TRACE &20 with its
  limit &21, WIDTH less one &23, the REPEAT, GOSUB and FOR depths &24–&26,
  the last expression's type &27 (&00 string, &40 integer, &FF real), OPT
  &28, the integer accumulator &2A–&2D and the real one from &2E; &37–&6F are
  the interpreter's own scratch, &70–&8F the user's.
- Page 4: `@%` and A%–Z% at &400 + 4n; the catalogue at &480 + 2 × (the
  initial's code − &40), FN's list at &4F6 and PROC's at &4F8. An entry is
  a link, the name's later characters, a zero, then the value: a real's five
  bytes, an integer's four, a string's descriptor (address, the room it
  holds, its length) with its text following a new one, or an array's
  dimension byte 2n + 1, n element counts and its elements. A string given
  room, made, moved or grown in place, gets its length below 8 characters
  and 8 more from 8, at most 255.
- Reals are five bytes: an exponent offset by &80, then the mantissa most
  significant first, its top bit the sign; zero is all zeros.

The modules: `ws` (the workspace map), `tokens` (the keyword table),
`crunch` (tokenising), `list` (detokenising, LISTO and line numbers),
`editor` (the prompt, lines, AUTO, DELETE, RENUMBER, LIST, SAVE, LOAD and
CHAIN), `exec` (the statement loop and assignment), `control` (IF, the loops,
GOTO, GOSUB, ON and TRACE), `procs` (PROC, FN and LOCAL), `expr` (the
operators), `func` (the functions), `vars` (variables, arrays, the heap and
the BASIC stack), `io` (PRINT, INPUT and READ), `files` (the channels),
`system` (the MOS statements, CALL and the pseudo-variables), `float` (the
five-byte arithmetic), `decimal` (reading and writing decimal), `trans` (the
transcendental functions and RND), `assembler` and `errors`. Each part is
tested on the emulated machine with the clean-room MOS, typing programs and
comparing the screen and workspace with what BASIC II produced, recorded
beside the tests in `rom::basic::tests`.

Behaviour pinned by observation, each with its test:

- Reals: every operation rounds, a tie to the odd neighbour; division keeps
  38 quotient bits; the multiplicand is the operand loaded second; an
  accumulator exponent of 0 is a value and only one below 0 underflows; a
  stored real is zero only when all five bytes are. Each function's
  algorithm and constants give BASIC II's bits, recorded in `trans`.
- A name is copied whole when tokenised, so no keyword is found inside it,
  and a conditional keyword followed by a name's character is a name.
- RENUMBER refuses a number past 32767 before it changes anything and
  reports each missing target as Failed at its line's new number; DELETE
  deletes the first line it finds from its first number.
- LOCAL clears a number to 0 and a string, at an address or not, by a 0 in
  its descriptor's length byte, and restores both; a byte is neither kept
  nor restored.
- READ starts again from the program's first DATA at each line typed at the
  prompt and at each clearing of the variables, CLEAR's included.
- The assembler lists the address, three bytes a line and the statement
  from column 14; `[` starts at OPT 3; with OPT's errors off a name no
  variable has is P%, inside an FN the operand calls too, until that FN
  assembles; an FN called from OPT assembles its own code, the macro form.
- Entering BASIC with the RND seed all zero, as at power-on, sets it to
  `41 52 57 00 00`; any other seed survives BREAK. The execution address
  SAVE gives a program, &8023, is the cold start the language entry makes.

Where it differs on purpose: its copyright message is its own, as long as
BASIC II's; an instruction whose operand calls an FN that assembles code is
placed after that code, where BASIC II's bytes go astray; a byte formal is
neither kept nor restored, where BASIC II's damages the program; and, in 16
KiB, against BASIC II's speed integer loops take from 8% to 29% longer and
the functions up to a tenth longer, while TAN and small whole powers are
faster.

**DFS.** The Acorn DFS 1.20 interface over the 8271: the catalogue format,
every `*` command, OSFILE, OSFIND, OSGBPB, OSARGS, OSBGET and OSBPUT on up to
five open files, OSWORD &7D–&7F, `!BOOT` and `*OPT 4`, and auto-boot on
SHIFT-BREAK. It claims the pages the original claims, so PAGE is &1900.

The DFS is written afresh in `rom::dfs`: the catalogue of the drive last read
at &0E00–&0FFF, the operation in hand and the settings software reads in page
&10 (*OPT 1 at &10C6, *ENABLE at &10C7, the directories and drives from
&10C9), five channel blocks and their sector buffers from &1100, and the NMI
routine at &0D00. The modules: `ws`, `disc` (the 8271's transfers and OSWORD
&7D–&7F), `nmi`, `catalogue` (names, the catalogue, *CAT, *INFO and
allocation), `files` (OSFILE), `channels` (OSFIND, the byte calls, OSARGS and
OSGBPB), `commands` (the command table and the settings), `copy` (*BACKUP,
*COPY, *COMPACT and *DESTROY), `utils` (*BUILD, *DUMP, *LIST and *TYPE),
`service`, `text` and `errors`. Its scenarios in `rom::dfs::tests` type at a
machine running the three clean-room ROMs and compare the screen, and the
SHA-256 of each disc written, with what DFS 1.20 produced for the same steps.

Behaviour pinned by observation, each with its scenario:

- The catalogue is read once and kept until the drive reports its disc
  changed; a save writes the catalogue before the data.
- A file opened for output is given 64 sectors. One that outgrows its room
  grows to the next multiple of 256 sectors, or to the next file's start or
  the disc's end if nearer, the catalogue written at each step; with no room
  left it is Can't extend, its buffer left unwritten.
- *COMPACT writes the catalogue only if a file moved.
- A question takes only the keys pressed after it.
- OSFILE reads a locked file's attributes as &08; written attributes with
  &02 or &08 set lock it.
- *RUN of a file to be entered in &8000–&BFFF, as a program BASIC saves is,
  loads it and returns, where DFS 1.20 enters its own code paged there.
- Loads take as long as DFS 1.20's to within two centiseconds.

Where it differs on purpose: its banner and copyright are its own; V, the C
flags the manuals leave undefined and the registers a program is *RUN with
are not DFS 1.20's; the rest of a file's last sector is zero, where DFS 1.20
writes whatever its buffer last held, of that file or another; a buffer is
written back only to its file's disc, where DFS 1.20 writes it to whatever
disc is in the drive; a channel follows its file when *COMPACT moves it; and
a save takes up to a revolution less than DFS 1.20's.

## 4. The worker and its protocol

The worker is the `Run` binary in its `--sandbox-session-worker` role over a
`SupervisedSession`, serving `worker::MachineService`, with one share holding
the canvas, a byte a pixel, row after row. The owner, through
`worker::MachineClient`, asks one request at a time; the worker answers each
with one reply; nothing is unsolicited.

- **Requests:** configure (the OS image, each slot's ROM image or sideways
  RAM, and the keyboard links), which builds the machine at power-on, the
  media kept; insert or eject a disc or tape, rewind the tape, press or
  release RECORD; run a slice of up to a second, its key, BREAK, analogue and
  fire-button changes stamped in cycles from the slice's start; power off and
  on, the media kept; take or restore a snapshot; collect the discs of the
  drives marked that were written since last collected, which stay in.
- **Replies:** the cycles run; a map of the canvas rows the slice rewrote in
  the share, every row after a build or a restore; the slice's sound when
  asked, signed 16-bit at 48 kHz, AC-coupled with a 10 Hz corner as the
  machine's output is; the CAPS LOCK and SHIFT LOCK lights, the cassette motor
  and each drive's head; what the drives and deck hold and whether RECORD is
  down; the media a request took out; refusals with a reason.
- **Media.** The worker holds the only copy of what a guest writes until the
  owner has it, so every request that takes media out — an eject, an insert
  over what is in, a restore over the drives and deck — answers with what it
  took: each disc written since last collected, in its own format with the
  first side and track its image cannot record, and the tape, as a UEF image,
  if it was recorded on since it went in. A tape knows it was recorded on,
  across power-on and snapshots, as a disc knows its written tracks. A
  recorded tape too large to carry is refused out and stays in. The bound on
  a message is the largest one exactly: both drives and the deck taken out
  whole.
- **Time.** Slices tile one timeline: a slice ends on the first instruction
  boundary at or past its end, and the next ends where it was asked to, so
  the overrun never accumulates.

Every message is decoded whole against fixed bounds — a ROM of 16 KiB, a disc
of both sides of 84 tracks, a tape or snapshot of 4 MiB, 256 input changes a
slice — and is the one encoding of what it decodes to. The client believes a
reply only when it answers what was asked within what that could produce: a
slice of the cycles asked, with no sound unless asked and no more samples
than those cycles make. A reply that does not condemns the worker. A crashed
worker is replaced by the supervisor and the machine restarts from power-on,
and the user is told why.

## 5. The front end

The host-tested model is `front`; `src/run.rs` joins it to the window, the
session, the audio stream and the store.

- **Window.** Resizable, opening at an `OPEN_PICTURE` 4:3 picture above the
  status strip; the picture is the largest 4:3 the window holds, centred.
  Scaling is *sharp* or *smooth*, optionally with scan lines, through
  `tairix_crt::screen::Picture` (shared with `PCEmulator.app`). Only the rows
  a slice reported changed are expanded through the slot palette, scaled and
  presented, and one paint presents only what it drew.
- **Keyboard.** Read raw. *Logical*: a symbol presses the BBC key and SHIFT
  `Key::typing` gives it, a letter its own key, its case the machine's CAPS
  LOCK and the host's Shift; *physical*: each key the BBC key in its place, the
  keys the PC's main block lacks on its editing keys. A key is held at least
  two keyboard scans; every key is let go when the window loses the focus; two
  host keys on one BBC key let it go with the last. Paste types the
  clipboard's text a key at a time, SHIFT from its case and the machine's
  CAPS LOCK.
- **Pacing.** While the sound plays the audio ring is kept at the latency the
  stream was granted: whenever it falls below, the slice that refills it is
  asked for, so the machine and the device cannot drift. Without a stream, or
  at double speed, a slice is due each 20 ms of the clock, a long stall given
  up rather than raced; at the fastest speed, or while fast tape loading has
  the motor running, slices of 100 ms run back to back, silent.
- **Media.** Discs and tapes arrive by the picker, a drop on the window or its
  icon-bar icon, or Open With; reads and write-backs run on the file worker,
  never the loop. `front::media::Shelf` keeps each one's record and decides
  what becomes of what the machine hands over. A disc whose file may be
  written is written back as the machine writes (collected when its drive's
  head unloads, and only such discs are collected); one whose may not is
  write-protected. A disc or tape on its way in takes its drive only once
  what it replaces is handled. A tape recorded on is saved as it is taken out:
  back to a writable UEF, else offered to be saved as one. A snapshot's discs
  and tape are its own, held only by the machine: a written one is offered to
  be saved as it is taken out, a disc in its own format. Quitting takes every
  medium out first. One picker is open at a time: offers to save wait their
  turn, and an Open while one is up is refused with a message.
- **Menus.** One set of rows, on the window's right click and the icon bar
  (`front::menu::bar`, Info first and Quit last): Drive 0 and Drive 1 (Insert
  Disc, Eject, Save Disc As), Tape (Insert, Rewind, Record, Eject), BREAK,
  Reset, Pause, Speed, Full Screen, Display (Sharp, Smooth, Scan Lines),
  Keyboard (By Character, By Position), Paste, Save and Restore Snapshot,
  Settings. Eject follows what the machine reports in its drives; Save Disc As
  needs the disc's image held.
- **Settings.** In `lib/appdata`, the `front::settings::Preferences` registry,
  edited in the shared settings window (`front::sheet`: ROMs, Startup,
  Display, Keyboard and sound) and written off the loop by the publish worker.
  A socket holds a shipped ROM, sideways RAM, or a user's ROM image installed
  as an app-data blob `rom.<name>`. Changes to the ROMs or the links take
  effect at the next Reset, which builds the machine again only when they
  changed.
- **Status.** A strip beneath the picture shows the CAPS LOCK, SHIFT LOCK and
  cassette-motor lamps, each drive's, and the latest message.
- **Diagnostics.** `front::events` in the `BEEB` range of `lib/log`: the
  machine's first second run after a build, and a worker replaced.

## 6. OS facilities this plan adds

- **Lock and ISO keys.** A window reading the keyboard raw
  (`SetKeyboardMode`, `plans/DISPLAY.md` D13) is handed every edge with its
  physical key, Caps Lock's press and release among them, alongside the lock
  state the seat keeps. The ISO keys are their HID usages: `\|` beside the
  left Shift is `0x64`, and `#~` beside Return is `0x32` from a USB or I2C
  keyboard and `0x31` through the evdev table, which gives the two positions
  one code; the physical mapping takes both for that key.
- **Menu symbols.** The vocabulary already holds a picture for each of the
  machine's rows, as `PCEmulator.app` draws them: `Restart` for Reset,
  `Drive` for Eject and `ZoomFit` for Full Screen; nothing joins it.
- **Content types.** Acorn DFS disc images (`.ssd`, `.dsd`,
  `application/x-acorn-dfs-disc`) and UEF and CSW tape images
  (`application/x-uef-tape`, `application/x-csw-tape`) are in the `lib/browse`
  registry, archives that hold no volume, so Files opens them with
  `beeb.app`.
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
- **Reference harness.** The corpus is `tests/corpus/`: each group a
  `.scenarios` file, scenarios of discs and scripted keys in the format
  `tairix_beeb::scenario` reads, and a `.fixtures` file of what each left —
  its screen's rows and the SHA-256 of each disc it checks. The build lists
  every scenario as a test of `tests/corpus.rs`, which replays it against
  the clean-room ROMs. `cargo xtask beeb --reference <dir>` runs the corpus
  against the original ROMs a developer holds in `<dir>`, outside the tree,
  and rewrites the fixtures from them, the clean-room ROMs' own names put in;
  a scenario marked `ours` keeps the fixture that states this machine's own
  behaviour. It refuses to write anything when an observation's text, or the
  bytes its rows write in hexadecimal, carries twelve bytes in a row of an
  original ROM that no clean-room ROM holds and the clean-room ROMs do not
  show for the same scenario.
- **Fuzzing.** Every decoder of untrusted bytes has a registered harness:
  `fuzz_media` the disc and tape images, `fuzz_snapshot` snapshots and
  hostile guest code, `fuzz_worker` the protocol in both directions, the
  worker's service and the owner's client.
- **QEMU.** `tests/integration/beeb_qemu_aarch64`: the desktop launches
  `beeb.app` from the program library; once the machine records its first
  second run, the window is dumped and must show the BASIC prompt's white
  text on black; Quit from its icon-bar menu ends it cleanly. Typing at BASIC
  is the recorded corpus's, through the same machine, and the key mapping the
  front end's host tests'.
- **Acceptance.** A recorded corpus of real software, named with its media
  hash, outcome and the ROM set it ran on.
