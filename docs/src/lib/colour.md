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

## Notation

`parse_hex` reads three, four, six or eight ASCII hex digits of either case and
nothing else — no `#`, sign or space — so an integer parser's `+f` is not a
digit here. The `#` belongs to the grammar around the digits: CSS requires it
and strips it before asking, a colour field takes it or not, and a settings
document cannot hold it at all, since its grammar starts a comment there and
`Rgb::from_hex` takes exactly the six bare digits. `Rgb::hex` and `Rgba::hex`
write lowercase digits — eight where the colour is translucent — and
`Hex::hashed` puts the `#` before them.
