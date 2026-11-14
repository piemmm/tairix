## NAME

OpenMicro — Open Micro Emulator, 8-bit microcomputer emulator

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Runs 8-bit microcomputers in desktop windows — the BBC Micro Model B and the
Acorn Electron — cycle for cycle at their own speed, each with its own
clean-room operating system and BASIC fitted, and on the Model B a disc filing
system, so a machine starts at the BASIC prompt. The picture is a
television's, four units wide for every three high, and the sound plays
through the desktop's audio.

The library window shows each machine as a card of its last screen. New makes
a machine, named as asked: a BBC Micro Model B, with a new blank disc in drive
0 or none, or an Acorn Electron, with a Plus 1 fitted or not. Start, a
double-click or Return starts the chosen machine in a window of its own, or
raises its window; up to twenty machines are kept, and several run at once.
Duplicate copies a machine with its saved state and its folder, and Delete
deletes a machine and its saved state once that is confirmed, leaving its
folder. Right-clicking a machine's card offers Start, Settings, Duplicate and
Delete for that machine, and while it runs, its machine's menu as well. The
first time the emulator starts it makes a BBC Micro Model B with a blank
disc.

Settings changes a machine's name, its volume and fast tape loading, which
runs the machine as fast as it can while the cassette motor runs. What is
fitted to it changes only while it is shut down: the ROM in each socket — the
Model B's OS and its sixteen sideways slots, the Electron's OS and BASIC, and
its Plus 1's ROM and cartridges — each a shipped ROM, sideways RAM in a Model
B slot, nothing in a slot or cartridge, or a ROM image of your own, put in the
emulator's store with Install an image; the Electron's Plus 1; and the Model
B's keyboard links, which set its screen mode at start-up, how a disc boots,
and the disc timings.

Closing a machine's window suspends the machine: started again, it resumes
where it stopped. Shut Down in its menu takes its discs and tape out and turns
it off, keeping only its last screen.

Each machine has a folder of its own, Open Micro, then the machine's name, in
your Documents folder. New Disc and New Tape make a blank disc or tape there
and put it in, and From Folder offers the discs and tapes already there. What
its drives and its deck hold from its folder stays with the machine: shut
down, it takes them in again when it starts; resumed, it writes back to them
only where no other program has changed them since it was suspended.

A disc image (`.ssd`, `.dsd`) or tape image (`.uef`, `.csw`) opened with the
emulator goes to the running machine whose window was last active, or another
running machine that takes it, or else the first machine in the library that
takes it, started for it. One chosen with Insert Disc or Insert Tape goes into
that drive or the cassette deck, and one dropped on a machine's window goes
into drive 0 or the deck, write-protected. What the machine writes to a disc
goes back to the disc's file while that file may be written; a disc whose
file may not be written is write-protected. Eject takes a disc out, and Save
Disc As keeps a copy anywhere. A tape recorded on is saved when it is taken
out. Save Snapshot keeps the whole machine in a file, and Restore Snapshot
takes it back, its discs and tape with it.

The menu, opened by a right click on the picture, offers each drive on a
Model B and the tape; BREAK, Reset, Pause and the speed; full screen, sharp or
smooth scaling and scan lines; how the keyboard is mapped; Paste, which types
the clipboard's text; the snapshots; Settings; Machine Library; and Shut Down.
By character, the keys type what they show, the machine's CAPS LOCK deciding a
letter's case. By position, each key is the machine's key in its place. On a
Model B, F1 to F9 are f1 to f9, F10 is f0, Home is SHIFT LOCK and Insert is
the backslash key. On an Electron, F1 to F10 are FUNC with 1 to 9 and 0, Alt
is FUNC, and Caps Lock is CAPS LK. On both, F12 is BREAK, End is COPY and the
arrows are the cursor keys.

Lamps along the foot of a machine's window show CAPS LOCK and the cassette
motor, and on a Model B, SHIFT LOCK and each drive. Each machine runs in a
worker of its own that holds no authority at all, so neither its software nor
its media is trusted; a worker that fails is replaced, the machine starting
again from power-on, and the reason is shown.

Closing the library window leaves running machines running, and closing every
window leaves the emulator on the icon bar: a click on its icon, or Machine
Library in its menu, brings the library back, and Quit in that menu suspends
every running machine and ends the emulator.

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero once Quit is chosen in its icon-bar menu. Non-zero when the window
channel, the event mailbox, or the desktop session was refused; the reason is
stated on the standard error stream.
