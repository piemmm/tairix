# `Draw` — the desktop vector editor

`Draw.app` draws and edits vector drawings: paths, rectangles, ellipses, stars
and polygons, lines, freehand curves and text, in groups and layers, on a page.
It opens and saves SVG and writes copies as PDF, PNG or SVG. The design is
`plans/DRAW.md`; the drawing model is `lib/drawing` and its geometry
`lib/bezier`.

Stability tier: **experimental**.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT` — and **no
filesystem capability**. A document reaches Draw only as the user's own act: a
descriptor the file manager or the desktop opened for it, or a grant the
session's trusted picker delegated. The manifest declares `document-access =
"read-write"`, so a document the user may write is handed over writable and
Save writes SVG back through it; any other opens read-only, and Save asks
where. A file holding what a drawing cannot keep opens read-only too, so it is
never written over.

`CAP_SANDBOX_SPAWN` re-enters the binary as a capability-empty worker, the one
process it may start: documents and pasted drawings are read there
(`tairix_sandbox::drawedit`), so none of their bytes are parsed in Draw's
address space.

## What runs where

The engine (`src/lib.rs` and its modules) is host-tested and does no I/O. The
`Run` binary keeps everything that waits, or grows with the drawing, off the
window's loop: a read worker reads documents and pastes through the sandbox; a
render worker draws each window's tiles, a job's tiles spread across the
cores; and the host's queue writes saves and copies and works out the boolean
operations, object to path and stroke to path. Glyph outlines are fetched only
by the workers that draw and trace text, a text's characters in one run; the
loop reads those the font client's cache already holds and frames text whose
glyphs have not arrived by its estimated box. The window's own labels never
wait on the font service: the document host fetches what it drew
provisionally on a thread of its own.

## Limits

- A document file is at most 64 MiB, the sandboxed read's own bound, and a
  drawing holds at most `tairix_drawing::limits`' objects, nodes, nesting and
  text.
- A PNG copy is the page at its own size, scaled down to fit 8192 pixels on
  its longer side.
- The history keeps 1000 steps, to undo and to redo together, at normal and
  mild memory pressure, 64 at moderate, 8 at severe and 1 at critical.
- A window keeps the tiles in view and twice as many again, once as many at
  moderate pressure and none beyond the view at severe.
