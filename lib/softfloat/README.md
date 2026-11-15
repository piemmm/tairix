# tairix-softfloat

Software floating point: exact binary32, binary64 and double-extended
arithmetic, each result rounded once in any of IEEE 754's modes, in integers
alone so every target computes the same bits.

- **The core** (`exact`): a result held exactly — a 64-bit significand, 62
  guard bits and a sticky bit — over operands in `Parts`, unnormals among
  them, and `Exact::rounded` at 24, 53 or 64 bits in any `Mode`. It decides
  nothing about overflow, underflow or NaNs, so a unit with rules of its own
  builds them on it: PCEmulator's 8087 does, its register wrap and memory
  stores its own.
- **The IEEE layer** (`Ieee`): IEEE 754's arithmetic on binary32 and binary64
  values with default exception handling — add, subtract, multiply, divide,
  square root, fused multiply-add, remainder, rounding to an integral value,
  conversions between the formats and to and from integers, and comparison —
  each raising the `Flags` the standard defines. A `Policy` makes the choices
  the standard leaves open: when an underflow is tiny, and whether a NaN
  result carries an operand's payload.
- **Decimal conversion** (`Decimal`): a finite value written to 1 to 38
  significant digits in any mode, and a decimal value read exactly enough to
  round correctly at any precision, both from the whole expansion.
- **The formats**: `Binary` (binary32, binary64), with the FPA's high word
  first order for binary64 (`fpa_words`); `Extended`, the double-extended
  value the 8087's registers and the FPA's extended precision hold, with the
  x87's ten-byte and the FPA's three-word memory layouts; and `Packed`, the
  FPA's packed decimal in its three-word and four-word forms.

`no_std`, allocation-free, with no `unsafe`. The IEEE layer is held to the
host's own IEEE 754 arithmetic bit for bit in round-to-nearest, and in the
directed modes to the exact error of the host's nearest result. Decimal
conversion is held to the host's exact formatting in every mode and to its
correctly rounded parsing.

## Stability

Tier: `experimental`.
