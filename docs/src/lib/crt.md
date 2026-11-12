# `tairix-crt`

The picture tube: what makes a picture read as a cathode-ray tube's. The
terminal draws its text through it and the PC emulator its machines'
monitors, so the two look alike and the effects are written once.

| Item | What it is |
|---|---|
| `Tube` | A strength per effect, a permille of `FULL`: scan lines, the phosphor's afterglow, the faceplate's glow, an analogue noise floor (fuzz) and a travelling wobble. The default draws none. |
| `Tube::passes(scale_percent)` | The ordered `Pass` list the strengths resolve to at a display density: wobble, phosphor, scan lines, glow, fuzz. A zero strength is no pass. |
| `Pipeline` | The passes' stages, kept between frames. `render(picture, damage, tube, scale_percent, phase, runner)` brings them up to date and answers the effected picture and the region of it that changed, or `None` with no pass on or no buffer to be had — the caller then shows the plain picture. |
| `Phase` | The animation step the moving passes are a pure function of. |
| `frame_due`, `frame_interval_ns` | When the next animation frame is due: whole display frames, never faster than `FASTEST_MHZ` (twenty a second), armed only while something moves. |
| `scaled(strength, full)` | A strength's share of a full-scale physical value, rounded to nearest. |

## The stages

The passes fall into three stages by what an output pixel depends on. Wobble,
phosphor and scan lines read one pixel of the picture and the row it is on
(`pre`); the glow reads the neighbourhood of `pre` its reach spans (`lit`);
fuzz reads one pixel of `lit` and the step (`out`). A stage exists only while
one of its passes is on, and a render redraws a stage only where its input
changed, so a change costs its rectangle plus the glow's reach around it and an
unchanging picture costs nothing. Rows are spread across the runner's workers
once a stage is large enough to be worth it.

The phosphor trail decays once per animation step, never per present, so it
fades in time as a tube does, and settles once every pixel's trail is the level
its own light holds it at; from then on nothing animates until the picture
changes. `Pipeline::animated` answers whether a step is owed: a wobble or fuzz
is on, or the trail has not settled.

## Bounds

The physical parameters are fixed at full strength — a wobble of 6 pixels, a
scan line dimming its row by 62%, a glow reaching 5 pixels at 120% intensity,
an afterglow keeping 82% of a lit pixel a step, a jitter of 56 on the 8-bit
scale — scaled by the display density where they are distances. None can be
pushed further: no strength makes what the tube shows unreadable. A scan line
dims a pixel's light, never its coverage.
