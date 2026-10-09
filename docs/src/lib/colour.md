# `tairix-colour`

The sRGB colour (`lib/colour`): its 8-bit value, the transfer its channels are
encoded in, its hue coordinates, and its hexadecimal spelling. Every colour
conversion and notation in the tree is defined here once — the theme's palette,
a settings document's colours, an SVG asset's `#rgb` and `hsl()`, the colour
picker's plane and fields — so two components never disagree about what a
colour is or how it is spelled.

| Item | What it is |
|---|---|
| `Rgb`, `Rgba` | The opaque and the straight-alpha 8-bit colour. A colour that can never be translucent — a desktop backdrop, a terminal scheme's ink — is an `Rgb`, so no round trip invents an alpha. `mix` and `over` resolve one authored colour against one ground; compositing belongs to `lib/raster`. |
| `Hue`, `Fraction` | An angle round the colour circle in 393216ths of a turn, 65536 to each sixth, and a proportion in 65535ths. Whole degrees and percentages, 8-bit levels and CSS numbers convert to and from them. |
| `Hsv`, `Hsl` | The coordinates a picker's plane and CSS `hsl()` describe a colour by. |
| `parse_hex`, `HexForm`, `Hex` | CSS's four digit spellings, read with the form they were spelled in and written lowercase. |
| `srgb_to_linear`, `linear_to_srgb` | The IEC 61966-2-1 transfer. |
| `linear_of`, `encode_linear`, `InGamut` | An 8-bit colour as linear light, and linear light back as the nearest 8-bit colour, each channel held to the gamut and the clip reported. |
| `Cmyk` | Device CMYK: the complement of sRGB with the black drawn out, uncalibrated, since without an output profile there is nothing else for the numbers to mean. Exact for every 8-bit colour. |
| `Xyz`, `Lab`, `Lch` | CIE 1931 XYZ (Y the luminance, 1 at white), L\*a\*b\* and its polar LCh(ab), all under D65. A colour outside sRGB comes back clipped and says so. |
| `Illuminant`, `KELVIN_MIN`, `KELVIN_MAX` | A light's white by its correlated colour temperature and its Duv off the Planckian locus, and the temperature and Duv a colour, or a point of the CIE 1960 uv plane, lies at. |
| `uv_of`, `chromaticity_of`, `white_of` | CIE 1931 chromaticity to the CIE 1960 uv plane and back, and a chromaticity's white as linear sRGB at luminance one. |
| `legibility` | WCAG 2.1 contrast, for host tests only (feature `test-util`). |

## Exact coordinates

The conversions are integer arithmetic rounded once, at the end, and fine
enough that every 8-bit colour comes back unchanged through `Hsv` and through
`Hsl`; the unit tests check all 2^24 of them. A grey has no hue, and black (and,
in HSL, white) no saturation either, so `from_rgb` takes those from the
coordinates the caller passes as `near`: a colour dragged to black in a picker,
or set to black by its owner, keeps the hue and saturation it showed, and comes
back to them when its value does. `Hsl::to_rgb` is CSS Color 4's conversion, so
`hsl(210, 50%, 40%)` is `#336699` here as in a browser.

## Measured spaces

The CIE spaces are `f64`, since their transfer is a cube root no integer form
rounds once. A Lab or LCh colour need not lie in sRGB at all; `to_rgb` answers
the nearest 8-bit colour with each linear channel held to `0.0..=1.0`, and
`InGamut::clipped` says when that moved it, so a picker can mark a value it
cannot show.

`Illuminant` measures temperature on Krystek's rational fit of the Planckian
locus in the CIE 1960 uv plane (1985), within 8e-5 of the true locus from
1000 K to 15000 K and smooth across it, so a light named off the locus measures
back to the temperature it was named by. Duv is the signed distance from the
locus in that plane, positive towards green. `of_uv` (and `of_linear`, through
it) finds the nearest point of the locus by sampling it a mired apart and
refining the bracket, which costs microseconds and runs once per pick. sRGB's own white measures 6504 K, 0.0032
towards green.

## Notation

`parse_hex` reads three, four, six or eight ASCII hex digits of either case and
nothing else — no `#`, sign or space — so an integer parser's `+f` is not a
digit here. The `#` belongs to the grammar around the digits: CSS requires it
and strips it before asking, a colour field takes it or not, and a settings
document cannot hold it at all, since its grammar starts a comment there and
`Rgb::from_hex` takes exactly the six bare digits. `Rgb::hex` and `Rgba::hex`
write lowercase digits — eight where the colour is translucent — and
`Hex::hashed` puts the `#` before them.
