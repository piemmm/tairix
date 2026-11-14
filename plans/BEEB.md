# BEEB — the Open Micro Emulator (`OpenMicro.app`) and its clean-room ROMs

Binding under `AGENTS.md`. This plan owns `OpenMicro.app`, the Open Micro
Emulator: emulated 8-bit microcomputers — the BBC Micro Model B and the Acorn
Electron with its Plus 1 — run as an ordinary sandboxed desktop application
over a library of machines like PCEmulator's; the clean-room ROMs they run: an
operating system equivalent to MOS 1.20 on the Model B and to Electron OS 1.00
on the Electron, built from one source, an interpreter equivalent to BBC BASIC
II, a disc filing system equivalent to Acorn DFS 1.20, and an expansion ROM
equivalent to the Plus 1's, each written for TAIRiX from published interfaces
and black-box observation; and the OS facilities it adds: the machine library
it shares with PCEmulator, the form dialog both emulators' dialogs are, and an
application's own documents folder.

Read first: `plans/APPWIN.md` (the window channel), `plans/SOUND.md` (the
`audio-v1` stream), `plans/CAPABILITY_USE.md` CU6 (the picker's one-shot
grants), `plans/OPENFILE.md` OF12 (the subtree grant), `plans/APPDATA.md`
(the store), `plans/PCEMU.md` §3 (the library this one shares),
`plans/NEW-MENUS.md`, `plans/ICONS.md`, `plans/SHADER.md` §0 item 5
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
| BB8 | The 6502 builder, `cargo xtask openmicro --write-roms`, the committed images and their drift gate | done |
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
| BB19 | The name: `OpenMicro.app` (`os.tairix.openmicro`), the Open Micro Emulator, its crate, xtask subcommand, gate, log range and vertical; the Model B's banner `OpenMOS Micro` | done |
| BB20 | `lib/controls`' `FormDialog`, and PCEmulator's assistant and settings dialogs on it | done |
| BB21 | `lib/machines`: the machine library — record, library window, standings, suspends, state blob, assistant, card menu — and `lib/crt`'s thumbnails and screenshots; PCEmulator moved onto both | done |
| BB22 | The machine crate for several models: the model, the shared sockets, cassette and snapshot container, the Model B under `machine::bbc` | done |
| BB23 | The Electron: its ULA (contention, video and palette, interrupts, sound, cassette, paging), keyboard and Plus 1 (ADC, printer, fire buttons, cartridges), and its snapshots | done |
| BB24 | The clean-room Electron OS, built from the MOS source | done |
| BB25 | The clean-room Plus 1 expansion ROM | done |
| BB26 | The worker and its protocol for several models | done |
| BB27 | An application's own documents folder: the `documents-folder` manifest key, the session's grant and its pin | done |
| BB28 | The library front end: the library window, the assistant, each machine's settings, several machines each in its window, suspend, resume and shut down, the icon-bar lifetime | done |
| BB29 | Machine folders: discs and tapes made for a machine, kept with it in the user's files | done |
| BB30 | The bundle again: the icon, help in every locale, the docs page; the fuzz harnesses; the QEMU vertical through the library | done |

Items are built in ledger order; each lands complete with its tests.

## 0. Binding decisions

1. **Two machines, more to come.** The 32K BBC Model B: an NMOS 6502 at
   2 MHz, 32 KiB of RAM, sixteen sideways slots, two VIAs, the
   6845/ULA/SAA5050 display, the SN76489, an 8271 with two double-sided
   drives, cassette, the uPD7002 and the printer port; B+, Master, Tube,
   Econet and speech are not emulated, and a probe for one finds it absent as
   on an unexpanded machine. The Acorn Electron: the 6502 at 2 MHz, 32 KiB
   behind the ULA, the ULA's display, sound, cassette, paging and
   interrupts, its keyboard, and optionally the Plus 1 with its printer port,
   analogue port and two cartridge slots; the Plus 3 and other expansions
   are not emulated. A further micro is a further model of the same crate.
2. **The guest is hostile.** Guest code, ROM images, disc and tape images and
   snapshots are untrusted input. All of it is decoded and executed only in
   a machine worker, a `lib/sandbox` session worker holding no capability.
   The front end validates every worker reply as it would any untrusted
   message, and survives a worker that crashes, stalls or lies.
3. **The ROMs are authored in Rust.** Each ROM is Rust code driving the
   crate's 6502 builder; `cargo xtask openmicro --write-roms` emits the
   16 KiB images into the crate's `firmware/`, they are committed and
   compiled into `Run`, and a static gate and a crate test rebuild them and
   compare bytes. No 6502 assembly text, Python or other tooling enters the
   tree. One MOS source builds the Model B's OS and the Electron's.
