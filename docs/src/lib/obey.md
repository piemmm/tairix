# `tairix-obey`

RISC OS's command-line machinery as pure functions over a variable store. The
RISC OS emulator's kernel runs it against the guest's variables
(`plans/RISCOS-EMULATOR.md` §4.4). `lib/icon` reads an application's `!Boot`
through its read-only mode, and the emulator's application registry is to do
the same (RO32).

The behaviour is RISC OS 5's, as its PRMs state it and as genuine RISC OS 5.30
answers what they leave unstated. `tests/riscos5/cases.txt` asks a machine
each such behaviour; `cargo xtask riscos --reference` runs the cases on RPCEmu
and records the answers in `tests/riscos5/answers.txt`, with the RISC OS
version and the ROM image's digest; and `tests/riscos5.rs` runs every case
through the crate, in order on one store, holding it to every answer. Each of
the crate's errors stands for one RISC OS error number throughout.

Where RISC OS 5.30 never returns, the reading is refused instead, with
`BufferOverflow`: an expression of more than 1022 characters once its
references are expanded, and a quoted string in one whose references expand
past 1023 bytes. Those cases are not asked.

## Variables

`Variables` is the store: `lookup`, `assign` and `delete`. Each matches a name
case-blind, folding ASCII and Latin-1 letters (`name::upper`). A pattern's `*`
stands for any run of characters and `#` for any one. A pattern that matches
several variables means the first in sort order (`name::compare`), a name
before every name it begins. `delete` takes that first match alone, as
`OS_SetVarVal` does; `*Unset` takes every match.

`Value` is what an expansion reads:

- a **string**, delivered as it is;
- a **number**, written in signed decimal;
- a **macro**, read again as GSTrans reads text.

`vars::set` is `OS_SetVarVal`. Each `Request` is one of its types:

| Request | What it stores |
|---|---|
| `String` | the text GSTrans'd to its terminator, quotes and escapes read, before the old value is replaced |
| `Number` | the number |
| `Macro` | the text as written, to its first control character, which must be CR, LF or NUL |
| `Expanded` | the expression's value: an integer, or a literal string |
| `Literal` | the bytes as they are |

`vars::read_expanded` is `OS_ReadVarVal` asked for a string: a number in
decimal, a macro GSTrans'd, as a path variable is read. `Store` holds
variables in host memory, bounded in count and in value length for text that
cannot be trusted.

## GSTrans

`gstrans::Reader` begins a string as `OS_GSInit` does: leading spaces are
skipped, and a leading `"` opens a quoted string. It continues one character
a call, as `OS_GSRead` does. `gstrans::gstrans` reads a whole string into a
buffer and stores the terminator after it. A result needs one byte more than
its length, so a buffer exactly the result's length overflows. Where the
string ends inside a variable's value, as a quoted string a macro closes
does, R0 lies in that value, not the text, and `Translation::next` is `None`.

- **Flags.** `SPACE_ENDS`, `NO_ESCAPES` and `NO_QUOTES` are bits 29 to 31 of
  R2.
- **Escapes.**
  - `|` before `@`, a letter or one of `[\]^_{}~` gives its control code,
    before `` ` `` 31, and before `?` DEL.
  - `|` before `|`, or before any byte from space to `>` but `!`, gives that
    byte itself, so `|"` and `|<` are a quote and a bracket. Before a top-bit
    byte it gives the byte with bit 5 flipped.
  - `|!` sets bit 7 of whatever character comes next, however it is made.
  - `|` before a control character, the string's end among them, is
    `BadString`.
- **References.**
  - `<n>` in any of `OS_ReadUnsigned`'s forms is the character code `n`.
    `OS_GSRead` answers the whole number; a buffer stores its low byte.
  - Any other `<name>` up to the first `>` names a variable, wildcards
    allowed, and one never set expands to nothing.
  - A `<` with no `>` before a space, a control character or 255 bytes is
    itself.
- **Bounds.**
  - At most 127 expansions are active at once; one more expands to nothing.
  - Every reference spends one unit of the reading's `Budget`, a wildcard one
    for each variable it may examine. One spent past it is
    `ExpansionTooComplex`, so no text can ask for unbounded work.
  - A `Store` holds at most so many variables, values and names of at most so
    many bytes, so matching a wildcard against a name is cheap; a wildcard
    with more literal bytes than a name may hold matches none unexamined.
  - `ExpansionTooComplex` and `SystemHeapFull` reach the caller as
    themselves wherever they arise: no RISC OS error stands in for them.

## Expressions

`expr::evaluate` is `OS_EvaluateExpression`.

- **Expansion first.** References outside quotes are expanded, to at most
  1022 characters. Inside a quoted string they expand as part of the string,
  where `|` is ordinary.
- **Items.** Numbers take `OS_ReadUnsigned`'s forms and end at the first byte
  that is not a digit of their base, so `2AND3` is 2 AND 3. Operator words are
  upper case and need nothing after them, so `ORDER` is `OR` and the name
  `DER`. A bare name is a variable: a number an integer, a string or macro a
  string, and one never set is `UnknownOperand`.
