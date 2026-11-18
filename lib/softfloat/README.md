# tairix-softfloat

Software floating point: exact binary16, binary32, binary64 and
double-extended arithmetic, each result rounded once in any of IEEE 754's modes, in integers
alone so every target computes the same bits.

- **The core** (`exact`): a result held exactly — a 64-bit significand, 63
  guard bits and a sticky bit — over operands in `Parts`, unnormals among
  them, and `Exact::rounded` at 24, 53 or 64 bits in any `Mode`. It decides
  nothing about overflow, underflow or NaNs, so a unit with rules of its own
  builds them on it: PCEmulator's 8087 does, its register wrap and memory
  stores its own.
- **The IEEE layer** (`Ieee`): IEEE 754's arithmetic on binary16, binary32 and
  binary64 values with default exception handling — add, subtract, multiply, divide,
  square root, fused multiply-add, remainder, rounding to an integral value,
  conversions between the formats and to and from integers, and comparison —
  each raising the `Flags` the standard defines. A `Policy` makes the choices
  the standard leaves open: when an underflow is tiny, and whether a NaN
  result carries an operand's payload; and ARM's flush-to-zero.
- **The elementary functions** (`elementary`): sin, cos, tan, the angle of a
  point, asin, acos, e^x, 2^x − 1, ln, log10, y log2 x, y log2(1 + x) and x^y,
  each computed in 128-bit significands and answered as an `Exact` to round
  once: correct unless the true value lies within 2^-120 of a rounding
  boundary, exact where the value is, and beside its leading term rounded to
  the side it lies. PCEmulator's 8087 and TAIRiX Bridge's FPA take
  their transcendentals from it.
- **Decimal conversion** (`Decimal`): a finite value written to 1 to 38
  significant digits in any mode, and a decimal value read exactly enough to
  round correctly at any precision, both from the whole expansion.
- **The formats**: `Binary` (binary16, binary32, binary64), with the FPA's
  high word first order for binary64 (`fpa_words`); `Extended`, the
  double-extended value the 8087's registers hold, with the x87's ten-byte
  memory layout; and `Packed`, the FPA's packed decimal in its three-word and
  four-word forms as the ARM7500FE data sheet draws them.

`no_std`, allocation-free, with no `unsafe`. The IEEE layer is held to the
host's own IEEE 754 arithmetic bit for bit in round-to-nearest, and in the
directed modes to the exact error of the host's nearest result. Decimal
conversion is held to the host's exact formatting in every mode and to its
correctly rounded parsing.

## Stability

Tier: `experimental`.