4. **Compatibility is with the documented machine.** Each ROM implements its
   original's documented interface, keeps its documented workspace at the
   documented addresses, and places internal state where software is known
   to read it. Private routine addresses inside a ROM are not reproduced;
   documented entry points, vectors, tables and workspace are.
5. **The clean-room boundary.** Implementation inputs are the published
   manuals (the BBC Micro User Guide and Advanced User Guide; the Electron
   User Guide and the Electron Advanced User Guide, Adder 1984), data sheets,
   and observations of genuine ROMs run as opaque programs: what they print,
   what they write to documented workspace, how they drive hardware. No
   original ROM's code, disassembly or annotated listing is read — the
   Electron's OS and the Plus 1's ROM included. Genuine ROM bytes never enter
   the tree, a fixture or a build.
6. **Genuine ROMs are the user's choice.** A user may install their own ROM
   images in any socket or as the OS; the built-in clean-room ROMs are the
   default, so every machine works out of the box.
7. **Timing is cycle-exact.** Every CPU bus cycle, dummy accesses included,
   happens at its true cycle, the Electron's memory contention included;
   devices are advanced to the cycle of each access that can observe them;
   video is produced at the beam position of the cycle that fetched it.
8. **The audio clock paces the machine.** With an audio stream the machine
   runs exactly as fast as the device consumes its samples, so the two cannot
   drift; without one, the monotonic clock paces it. Video presents whichever
   frame is newest.
9. **Determinism.** A machine is a pure function of its model, ROMs, media
   and input events with their cycle stamps. Its power-on state is fixed, so
   snapshots and recordings replay exactly.
10. **Names.** The application is the Open Micro Emulator. A machine model is
    named descriptively — *BBC Micro Model B*, *Acorn Electron* — and
    nothing else names a manufacturer: the clean-room OS calls itself
    OpenMOS (`OpenMOS Micro 32K` on the Model B, `OpenMOS Electron` on the
    Electron), the DFS `TAIRiX DFS`, and no ROM claims to be another's.
11. **One library.** Machines are kept, shown, made, configured, copied,
    deleted, suspended and resumed through `lib/machines`, the library
    PCEmulator keeps its machines in; neither application carries a copy.
12. **What a machine makes is the user's.** A disc or tape made for a
    machine is a file in the user's `Home/Documents/Open Micro/<machine>/`,
    which the file manager shows, backup covers and the user may delete; it
    is reached through the application's documents folder (§6), never the
    application's private store.

## 1. Crate shape

One crate, `userland/apps/openmicro` (`tairix-openmicro`, bundle
`OpenMicro.app`, title *Open Micro Emulator*). An application crate may
depend only on `lib/*`, so the machines, the ROM sources and the front-end
model are modules of its `[lib]`, and the one `Run` binary is both the front
end and, re-spawned through `@self` in its sandbox role, a machine worker.

- `cpu` — the 6502: `isa` (the single opcode table: mnemonic and mode per
  opcode, from which the CPU dispatch, the disassembler and the builder's
  encoder are all derived), the core, `disasm`.
- `machine` — what every model shares: the clock, the `Model`, ROM sockets
  (`Rom`, sideways `Slot`s and their fingerprints), the bus monitor, and
  `cassette` (the tape under the motor relay, the data separator and the
  carrier detector, serving the Model B's 6850 or the Electron's ULA); then
  one module a model: `bbc` (the bus, `via`, `keyboard`, `sound`, `adc`,
  `acia`, `serial`, `crtc`, `ula`, `teletext`, `video`, `fdc`) and
  `electron` (the bus, `ula`, `video`, `keyboard`, `plus1`).
- `snapshot` — the snapshot container and the codec every device's state
  goes through.
- `media` — the disc and tape image decoders.
- `rom` — host-only (`cfg(not(freestanding))`): the builder and the ROM
  sources, `mos` (for either model), `basic`, `dfs`, `plus1`. The
  freestanding binary never compiles it; xtask and the crate's tests drive
  it.
- `worker` — the protocol, encoded and validated on both sides; the
  worker's `MachineService`; the owner's `MachineClient`.
- `front` — the host-tested front end: the library record of a machine, key
  mapping per model, the presentation, pacing, menus, each machine's
  settings, media and machine folders.

## 2. The machines

### 2.1 The Model B

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
anti-aliased teletext costs nothing extra. Both models share the canvas and
its encoding.

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

### 2.2 The Electron

Facts below are the Electron Advanced User Guide's (chapters 13–15), its
errata, and the Electron User Guide's.

**Clock and frame.** 2 MHz cycles; a frame is 312 lines of 128 cycles,
39 936 cycles, not interlaced. A line's cycles count from its HSYNC; its
display window, the 640 pixels at 16 MHz, is cycles 24 to 104. Line 0 is the
first with pixels.

