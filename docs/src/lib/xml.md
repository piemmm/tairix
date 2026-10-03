# `tairix-xml` — the XML element scanner

`lib/xml` reads an XML document into its elements, in order, each with its
attributes and its interleaved character data. Comments, processing
instructions and the doctype are dropped; character references and the five
predefined entities are decoded; a CDATA section is taken verbatim.

## Namespaces

A reader names its own namespace. An element with no prefix, or whose prefix
is bound to that namespace, is named by its local name; an element in any
other namespace keeps its prefixed name, so it matches nothing the reader
looks for. The SVG decoder reads with the SVG namespace; OpenRaster's
`stack.xml` has none.

## Bounds

The scanner refuses rather than panics: an unterminated tag, quote, comment
or element, or a close tag that does not match, is `XmlError::Malformed`; a
document with no element is `MissingRoot`. Nesting deeper than `MAX_DEPTH`
or more than `MAX_ELEMENTS` elements is `TooComplex` — fixed defences
against a hostile document, not capacities.

## Readers

- `lib/svg`, through its `xml` adapter, which fixes the namespace and maps
  each refusal to its own error.
- `lib/image`'s OpenRaster reader, for the layer stack.

A fuzz harness (`tests/fuzz_xml.rs`) feeds it mutated documents and noise.
