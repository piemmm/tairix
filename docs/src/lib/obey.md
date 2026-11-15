# `tairix-obey`

RISC OS's command-line machinery as pure functions over a variable store. The
RISC OS emulator's kernel runs it against the guest's variables
(`plans/RISCOS-EMULATOR.md` §4.4). `lib/icon` reads an application's `!Boot`
through its read-only mode, and the emulator's application registry is to do
the same (RO32).

The behaviour is RISC OS 5's, read from its PRMs and its Apache-2.0 kernel
sources. Where those sources misbehave, the documented intent is kept instead:

- they read past the end of a string, where an unclosed quote ends an
  argument, so here the string ends;
- they crash the machine, where an expression outgrows its buffer, so here
  that expression is refused with `BufferOverflow`;
- `OS_ReadArgs` miscounts an `/E` integer's room, so here it takes the five
  bytes its block holds;
- `OS_ReadArgs` means to read a `-word` that names no entry as a value but
  cannot, so here it is refused, as RISC OS 5 refuses it on every command line.

## Variables

`Variables` is the store: `lookup`, `assign` and `delete`. Each matches a name
case-blind, folding ASCII and Latin-1 letters as the kernel does
(`name::upper`). A pattern's `*` stands for any run of characters and `#` for
any one. A pattern that matches several variables means the first in sort
order (`name::compare`), a name before every name it begins.

`Value` is what an expansion reads:

- a **string**, delivered as it is;
- a **number**, written in signed decimal;
- a **macro**, read again as GSTrans reads text.

A code variable is the store's own: a lookup answers what its read entry
gives, and an assignment hands it the value's text.

`vars::set` is `OS_SetVarVal`. Each `Request` is one of its types:

| Request | What it stores |
|---|---|
| `String` | the text GSTrans'd, quotes and escapes read, before the old value is replaced |
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
its length, so a buffer exactly the result's length overflows.

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
  - At most 127 expansions are active at once; one more expands to nothing,
    as RISC OS has it.
  - Every reference spends one unit of the reading's `Budget`. One spent past
    it is `ExpansionTooComplex`, so no text can ask for unbounded work.

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
- **Errors.** Every item goes onto one stack of values and operators, as RISC
  OS's evaluator works, so a malformed expression gives the error RISC OS
  gives: `1 2` is a missing operand, `1 2+3` a missing operator. That stack
  holds 15 160 bytes, as RISC OS's does.

## Arguments and command lines

- **`args::substitute`** is `OS_SubstituteArgs32`.
  - `%0`–`%9` stand for the arguments as written, quotes kept, and `%*n` for
    the rest of the line from argument `n`.
  - Unless the rest was taken, or appending is refused, the arguments past
    the highest named are appended.
- **`readargs::read_args`** is `OS_ReadArgs`.
  - It returns each entry's argument, and the buffer as the call leaves it: a
    word per entry, then the values in the order they were given.
  - `/E` values are a type byte, then an integer or a length and bytes. `/G`
    values are a length, then bytes.
  - A keyword's first letter abbreviates it, and abbreviated switches run
    together, `-abc`.
- **`command::choose`** is `If`'s choice. The condition ends at the first
  space before `Then`, quotes or no quotes, and must be an integer.
- **`command::read_only`** reads an Obey file as data, given no parameters,
  with `Obey$Dir` set first.
  - Of its commands it reads only `Set`, `SetMacro`, `Unset`, `If`, which
    nests, and `IconSprites`, which it reports.
  - It skips a line that is longer than 1023 bytes, opens a redirection, or
    names its command by an alias or an abbreviation.
  - It runs nothing, and touches nothing but the variables it is given.

## Tests

- **Units.** Every escape, reference form and variable type; the examples
  the PRMs give for every operator; the error each malformed shape gives;
  `OS_ReadArgs`' buffer bytes against the layout; and the read-only mode's
  refusals.
- **Fuzzing.** `fuzz_obey` sends hostile strings, expressions, templates and
  Obey files through every entry and holds each to its bound. It also holds
  two exact oracles:
  - any bytes, escaped as `*Show` writes a value, GSTrans back to themselves;
  - a fully bracketed integer expression evaluates as wrapping 32-bit
    arithmetic does.
