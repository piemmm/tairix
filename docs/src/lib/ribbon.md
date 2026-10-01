# `tairix-ribbon` — the ribbon of light

`lib/ribbon` is the ember scene TAIRiX draws behind text on a dark screen:
five soft strands of light running from the left edge to the right, each one
smooth Bézier curve, roaming on travelling waves and toned through an ember's
heat from deep red to pale gold. It has two embedders, and neither may depend
on the other, which is why the scene lives in `lib/*`:

| Embedder | Keeps the ribbon clear of |
|---|---|
| the minimal-clock screensaver (`userland/gui/session/src/saver/ribbon.rs`) | the time and the date |
| the graphical login screen (`userland/session/greeter`) | its login column (`AuthSurface::column_rect`) |

Each embedder owns only where the pixels go: the screensaver repaints its
compositor window, the login screen a layer it composites beneath its column.

## `Light`

The ribbon at an instant.

- `Light::new(size, exclusion, t)` lays the ribbon out on a `size` screen,
  kept clear of `exclusion`, as it stands at `t` seconds of its own time.
  Every buffer a frame needs — the per-column state, the toning table, the
  strand profile, the scratch light is summed in — is reserved here, so a
  frame allocates nothing, and a heap that refuses one is `None`.
- `step(t, exclusion, damage)` moves it to a new time, adding to `damage`
  every pixel whose light may have changed; the very same instant around the
  very same clear space is the frame already placed and moves nothing.
- `paint_moved(surface, after)` repaints exactly the strips the last step
  reported, handing each to `after` once it is painted, so an embedder can
  letter its text back over just those pixels.
- `paint(surface, area)` paints any area at all. Every sample is summed the
  same way whatever area asks for it, so a part painted on its own matches the
  whole painted at once, pixel for pixel.

### Keeping clear of text

Beneath the clear space the strands gather and the ribbon's course is held
low enough for their light to clear it; wherever a strand's light would still
reach it, that strand is pushed down a smooth bump of its own control points
just far enough to hold the clear space dark, so a path passing beneath the
text is still one curve.

What is kept clear is the light that **shows**: from the toning table's
second step on, the first light that lifts every pixel it reaches a whole
level. The glow is drawn on past that, down to a small fraction of a level,
but that invisible tail no longer holds the ribbon back: the ribbon rises
until its light that shows meets the clear space, and `paint` leaves the whole
clear space `SKY`, so no pixel inside it is ever lifted off the sky and the
row beneath it is lifted by at most the dither's scatter. The glow itself is
drawn exactly as anywhere else. A column whose light falls wholly within the
clear space's columns reports only the rows beneath it as lit, so a frame
repaints nothing inside the text.

### Cost

The light is summed at every other pixel each way and blended up to every
pixel as it is toned and dithered. A frame repaints only the rows each column's
light reaches now and reached the frame before — under half the screen — in
strips a few dozen pixels wide.

## Pacing

Both embedders pace the ribbon by `tairix_theme::motion::SceneClock`, the clock
every idle scene moves by: a frame falls due every `SceneClock::FRAME_NS`
while it moves, a late wake carries it at most a few frames on, and a ribbon
made still under reduced motion asks for no frame at all.

## `SKY`

The black wherever the ribbon's light does not reach, and over the whole of the
text's clear space. Text set over the
ribbon is shadowed in it, so over the dark the shadow composes to exactly what
is already there.

## Tests

`cargo test -p tairix-ribbon` holds the ember's tone curve to the storyboard,
every path to a single smooth curve across the width with no kink, the strands
to roaming freely and moving independently, the clear space to staying
`SKY` with no light that shows beneath it and no frame repainting it, the
ribbon rising nearer the text than its glow's whole tail would let it, the
light that shows to be the first toning step that lifts every pixel a whole
level, strips to matching the whole with and without text, the light to being soft everywhere and
black beyond the rows it reports, and a frame to repainting under half the
screen. The clock's own tests live with `SceneClock` in `lib/theme`.

## Stability

Experimental.
