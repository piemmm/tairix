# `tairix-softfloat`

The one software floating point: binary16, binary32, binary64 and
double-extended arithmetic, each result computed exactly and rounded once, in integers alone
so every target computes the same bits. PCEmulator's 8087 and the RISC OS
emulator's FPA, VFP and Advanced SIMD compute through it, and `lib/drawing`
stores a Draw file's doubles in the FPA's word order by it.

## The core

The core (`exact`) holds a result exactly and decides nothing about overflow,
underflow or NaNs, so a unit with rules of its own builds them on it.

| Item | What it is |
|---|---|
| `Parts` | A finite operand, `sig × 2^(exp − 63)`, its integer bit explicit, so an unnormal is an operand like any other. `Parts::of` reads a double-extended value, a denormal as the unnormal it equals. |
| `Exact` | A result before rounding, `sig × 2^(exp − 126)`: a 64-bit significand, 62 guard bits below it, a carry bit above, and a sticky bit for anything lower. |
| `add`, `multiply`, `divide`, `square_root` | The exact result of two `Parts`, or one. A sum is normalized as its larger operand is and a product with an unnormal factor stays unnormal, as the 8087's unnormal arithmetic leaves them. |
| `Exact::rounded` | The significand rounded to a `Rounding`: a `Mode` and the bits kept, 24, 53 or 64 for the three formats. It answers whether rounding carried and whether the result is inexact. |
| `to_integer` | A value rounded to an integer in a `Mode`, `None` past `u64`. |

`Mode` is IEEE 754's five rounding directions: to nearest with ties to even or
away from zero, down, up, and toward zero.

## The IEEE layer

`Ieee { format, mode, policy }` is IEEE 754's arithmetic on binary16,
binary32 and binary64 bit patterns with default exception handling. Each operation answers
an `Outcome`: the result's bits and the `Flags` it raised (invalid,
divide-by-zero, overflow, underflow, inexact).

| Operation | Notes |
|---|---|
| `add`, `sub`, `mul`, `div`, `sqrt` | |
| `fused_multiply_add` | Rounded once, the product held exactly. A NaN operand is chosen addend first, as ARM's fused multiply-add takes them. `∞ × 0` beside a quiet NaN addend is invalid and answers the default NaN; beside a signalling one, that addend quieted. |
| `rem` | IEEE 754's remainder, the quotient rounded to the even integer. It is always exact. |
| `round_to_integral` | Rounding in the value's own format; inexact only where asked, as `roundToIntegralExact` raises it. |
| `to_integer`, `from_integer` | An `Integral` (a value, out of range, or a NaN) for the caller to hold to its own range, and the rounded value of an integer. |
| `to_fixed`, `from_fixed` | The same of a fixed-point value with a number of fraction bits, rounded once. |
| `convert` | Between the three formats, rounded where the target is narrower. |
| `compare` | `None` when unordered. A signalling NaN raises invalid, and so does a quiet one where the comparison signals. |

`Policy` makes the choices the standard leaves to an implementation:

- `Tininess::BeforeRounding` (ARM) or `AfterRounding` (x86) decides when an
  underflow is tiny.
- `Nans::Propagate` answers the first signalling NaN operand quieted, else the
  first quiet one. `Nans::Default` answers the default NaN, as VFP's
  default-NaN mode does.
- `flush_to_zero` makes a result tiny before rounding a zero of its sign,
  raising underflow alone, as ARM's flush-to-zero mode does. Flushing a
  denormal operand is the caller's: the standard has no flag for it.

## Decimal conversion

`Decimal` is `±significand × 10^exponent`, its significand a `u128` of up to
38 digits. Both directions work from the value's whole expansion in a
fixed-width integer, so neither approximates and neither allocates.

- `Decimal::of(parts, digits, mode)` writes a finite value to 1 to 38
  significant digits rounded in any `Mode`, and says whether that was
  inexact. Any value in double-extended's range converts, the least
  subnormal among them.
- `Decimal::exact` reads a decimal value as an `Exact`: its top 127 bits and
  a sticky bit, enough to round correctly at any precision. A value past
  double-extended's range is `Beyond::Above` its largest finite value or
  `Beyond::Below` half its least subnormal, for the caller to overflow or
  underflow by its own rules.

