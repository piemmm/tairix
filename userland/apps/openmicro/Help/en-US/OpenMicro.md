## NAME

OpenMicro — Open Micro Emulator, 8-bit microcomputer emulator

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Runs 8-bit microcomputers in desktop windows — the BBC Micro Model B, the
Acorn Electron and the Commodore 64 — cycle for cycle at their own speed, each
with its own clean-room operating system and BASIC fitted, on the Model B a
disc filing system and on the Commodore 64 a 1541 disk drive with its own DOS,
so a machine starts at the BASIC prompt. The picture is a television's, four
units wide for every three high, and the sound plays through the desktop's
audio.

The library window shows each machine as a card of its last screen, or of its
model's start-up screen until it has run. New makes a machine, named as asked:
a BBC Micro Model B, with sideways RAM in slots 4 to 7 and a new blank disc in
drive 0 or none; an Acorn
Electron, with a Plus 1 fitted or not; or a Commodore 64, PAL or NTSC, with a
6581 or 8580 SID and one 1541 drive, two or none. Start, a double-click or
Return starts the chosen machine in a window of its own, or raises its window;
up to twenty machines are kept, and several run at once. Duplicate copies a
machine with its saved state and its folder, and Delete deletes a machine and
its saved state once that is confirmed, leaving its folder. Right-clicking a
machine's card offers Start, Settings, Duplicate, Delete and Show Folder for
that machine, and while it runs, its machine's menu as well. The first time
the emulator starts it makes a BBC Micro Model B with a blank disc.

Settings changes a machine's name, its volume, fast loading, which runs the
machine as fast as it can while the cassette motor runs or a drive's disc turns
and the machine is silent, and on a Commodore 64 the control port its keypad
joystick is in. What is fitted to it changes only while it is shut down: the
ROM in each socket — the Model B's OS and its sixteen sideways slots, the
Electron's OS and BASIC, its Plus 1's ROM and cartridges, and the Commodore
64's KERNAL, BASIC, characters and each drive's DOS — each a shipped ROM,
sideways RAM in a Model B slot, nothing in a slot or cartridge, or a ROM image
of your own, put in the emulator's store with Install an image; the Electron's
Plus 1; the Commodore 64's standard, SID and drives; and the Model B's keyboard
links, which set its screen mode at start-up, how a disc boots, and the disc
timings. A few programs read the original ROMs' own code, and run only with
images of those fitted.

A Model B also has VIM, a vi-style editor, fitted in slot 13. At its prompt,
`*VIM` and a file's name edits that file, and BREAK in VIM keeps the text it
holds. VIM edits text, such as the `!BOOT` files `*BUILD` makes, and
BASIC programs, which it lists as text and saves back tokenised, colouring
each as the screen mode's colours allow. In VIM, `:help` lists its commands
and the screen modes with the room each leaves in main memory, `:mode` and a
number changes the mode, and `:q` goes back to BASIC. Once main memory is
full, VIM goes on into banks of sideways RAM that hold no ROM, up to two, so
it can edit a file larger than main memory; such a file takes longer to read
and write.

Closing a machine's window suspends the machine: started again, it resumes
where it stopped. Shut Down in its menu takes its media out and turns it off,
keeping only its last screen.

Each machine has a folder of its own, Open Micro, then the machine's name, in
your Documents folder. New Disc, New Disk on a Commodore 64, and New Tape make
a blank disc, disk or tape there and put it in, From Folder offers the discs,
disks and tapes already there, and Save Snapshot keeps the whole machine there
as a `.snapshot` file. Show Folder opens the folder in Files, so what the
machine keeps there can be copied to other media or another computer. What its
drives and its deck hold from its folder stays with the machine: shut down, it
takes them in again when it starts; resumed, it writes back to them only where
no other program has changed them since it was suspended.

A disc image (`.ssd`, `.dsd`) or tape image (`.uef`, `.csw`), or for a
Commodore 64 a disk (`.d64`, `.g64`), tape (`.tap`), cartridge (`.crt`) or
program (`.prg`, `.t64`), opened with the emulator goes to the running machine
whose window was last active, or another running machine that takes it, or
else the first machine in the library that takes it, started for it, and that
machine's window comes forward. Opening a BBC Micro disc in Files shows the
files on it instead of starting a machine. One chosen with Insert Disc, Insert
Disk or Insert Tape goes into that drive or the cassette deck, and one dropped
on a machine's window goes into its first drive or the deck, write-protected,
the window taking the keyboard. What the machine writes to a disc or disk goes
back to its file while that file may be written, and while it is in the drive
neither a mount nor another machine may write it; one whose file may not be
written, or that another program is using, is write-protected, and the window
says why. A `.d64` disk that has come to need a `.g64` is offered to be saved
as one. Eject takes a disc or disk out, and Save Disc As or Save Disk As keeps
a copy anywhere. A tape recorded on is saved when it is taken out. On a
Commodore 64, Play, Record, Stop and Rewind in the Tape menu work its
datasette; Insert Cartridge plugs a cartridge in and Remove Cartridge takes it
out, each restarting the machine; and Load Program puts a program in memory,
as LOAD would, for RUN to start. Save Snapshot keeps the whole machine in a
file — in its folder, or where you choose before it has one — and Restore
Snapshot takes one back, its media with it.

The menu, opened by a right click on the picture, offers each drive of a Model
B or a Commodore 64 and the tape, and a Commodore 64's cartridge and Load
Program; BREAK, or RESTORE on a Commodore 64; on a Model B and an Electron,
SHIFT-BREAK, which presses BREAK with SHIFT held to boot the disc or tape;
Reset, Pause and the speed; full screen, sharp or smooth scaling and scan
lines; how the keyboard is mapped; Paste, which types the clipboard's text,
each letter in the case the machine shows; the snapshots; Show Folder;
Settings; Machine Library; and Shut Down. By character, the keys type what
they show, the machine's CAPS LOCK, or on a Commodore 64 Shift, deciding a
letter's case. By position, each key is the machine's key in its place. On a
Model B, F1 to F9 are f1 to f9, F10 is f0, Home is SHIFT LOCK and Insert is
the backslash key. On an Electron, F1 to F10 are FUNC with 1 to 9 and 0, Alt
is FUNC, and Caps Lock is CAPS LK. On both, F12 is BREAK, End is COPY and the
arrows are the cursor keys. On a Commodore 64, F1, F3, F5 and F7 are its
function keys and F2, F4, F6 and F8 the same with SHIFT, Escape is RUN/STOP,
Alt is the Commodore key, Ctrl is CTRL, Home is CLR/HOME, F12 or Page Up is
RESTORE, and the arrows are the cursor keys; with its keypad joystick on, the
numeric keypad's arrows move the joystick and 0 and 5 fire.

Lamps along the foot of a machine's window show CAPS LOCK and the cassette
motor, and on a Model B, SHIFT LOCK and each drive; on a Commodore 64 they
show the datasette's motor and each drive. On a Model B and an Electron, the
SHIFT-BREAK button at the foot's right does what the menu's row does. Each
machine runs in a worker of its own that holds no authority at all, so neither
its software nor its media is trusted; a worker that fails is replaced, the
machine starting again from power-on, and the reason is shown.

Closing the library window leaves running machines running, and closing every
window leaves the emulator on the icon bar: a click on its icon, or Machine
Library in its menu, always brings the library forward, and Quit in that menu
suspends every running machine and ends the emulator.

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero once Quit is chosen in its icon-bar menu. Non-zero when the window
channel, the event mailbox, or the desktop session was refused; the reason is
stated on the standard error stream.
