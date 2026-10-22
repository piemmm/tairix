# tairix-saver-clock

**Stability tier:** `experimental`

Clock, a desktop screensaver: the time, and as its options say the date and
who is signed in where, moved about the screen each minute so no pixel stays
lit for long. It ships as `/System/Screensavers/Clock.app`.

- `src/lib.rs` — the face, host-tested over `tairix_saver::testing`.
- `src/run.rs` — the bundle's `Run`: the face served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability and no feed. The time and date are the host's wall
reading, spelled as the icon bar spells them, and the face asks for its next
frame when that reading next changes. Its options are `date` and `identity`
(switches); under reduced motion the block moves without fading.

The design is `plans/SCREENSAVERS.md`.
