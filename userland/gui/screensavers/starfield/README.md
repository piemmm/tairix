# tairix-saver-starfield

**Stability tier:** `experimental`

Starfield, a desktop screensaver: stars flown through at a speed that cruises,
surges into warp and settles back, the field turning slowly about the line of
flight. It ships as `/System/Screensavers/Starfield.app`.

- `src/lib.rs` — the scene, host-tested over `tairix_saver::testing`.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability and no feed; it declares `threads = "cores"`, and
erases and draws a frame's streaks a band of rows at a time across them,
through `tairix_raster::BandScratch`. Its options are `stars` (`sparse`,
`normal`, `dense`) and `warp` (a switch); under reduced motion it cruises
without turning.

The design is `plans/SCREENSAVERS.md`.
