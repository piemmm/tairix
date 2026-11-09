# tairix-unicode

Stability tier: **experimental**.

The Unicode character properties showing text a stranger authored needs,
normalization and SASLprep, answered from tables generated out of the Unicode
Character Database and RFC 3454. The display policy built on the properties is
`tairix_visible` (`Shown`, `shown_chars`); this crate holds only the
properties, the UTS #39 (Unicode Security Mechanisms) checks and the
algorithms.

## API

- `is_hidden` — a character that draws nothing yet can hide, reorder or break
  the text around it: a C0 or C1 control, a line or paragraph separator, an
  interlinear-annotation control, or a `Default_Ignorable_Code_Point`.
- `is_default_ignorable`, `is_nonspacing_mark` (`General_Category` `Mn` or
  `Me`).
- `MarkRun` and `MAX_MARKS` — UTS #39 §5.4 over a combining sequence's
  canonical decomposition: at most four nonspacing marks, and none an earlier
  character of the sequence already carries.
- `Faithful` — which characters a rendering that escapes rather than replaces
  must escape: each hidden character and each mark past the `MarkRun` limits.
- `restriction_level` and `RestrictionLevel` — UTS #39 §5.2 over the
  `Script_Extensions` UTS #39 §5.1 augments, with UAX #31's Recommended
  Scripts.
- `normalize`, `Form` and `Overflow` — UAX #15's four normalization forms,
  written into a buffer the caller holds that need only be as long as the
  result; `MAX_DECOMPOSITION` times the input's length always is.
- `saslprep`, `Unassigned` and `SaslprepError` — RFC 4013's stringprep
  profile for user names and passwords, over Unicode 3.2 as RFC 3454 fixes it,
  into a buffer sized alike.
- `VERSION` — the Unicode version the tables describe.

## Provenance

`assets/` holds `Scripts.txt`, `ScriptExtensions.txt`,
`PropertyValueAliases.txt`, `DerivedGeneralCategory.txt`,
`DerivedCoreProperties.txt`, `UnicodeData.txt`, `CompositionExclusions.txt`,
`NormalizationCorrections.txt` and `NormalizationTest.txt` byte-identical to
`https://www.unicode.org/Public/18.0.0/ucd/`, under the Unicode License V3,
and `rfc3454.txt` byte-identical to
`https://www.rfc-editor.org/rfc/rfc3454.txt`, under the Internet Society's
copyright notice that closes it, which every work derived from the RFC must
carry. `assets/LICENSE` holds both, and the generated `src/tables.rs` carries
the RFC's notice; the crate's `license` field is the workspace's, for its own
code. `cargo xtask unicode` pins each asset by SHA-256, checks that
`UnicodeData.txt` and `DerivedGeneralCategory.txt` agree on the marks, that
each correction is the mapping `UnicodeData.txt` holds and that
`assets/LICENSE` carries the RFC's notice, refuses tables in which a
character Unicode 3.2 assigns decomposes or composes to one it does not, and
fails closed when `src/tables.rs` differs from a fresh generation.
`NormalizationTest.txt` is read only by the conformance test.

Taking a new release: replace the UCD assets, their pins in
`tools/xtask/src/commands/unicode.rs`, its `VERSION` and its `RECOMMENDED`
list (UAX #31 Table 5) together, then run `cargo xtask unicode --write`. RFC
3454 is fixed to Unicode 3.2 and never changes with a release. When
the shape of the tables changes, the generator's own build links this crate,
so give `src/tables.rs` the new items empty first.

`no_std`, no allocation, `#![forbid(unsafe_code)]`.
