## NAME

music — the desktop music player

## SYNOPSIS

`music`

## DESCRIPTION

Plays a list of tracks in one window. Open files or a whole folder through the
desktop's file chooser, or open a track from the file manager: it joins the
list of the player already running. Tracks of one rate, sample format and
channel layout play into each other without a gap.

The player holds no filesystem capability. The desktop session browses on its
behalf and delegates, one-shot and read-only, exactly the files the user
chooses — for a folder, the files in it this player opens. No file is decoded
inside the player: its sound and its album art are each decoded by a separate
worker process that holds no reach at all, so a malformed or hostile file
cannot reach anything the player can.

The top of the window shows what is playing: its album art, title, artist and
album, its format, the place in it, and a level meter for each channel. Under
that are the transport, the shuffle and repeat controls and the volume, and
under those the list. Drag the place slider to seek, and the volume slider to
set the level; both act where you let go. Double-click a track to play it. A
secondary press on the list opens the player's menu, which also chooses the
output and whether tracks are levelled by the loudness their own tags state.

A file the player cannot read is left out with its reason on the status line.

* `Space` — play or pause
* `Enter` — play the track selected
* `Left` / `Right` — ten seconds back or on
* `Ctrl` + `Left` / `Right` — the track before or the next
* `Up` / `Down` — select the track above or below
* `Alt` + `Up` / `Down` — move the track selected
* `Delete` — take the track selected off the list
* `+` / `-` — three decibels louder or quieter
* `S` — shuffle or play in order
* `R` — repeat nothing, the list, or the track
* `Ctrl` + `O` — open files
* `Ctrl` + `Shift` + `O` — open a folder

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero after the window is closed or Quit is chosen. Non-zero when the window
channel, the shared frame region, or the desktop session was refused; the
reason is stated on the standard error stream.
