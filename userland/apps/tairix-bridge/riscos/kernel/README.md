# tairix-riscos-kernel

TAIRiX Bridge's kernel: the guest address space over the processor's
MMU, RISC OS 5's kernel SWIs performed in Rust over the guest's own memory, and
the observation corpus it is held to. The design and its staging are
`plans/RISCOS-EMULATOR.md` §3 and §4; the description is
`docs/src/desktop/tairix-bridge.md`.

Its notices are [the bridge's](../../README.md).

Stability: `experimental`.

- `os` — the core: the `Kernel`, the `Os` view every SWI works through, the
  `Host` it asks for time and input, calls into the guest, unwinding and the
  run.
- `memory` — the address space: its two layouts, the memory policy, task
  slots, and the dynamic-area manager.
- `swi` — SWI dispatch, names and numbers.
- `module` — the module chain: the `Builtin` seam the session's other crates
  implement their modules through, `Config::modules` passing them in, the
  inventory of RISC OS 5.30's ROM, guest modules in either profile and their
  incarnations, the service chains, module SWIs, `OS_Module` and the
  `UtilityModule`, whose command table holds the kernel's commands.
- `cli` — the command line: `OS_CLI` through `CliV`, aliases, prefixes,
  redirection and the command tables, and the kernel's commands: output and
  variables, the file commands, `*Help`, `*Configure` and `*Status`, and the
  Supervisor.
- `vectors`, `error`, `heap`, `input`, `output`, `release`, `corpus` — the
  software vectors, errors and their texts, `OS_Heap`'s heaps, the input
  events and buffers, character output and the VDU drivers' seam, the release
  the kernel answers as, and the observation corpus: its scenarios of SWI
  calls and the fixtures genuine RISC OS left.
- `resourcefs`, `resources`, `msgtrans` — ResourceFS, the Messages module
  holding the ROM's files, and MessageTrans.
- `territory` (the Territory Manager, and `territory::uk`), `international`,
  `keyboard` and `intkey` — territories, International, the kernel's key path
  and InternationalKeyboard with its layouts.
- The rest are subsystems of `Os`: events, callbacks, the clock,
  the kernel heaps, system variables, the environment, `OS_Byte`, `OS_Word`,
  CMOS, the information and memory SWIs, conversions, text and exceptions.

It holds no `unsafe` code. `resources/` holds each module's message file, in
the emulator's own words, and `resources/Wimp/Sprites22`, the
file-type sprites `cargo xtask riscos --write-sprites` draws from the desktop's
icon masters. `tests/corpus/` holds the scenarios and their fixtures, which
`tests/corpus.rs` pairs and replays on the kernel; `fuzz_areas`, `fuzz_swis`,
`fuzz_modules`, `fuzz_squeeze`, `fuzz_cli` and `fuzz_international` are its
fuzz harnesses, those that boot a kernel booting it through `tests/support/`.
