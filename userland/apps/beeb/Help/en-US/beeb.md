## NAME

beeb — BBC Micro Model B emulator

## SYNOPSIS

`beeb`

## DESCRIPTION

Runs a BBC Micro Model B in a desktop window, cycle for cycle at its own
speed, with its own clean-room MOS, BBC BASIC and DFS fitted, so it starts at
the BASIC prompt with a disc filing system. The picture is a television's,
four units wide for every three high, and the sound plays through the
desktop's audio.

A disc image (`.ssd`, `.dsd`) or tape image (`.uef`, `.csw`) opened with the
emulator, dropped on its window, or chosen with Insert Disc or Insert Tape in
its menu goes into the drive or the cassette deck. What the machine writes to
a disc goes back to the disc's file while that file may be written; a disc
whose file may not be written is write-protected. Eject takes a disc out, and
Save Disc As keeps a copy anywhere. A tape recorded on is saved when it is
taken out. Save Snapshot keeps the whole machine in a file, and Restore
Snapshot takes it back, its discs and tape with it.

The menu, opened by a right click on the picture or from the emulator's icon
on the icon bar, also offers BREAK, Reset, Pause and the speed; full screen,
sharp or smooth scaling and scan lines; how the keyboard is mapped; and Paste,
which types the clipboard's text. By character, the keys type what they show,
the machine's CAPS LOCK deciding a letter's case. By position, each key is the
BBC key in its place: F1 to F9 are f1 to f9, F10 is f0, F12 is BREAK, End is
COPY and Home is SHIFT LOCK.

Settings chooses what the OS socket and the sixteen sideways slots hold — the
shipped ROMs, sideways RAM, or ROM images of the user's own, installed in the
emulator's store — the keyboard links' screen mode, boot and disc timings,
the picture, the keys, the volume, and fast tape loading, which runs the
machine as fast as it can while the cassette motor runs. Changes to the ROMs
and the links take effect at the next Reset.

Lamps along the foot of the window show CAPS LOCK, SHIFT LOCK, the cassette
motor and each drive. The machine runs in a worker that holds no authority at
all, so neither its software nor its media is trusted; a worker that fails is
replaced, the machine starting again from power-on, and the reason is shown.

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero once its window is closed or Quit is chosen in its icon-bar menu.
Non-zero when the window channel, the event mailbox, or the desktop session
was refused; the reason is stated on the standard error stream.
