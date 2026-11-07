## NAME

Draw — vector drawing editor

## SYNOPSIS

`Draw`

## DESCRIPTION

Draws and edits vector drawings in a desktop window: paths, rectangles,
ellipses, stars and polygons, lines, freehand curves and text, in groups and
layers, on a page. Launched with a document — from the file manager, from the
desktop, or by dropping a file on its icon in the icon bar — it opens a window
on it. Launched on its own it opens a new drawing on an A4 page. Each document
is a window of the one program; closing the last leaves it on the icon bar,
and the Quit row of its icon menu ends it.

Draw opens and saves SVG. A drawing keeps its page and units, its layers,
groups and clips, every path, shape and text, solid colours and linear and
radial gradients, the opacity of each object, dashes, joins, caps and
arrowheads, the guides and the grid, and the drawing's title. A file holding
what a drawing cannot keep — filter effects, masks, pattern fills, markers,
embedded pictures, mixed text styles, animation or scripts among them — opens
read-only, the status band naming what it holds, and Save asks for a new name
so the original is never overwritten. Save a copy writes the drawing as PDF,
as PNG — the page at its own size, up to 8192 pixels along its longer side —
or as SVG, leaving the document's own file as it is.

The tool box runs down the window's left, the inspector down its right, and
the status band along its foot, which shows the pointer's place and the
selection's size in the page's units and says what the last command did.
Across the top, the bar says how the tool in hand is used and shows the zoom:
pressing the zoom opens a menu of zooms. Rulers in the page's units run along
the canvas's top and left, and scrollbars along its right and foot. Pausing
over a tool's button names it and the key that chooses it.

The tools, and the keys that choose them, are: select (`V`), nodes (`A`),
rectangle (`R`), ellipse (`E`), star and polygon (`S`), line (`L`), pen (`B`),
pencil (`P`), text (`T`), eyedropper (`I`), zoom (`Z`) and hand (`H`).

The select tool picks an object with a press, a group whole, and Shift adds
an object to the selection or takes it out. A press on empty canvas drags a
marquee that selects what lies wholly within it, or with Alt whatever it
touches. Dragging the selection moves it, along one axis with Shift, and with
Alt moves a copy. Its eight handles scale it from the opposite handle, a
corner with Shift keeping its proportions; dragging just outside a corner
turns it about its centre, with Shift in steps of 15 degrees. The arrow keys
move it a pixel, and ten with Shift.

The nodes tool shows the nodes of the path selected and the handles of its
curves: drag a node to move it, or a handle to bend its curve, a smooth
node's other handle following in line. Rectangles, ellipses, stars and lines
are drawn by a drag: Shift makes the sides even — a square, a circle, a line
at a multiple of 15 degrees — and Alt draws out from where the drag began. A
star is drawn out from its centre, its points, inner radius, and whether it
is a polygon set in the inspector's Shape tab, as are the corners of a new
rectangle.

The pen sets a path node by node: click for a corner, or drag for a smooth
node and its curve. A click on the first node closes the path, and Enter, or
choosing another tool, ends it open; `Delete` or `Backspace` takes back the
last node, and Escape drops the path. The pencil draws freehand, and its line
is fitted with smooth curves when it is let go. The text tool begins text
where it is clicked, or edits the text clicked: type, Enter begins a line,
the arrow keys, Home and End move the caret, and Escape, or a click
elsewhere, ends the typing as one change; text left empty is removed. The
eyedropper gives the selection, and what is drawn next, the style of the
object clicked. The zoom tool zooms in where it is clicked, out with Alt, and
a box dragged with it fills the window. The hand drags the canvas, as does a
drag with the middle button or with Space held, whatever the tool.

The wheel scrolls the canvas, across with Shift; with Ctrl it zooms about the
pointer, as a pinch on a touchpad or touchscreen does. View ▸ Fit page and Fit
selection fit them in the window, and Actual size shows the drawing at
100 per cent; the zoom runs from 1 per cent to 25600.

The inspector holds four tabs. Style sets the selection's fill and stroke —
and, with nothing selected, what is drawn next: choose Fill or Stroke, then
None or a colour from the colour picker, which holds a square of saturation
and value beside a strip of hues, a wheel or sliders, and the colour's
numbers and opacity; then the stroke's width, its joins, mitred, round or
bevelled, its caps, butt, round or square, its dashes, solid, dashed, dotted
or dash-dotted, the arrowheads at its start and end — each press steps
through none, triangle, arrow, circle and diamond — the fill rule, non-zero
or even-odd, and the opacity of each object selected. Layers lists the
layers, the top first: press one to draw into it, and its Shown button to
show or hide it; above the list, New adds a layer, Remove removes the layer
in hand, Lock locks it against change, and Raise and Lower move it. Object
places the selection by its left and top and scales it to a width and a
height, lines it up — by left, centre or right edges, or by top, middle or
bottom — and spreads three or more objects evenly across or down. Objects
line up within their selection, one object, or any number with Align to the
page on, within the page. Shape holds a rectangle's corners, a star's points
and inner radius and whether it is a polygon, and text's family — sans,
serif or monospaced — size, bold, italic and alignment on its origin. A tab
taller than the window scrolls. A colour being chosen, a width being typed
or stepped and an opacity are shown on the canvas as they change and become
one change to undo when they settle; Tab moves to the next field and
Shift+Tab to the one before.

