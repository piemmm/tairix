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
the picture. The Layers pane, shown from View ▸ Panes, holds the opacity of
the layer painted on and the New, Duplicate, Delete and Merge down buttons
above a row for each layer, the top first, with an eye, a small picture of
the layer, its name and its opacity. Press a row to paint on its layer, its
eye to show or hide the layer, or the name of the layer painted on to rename
it, and drag a row to move its layer up or down; each of these but choosing a
row, and a change of opacity however long it is dragged, is one change to
undo. Floating, the pane's window resizes from two rows to all of them.
Adjustments, fills and strokes change the layer painted on; turns,
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
was and saved back unchanged. A TIFF holds any number of pages, and for one
the Pages menu goes to, adds, copies, deletes and reorders them.

A sprite file opens in a small window of its sprites, each shown as a picture
over its name. Choose sprites by pressing them, with Ctrl to add one and Shift
for a run; the window's menu makes, renames, copies, deletes and reorders
them, and saves the file. Double-click a sprite, or press Enter, to edit it in
a window of its own; any number of sprites can be open at once, from any
number of files. A sprite window's changes are its file's as soon as they are
made, and saving there saves the file. Closing a sprite window asks nothing;
closing the file's window asks whether to save every sprite's changes, and
closes their windows too.

The primary (left) button paints with the primary colour and the middle
button with the secondary colour; holding Alt picks a colour instead, as
the layers show it. The tool box, in the Tools pane, holds the tools in two
columns: select, pencil, brush, airbrush, eraser, clone, fill, gradient,
eyedropper, text, line, rectangle, ellipse, polygon, crop, hand and zoom. The
bar across the top names the tool in use and holds its settings — a brush's
size, hardness, opacity, flow and spacing, a fill's tolerance, a gradient's
shape, the text's size, a rectangle's corners — typed or stepped with the
arrow keys, and the buttons that zoom and show the pixel grid; the palette
strip beneath the picture holds the picture's palette, or the desktop
colours, in one row that scrolls sideways when it is longer than the window.
The airbrush keeps spraying while it is held still. Holding Shift draws a
square, a circle or a line at a multiple of 45 degrees.

Panes run down both sides of the window: by default the Tools pane on the left
and the Colour pane on the right, joined beneath by the Layers, Adjustments and
Effects panes when they are shown or an adjustment or an effect is opened. The
panes on a side stack in one column: each gets at least the room it needs, the
one opened last then grows first towards all it holds, and a pane given less
scrolls within itself, while the column never does. Each is headed by a slim
band naming it, with a control
that rolls it up to its band and a mark that closes it; View ▸ Panes shows a
closed pane again, and Reset panes puts every pane back as a new window has
them. Dragging a band moves its pane within its side or to the other, the
place it will land marked as it goes; let go away from both sides, or dragged
out of the window, the pane floats in a small window of its own, moved by its
band and kept above the picture, its mark closing it; dragged back over a
side, it docks there again. The palette floats too: the button at the right
end of its strip, or View ▸ Panes ▸ Float palette, moves it into a small
window of its own, its colours set out in a grid. Resizing that window sets
the colours out again to fit its width, and they scroll up and down when they
do not all fit; closing it, or dragging it back to the bottom of the window,
puts it back in the strip. In the strip and in its window alike, the primary
and secondary colours are marked with their RISC OS colour numbers.

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

Two panes hold the picture's operations, each headed by a list that chooses
one: the Adjustments pane holds brightness and contrast, levels, curves, hue
and saturation, colour balance, white balance, desaturate, invert, posterise,
threshold and the channel mixer, and the Effects pane holds Gaussian blur,
unsharp mask, median, add noise, find edges, emboss and pixelate. Each row of
the Adjust menu opens its operation in its pane. Beneath the list lie the
chosen operation's settings, then Preview, Reset and Apply; every other tool,
pane and menu stays to hand. The picture shows the operation as its settings
move, the part in view first; Preview turns that off and on to compare, Reset
puts its settings back, and Apply keeps it as one change to undo. One preview
runs across both panes: choosing another operation in a list, or moving the
other pane's settings, applies the operation left as one change where its
settings were moved and it changes the picture, and drops it otherwise, so
looking down a list changes nothing. Painting, filling, a transform or a save
first applies or drops the open operation in the same way; marking a
selection, the view, the colours and undo leave it open.

