# `tairix-buildcfg`

The one definition of the `freestanding` cfg. A program crate compiles two
ways: as a freestanding binary on a bare-metal target, and as an inert stub on
the host, where clippy and the host test suite build the whole workspace.
Which form a build takes is a target decision, so it lives in build glue
rather than in source, and it keys off the target's operating system alone so
no gated source names an instruction set.

| Item | What it is |
|---|---|
| `FREESTANDING` | The cfg name, `freestanding`. |
| `NATIVE_ARCHES` | The native Tier-1 instruction sets: `x86_64`, `aarch64`, `riscv64`. |
| `is_freestanding(os)` | Whether a target whose operating system is `os` is bare metal (`none`). |
| `native_arch_of(os, arch)` | The native instruction set `arch` names on a bare-metal `os`, or `None` — on the host, the browser target, or any other set. |
| `target_is_freestanding()`, `native_arch()` | The same two questions about the target cargo is building, for a build script (`std` feature). |
| `emit_freestanding()` | Called from a build script: declares the cfg, enables it on a bare-metal target, and has cargo rerun the script only when the script changes (`std` feature). |

A crate's build script is the one line:

```rust,ignore
tairix_buildcfg::emit_freestanding();
```

A fixture program that its vertical compiles with pinned constants adds its own
`cargo:rerun-if-env-changed` lines after it. A crate whose code differs per
instruction set — a syscall trap, a start trampoline, a register probe — names
the set in its own cfg, `<prefix>_{arch}` for the `native_arch()` it is built
for, so the instruction-set choice is made in build glue and the list of native
sets has one definition.

It runs only on the build host — in build scripts, and in the host tools that
share a build script's logic — and links into no TAIRiX binary. The
classification is `no_std`, so the crate builds for every target the workspace
is linted for; reading cargo's environment and printing directives need `std`,
which a build script's dependency enables through the `std` feature. It has no
dependencies, so a fixture program rebuilt inside a vertical's nested
cross-compile pays nothing for it.

The QEMU integration harness (`tests/integration/harness`) and the kernel
binary's build script classify through the same functions, the harness adding
its per-port `itest_*` names and the kernel its `kernel_isa`
([Modularity](../architecture/modularity.md)).
