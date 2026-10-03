# tairix-xml

Stability: **experimental**.

The shared, fail-closed XML element scanner. It yields a document's elements
in order — each with its attributes and its interleaved character data —
and drops comments, processing instructions and the doctype. An element in
the namespace its reader names, or with no prefix, is named by its local
name; any other keeps its prefix.

Structural damage is refused, never a panic, and a document's nesting and
element count are held to fixed bounds (`MAX_DEPTH`, `MAX_ELEMENTS`): these
are defences against hostile input, not capacities.

Readers: `lib/svg` (the SVG decoder) and `lib/image` (OpenRaster's
`stack.xml`). See `docs/src/lib/xml.md`.
