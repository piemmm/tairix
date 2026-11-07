# Draw

`Draw.app` (`userland/apps/draw`, `plans/DRAW.md`) is the desktop vector
editor. It is one resident instance with a window per document; closing the
last window leaves it on the icon bar, whose menu offers a new window and
Quit. A drawing is `lib/drawing`'s ([its page](../lib/drawing.md)), its
geometry `lib/bezier`'s ([its page](../lib/bezier.md)).

## What a window shows

The tool box runs down the left, the inspector down the right with its tabs
above its panel, and the status band along the bottom, showing the pointer's
place and the selection's size in the page's units and what the last command
did. The bar across the top says how the tool in hand is used and shows the
zoom, which opens a menu of zooms when pressed. Rulers in the page's units run
along the canvas's top and left and scrollbars along its right and foot; a
bar's span is derived from the viewport each time it is wanted — the page and
what is drawn, half a canvas of pasteboard beyond them, and wherever the
canvas looks — so it can never disagree with the canvas. A tool button's tip
names the tool and the key that chooses it.

Every length is a logical one scaled to device pixels: handles, node marks,
the reach a press picks within and a wheel detent's scroll grow with the
display's scale.

The window has no menu bar. A secondary press anywhere opens the window's one
menu: File, Page, Edit, Object, Path, Layer, View and Tools, each a submenu. A
command's row — its picture, label, chord and when it may be chosen — is
stated once (`menu::Command::spec`); the menu draws its rows from it and the
keyboard finds its commands in it, so a row can never show a chord the
keyboard does not honour, and a test holds that no two commands answer one
chord. Every row names its function from the shared vocabulary
([menus](menus.md)); Draw's own functions — group and ungroup, lock and
unlock, bring to front and send to back, the four boolean operations, object
and stroke to path, the node tool and the pen, outlines, snapping, zoom to
the selection, units and the page's ground — are pictured as the rest are
([icons](icons.md#menu-pictures)).

## The tools

Select, nodes, rectangle, ellipse, star and polygon, line, pen, pencil, text,
eyedropper, zoom and hand, chosen by `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`,
`T`, `I`, `Z` and `H`. A drag is previewed over the tiles and lands as one
step when it ends: moving (Shift to one axis, Alt a copy), scaling from the
opposite handle (Shift on a corner keeps the proportions), turning past a
corner (Shift in 15° steps), a marquee (wholly within, or with Alt whatever it
touches), a shape (Shift even, Alt from the centre). The pen sets corner and
smooth nodes and closes on the first; the pencil's points are fitted with
cubics when it is let go. Typing into text is shown live and lands as one step
when it ends — a change made in the inspector meanwhile joins it, and Undo, a
command, a paste or a save lands it first; text left empty is removed. A node
drag previews the path and its handles where the pointer has them. Nothing new
goes into a hidden or locked layer, and what one holds leaves the selection.
Undo and Redo keep the selection on the same objects, found where they went.
While Snap is on the pointer snaps,
as a shape is drawn, a pen node set or the selection moved or scaled, to
guides, the page's edges and centre, and the grid while it shows
(`snap::snapped`).

## The inspector

Four tabs: Style (fill and stroke, the colour picker, width, joins, caps,
dashes, arrowheads, the fill rule and opacity), Layers, Object (place and
size, align and distribute) and Shape (a rectangle's corners, a star's points,
inner radius and polygon, and text's family, size, weight, posture and
alignment). A tab is laid out as though the panel were tall enough to hold it
and shown through a scroll view (`tairix_controls::ScrollView`); one taller
than the panel scrolls by the wheel or the bar beside it, whose held ends
step on the window's own deadline. A colour being dragged, a width being
typed and an opacity are shown live and land as one step when they settle;
the inspector leaves a control being edited as it is, so a live change always
settles. The colour picker takes the keyboard as the fields do, and Tab walks
from its parts on to the fields. Leaving a field — by Tab, a press elsewhere,
or a chord — commits what was typed into it, and that commit is a change like
any other; a place field sets its own measure and leaves the others as they
are. A change that changes nothing is no step.

## Documents and authority

The manifest requests no filesystem capability, declares
`document-access = "read-write"`, and claims `image/svg+xml`; the
double-click order between Draw and the viewer follows the store's
(`plans/PAINT.md`). A document is read in the parser sandbox
(`tairix_sandbox::drawedit`): its bytes go to a capability-empty worker and
the drawing comes back in a checked wire form, every value validated as the
drawing is rebuilt. What a drawing cannot keep is listed as it is read
(`tairix_drawing::unkept`), and a document that lost anything opens read-only
and says what. Save writes SVG through the handed-over descriptor. Save a copy
(`Request::SaveCopy`, the document host's) asks the trusted picker for a new
file in a format of the view's `copy_formats` — PDF, PNG or SVG — and the
queue writes the copy, the document's own file, name and changes left as
they are. Copy puts the selection on the clipboard as SVG; Paste reads the
clipboard's drawing in the sandbox and adds it to the layer in hand at its
place on the page, every object within it given an identity of its own.

## The loop

Draw runs in the shared document host (`tairix_window::docapp`). The loop
reads nothing and waits on nothing:

- the canvas is painted from tiles of the device grid at the present zoom
  (`tiles::Tiles`), rendered by a worker from the drawing as it stood — a
  persistent tree, so a job holds the drawing for the price of a pointer —
  a job's tiles spread across the cores. A window has one job out at a time.
  An edit marks stale only the tiles its changed area meets, and a stale tile
  is painted until its replacement lands, so nothing blinks; a tile a render
  could not make is not asked for again until the drawing, the mode or the
  memory pressure changes, so a short machine is never asked in a loop, and a
  job the renderer could not take is asked for again with the next;
- the boolean operations, object to path and stroke to path are worked out on
  the host's queue from the drawing as it stood, the window free meanwhile; a
  drawing changed before the work lands keeps its change and the work is not
  applied;
- glyph outlines are fetched by the workers that draw and trace text, a
  text's characters in one run, which brings them into the font client's
  bounded outline cache. The loop reads only what has arrived there
  (`tairix_font::cached_outline`), so text a worker has drawn is picked,
  framed, aligned and given its caret (`Text::pen_at`) by the glyphs it was
  drawn with, and text whose glyphs are still on their way by its estimated
  box; a selection framed by an estimate is framed again when tiles land, one
  framed by its glyphs is not. Object to path refuses text whose glyphs could
  not all be had rather than tracing it without them;
- the window's own labels are drawn without waiting on the font service: the
  document host draws what it has not fetched provisionally, fetches it on a
  thread of its own, and lays the windows out again when it lands.

Held controls — the tool box's and the inspector's and canvas's bars — arm
the window's deadline, so a held press steps on a timer rather than a poll.

## Limits

- A document file is at most 64 MiB (`MAX_DOCUMENT_BYTES`), and a drawing at
  most `tairix_drawing::limits`' objects, path nodes, nesting and text.
- A PNG copy is the page at its own size, scaled to fit 8192 pixels on its
  longer side.
- The history keeps 1000 steps, to undo and to redo together, at normal and
  mild memory pressure, 64 at moderate, 8 at severe and 1 at critical; the
  oldest to undo go first, then the farthest to redo.
- The canvas is never scrolled further from the drawing's origin than twice
  the largest coordinate a drawing holds.
- A window keeps the tiles in view and twice as many again beyond them, once as
  many at moderate pressure and none at severe or critical.
