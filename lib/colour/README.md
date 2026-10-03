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
- `srgb_to_linear`, `linear_to_srgb` — the IEC 61966-2-1 transfer.
- `legibility` (feature `test-util`, host tests only) — the WCAG 2.1 contrast
  ratio a test holds a colour pair to.

`no_std`, allocation-free and with no `unsafe`; it depends only on `lib/util`
for the transfer's exponent and the floor that wraps a CSS angle. Tests check
the round trip through `Hsv` and `Hsl` for every one of the 2^24 colours, the
CSS reference conversions, and a hex spelling's refusal of anything but its
digits, a sign included; a fuzz harness (`fuzz_colour`) holds the readers total
over any text and any number.

## Stability

Tier: `experimental`. The coordinates and the notation follow their
definitions; the surface grows only with a consumer.
