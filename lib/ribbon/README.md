# tairix-ribbon — the ribbon of light

The ember scene TAIRiX draws behind text on a dark screen: five soft strands
running edge to edge, each one smooth Bézier curve, roaming on travelling waves
and toned through an ember's heat from deep red to pale gold.

**Stability tier: experimental.**

It has two embedders, and neither may depend on the other:

| Embedder | Keeps the ribbon clear of |
|---|---|
| the desktop session's minimal-clock screensaver (`userland/gui/session`) | the time and the date |
| the graphical login screen (`userland/session/greeter`) | its login column |

## What it provides

- `Light` — the ribbon at an instant. `Light::new(size, exclusion, t)` places it
  on a screen around a clear space; `step(t, exclusion, damage)` moves it to a
  new time, adding every pixel whose light may have changed to `damage`;
  `paint_moved` repaints exactly those strips and `paint` any area at all. A
  part painted on its own matches the whole painted at once, pixel for pixel.
- `Motion` — the ribbon's clock. It answers when the next frame is due
  (`FRAME_NS` apart while it moves, none while it holds still) and how far a
  frame moves it, bounded so a late wake carries it a few frames rather than
  all the way.
- `SKY` — the black wherever its light does not reach. Text over the ribbon is
  set against it.

## Keeping clear of text

Wherever a strand's light that shows would reach the clear space, that strand
is pushed down a smooth bump of its own control points just far enough to hold
the clear space dark, so a path passing beneath the text is still one curve.
"Shows" is the toning table's second step, the first light that lifts every
pixel a whole level: the glow's fainter tail no longer holds the ribbon back,
and the paint leaves the whole clear space `SKY` instead, so no pixel inside
it is ever lifted off the sky. The glow is drawn exactly as anywhere else.

## Cost

Every buffer a frame needs is reserved when the ribbon is made, so a frame
allocates nothing; a heap that refuses one is `None`, never a panic. The light
is summed at every other pixel each way and blended up as it is toned and
dithered, and a frame repaints only the rows its light reaches and reached —
under half the screen.

## Tests

`cargo test -p tairix-ribbon`: the ember's tone curve, every path a single
smooth curve across the width, the strands roaming freely and moving
independently, the clear space kept `SKY` with no light that shows beneath it,
the ribbon rising nearer the text than its glow's whole tail allows, strips
matching the whole, the
light soft everywhere, and the clock's frames and late-wake bound.
