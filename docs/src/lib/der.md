# `tairix-der` — the DER codec

`lib/der` reads and writes ASN.1's Distinguished Encoding Rules (ITU-T X.690
§10). SPNEGO (RFC 4178) and Kerberos (RFC 4120) are its consumers; both speak
DER, and both are spoken by strangers on the network, so there is one strict
codec rather than a parser per protocol.

## Reading

`Reader` borrows its input and returns contents as slices of it. `tlv` reads
any element — its `Tag`, contents and whole encoding. `element(tag)` requires
an identifier, and `optional(tag)` reads one only when it is next, which is
how an `OPTIONAL` field is read. `nested(tag, read)` hands a constructed
element's contents to a closure on a reader one level deeper and requires it
to consume them exactly; `sequence` and `explicit(n, …)` are its two common
forms, and `peek_tag` decides a `CHOICE`. The typed readers cover `INTEGER`
and `ENUMERATED` (into `i64`), `BOOLEAN`, `NULL`, `OCTET STRING`,
`GeneralString`, `OBJECT IDENTIFIER` (its encoded arcs, compared as bytes),
`BIT STRING` and `GeneralizedTime`, which is read into a `Time64` through the
ABI's one calendar (`CivilTime`).

## Strictness

A value has one DER encoding, and the reader accepts no other: indefinite and
non-minimal lengths, padded integers, booleans other than `00`/`FF`, bit
strings with stray padding, non-minimal identifiers and arcs, a constructed
string, and a GeneralizedTime other than `YYYYMMDDHHMMSS[.f]Z` naming a real
date are all `DerError::NonCanonical`. That makes the encoding of an accepted
structure a function of its value, which is what lets the fuzz harness hold
every accepted input to re-encode byte for byte.

Nesting is bounded at `MAX_DEPTH` (32), a security bound on a stranger's
structure — Kerberos's deepest message nests about a dozen levels — and an
element claiming more than what encloses it is `Truncated`.

## Writing

`Writer` appends one encoding. `constructed(tag, write)` writes the contents
in place and inserts the length once it is known, so nesting costs no
allocation per level. `raw` carries an element encoded elsewhere — a ticket
copied into a request — after checking it is exactly one well-formed element.
A failed write latches, and `finish` returns the encoding or the first
failure, so a message is built with plain calls and checked once.

## Tests

Unit tests cover each type's canonical form and its refusals, identifiers of
every class including the multi-byte form, lengths at every boundary,
`MAX_DEPTH`, and an SPNEGO `negTokenResp`. `tests/fuzz_der.rs` reads noise,
mutated encodings and splices, and re-encodes whatever it accepts.
