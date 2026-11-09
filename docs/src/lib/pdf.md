# `tairix-pdf` — the PDF reader

`lib/pdf` reads PDF 1.0 to 2.0 (`plans/PDF.md`). Draw opens one page of a
PDF through it as an editable drawing (`lib/drawing`'s `pdf::read`). It is
`no_std` with `alloc` and names no crate outside the tree: its decompression
is `lib/compress`'s, its JPEG decoder `lib/image`'s, its cryptography
`lib/crypto`'s, its colour conversion `lib/colour`'s and its geometry
`lib/bezier`'s.

## Opening a document

`Document::open(file, &Options { password, budget })`:

- The document keeps `file`, the bytes handed over; a refusal hands them back
  (`Refused`), so a caller may try them again with another password at no
  copy.
- The header must lie within the file's first kilobyte; `recognised` is that
  test alone, for a caller choosing a reader by content.
- The cross-reference is read from `startxref` back through `/Prev`, the
  newest definition of each object winning: tables, cross-reference streams
  (`/W`, `/Index`) and hybrid files, whose stream places what the table
  marks free for older readers. Its trailers are merged once, the newest
  value of each key winning. A `/Prev` loop is malformed.
- Where that fails — no `startxref`, a bad offset, a catalog that does not
  resolve, junk before the header — the file is scanned instead: every
  `N G obj` the last of a number winning, and every trailer or
  cross-reference stream naming a catalog or an encryption dictionary,
  merged as a chain of updates is. With none left, the catalog is the last
  among the objects, looked for once, and an encryption dictionary among
  them is the file's, so an encrypted file cut short is never read as
  plaintext. The object streams found are indexed only once decryption is
  set up, since their bytes may be encrypted.
- An object is parsed only when asked for and then kept, as is why it
  cannot be read, so none is parsed twice; what is kept takes at most a
  quarter of the budget, past which an object is parsed again when asked
  for. Damage an object or object stream came to by meeting another still
  being read is kept only for the read that met it, since read from
  elsewhere it may be found. References are followed at most 32 deep, resolutions made to read a
  stream's length, filters or object stream among them: a cycle is
  malformed, and a chain past the bound too complex, never read as a stream
  with no length. A reference's generation is not insisted on, as no reader
  of real files does. A stream's `/Length` may be indirect or wrong; a wrong
  one is recovered from its `endstream`. Each object stream is decoded once
  and kept, or found unreadable once. A cross-reference stream's entries and
  an object stream's index are charged to the budget with the bytes they
  come from, and reading a file's objects lexes at most 16 times over what
  it and its object streams hold.
- The page tree is walked with its inherited `/Resources`, `/MediaBox`,
  `/CropBox` and `/Rotate`; a node or an array of kids reached twice —
  shared, through another reference, or in a loop — is read once, and one
  that cannot be read is passed over. A page box or
  rotation that cannot be read is none — a US Letter page, unturned — and a
  `/UserUnit` is kept only where the size it scales to is still a size, so a
  damaged page costs its geometry, never the document its page list.
- `Document::about` states the document's title and subject — text strings
  read as UTF-16BE or UTF-8 behind their byte order marks and
  `PDFDocEncoding` otherwise — and whether it holds metadata besides them and
  the programs that made it, or parts besides its pages: bookmarks,
  attachments, forms, scripts, layers, structure, viewing preferences. What
  cannot be read counts as more, so a reader of its pages keeps from writing
  over what it never saw.

## Encryption

The standard security handler, revisions 2 to 6: RC4 of 40 to 128 bits,
AES-128 (`AESV2`) and AES-256 (`AESV3`) through crypt filters, the
`/Identity` filter, a stream's own `/Crypt` filter and `/EncryptMetadata`. A
crypt filter is the one `/CF` defines under its name, or the standard's
`/Identity`, which `/CF` cannot redefine: `/StmF`, `/StrF` or a stream's
`/Crypt` naming one `/CF` does not define is `PdfError::Unsupported`, never
read as the identity, as is a stream of an unencrypted file naming any but
`/Identity`; a `/Crypt` that is not the first filter of its stream, and a
`/CF` entry, `/CFM`, `/StmF` or `/StrF` of the wrong kind, are
`PdfError::Malformed`. The empty password is tried first, so a
file encrypted only to state permissions opens as any other; otherwise
`PdfError::NeedsPassword`, and a password that opens it as neither user nor
owner `PdfError::WrongPassword` — as is one longer than 1024 bytes, which is
not tried, so no password costs more to prepare.
Revisions 2 to 4 hash a password in `PDFDocEncoding` where it is text every
character of which that encoding holds, and as its bytes otherwise; revisions
5 and 6 hash its text as SASLprep prepares it (`tairix_unicode::saslprep`, as
a query), cut to 127 bytes of UTF-8 — so `Ⅸ`, `IX` and `I` soft-hyphen `X`
open alike — and then, where that opens nothing, its bytes as given, since
several producers hash a password unprepared. Encryption entries no password
could unlock — a revision the handler does not define, a key length out of
range, an `/O`, `/U`, `/OE` or `/UE` too short — are `PdfError::Unsupported`,
never a password asked for again. The key derivations, RC4 and AES-CBC are
`lib/crypto`'s `pdf` constructions; strings are decrypted as each object is
read, and streams as they are decoded, an AES stream's final part block
dropped. A password is never kept or logged, and its encoded copy is erased
when dropped; the file key is kept in one place for the document's life, so
moving the document copies none of it, and is erased with the document.

## Filters

Flate (`lib/compress`, a zlib header skipped where valid, the Adler-32 not
insisted on) and LZW with `/EarlyChange`, each with the PNG and TIFF (8- and
16-bit) predictors, undone in place by `lib/image`'s own row routines;
ASCIIHex, ASCII85, RunLength and Crypt. Damaged Flate or
LZW data keeps what decoded before the damage, marked damaged. DCT, JPX,
CCITTFax and JBIG2 end a chain undecoded, for the image stage. Every stage's
output — and every picture's pixels — is charged to the document's budget.

## Content

`Page::display_list` interprets the page's content into a `DisplayList` (a
page's resources are read only then, so listing pages and their sizes reads
none): `Item::Fill`, `Stroke`, `Clip`/`Unclip`, `Group`/`Ungroup` and
`Image`, each with its geometry in the space it was painted in and the
transform into the page's own space — points from the top left of its rotated
crop box, y down — so a stroke under a squashing transform keeps its pen.
The page's content and each form's restore only the graphics states, and
close only the marked content, they opened themselves.

