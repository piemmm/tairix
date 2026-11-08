# PDF — the first-party PDF reader (`lib/pdf`) and opening PDFs in Draw

Binding under `AGENTS.md`. `lib/pdf` reads PDF 1.0 to 2.0 into a display list
per page. Draw opens one page of a PDF as an editable drawing through it, and
View.app renders pages through it behind its page source (`plans/VIEW.md`).
Every byte of a PDF is untrusted: it is decoded only in a capability-empty
sandbox worker, every count and length is bounded, every refusal fails closed,
and the decoder is fuzzed.

Read first: `plans/DRAW.md` (the drawing a page becomes, its formats and its
sandbox seam), `plans/VIEW.md` (the page source), `plans/SVG.md` (the sibling
vector decoder's discipline), `plans/APPWIN.md` (every dialog is a window of
its own).

## Ledger

| Item | What it is | State |
|---|---|---|
| PDF1 | Syntax and structure: the lexer, objects, cross-reference tables and streams, object streams, incremental updates, damaged-file repair, the page tree and its inherited attributes, the containment bounds | done |
| PDF2 | Stream filters: Flate and LZW with their predictors, ASCIIHex, ASCII85, RunLength and Crypt; the image filters handed on undecoded | done |
| PDF3 | The standard security handler, revisions 2 to 6: RC4 and AES keys, the empty password tried first and any other asked for; the handler's constructions, RC4 and AES-CBC added to `lib/crypto` for interop only | done |
| PDF4 | Content streams: the graphics state, paths, clipping, colour spaces and functions, Form XObjects, ExtGState opacity, axial and radial shadings, optional content — a page's display list | done |
| PDF5 | Text: simple and composite fonts, encodings and glyph names, ToUnicode, the standard 14's metrics, Type 3 glyphs, embedded TrueType outlines; Draw setting a run as text where its font maps and it decodes to Unicode, and as outlines where not | planned |
| PDF6 | Embedded CFF and Type 1 font programs: Type 2 and Type 1 charstrings to outlines | planned |
| PDF7 | Images: XObjects and inline images in every colour space and depth, decode arrays, stencil, soft and colour-key masks; grey and RGB JPEGs through `lib/image` | done |
| PDF8 | Draw opens a PDF: a page chosen where it has several and its password asked for where it needs one, read in the sandbox into an editable drawing — paths, clips, groups, gradients, pictures — and what a drawing cannot hold listed | done |
| PDF9 | What a drawing cannot hold as vectors — mesh and function shadings, tiling patterns, soft masks, blend modes — rendered into pictures of their area | planned |
| PDF10 | JPEG 2000 (`JPXDecode`) in `lib/image` | planned |
| PDF11 | JBIG2 in `lib/image` | planned |
| PDF12 | The predefined CJK CMaps and character collections | planned |
| PDF13 | View.app renders PDF pages through its page source | planned |
| PDF14 | CMYK and YCCK JPEGs and CCITT fax pictures: `lib/image`'s JPEG decoder answering four components, and its fax decoder reached with PDF's parameters | planned |
| PDF15 | SASLprep (RFC 4013) of a revision 5 or 6 password before it is hashed, over Unicode 3.2 as RFC 3454 fixes it, on `lib/unicode`'s normalization | done |

## The crate

`lib/pdf` is `no_std` with `alloc`, forbids `unsafe`, and names no external
crate: its decompression is `lib/compress`'s, its JPEG decoder `lib/image`'s,
its decryption `lib/crypto`'s, its colour conversion `lib/colour`'s and its
geometry `lib/bezier`'s (its TrueType outlines will be `lib/fontface`'s). It
parses and interprets; it renders nothing and knows nothing of Draw. The
reference is `docs/src/lib/pdf.md`.

- **`Document`** holds the file's bytes and its cross-reference index, and
  resolves an object lazily the first time it is asked for, so a page is read
  without parsing the objects of every other. Resolution is depth-bounded, so
  a reference cycle is malformed rather than a loop.
- **`Page`** is a page's geometry and resources: its media and crop boxes, its
  rotation and user unit, each inherited through the page tree as the
  specification says, the crop box held within the media box.
- **`DisplayList`** is what interpreting a page's content makes: fills,
  strokes, clip pushes and pops, groups and images (and, with PDF5, text
  runs), each holding its geometry in the space it was painted in and the
  transform into the page's own space — points from the top left of its
  rotated crop box, y down — so a stroke keeps the pen it was drawn with;
  every colour in sRGB. What a page holds that the list cannot is recorded
  as `Losses`. Both consumers read this one list; neither re-interprets
  content.

## The containment bounds

Fixed validation bounds on untrusted input, never capacities: a PDF past one
is refused as too complex, never truncated into a different document.

- Nesting of arrays and dictionaries, 64; of `q` saves and of Form XObjects
  painting forms, 64 and 16.
