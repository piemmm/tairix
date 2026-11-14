# tairix-openmicro

`OpenMicro.app`, the Open Micro Emulator: cycle-exact BBC Micro Model B and
Acorn Electron machines, the Electron's Plus 1 among them, with clean-room
MOS, Electron OS, BBC BASIC, DFS and Plus 1 ROMs written in Rust, run over a
library of machines. The design and its staging are `plans/BEEB.md`; the
user-facing description is `docs/src/desktop/openmicro.md`.

Stability: `experimental`.

- `cpu`, `machine` — the NMOS 6502, what every model shares, and each model's
  bus and devices (`machine::bbc`, `machine::electron`), each at the cycle of
  every access that can observe it.
- `media`, `snapshot` — disc and tape images and whole-machine snapshots,
  every one decoded fail-closed against fixed bounds.
- `rom` — host-only: the 6502 builder and the ROM sources the images in
  `firmware/` are built from (`cargo xtask openmicro --write-roms`).
- `firmware` — those images, compiled into `Run`.
- `worker` — the protocol between a machine's window and its sandboxed
  machine, the worker's service and the owner's client.
- `front` — the host-tested model the windows are built on: a machine as the
  library keeps it, its settings, the new-machine assistant's offers, its
  folder, key mapping, pacing, the media's records and the saves waiting for
  the picker, the picture's colours, and the menus.
- `scenario` — host-only: the recorded corpus's scenario format and runner.
- `src/run.rs` — the bundle's `Run`: the library and each machine's window,
  their store and file workers, and the same binary re-entered as the
  capability-empty machine worker. The library itself is `lib/machines`'.

It holds no `unsafe` code. Capabilities: `CAP_CONSOLE_WRITE`, `CAP_SHM`,
`CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT`; no filesystem capability, its machines'
discs and tapes being made in its own documents folder.