- The graphics state, saved 64 deep (a deeper `q` is counted but saves
  nothing, and unbalanced content is absorbed); paths, both fill rules and
  clipping; the line width, caps, joins, miter limit and dashes. A clip to no
  path lets nothing through, and so does one placed past what a number holds,
  recorded as damage, as is a fill, stroke, picture or shading so placed:
  nothing beneath a clip is drawn unclipped.
- Colour spaces: the device spaces, `CalGray`, `CalRGB` and L\*a\*b\* through
  CIE XYZ adapted to D65, ICC profiles through their alternates or component
  counts, Indexed, Separation and DeviceN through their tint functions (the
  colorant `/None` painting nothing), and Pattern. Functions of all four
  types, each function object read once however many stitching functions
  share it and however often a colour space holding it is read again; a
  sampled function interpolates up to eight inputs.
- `gs`: the pen, `ca`/`CA` opacity, blend modes (recorded as lost) and soft
  masks: a luminosity mask that is one axial or radial shading over the same
  geometry as the shading it masks becomes that gradient's alpha; any other
  is recorded as lost.
- Forms, in their own space and resources and clipped to their box, decoded
  once however often painted; a transparency group is a `Group` at the fill
  opacity, its content opaque within it. A form painting itself is damage.
- A colour space, pattern, shading or graphics state named again is read
  once for each resources dictionary naming it, since a name means what its
  own dictionary says; the device spaces are shared by every colour set in
  them.
