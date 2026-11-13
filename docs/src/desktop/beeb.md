# BBC Micro (`beeb.app`)

`beeb.app` is an emulated BBC Micro Model B run as an ordinary sandboxed
desktop application, shipping its own clean-room ROMs: the machine, the media
it reads, its snapshots, the Rust 6502 builder with the clean-room MOS, BASIC
and DFS built on it, the machine worker and the window that drives it. What
is left is staged in `plans/BEEB.md`.

## The machine

The machine is a 32K Model B, cycle-exact: every 6502 bus cycle, dummy
accesses included, happens at its true 2 MHz cycle, and each device is brought
up to the cycle of any access that can observe it. It holds the NMOS 6502, the
System and User VIAs with the keyboard matrix and its links, the 6845, the
video ULA and the SAA5050, the SN76489, the uPD7002, the 6850 and the serial
ULA, the 8271 with two double-sided drives, the cassette interface, and
sixteen sideways slots of ROM or RAM. It is a pure function of its ROMs, media
and timed input, so a run replays exactly.

`tairix_beeb::machine::Machine` is the whole interface: run it to a cycle,
press keys, insert and take back discs and tapes, and collect its picture, its
sound and its printer and RS423 output. A bus monitor records accesses to the
I/O pages while asked.

## Media and snapshots

Discs are SSD and DSD images, written back at their own length, longer only
as far as a guest wrote past it. Tapes are UEF or CSW images, either of which
may arrive gzipped; a tape is recorded over and encodes back to UEF. A
snapshot is the whole machine: every register, RAM, each device's state and
the media in the drives, restored only onto the ROMs it was taken with.

Every image and snapshot is untrusted input. Each decoder holds what it reads
and allocates to fixed bounds, checks every field before it is used, and
refuses the whole input on any failure. A snapshot changes nothing until all
of it has passed, and never names a file. The crate holds no `unsafe` code.

## The machine worker

All guest code and every image runs in a sandboxed worker holding no
capability. `tairix_beeb::worker::MachineService` is what it serves: built
from the owner's ROM images, it runs slices of up to a second with the input
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

## The window

The picture is the canvas's 4:3, the largest that fits the window above a
strip of lamps for CAPS LOCK, SHIFT LOCK, the cassette motor and each drive.
It is enlarged sharp, each dot a block of whole pixels, or smooth, optionally
with scan lines, through `tairix_crt::screen` — the PC emulator's picture
too — and only the rows a slice changed are expanded, scaled and presented.

The keyboard is read raw. *By character*, a symbol presses the BBC key and
SHIFT that type it, and a letter its own key, its case the machine's CAPS
LOCK and the host's Shift make of it; *by position*, each key is the BBC key
in its place, the keys the PC's main block lacks on its editing keys. A key
is held for at least two keyboard scans, every key is let go when the window
loses the focus, and Paste types the clipboard's text a key at a time.

While its sound plays, the machine is kept just ahead of the audio device: a
slice is asked for whenever the stream's ring falls below the latency it was
granted, so the two cannot drift. Without sound, or at double speed, a slice
is due each 20 ms of the clock; at the fastest speed, or while a tape loads
fast, slices run back to back.

Discs and tapes come from the picker, a drop on the window, or Open With. A
disc whose file may be written is written back to it as the machine writes,
and one whose file may not be is write-protected; a tape recorded on is saved
as it is taken out, back to the UEF it came from or offered to be saved as
one. A restored snapshot's discs and tape belong to it: one written is offered
to be saved as it is taken out, and quitting takes every medium out first.
Offers to save wait their turn for the one file picker. Every file is read and
written on a worker thread, never the window's loop, and so are the settings,
which the settings window edits: the ROMs in the OS socket and sideways slots,
installed ROM images kept in the application's store, the keyboard links, the
picture, the keys, the volume and fast tape loading.

## The ROMs