**Memory map.** RAM `&0000–&7FFF`; the paged ROM window `&8000–&BFFF`; the OS
`&C000–&FBFF` and `&FF00–&FFFF`; FRED `&FC00`, JIM `&FD00`; SHEILA `&FE00`,
where the ULA answers in every 16 bytes. A read of a write-only register
returns the OS byte beneath. ROMs 8 and 9 are the keyboard, 10 and 11 BASIC;
with the Plus 1, 12 is its ROM, 2 and 3 the first cartridge slot and 0 and 1
the second. Paging writes `&FE05`'s low nibble: while ROM 8–11 is paged only
8–15 is taken, otherwise any; the Plus 1 latches 0–7 and 12–15. A window
nothing answers reads the floating bus.

**Contention.** ROM, keyboard and ULA accesses run at 2 MHz. RAM in modes
4–6, and FRED and JIM always, run at 1 MHz, stretched to the next coincident
edge exactly as the Model B's 1 MHz bus is: two or three cycles. In modes 0–3
a RAM access in a display line's window waits until the window ends, and
outside it runs at 2 MHz; a mode written applies once the current line ends.

**ULA registers.**
- `&FE00` read: bit 0 master IRQ (an enabled condition present), 1 power-on
  (cleared by the first read), 2 display end, 3 RTC, 4 receive full, 5
  transmit empty, 6 high tone, 7 always set; bits 2–6 show a condition
  whether enabled or not. Write: the enables of bits 2–6. The Guide's figure
  transposes bits 4 and 5; its errata and the hardware place them so.
- `&FE02`/`&FE03` write: the screen start, A6–A8 from `&FE02` bits 5–7 and
  A9–A14 from `&FE03` bits 0–5, 64-byte steps, sampled at the frame's start;
  zero is the mode's base.
- `&FE04`: the cassette data register, read for the byte received (clearing
  receive full), written for the next to send (clearing transmit empty).
- `&FE05` write: the ROM number (bits 0–3) under the paging rule; bits 4, 5,
  6 clear display end, RTC and high tone; bit 7 the NMI release.
- `&FE06` write: the counter, a sound of 1 MHz / (16 (S + 1)) in sound mode.
- `&FE07` write: bits 1–2 the counter's use (cassette input, sound,
  cassette output, none), bits 3–5 the mode (7 is mode 4), bit 6 the motor
  relay, bit 7 the CAPS LOCK light.
- `&FE08–&FE0F` write: the palette, sixteen entries of red, green and blue,
  a bit each, set to turn its gun off, laid out as the Guide's 16-colour
  table. A pixel's entry is its colour's bits in place: a two-colour pixel is
  entry 0 or 8, a four-colour one 0, 2, 8 or 10, a sixteen-colour one its own
  number.

**Interrupts.** Display end is raised at the end of line 255 in every mode,
RTC at the end of line 100; receive full as a byte completes, transmit empty
as the last byte has gone and at once on entering cassette output, high tone
as the carrier detector hears the carrier. Each condition holds until its
clear; IRQ is any enabled condition. Nothing raises NMI.

**Video.** Modes 0–6 at the Model B's addresses and character-cell layout;
modes 3 and 6 show 25 rows of 8 lines and 2 blank. A frame's addresses wrap
past `&7FFF` to the mode's base (`&3000`, `&4000`, `&5800`, `&6000`). There is
no cursor hardware. The ULA fetches each byte at its cycle, so a write the
beam has not reached shows. The canvas is the Model B's 896 × 600: canvas
column 0 is line cycle 8, so the display window is columns 128–767, and
frame line *n* is canvas rows 2 (*n* + 22) and the next, the 256 display lines
centred.

**Sound.** In sound mode the counter's square wave, rendered at 250 kHz and
resampled with the Model B's resampler; in any other mode, silence.

**Cassette.** 1200 baud: a start bit of 0, the byte least significant first
and a stop bit of 1, on the shared cassette model. The motor follows
`&FE07` bit 6. Recording sends each byte written in cassette output mode and
the high tone while transmit empty stands.

**Keyboard.** Fourteen lines of four keys, read through ROM 8 or 9: a read
with address bit *n* low selects line *n*, pressed keys read 1 in bits 0–3,
lines selected together are ORed, and bits 4–7 read 0. The matrix is the
Guide's §14.2 table. BREAK resets the CPU; the power-on bit tells power-on
from BREAK.

**The Plus 1.** `&FC70` write 4–7 starts a conversion of channel 1–4, read
the last result; `&FC71` the printer's data, a write strobing it; `&FC72`
read: bit 7 printer busy, bit 6 low once a conversion has ended, bits 4 and 5
the fire buttons, low while held. A conversion takes 40 µs, the ADC0844's;
the printer is busy for 15 µs from each strobe. The Plus 1 raises no
interrupt. A cartridge is up to two ROM images in its slot's pair.

