# tairix-saver-bubbleuniverse

**Stability tier:** `experimental`

Bubble Universe, a desktop screensaver: two hundred curves of the Bubble
Universe map, each two hundred points long, drawn as soft dots of light that
add where they crowd, so rings, spirals and stars flow through a glowing disc
as its time runs. It ships as `/System/Screensavers/BubbleUniverse.app`.

- `src/lib.rs` — the scene: its options, its pace, its slow motions and the
  frame they make.
- `src/orbit.rs` — the map, traced eight curves at a time, one to a lane.
- `src/palette.rs` — the original's colours and the designed ramps.
- `src/light.rs` — the dots, sorted into strips of rows and drawn and toned a
  band at a time.
- `src/glow.rs` — the halo and the haze, gathered on a coarse grid.
- `src/tone.rs` — the tone curve.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability and no feed; it declares `threads = "cores"`, traces
each frame's curves across them and draws and tones its strips a band of rows
at a time, painting only the square about the bubble. Its options are
`colours` (`original`, `nebula`, `aurora`, `ember`, `ice`, `spectrum`), `speed`
(`slow`, `normal`, `fast`) and `glow` (a switch); under reduced motion each
picture is held still for a minute, then another is set elsewhere.

The design is `plans/SCREENSAVERS.md`; the page is
`docs/src/desktop/screensavers.md`.
