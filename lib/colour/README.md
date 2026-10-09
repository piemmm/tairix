# tairix-colour

The sRGB colour and everything about it that is the colour space's own: the
8-bit value, opaque (`Rgb`) or with straight alpha (`Rgba`); the sRGB transfer
its channels are encoded in; its hue, saturation, value and lightness
coordinates (`Hsv`, `Hsl`); and its hexadecimal notation (`parse_hex`, `Hex`).
A theme authors its palette in it, a settings document stores it, CSS `hsl()`
and `#rrggbb` resolve through it and a colour picker edits in it, so every
colour conversion and spelling in the tree has one definition.

- `Rgb`, `Rgba` — the opaque and the straight-alpha 8-bit colour, with `mix`
  and `over` for resolving one authored colour against one ground.
  Compositing is `lib/raster`'s premultiplied pixel, reached through
  `From<Rgba> for Color` there.
- `Hue`, `Fraction` — an angle round the colour circle in 393216ths of a turn
  (65536 to a sixth, so primaries and secondaries are exact) and a proportion
  in 65535ths, with whole-degree, whole-percent, 8-bit and CSS-number
  conversions.
- `Hsv`, `Hsl` — the coordinates, converted to and from `Rgb` in integer
  arithmetic rounded once: every 8-bit colour comes back unchanged through
  each, and a grey's hue and black's or white's saturation, which the colour
  does not hold, are taken from the coordinates the caller held.
- `parse_hex`, `HexForm`, `Hex` — CSS's four digit spellings (`rgb`, `rgba`,
  `rrggbb`, `rrggbbaa`) read with the form they were spelled in, and written in
  lowercase, alpha last where translucent; `Rgb::from_hex` is the six-digit
  form alone. The `#` is the surrounding grammar's: CSS and a colour field
  write one, a settings document cannot.
- `srgb_to_linear`, `linear_to_srgb` — the IEC 61966-2-1 transfer, and
  `linear_of`, `encode_linear` — an 8-bit colour as linear light and back,
  clipped to the gamut and saying so (`InGamut`).
- `Cmyk` — device CMYK, uncalibrated: the complement of sRGB with the black
  drawn out, in 65535ths, exact for every 8-bit colour.
- `Xyz`, `Lab`, `Lch` — CIE 1931 XYZ, L\*a\*b\* and its polar LCh(ab) under
  D65, in `f64`; a colour outside sRGB comes back clipped and marked.
- `Illuminant`, `KELVIN_MIN`, `KELVIN_MAX` — a light's white by its correlated
  colour temperature and its Duv off the Planckian locus (Krystek's rational
  fit, 1000–15000 K), and the temperature and Duv a colour, or a point of the
  CIE 1960 uv plane, lies at.
- `uv_of`, `chromaticity_of`, `white_of` — chromaticity to the uv plane and
  back, and a chromaticity's white as linear sRGB.
- `legibility` (feature `test-util`, host tests only) — the WCAG 2.1 contrast
  ratio a test holds a colour pair to.

`no_std`, allocation-free and with no `unsafe`; it depends only on `lib/util`
for the transfer's exponent, the cube root and angles of the CIE spaces, and
the floor that wraps a CSS angle. Tests check the round trip through `Hsv`,
`Hsl` and `Cmyk` for every one of the 2^24 colours and through `Lab` and `Lch`
for a sample of them, the CIE values of the primaries, D65 and illuminant A's
temperatures, the
CSS reference conversions, and a hex spelling's refusal of anything but its
digits, a sign included; a fuzz harness (`fuzz_colour`) holds the readers total
over any text and any number.

## Stability

Tier: `experimental`. The coordinates and the notation follow their
definitions; the surface grows only with a consumer.