Levels sets black, grey and white points over a histogram of the layer, for
every channel together or each alone, with pickers that take them from the
picture and Auto; Curves bends a channel's tones through points dragged over
its histogram; white balance sets the light's temperature and tint, from a
neutral pixel picked or by Auto; hue and saturation turns, strengthens and
lightens all colours or one range of them; colour balance moves the shadows,
midtones and highlights towards red, green or blue, keeping their lightness
when asked; the channel mixer weighs each channel from red, green and blue
and a constant, or makes one grey of them. Gaussian blur softens the picture
by its radius; unsharp mask sharpens it, pushing each colour away from its
blur by an amount where the two differ by at least a threshold; median gives
each pixel the middle value of those about it, smoothing speckle and keeping
edges; add noise lays uniform or Gaussian speckle, in colour or grey; find
edges lights the edges between colours; emboss turns the picture into a grey
relief lit from an angle; and pixelate lays squares, each the average colour
of what it covers. Everything is held to the selection. On a palette picture
an adjustment changes its palette, and the effects, which need neighbouring
colours, are not offered.

The Colour pane holds the primary and secondary colours and a colour picker
for whichever of them is chosen: click a colour to choose it, then pick it on
a square of saturation and value beside a strip of hues, on a wheel of hues
round a triangle, or on a slider for each channel, and type it in RGB, HSV,
HSL, CMYK, Lab, LCh or as a grey, or by its hexadecimal spelling and, where the
picture holds transparency, its opacity. A Lab or LCh colour outside what the
screen can show is shown as near as it can be, and marked. Swap exchanges the
two colours, Reset makes them black and white, and Pick takes the next colour
clicked in the picture. The colour it had stands beside it, a click takes it
back, and the colours last chosen wait beneath to be chosen again. On a
picture with a palette the colours are its entries, so the picker edits the
palette, and each edit is one change to undo.

Settings in the icon bar's menu opens Paint's settings window: the tool a new
window starts with and whether a picture opens fitted to the window or at
actual size; the size, format, colours and background New picture offers; the
grid's spacing, offset, colour, opacity and style — lines, dashes, dots or
crossings — whether a new window shows it, snapping to it, and the zoom from
which the grid between pixels shows; the checkerboard's size and shades and
what surrounds the picture; and the panes a new window opens with. A change
applies to every window at once and is kept for the next time; Restore
defaults puts them all back. View ▸ Grid shows a window's grid. While it
shows and snapping is on, shapes, selections and crop boxes cover whole
cells, a line's ends, a gradient's ends and a polygon's corners land on its
crossings, and a selection dragged lands with its corner on one.

The painter holds no filesystem capability. It edits only the file it was
handed. A file the user may change is handed over writable, and Save writes
it back; any other is read-only, and Save asks where to save a copy.
Pictures, and pictures pasted from the clipboard, are decoded in a separate
worker process with no reach at all, and each document gets a fresh one, so
a hostile file cannot touch anything the painter can.

Pressing the secondary (right) mouse button anywhere in the window opens
its menu: Cut, Copy, Paste, Select all and Deselect, then File, Edit, Image,
Layers, Colours, Adjust, Pages for a TIFF, View and Tools, each opening its
own submenu. The window has no menu bar. Closing a window or quitting with
changes not saved asks first. Closing a window with the secondary (right)
button on its close button also shows its file in the file manager, in the
folder that holds it.

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
* `Ctrl+'` — show or hide the grid
* `Page Up` / `Page Down` — the page before or after
* In a sprite file's window: arrow keys, `Home` and `End` — choose a sprite; `Enter` — edit the chosen sprites; `Delete` — delete them
* arrow keys — move a floating selection a pixel; with `Shift`, ten; in the palette strip, step through its colours

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero after Quit. Non-zero when the window channel, the event mailbox, or the
desktop session was refused; the reason is stated on the standard error
stream.
