# Draw

`Draw.app` (`userland/apps/draw`, `plans/DRAW.md`) is the desktop vector
editor. It is one resident instance with a window per document; closing the
last window leaves it on the icon bar, whose menu offers a new window and
Quit. A drawing is `lib/drawing`'s ([its page](../lib/drawing.md)), its
geometry `lib/bezier`'s ([its page](../lib/bezier.md)).

## What a window shows

The tool box runs down the left, the inspector down the right with its tabs
above its panel, and the desktop's one status band along the bottom
(`lib/controls`' `StatusBand`): what the last command did — or, while it says
nothing, how the tool in hand is used (`Tool::hint`) — cut short of the
readings beside it, then the pointer's place and the selection's size in the
page's units and the zoom, each in a slot measured for the most it reads; the
zoom opens a menu of zooms when pressed. There is no bar across the top, as
there is no menu bar. Rulers in the page's units run
along the canvas's top and left and scrollbars along its right and foot; a
bar's span is derived from the viewport each time it is wanted — the page and
what is drawn, half a canvas of pasteboard beyond them, and wherever the
canvas looks — so it can never disagree with the canvas. A tool button's tip
names the tool and the key that chooses it.

Every length is a logical one scaled to device pixels: handles, node marks,
the reach a press picks within — the settings' pick distance — and a wheel
detent's scroll grow with the display's scale.

The window has no menu bar. A secondary press anywhere opens the window's one
menu: File, Page, Edit, Object, Path, Layer, View and Tools, each a submenu. A
command's row — its picture, label, chord and when it may be chosen — is
stated once (`menu::Command::spec`); the menu draws its rows from it and the
keyboard finds its commands in it, so a row can never show a chord the
keyboard does not honour, and a test holds that no two commands answer one
chord. Every row names its function from the shared vocabulary
([menus](menus.md)); Draw's own functions — group and ungroup, lock and
unlock, bring to front and send to back, the four boolean operations and
divide, object and stroke to path, outset and inset, simplify, reverse
direction, clip and release clip, the node tool and the pen, outlines,
snapping, zoom to the selection, units, the page's ground and placing a
picture — are pictured as the rest are ([icons](icons.md#menu-pictures)).

## The tools

Select, nodes, rectangle, ellipse, star and polygon, line, pen, pencil, text,
gradient, eyedropper, zoom and hand, chosen by `V`, `A`, `R`, `E`, `S`, `L`,
`B`, `P`, `T`, `G`, `I`, `Z` and `H`. The gradient tool lays the Style tab's
paint as a gradient along a drag, mapped into each selected object's own
space through its transform — radial about the drag's start with Alt or
where it is radial already, in 15° steps with Shift — shown live and landing
as one step; while it is in hand, the one object selected shows its
gradient's course. The eyedropper gives a gradient it takes to each object
fitted to that object. A drag is previewed over the tiles and lands as one
step when it ends: moving (Shift to one axis, Alt a copy), scaling from the
opposite handle (Shift on a corner keeps the proportions), turning past a
corner (Shift in 15° steps), a marquee (wholly within, or with Alt whatever it
touches), a shape (Shift even, Alt from the centre). The pen sets corner and
smooth nodes and closes on the first; the pencil's points are fitted with
cubics when it is let go. Typing into text is shown live and lands as one step
when it ends — a change made in the inspector meanwhile joins it, and Undo, a
command, a paste or a save lands it first; text left empty is removed. A press
puts the caret at the character boundary nearest it (`Text::offset_at`); a
drag, or Shift with the arrows, Home and End, selects, and Ctrl+A selects the
whole text. A key replaces the selection through `Text::replaced`, so runs
follow their characters and what is typed takes the style of what it replaces
or follows (`Text::run_for`). While characters are selected, the inspector's
family, size, weight, posture, letter spacing and raise, and a fill or stroke
colour, restyle them as runs (`Text::restyled`); with none selected they set
the whole text and clear the same from its runs, and a paint given a whole
text, a gradient among them, clears its runs' paint. The caret stands across
the line where the next character goes, turned with a path the text follows,
and the selection is marked character by character from the line's pens. A
node
drag — on a path, or on the path a text is set along (`input::node_path`) —
previews the path and its handles where the pointer has them. Nothing new
goes into a hidden or locked layer, and what one holds leaves the selection.
Undo and Redo keep the selection on the same objects, found where they went.
While Snap is on the pointer snaps,
as a shape is drawn, a pen node set or the selection moved or scaled, to
guides, the page's edges and centre, and the grid while it shows
(`snap::snapped`).

## The inspector

Five tabs: Style (fill and stroke — none, a colour, or a linear or radial
gradient — the colour picker, the swatches, width, joins, caps, dashes,
arrowheads, the fill rule and opacity), Layers, Object (place, size and
rotation, align and distribute), Shape (a rectangle's corners, a star's points,
inner radius and polygon, text's family, size, weight, posture, letter
spacing, raise and alignment, its line height — or along a path, where along
it the text starts — shown for the first character selected while typing, and
the distance Outset and Inset move an outline and the smoothness Simplify
keeps it within) and Page (the page's width and height in its unit, its
orientation and unit, the grid's spacing and subdivisions, the resolution
raster copies are made at, and its ground: none, or any colour from a picker
of its own).

A gradient chosen is fitted to each selected object's own box
(`gradient::fitted`): a linear one so its ends meet the box's farthest
corners at its angle, a radial one from the box's centre to its corners,
from the object's colour to white, or to black where the colour is lighter
than a mid grey. An object drawn while the style in hand holds a gradient
takes it fitted to itself, and text when its typing lands. Editing a
gradient — its stops, their places and colours, its spread, reversing it —
gives every selected object the stops and spread shown, each keeping where
its own gradient lies; changing its form or angle fits it again. The stop
bar (`stops::Bar`) draws the gradient as the renderer blends it, over a
checker, with a handle per stop: a press on a handle takes that stop in hand
and drags it, held between its neighbours; a press on the strip adds a stop
of the colour the gradient has there. The swatches are a fixed palette —
greys, then twelve hues in full colour, tint and shade — followed by the
drawing's own, saved with it (`Drawing::swatches`); a swatch paints the
colour or the stop in hand. Rotation reads one object's angle from its
transform, anticlockwise, and turns it about its centre to the angle typed;
several turn together by it. The Page menu's Page setup
shows the Page tab, and its Ground row gives the page the colour that tab
holds, or none. A change to the page leaves the view where it is; choosing a
paper size fits the page in the window. A tab is laid out as though the panel were tall enough to hold it
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

## Settings

*Settings…* on the icon bar's menu opens Draw's settings window, or brings the
open one forward: the shared settings window of every document application
(`tairix_window::docapp::settings`, over `tairix_controls::SettingsFrame`),
its categories down a sidebar, each one's settings as groups of form rows
beside it, and *Restore defaults* beneath. The registry is
`tairix_draw::preferences::Preferences`, kept in Draw's own app-data store by
the shared engine ([app data](../lib/appdata.md)), lengths in hundredths of a
drawing pixel and a paint as `none` or its hex digits.

| Category | Settings |
|---|---|
| General | the tool a new window starts with; whether it shows the grid, snaps to guides, the page and the grid, and draws outlines only |
| New drawing | the paper, upright or on its side, and the unit a new drawing is measured in; its grid's spacing, typed in that unit, and subdivisions |
| Style | the fill and the stroke new objects are drawn in — none or a colour, translucent or not — and the stroke's width |
| Editing | how far an arrow key nudges the selection, ten times as far with Shift; how far a duplicate stands from what it copies; how near the pointer must come to pick a handle, a node or a guide, and to snap |

A new window starts with the tool, the view and the style; a new drawing with
the page and the grid; every open window takes up the editing settings as they
are made. A change is written off the loop as its interaction settles, what the
store then says is what applies wherever the user has not changed something
since, and a refused write is said in the settings window and on `stderr`.

## Documents and authority

The manifest requests no filesystem capability, declares
`document-access = "read-write"`, and claims `image/svg+xml`,
`image/x-riscos-draw` and `application/pdf`; the double-click order between
Draw and the viewer follows the store's (`plans/PAINT.md`). A document is read
in the parser sandbox (`tairix_sandbox::drawedit`): its bytes go to a
capability-empty worker, which reads it as a RISC OS Draw file, a PDF or SVG
by its content, and the drawing comes back in a checked wire form, every value
validated as the drawing is rebuilt. What a drawing cannot keep is listed as
it is read (`tairix_drawing::unkept`), and a document that lost anything opens
read-only and says what.

A PDF is read through `lib/pdf` (`plans/PDF.md`). The load worker opens it
in a sandbox worker of its own (`drawedit::open_drawing`); one of several
pages comes back as a question (`LoadAnswer::Asks`), and so does one
protected by a password: the empty password is always tried first, so a file
encrypted only to state its permissions opens without asking. The worker that
opened it keeps it until the question is answered or let go — its bytes while
the password is asked, the document opened while a page is chosen — so no
answer reads the file, uploads it or opens it again. A question opens a
dialog window of Draw's own (`pdfopen::OpenPdf`, one kind of
`appwindow::DrawWindow` beside the settings window) over the document's
loading window, asked on its behalf, so it closes with that window
(`Host::open_app_window`), the document's name shown as
`tairix_util::utf8::shown` shows it: *Page* takes a number, its size in points
described beneath it once the worker has said it (`LoadWork::PageSize`, one
page at a time, the latest page wanted winning), or *Password* a secret field
of the most a request carries (`drawedit::MAX_PASSWORD_BYTES`). A page typed
past the document's is held to them and shown before anything opens, and Open
with no password typed, or more than a password can be, opens nothing and
says why. Open hands the answer to the worker keeping the document
(`LoadWork::Answer`); Cancel, closing the window, or a question window that
cannot open — whose refusal the host has already said — lets that worker go
(`LoadWork::Forget`) and gives the load up without an alert of Draw's
(`Host::not_wanted`). A password is held in a buffer erased when dropped,
travels to the worker in a request erased once sent and once read, and is
held by Draw only until that request is made: the worker keeps the document
it opened, never the password, and nothing logs or shows it. A wrong one asks
again, saying so. The page opens as a drawing of its own, its crop box the
page, in points. A page of a PDF of several, or of one holding bookmarks,
attachments or other parts, or of one protected by a password, opens
read-only, so Save never writes one page back over the whole nor a protected
document back unprotected. Text a PDF sets
in its fonts is not yet read (`plans/PDF.md` PDF5), so a page showing any opens
read-only, as does one holding a soft mask, blend mode, tiling pattern, other
shading, unreadable picture, annotation or damage.

Save writes, through the handed-over descriptor, the format
the file's name says, or the filetype its volume records for a save over the
document's own file (`SaveFormat::of`, `SaveTarget::Own`), which never speaks
for another file a Save As names: SVG, Draw's own format, which holds all of a drawing;
a RISC OS Draw file, the save saying what the format holds only as near as it
can; or PDF, the save saying a PDF opens again as one layer whose text is
outlines (`SaveFormat::keeps_editing`). Save As offers
all three, opening on the document's own; any other name is refused before the
file is touched. Each is written by the copy encoders (`copy::copy`), so a
save and a copy in one format are the same bytes. Save a copy
(`Request::SaveCopy`, the document host's) asks the trusted picker for a new
file in a format of the view's `copy_formats` — PDF, EPS, PNG, JPEG, TIFF,
SVG or a RISC OS Draw file — the format the one media registry takes the
file's name, or the filetype its volume records, to be (`CopyFormat::of`),
and the queue writes the copy (`copy::copy`), the document's own file, name
and changes left as they are. An EPS copy is written opaque and the status
band says so, and a Draw copy says what the format could not hold; PNG, JPEG and TIFF copies are rendered by the one
renderer at the drawing's copy resolution, set in the Page tab, record that
resolution, and a JPEG lays what is clear over white paper. Copy puts the
selection on the clipboard as SVG, written on the host's queue, since a
picture makes it as long as its bytes, and refused with a message where it
is longer than the clipboard holds (`CLIPBOARD_MAX_BYTES`); a cut's
deletion is a step Undo takes back. Paste reads the clipboard's drawing in
the sandbox and adds it to the layer in hand at its place on the page, every
object within it given an identity of its own.

## Pictures

*Place picture…* on the File menu (Ctrl+Shift+P) asks the trusted picker for a
picture (`Request::Import`, the document host's): it is handed over read-only
whatever Draw may edit (`PickPurpose::Import`), since a picture placed is the
drawing's to keep and never the file's to change. The load worker sends its
bytes to the sandbox, which decodes any still picture the image library reads
by its content (`tairix_drawing::decode_placed`) — the first page or sprite of
a file of several — and sends back its pixels, its size at the density the
file states, and, for a PNG or a JPEG, the bytes it came as; the worker builds
its levels, so the loop only lays it. It lands in the layer in hand as one
step, selected and named after its file, in the middle of what the canvas
shows at its own size, or as large as four fifths of the canvas holds where
that is smaller. A picture dragged from the file manager or off the desktop and
let go over the canvas is placed the same way, centred where it was let go: the window answers
the desktop's file offer (`DocumentView::places`) for the types the image
library reads whole and for sprite files, over the canvas alone, and the file
arrives read-only through the document host (`DocumentApp::import_chosen`,
with the drop point). Any other file dropped on the window, or a picture
dropped off the canvas, is taken as Draw's icon-bar slot takes it: an SVG or
Draw file opens, anything else is refused in Draw's name. A drawing's pictures hold no more pixels together than a
drawing's may (`MAX_TOTAL_IMAGE_PIXELS`, each picture counted once however
often it is shown), the bound every reader holds a document to, so placing
or pasting past it is refused and said (`EditError::Crowded`) and no drawing
Draw writes is refused for its pictures' pixels. The file a drawing makes
is bounded by nothing in it: a save, or a copy as SVG or a Draw file, longer
than this computer's sandbox worker may be handed (its `document_room`) is
still written, and the window says Draw cannot open it again here.

A picture is drawn from the first level of its pixels that sets no more than
two of them across a screen pixel, so one shown far smaller is averaged
rather than aliased, interpolated within its own pixels and covering exactly
its rectangle. It moves, scales, turns,
clips and is copied as any object does; it has no style, no nodes and no
outline of its own, so the Style tab, the node tool, object and stroke to path
and Outset pass it by, a boolean operation takes its rectangle, and it clips
by its rectangle. The outline view draws its frame. Saved and copied, it keeps
the bytes it came as wherever the format can carry them: SVG embeds them as a
data URL, a PDF a JPEG as it came, an EPS and a Draw file a baseline JPEG, and
a Draw file a sprite it was read from as that sprite; anything else is written
from its pixels.

## The loop

Draw runs in the shared document host (`tairix_window::docapp`). The loop
reads nothing and waits on nothing:

- the canvas is painted from tiles of the device grid at the present zoom
  (`tiles::Tiles`), rendered by a worker from the drawing as it stood — a
  persistent tree, so a job holds the drawing for the price of a pointer —
  a job's tiles spread across the cores. A window has one job out at a time.
  Tiles hold what is drawn and not the page's ground, which the canvas lays
  beneath them over white paper, so a change to the page alone renders no
  tile again.
  An edit marks stale only the tiles its changed area meets, and a stale tile
  is painted until its replacement lands, so nothing blinks; a tile a render
  could not make is not asked for again until the drawing, the mode or the
  memory pressure changes, so a short machine is never asked in a loop, and a
  job the renderer could not take is asked for again with the next. A new
  zoom is shown at once: the tiles of the last view shown whole — or, with
  none kept, what there was of the view just left — stand in for its own,
  scaled and placed where each of its tiles will land, until they do, so a
  pinch or a wheel zoom never shows the bare page;
- the boolean operations and Divide, object to path and stroke to path,
  Outset, Inset and Simplify are worked out on the host's queue from the
  drawing as it stood, the window free meanwhile; a drawing changed before the
  work lands keeps its change and the work is not applied. Divide cuts the
  selection into the pieces its overlaps make, each painted as the uppermost
  object over it was; Outset and Inset grow or shrink each outline by a
  distance in the drawing's units, refusing an inset that would leave nothing;
  Simplify traces each again with as few nodes as keep it within its
  smoothness, corners kept, and leaves a path it would not make simpler.
  Clip (Ctrl+7) shows the selection only within its uppermost object; Release
  clip puts that object back on top of what the group holds; Reverse direction
  runs a path the other way; Set text on path (`edit::set_on_path`) sets the
  one text selected along the one shape's outline, brought into the text's own
  space, the shape taken away and each line feed a space; Release from path
  (`edit::release_from_path`) sets each such text in lines from its path's
  start and gives the path back after it, stroked as new lines are; Object to
  path traces a text its runs paint otherwise as a group of one path for each
  paint;
- glyph outlines are fetched by the workers that draw and trace text, a
  text's characters in one run, which brings them into the font client's
  bounded outline cache. The loop reads only what has arrived there
  (`tairix_font::cached_outline`), so text a worker has drawn is picked,
  framed, aligned and given its caret (`Text::caret`) by the glyphs it was
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

- A document file, and a picture file placed, is at most what the reading
  worker's share of this computer holds beside its frames
  (`WorkerMemory::document_bytes`: a quarter of the RAM less the worker's
  message frames, and never less than 64 MiB), and a drawing at most
  `tairix_drawing::limits`' objects, path nodes, nesting and text; a picture at most 65 536 pixels on
  a side and what one surface holds, and a drawing's pictures four times that
  together.
- A PNG, JPEG or TIFF copy is made at the drawing's copy resolution, lowered
  to the finest whole resolution that fits 8192 pixels on its longer side
  (`copy::MOST_SIDE`), and the status band says when it was.
- The history keeps 1000 steps, to undo and to redo together, at normal and
  mild memory pressure, 64 at moderate, 8 at severe and 1 at critical; the
  oldest to undo go first, then the farthest to redo.
- The canvas is never scrolled further from the drawing's origin than twice
  the largest coordinate a drawing holds.
- A window keeps the tiles in view and twice as many again beyond them, once as
  many at moderate pressure and none at severe or critical; the tiles standing
  in for a new zoom's are let go at severe pressure too.
