# tairix-saver-life

**Stability tier:** `experimental`

Life, a desktop screensaver: Conway's Game of Life on a torus, its colonies
coloured by descent and age, reseeded once it settles. It ships as
`/System/Screensavers/Life.app`.

- `src/lib.rs` — the scene, host-tested over `tairix_saver::testing`.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability and no feed. Its options are `cells` (`small`,
`medium`, `large`) and `speed` (`slow`, `normal`, `fast`); under reduced
motion a cell is born and dies at once. A world whose every cell has settled
asks for no frame before its next generation.

The design is `plans/SCREENSAVERS.md`.
