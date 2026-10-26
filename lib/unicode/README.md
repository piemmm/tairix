# tairix-unicode

Stability tier: **experimental**.

The Unicode character properties showing text a stranger authored needs,
answered from tables generated out of the Unicode Character Database. The
display policy built on them is `tairix_visible` (`Shown`, `shown_chars`);
this crate holds only the properties and the UTS #39 (Unicode Security
Mechanisms) checks.

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
- `VERSION` — the Unicode version the tables describe.

## Provenance

`assets/` holds `Scripts.txt`, `ScriptExtensions.txt`,
`PropertyValueAliases.txt`, `DerivedGeneralCategory.txt`,
`DerivedCoreProperties.txt` and `UnicodeData.txt` byte-identical to
`https://www.unicode.org/Public/18.0.0/ucd/`, under the Unicode License V3
(`assets/LICENSE`). `cargo xtask unicode` pins each by SHA-256, checks that
`UnicodeData.txt` and `DerivedGeneralCategory.txt` agree on the marks, and
fails closed when `src/tables.rs` differs from a fresh generation.

Taking a new release: replace the assets, their pins in
`tools/xtask/src/commands/unicode.rs`, its `VERSION` and its `RECOMMENDED`
list (UAX #31 Table 5) together, then run `cargo xtask unicode --write`. When
the shape of the tables changes, the generator's own build links this crate,
so give `src/tables.rs` the new items empty first.

`no_std`, no allocation, `#![forbid(unsafe_code)]`.
