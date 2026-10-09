# Paint

`Paint.app` (`userland/apps/paint`, `plans/PAINT.md`) is the desktop image
editor. It is one resident instance with a window per document; closing the
last window leaves it on the icon bar, whose menu offers a new window and
Quit.

## What a window shows

A picture is edited at the depth it is stored in: a 1, 2, 4 or 8-bit palette,
or 32-bit colour with alpha. The tool-controls bar runs across the top — the
tool in use and its settings, wrapping onto further rows when they do not fit,
then the view's own commands (zoom out and in, fit, actual size, and the pixel
grid, marked while it shows). A dock of panes runs down each side — by default
the Tools pane on the left and the Colour pane on the right, with the
Adjustment pane beneath it once an adjustment is opened — the canvas and its
bars lie between them with the palette strip beneath, and the status band runs
along the bottom.

Each pane is headed by a mini title band naming it, with a roll-up control
that folds it to its band and a close mark that hides it; View ▸ Panes shows a
hidden pane again and *Reset panes* puts every pane back where the settings
say a new window starts.
Dragging a band moves its pane within its dock or into the other: while the
drag is over a dock, or near the window's edge where a dock is empty, the gap
it would land in is marked, and Escape turns the drag down. A dock is as wide
as its widest pane and gives each pane its height in turn, the open adjustment
first; a pane given no room shows its band alone.

