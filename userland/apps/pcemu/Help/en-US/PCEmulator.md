## NAME

PCEmulator — IBM PC and XT-class machine emulator

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Runs PCs of the 8088 to 80186 era in desktop windows: each machine's
processor, display card, disks, sound and ports, at the speed the machine had.

The library window shows each machine as a card of its last screen. New makes
a machine from one of four models — the IBM PC, the IBM PC XT, a turbo XT and
an 80186 workstation — named as asked, with a 30 MB hard disk that boots
TAIRiX DOS on a model with a hard disk controller. Start, or a double-click,
starts the chosen machine in a window of its own, or raises its window;
several machines run at once. Settings changes a machine's hardware while it
is off, Duplicate copies a machine with its disks, and Delete deletes one with
its disks once that is confirmed. The first time PCEmulator starts it makes a
turbo XT. What a machine writes to its disks, and the time its clock is set
to, are kept from one run to the next.

The turbo XT and the 80186 workstation have an 8087 numeric coprocessor, and
Settings fits one to any machine or takes it out. A program that waits for the
coprocessor without first making sure there is one stops on a machine without
it, as it did on the real hardware.

The picture is the monitor's, four units wide for every three high, each of
the display card's dots kept square-edged whatever the size of the window.
Closing a machine's window suspends the machine: started again, it resumes
where it stopped, its diskettes out of their drives and its clock moved on by
the time it was away. A suspended machine's hardware is changed once it has
been started and shut down.

Along the foot of a machine's window, each of its drives has a lamp that
lights while the drive works. Pressing a diskette drive there offers Insert
Disk, which picks a diskette image to put in the drive, and Eject. A diskette
image dropped on the window goes into the drive it is dropped on, or else the
first, and cannot be written to there.

At the end of that bar, the machine's menu pauses the machine, resets it,
sends it Ctrl+Alt+Delete, and sets its processor's speed for the run. Display
turns the picture's scan lines, glow, phosphor afterglow, noise and wobble on
and off, and the machine keeps what is chosen. Take Screenshot saves the
screen as a PNG picture where the Save panel says. Full Screen fills the
display with the window and leaves the bar in place, where the same menu gives
the display back. Shut Down turns the machine off, keeping only its last
screen.

Copy Screen Text puts the text the machine's screen shows on the clipboard,
while it shows text. Paste types the clipboard's text on the machine's
keyboard, each character once the program has read the last. What the machine
cannot type as itself — the PC's faces and arrows, or a letter it has no form
of — is left out, and the window says so. Stop Pasting, or a key pressed in the
machine's window, stops the typing.

Share a Folder lends the machine a folder of yours, chosen in the picker, as
a drive of its own: the first letter its disks leave free, once TAIRiX DOS has
started. A name DOS could not hold is shortened, as Windows shortens a long
name, and a folder shared to read only, or a file you may not change, cannot
be changed from the machine. Each shared folder has a slot on the bar naming
its drive; pressing it offers Stop Sharing. A machine shares up to four
folders, and none once it is suspended or shut down.

Debugger in the machine's menu opens a window on the machine: its registers,
its 8087, its code and memory as they stood when it stopped, and its
breakpoints. Break stops the machine where it is; Continue, Step, Step Over
and Step Out send it on, as F5, F11, F10 and Shift+F11 do, F5 also stopping a
machine that runs. Pressing beside a line of code sets a breakpoint there or
clears it, as F9 does for the current line, and Add Breakpoint stops the
machine instead when it writes or reads memory, reads or writes a port, or
enters an interrupt. The machine's time stands still while it is stopped, and
closing the debugger lets it go on.

While a machine's window holds the keyboard, every key goes to the machine as
a key of its own keyboard, Ctrl, Alt and the lock keys included; the desktop
keeps Ctrl+Alt+Delete, its capture chords and the Meta key. A program that
uses the mouse through the DOS mouse driver follows the desktop pointer
wherever it is over the picture. A program that reads the mouse itself takes
it when the picture is clicked, and gives it back when the Meta key is tapped.

Closing the library window leaves running machines running, and Machine
Library in the icon bar's menu brings it back. Each machine runs in a worker
of its own that holds no authority at all: its software and its disks are
never trusted.

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero once its last window is closed. Non-zero when the window channel, the event
mailbox, or the desktop session was refused; the reason is stated on the
standard error stream.