Each ROM is Rust driving `tairix_beeb::rom::asm::Asm`, which encodes from the
CPU's own instruction table, checks every branch, placement and run-on into
a label, and takes
hardware addresses from the machine, so a register address is defined once
for the emulation and the firmware. The images are committed in the crate's
`firmware/` and compiled into `Run`, so the window needs no filesystem
authority to fit them: `cargo xtask beeb --write-roms` rebuilds them, and the
`beeb-roms` gate refuses one the tree does not build.

`tairix_beeb::rom::mos::build` assembles the MOS: the whole MOS 1.20
interface — the vectors and extended vectors, sideways ROM services, OSBYTE,
OSWORD, OSCLI and its commands, the VDU drivers in all eight modes, the
keyboard with soft keys and auto-repeat, buffers and events, sound, the serial
and printer streams, the ADC, and the cassette and ROM filing systems — with
its documented workspace at the documented addresses. It is written from the
published manuals and from genuine ROMs observed only as opaque programs;
no original ROM's code is read, and no genuine ROM byte enters the tree.

Its screen memory, sound chip writes and documented results match MOS 1.20's
for the same input. Where it differs on purpose: its character set is its own
design; it does not reproduce &31A for a clipped line's start, some private
keyboard and sound workspace, or redundant writes to a silenced sound
channel; and, with no room left in 16 KiB, a whole-screen clear, VDU 5 and
lines are slower than MOS 1.20's, where text output is within a tenth of it.
The behaviour pinned against MOS 1.20 is listed in the ROMs section of
`plans/BEEB.md`.

`tairix_beeb::rom::basic::build` assembles BBC BASIC: the whole BASIC II
language — every keyword and the tokenised program format, the line editor
with AUTO, DELETE, RENUMBER, LIST and LISTO, the five-byte reals and every
function, `@%` formatting, PROC and FN with LOCAL, the inline assembler, CALL
and USR, errors with BASIC II's numbers and messages, and SAVE, LOAD, CHAIN
and the file channels through the MOS — in BASIC II's workspace: zero page
&00–&6F, the resident integers at &400, the variable catalogue from &480, the
stacks of page 5, the string buffer at &600 and the input buffer at &700. It
is written from the published guides and from BASIC II observed running, by
the same rules as the MOS.

Its screens, the memory it writes and its results match BASIC II's for the
same input: every real it computes has BASIC II's bits, and a string is given
BASIC II's room as it grows. Where it differs on purpose: its copyright
message is its own, as long as BASIC II's; an instruction whose operand calls
an FN that assembles code is placed after that code, where BASIC II's goes
astray; a byte (`?`) as a PROC or FN parameter is neither kept nor put back,
where BASIC II's damages the program; and, in 16 KiB, its integer work is
slower than BASIC II's, loops taking from 8% to 29% longer and the
functions within a tenth, while TAN and small whole powers are faster.

`tairix_beeb::rom::dfs::build` assembles the DFS: the whole Acorn DFS 1.20
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

## Testing

- The crate's tests cover every device, the media codecs, snapshots taken and
  restored mid-run, the builder, the MOS driven through its entry points by
  test language ROMs, and BASIC typed at its prompt, each screen and result
  as BASIC II gave them.
- `tests/corpus.rs` replays the recorded corpus in `tests/corpus/`: scenarios
  of discs and keys typed at BASIC, each screen and disc as MOS 1.20, BASIC
  II and DFS 1.20 left them. `cargo xtask beeb --reference <dir>` records
  the fixtures again from the original ROMs a developer holds in `<dir>`
  (`mos.rom`, `basic.rom`, `dfs.rom`), outside the tree, and refuses to
  write any that would carry those ROMs' bytes.
- `tests/cpu_vectors.rs` replays a committed sample of the SingleStepTests
  65x02 suite; `cargo xtask beeb --vectors <dir>` runs the whole suite.
- `fuzz_media`, `fuzz_snapshot` and `fuzz_worker` are registered with
  `cargo xtask fuzz`.
