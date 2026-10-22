# tairix-saver-slideshow

**Stability tier:** `experimental`

Slideshow, a desktop screensaver: the shipped pictures, one after another. It
ships as `/System/Screensavers/Slideshow.app`.

- `src/lib.rs` — the running order and the pace, host-tested over
  `tairix_saver::testing`.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability. It declares the slides feed: the host offers the
shipped pictures, narrowed to the category the desktop's settings name, reads
each one asked for in its own sandbox, fits it to the screen and lays it in
the feed area; the saver reads it on the next frame. Its options are
`interval` (5 seconds to an hour) and `order` (`sequential`, `shuffled`).

The design is `plans/SCREENSAVERS.md`.