- `sh` and shading patterns: axial and radial shadings as gradients, their
  functions sampled into stops, a run between two stops halved until, probed
  a quarter, half and three quarters along, it strays at most half an sRGB
  level from its function — at most eight halvings and 256 stops — and a
  sharp step where stitched pieces meet kept as two stops. Other shading
  types, and tiling patterns (an uncoloured one painted in its colour), are
  recorded as lost.
- Images and inline images: every colour space at 1, 2, 4, 8 and 16 bits,
  `/Decode`, stencils in the fill colour, `/Mask` stencils and colour keys,
  `/SMask` soft masks with `/Matte`, and grey and RGB JPEGs through
  `lib/image`, a JPEG's decode array and colour key read from the samples it
  decodes to, kept with their bytes where those alone are the picture (an EXIF
  orientation is undone: the page places a picture). A stencil short of its
  samples paints nowhere past them, inverted or not. Samples become colours
  through a table of every colour they can spell where that is no larger than
  the image, and otherwise a colour at a time, a fixed memo keeping those met
  so a tint transform is not run, nor charged, for them again. An image object
  is decoded once however often painted, a stencil's samples once in every
  colour it is painted in, and an inline image is found and decoded once where
  it lies. Only an inline image's colour space may be a resource's name, as
  every reader reads them; an unfiltered inline image's data is as long as its
  samples in that space, and a filtered one's, or one whose size cannot be
  read, ends at the first `EI` that stands alone before content. CMYK JPEG,
  JPEG 2000, JBIG2 and CCITT pictures are recorded as lost.
- Optional content the default configuration hides — groups, membership
  dictionaries and visibility expressions — paints nothing. Each expression
  object is read once, whatever it came to, and judged where it is met
  against the 16 levels an expression may nest and the 4096 expressions one
  is built of; what an object governing content comes to is found once,
  however much content it governs, and so is what a name in a resources
  dictionary governs. Every membership tested and expression met is charged
  to the document's evaluation steps.
- Text is not yet read: a page showing any records it as lost.
- Damaged content is passed over and recorded; annotations are recorded as
  lost.

## Containment bounds

Fixed validation bounds on untrusted input, never capacities: a PDF past one
is refused as `PdfError::TooComplex`. Every copy the reader makes of what the
file holds — an object, a name, a list — takes an allocation that can be
refused, so a file cannot abort the reader by size. Arrays and dictionaries nest at most
64 deep; a cross-reference places at most 8 388 607 objects, an object stream
holds at most 100 000, and at most 4096 updates are followed; the page tree
nests at most 64 deep and lists at most 100 000 pages and 200 000 kids; a
stream chains at most 8 filters; a page runs at most 4 000 000 operators and
reads at most 256 MiB of content, a form's again each time it is painted; its
display list holds at most 1 048 576 items and, with the path being built,
2 000 000 path points; forms nest at most 16 deep, and colour spaces 4; an
image is at most 65 536 pixels a side; a page
box coordinate is at most 10⁶ points from the origin and a user unit at most
75 000; a function has at most 32 outputs and is built of at most 4096
functions, one it shares counted each time it is met; a sampled function has
at most 8 interpolated inputs, a stitching function 256 pieces nested 8 deep
and a calculator program 65 536 operators; a gradient has at most 256 stops;
and a document's functions and optional content tests take at most 2³⁰
steps to read and evaluate, its pages together.

## Fuzzing

`tests/fuzz_pdf.rs` reads a page of every kind of content, a document of
streams and an encrypted file, unmutated first and then with bytes flipped
and cut short, rich pages whose content is generated from the operator
alphabet with hostile bytes spliced in, and noise behind a header: reading
never panics.
