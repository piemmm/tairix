# tairix-saver-systemmonitor

**Stability tier:** `experimental`

System Monitor, a desktop screensaver: the machine's own readings at a glance
— its processors, memory, tasks, storage and network, and whatever needs
attention — set out to be read from across a room. It ships as
`/System/Screensavers/SystemMonitor.app`.

- `src/lib.rs` — the scene: readings adopted, staleness, the burn-in orbit.
- `src/board.rs` — where each part stands and what it draws, from the shared
  controls.
- `src/verdict.rs` — what the board says first, read from the monitor's own
  verdicts.
- `src/run.rs` — the bundle's `Run`: the scene served by `tairix_saver::run`
  in the drawing sandbox.

It asks for no capability. It declares the machine feed: the host relays the
Switchboard's reports, each busiest task's name already withheld as the
desktop's settings say, and says when the monitor stops. A task whose name is
withheld is drawn as whose it is. Between readings nothing is drawn; a
reading repaints the parts it changed, and the board steps round a small
orbit each minute against burn-in.

The design is `plans/SCREENSAVERS.md`.
