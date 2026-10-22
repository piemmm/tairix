# tairix-saver-ribbon

**Stability tier:** `experimental`

Ribbon, the minimal-clock desktop screensaver: the time and the date held
still while a ribbon of orange light (`lib/ribbon`, shared with the login
screen) travels around them. It ships as `/System/Screensavers/Ribbon.app`.

- `src/lib.rs` — the face, host-tested over `tairix_saver::testing`.
- `src/run.rs` — the bundle's `Run`: the face served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability and no feed. The time and the spelled-out date are
the host's wall reading. Its option is `date` (a switch); under reduced motion
the ribbon holds still and only the reading changing redraws anything.

The design is `plans/SCREENSAVERS.md`.
