# tairix-der

Stability tier: **experimental**.

The one DER codec (ITU-T X.690 §10): SPNEGO (RFC 4178) and Kerberos
(RFC 4120) read and write their ASN.1 through it rather than each embedding
a parser.

## API

- `Reader` — zero-copy reading: `tlv` for any element, `element` and
  `optional` for an expected identifier, `nested`/`sequence`/`explicit` (and
  their `_optional` forms) to read a constructed element's contents whole, and
  the typed readers `integer`, `enumerated`, `boolean`, `null`,
  `octet_string`, `general_string`, `oid`, `bit_string` and
  `generalized_time` (into a `Time64`).
- `Writer` — builds one encoding; the same typed writers, `constructed`,
  `sequence` and `explicit` around a closure, and `raw` for an element
  carried verbatim. A failure latches and `finish` reports it.
- `Tag` and `Class` — identifiers of every class and number.
- `DerError` — why an encoding was refused.

## Strictness

Every value has one encoding, and only that one is read: indefinite and
non-minimal lengths, padded integers, booleans other than `00`/`FF`, bit
strings with stray padding bits, non-minimal identifiers and object-identifier
arcs, and GeneralizedTime in any form but `YYYYMMDDHHMMSS[.f]Z` (no trailing
fraction zero, a valid calendar date) are refused as `NonCanonical`. Nesting
past `MAX_DEPTH` (32) is `TooDeep`, and an element that does not fit inside
what encloses it is `Truncated`.

`no_std` + `alloc`, `#![forbid(unsafe_code)]`. Fuzzed by `tests/fuzz_der.rs`,
which also holds every accepted encoding to re-encode byte for byte.
