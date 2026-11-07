# DRAW — the desktop vector drawing editor (`Draw.app`)

Binding under `AGENTS.md`. What the vector editor is, the geometry and the
drawing model it is built on, the formats it reads and writes, and the seams
that keep parsing out of its address space and rendering off its loop.

Read first: `plans/PAINT.md` (the document host and the editor conventions
Draw shares), `plans/SVG.md` (the decoder whose grammar Draw's reader reuses),
`plans/SAVE-DIALOG.md`, `plans/FONT-SERVICE.md`, `plans/FIX-DESKTOP.md`.

## Ledger

| Id | Item | Status |
|---|---|---|
| DR1 | `lib/bezier`: points, the affine map (moved from `lib/raster`), bounds, line, quadratic and cubic segments, elliptical arcs, roots, flattening (moved from `lib/svg`), paths, winding and area, nearest points, arc length, intersections, curve fitting, offsets; `lib/svg`, `lib/icon` and `lib/raster` built on it | done |
| DR2 | `lib/bezier` boolean operations — union, intersection, difference, exclusion, divide — keeping curves, under either fill rule | done |
| DR3 | `lib/svg` exposes its path commands, shape rules and gradient definitions as values its own decode is built from; `lib/xml` takes each reader's own bounds; the XML escaper moves to `lib/xml` | done |
| DR4 | `lib/drawing`: the editable drawing — objects, styles, gradients, text, groups, clip groups, layers, the page — a persistent tree of shared nodes with stable identities and fixed bounds | done |
| DR5 | `lib/drawing` SVG reader and writer, with Draw's own namespace for layers, live shapes, guides, the grid and the page | done |
| DR6 | `lib/drawing` RISC OS Draw file reader and writer, read through the sandbox and claimed as `image/x-riscos-draw` | planned |
| DR7 | `lib/drawing` PDF writer | done |
| DR8 | `lib/drawing` EPS writer | planned |
| DR9 | `lib/drawing` rendering: any device rectangle of a drawing at any scale through `lib/raster`'s one renderer; the outline view | done |
| DR10 | `lib/sandbox::drawedit`: the sandboxed read of SVG and of pasted SVG into the drawing, its checked wire form and fuzz harness | done |
| DR11 | The engine: the document, its history, the selection, the viewport, the tile cache and its render worker, the overlays, the canvas's scrollbars | done |
| DR12 | Tools: select and transform, nodes, pen, pencil, line, rectangle, ellipse, polygon and star, text, eyedropper, zoom, hand | done |
| DR13 | Commands: clipboard, duplicate, arrange, align and distribute, flip and turn, lock and hide, group, the boolean operations, stroke to path and object to path are done; clip, divide, offset, simplify and reverse remain | in progress |
| DR14 | The inspector: Style, Layers, Object and Shape are done; the gradient editor, swatches and a rotation field remain | in progress |
| DR15 | Rulers, guides, the grid and snapping | done |
| DR16 | Page setup: paper presets, orientation, units and a white ground are done; a page of any size and a ground of any colour remain | in progress |
| DR17 | Files: Save and Save As in SVG, Save a copy as PDF, PNG or SVG, and a drawing that lost content on reading opening read-only are done; Draw files (DR6), EPS (DR8), JPEG and TIFF copies and a chosen resolution remain | in progress |
| DR18 | The settings window and Draw's preferences | planned |
| DR19 | The `Run` binary: the read worker, the render worker across the cores, the queue's work, and glyphs fetched only on workers | done |
| DR20 | The bundle: manifest, icon, Help in every required locale, docs, registration | done |
| DR21 | Shared chrome: the status band out of Paint and TextEdit into `lib/controls`; numeric form rows; the ruler | planned |
| DR22 | Embedded raster images: Draw sprite and JPEG objects, SVG `<image>`, placing a picture | planned |
| DR23 | Styled runs within one text object | planned |
| DR24 | Text set along a path | planned |
| DR25 | The gradient tool: a drag sets a linear or radial gradient's ends on the selection | planned |
| DR26 | A new zoom shown at once from the last zoom's tiles resampled, until its own land | planned |

## What it is

`Draw.app` (`name = "Draw"`, `title = "Draw"`, `id = os.tairix.draw`,
`kind = "application"`, `library = "Graphics"`): vector drawing in layers,
from a page of paths, shapes, text and groups. It claims `image/svg+xml`, the
format it reads back what it writes (`image/x-riscos-draw` joins it with DR6);
the double-click order between Draw and the viewer follows the store's, as
Paint's does. The manifest requests `CAP_CONSOLE_WRITE`, `CAP_SHM`,
`CAP_SANDBOX_SPAWN` and `CAP_LOG_EMIT`, and no filesystem capability: every
document is one the user handed over, and every write goes through it.

## Geometry (`lib/bezier`, DR1, DR2)

- **One geometry currency.** `Point` (a vector with the arithmetic geometry
  needs) and the affine map (`Affine`, SVG's `matrix(a b c d e f)`, moved here
  from `lib/raster`, which re-exports it). Below rasterisation and below SVG,
  so the decoder, the rasteriser and the editor share one definition.
- **Segments**: line, quadratic (elevated exactly to a cubic where a cubic is
  wanted), cubic: evaluation, derivative, split at a parameter, sub-segment,
  tight bounds from the derivative's roots, nearest point, arc length and its
  inverse, reversal.
- **Arcs**: SVG's endpoint form to its centre form (radii grown where too
  small, degenerate arcs a line) and a centre arc to cubics, at most a quarter
  turn each.
