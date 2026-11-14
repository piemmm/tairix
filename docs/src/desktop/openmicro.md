# Open Micro Emulator (`OpenMicro.app`)

`OpenMicro.app`, the Open Micro Emulator, runs emulated 8-bit microcomputers —
the BBC Micro Model B and the Acorn Electron, with or without its Plus 1 — as
an ordinary sandboxed desktop application over a library of machines, shipping
its own clean-room ROMs: the machines, the media they read, their snapshots,
the Rust 6502 builder with the clean-room MOS, Electron OS, BASIC, DFS and
Plus 1 ROM built on it, the machine worker, the library and the windows that
drive the machines. What is left is staged in `plans/BEEB.md`.

## The machines

The Model B is a 32K machine, cycle-exact: every 6502 bus cycle, dummy
accesses included, happens at its true 2 MHz cycle, and each device is brought
up to the cycle of any access that can observe it. It holds the NMOS 6502, the
System and User VIAs with the keyboard matrix and its links, the 6845, the
video ULA and the SAA5050, the SN76489, the uPD7002, the 6850 and the serial
ULA, the 8271 with two double-sided drives, the cassette interface, and
sixteen sideways slots of ROM or RAM.

The Electron is the 6502 at 2 MHz and 32 KiB behind its ULA, cycle-exact in
the same way: the ULA's contention of RAM (the 1 MHz modes, and the display
window of the 20 KiB modes), its display in modes 0 to 6 and its palette, its
interrupts, its sound counter, its cassette interface and its paging of the
ROM window, and the keyboard read through the paged ROMs. The Plus 1, when
fitted, adds its analogue port, its printer port, its fire buttons and two
cartridge slots of two ROMs each.

Each machine is a pure function of its ROMs, media and timed input, so a run
replays exactly. `tairix_openmicro::machine::Machine` runs either model: run
it to a cycle, press keys, insert and take back discs (the Model B's) and
tapes, and collect its picture, its sound and its printer output. A bus
monitor records accesses to the I/O pages while asked.

## Media and snapshots

Discs are SSD and DSD images, written back at their own length, longer only
as far as a guest wrote past it. Tapes are UEF or CSW images, either of which
may arrive gzipped; a tape is recorded over and encodes back to UEF. A
snapshot is the whole machine: its model, every register, RAM, each device's
state and the media in the drives and the deck, restored only onto the model
and the ROMs it was taken with.

Every image and snapshot is untrusted input. Each decoder holds what it reads
and allocates to fixed bounds, checks every field before it is used, and
refuses the whole input on any failure. A snapshot changes nothing until all
of it has passed, and never names a file. The crate holds no `unsafe` code.

## The machine worker

All guest code and every image runs in a sandboxed worker holding no
capability, one worker a machine. `tairix_openmicro::worker::MachineService`
is what it serves: built for its model from the owner's ROM images, it runs
slices of up to a second with the input
changes timed within them, writes the picture rows each slice changed into
the one share, and carries the slice's sound at 48 kHz, AC-coupled as the
machine's output is. Slices tile one timeline, so the cycles a slice runs past
its end are taken from the next. Every request that takes media out — an
eject, an insert over a disc or tape, a restore — answers with what it took:
each disc written since last collected, in its own format, and the tape if it
was recorded on, so nothing a guest wrote is lost inside the worker. A collect
reaches only the drives it names, so a disc with no file to go back to stays
unsaved until it is taken out.

The owner's `MachineClient` asks one request at a time and believes a reply
only when it answers what was asked within what that could produce; one that
does not condemns the worker, which is replaced. Every message is decoded
whole against fixed bounds and is the one encoding of what it decodes to.

## The library

The library is `lib/machines`' (`docs/src/lib/machines.md`), the one
PCEmulator keeps: a record of every machine — its number, name, model and
fitting, its display, key mapping, volume and fast tape loading, its folder
and the media of its folder its drives and deck hold — written to `library-a`
and `library-b` in turn, and a state blob a machine holding its last screen
and, while it is suspended, its snapshot, all in the application's own store.
The library window shows each machine as a card of its last screen; *New…*
opens the assistant, which offers the two models, a name, and each model's one
choice: a Model B a new blank disc in drive 0 or none, an Electron its Plus 1
or none. The first run makes a Model B with its blank disc. A card's menu
starts, configures, copies and deletes its machine, and carries the machine's
own rows while it runs.

A machine's settings are an owner-modal dialog over the window it was opened
from: its name, its volume and fast tape loading at any time, and, only while
the machine is off, what is fitted — each socket's ROM (the Model B's OS and
sixteen slots, the Electron's OS and BASIC, its Plus 1's ROM and cartridges),
sideways RAM in a Model B slot, the Electron's Plus 1 and the Model B's
keyboard links. A ROM image the user installs is kept in the store as
`rom.<name>` and offered in every socket; one no machine is fitted with any
longer is swept when the library is next read.