A drag from a ruler sets a guide across or down the canvas; drag a guide to
move it, or back onto a ruler to remove it. View ▸ Grid shows the page's
grid. While Snap is on, the pointer snaps as a shape is drawn, a pen node set
or the selection moved or scaled: to guides, to the page's edges and centre,
and to the grid while it shows. View ▸ Outlines shows every object as its
outline, which reads a crowded drawing. The Page menu sets the page's paper — A4, A3, A5, A6, A2, A1, A0, B5, B4, US Letter,
US Legal, Tabloid, Executive, two screen sizes and an icon — turns it
between upright and landscape, gives it a white ground or none, and sets its
units: pixels, points, picas, millimetres, centimetres or inches.

Object ▸ Group gathers the selection into a group and Ungroup releases it;
Bring to front, Raise, Lower and Send to back reorder objects within their
layer; Flip and Turn mirror the selection or turn it a quarter. Lock and Hide
take objects out of reach, and Unlock all and Show all in layer bring back
those of the layer in hand. The Path menu joins the selected shapes into one
(Union), cuts those above out of the lowest (Difference), keeps where they
overlap (Intersection) or where they do not (Exclusion), turns shapes and
text into paths whose nodes can be edited, and turns strokes into filled
outlines. These are worked out apart from the window, which stays free to
use meanwhile; a drawing changed before the work lands keeps its change, and
the work is not applied.

Every change is one step to undo, and Undo and Redo name the step they take;
up to 1000 steps are kept, fewer while memory is short. Copy puts the
selection on the clipboard as SVG, and Paste adds what the clipboard holds to
the layer in hand, at its place on the page, selected.

Draw holds no filesystem capability. It edits only the file it was handed: a
file the user may change is handed over writable and Save writes it back, and
any other opens read-only and Save asks where to save it. Documents, and
drawings pasted from the clipboard, are read in a separate worker process
with no reach at all, so a hostile file cannot touch anything Draw can.

Pressing the secondary (right) mouse button anywhere in the window opens its
menu: File, Page, Edit, Object, Path, Layer, View and Tools, each a submenu.
The window has no menu bar. Closing a window, or quitting, with changes not
saved asks first. Closing a window with the secondary button on its close
button also shows its file in the file manager, in the folder that holds it.

* `Ctrl+N` — a new window; `Ctrl+O` — open a file
* `Ctrl+S` — save; `Ctrl+Shift+S` — save as; `Ctrl+Shift+E` — save a copy as PDF, PNG or SVG
* `Ctrl+W` — close the window
* `Ctrl+Z` — undo; `Ctrl+Shift+Z` or `Ctrl+Y` — redo
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cut, copy, paste; `Ctrl+D` — duplicate
* `Delete` or `Backspace` — delete the selection
* `Ctrl+A` — select all; `Ctrl+Shift+A` — select none; `Escape` — select none, or drop the path being set
* `Ctrl+G` — group; `Ctrl+Shift+G` — ungroup
* `Page Up` / `Page Down` — raise or lower; `Home` / `End` — bring to front or send to back
* `Ctrl+[` / `Ctrl+]` — turn left or right
* `Ctrl+L` — lock; `Ctrl+Shift+L` — unlock all in the layer
* `Ctrl+H` — hide; `Ctrl+Shift+H` — show all in the layer
* `Ctrl+Shift+C` — object to path
* `Ctrl+Shift+N` — a new layer; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — raise or lower the layer
* `Ctrl+=` or `Ctrl++` / `Ctrl+-` — zoom in or out; `Ctrl+1` — actual size; `Ctrl+0` — fit the page
* `Ctrl+'` — show or hide the grid; `%` — snap or not
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `I`, `Z`, `H` — the tools, in order
* arrow keys — move the selection a pixel; with `Shift`, ten
* `Space` — held, drag the canvas with any tool
* `Ctrl` + wheel — zoom about the pointer; `Shift` + wheel — scroll across

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero after Quit. Non-zero when the window channel, the event mailbox, or the
desktop session was refused; the reason is stated on the standard error
stream.
