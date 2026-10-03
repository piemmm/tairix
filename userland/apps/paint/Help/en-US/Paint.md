## NAME

Paint — graphical picture and sprite editor

## SYNOPSIS

`Paint`

## DESCRIPTION

Paints and edits pictures in a desktop window, pixel by pixel or with
brushes and shapes. Launched with a document — from the file manager, from
the desktop, or by dropping a file on its icon in the icon bar — it opens a
window on it. Launched on its own it opens a new, white picture. Each
document is a window of the one painter; closing the last leaves it on the
icon bar, and the Quit row of its icon menu ends it.

It opens every picture format the system reads: PNG, JPEG, GIF, BMP, TIFF,
WebP, Windows icons, and RISC OS sprite files. It writes PNG, JPEG and
sprite files; a picture read from any other format is saved as a new file.
A PNG keeps its palette and a JPEG is written at the quality set with JPEG
quality in the File menu.

A sprite file holds any number of sprites, each with its name, screen mode,
palette and mask. Every depth is edited as it is stored: 2, 4, 16 and 256
colours and millions of colours. A sprite with no palette of its own shows
the RISC OS desktop's colours — for 16 colours, colour n is Wimp colour n;
for 2 colours, Wimp colours 0 and 7; for 4 colours, Wimp colours 0, 2, 4 and
7; for 256 colours, the RISC OS tint arrangement — never a PC palette. A
sprite whose pixels are taller than wide, as in mode 12, is shown so. A
sprite this editor cannot read, such as a CMYK one, is kept exactly as it
was and saved back unchanged. The Sprites menu goes to, adds, copies,
renames, deletes and reorders sprites.

The primary (left) button paints with the primary colour and the middle
button with the secondary colour; holding Alt picks a colour instead. The
tools are select, pencil, brush, spray, eraser, fill, eyedropper, line,
rectangle and ellipse; the panel beside the picture holds the picture's
palette or the desktop colours and the settings of the tool in use. The
colour dock on the right holds the primary and secondary colours and a
colour picker for whichever of them is chosen: click a colour to choose
it, then set it by hue, saturation and value, by red, green and blue, by
its hexadecimal spelling and, where the picture holds transparency, by its
opacity. The colour it had stands beside it, and a click takes it back. On
a picture with a palette the colours are its entries, so the picker edits
the palette, and each edit is one change to undo. Holding Shift draws a square, a circle or a line
at a multiple of 45 degrees.

With the select tool, drag to mark out part of the picture, then drag the
selection to move it; it floats until it is put down, and moving it is one
change to undo. Copied pictures travel through the clipboard as PNG, and
what is pasted floats until it is put down.

The painter holds no filesystem capability. It edits only the file it was
handed. A file the user may change is handed over writable, and Save writes
it back; any other is read-only, and Save asks where to save a copy.
Pictures, and pictures pasted from the clipboard, are decoded in a separate
worker process with no reach at all, and each document gets a fresh one, so
a hostile file cannot touch anything the painter can.

Pressing the secondary (right) mouse button anywhere in the window opens
its menu: Cut, Copy, Paste, Select all and Deselect, then File, Edit, Image,
Colours, Sprites, View and Tools, each opening its own submenu. The window
has no menu bar. Closing a window or quitting with changes not saved asks
first.

* `Ctrl+N` — a new picture; `Ctrl+O` — open a file
* `Ctrl+S` — save; `Ctrl+Shift+S` — save as
* `Ctrl+W` — close the window
* `Ctrl+Z` — undo; `Ctrl+Shift+Z` or `Ctrl+Y` — redo
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cut, copy, paste
* `Ctrl+A` — select everything; `Ctrl+D` — deselect
* `Enter` — put a floating selection down; `Escape` — put it back
* `Delete` — clear the selection
* `Ctrl+Shift+X` — crop to the selection
* `Ctrl+R` — resize; `Ctrl+Shift+R` — canvas size
* `Ctrl+[` / `Ctrl+]` — rotate left or right
* `Ctrl+I` — invert the colours
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — the tools, in order
* `X` — swap the primary and secondary colours
* `Tab` — into the colour dock and through its parts; `Escape` — back to the picture
* `+` / `-` — zoom in or out; `1` — actual size; `Ctrl+0` — fit
* `Ctrl` + wheel — zoom in or out about the pointer
* Pinch with two fingers — zoom smoothly; on a touchscreen the picture follows the fingers
* `G` — show or hide the grid between pixels
* `Page Up` / `Page Down` — the sprite before or after
* arrow keys — move a floating selection a pixel; with `Shift`, ten

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero after Quit. Non-zero when the window channel, the event mailbox, or the
desktop session was refused; the reason is stated on the standard error
stream.