## The machine windows

Several machines run at once, each in a window of its own over a worker of its
own with a sound stream of its own. The picture is the canvas's 4:3, the
largest that fits the window above a strip of the model's lamps: CAPS LOCK and
the cassette motor, and the Model B's SHIFT LOCK and each drive. It is
enlarged sharp, each dot a block of whole pixels, or smooth, optionally with
scan lines, through `tairix_crt::screen` — the PC emulator's picture too — and
only the rows a slice changed are expanded, scaled and presented; a lamp or
message repaints only the strip.

The keyboard is read raw. *By character*, a symbol presses the machine's key
and SHIFT that type it, and a letter its own key, its case the machine's CAPS
LOCK and the host's Shift make of it; *by position*, each key is the machine's
key in its place, the keys the PC's main block lacks on its editing keys, and
the Electron's FUNC on the host's Alt. A key is held for at least two keyboard
scans, every key is let go when the window loses the focus, and Paste types
the clipboard's text a key at a time.

While its sound plays, a machine is kept just ahead of the audio device: a
slice is asked for whenever the stream's ring falls below the latency it was
granted, so the two cannot drift. Without sound, or at double speed, a slice
is due each 20 ms of the clock; at the fastest speed, or while a tape loads
fast, slices run back to back.

Discs and tapes come from the picker, a drop on the window, Open With, or the
machine's folder. A disc whose file may be written is written back to it as
the machine writes, and one whose file may not be is write-protected; a tape
recorded on is saved as it is taken out, back to the UEF it came from or
offered to be saved as one. A restored snapshot's discs and tape belong to it:
one written is offered to be saved as it is taken out. Offers to save wait
their turn for the one file picker. A document handed to the application goes
to the running machine whose window had the keyboard last where it takes it,
else to another running machine that does, else to the first machine of the
library that does, started for it, and is refused when none does. Every file
is read and written on a worker thread, never the window's loop, and so is the
store.

Closing a machine's window suspends it: its discs are collected, its snapshot
and last screen kept, and its window stays open until they are on the store.
Starting a suspended machine resumes it, the snapshot restored before anything
else is put in. *Shut Down* takes every medium out through the same handover —
written discs back to their files or offered to be saved — and turns the
machine off, keeping its last screen alone, which is what unlocks its fitting.
Closing the library leaves machines running; closing every window leaves the
program on the icon bar, whose slot or *Machine Library* row brings the
library back; only *Quit* ends it, suspending every running machine first.

## Machine folders

