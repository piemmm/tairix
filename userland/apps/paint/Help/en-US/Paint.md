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
WebP, Windows icons, RISC OS sprite files and OpenRaster. It writes PNG,
JPEG, GIF, BMP, TIFF, sprite files and OpenRaster; a picture read from any
other format, or from a file holding more than its picture, such as a colour
profile, is saved as a new file. New picture asks first which format the
picture is for, and offers the colours that format holds. Save as asks the
format and that format's own settings — a JPEG's quality, whether a GIF is
interlaced, a TIFF's compression — and says what the format cannot keep,
before it asks where. A palette is kept wherever the format holds one, and so
is a picture's density.

A picture may be made of layers, the bottom first, each with a name, an
opacity and whether it shows; painting lands on one layer at a time, and the
window shows them laid together. OpenRaster keeps the layers; every other
format is written them laid together. The Layers menu adds, copies, deletes,
raises and lowers layers, merges one down onto the one beneath and flattens
the picture, and its Layer properties renames a layer and sets how much of
it shows. Adjustments, fills and strokes change the layer painted on; turns,
flips, resizes and crops change every layer. A palette picture holds one
layer.

A sprite file holds any number of sprites, each with its name, screen mode,
palette and mask. Every depth is edited as it is stored: 2, 4, 16 and 256
colours and millions of colours. A sprite with no palette of its own shows
the RISC OS desktop's colours — for 16 colours, colour n is Wimp colour n;
for 2 colours, Wimp colours 0 and 7; for 4 colours, Wimp colours 0, 2, 4 and
7; for 256 colours, the RISC OS tint arrangement — never a PC palette. A
sprite whose pixels are taller than wide, as in mode 12, is shown so. A
sprite this editor cannot read, such as a CMYK one, is kept exactly as it
was and saved back unchanged. The Sprites menu goes to, adds, copies,
renames, deletes and reorders sprites. A TIFF holds any number of pages, and
for one the Pages menu goes to, adds, copies, deletes and reorders them.

The primary (left) button paints with the primary colour and the middle
button with the secondary colour; holding Alt picks a colour instead, as
the layers show it. The tools down the tool box on the left are select,
pencil, brush, airbrush, eraser, clone, fill, gradient, eyedropper, text,
line, rectangle, ellipse, polygon, crop, hand and zoom. The bar across the
top names the tool in use and holds its settings — a brush's size, hardness,
opacity, flow and spacing, a fill's tolerance, a gradient's shape, the
text's size, a rectangle's corners — typed or stepped with the arrow keys,
and the buttons that zoom and show the pixel grid; the palette strip beneath
the picture holds the picture's palette, or the desktop colours. The
airbrush keeps spraying while it is held still. Holding Shift draws a square, a circle or a
line at a multiple of 45 degrees.

The select tool marks out a rectangle, an ellipse, a freehand lasso, a
polygon clicked corner by corner, or with the magic wand the pixels joined
to one through colours like it. Holding Shift adds to the selection, Alt
takes away from it, and both keep only what the two share; Feather softens
its edge. While a selection is held, every tool, fill and adjustment is held
to it. Drag inside it to lift it and move it: it floats until it is put
down, and moving it is one change to undo. Copied pictures travel through
the clipboard as PNG, and what is pasted floats until it is put down.

The clone tool paints what lies elsewhere in the picture: Alt-click where to
copy from, then paint. The gradient tool blends the primary colour into the
secondary along a drag, in bands or in rings. The text tool sets the words
typed where it is clicked; Enter begins a new line, another click or tool
sets them down, and Escape drops them. The polygon tool's corners are
clicked in turn, and a click on the first or Enter closes it. The crop tool
marks the part to keep, its handles move its edges, and Enter crops. The
hand drags the picture across the window, as Space does with any tool; the
zoom tool magnifies a click, or with Alt shrinks it, and a box dragged fills
the window.

The Adjust menu changes brightness and contrast, hue and saturation, levels,
posterises, thresholds, desaturates, blurs, sharpens, pixelates, adds noise
and finds edges, each shown on the picture as its settings move and kept
only when applied. On a palette picture an adjustment changes its palette,
and those that need neighbouring colours are not offered.

The colour dock on the right holds the primary and secondary colours and a
colour picker for whichever of them is chosen: click a colour to choose it,
then set it by hue, saturation and value, by red, green and blue, by its
hexadecimal spelling and, where the picture holds transparency, by its
opacity. The colour it had stands beside it, and a click takes it back. On a
picture with a palette the colours are its entries, so the picker edits the
palette, and each edit is one change to undo.

The painter holds no filesystem capability. It edits only the file it was
handed. A file the user may change is handed over writable, and Save writes
it back; any other is read-only, and Save asks where to save a copy.
Pictures, and pictures pasted from the clipboard, are decoded in a separate
worker process with no reach at all, and each document gets a fresh one, so
a hostile file cannot touch anything the painter can.

Pressing the secondary (right) mouse button anywhere in the window opens
its menu: Cut, Copy, Paste, Select all and Deselect, then File, Edit, Image,
Layers, Colours, Adjust, Sprites or Pages, View and Tools, each opening its
own submenu. The window has no menu bar. Closing a window or quitting with
changes not saved asks first.

* `Ctrl+N` — a new picture; `Ctrl+O` — open a file
* `Ctrl+S` — save; `Ctrl+Shift+S` — save as
* `Ctrl+W` — close the window
* `Ctrl+Z` — undo; `Ctrl+Shift+Z` or `Ctrl+Y` — redo
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cut, copy, paste
* `Ctrl+A` — select everything; `Ctrl+D` — deselect
* `Enter` — put a floating selection down, close a polygon, or crop; `Escape` — turn it back
* `Delete` — clear the selection; `Alt+Backspace` — fill it with the primary colour
* `Ctrl+Shift+X` — crop to the selection
* `Ctrl+R` — resize; `Ctrl+Shift+R` — canvas size
* `Ctrl+[` / `Ctrl+]` — rotate left or right
* `Ctrl+I` — invert the colours
* `Ctrl+Shift+N` — a new layer; `Ctrl+E` — merge down; `Ctrl+Shift+E` — flatten
* `Ctrl+Page Up` / `Ctrl+Page Down` — paint on the layer above or below
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — raise or lower the layer
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — the tools, in order
* `Space` — held, drag the picture with any tool
* `X` — swap the primary and secondary colours
* `Tab` — through the tool's settings, the palette strip and the colour dock; `Shift+Tab` — back; `Escape` — back to the picture
* `+` / `-` — zoom in or out; `1` — actual size; `Ctrl+0` — fit
* `Ctrl` + wheel — zoom in or out about the pointer
* Pinch with two fingers — zoom smoothly; on a touchscreen the picture follows the fingers
* `G` — show or hide the grid between pixels
* `Page Up` / `Page Down` — the sprite or page before or after
* arrow keys — move a floating selection a pixel; with `Shift`, ten; in the palette strip, step through its colours

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero after Quit. Non-zero when the window channel, the event mailbox, or the
desktop session was refused; the reason is stated on the standard error
stream.
