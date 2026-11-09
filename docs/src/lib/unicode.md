# `tairix-unicode`

The Unicode character properties that showing text from elsewhere needs: a
name a foreign volume carries, a host a network announces, a title an
application sets. The policy that uses them — what a surface draws for such
text — is [`tairix-visible`](./visible.md)'s; this crate answers only what the
Unicode Character Database and UTS #39 (Unicode Security Mechanisms) say about
a character or a string, and runs the algorithms over them: normalization and
SASLprep.

## What it answers

- **`is_hidden(ch)`** — the characters that draw nothing yet hide, reorder or
  break the text beside them: the C0 and C1 controls, U+2028 and U+2029, the
  interlinear-annotation controls U+FFF9–U+FFFB, and every
  `Default_Ignorable_Code_Point` (the bidirectional controls, zero-width
  characters, fillers, variation selectors and tags). `lib/vt`'s print encoder
  and `lib/curses`' cells replace by it, as does `shown`.
- **`MarkRun`** — UTS #39 §5.4's checks on a combining sequence: at most
  `MAX_MARKS` (four) nonspacing marks, and none that an earlier character of
  the sequence already carries, since a repeated mark draws over itself. Both
  are judged over the canonical decomposition, as the standard specifies, so
  `é` followed by U+0301 repeats its acute. A character's own decomposition
  may repeat a mark — Gurung Khema's vowel sign U is two vowel signs AA — as it
  draws as the single glyph it encodes.
- **`Faithful`** — the lossless twin of the display policy, for a rendering
  that escapes a character rather than replacing it: each hidden character,
  and each mark that breaks its sequence's `MarkRun` limits. The log renderer,
  `lib/util`'s quoting and report escaper and `lib/net`'s `DisplayLabel` each
  escape by it, in their own spelling.
- **`restriction_level(text)`** — UTS #39 §5.2's restriction level over the
  `Script_Extensions` of each character, augmented as §5.1 specifies (Han with
  the Japanese, Korean and Chinese writing systems, Hiragana and Katakana with
  Japanese, Hangul with Korean, Bopomofo with Chinese) and judged against UAX
  #31's Recommended Scripts. `MinimallyRestrictive` — Latin with Cyrillic or
  Greek, among other mixtures — is what the file manager's network view marks (`lib/browse`'s `NetworkEndpoint::mixes_scripts`). Levels 5
  and 6 coincide, as no identifier profile applies to a name that is shown
  rather than parsed.

## Normalization

**`normalize(input, form, out)`** puts text in one of UAX #15's four forms —
`Form::Nfd`, `Nfc`, `Nfkd` or `Nfkc` — writing it into a buffer the caller
holds and answering how many characters it takes there, or `Overflow` exactly
where the result is longer than that buffer: a buffer as long as the result
suffices, and as no character normalizes to more than `MAX_DECOMPOSITION`
characters (18, U+FDFA), one that many times the input's length always does.
Each character is decomposed through its full decomposition, precomputed —
Hangul syllables by the standard's arithmetic — and each part takes its place
in canonical order as it lands and, in NFC and NFKC, joins the starter before
it at once where nothing between blocks the two, so a combining sequence
never takes more room than it composes to. A mark that follows a mark of a
higher class is moved before them, and in NFC and NFKC its sequence is
decomposed and composed again in its new order; meanwhile the sequence may
run past the buffer by at most the characters one starter can absorb, which
the normalizer holds itself, so `Overflow` stays exact. The work is linear in
the input but for those marks, each costing work in proportion to its
sequence, so a long run of marks out of canonical order costs time quadratic
in its length. The whole of `NormalizationTest.txt` holds: every case in all
four forms, each into a buffer exactly as long as its result and refused by
one a character shorter, and every code point its first part does not list
unchanged by each.

## SASLprep

**`saslprep(input, unassigned, out)`** prepares a user name or password by
RFC 4013's stringprep profile, so two spellings of one password hash alike: the
characters RFC 3454 table B.1 maps to nothing are dropped, the non-ASCII spaces
(table C.1.2) become U+0020, the text is normalized to NFKC, and it is refused —
`SaslprepError` — where it holds a character the profile prohibits (tables
C.1.2, C.2 and C.3 to C.9), right-to-left text that breaks RFC 3454 §6's rules,
or, for `Unassigned::Refused` (a string to be stored), a code point Unicode 3.2
does not assign; `Unassigned::Allowed` (a query) passes one through untouched.
Like `normalize`, it needs a buffer only as long as its result. The few
characters a normalization holds past the buffer while it puts marks in order
are wiped as it ends, so a caller wiping its own buffer leaves no copy of a
password behind.