**Snapshots.** The container records its model, and each model's sections
are its own; a snapshot restores only onto the model and fitting it records.

### 2.3 Snapshots

A versioned container (`OMICSNAP` and a version, then the body compressed
with `lib/compress`'s RLZ codec, whose frame carries its length). The body is
tagged, length-prefixed little-endian sections: first the model (`MODL`),
then the ROM fitting, each ROM by
its `lib/crypto` fingerprint and each sideways RAM bank by its contents; the
machine's time; every register; RAM; and each device's state, the media in
the drives and deck, the picture being drawn and the last one shown among
it, so a restored machine shows at once what it showed. Sound and output the
front end has not taken are not kept. Decoding checks every field against
the range its device can hold it in and every time against the machine's
clock, refuses the whole snapshot on any failure, and changes nothing until
all of it has passed; a snapshot never names a file.

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
window does not hold. `rom::SHIPPED` pairs each name with its builder — the
Model B's MOS, the Electron's OS, BASIC, the DFS and the Plus 1 ROM. `cargo
xtask openmicro --write-roms` rebuilds them; the `openmicro-roms` static gate
and the crate's `firmware` test refuse a committed image the tree does not
build, the test also that the carried bytes are the committed ones, and the
gate refuses an image file the crate does not ship.

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
pages &08–&0D. It announces itself as `OpenMOS Micro`, where the original
prints its maker's name; the reference harness puts that in the originals'
observations (§7).

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

**The Electron's OS.** `rom::mos::build(Target::Electron)` builds it from the
same source: every part the two machines' interfaces share is one routine,
and only the hardware's own parts differ — reset and the start-up options,
the interrupt service, the screen's mode, start and palette, the cursor, the
keyboard scan, the sound output and the cassette. Its interface is Electron
OS 1.00's as the Electron Advanced User Guide documents it, with its
workspace, which is MOS 1.20's but where the Guide says otherwise:

- OSBYTE 0 and OSBYTE &81 with X 0 and Y &FF identify the Electron's OS (X 0
  and X 1). The start-up option byte defaults to &FF; MODE 7 is MODE 6, the
  mode the machine starts in. *TV and OSBYTE &90 do nothing; &9B returns at
  once; &9C, &9E and &9F, and &07, &08, &10, &11, &A3 and a positive OSBYTE
  &80, are offered to the paged ROMs, &80 then answering from the ADC values
  in page 2; &CB is the ULA's interrupt mask and &F2 the copy of `&FE07`;
  &B2 stops the keyboard's scan. Keyboard status (&CA) holds CAPS LOCK in
  bit 4 (clear while on), FUNC in 5, SHIFT in 6 and CTRL in 7.
