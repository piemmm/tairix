# tairix-riscos-app

`TAIRiXBridge.app`, TAIRiX Bridge on the desktop (`plans/RISCOS-EMULATOR.md`
§15.3): the front end, and — re-entered as a capability-empty worker — a
session itself. The description is `docs/src/desktop/tairix-bridge.md`.

Its notices are [the bridge's](../../README.md).

Stability: `experimental`.

The library is host-tested:

- `memory` — a session's memory divided: its worker's ceiling, the guest's
  RAM within it, the decoded pages and the slot each launch is given.
- `begin` — the real time and the zone a session starts with.
- `provenance` — which session a launch runs in.
- `screen` — a context's screen as its window shows it, the cursor and the
  pointer drawn over the guest's lines.
- `input` — the keys, the buttons and the pointer a screen gives its guest.
- `bar` — a screen window's bar and its menu.
- `tasks` — a session's window, the Task Manager's view of it.
- `settings` — the application's settings and the form editing them.
- `worker` — the worker's half of a session: the kernel made with every
  module.
- `events` — the diagnostic records.

`src/run.rs` is the bundle's `Run`: the front end's windows, sessions and
workers (`src/run/`), and the session-worker role. It holds no `unsafe` code.

It requests `CAP_SANDBOX_SPAWN` and `CAP_SHM` to start each session's worker
with its screens' ring, `CAP_CONSOLE_WRITE` for its diagnostics on standard
error, and `CAP_LOG_EMIT` for its records.
