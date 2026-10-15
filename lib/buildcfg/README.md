# tairix-buildcfg

Stability tier: **experimental**

The one definition of the `freestanding` cfg. A program crate — a driver's
`Run`, a command or application bundle, a QEMU fixture program — compiles as a
freestanding binary on a bare-metal target and as an inert stub on the host,
where clippy and the host test suite build the whole workspace. Its build
script calls `emit_freestanding()`, which declares the cfg and enables it when
the target's operating system is `none`, keying off the operating system alone
so no gated source names an instruction set.

A crate whose code differs per instruction set names the set in its own cfg,
chosen from `native_arch()` — the native Tier-1 set a bare-metal build targets
— so the list of native sets is defined here once. The QEMU integration
harness (`tests/integration/harness`) and the kernel binary's build script
classify through the same functions.

It runs only on the build host — in build scripts, and in the host tools that
share a build script's logic — and links into no TAIRiX binary. The
classification is `no_std`, so the crate builds for every target the workspace
is linted for; `target_is_freestanding()`, `native_arch()` and
`emit_freestanding()` read cargo's environment and print directives, so they
are behind the `std` feature a build script's dependency enables.
