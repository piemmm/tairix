## NAME

TAIRiXBridge — TAIRiX Bridge, RISC OS programs on the TAIRiX desktop

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Runs RISC OS programs — BASIC programs, command-line tools and
single-tasking games — on the TAIRiX desktop, each showing its screen in a
window of its own. The RISC OS they run on is TAIRiX Bridge's own, written
from published documentation; it holds no RISC OS code.

Programs run in sessions. The shared session is the RISC OS machine the
TAIRiX Bridge window shows: its own disc is the RISC OS folder in your
Documents, its boot sequence runs as it starts, and a program from your own
storage runs in it. A program from anywhere else — removable media, a disc
image, a network share — or from nowhere known runs in an isolated session
of its own, which never sees your RISC OS folder and ends with its last
program. Run in Shared Session runs a file in the shared session wherever it
came from.

A session's window lists its tasks, each with its memory and whether it is
paused or busy, and what the session's shared areas hold. Run picks a file to run; a
file dropped on the window, or opened with TAIRiX Bridge, runs too. Choosing
a task and then Pause or Quit holds or ends it.

Each program's screen is a window of its own, its picture scaled to the
window in its mode's own proportions. The Screen button opens the window's
menu: Full Screen, Sharp or Smooth scaling, Pause, and Reset, which ends the
program and runs it again in the same window. The screen takes the whole
keyboard, each key as RISC OS numbers it. Select, Menu and Adjust are the
primary, secondary and middle buttons. Once a program moves the mouse
itself, a click captures the mouse for it, and a lone tap of the Meta key
releases it.

Settings chooses the order of the buttons, the memory each session may
take, whether every program is isolated, and how screens are scaled. A
session's worker holds no authority: the files it reads and writes are
carried for it, and a worker that fails ends its session, the reason shown in
the session's window.

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero once the last session's window is closed. Non-zero when the window
channel, the event mailbox, or the desktop session was refused; the reason is
stated on the standard error stream.