The work grows with the value's distance from one: a few words for ordinary
values, a few hundred thousand word operations at the far ends of
double-extended's range.

## The formats

- `Binary::SINGLE` and `Binary::DOUBLE` unpack a value's bits into `Unpacked`,
  pack them back, and name the format's infinities, zeros, largest values and
  NaNs. A subnormal unpacks as the unnormal it equals.
- `fpa_words` and `from_fpa_words` hold a binary64 value in the FPA's memory
  order, the word with the sign and exponent first.
- `Extended` is the double-extended value: a sign, a 15-bit exponent, and a
  64-bit significand with its integer bit explicit.
  - `to_bytes` and `from_bytes` are the x87's ten bytes, the significand
    first and low byte first.
  - `to_fpa_words` and `from_fpa_words` are the FPA's three words: the sign
    in bit 31 and the exponent in bits 14 to 0, then the significand's high
    word and its low word.
- `Packed` is the FPA's packed decimal as memory holds it (ARM7500FE data
  sheet, 9.3.5 and 9.3.6). The first word holds the number's sign in bit 31
  and the exponent's sign in bit 30, then the exponent's BCD digits, then the
  significand's, most significant first, with the decimal point after the
  first significand digit.
  - `packed::Standard` (P) is three words: four exponent digits and nineteen
    significand digits.
  - `packed::Expanded` (EP) is four words: seven exponent digits and
    twenty-four significand digits.
  - `Packed::of` normalizes a `Decimal` into the format, and `Packed::value`
    reads one back. A nibble above nine has no value, as an infinity's or a
    NaN's exponent has none.

## What each unit keeps

A unit keeps only the rules that are its own.

- **The 8087** keeps:
  - its register wrap of 24 576 on an unmasked overflow or underflow;
  - a masked overflow giving infinity when chopping, and a masked underflow
    flagged on tininess alone when chopping;
  - its partial remainder;
  - its value classes, its real indefinite and its packed BCD.
- **The FPA** (RO11) keeps:
  - which encodings of its extended and packed formats are infinities, NaNs
    or illegal;
  - the digits its stores write and how its loads round;
  - the rules of its extended precision.

## Tests

- **Round to nearest.** 40 000 random operand sets per format, spread over
  every class and exponent, are held bit for bit to the host's IEEE 754
  `+ − × ÷ √` and fused multiply-add. A NaN result need only be a NaN, as the
  host picks its own.
- **Directed modes.** 20 000 pairs whose results stay in range are held to the
  neighbour of the host's nearest result that the exact error points to, with
  inexact raised exactly when the error is nonzero. The error comes from
  error-free transformations.
- **Each rule.** The sign of an exact zero sum, overflow by mode, tininess
  either side of rounding, invalid operations, NaN propagation, remainders,
  integral rounding and integer conversion in each mode, narrowing as the host
  narrows, comparisons and their flags, and both memory layouts.
- **The core.** Ties broken to even, away or by direction and by the sticky
  bit, a carry into the next exponent, a narrower precision's own last bit,
  integers in every mode, and exact roots of perfect squares.
- **Decimal.**
  - 3 000 random doubles, subnormals among them, are written to a random
    number of digits in every mode. Each is held to the host's exact
    expansion of the double and to its own rounding to nearest.
  - 20 000 random decimal values over and past binary64's range are read and
    held to the host's correctly rounded parsing, as doubles and as singles.
  - Decimal ties between doubles break by what follows them.
  - 3 000 random normal double-extended values come back exactly from
    twenty-one digits.
  - The range's bounds are held to the powers of two where double-extended
    ends.
  - PCEmulator's debugger holds its register display to values worked out
    with 6000-digit arithmetic.
- **Packed decimal.** Both layouts are held to the data sheet's drawings,
  together with the zero and infinity encodings, the bits the format keeps
  zero, and values neither format can hold.
- **Fuzzing.** `fuzz_decimal` converts hostile values and words: any parts,
  digit count, decimal value and packed words. It holds each to the
  directions' bracket, the twenty-one-digit round trip and the packed round
  trip.

The crate is `no_std`, allocation-free and forbids `unsafe`, and it depends on
nothing.
