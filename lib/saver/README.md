# tairix-saver

**Stability tier:** `experimental`

The saver side of a TAIRiX screensaver: the loop a bundle in the drawing
sandbox runs its scene through — `Start`, the frame region, each `Frame`
drawn in place on it and `Presented`, the font relay, its options and its
feeds.

- `src/lib.rs` — `Scene`, `Setup` (options and their pace, seed, theme,
  pool), `Told` (a wall reading compared with no allocation) and the frame's
  `Host`.
- `src/serve.rs` — the protocol loop over the two pipes and the region.
- `src/link.rs` — the font transport relayed through the host.
- `src/program.rs` — `run`, for a bundle's `Run` (feature `program`).
- `src/testing.rs` — a host for a scene's own tests (feature
  `test-support`).

The design is `plans/SCREENSAVERS.md`; the page is `docs/src/lib/saver.md`.
