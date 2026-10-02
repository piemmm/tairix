# Paint

`Paint.app` (`userland/apps/paint`, `plans/PAINT.md`) is the desktop image
editor. It is one resident instance with a window per document; closing the
last window leaves it on the icon bar, whose menu offers a new window and
Quit.

## What a window shows

A picture is edited at the depth it is stored in: a 1, 2, 4 or 8-bit palette,
or 32-bit colour with alpha. A toolbar strip runs across the top — the tools,
then the view's own commands (zoom in and out, fit, actual size, grid) — with
the panel down the left, the canvas and its bars beside it and the status band
along the bottom. The window shows the picture at any zoom from 1/16 to 64,
kept in 4096ths of actual size (`viewport::Zoom`), every rung of the ladder
(`viewport::ZOOMS`) exactly. Ctrl and the wheel step it a rung for each
detent's worth of turn, a fine wheel's fractions adding up, anchored so the
pixel under the pointer stays under it, and from between two rungs the first
step lands on the nearer one in its direction; zoom in and out step the same
way. A pinch begun over the canvas zooms smoothly by the fingers' spread and
follows their centre, both measured from where it began, so it accumulates no
rounding, and a cancelled pinch puts the view back. The picture is drawn as
tall or wide as the sprite's pixels are, over a
checkerboard where it is clear, with a grid between pixels once one spans
`GRID_FROM` screen pixels. Beside it the panel holds the primary and secondary
colours, the picture's palette — the desktop's sixteen colours for a
truecolour picture — and the settings of the tool in use; the status band says
where the pointer is, what was last said, which sprite shows, and the zoom.

The tools are select, pencil, brush, spray, eraser, fill, colour picker, line,
rectangle and ellipse. The primary button paints the primary colour and the
middle button the secondary; Alt picks a colour instead. A drag belongs to the
button that began it, and is finished by anything that takes the pointer or
changes the picture under it — a menu, a question, another sprite, a
transform — while Escape turns it down, keeping nothing it did: a stroke is
taken back off the picture, a marquee marks nothing, and a dragged selection
goes back where the drag began. A shape is previewed as it is dragged and
drawn exactly as the preview showed. Dragging a selection lifts it into a
floating layer that writes nothing until it is put down — by a press off it,
Escape, or anything that changes the picture — and its move is one change to
undo.
Undo holds every step while memory is plentiful and gives up the oldest as
pressure rises — twice the document's own size under mild pressure, once under
moderate, nothing under severe — holding to that as steps are added; memory
refused costs the history, redo first, before it costs a change.

The window has no menu bar. A secondary press anywhere opens its menu — Cut,
Copy, Paste, Select all and Deselect, then File, Edit, Image, Colours,
Sprites, View and Tools, each a submenu — drawn by the desktop like every
application's ([menus](menus.md)).

## Formats

Paint opens every raster format TAIRiX decodes ([image](../lib/image.md)) —
SVG has no pixels of its own to edit, and is refused — and writes PNG, JPEG
and RISC OS sprite areas. A PNG keeps its palette; a JPEG is written at the
quality File sets, and as it holds no transparency, a picture with any is laid
over white and the save says so. A sprite area holds any number of sprites,
each with its name, mode, palette and mask, and the Sprites menu adds,
duplicates, renames, deletes and reorders them. A new sprite is offered the
name `sprite`, and a copy its original's, each numbered with the least number
free when the name is taken; a picture saved as a sprite with no name of its
own is named after the file.

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
reading kept less than the file held: a 16-bit PNG read as 8 bits, or a PNG or
JPEG holding a colour profile, text, metadata or further frames beside its
picture. Such a file opens read-only and its Save asks where. A document of several sprites is refused as
a PNG or JPEG, before the file is touched, rather than written as one of them.
Save As holds the name to the endings of the formats that can hold the
document (`.png`, `,b60`, `.jpg`, `.spr`, `,ff9`, …), its own format's first:
the desktop's chooser refuses another ending, and gives a name with none the
first, before it makes any file.

## Documents and authority

The manifest requests no filesystem capability and declares
`document-access = "read-write"`: a document reaches Paint only as the user's
own act, and a save writes back through the descriptor it was handed, exactly
as for [TextEdit](textedit.md). A document is untrusted input and so is a
pasted picture, so both are decoded by a capability-empty worker this binary
is re-entered as ([sandbox](../security/sandbox.md)); each document is read by
a fresh one, released once the document is in, so a hostile file can reach no
other document's decode. What is copied goes to the clipboard as a PNG.

## The loop

Paint runs in the shared document host (`tairix_window::docapp`), which owns
its windows, their saves and choosers, and the work queue saves are written
on. Nothing that grows with the picture rather than with the brush runs on the
loop: a decode worker reads documents and pastes, and the queue encodes saves
and copies and carries out fills, transforms, and the putting down or clearing
of a floating selection beside them. A cut is a copy queued ahead of its
clearing, so the clipboard has the selection before it leaves the picture, and
an action that needs a selection down goes on once it is. While a fill or
transform is out, nothing changes the document or the sprite showing — no
edit, undo, palette change, rename or move to another sprite — so its answer
is written over exactly the state it was asked of, and one that no longer
finds that state is refused. A save chooses its format from the frozen
document it writes, so a chained save is written as what it was when it was
asked for. A copy still reaches the clipboard once its window has moved on or
closed, through whichever window of the program has the keyboard, and is said
to be lost when none has. Every allocation that grows with the picture is
asked for, so a refusal is said in the window rather than ending the program;
closing a window, or quitting, with unsaved changes asks first, and a quit puts
that question in place of any other.

## Limits

- A picture is at most 65,536 pixels on a side and 64 Mi pixels in all, and a
  file at most 64 MiB: the sandboxed decode's own bounds.
- A sprite area holds at most 4,096 sprites.
- A brush is at most 64 pixels across (`tool::MAX_SIZE`).