- A cross-reference holds at most 8 388 607 objects (PDF's own limit); an
  object stream at most 100 000.
- The decoded bytes a document may produce, filters and pictures included,
  are the caller's budget — Draw's the sandbox worker's document room — so no
  filter chain can expand past the memory the worker was given.
- A page's content runs at most 4 000 000 operators and its display list at
  most 2 000 000 path points; graphics states are saved at most 64 deep (a
  deeper `q` counted, saving nothing), forms nest at most 16 deep, and an
  image is at most 65 536 pixels a side. A drawing's own limits then bound
  what a page becomes.

## Stages

### PDF1 — syntax and structure

- **Lexer and objects**: the tokens and objects of ISO 32000-2 §7.2–§7.3 —
  null, booleans, integers and reals, literal strings with their escapes and
  balanced parentheses, hexadecimal strings, names with `#xx` escapes,
  arrays, dictionaries, streams and indirect references. A stream's length is
  read from `/Length`, resolved if indirect, and checked against the file; a
  wrong length is recovered by finding `endstream`.
- **Cross-reference**: tables (§7.5.4) and streams (§7.5.8), object streams
  (§7.5.7), hybrid files, and the `/Prev` chain of incremental updates with
  the newest definition winning; a `/Prev` loop is malformed.
- **Repair**: a missing or damaged cross-reference is rebuilt by scanning the
  file for `N G obj` and for trailers and cross-reference streams, the last
  definition of an object winning, as every reader of real-world files does.
- **The page tree**: `/Pages` walked with inherited `/Resources`,
  `/MediaBox`, `/CropBox` and `/Rotate`; a node reached twice — shared or in a
  loop — read once, and at most 200 000 kids listed.

### PDF2 — filters

Flate (`lib/compress`) and LZW with `/EarlyChange`, each with the PNG and TIFF
predictors of `/DecodeParms`; ASCIIHex, ASCII85 and RunLength; Crypt naming a
crypt filter. A chain applies in order, each stage's output bounded. DCT, JPX,
CCITTFax and JBIG2 end a chain undecoded, handed with their parameters to the
image stage.

### PDF3 — encryption

The standard security handler of §7.6.4: revisions 2 to 4 (RC4 of 40 to 128
bits, AESV2 through crypt filters) and 5 and 6 (AESV3, the revision 6 hash).
The empty user password is tried first, so the many files encrypted only to
state permissions open as any other; a file needing a password says so, and
the caller asks the user. Strings and streams are decrypted per object, a
stream's own `/Crypt` filter, the `/Identity` crypt filter and
`/EncryptMetadata` honoured. Revisions 2 to 4 hash a password in
`PDFDocEncoding` where it is text that encoding holds, and as its bytes
otherwise; revisions 5 and 6 its text prepared by SASLprep (PDF15) as a
query, then cut to 127 bytes of UTF-8 — and the bytes as given once that
fails, since several producers prepare nothing. SASLprep is `lib/unicode`'s,
over Unicode 3.2 exactly: what 3.2 does not assign passes inert, and the five
decompositions Corrigendum #4 corrected are 3.2's own. Entries no password
could unlock are unsupported, never a password asked for again. The password is a secret: held zeroizing, never
logged, gone once the file key is made.

The handler's constructions enter `lib/crypto` as its `pdf` module, beside
NTLM's, over the MD5 and SHA-2 it already carried and RC4 and AES-CBC from
audited, pinned crates (`rc4`, `cbc`): they decrypt foreign files, RC4 is
reached only through them, and nothing in the system protects data with them.

### PDF4 — content and graphics

The operators of §8: the graphics state stack, `cm`, path construction and
painting with both fill rules, clipping, the line width, cap, join, miter
limit and dash, every colour space (Device, CIE-based through XYZ adapted to
D65, ICC through their alternates, Indexed, Separation and DeviceN through
their tint transforms, Pattern), the four function types, `gs` with its
opacities, `Do` of a Form XObject (its matrix, its bounding box clipped, a
transparency group kept as a group, decoded once) and of an image, `sh`,
shading patterns, and optional content the default configuration hides. A
named colour space, pattern, shading or graphics state is read once per
resources dictionary, the scope a name has meaning in.
Axial and radial shadings become gradients, their functions sampled into
stops within half an sRGB level, a sharp step kept; a luminosity soft mask
that is one shading over the same geometry becomes its gradient's alpha,
which is how Draw writes a translucent gradient. The other shading types,
tiling patterns, other soft masks and blend modes are recorded as lost until
PDF9; text-showing operators are recorded as lost until PDF5.

### PDF5 — text

The text state and operators of §9: simple fonts with their base and
differences encodings, glyph names mapped to Unicode through the Adobe Glyph
List, `/Widths` and the standard 14's metrics for the fonts PDF lets go
unembedded, composite fonts through `Identity-H`/`-V` and embedded CMaps,
`/W` widths, `ToUnicode`, and Type 3 glyphs as content. A run carries its
Unicode text where the font yields it, the family its name and descriptor
flags suggest, and each glyph's outline where the font's program is embedded
and readable.
Draw sets a run as a text object where its font's family maps to an installed
one and its glyphs decode to Unicode, in that family at the run's size along
its baseline; where either fails its glyphs are outlines of their own, and
where no outline exists they are listed as lost.

### PDF6 — CFF and Type 1

Embedded CFF (`FontFile3` `Type1C` and `CIDFontType0C`, and CFF-flavoured
OpenType) and Type 1 (`FontFile`: `eexec` and Type 1 charstrings with `seac`
and flex) programs read into outlines, so a glyph whose text cannot be
recovered is still drawn as itself.

### PDF7 — images

Image XObjects and inline images in every colour space at 1, 2, 4, 8 and 16
bits, `/Decode` arrays, `/ImageMask` stencils in the fill colour, `/SMask`
soft masks (with `/Matte`) and `/Mask` stencils and colour keys as alpha, and
grey and RGB `DCTDecode` through `lib/image`, kept with their bytes and any
EXIF orientation undone. An image used again is decoded once. Only an inline
image's colour space may name a resource, as every reader reads them, and an
unfiltered inline image is as long as its samples in it. CMYK and YCCK
JPEGs and `CCITTFaxDecode` are PDF14's, recorded as lost until then.

### PDF8 — Draw opens a PDF

- **The format**: Draw's manifest claims `application/pdf`; the sandbox reads
  a PDF by its header (`tairix_drawing::read`, `pdf::read`), as it reads SVG
  and Draw files by their content.
- **The questions**: the load worker asks the sandbox worker for the pages
  first (`drawedit::read_pages`, `OP_PAGES`, the document kept for the read).
  One of several pages, unless one was chosen, and one needing a password come
  back as a question (`LoadAnswer::Asks`), asked in a dialog window of Draw's
  own (`pdfopen::OpenPdf`, beside the settings window in
  `appwindow::DrawWindow`), asked on the loading window's behalf so it closes
  with it: a page number, its size in points described, or a secret password
  field of `drawedit::MAX_PASSWORD_BYTES`. A page typed past the document's is
  held to them and shown before anything opens; Open with no password, or
  more than a password can be, opens nothing and says why. Open reads again
  with the answer (`OP_OPEN` carrying the page, the budget — the worker's room
  beside the document — and the password); Cancel or closing it gives the
  load up without an alert (`Host::not_wanted`). A wrong password asks again,
  saying so. The password is erased when dropped, in the request once sent and
  once read, and never kept or logged.
- **The drawing**: the page's crop box is the drawing's page, in points.
  Fills and strokes are shapes in their style — a fill and the stroke of the
  same path that follows it one shape, a stroke as thin as a device draws a
  pixel of the page — clips are clip groups, a transparency group is a group
  at its opacity, images are pictures (a JPEG kept with its bytes), and axial
  and radial shadings are gradients (one not extending past an end painting
  nothing there; a radial one whose first circle has a radius read as
  concentric, recorded as lost where it is not).
- **What is not kept** is listed as for any document (`Unkept`, which gains
  blend modes, shadings no gradient holds, text that could not be read,
  annotations and form fields, and damaged parts): soft masks, blend modes,
  tiling patterns, other shadings, annotations, text and images in codecs not
  yet read — until PDF5, PDF9 to PDF11 and PDF14 keep each — and so are the
  document's other pages, its bookmarks, attachments and other parts, and
  metadata besides its title and subject, which become the drawing's. A PDF
  that lost anything opens read-only, so it is never written over; one Draw
  wrote loses nothing and opens writable. A drawing saved as PDF says it
  opens again as one layer whose text is outlines.

### PDF9 — rendered fallbacks

What no drawing object holds is rendered by the one renderer into a picture
of its area at the drawing's copy resolution, so the page still looks as it
should, and listed as rendered rather than lost.

### PDF10 to PDF12

JPEG 2000 and JBIG2 are codecs of their own in `lib/image`, each with the
fail-closed discipline and fuzzing of the codecs there. The predefined CJK
CMaps and character collections are data compiled into `lib/pdf` from Adobe's
published tables.

### PDF13 — View.app

`lib/pdf` behind View's page source: the page count, each page's geometry,
and a page rendered at a target size from its display list.

### PDF14 — CMYK JPEGs and fax pictures

`lib/image`'s JPEG decoder answers a four-component frame's samples — Adobe's
inverted CMYK and its YCCK transform — for the PDF colour space and decode
array to read, and its CCITT decoder is reached with PDF's parameters (`/K`,
`/Columns`, `/Rows`, `/EncodedByteAlign`, `/EndOfBlock`, `/BlackIs1`).
