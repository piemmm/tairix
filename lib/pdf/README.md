# tairix-pdf

Stability: **experimental**.

The first-party PDF reader (`plans/PDF.md`): a document's structure read
lazily, each page interpreted into a display list. It parses and interprets;
it renders nothing and knows nothing of its consumers.

- `Document::open` — the cross-reference as the file states it (tables,
  streams, object streams, hybrid files, the `/Prev` chain of updates), and
  rebuilt by scanning where that fails; the standard security handler, the
  empty password tried first; the page tree with its inherited attributes.
  Objects are read only when asked for.
- `Page` — one page's crop box, rotation and user unit; `size` is what it is
  shown at, in points. Its resources are read only with its content.
- `Document::about` — its title and subject, and whether it holds more than
  a reader of its pages keeps.
- `recognised` — whether bytes are a PDF, by their header.
- `Page::display_list` — what a page paints, in order: fills, strokes,
  clips, transparency groups and pictures, each with its geometry in the
  space it was painted in and the transform into the page's own space (points
  from the top left of its rotated crop box, y down), every colour sRGB.
  What a page holds that the list cannot is recorded as `Losses`, never
  dropped silently.

Every byte of a PDF is untrusted. Reading is total: no input panics, every
count, depth and length is held to a fixed containment bound, every stream
decodes within the caller's byte budget, every allocation that grows with the
file can be refused, and a refusal is a `PdfError`, never a different
document. A caller reads PDFs only in a sandbox worker. `#![forbid(unsafe_code)]`.

Readers: `lib/drawing`'s PDF reader, in Draw's sandbox worker. See
`docs/src/lib/pdf.md`; `tests/fuzz_pdf.rs` is the fuzz harness.