- **Flattening** is the one flattener every consumer draws through, moved from
  `lib/svg` with its arithmetic unchanged, so every decoded asset keeps its
  bits.
- **Paths** are sub-paths of segments, open or closed: bounds, transform,
  reversal, winding number at a point by exact curve crossings, signed area by
  Green's theorem in closed form per segment, containment under either rule.
- **Intersections**: line–line exactly; line–curve as the roots of one
  polynomial; curve–curve by subdivision of bounding hulls refined by Newton
  steps; a curve's own crossing.
- **Fitting**: Schneider's least-squares cubic fit with Newton
  reparameterisation, split at the worst point, corners kept — the pencil and
  Simplify.
- **Offsets**: a curve offset by Tiller–Hanson, subdivided until within
  tolerance — Outline stroke and Offset.
- **Booleans** (DR2): every curve split where it meets another, coincident
  runs merged, each piece kept or dropped by the winding of the two operands
  on either side of it, and the kept pieces traced into closed sub-paths:
  union, intersection, difference, exclusion and divide, curves kept as curves.
- **Bounds**: every algorithm is total over finite input and bounded in the
  work it does: recursion depth, intersections per pair and output segments
  are fixed bounds, and a result past one is a typed refusal.

## The drawing (`lib/drawing`, DR4)

- **Objects**: a path (sub-paths of nodes, each a point with optional incoming
  and outgoing handles and a corner, smooth or symmetric constraint), a
  rectangle with corner radii, an ellipse with an optional arc as an open arc,
  a pie or a chord, a polygon or star (points, inner radius, rounding, turn),
  text, and groups — a group may be clipped by a path of its own. Every object
  carries an identity, an optional name, visibility, a lock, an opacity and a
  transform.
