# tairix-saver-retrogames

**Stability tier:** `experimental`

Retro Games, a desktop screensaver: a flight over a glowing wireframe grid
towards a banded sun setting between wireframe mountains, where wireframe craft
now and then play out retro arcade games — a spaceship passing, a flying
saucer darting about, tanks trading fire, riders walling each other in. It
ships as `/System/Screensavers/RetroGames.app`.

- `src/lib.rs` — the flight and its frame: what moves is repainted, the far
  sky, sun and mountains are painted once and kept.
- `src/cast.rs` and its players (`ship.rs`, `saucer.rs`, `tanks.rs`,
  `riders.rs`, `blast.rs`), `src/wire.rs` — the craft and the wireframe stage
  they are drawn through.
- `src/sky.rs`, `src/mountains.rs`, `src/floor.rs`, `src/under.rs` — the
  backdrop, and what lies under the craft as last drawn.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability and no feed; it declares `threads = "cores"`, and
paints each frame's bands across a pool of the threads its host granted. Its
option is `speed` (`slow`, `normal`, `fast`); under reduced motion only the
first frame is drawn.

The design is `plans/SCREENSAVERS.md`.