A signed manifest may name one folder of the user's `Home/Documents/` as the
application's own (`documents-folder`, `docs/src/desktop/session.md`), which
the desktop session grants without a picker; `OpenMicro.app` names `Open
Micro`. A machine's folder is made there once it needs one, named after the
machine and made unique: *New Disc* (the Model B's drives) and *New Tape*
make a blank image in it — a formatted DFS disc, an empty UEF — and put it in
writable, and *From Folder* lists the folder's images, the first eight of each
kind by name. What the drives and deck hold from the folder is kept with the machine:
one started afresh takes them in again; one resumed holds its snapshot's
media, written back to its folder's files only where each still holds what it
did when the machine was suspended, its SHA-256 recorded then, so a file
changed meanwhile is never overwritten. Duplicating a machine copies its
folder; deleting it leaves the folder, which is the user's.

## The ROMs

Each ROM is Rust driving `tairix_openmicro::rom::asm::Asm`, which encodes from the
CPU's own instruction table, checks every branch, placement and run-on into
a label, places entry points at fixed addresses with the code flowing round
them and the gaps they leave filled with the tables and routines held for
them, and takes hardware addresses from the machine, so a register address is defined once
for the emulation and the firmware. The images are committed in the crate's
`firmware/` and compiled into `Run`, so the window needs no filesystem
authority to fit them: `cargo xtask openmicro --write-roms` rebuilds them, and the
`openmicro-roms` gate refuses one the tree does not build.

`tairix_openmicro::rom::mos::build` assembles the operating system for a
model from one source: for the Model B the whole MOS 1.20
interface — the vectors and extended vectors, sideways ROM services, OSBYTE,
OSWORD, OSCLI and its commands, the VDU drivers in all eight modes, the
keyboard with soft keys and auto-repeat, buffers and events, sound, the serial
and printer streams, the ADC, and the cassette and ROM filing systems — with
its documented workspace at the documented addresses, and its vectors'
defaults at MOS 1.20's addresses, so software that sets a vector back to its
original value still reaches the routine. The internals software reads at
MOS 1.20's addresses lie there too: the default vector table at &D940, the
row table at &C375, and the MODE routine at &CB1D. It is written from the
published manuals and from genuine ROMs observed only as opaque programs;
no original ROM's code is read, and no genuine ROM byte enters the tree.

Its screen memory, sound chip writes and documented results match MOS 1.20's
for the same input. Where it differs on purpose: its character set is its own
design; it does not reproduce &31A for a clipped line's start, some private
keyboard and sound workspace, or redundant writes to a silenced sound
channel; and, with no room left in 16 KiB, a whole-screen clear takes twice
MOS 1.20's time, a horizontal line fill three times, VDU 5 a third longer
and lines a quarter longer, where text output, scrolling and triangles are
within a few percent of it.
The behaviour pinned against MOS 1.20 is listed in the ROMs section of
`plans/BEEB.md`.

`tairix_openmicro::rom::basic::build` assembles BBC BASIC: the whole BASIC II
language — every keyword and the tokenised program format, the line editor
with AUTO, DELETE, RENUMBER, LIST and LISTO, the five-byte reals and every
function, `@%` formatting, PROC and FN with LOCAL, the inline assembler, CALL
and USR, errors with BASIC II's numbers and messages, and SAVE, LOAD, CHAIN
and the file channels through the MOS — in BASIC II's workspace: zero page
&00–&6F, the resident integers at &400, the variable catalogue from &480, the
stacks of page 5, the string buffer at &600 and the input buffer at &700. Its
error handler is entered at BASIC II's &B402, and &B433, where software
points BRKV to stop the machine at any error, stops it as BASIC II does. It
is written from the published guides and from BASIC II observed running, by
the same rules as the MOS.

Its screens, the memory it writes and its results match BASIC II's for the
same input: every real it computes has BASIC II's bits, and a string is given
BASIC II's room as it grows. Where it differs on purpose: its copyright
message is its own, as long as BASIC II's; an instruction whose operand calls
an FN that assembles code is placed after that code, where BASIC II's goes
astray; a byte (`?`) as a PROC or FN parameter is neither kept nor put back,
where BASIC II's damages the program; and, in 16 KiB, it is slower than
BASIC II, loops taking from 2% to 11% longer, 29% where an integer FOR adds
integers, and the functions within a tenth, while TAN and small whole powers
are faster.

`tairix_openmicro::rom::dfs::build` assembles the DFS: the whole Acorn DFS 1.20
interface over the 8271 — the catalogue format, every `*` command, OSFILE,
OSFIND, OSGBPB, OSARGS and the byte calls on up to five open files, OSWORD
&7D–&7F, `!BOOT`, `*OPT 4` and SHIFT-BREAK — in DFS 1.20's workspace: the
catalogue at &E00, the settings in page &10, the channels from &1100 and the
NMI routine at &D00, so PAGE is &1900. It is written by the same rules as the
MOS.

Its screens and the discs it writes match DFS 1.20's for the same steps, and
it loads as fast. Where it differs on purpose: its banner and copyright are
its own, as are the flags the manuals leave undefined and the registers a
program is `*RUN` with; the rest of a file's last sector is zero, where DFS
1.20 writes whatever its buffer last held, of that file or another; a buffer
is written back only to its file's disc, where DFS 1.20 writes it to
whatever disc is in the drive; a channel follows its file when `*COMPACT`
moves it; a file to be entered in the sideways window, where DFS 1.20 runs
its own code, is loaded and not entered; and a save can take a revolution
less.

For the Electron the same source builds the Electron OS 1.00 interface over
the ULA: the paged ROM window and its keyboard, the single sound channel the
four SOUND channels address, the screen without cursor hardware, FUNC with a
digit for the soft keys and with a letter for the keyword its keycap carries,
the cassette at 1200 baud, and the expansions' service calls for the printer,
RS423 and the ADC. It announces itself as `OpenMOS Electron`. Where it differs
on purpose: Electron OS 1.00's private addresses are not reproduced, so the
Guide's recipe for disabling the Plus 1 does not apply, and the firm keys the
Guide gives no keyword expand to nothing.

`tairix_openmicro::rom::plus1::build` assembles the Plus 1's service ROM for
slot 12, to the interface the Electron Advanced User Guide gives the Plus 1:
counted into the 100 Hz poll at each reset, it feeds the parallel printer and
runs the ADC a conversion a poll, each reading raising event 3; it claims no
workspace, and `*FX163,128,1` turns both ports off and `*FX163,128,0` on. Its
title is `OpenMOS Plus 1`.

## Testing

- The crate's tests cover every device of both models, the media codecs,
  snapshots taken and restored mid-run, the builder, the MOS and the Electron
  OS driven through their entry points by test language ROMs, the Plus 1 ROM
  against the Guide's interface, and BASIC typed at its prompt, each screen
  and result as BASIC II gave them; the front end's model — the library
  record, settings, assistant, folder, keys, menus, media and pacing — is
  host-tested.
- `tests/corpus.rs` replays the recorded corpus in `tests/corpus/`: scenarios
  of discs and keys typed at BASIC, each screen and disc as MOS 1.20, BASIC
  II and DFS 1.20 left them. `cargo xtask openmicro --reference <dir>` records
  the fixtures again from the original ROMs a developer holds in `<dir>`
  (`mos.rom`, `basic.rom`, `dfs.rom`), outside the tree, and refuses to
  write any that would carry those ROMs' bytes.
- `tests/cpu_vectors.rs` replays a committed sample of the SingleStepTests
  65x02 suite; `cargo xtask openmicro --vectors <dir>` runs the whole suite.
- `fuzz_media`, `fuzz_snapshot` and `fuzz_worker` are registered with
  `cargo xtask fuzz`, and `lib/machines`' `fuzz_library` covers the library's
  record and state blobs.
- `openmicro_qemu_aarch64` launches the emulator on the desktop, starts the
  Model B its first run makes from its card's menu, reads its BASIC prompt off
  the screen, suspends it by closing its window, resumes it from its card, and
  quits it from the icon bar.
