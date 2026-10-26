# tairix-xml

Stability: **experimental**.

The shared, fail-closed XML element scanner. It yields a document's elements
in order — each with its attributes, its interleaved character data and the
namespace its name resolves to — and drops comments, processing instructions
and the doctype. An element in the namespace its reader names, or with no
prefix, is named by its local name; any other keeps its prefix. `Scope`
resolves a qualified name carried in text.

Structural damage and an attribute given twice are refused, never a panic,
and a document's nesting, element count and attributes per element are held
to fixed bounds (`MAX_DEPTH`, `MAX_ELEMENTS`, `MAX_ATTRIBUTES`): these are
defences against hostile input, not capacities.

Readers: `lib/svg` (the SVG decoder), `lib/image` (OpenRaster's
`stack.xml`) and `lib/net` (WS-Discovery). See `docs/src/lib/xml.md`.
