# tairix-crt

The picture tube: scan lines, the phosphor's afterglow, the faceplate's glow,
an analogue noise floor and a travelling wobble, drawn over a picture.

- `Tube` — a permille strength per effect, and the ordered passes they
  resolve to at a display density; a zero strength is no pass.
- `Pipeline` — the passes' stages, kept between frames: a render redraws a
  stage only where its input changed, so a change costs its rectangle plus the
  glow's reach and an unchanging picture costs nothing. A buffer it cannot
  have costs the effect, never the picture.
- `Phase`, `frame_due` — the clock the wobble, the noise and an unsettled
  afterglow move on: whole display frames, never faster than twenty a second,
  armed only while something moves.

The terminal (`userland/apps/terminal`) and the PC emulator
(`userland/apps/pcemu`) draw through it. Its design is
`docs/src/lib/crt.md`.

## Stability

Experimental.
