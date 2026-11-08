# `Draw` — the desktop vector editor

`Draw.app` draws and edits vector drawings: paths, rectangles, ellipses, stars
and polygons, lines, freehand curves and text, in groups and layers, on a page.
It opens SVG and RISC OS Draw files, saves as SVG, a Draw file or PDF, and
writes copies as PDF, EPS, PNG, JPEG, TIFF, SVG or a Draw file. The design is
`plans/DRAW.md`; the drawing model is `lib/drawing` and its geometry
`lib/bezier`.

Stability tier: **experimental**.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT` — and **no
filesystem capability**. A document reaches Draw only as the user's own act: a
descriptor the file manager or the desktop opened for it, or a grant the
session's trusted picker delegated. The manifest declares `document-access =
"read-write"`, so a document the user may write is handed over writable and
Save writes it back through it, in the format its name says; any other opens
read-only, and Save asks where. A file holding what a drawing cannot keep opens read-only too, so it is
never written over.

`CAP_SANDBOX_SPAWN` re-enters the binary as a capability-empty worker, the one
process it may start: documents, pasted drawings and placed pictures are read
there (`tairix_sandbox::drawedit`), so none of their bytes are parsed in
Draw's address space. A picture to place is asked of the picker as an import
(`Request::Import`), or dropped on the canvas from the file manager or the
desktop, handed over read-only either way whatever Draw may edit, since it is
the drawing's to keep and never the file's to change.

## Settings

*Settings…* on the icon bar's menu opens the settings window: the tool, view
and style a new window starts with, the page and grid a new drawing starts
with, and how far the keys nudge and duplicate and how near the pointer
picks. They are kept in Draw's own app-data store and written off the loop,
on a worker of their own, as each interaction settles.

## What runs where

The engine (`src/lib.rs` and its modules) is host-tested and does no I/O. The
`Run` binary keeps everything that waits, or grows with the drawing, off the
window's loop: a read worker reads documents, pastes and placed pictures
through the sandbox and builds each picture's levels; a render worker draws
each window's tiles, a job's tiles spread across the cores; the settings
writer writes the settings; and the host's queue writes
saves and copies and works out the boolean operations, object to path and
stroke to path. Glyph outlines are fetched only by the workers that draw and
trace text, a text's characters in one run; the loop reads those the font
client's cache already holds and frames text whose glyphs have not arrived by
its estimated box. The window's own labels never wait on the font service: the
document host fetches what it drew provisionally on a thread of its own.

## Limits

- A document file is at most what the sandboxed read's worker holds on this
  computer — a quarter of the RAM less its message frames, and never less
  than 64 MiB — and a drawing holds at most `tairix_drawing::limits`' objects,
  nodes, nesting and text. A save, or a copy as SVG or a Draw file, longer
  than that is still written, and the window says Draw cannot open it again
  here. A picture placed is held to the same; it is at most 65 536 pixels on
  a side and a surface's pixels, and a drawing's pictures together at most
  four surfaces', each counted once however often it is shown.
- A PNG, JPEG or TIFF copy is made at the drawing's copy resolution, lowered
  where it must be to fit 8192 pixels on its longer side.
- The history keeps 1000 steps, to undo and to redo together, at normal and
  mild memory pressure, 64 at moderate, 8 at severe and 1 at critical.
- A window keeps the tiles in view and twice as many again, once as many at
  moderate pressure and none beyond the view at severe.