- Interrupts: display end and RTC each mark one of the frame's two
  ten-millisecond ticks, so TIME, the interval timer, INKEY, auto-repeat,
  the sound, the keyboard's scan, the printer's ten-millisecond call and the
  100 Hz poll (service &15, while OSBYTE &16's semaphore stands) run at
  100 Hz. Display end is the vertical sync: event 4, *FX19, the flash count
  and the cassette's timeout. Anything else goes to IRQ2V, which restores A
  from &FC and returns.
- The keyboard: CAPS LK/FUNC is FUNC alone and toggles CAPS LOCK with SHIFT,
  its light `&FE07` bit 7; there is no SHIFT LOCK. FUNC with a digit is that
  soft key; FUNC with a letter, `,`, `.` or `/` is a firm key (&90–&AF), which
  a language whose type byte has bit 4 set expands through its entry point
  (A=3 then A=2), and the OS otherwise expands from its own table, the
  keywords the keycaps carry — A AUTO, B RENUMBER, C COLOUR, D DRAW, E ELSE,
  F FOR, G GOTO, H DEG, I INPUT, J RAD, K CHAIN, L LIST, M MODE, N NEXT,
  O OLD and RETURN, P PLOT, Q LOCAL, R RUN and RETURN, S STEP, T THEN,
  U UNTIL, V VDU, W RESTORE, X PROC, Y REPEAT, Z END, `,` LOAD, `.` SAVE,
  `/` PRINT. OSBYTE &E2 and &E3 set what firm keys do, &CC and &CD read the
  expansion in hand. The cursor keys move the cursor alone and type their
  characters with SHIFT and CTRL: ← `^` `~`, → `|` `\`, ↑ `£` `{`, ↓ `_` `}`,
  COPY `[` `]`, the arrangement the User Guide documents for ←.
- The screen: no cursor hardware, so the OS draws and flashes its own; VDU
  19 writes the ULA's palette from its table of entries; hardware scroll
  moves `&FE02`/`&FE03`.
- Sound: one channel and one queue, which channels 0–3 all address; a note's
  amplitude is not varied, an envelope's pitch is, and noise is a pitch the
  queue steps through; flushing any sound buffer flushes it. With the
  external sound flag (OSBYTE &DB) set, BEL and a flush are service calls
  &16 and &17 instead.
- RS423, the printer and the ADC are the expansions': a byte for one is a
  service call (&13, &14), and an unclaimed RS423 byte is purged.
- It announces itself as `OpenMOS Electron`.

Where it differs on purpose: Electron OS 1.00's private addresses are not
reproduced, so the Guide's recipe for disabling the Plus 1, which writes one
of them into FILEV, does not apply here; the firm keys `:`, `;` and `-` expand
to nothing, as the Guide documents no keyword for them; and the arrangement
of the four keys besides ← whose characters the Guide does not give.

**BASIC.** The whole BBC BASIC II language: every keyword, the tokenised
program format, the line editor and its commands, the five-byte floating
point and every function to the original's accuracy, `@%` formatting, the
inline assembler, `CALL`/`USR` conventions, errors with their numbers and
messages, and file access through the MOS. Workspace follows BASIC II: zero
page &00–&6F, the resident integers at &0400, the variable catalogue from
&0480, the control stacks of page &05, the string buffer at &0600 and the
input buffer at &0700. The one image runs on both machines.

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
SHIFT-BREAK. It claims the pages the original claims, so PAGE is &1900. It is
the Model B's alone.

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

**The Plus 1 ROM.** `rom::plus1`, a service ROM for slot 12, written afresh
to the interface the Electron Advanced User Guide gives the Plus 1. It claims
no workspace; its private workspace byte holds its flags.

- Each reset (service &01) counts it into the poll with OSBYTE &16 and
  starts a conversion; on the poll (service &15) it does its work, then
  decrements Y and claims the call at zero.
- The parallel printer, printer type 1: woken by service &14 with the first
  character since its driver went dormant, then fed each poll, each byte
  examined, sent to `&FC71` only once `&FC72` says the printer is ready, and
  removed; at most eight a poll, so the poll holds interrupts off briefly.
  An empty buffer sends the driver dormant with OSBYTE &7B.
- The ADC: one conversion a poll, the channels in turn from the highest
  OSBYTE &10 selects (&BD) down, the channel converting at &BC; a setting
  above four reads as four. Each reading is the converter's eight bits high
  in sixteen, kept in page 2 with the channel, and raises event 3. OSBYTE &10
  returns the previous selection; &11 converts its channel next, ADVAL(0)
  naming no channel until it has; &80 with X 0 answers the fire buttons and
  the last channel, the OS reading the channels from page 2 itself. &BE's
  conversion type is moot on an eight-bit converter.
- *FX163,128,1 turns both ports off, counts the ROM out of the poll, and
  purges what waits for the parallel printer; *FX163,128,0 turns them on.
  The setting survives BREAK; a hard reset clears it with the rest of
  memory.
- An OSBYTE it makes inside another call or the poll keeps that call's A, X
  and Y at &EF–&F1.

Its title is `OpenMOS Plus 1`, which a bare *HELP prints before the OS names
itself.

## 4. The worker and its protocol

The worker is the `Run` binary in its `--sandbox-session-worker` role over a
`SupervisedSession`, serving `worker::MachineService`, with one share holding
the canvas, a byte a pixel, row after row. The owner, through
`worker::MachineClient`, asks one request at a time; the worker answers each
with one reply; nothing is unsolicited. One worker runs one machine.

- **Requests:** configure — the model and its fitting: for the Model B the
  OS image, each slot's ROM image or sideways RAM, and the keyboard links;
  for the Electron the OS image, BASIC's, and the Plus 1 if fitted with its
  ROM and each cartridge slot's two images — which builds the machine at
  power-on, the media kept. A session's model is its first configure's; a
  configure for another model is refused, so no change of model drops the
  media the worker holds. Insert or eject a disc (the Model B's) or tape,
  rewind the tape, press or release RECORD; run a slice of up to a second,
  its key, BREAK, analogue and fire-button changes stamped in cycles from
  the slice's start, each key one the model has and the analogue port the
  Electron's only with the Plus 1, a slice holding any other refused before
  it runs; power off and on, the media
  kept; take or restore a snapshot; collect the discs of the drives marked
  that were written since last collected, which stay in.
- **Replies:** the cycles run; a map of the canvas rows the slice rewrote in
  the share, every row after a build or a restore; the slice's sound when
  asked, signed 16-bit at 48 kHz, AC-coupled with a 10 Hz corner as the
  machine's output is; the model's lamps — CAPS LOCK, SHIFT LOCK (the Model
  B's), the cassette motor and each drive's head (the Model B's); what the
  drives and deck hold and whether RECORD is down; the media a request took
  out; refusals with a reason, a request the model cannot take among them.
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
  the overrun never accumulates. The sound the overrunning instruction made
  goes with the next slice, so a slice's sound is its own cycles' — an
  Electron instruction waiting out the display window overruns by up to a
  hundred cycles.

Every message is decoded whole against fixed bounds — a ROM of 16 KiB, a disc
of both sides of 84 tracks, a tape or snapshot of 4 MiB, 256 input changes a
slice — and is the one encoding of what it decodes to. The client believes a
reply only when it answers what was asked within what that could produce: a
slice of the cycles asked, with no sound unless asked and no more samples
than those cycles make, lamps and media the model has — the model of the last
configure the worker took, none believed before one. A reply that does not
condemns the worker. A crashed worker is replaced by the supervisor and its
machine restarts from power-on, and the user is told why.

## 5. The front end

The host-tested model is `front` and `lib/machines`; `src/run.rs` joins them
to the windows, the sessions, the audio streams and the store.

- **The library.** `lib/machines` keeps every machine: a record of each —
  a number minted once, its name, its model and fitting, its display, key
  mapping, volume and tape loading, the name of its folder and the media of
  its folder its drives and deck held — written alternately as `library-a`
  and `library-b`, and a state blob per machine, its last screen and, while
  it is suspended, its snapshot. The store holds at most 64 blobs: the two
  copies, twenty machines' states, and the ROM images installed. The library
  window shows each machine as a card with its last screen, what it is and
  how it stands; *New…*, *Start*, *Settings…*, *Duplicate* and *Delete…* are
  its commands, and a secondary press on a card opens its menu, the
  machine's own rows among them while it runs. Deleting asks first, and
  leaves the machine's folder, the user's files, where it is.
- **The new-machine assistant** offers the models, a name, and the model's
  one choice: the Model B a new blank disc in drive 0 or none, the Electron
  the Plus 1 or none. The first run makes a Model B with its blank disc.
- **Each machine's settings** is a `FormDialog` owner-modal to the window it
  was opened from, the library or the machine's own: its name; its ROMs — the
  Model B's OS and sixteen slots, the Electron's OS, BASIC, the Plus 1 and its
  ROM and the cartridges' images — each a shipped ROM, sideways RAM where the
  socket takes it, nothing in a slot or cartridge, an installed image, or
  *Install an image…*, which asks the picker for an image and keeps it in the
  store as `rom.<name>`, offered in every socket from then on; the Model B's
  start-up links; and the sound's volume and fast tape loading. The fitting —
  ROMs, Plus 1 and links — is read-only unless the machine is off, so one that
  runs or is suspended is shut down to change it. An installed image no
  machine is fitted with is swept when the library is next read.
- **Running.** Several machines run at once, each in a window of its own over
  a worker of its own with a sound stream of its own. The window is resizable,
  opening at an `OPEN_PICTURE` 4:3 picture above the status strip; the picture
  is the largest 4:3 the window holds, centred, *sharp* or *smooth*,
  optionally with scan lines, through `tairix_crt::screen::Picture`. Only the
  rows a slice reported changed are expanded through the slot palette, scaled
  and presented, a lamp or a message repaints only the strip, and one paint
  presents only what it drew.
- **Lifetime.** Closing a machine's window suspends it — its folder's media
  written back first, then its snapshot and last screen kept — and its window
  stays open until that is on the store. *Shut Down* takes every medium out
  through the handover below — written discs back to their files or offered
  to be saved, a recorded tape saved — then turns the machine off keeping its
  last screen alone; the media of its folder stay recorded with it, and its
  window stays open until each save offered is answered. A machine being
  suspended or shut down takes no other command. Closing the library leaves
  machines running; closing every window leaves the program on the icon bar,
  whose slot's click or *Machine Library* row brings the library back; only
  *Quit* ends it, suspending every running machine first. Starting a
  suspended machine resumes it where it stopped, its snapshot restored before
  anything else is put in; a snapshot the machine refuses leaves it running
  from power-on with its folder's media put in, as a fresh start's are.
- **Keyboard.** Read raw. *Logical*: a symbol presses the machine's key and
  modifier its keyboard types it with, a letter its own key, its case the
  machine's CAPS LOCK and the host's Shift; *physical*: each key the
  machine's key in its place, the keys the PC's main block lacks on its
  editing keys, FUNC on the host's Alt for the Electron. A key is held at
  least two keyboard scans; every key is let go when the window loses the
  focus; two host keys on one machine key let it go with the last. Paste
  types the clipboard's text a key at a time.
- **Pacing.** While the sound plays the audio ring is kept at the latency the
  stream was granted: whenever it falls below, the slice that refills it is
  asked for, so the machine and the device cannot drift. Without a stream, or
  at double speed, a slice is due each 20 ms of the clock, a long stall given
  up rather than raced; at the fastest speed, or while fast tape loading has
  the motor running, slices of 100 ms run back to back, silent.
- **Media.** Discs and tapes arrive by the picker, a drop on the window or its
  icon-bar icon, Open With, or the machine's folder; reads and write-backs run
  on the file worker, never the loop. A document handed to the application
  goes to the running machine whose window had the keyboard last where it
  takes it, else another running machine that does, else the first machine of
  the library that does, started for it; it is refused when none does. A drop
  on a window is read-only, so its disc is write-protected.
  `front::media::Rack` keeps each one's record and decides what becomes of
  what the machine hands over. A disc
  whose file may be written is written back as the machine writes (collected
  when its drive's head unloads, and only such discs are collected); one
  whose may not is write-protected. A disc or tape on its way in takes its
  drive only once what it replaces is handled. A tape recorded on is saved as
  it is taken out: back to a writable UEF, else offered to be saved as one.
  A snapshot's discs and tape are its own, held only by the machine: a
  written one is offered to be saved as it is taken out. One picker is open
  at a time: offers to save wait their turn, and an Open while one is up is
  refused with a message.
- **Machine folders.** A machine's folder is `Open Micro/<name>` in the
  application's documents folder, made once the machine first needs one — its
  first blank disc, *New Disc*, *New Tape* — named from the machine's name and
  made unique there, the blank that asked for it made in it once it is, and
  each machine's made once however often it is asked for. *New Disc* (the
  Model B's drives) and *New Tape* make a blank image in it — a formatted DFS
  disc, an empty UEF — and insert it writable; *From Folder* lists the first
  eight of the folder's discs and of its tapes by name, a file the user lets
  no one write going in write-protected. A machine started or resumed before
  the documents folder is reached takes or rebinds its folder's media once
  it is. What the drives and deck
  hold from the folder is kept with the machine: a machine started fresh
  takes them in again, and a resumed one keeps its snapshot's media
  written back to them only while the file is still what was written at the
  suspend, its digest recorded then; one that changed meanwhile leaves the
  machine's copy its own. Duplicating a machine copies its folder.
- **Menus.** One set of rows, on the window's right click and, while the
  machine runs, its card's menu, the model's own: the drives (Insert Disc,
  From Folder, New Disc, Eject, Save Disc As), Tape (Insert, From Folder, New
  Tape, Rewind, Record, Eject), BREAK, Reset, Pause, Speed, Full Screen,
  Display (Sharp, Smooth, Scan Lines), Keyboard (By Character, By Position),
  Paste, Save and Restore Snapshot, Settings and Machine Library (the
  window's alone), and Shut Down. Eject follows what the machine reports in
  its drives; Save Disc As needs the disc's image held. The icon bar's menu is
  every library application's: Info, *Machine Library*, Quit.
- **Status.** A strip beneath the picture shows the model's lamps and the
  latest message.
- **Diagnostics.** `front::events` in the `OPENMICRO` range of `lib/log`: a
  machine's first second run after a build, a worker replaced, the library
  read, a machine's suspend kept, and a resumed machine's state restored.

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
  `OpenMicro.app`.
- **gzip members.** A bounded gzip member decoder joins `lib/compress`; the
  archive driver and the tape decoder share it.
- **`FormDialog`.** `lib/controls`' dialog window whose client is a column of
  field groups above its actions: a press is routed to the part it began in,
  so one that takes an open list away presses nothing beneath it; an open
  list keeps every key; Escape dismisses and an Enter no field takes presses
  the trailing action. PCEmulator's assistant and settings and this plan's
  are all one.
- **`lib/machines`.** The machine library both emulators keep: the record
  (two copies in turn, a CRC and a generation, the newer whole one read, no
  write able to leave it torn; numbers minted once; a fresh name for a new
  machine or a copy; a load that sweeps the blobs no machine owns and, when
  neither copy reads, reads as empty and sweeps nothing); the state blob (a
  last screen and a snapshot, each checked on its own, bounded by the
  application's largest frame and snapshot); the library window; how a
  machine stands; the suspends whose windows stay open until kept; the
  new-machine assistant over an application's models; the card menu and the
  icon-bar row; and, with its `rt` feature, the store over the application's
  own blobs and the library window and its dialogs over the window channel.
  An application gives its machine's own fields, its blob names and its
  bounds.
  `lib/crt::screen` gains the card's thumbnail, the screenshot and the window
  point's dot, which both emulators draw through.
- **An application's own documents folder.** A signed manifest may name one
  folder, `documents-folder = "<name>"`: a plain name of at most 64 bytes, no
  separator, no control character, not `.` or `..` and not starting with a
  dot; only an application or a component, which hold windows, may. An
  application asks the desktop session for it on one of its windows
  (`RequestDocumentsFolder`), and the session — which holds the user's own
  authority over their home — answers with `DocumentsFolder`, a grant of
  `Home/Documents/<name>/` confined beneath itself and writable
  (`fd_grant`'s `GRANT_SUBTREE`, `plans/OPENFILE.md` OF12), minted to the
  window's owner, or `DocumentsFolderRefused` with its reason: `NotTheirs`,
  `NotAFolder` or `Unavailable`. No picker is shown: the manifest's
  declaration, which the bundle's signature covers, is the request, as a
  capability is. A caller running no verified bundle, or whose installed
  manifest names no folder, is refused at once with `PermissionDenied`, and
  one already owed `DOCUMENTS_FOLDER_MAX_OPEN` with `LimitExceeded`, a request
  whose window has closed counting until the session concludes it. The path to
  `Home/Documents/` is the user's and is followed as they laid it out, each
  folder on it made where missing; the application's folder is opened without
  following a link and made if it is missing. The session marks a folder it
  makes for an application with the `tairix.documents-owner` attribute, the
  application's publisher and bundle identity, which the application cannot
  set, having no `CAP_FS_ACCESS`, and takes away one it cannot mark; it grants
  only a folder whose mark is the requester's. A folder another application's
  mark holds, or one the session did not make, is refused, so no application
  reaches another's documents or files the user made, and a folder replaced by
  a link is refused. Every grant and refusal is logged. The user may rename,
  move or delete the folder like any of theirs; the next request then makes it
  afresh. Writing there is bounded as the user's files are, by the volume,
  since the user sees and owns everything in it. The charter's §16.5 states
  this as the one write an application makes outside its store without a
  picker.

## 7. Verification

- **CPU.** A deterministic sample of the SingleStepTests 65x02 vectors
  (MIT-licensed, notice kept beside them) checks every opcode's registers,
  memory and bus cycles; unit tests pin decimal mode, interrupt timing and
  JAM.
- **Devices.** Each device has unit tests against its data sheet's
  behaviour; each machine has scenario tests on synthetic ROMs written with
  the builder — the Electron's contention, paging rule, interrupts, palette,
  wrap, keyboard lines and the Plus 1's ports among them.
- **ROMs.** Each ROM is tested on the emulated machine: typed input, captured
  VDU output and documented workspace, against expectations from the manuals
  and from the recorded corpus; the Electron's OS and the Plus 1 ROM against
  the Electron Advanced User Guide's.
- **Reference harness.** The corpus is `tests/corpus/`: each group a
  `.scenarios` file, scenarios of discs and scripted keys in the format
  `tairix_openmicro::scenario` reads, and a `.fixtures` file of what each
  left — its screen's rows and the SHA-256 of each disc it checks. The build
  lists every scenario as a test of `tests/corpus.rs`, which replays it
  against the clean-room ROMs. `cargo xtask openmicro --reference <dir>` runs
  the corpus against the original ROMs a developer holds in `<dir>`, outside
  the tree, and rewrites the fixtures from them, the clean-room ROMs' own
  names put in; a scenario marked `ours` keeps the fixture that states this
  machine's own behaviour. It refuses to write anything when an observation's
  text, or the bytes its rows write in hexadecimal, carries twelve bytes in a
  row of an original ROM that no clean-room ROM holds and the clean-room ROMs
  do not show for the same scenario. The corpus is the Model B's; the
  Electron's ROMs are held to the Guide by their own tests.
- **Fuzzing.** Every decoder of untrusted bytes has a registered harness:
  `fuzz_media` the disc and tape images, `fuzz_snapshot` snapshots of either
  model and hostile guest code on either, `fuzz_worker` the protocol in both
  directions, the worker's service and the owner's client, and the library's
  record and state blobs in `lib/machines`.
- **QEMU.** `tests/integration/openmicro_qemu_aarch64`: the desktop launches
  `OpenMicro.app` from the program library; once the library is read, the
  first run's Model B is started from its card's menu in the library window,
  the press and the row reconstructed from `lib/machines`' own layout and
  menu; once its machine records its first second run, the machine's window
  is dumped and must show the BASIC prompt's white text on black; closing the
  window suspends the machine, and once that is kept it is resumed from its
  card's menu; once its snapshot is restored, Quit from the icon-bar menu
  suspends it and ends the program cleanly.
- **Acceptance.** A recorded corpus of real software, named with its media
  hash, outcome and the ROM set it ran on.
