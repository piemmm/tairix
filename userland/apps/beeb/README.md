# tairix-beeb

`beeb.app`: a BBC Micro Model B, cycle-exact, with clean-room MOS, BBC BASIC
and DFS ROMs written in Rust. The design and its staging are
`plans/BEEB.md`; the user-facing description is `docs/src/desktop/beeb.md`.

Stability: `experimental`.

- `cpu`, `machine` — the NMOS 6502 and the Model B's bus and devices, each at
  the cycle of every access that can observe it.
- `media`, `snapshot` — disc and tape images and whole-machine snapshots,
  every one decoded fail-closed against fixed bounds.
- `rom` — host-only: the 6502 builder and the three ROM sources the images in
  `firmware/` are built from (`cargo xtask beeb --write-roms`).
- `firmware` — those images, compiled into `Run`.
- `worker` — the protocol between the window and the sandboxed machine, the
  worker's service and the owner's client.
- `front` — the window's host-tested model: key mapping, pacing, the media's
  records and the saves waiting for the picker, the picture's colours, the
  menu, the settings and the settings window's sheet.
- `scenario` — host-only: the recorded corpus's scenario format and runner.
- `src/run.rs` — the bundle's `Run`: the window, and the same binary
  re-entered as the capability-empty machine worker.

It holds no `unsafe` code. Capabilities: `CAP_CONSOLE_WRITE`, `CAP_SHM`,
`CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT`; no filesystem capability.
