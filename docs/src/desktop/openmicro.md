# Open Micro Emulator (`OpenMicro.app`)

`OpenMicro.app`, the Open Micro Emulator, runs emulated 8-bit microcomputers —
the BBC Micro Model B, the Acorn Electron, with or without its Plus 1, and the
Commodore 64, with its 1541 drives — as an ordinary sandboxed desktop
application over a library of machines, shipping its own clean-room ROMs: the
machines, the media they read, their snapshots, the Rust 6502 builder with the
clean-room MOS, Electron OS, BASIC, DFS and Plus 1 ROM and the Commodore 64's
KERNAL, BASIC V2, character ROM and 1541 DOS built on it, the machine worker,
the library and the windows that drive the machines. What is left is staged
in `plans/BEEB.md` and `plans/C64.md`.

## The machines

The Model B is a 32K machine, cycle-exact: every 6502 bus cycle, dummy
accesses included, happens at its true 2 MHz cycle, and each device is brought
up to the cycle of any access that can observe it. It holds the NMOS 6502, the
System and User VIAs with the keyboard matrix and its links, the 6845, the
video ULA and the SAA5050, the SN76489, the uPD7002, the 6850 and the serial
ULA, the 8271 with two double-sided drives, the cassette interface, and
sixteen sideways slots of ROM or RAM. The drives' motors run while the 8271
has a head loaded, so a drive left idle stops, and a disc filing system then
finds it not ready and reads its catalogue again.

The Electron is the 6502 at 2 MHz and 32 KiB behind its ULA, cycle-exact in
the same way: the ULA's contention of RAM (the 1 MHz modes, and the display
window of the 20 KiB modes), its display in modes 0 to 6 and its palette, its
interrupts, its sound counter, its cassette interface and its paging of the
ROM window, and the keyboard read through the paged ROMs. The Plus 1, when
fitted, adds its analogue port, its printer port, its fire buttons and two
cartridge slots of two ROMs each.

The Commodore 64 is cycle-exact in the same way, PAL (the 6569 VIC-II at
985 248 Hz) or NTSC (the 6567R8 at 1 022 727 Hz): the 6510 and its port, the
PLA's banking, the VIC-II's fetches with its bad lines, sprites, collisions
and border, two CIAs (6526s beside a 6581 SID, 8521s beside an 8580), the
SID, the keyboard matrix with RESTORE, two control ports with joysticks and
paddles, the datasette and the expansion port. Its drives are devices 8 and
9 on the serial bus, each a true 1541: its own 6502 at 1 MHz, two VIAs and a
GCR head over a turning disk, its time the machine's scaled by the exact
ratio of the two clocks.

A Commodore 64's SID and its drives each run on a thread of their own
(`tairix_openmicro::machine::lane`), behind the machine: the machine posts
what it does to them stamped with its cycle — a write to the SID, a write to
CIA2, and with two drives every read of it — and tells their threads how far
it has run, and wherever it needs one as it stands at a cycle (a read of OSC3
or ENV3, of the serial lines, the end of a slice) it brings it up itself,
waiting on its thread for one piece of work at most. A thread takes the SID
in pieces and two drives only through whole turns, as far as bringing them up
to any later cycle would have, so which thread did the work never shows: the
machine is the same with its threads or without them, and runs without them
wherever none can be had. The SID's data bus and paddle readings, which no
sound depends on, stay with the machine, so reading them waits on nothing.

