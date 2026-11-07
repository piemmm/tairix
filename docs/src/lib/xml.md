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

Every element also carries the namespace its name resolves to — through its
prefix, or for an unprefixed name the default namespace `xmlns="…"` in force —
so a reader of a document mixing several, as SOAP does, matches each element
with `Element::is(namespace, local)` whatever prefixes the author chose. An
unbound prefix, an undeclared default and a namespace name written with a
character reference all resolve to no namespace, which matches nothing a
reader looks for. A qualified name carried in text, such as WS-Discovery's
`Types` list, is resolved with a `Scope`, entered element by element as the
reader walks down, which reads each element's own declarations rather than
copying the bindings onto every element.

## Bounds

The scanner refuses rather than panics: an unterminated tag, quote, comment
or element, a close tag that does not match, an attribute given twice, or a
name with an empty prefix or local part is `XmlError::Malformed`; a document
with no element is `MissingRoot`. A reader names the bounds it holds a
document to (`Limits`): `parse` reads under `Limits::COMPACT` — `MAX_DEPTH`
deep, `MAX_ELEMENTS` elements — for an asset, a manifest or a protocol
message, and `parse_with` under any other, `Limits::DOCUMENT` being what a
document opened to edit is read under in the parser sandbox. Past them, or
past `MAX_ATTRIBUTES` attributes on one element, is `TooComplex` — fixed
defences against a hostile document, not capacities.

## Writing

`Escaped` spells text for an attribute value or character data, the five
characters XML gives meaning to written as references: what every writer of
XML in the tree — OpenRaster's layer stack, the drawing editor's SVG — writes
text through.

## Readers

- `lib/svg`, through its `xml` adapter, which fixes the namespace and maps
  each refusal to its own error.
- `lib/image`'s OpenRaster reader, for the layer stack.
- `lib/net`'s WS-Discovery decoder, for SOAP envelopes and metadata.

A fuzz harness (`tests/fuzz_xml.rs`) feeds it mutated documents and noise.