U+200B ZERO WIDTH SPACE is the one character both table B.1 and table C.1.2
list. RFC 3454 does not say which mapping comes first, and implementations
differ; here it is mapped to nothing, so `a`, U+200B, `b` prepares to `ab`,
not `a b`.

Stringprep is fixed to Unicode 3.2 so that a prepared string never changes.
The preparation reads this release's tables restricted to 3.2's repertoire: a
code point 3.2 does not assign (table A.1) has no decomposition, class or
composite, and the five decompositions Corrigendum #4 corrected after 3.2 are
3.2's own (`NormalizationCorrections.txt`), so an implementation that applies
the corrections prepares those five otherwise. A character 3.2 assigns
otherwise takes this release's decomposition, combining class and composition
exclusions, and the generator refuses tables in which one decomposes or
composes to a code point 3.2 does not assign. The tests hold the preparation
to RFC 4013's examples, to each step of RFC 3454 on characters chosen from its
tables, and to the repertoire on a code point and a combining class assigned
since 3.2 and on the corrections.

## Tables

`src/tables.rs` is generated by `cargo xtask unicode --write` from the UCD
files in `lib/unicode/assets/` — Unicode 18.0.0, each byte-identical to its
unicode.org release and pinned by SHA-256 in the generator. `cargo xtask
unicode`, run by `ci`, regenerates in memory and fails closed on drift, and
refuses assets whose `UnicodeData.txt` and `DerivedGeneralCategory.txt`
disagree on the marks, so a snapshot mixed from two releases cannot be taken.
The tables are sorted and searched by bisection: the script of every code
point, the explicit `Script_Extensions` and their distinct sets, the
nonspacing marks, the default ignorables, the nonzero combining classes, the
full canonical decomposition of each character with a canonical mapping and,
apart from them, the full compatibility decomposition of each character whose
mapping is a compatibility one — a character with a canonical mapping
decomposes by compatibility through its canonical parts — the characters that
follow another in a primary composite, which spare every other character the
composites' search, the primary composites — every canonical pair but the
composition exclusions, the singletons and the non-starter decompositions —
Unicode 3.2's five corrected decompositions, and the stringprep tables
SASLprep reads, taken from `rfc3454.txt` between its table markers, its page
furniture passed over and any other line refused. `MarkRun` reads the marks a
character carries from the canonical decompositions alone, so a binary that
only escapes text links none of the compatibility data. The generator refuses
a mapping the runtime could not follow: one into a Hangul syllable, a
canonical one longer than two characters, or a compatibility one on a
combining mark.

The crate is `no_std`, allocates nothing and forbids `unsafe`, so the
allocation-free log and the TUI layers link it. `tests/fuzz_unicode.rs` holds
normalization and SASLprep to their invariants over arbitrary text: neither
panics, the bound is never overflowed and a shorter buffer is said to be,
every form is idempotent and agrees with the others, and a prepared string
prepares to itself.

## Licences

The Unicode Character Database files, and the tables generated from them, are
Copyright © 1991-2026 Unicode, Inc., under the Unicode License V3
(`lib/unicode/assets/LICENSE`). The stringprep tables are taken from RFC 3454,
which lets a work derived from it be distributed only carrying its copyright
notice. `lib/unicode/assets/LICENSE` and the generated `src/tables.rs` carry
it, `cargo xtask unicode` refuses a `LICENSE` that does not, and this page
carries it too:

> Copyright (C) The Internet Society (2002).  All Rights Reserved.
>
> This document and translations of it may be copied and furnished to
> others, and derivative works that comment on or otherwise explain it
> or assist in its implementation may be prepared, copied, published
> and distributed, in whole or in part, without restriction of any
> kind, provided that the above copyright notice and this paragraph are
> included on all such copies and derivative works.  However, this
> document itself may not be modified in any way, such as by removing
> the copyright notice or references to the Internet Society or other
> Internet organizations, except as needed for the purpose of
> developing Internet standards in which case the procedures for
> copyrights defined in the Internet Standards process must be
> followed, or as required to translate it into languages other than
> English.
>
> The limited permissions granted above are perpetual and will not be
> revoked by the Internet Society or its successors or assigns.
>
> This document and the information contained herein is provided on an
> "AS IS" basis and THE INTERNET SOCIETY AND THE INTERNET ENGINEERING
> TASK FORCE DISCLAIMS ALL WARRANTIES, EXPRESS OR IMPLIED, INCLUDING
> BUT NOT LIMITED TO ANY WARRANTY THAT THE USE OF THE INFORMATION
> HEREIN WILL NOT INFRINGE ANY RIGHTS OR ANY IMPLIED WARRANTIES OF
> MERCHANTABILITY OR FITNESS FOR A PARTICULAR PURPOSE.