- **Operators**, by priority, from loosest to tightest:

  | Operators | Associativity |
  |---|---|
  | `OR`, `EOR` | left |
  | `AND` | left |
  | `= <> < > <= >= << >> >>>` | left |
  | `+ -` | left |
  | `* / MOD` | left |
  | prefix `+ -`, `NOT`, `LEN`, `STR`, `VAL`; `LEFT`, `RIGHT` | right |

- **Integer conversion.** A string where an integer is wanted must be one,
  signed, with spaces about it, and an empty string is 0. `+` joins two
  strings and adds otherwise.
- **Division.** `/` truncates toward zero, and `MOD` takes the dividend's
  sign.
- **Shifts.** A count past 31 clears the value or leaves only its sign, and a
  negative count shifts the other way.
- **`LEFT` and `RIGHT`.** They compare their count unsigned, so a negative
  count keeps the whole string.
- **Errors.** Every item goes onto one stack of values and operators, applied
  by the priorities above, so a malformed expression gives the error its
  shape does: `1 2` is a missing operand, `1 2+3` a missing operator. The
  stack is bounded as RISC OS 5.30's is: an entry counts 8 bytes and a string
  its bytes besides, rounded up to a word, and the stack overflows on
  reaching 15 160. Sweeps of nested strings at depths from 13 to 50 pin both
  figures to the word.

## Arguments and command lines

- **`args::substitute`** is `OS_SubstituteArgs32`.
  - `%0`–`%9` stand for the arguments as written, quotes kept, and `%*n` for
    the rest of the line from argument `n`.
  - Unless the rest was taken, or appending is refused, the arguments past
    the highest named are appended, with nothing between.
  - A quote left open takes the CR, LF or NUL it meets as its own last byte,
    and the list goes on after it.
  - The result, its terminator counted, leaves a byte of its buffer spare.
- **`readargs::read_args`** is `OS_ReadArgs`.
  - It returns the buffer as the call leaves it, a word per entry and then the
    values in the order they were given, each laid where the one before
    ended, and each entry's argument: a text or `/G` argument as where the
    buffer holds it.
  - Each `/E` and `/G` value's text is copied to its place with a NUL after
    it first, and the value laid over the copy, so a shorter value leaves the
    copy's tail. `/E` values are a type byte, then an integer or a length and
    bytes; an integer is counted as taking eight bytes of room though five
    are written. `/G` values are a length, then bytes, then a terminator the
    next value is laid over.
  - A keyword is its whole name, or its first letter alone; abbreviated
    switches run together, `-abc`.
  - A `-word` that names no entry, first on the line, gives the first entry
    a positional item could take an empty value and ends the reading;
    anywhere else it is refused.
  - Its work is linear in the template and the line: positional items fill
    entries from a cursor that never moves back, and a `-word` is found in
    the template's names sorted once, an abbreviation by its first two
    letters.
- **`command::read_line`** reads one command line as data, as `OS_CLI` begins
  to run it.
  - A line whose terminator does not come within 1024 bytes is
    `LineTooLong`.
  - Of its commands it reads only `Set`, `SetMacro`, `Unset`, `If`, which
    nests, and `IconSprites`, which it reports; each refuses parameters its
    syntax does not take with `Syntax`. `Unset` deletes every variable its
    pattern matches.
  - A line that opens a redirection (`{ > file }`, `{ < file }`), names its
    command by an alias or an abbreviation, or runs any other command, changes
    nothing and fails nothing: what it would do is not this reading's to
    know.
- **`command::choose`** is `If`'s choice. The condition ends at the first
  space before `Then`, quotes or no quotes, and must be an integer.
- **`command::read_only`** reads an Obey file as data over any `Variables`
  store, with its parameters and with `Obey$Dir` set first.
  - Its lines end at a CR, an LF or a NUL. Each has the parameters put in,
    with none appended, in a line's room, then is read as `read_line` reads
    one.
  - It runs nothing, and touches nothing but the variables it is given.
  - The reading ends at the first line that fails, with that error, as
    `*Obey` abandons a file at its first: what it set and reported up to then
    is the file's lines before it.

## The crate's own bounds

- An `If` nests at most 32 deep; a deeper one is `ExpansionTooComplex`.
- The `Budget` and the `Store`'s bounds above.

## Tests

- **Replay.** `riscos5` holds the crate to every answer genuine RISC OS 5.30
  recorded: GSTrans and GSRead, every escape and reference form, expressions
  and their errors, `OS_ReadArgs`' buffers, substitution, each variable type
  set and read, `OS_ReadUnsigned`, and command lines and Obey files by how
  they fail and the variables they leave.
- **Units.** Every escape, reference form and variable type; the error each
  malformed shape gives; `OS_ReadArgs`' buffer bytes against the layout; and
  the read-only mode's refusals.
- **Fuzzing.** `fuzz_obey` sends hostile strings, expressions, templates,
  variable reads and Obey files through every entry and holds each to its
  bound, over a store whose lookups and deletions count the variables they
  examine, long names and wildcards among them: no reading examines more
  than its budget allows. It also holds two exact oracles:
  - any bytes, written in GSTrans's escapes, GSTrans back to themselves;
  - a fully bracketed integer expression evaluates as wrapping 32-bit
    arithmetic does.
