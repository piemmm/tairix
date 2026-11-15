# `tairix-bbcbasic`

BBC BASIC's tokens and the tokenising and listing of its programs, for BASIC
II and for BASIC V and VI, which tokenise and list alike. OpenMicro's BASIC
ROM is built from its BASIC II tables. The RISC OS emulator's BASIC (RO20) and
TextEdit (RO49) are to use its BASIC V half (`plans/RISCOS-EMULATOR.md` RO7).

BASIC II's rules are its ROM's, as OpenMicro's clean-room BASIC keeps them;
the two are tested against each other by typing lines into the emulated
machine. BASIC V's are RISC OS 5's, read from its Apache-2.0 BASIC sources.

## Keywords

`keyword::word` holds the 113 keywords both dialects spell and number alike,
each a `Word<u8>`: its spelling and its one-byte token. `basic2` adds BASIC
II's eight commands, `&C6` (`AUTO`) to `&CD` (`SAVE`). `basic5` adds BASIC V's
single-byte tokens (`OTHERWISE` `&7F`, `WHEN` to `ENDWHILE` `&C9`–`&CE`) and
its two-byte ones: a prefix, `&C6` for functions, `&C7` for commands and `&C8`
for statements, then a byte from `&8E`.

Each dialect's `KEYWORDS` is its tokeniser's table: every keyword in the order
it is tried, each with its `Flags`. The table keeps each first letter's
keywords together, which is checked when the crate is built. BASIC II's is a
`Keyword<u8>` table, every token one byte. BASIC V's is a `Keyword<Token>`
table. `SPELLINGS` gives each token's spelling for listing: the first keyword
the table gives it, so `&FB` is `COLOUR` and never `COLOR`, and a
pseudo-variable's assigning token is spelt as the variable. BASIC V also spells
two tokens its tokeniser never makes but a program holds: `&CC`, a stored line's
first `ELSE`, and `&C8 &9A`, `INSTALL` as a statement.

## Tokenising

`tokenise::tokenise` takes a line from a typed line's start (`Start::Line`),
where a number is a line number, from a statement's start, or from an
expression's middle. In both dialects:

- At a capital letter the keywords are tried in table order, and the first
  the text spells whole, or begins and then stands at a `.`, is taken: `P.` is
  `PRINT`. A *conditional* keyword (`TIME`, `END`, `PI` …) followed by a
  letter, digit, `_` or `£` (`` ` ``) is not spelt whole, so `TIMER` is a
  name.
- A pseudo-variable (`PTR`, `PAGE`, `TIME`, `LOMEM`, `HIMEM`) at a
  statement's start takes its assigning token, `&40` higher.
- A typed line's own number, and the numbers after `GOTO`, `GOSUB`, `THEN`,
  `ELSE`, `RESTORE`, `TRACE` and the line-numbered commands, become line
  references while below the dialect's limit, and are left as typed past it.
- Strings, the text after `REM` and `DATA` (and BASIC V's `EDIT`), a star
  command at a statement's start and a hex number's digits are left as typed.
  So are the names after `FN` and `PROC`.

Where the dialects differ:

| | BASIC II | BASIC V |
|---|---|---|
| an abbreviation of a conditional keyword | taken unchecked: `X=TI.X` is `TIME` and `X` | checked as a whole keyword is |
| a conditional keyword turned down | later entries are tried | its text is a name |
| line numbers stop at a keyword | that takes none | after which a statement starts or goes on |
| a line number's limit | below 65 536 | below 65 280, with a warning past it |
| hex digits | capitals only | either case |
| a line's end | CR | CR or LF |
| `TRACE` in an expression | takes a line number | is the trace file's handle and takes none |
| `VDU` spelt whole before `P.` | `VDU PRINT` | `VDU` and the text `P.`, as an assembler mnemonic |

BASIC V also reports `Warnings`: brackets that do not pair (a keyword's own
`(` counted), a line number left as typed, and a line ending inside a string.

## Line references

A line number in a line's text is `&8D` and three bytes, each `&40`–`&7F`, so
none reads as a token, a terminator, a quote or a digit (`lineref`). The first
holds the top two bits of the number's low and high bytes, flipped, the others
each byte's low six.

## Listing

`list::Lister` lists a program a line at a time under `LISTO`. It carries the
depth of the structures open from each line to the next. Each line it gives
ends with an LF, which a screen shows as LF and CR.

BASIC II puts the number in five columns, then:

- `LISTO 1`: a space;
- `LISTO 2` and `4`: two spaces for each `FOR` loop and each `REPEAT` loop open
  before the line.

The loops are counted a byte each, a closing keyword with none open closing
none, and their indent is summed and doubled in a byte, as the ROM does. A byte
with its top bit set lists as a keyword wherever it stands, inside a string
too.

BASIC V gives:

- `LISTO 1`: a space after the number;
- `LISTO 2`: an indent of the lesser depth, before the line or after it;
- `LISTO 4`: each statement on a line of its own, split at colons outside
  strings and `REM`;
- `LISTO 8`: no numbers, the listing stopping at a line reference with
  `ReferenceListed`;
- `LISTO 16`: keywords in lower case.

For the depth, `FOR`, `REPEAT`, `WHILE` and `CASE` open, a line ending at
`THEN` opens, and `NEXT`, `UNTIL`, `ENDWHILE`, `ENDCASE` and `ENDIF` close.
None is counted inside a string or after `REM`. The depth is kept in a word, a
negative one counting from zero again at the next line, and indents by its low
byte, as RISC OS 5 prints a count of spaces. RISC OS 5 knows a two-byte opener
by the byte before its second, which before a line's first byte is the line's
length, and so does this lister. Strings and `REM` text list as they are. A
token BASIC V has no spelling for lists as nothing, where RISC OS 5's lister
reads past its table.

## Program images

A program image holds, from PAGE, each line as a CR, its number high byte
first, its length and its text, in order of number, then a CR and `&FF`.
`program::lines` reads an image's lines, ending with `BadProgram` at the first
that does not chain to the next. `program::image` makes an image, refusing
lines out of order, numbered `&FF00` or above, or holding more than 251 bytes
of text. `program::last_line` is each dialect's highest line number, 32 767 and
65 279.

`program::stored` is a typed line's text as it is kept once its number is
taken off. BASIC II's is as tokenised. BASIC V's loses its leading spaces where
`LISTO` is not 0, then its trailing ones back to, but never past, its first
byte, and its first `ELSE` becomes `&CC`. An empty text deletes its line before
this is asked, and a text this leaves empty is kept as an empty line, as RISC
OS 5 keeps it.

## Bounds and failure

Every allocation is fallible and refused as `NoMemory`. A tokenised line takes
at most four bytes for each byte read, a line reference being the most a digit
becomes. A listed line takes at most a fixed amount for each byte of its text,
however deep the structures before it. The `fuzz_bbcbasic` harness holds both
bounds over hostile text, tokens and images in both dialects.
