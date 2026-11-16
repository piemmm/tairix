# tairix-openmicro

`OpenMicro.app`, the Open Micro Emulator: cycle-exact BBC Micro Model B,
Acorn Electron and Commodore 64 machines, the Electron's Plus 1 and the C64's
true 1541 drives among them, with clean-room MOS, Electron OS, BBC BASIC,
DFS, Plus 1, KERNAL, BASIC V2, character and 1541 DOS ROMs written in Rust,
run over a library of machines. The design and its staging are
`plans/BEEB.md` and `plans/C64.md`; the user-facing description is
`docs/src/desktop/openmicro.md`.

Stability: `experimental`.

- `cpu`, `machine` — the NMOS 6502, what every model shares, and each model's
  bus and devices (`machine::bbc`, `machine::electron`, `machine::c64` with
  its 1541s), each at the cycle of every access that can observe it.
- `media`, `snapshot` — disc, disk, tape, cartridge and program images and
  whole-machine snapshots, every one decoded fail-closed against fixed
  bounds.
- `rom` — host-only: the 6502 builder and the ROM sources the images in
  `firmware/` are built from (`cargo xtask openmicro --write-roms`).
- `firmware` — those images, compiled into `Run`.
- `worker` — the protocol between a machine's window and its sandboxed
  machine, the worker's service and the owner's client.
- `front` — the host-tested model the windows are built on: a machine as the
  library keeps it, its settings, the new-machine assistant's offers, its
  folder, key mapping, pacing, the media's records and the saves waiting for
  the picker, the picture's colours, and the menus.
- `scenario` — host-only: the recorded corpora's scenario format and their
  runners, the Model B's and the Commodore 64's (`scenario::c64`).
- `src/run.rs` — the bundle's `Run`: the library and each machine's window,
  their store and file workers, and the same binary re-entered as the
  capability-empty machine worker. The library itself is `lib/machines`'.

It holds no `unsafe` code. Capabilities: `CAP_CONSOLE_WRITE`, `CAP_SHM`,
`CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT`; no filesystem capability, its machines'
discs, disks and tapes being made in its own documents folder.