- **Style**: fill and stroke paints — none, a colour with alpha, or a linear
  or radial gradient (stops, spread, focal point, in the object's own space) —
  the fill rule, stroke width, joins with their miter limit, caps, a dash
  pattern and its offset, arrowheads at either end, and which of fill and
  stroke paints first.
- **Text**: lines of one style — family, weight, italic, size, letter spacing,
  line height, alignment — from a baseline origin. Glyphs are geometry
  fetched from the font service, never part of the drawing.
- **Layers** are the drawing's top-level groups, each named or not, shown or
  hidden, locked or not, with an opacity. **The page**: its size held in its
  own unit as it was stated (so A4 is exactly 210 × 297 mm), and an optional
  background colour; the named paper and screen sizes are one table. Guides,
  the grid, and the drawing's one-line title and description belong to the
  drawing.
- **Persistence**: every node is immutable once shared, behind `Arc`; an edit
  replaces the nodes on the way to the root and shares the rest, so a save or
  a render takes the drawing whole for the price of a pointer and the history
  holds only what left it. A container's children are held in chunks, each
  carrying the bounds of what it holds, so an edit copies only the chunks it
  lands in — one edit or many at once, insertions, removals and replacements
  alike — and a render or a hit test skips chunks it cannot meet. A chunk a
  removal leaves short joins its neighbour, so the list never decays into
  slivers.
- **Identity**: every object keeps its identity across edits, and many are
  found in one walk (`Drawing::find_each`), so an editor keeps its selection
  on the same objects when the drawing beneath it is taken back.
- **Bounds by construction**: a node's conservative bounds are computed as it
  is made; text's from an estimate no character is set wider than, a tab as
  its spaces, letter spacing of either sign counted, and no glyph inks past —
  one that would is drawn scaled down about its pen. Exact bounds are asked of the node with the text measured, and
  say whether any text in them waited on a glyph not yet known.
- **Fixed bounds**: objects, nesting, nodes per path, dash entries, gradient
  stops and text length are containment bounds, as `lib/svg`'s are, sized for
  documents rather than for assets.

## Files (DR5–DR8, DR17)

- **SVG** is the native form: clean SVG 1.1 with Draw's namespace carrying
  what SVG cannot say — layers, live shapes, node constraints, arrowheads,
  guides, the grid and the page's background — and every number spelled
  exactly, so a saved drawing reopens as itself. Read through `lib/svg`'s own grammar and cascade: the
  path, number, length, colour, transform and `viewBox` grammars, the
  stylesheet and the presentation-property cascade, `use` and `symbol`
  expanded, gradients with their `href` chains and bounding-box units, clip
  paths as clip groups, `<text>` and its `<tspan>` lines as text. What the
  drawing cannot hold — filters, masks, patterns, markers other than
  arrowheads, images until DR22, objects' titles and metadata — is listed as
  the document is read (`Unkept`), and a document that lost anything opens
  read-only, as Paint's does. A reference cycle is malformed, and reading
  descends no deeper than a drawing may hold.
- **RISC OS Draw files** (`.aff`, `&AFF`): paths, text and transformed text,
  groups, tagged objects and the options object, in draw units of 1/640
  point; RISC OS font names mapped to and from the installed families by their
  generic kind (Trinity serif, Homerton sans-serif, Corpus monospace);
  triangle caps as arrowheads.
- **Save a copy** (the document host's `Request::SaveCopy`) writes a copy and
  leaves the document's own file, name and changes as they are: PDF (paths,
  gradients as shadings, opacity, clips, text as outlines, streams
  compressed), PNG (the page at its own size, fitted within 8192 pixels,
  rendered by the one renderer) or SVG. EPS (DR8), JPEG, TIFF and a chosen
  resolution (DR17) join them.
- **Reading** runs in the parser sandbox (`lib/sandbox::drawedit`, DR10): the
  bytes go to a capability-empty worker, the drawing comes back in a checked
  wire form a batch at a time, and every value is validated as the drawing is
  rebuilt from it.

## The window (DR11, DR14–DR16, DR21)

- The tool box down the left, the tool's own bar across the top, rulers along
  the canvas's top and left and scrollbars along its right and foot, the
  inspector down the right — tabs over Style, Layers, Object and Shape, a tab
  taller than the panel scrolling — and the status band along the bottom. A
  secondary press opens the one window menu: File, Page, Edit, Object, Path,
  Layer, View and Tools.
- A command's row — picture, label, chord, when it may be chosen — is stated
  once (`Command::spec`), and the menu and the keyboard both read it.
- The canvas shows the page on the pasteboard at a continuous zoom from 1% to
  25600%, anchored on the pointer, by the wheel, pinch, the zoom tool and the
  view menu; the outline view draws every path as a hairline. A scrollbar's
  span is derived from the viewport whenever it is wanted. The view is never
  taken further from the drawing's origin than twice the largest coordinate a
  drawing holds, so tile indices, rulers and the grid stay exact and bounded
  whatever a document's transforms make of its bounds.
- What is under way — typing, a field of the inspector, a colour being
  chosen — lands before anything that acts on the drawing: Undo and Redo, a
  command, a paste, a save or a copy. A change made in the inspector while
  text is being typed joins the typing's one step. Undo and Redo keep the
  selection on the same objects, found by identity where they went; an object
  in a hidden or locked layer leaves the selection, and nothing new goes into
  such a layer.

## Rendering and the loop (DR9, DR11, DR19)

- The drawing is rendered in tiles of the device grid at the current zoom by
  a render worker, a job's tiles spread across the cores — each core taking
  the next tile as it finishes one, with the glyphs it has made shared across
  them — each job holding the drawing's root, so the loop renders nothing
  itself. A window has one job out at a time, whatever changes while it is
  out. A tile not yet rendered shows the page's ground (DR26 brings the last
  zoom's tiles). A tile a render could not make is not asked again until the
  drawing, the mode or the memory pressure changes; a job the renderer could
  not take is asked for again with the next. A window holds the
  tiles in view and twice as many again, once at moderate pressure and none
  beyond the view at severe.
- An edit invalidates only the tiles its old and new bounds meet. A drag
  previews the moving objects over the tiles each frame — at full fidelity
  under a budget of points, by their bounds past it, a path past what is
  left never built — and lands once, when it ends; a node drag previews the
  path and its handles where the pointer has them.
- Glyph outlines are fetched by the workers that draw and trace text, a
  text's characters in one run (`Glyphs::prepare`), into the font client's
  bounded outline cache, which answers a run it holds whole without asking
  the service; the loop reads only what has arrived
  (`tairix_font::cached_outline`), so text is picked, framed and given its
  caret by the glyphs it is drawn with once they have landed, and by its
  estimated box until then. Text whose glyphs could not all be had is never
  traced into paths without them. The window's own labels are drawn without
  waiting on the font service at all: the document host fetches what was
  drawn provisionally on a thread of its own and lays the windows out again
  when it lands.
- The host's queue writes saves and exports and runs what grows with the
  drawing: boolean operations, outlines, offsets and simplification over a
  large selection.

## Invariants

- No filesystem capability; nothing of a document or a pasted drawing is
  parsed in the editor's address space.
- What a window shows is what an export renders: one renderer.
- A paint reads nothing and costs what the window shows, not what the drawing
  holds.
- Every allocation that grows with the drawing is fallible; a refusal is
  stated in the window, never an abort.
- A drag, a slider or a typed value lands as one step when it settles.
- A document that lost content on reading is never written back over its own
  file.
- Closing or quitting with unsaved changes asks first.
