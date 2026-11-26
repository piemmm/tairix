# tairix-riscos-filing

TAIRiX Bridge's filing: FileSwitch, which every filing SWI reaches
through the filing vectors, and the filing systems it calls through their entry
points. The design and its staging are `plans/RISCOS-EMULATOR.md` §6; the
description is `docs/src/desktop/tairix-bridge.md`.

Its notices are [the bridge's](../../README.md).

Stability: `experimental`.

- `fileswitch` — FileSwitch: the default of the seven filing vectors while it
  is active.
  - `system` — the registered filing systems, each one's information block
    copied with its entries made absolute, and the call into an entry, guest
    code or a built-in module's Rust alike.
  - `name` — names parsed into a filing system, a special field and a path,
    and the path made absolute against the filing system's directories, its
    wildcards matched, as RISC OS 5's FileSwitch does before a filing system
    sees it; path variables.
  - `open` — open files: handles from 255 down, each held while its filing
    system opens it, one buffer a file of its natural block size, whole
    blocks moved straight to and from the caller's memory.
  - `OS_File`, `OS_Find`, `OS_Args`, `OS_BGet`, `OS_BPut`, `OS_GBPB` and
    `OS_FSControl`, each in its own module.
- `devicefs` — DeviceFS, `devices:`: the devices drivers register and the
  streams opened on them, a byte at a time or through BufferManager.
- `systemdevices` — SystemDevices: `null:`, `printer:`, `vdu:`, `rawvdu:`,
  `kbd:` and `rawkbd:`.
- `pipefs` — PipeFS, `pipe:`: named pipes between a reader and a writer, and
  `*PipeCopy`.
- `hostfs`, `filecore`, `ramfs`, `obey`, `bootcommands`, `boot`, `mimemap` —
  the session's files, FileCore and the RAM disc, Obey files, the boot
  sequence and its commands, and MIME type mappings.

What FileSwitch hands a filing system — a path, a special field, a block to be
filled — is built below the supervisor stack, so a filing system calling
FileSwitch again finds its own untouched.

It holds no `unsafe` code. `resources/` holds FileSwitch's message file, in the
emulator's own words; the errors it shares with the filing vectors' defaults
are the kernel's. `tests/corpus/` holds its recorded corpus: a logging filing
system's view of every call FileSwitch makes, recorded on RISC OS 5 and
replayed here.
