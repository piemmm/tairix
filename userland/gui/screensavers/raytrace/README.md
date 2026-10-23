# tairix-saver-raytrace

**Stability tier:** `experimental`

Ray Tracer, a desktop screensaver: a scene composed at random in one of the
tracer's settings (`lib/raytrace`), revealed coarse to fine — soft within
moments, each pass sharpening it — held for a minute, faded out, and followed
by a scene set elsewhere, a readout in the corner saying how far the scene is
prepared and then traced. It ships as `/System/Screensavers/Raytrace.app`.

- `src/lib.rs` — the reveal's phases and how its changes reach the screen.
- `src/engine.rs` — composing, preparing and tracing a scene, free of
  threads, paced by the monotonic clock.
- `src/crew.rs` — the desk between a tracing thread and the frames.
- `src/preview.rs`, `src/crossfade.rs` — the picture out of focus sharpening
  pass by pass, and a change faded in over at least eight frames however
  slowly they come, each tracked in the tiles it touches
  (`tairix_geometry::Tiles`).
- `src/readout.rs` — the readout, laid over the picture with what it covers
  kept aside, so the picture never holds a letter.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability; it declares `threads = "cores"`, `memory =
"large"` and the keeping feed. Its options are `cpu` (`idle` traces on one
thread of its own and paints its frames across the rest, `performance` traces
on every thread granted), `save` (hand each
whole picture to the host to keep under `Home/Pictures/Ray Tracer/`) and
`detail` (`simple`, `maximum`; a scene whose peak the memory band cannot
spare is set out simply). Where no thread is granted, each frame traces a
slice itself. The long whole-scene tests are behind `scene-tests`.

The design is `plans/SCREENSAVERS.md`; the tracer's is `plans/RAYTRACE.md`.