Let go away from a dock, or carried to the window's edge, a pane floats in a
tool window of its own: the window manager's mini-titled palette, above the
picture window, off the icon bar, hidden while the window is minimised and
closed with it, taking the keyboard only when pressed. Carried out, it opens
under the pointer held where the press held its band, and the press goes on
moving it; let go in the window, it opens where it was dropped, its band where
the pane's would have been. Its close
mark hides the pane; moved back over a dock, the landing gap is marked and let
go there it docks again. A floating pane is laid out at its own size in a
rectangle of the window's drawing beside the window (`Layout::floating`), so
it is drawn and hit-tested by the same code as a docked one, and its lists
open within its own window. A pane floating in the arrangement the settings
name opens under the top band at the edge of its home side. The window shows the
picture at any zoom from 1/16 to 64, kept in 4096ths of actual size
(`viewport::Zoom`), every rung of the ladder (`viewport::ZOOMS`) exactly. Ctrl
and the wheel step it a rung for each detent's worth of turn, a fine wheel's
fractions adding up, anchored so the pixel under the pointer stays under it,
and from between two rungs the first step lands on the nearer one in its
direction. A pinch begun over the canvas zooms smoothly by the fingers' spread
and follows their centre, both measured from where it began, so it accumulates
no rounding, and a cancelled pinch puts the view back. The picture is drawn as
tall or wide as the sprite's pixels are, over a checkerboard where it is clear,
with a grid between pixels from the zoom the settings name (*View ▸ Pixel
grid*, G). *View ▸ Grid* (Ctrl+') lays the settings' grid over the picture: a
line at every `offset + k × spacing` pixel boundary, drawn where the pixel after
it starts so it crosses the same pixels at every zoom, as lines, dashes, dots
or crossings in its colour and opacity, and left out where a cell would be
under four screen pixels. The palette
strip holds the picture's palette — the desktop's sixteen colours and the clear
ink for a truecolour picture — in as few rows as hold every well at least 14
pixels across, each up to 24; the status band says where the pointer is and the
colour the layers show there, what was last said, the picture's size, depth
and, of several layers, which is painted on, which sprite shows, and the zoom.
The window is never narrower than the view's commands beside the widest
setting, nor too small for the largest palette round a canvas.

## Tools and their settings

The tool box — a toolbar turned on its side ([controls](../lib/controls.md)),
its tools two to a line, that marks the one in use on its leading edge and
scrolls a line at a time when its pane is too short — holds select, pencil, brush, airbrush, eraser, clone, fill,
gradient, eyedropper, text, line, rectangle, ellipse, polygon, crop, hand and
zoom (`tool::Tool`). The bar offers the tool's settings (`tool::Setting`) as
number fields, choices and switches; a number takes effect as it is typed while
it spells one in range, Up and Down step it, and the wheel steps the field with
the keyboard. A setting is the window's own, so nothing is written as it
changes, and a tip over each says what it does. Smoothing is held off on a
palette picture, whose pixels are one entry each.

- **Painting** — the brush, airbrush, eraser and clone lay round dabs along the
  pointer's path (`brush::Tip`): size, hardness (the falloff from a solid core),
  opacity (the most a stroke builds to), flow (what each dab adds) and spacing
  (between dabs, carried across the path's segments). A stroke builds from the
  picture as it stood, so it never darkens where it crosses itself. The airbrush
  goes on laying dabs while held still (`AIRBRUSH_INTERVAL_NS`). The clone tool
  paints what lies at a fixed offset, set by an Alt-click, from the picture as
  the stroke began. The pencil sets whole pixels.
- **Fills and gradients** — a fill takes the region joined to the pixel pressed
  through colours within its tolerance, or with *contiguous* off every such
  pixel; a gradient blends the primary ink into the secondary along a drag, in
  bands or rings, mixed with each one's alpha weighed in, and dithered between
  the two entries on a palette picture. Both run on the worker.
- **Shapes and text** — lines, rectangles with rounded corners, ellipses and
  polygons, outlined, filled or both, previewed as dragged and drawn exactly as
  previewed. Text is set in the desktop's face at the size chosen and laid down
  as one change.
- **The view's tools** — crop marks the part to keep, its eight handles moving
  its edges, and Enter crops; the hand drags the view, as Space does with any
  tool; zoom magnifies a click, shrinks an Alt-click, and fills the window with
  a box dragged.

The primary button paints the primary colour and the middle button the
secondary; Alt picks a colour instead, the colour the layers show there. A drag
belongs to the button that began it, and is finished by anything that takes the
pointer or changes the picture under it — a menu, a question, another sprite, a
transform — while Escape turns it down: a stroke is taken back off the picture,
a selection being marked marks nothing, and a dragged selection goes back where
the drag began.

Undo holds every step while memory is plentiful and gives up the oldest as
pressure rises — twice the document's own size under mild pressure, once under
moderate, nothing under severe; memory refused costs the history, redo first,
before it costs a change.

## Selections

A selection is a soft mask (`mask::Mask`): a rectangle, an ellipse, a freehand
lasso, a polygon clicked corner by corner, or the magic wand's pixels joined to
one through colours within a tolerance. A new part replaces the selection, is
added to it, taken from it, or met with it (`mask::Combine`), chosen on the bar
or held with Shift, Alt or both; feathering softens its edge. The mask is made
on the worker. While a selection is held every stroke, fill, gradient, text and
adjustment is held to it, as much of each pixel as it chooses. Dragging inside
it lifts it into a floating selection that writes nothing until it is put down —
by a press off it, Escape, or anything that changes the picture — and its move
is one change to undo.

## Layers

A picture is one layer or more, the bottom first (`document::Layer`), each with
a name, an opacity and whether it shows; painting lands on one at a time, and
every paint lays them together source-over (`compose`), the layer painted on
carrying what is being drawn on it, so what shows is what a save writes. Each
layer is the picture's size, its untouched tiles one shared clear tile, so a
small layer on a large picture costs what it covers. The Layers menu adds a
clear layer, duplicates, deletes, raises and lowers one, steps to the layer
above or below, goes to one by name or number, merges one down onto the one
beneath, flattens the picture, shows or hides a layer, and opens its
properties (name, opacity, shown). A merge lays the two together onto nothing
and keeps the lower one's name, which keeps the look to within the rounding of
a level, since source-over is associative; both layers must show, since the
pixels of one hidden or wholly faint would have nowhere to go. A merge or
flatten runs on the worker. Each change is one step to undo, and undoing a
stroke made on another layer paints on that layer again.
Adjustments, fills and strokes change the layer painted on; turns, flips,
resizes and crops change every layer, a canvas grown being filled beneath them
all and left clear over the bottom. A palette picture holds one layer shown
wholly (`document::MOST_LAYERS` bounds a colour one), so a picture of layers,
or of one layer faded, is flattened before it is made a palette picture, and
one hidden layer is shown first, since flattened it would come out clear.

## Adjustments and filters

The Adjust menu (`filter::Filter`) holds brightness and contrast, hue and
saturation, colour balance, levels, curves, white balance, posterise,
threshold and desaturate, which map each colour alone, and blur, sharpen,
pixelate, noise and find edges, which make each pixel from its neighbours;
Invert colours is the same kind of adjustment. One with nothing to set —
invert, desaturate, find edges — applies at once; one with settings opens in
the Adjustment pane, and nothing else is taken from the window while it is
open: the tools, the panes and the menus stay live. With nothing open the pane
offers the list to open one from.

The picture shows the settings as they move: a worker works the preview out one
job at a time, the latest settings asked again once it lands, and an answer
worked from a picture, layer or selection that has moved since is dropped and
asked again. *Preview* turns it off and on to compare; Reset, or Escape in the
pane, puts the settings back; closing the pane drops the adjustment; Apply lands
it as one step, adopting the preview's own tiles where they show exactly what is
applied. Marking a selection or a crop box, undo and redo leave it open and
show it over what the picture became; anything else that changes or reads the
picture — a stroke, a fill, a transform, a save, a copy, another adjustment —
applies it first, and a press that finds its preview still being worked out
paints nothing. On a palette picture an adjustment maps the palette alone, and
the neighbourly filters are not offered.

- **Levels** sets the composite and each channel: input black, grey and white
  handles beneath the channel's histogram — the grey point placed where the
  levels between reach half way, so its gamma is `ln p / ln ½` — and output
  black and white, each value typed in its field, the gamma to two places. The
  black, grey and white eyedroppers set every colour channel from a pixel of
  the picture beneath the preview, and Auto stretches each channel over what
  it holds, setting aside 0.1% at each end.
- **Curves** sets a tone curve for the composite and each channel through up to
  16 points, a monotone cubic that never overshoots between them, drawn over
  the channel's histogram. A press adds a point or takes the one within reach,
  a point carried out of the graph is taken away, the arrows nudge the chosen
  point and Delete takes it away, Page Up and Page Down choose another, and its
  input and output are typed in their fields.
- **White balance** takes the light the picture was lit by to be a temperature
  from 2000 to 12000 K and a tint, each on a track showing the cast a grey
  takes, and corrects it to daylight. The neutral eyedropper reads both from a
  pixel that should be grey; Auto takes the picture's mean as that pixel.
- **Hue and saturation** sets the master and each of six colour ranges, a range
  fading to nothing at the next range's centre and a grey keeping its
  lightness, over the input and output hue spectra. **Colour balance** moves the
  shadows, midtones and highlights toward red, green or blue, keeping each
  colour's lightness when asked. **Threshold** sets its level on a handle
  beneath the histogram.

The histograms count the layer painted on, held to the selection, each pixel
weighed by its opacity and its share of the selection
(`histogram::Histogram`), worked out on the worker while the pane reads one.

The window has no menu bar. A secondary press anywhere opens its menu — Cut,
Copy, Paste, Select all and Deselect, then File, Edit, Image, Layers, Colours,
Adjust, Sprites, View and Tools, each a submenu — drawn by the desktop like
every application's ([menus](menus.md)).

## The Colour pane

The Colour pane holds the primary and secondary colours, the secondary behind,
with Swap, Reset — black and white, or on a palette picture the entries
nearest them — and Pick, a one-shot pick that takes the next press on the
picture into the ink the pane edits while the tool in use carries on (Escape
puts it down); the shared colour picker ([controls](../lib/controls.md))
editing whichever of the two was last chosen, its caption naming it and on a
palette picture which entry it is; and the last sixteen colours settled or
picked, which a press takes back. The View choice picks on a square, a wheel
or sliders, and the Fields choice shows the picker's fields in RGB, HSV, HSL,
CMYK, Lab, LCh or grey, each model's values held as typed, a Lab or LCh value
outside sRGB shown at its nearest and marked. A palette swatch, the eyedropper, the swap key and the Colours menu all
set an ink from outside the picker, which then shows the ink's colour with the
same colour beside it as the earlier one a press takes back. The picker carries
opacity on a picture that holds it — truecolour, or a palette picture that is
not a sprite — and is opaque on a sprite, whose transparency is its mask.

On a truecolour picture an ink is a colour, and the picker changes it as it
moves; nothing in the picture changes, so nothing is recorded. A colour with
no alpha at all is the clear ink, since laid over the picture it would change
nothing. A conversion that lands while the picker is dragged ends the drag: the
ink it was choosing has become an entry. On a palette picture an ink is an
entry, so the picker edits the palette: the entry takes each colour live, and
the palette it had is kept aside until the interaction settles, when the change
is recorded as one step — a drag is one step however long it runs, a drag
abandoned with Escape is none, and one that ends where it began records
nothing. The room for that step is reserved before the first colour lands. A
palette picture's picker is withheld while a worker has the picture, and so is
the picker for the mask's clear ink, which is the mask and no colour.

Tab walks the keyboard from the picture through the bar's settings, the palette
strip, the Colour pane's buttons and choices, its picker and its recent
colours, and the Adjustment pane's settings, and back to the picture; Shift+Tab walks the other
way, and a part with nothing that can act is passed. In the palette the arrows
walk the wells. Escape gives the keyboard back to the picture once a field has
nothing to take back. While a field has the keyboard it takes every key it can
use, so a letter typed there is never a tool's shortcut; a chord it has no use
for — Ctrl+S, Ctrl+Z — is the window's, and what the field holds is settled
first.

## Formats

Paint opens every raster format TAIRiX decodes ([image](../lib/image.md)) —
SVG has no pixels of its own to edit, and is refused — and writes PNG, JPEG,
GIF, BMP, TIFF, RISC OS sprite areas and OpenRaster. Each file is read as it
stores its picture: a palette picture as its indices and palette, a TIFF as its
pages, a sprite area as its sprites, an OpenRaster file as its layers, each
placed where its stack puts it on the canvas, the topmost painted on, and every
picture with the density its file states. A sprite area holds any number of
sprites, each with its name, mode, palette and mask, and the Sprites menu adds,
duplicates, renames, deletes and reorders them; a new sprite is offered the
name `sprite`, numbered with the least number free when taken. A TIFF holds any
number of pages, and for one the Pages menu does the same; a page has no name.
A document of several pictures read from any other format is a sprite area.

**New picture** asks first which format the picture is for, and offers the
colours that format holds (`SaveFormat::admits`) — a JPEG and OpenRaster have
no palette, a GIF nothing else — and a clear background only where the format
can show one. **Save As** first works out, on the worker, what each format the
document can be written as would not keep (`save::survey`), since finding a
loss reads every pixel, and then opens a sheet of those formats, holding the
chosen one's settings alone and saying in its message what it cannot keep
(`save::Loss`): layers in any format but OpenRaster, which is written them laid
together; transparency in a JPEG; partial transparency or more than 256 colours
in a GIF; a palette in OpenRaster, whose layers are read back as colour; square
pixels alone outside a sprite; a density the format cannot state; a sprite's
name and mode outside a sprite area. An OpenRaster file holds each layer whole
on the canvas, with the layers' composite and a thumbnail no larger than 256
pixels on a side. The settings are the document's (`SaveSettings`), seeded from
how its file was written, and the picker that follows holds the name to the
chosen format's endings. A save works out its losses before the file is
touched, so a refusal leaves it whole.

A sprite with no palette of its own shows the desktop's colours, never a PC
palette: a 16-colour sprite's colour *n* is Wimp colour *n*, a 2-colour one's
are Wimp colours 0 and 7, a 4-colour one's 0, 2, 4 and 7, and a 256-colour
one's the RISC OS tint arrangement (`tairix_image::desktop_palette`). It is
written back without one when its colours are still those, and with its own
palette when they are not. A sprite the editor cannot read — a CMYK sprite, say
— is kept byte for byte and written back unchanged; one damaged to a length
that is not a whole number of words would misplace every sprite after it, so a
save that holds one is refused with that reason, before the file is touched.

A file is written back only in a format Paint writes, and never where its
reading kept less than the file held (`tairix_image::Unkept`): samples narrowed
to eight bits, data beside the picture — a colour profile, text, an animation's
further frames, an OpenRaster stack's nesting or blending other than plain
compositing — or colours restated rather than kept, as CMYK is. Such a file
opens read-only and its Save asks how and where. A document of several pictures
is refused as a format of one picture, before the file is touched, and a sprite
kept as its bytes as anything but a sprite area.

## Settings

*Settings…* on the icon bar's menu opens Paint's settings window, or brings
the open one forward: its categories down a sidebar — General, New picture,
Grid, Canvas, Panes — each category's settings in a panel beside it, and
*Restore defaults* beneath.

| Category | Settings |
|---|---|
| General | the tool a new window starts with; whether a picture opens fitted to the window, never past actual size, or at actual size |
| New picture | width, height, format, the colours that format holds, a transparent background where it can be clear — what *New picture* offers to start |
| Grid | spacing and offset across and down, colour, opacity, style; whether a new window shows it; snapping to it; the zoom from which the pixel grid shows, or never |
| Canvas | the checkerboard's square and its shades, the theme's or two chosen; what surrounds the picture, the theme's ground or a chosen colour |
| Panes | the arrangement a new window opens with and *Reset panes* returns to: the front picture window's, taken, or the shipped one |

A change applies to every picture window as the interaction settles and is
written off the loop to Paint's own app-data store, which no other application
reads ([app data](../lib/appdata.md)); what the store then says is what
applies, wherever the user has not changed something since. A refused write is
said in the settings window and on `stderr`, and the values it carried go back
to what the store holds. The tool, the panes, the grid shown and the opening
zoom are a new window's: one already open keeps its own.

With a window's grid shown and snapping on, what is drawn and marked lands on
the grid: a shape's, a marquee's or a crop box's box covers whole cells,
whichever way it is dragged and never less than one; a line's ends, a
gradient's ends and a polygon's corners land on the pixel at the nearest
crossing; a dragged selection's top left lands on a crossing; a crop handle's
edge lands on the nearest line.

## Documents and authority

The manifest requests no filesystem capability and declares
`document-access = "read-write"`: a document reaches Paint only as the user's
own act, and a save writes back through the descriptor it was handed, exactly
as for [TextEdit](textedit.md). A document is untrusted input and so is a
pasted picture, so both are decoded by a capability-empty worker this binary
is re-entered as ([sandbox](../security/sandbox.md)); each document is read by
a fresh one, released once the document is in. A pasted file of layers is
pasted as they show together. What is copied goes to the clipboard as a PNG.

## The loop

Paint runs in the shared document host (`tairix_window::docapp`), which owns
its windows — its picture windows and its settings window — their saves and
choosers, and the work queue saves are written on; the settings are written by
a worker of their own, and the last of them is seen out before the process
ends. Nothing that grows with the picture rather than with the brush runs on the
loop: a decode worker reads documents and pastes, and the queue encodes saves
and copies and carries out fills, gradients, selections, adjustments and their
previews, transforms of every layer, merges and flattens, the Save As survey,
and the putting down or clearing of a floating selection. A job lands on the
layer it was asked of. While a job is out, nothing changes the document, the
sprite showing or the layer painted on, so its answer is written over exactly
the state it was asked of, and one that no longer finds that state is refused.
A save chooses its format from the frozen document it writes. Every allocation
that grows with the picture is asked for, so a refusal is said in the window
rather than ending the program; closing a window, or quitting, with unsaved
changes asks first.

## Limits

- A picture is at most 65,536 pixels on a side and 64 Mi pixels in all, and a
  file at most 64 MiB: the sandboxed decode's own bounds.
- A sprite area holds at most 4,096 sprites, and a picture at most 256 layers
  (`tairix_image::MOST_ORA_LAYERS`), each named in at most 255 bytes.
- A brush is at most 64 pixels across (`tool::MAX_SIZE`), feathering reaches at
  most 100 pixels, and text is set from 6 to 400 pixels high.