Each machine is a pure function of its ROMs, media and timed input, so a run
replays exactly. `tairix_openmicro::machine::Machine` runs any model: run it
to a cycle, press keys, insert and take back discs (the Model B's), disks (a
Commodore 64's), tapes and cartridges, load a Commodore 64 program into
memory, and collect its picture, its sound and its printer output. A bus
monitor records accesses to the I/O pages while asked.

## Media and snapshots

Discs are SSD and DSD images, written back at their own length, longer only
as far as a guest wrote past it. Tapes are UEF or CSW images, either of which
may arrive gzipped; a tape is recorded over and encodes back to UEF. A
Commodore 64's disks are D64 and G64 images: a disk written goes back as a D64
of its track count while every sector still decodes, else as a G64, and a G64
always as a G64. Its tapes are TAP images, recorded over and encoded back as
version 1; its cartridges CRT images of the plain 8K, 16K and Ultimax kinds
and the common bank-switched ones; and its programs PRG files and the first
program of a T64, put in memory as the KERNAL's LOAD leaves them. A snapshot
is the whole machine: its model, every register, RAM, each device's state and
the media in the drives, the deck and the expansion port, restored only onto
the model, the ROMs and, on a Commodore 64, the standard, SID and drives it
was taken with.

Every image and snapshot is untrusted input. Each decoder holds what it reads
and allocates to fixed bounds, checks every field before it is used, and
refuses the whole input on any failure. A snapshot changes nothing until all
of it has passed, and never names a file. The crate holds no `unsafe` code.

## The machine worker

All guest code and every image runs in a sandboxed worker holding no
capability, one worker a machine, granted three threads: its own, and one
each for a Commodore 64's SID and drives (`worker::WORKER_THREADS`).
`tairix_openmicro::worker::MachineService`
is what it serves: built for its model from the owner's ROM images, it runs
slices of up to a second of the machine's own clock with the input changes
timed within them, writes the picture rows each slice changed into the one
share, and carries the slice's sound at 48 kHz, resampled from the machine's
clock and AC-coupled as its output is. Each slice also reports the lamps, the
drives' lights, what the drives, deck and port hold, and how the letter keys
type, which Paste types by. Slices tile one timeline, so the cycles a slice
runs past its end are taken from the next. Every request that takes media out
— an eject, an insert over a disc, disk or tape, a restore — answers with what
it took: each disc or disk written since last collected, in its own format,
and the tape if it was recorded on, so nothing a guest wrote is lost inside
the worker. A collect reaches only the drives it names, so a disc with no file
to go back to stays unsaved until it is taken out.

The owner's `MachineClient` asks one request at a time and believes a reply
only when it answers what was asked within what that could produce — a slice's
sound within the machine's own second, and lamps, a letter case and media only
of the model and the drives it has; one that does not condemns the worker,
which is replaced. Every message is decoded
whole against fixed bounds and is the one encoding of what it decodes to.

A window drives its worker over `tairix_openmicro::worker::Link`: the
supervised session, its two pipes on the window's wait-set, and the client. A
worker lost leaves nothing asked and neither pipe watched, so its replacement,
built from power-on with the machine's media put back — a cartridge only when
plugged in from its file, a restored snapshot's having no image to put back —
is asked afresh; a window let go stops watching its pipes before it closes
them, so the descriptor numbers they held are free for the next pipe. A machine
whose pipes cannot be watched is stopped with that reason.

## The library

The library is `lib/machines`' (`docs/src/lib/machines.md`), the one
PCEmulator keeps: a record of every machine — its number, name, model and
fitting, its display, key mapping, volume and fast loading, its folder
and the media of its folder its drives and deck hold — written to `library-a`
and `library-b` in turn, and a state blob a machine holding its last screen
and, while it is suspended, its snapshot, all in the application's own store.
The library window shows each machine as a card of its last screen, and a
machine never yet run as its model's boot screen: the clean-room ROMs' own,
drawn once on the build host by `cargo xtask openmicro --write-roms` and
carried in `Run` beside the ROM images, so no guest code runs outside a
worker to draw it. *New…*
opens the assistant, which offers the three models, a name, and each model's
choices: a Model B a new blank disc in drive 0 or none, an Electron its Plus 1
or none, and a Commodore 64 its standard (PAL or NTSC), its SID (6581 or
8580) and its drives (one 1541, two or none). A new Model B has BASIC, the DFS
and VIM in slots 15, 14 and 13, and 64 KiB of sideways RAM, four banks in slots
4 to 7, where the Master keeps its own and software looks for them. The first
run makes a Model B with its blank disc. A card's menu
starts, configures, copies and deletes its machine, and carries the machine's
own rows while it runs.

A machine's settings are an owner-modal dialog over the window it was opened
from: its name, its volume, fast loading and a Commodore 64's keypad
joystick at any time, and, only while the machine is off, what is fitted —
each socket's ROM (the Model B's OS and sixteen slots, the Electron's OS and
BASIC, its Plus 1's ROM and cartridges, the Commodore 64's KERNAL, BASIC,
character ROM and each drive's DOS), sideways RAM in a Model B slot, the
Electron's Plus 1, the Commodore 64's standard, SID and drives, and the Model
B's keyboard links. A ROM image the user installs is kept in the store as
`rom.<name>` and offered in every socket; one no machine is fitted with any
longer is swept when the library is next read.

## The machine windows

Several machines run at once, each in a window of its own over a worker of its
own with a sound stream of its own. The picture is the canvas's 4:3, the
largest that fits the window above a strip of the model's lamps: an Acorn's
CAPS LOCK and cassette motor, the Model B's SHIFT LOCK and each drive, and a
Commodore 64's datasette motor and each 1541's light. On a Model B or an
Electron the strip's trailing end holds a SHIFT-BREAK button, as the menu has
a row: BREAK pressed and let go with SHIFT held on until the reset has read
the keyboard, which boots the disc or tape. It is
enlarged sharp, each dot a block of whole pixels, or smooth, optionally with
scan lines, through `tairix_crt::screen` — the PC emulator's picture too — and
only the rows a slice changed are expanded, scaled and presented, a row
alike the one before it copied rather than expanded again, as a Commodore 64's
every line is; a lamp or message repaints only the strip. While the tube adds
nothing at the window's size, the canvas is not enlarged here at all: the
window carries it as its view ([Window views](wm.md#window-views)), which the
display engine enlarges on a layer of its own where it can, so a full-screen
machine costs the canvas's rows a frame rather than the screen's. A view the
desktop refuses is not asked for again at that window size: the window
enlarges the canvas itself there.

The keyboard is read raw. *By character*, a symbol presses the machine's key
and SHIFT that type it, and a letter its own key, its case the machine's CAPS
LOCK and the host's Shift make of it; *by position*, each key is the machine's
key in its place, the keys the PC's main block lacks on its editing keys, and
the Electron's FUNC on the host's Alt. On a Commodore 64 the host's Alt is the
Commodore key and Ctrl is CTRL, Escape is RUN/STOP, F12 and Page Up are
RESTORE, F1, F3, F5 and F7 are its function keys and F2, F4, F6 and F8 them
with SHIFT; while its keypad joystick is on, the numeric keypad's arrows move
the stick in control port 2 or 1 and 0 and 5 fire. A key is held until two of
the OS's keyboard scans have seen it, every key and the stick are let go when
the window loses the focus, and Paste types the clipboard's text a key at a
time, each letter in the case the machine shows: by its CAPS LOCK, or by a
Commodore 64's character set, its upper-case set typing a small letter as its
capital.

While its sound plays, a machine is kept just ahead of the audio device: a
slice is asked for whenever the stream's ring falls below the latency it was
granted, so the two cannot drift. A machine with no stream to be had, or
whose stream fails, runs silent, and its status strip says why. Without sound, or at double speed, a slice
is due each 20 ms of the clock; at the fastest speed, or while a tape loads
fast, slices run back to back.

Discs, disks and tapes come from the picker, a drop on the window, Open
With, or the machine's folder, and a Commodore 64's cartridges and programs
from the picker, a drop or Open With; a drop the machine has nothing to take
is refused, and one that arrives while the machine starts goes in once it is
built, and one for a machine being put away is refused, the window saying so.
A machine suspended, shut down or stopped lets go of every file its media
came from. A disc or disk whose file may be written is written back to it as
the machine writes, and one whose file may not be is write-protected. A disc,
disk or tape the machine may write back is held under an exclusive advisory
lock on its whole file while it is in, so mounting it as a volume, or
putting it in another machine to write, waits until it is taken out; one
already locked elsewhere — a DFS disc mounted to show its files, or a disc
another machine holds — goes in write-protected, the status strip saying
why. Opening a DFS disc in Files shows its files
([archives](../filesystem/archive.md)) rather than starting a machine; a drop
on the window or Open With puts it in a drive. A disc dropped on a window,
or a document opened with the emulator, brings its machine's window forward
with the keyboard, on the one raise the user's gesture lends. A D64
that can no longer hold its disk is let go and the disk offered to be saved
as a G64. A tape recorded on is saved as it is taken out, back to the UEF or
TAP it came from or offered to be saved as one. Plugging a cartridge in or
taking it out resets a Commodore 64, and Reset and a suspend keep it, Shut
Down not; *Load Program* puts a program in memory as LOAD would, neither
resetting the machine nor typing RUN. A restored snapshot's discs and tape
belong to it: one written is offered to be saved as it is taken out. Offers to save wait
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
program on the icon bar. Its slot's click, like its *Machine Library* row,
always brings the library forward, a machine's window open or not; only
*Quit* ends it, suspending every running machine first.

## Machine folders

A signed manifest may name one folder of the user's `Home/Documents/` as the
application's own (`documents-folder`, `docs/src/desktop/session.md`), which
the desktop session grants without a picker; `OpenMicro.app` names `Open
Micro`. A machine's folder is made there once it needs one, named after the
machine and made unique: *New Disc* (the Model B's drives), *New Disk* (a
Commodore 64's) and *New Tape* make a blank image in it — a formatted DFS disc,
a D64 formatted as the 1541 DOS formats one, an empty UEF or TAP — and put it
in writable, and *From Folder* lists the folder's discs or disks and tapes, the
first eight of each kind by name. What the drives and deck hold from the folder
is kept with the machine: one started afresh takes them in again; one resumed
holds its snapshot's media, written back to its folder's files only where each
still holds what it did when the machine was suspended, its SHA-256 recorded
then, so a file changed meanwhile is never overwritten. *Save Snapshot* keeps
the whole machine in its folder as `<name>.snapshot`, the first number after
the name where that is taken, a file cut short taken away again; a machine with
no folder yet saves where the picker is told. *Show Folder*, on the machine's
menu and its card's, has the desktop open the folder in Files, through a
location grant that confers nothing, so what the machine keeps there can be
copied to other media. Duplicating a machine copies its folder; deleting it
leaves the folder, which is the user's.

## The ROMs

Each ROM is Rust driving `tairix_openmicro::rom::asm::Asm`, which encodes from the
CPU's own instruction table, checks every branch, placement and run-on into
a label, places entry points at fixed addresses with the code flowing round
them and the gaps they leave filled with the tables, routines and blocks held
for them, the rest packed into the free space after the code, and takes
hardware addresses from the machine, so a register address is defined once
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
row tables at &C375, the mode tables from &C3F7 to &C4A9 in MOS 1.20's
order, the MODE routine at &CB1D, the routines a program setting a mode
itself calls — the VDU status and'ed at &C5A8, VDU 20 at &C839, a 6845
register at &C95E, VDU 26 at &C9BD, the cursor at &C9F6, a 6845 register
pair at &CA2B, OSBYTE 20 at &CD07, the latch at &E9F8 and the video ULA at
&EA00, each with MOS 1.20's registers — and the reset and interrupt entries
at &D9CD and &DC1C the processor's vectors name, which software reads to
know the OS by. The workspace drives the
drivers as it drives MOS 1.20's, so a program that sets out a mode by writing
it is drawn alike: the row table at &E0 is read halved for the 320-byte rows,
and text is drawn through its masks at &D2 and &D3 as an OR then an EOR. It
is written from the published manuals and from genuine ROMs observed only as
opaque programs;
no original ROM's code is read, and no genuine ROM byte enters the tree.

Its screen memory, sound chip writes and documented results match MOS 1.20's
for the same input. Where it differs on purpose: its character set is its own
design, [`lib/vdu`](../lib/vdu.md)'s BBC arrangement; it does not reproduce
&31A for a clipped line's start, some private keyboard and sound workspace, or
redundant writes to a silenced sound channel; a program `*RUN` is entered with
two bytes fewer on the stack than under MOS 1.20; and, with no room left in
16 KiB, a whole-screen clear takes 1.7 times MOS 1.20's time, a horizontal line fill
two and a half times and a line a fifth longer, where text output, scrolling,
triangles and POINT are within a few percent of it, a VDU 5 cell takes a
fifth less and its unlit pixels cost nothing. The behaviour pinned against MOS 1.20 is listed in
the ROMs section of `plans/BEEB.md`.

`tairix_openmicro::rom::basic::build` assembles BBC BASIC: the whole BASIC II
language — every keyword and the tokenised program format, the line editor
with AUTO, DELETE, RENUMBER, LIST and LISTO, the five-byte reals and every
function, `@%` formatting, PROC and FN with LOCAL, the inline assembler, CALL
and USR, errors with BASIC II's numbers and messages, and SAVE, LOAD, CHAIN
and the file channels through the MOS — in BASIC II's workspace: zero page
&00–&4E, the resident integers at &400, the variable catalogue from &480, the
stacks of page 5, the string buffer at &600 and the input buffer at &700. Like
BASIC II, it never writes &4F–&6F, where programs keep values their interrupt
code reads. Its
error handler is entered at BASIC II's &B402, and &B433, where software
points BRKV to stop the machine at any error, stops it as BASIC II does. A
call to &AF12 with n in the integer accumulator at &2A returns RND(n), and
one to &AF51 leaves the next random integer there, as games calling BASIC II
at those addresses for a random choice expect. An error drops the
loops, PROCs and FNs it left open and starts READ from the first DATA before
ON ERROR's statements run, as BASIC II's does. It is written from the
published guides and from BASIC II observed running, by the same rules as
the MOS. Its keywords and token values are
[`lib/openbasic`](../lib/openbasic.md)'s BASIC II tables, and its tokeniser and
lister are tested against that library's on the emulated machine.

Its screens, the memory it writes and its results match BASIC II's for the
same input: every real it computes has BASIC II's bits, and a string is given
BASIC II's room as it grows. Where it differs on purpose: its copyright
message is its own, as long as BASIC II's; an instruction whose operand calls
an FN that assembles code is placed after that code, where BASIC II's goes
astray; a byte (`?`) as a PROC or FN parameter is neither kept nor put
back, where BASIC II's damages the program; and an array keeps its shape
through an FN in any of its dimensions, and a loop through an FN that runs
FOR in its limit or step, where BASIC II's loses both. Measured as a program runs
them, with their separators or alone on their lines, its statements are as
fast as BASIC II's or faster — REPEAT, a false IF, TRUE, a GOSUB's line
search, real division, the functions and string joins among them, a quoted
string joining a quarter to a third faster. An expression nests 18
integer levels and 20 real before it is No room, where BASIC II's evaluates
17 and 16 before its stack wraps, and an FN's level takes no more of the
BASIC stack than BASIC II's. A long line
with many names takes up to twice as long to enter.

`tairix_openmicro::rom::dfs::build` assembles the DFS: the whole Acorn DFS 1.20
interface over the 8271 — the catalogue format, every `*` command, OSFILE,
OSFIND, OSGBPB, OSARGS and the byte calls on up to five open files, OSWORD
&7D–&7F, `!BOOT`, `*OPT 4` and SHIFT-BREAK — in DFS 1.20's workspace: the
catalogue at &E00, the settings in page &10, the channels from &1100 and the
NMI routine at &D00, so PAGE is &1900. It is written by the same rules as the
MOS.

Its screens and the discs it writes match DFS 1.20's for the same steps, and
it loads as fast. Where it differs on purpose: its banner and copyright are
its own, as are the flags the manuals leave undefined; a load may land
anywhere from &E00 up, the DFS's own workspace included, and lands whole and
is entered, where DFS 1.20 can lose the rest of it; the rest of a file's last
sector is zero, where DFS 1.20 writes whatever its buffer last held, of that
file or another; a buffer is written back only to its file's disc, where
DFS 1.20 writes it to
whatever disc is in the drive; a channel follows its file when `*COMPACT`
moves it; a file to be entered in the sideways window, where DFS 1.20 runs
its own code, is loaded and not entered; and a save can take a revolution
less.

For the Electron the same source builds the Electron OS 1.00 interface over
the ULA: the paged ROM window and its keyboard, four SOUND queues on its one
sound channel, a SOUND for another channel flushing the one last sounded, the
screen without cursor hardware, FUNC with a digit for the soft keys and with a
letter for the keyword its keycap carries, the cassette at 1200 baud, and the
expansions' service calls for the printer, RS423 and the ADC, with page 2 laid
out where Electron OS 1.00 keeps it. It announces itself as `OpenMOS
Electron`. Where it differs on purpose: Electron OS 1.00's private addresses
are not reproduced, so the Guide's recipe for disabling the Plus 1 does not
apply; the firm keys the Guide gives no keyword expand to nothing; its key
scan keeps its state elsewhere than `&2C0–&2C2`; and 31 pitches set the ULA's
counter a step to three from the original's own table.

`tairix_openmicro::rom::plus1::build` assembles the Plus 1's service ROM for
slot 12, to the interface the Electron Advanced User Guide gives the Plus 1:
counted into the 100 Hz poll at each reset, it feeds the parallel printer, once
`*FX5,1` chooses it, since the Electron starts with no printer chosen, and
runs the ADC a conversion a poll, each reading raising event 3; it claims no
workspace, and `*FX163,128,1` turns both ports off and `*FX163,128,0` on. Its
title is `OpenMOS Plus 1`.

`tairix_openmicro::rom::vim::build` assembles `VIM`, a vi-style editor of the
tree's own design (`plans/BEEB-VIM.md`), which a new Model B has in slot 13. It
is a language with a service entry: `*VIM name` from any language edits a
file, `:q` returns to that language, `*VIM` alone starts with an empty
text, and BREAK keeps the text, which leaving gives up to the language
returned to. Its workspace is a language's — zero page
&00–&6F, leaving &70–&8F to the user, and pages &04–&07 — and its text, undo
record and register lie in that order from OSHWM, the text a gap buffer.
Once main memory is full they run on into free banks of sideways RAM — no ROM
the MOS found at the last BREAK, no ROM image's copyright since, and a write
holds — at most two, which every mode's addresses reach, each marked so that
no BREAK takes it for a ROM; a text with room below HIMEM takes none, and the
Electron gives none. If another program takes a bank the text holds while the
editor runs an OS command, the text is lost with it, and the editor says so.
While it runs the cursor keys give codes, ESCAPE is a key and the locks are
off, all three put back as they were when it leaves. It edits text
whose lines end in CR, as `*BUILD` writes them, or in LF, CR LF or LF CR as
`*SPOOL` writes them, each written back as found, and BASIC II programs,
listed into the text on loading and tokenised by BASIC II's rules on saving,
byte for byte as BASIC II tokenises the same lines typed at its prompt. A
file main memory holds is read and written by one OSFILE call. One past HIMEM
is read and written a part at a time with OSGBPB, as is a program or a text
with paired line ends that outgrows the free memory as it is built, the line
in progress kept back and the file then given its addresses by OSFILE 1; the
DFS spends a revolution of the disc on each sector OSGBPB moves, so this
takes about five times as long. It colours keywords, strings, numbers,
line numbers, comments and star commands as the mode allows — teletext
codes, each taking a cell, in MODE 7, sixteen colours in MODE 2 and four in
MODES 1 and 5 — and `:mode` takes a mode whose screen leaves the text room,
taking on a bank of sideways RAM where main memory alone would not.
Normal mode has vi's motions, counts, operators, undo and repeat; the
command line has `:w`, `:q`, `:e`, `:r`, line ranges, `:d` and `:s`, the
options, OS commands and a paged `:help` giving each mode's size, colours
and the room it leaves in main memory. A line holds at most 255 characters,
and a file with a longer one is refused. It reaches the machine only through
MOS 1.20's documented interface, and is checked beside the original MOS 1.20,
BASIC II and DFS 1.20, observed only as opaque programs, as beside this
tree's.

The Commodore 64's four ROMs are built by the same rules, from the Commodore
64 Programmer's Reference Guide, the 1541 User's Guide and the chips' data
sheets, with genuine ROMs observed only as opaque programs.
`tairix_openmicro::rom::c64` assembles the KERNAL and BASIC V2 as one
program, as the originals are: the KERNAL's reset and cartridge check, memory
test and I/O set-up, the screen editor, the keyboard scan and its tables, the
jiffy clock, the serial bus, the tape, RS-232 and the logical files, its jump
table at `$FF81–$FFF3` and its RAM vectors at `$0314–$0333`; and BASIC's
tokens, every statement and function, its five-byte reals to the same bits as
the original's, its vectors at `$0300–$030B` and its documented workspace,
under the banner `**** OPENMICRO 64 BASIC V2 ****`; and the addresses inside
the originals software is found calling — the screen's set-up and output,
CINT's parts, the IRQ's entry, RESTOR's table, the serial bus's bodies,
RUN's start, NEWSTT, BASIC's prints and its cold start's halves among them —
each doing there what the original's does.
`tairix_openmicro::rom::c64::charset` draws the character ROM's two sets, the
letters and digits this ROM's own design and the graphics the shapes their
function fixes. `tairix_openmicro::rom::c1541` assembles the 1541's DOS: the
command and error channels, the file types, the BAM and directory, the job
queue and the controller that runs it against the head, and the memory,
block and user commands fast loaders use; its power-on message is
`73,OPENMICRO DOS 1541,00,00`.

Their screens and the disks they write match KERNAL 3's, BASIC V2's and 1541
DOS 2.6's for the same steps. Where they differ on purpose: the memory test
reads a byte of each page, so READY comes about two seconds sooner; a full
format takes about 28 seconds where the original's takes 74; the bytes past a
file's last byte in its last block are zero; a program reading the original
ROMs' own code as data runs only with images of those fitted; and the rest
`plans/C64.md` lists with the behaviour pinned against the originals.

## Testing

- The crate's tests cover every device of every model, the 1541 among them,
  the media codecs, snapshots taken and restored mid-run, the builder, the
  MOS, the Electron OS and the KERNAL driven through their entry points by
  test language ROMs, the Plus 1 ROM against the Guide's interface, BASIC II
  and BASIC V2 typed at their prompts, each screen and result as the
  original gave them, and the 1541's DOS on the true drive through the
  KERNAL and BASIC; the front end's model — the library record, settings,
  assistant, folder, keys, menus, media and pacing — is host-tested, and
  `tests/associations.rs` holds the bundle's file associations to the media
  a machine takes.
- `tests/corpus.rs` replays the recorded corpus in `tests/corpus/`: scenarios
  of discs and keys typed at BASIC, each screen and disc as MOS 1.20, BASIC
  II and DFS 1.20 left them. `cargo xtask openmicro --reference <dir>` records
  the fixtures again from the original ROMs a developer holds in `<dir>`
  (`mos.rom`, `basic.rom`, `dfs.rom`), outside the tree, and refuses to
  write any that would carry those ROMs' bytes.
- `tests/c64_corpus.rs` replays the Commodore 64's corpus in
  `tests/corpus/c64/`: BASIC typed at its prompt, the screen editor, and the
  1541's DOS through BASIC on a freshly formatted disk, each screen and disk
  as KERNAL 3, BASIC V2 and 1541 DOS 2.6 left them; `--reference` records it
  from `c64-kernal.rom`, `c64-basic.rom`, `c64-character.rom` and
  `c1541-dos.rom` in `<dir>`, by the same rules.
- `tests/cpu_vectors.rs` replays a committed sample of the SingleStepTests
  65x02 suite; `cargo xtask openmicro --vectors <dir>` runs the whole suite.
- A Commodore 64 run with its SID and drives on host threads is held to the
  same picture, sound and snapshot as one run alone, and two drives brought up
  in irregular pieces to the same drives brought up whole; `tests/loom.rs`
  model-checks the lane's runner against the machine (`cargo xtask loom`).
- `fuzz_media` (every model's discs, disks, tapes, cartridges and programs),
  `fuzz_snapshot` and `fuzz_worker` are registered with `cargo xtask fuzz`,
  and `lib/machines`' `fuzz_library` covers the library's record and state
  blobs.
- `openmicro_qemu_aarch64` launches the emulator on the desktop, starts the
  Model B its first run makes from its card's menu, reads its BASIC prompt off
  the screen, suspends it by closing its window, resumes it from its card, and
  quits it from the icon bar.
