# tairix-sysconfig

Stability tier: **experimental**.

The boot-time system-configuration store engine: the one definition of the
`/System/Settings/Configuration/system.conf` document — its line grammar,
the closed key registry, each key's typed value set, the bounded fail-closed
parser, and the canonical render. The registry's families and who reads each:

| Keys | Read by |
|---|---|
| `os.loginType` | the login service |
| `cache.*` | the kernel's cache-admission control (`kernel/core::syscfg`) |
| `net.*` | the network stack, delivered by the device manager and `configure` |
| `time.*` | the clock service, which follows the file |
| `input.mouse.debounce` | the kernel's seat |
| `audio.*` | the audio service, delivered by the device manager |
| `cooling.fan.*` | the cooling service, which follows the file (`plans/COOLING.md`) |

The settings service writes the store through this engine on behalf of the
`configure` command app; every consumer reads it through the same engine, so
producer and consumer can never diverge. A reader with no filesystem
authority — the desktop Settings application — reaches the same document
through the ungated `SYSTEM_CONFIG` System Information API query and parses
it with this engine (`tairix_procinfo::system_config`), so what a settings row
shows and what `configure` would set cannot disagree.

The crate performs no
I/O and holds no authority: file access goes through the secured VFS under
the caller's own kernel-attested identity, and the per-inode policy on
`/System/Settings` decides who may write.

`no_std` + `alloc` (the render); host-unit-tested in `src/lib.rs`.
