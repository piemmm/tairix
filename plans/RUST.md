# RUST.md — The Rust toolchain on TAIRiX, and TAIRiX building itself

| ID | Item | Status |
|---|---|---|
| R1 | `tairix-ld`: the userland targets' linker; `rxe` with a relocation table the kernel applies at a random base on every spawn | planned |
| R2 | Dynamic linking: executable file mappings granted on provenance, and a dynamic loader in the runtime | planned |
| R3 | Thread-local storage in executables and loaded modules | planned |
| R4 | Unwinding for the toolchain and for test builds; installed programs stay `panic = abort` | planned |
| R5 | Spawn: `Code/` entry points, a working directory per child, descriptors beyond fd 0–3 | planned |
| R6 | The load gate at toolchain scale: verify-on-read content, user-store caching, shared text pages | blocked: the §24.4 decision on the bundle bounds |
| R7 | Trust: anchors scoped by store, developer and release keys, the signing service, revocation | blocked: Stage 8's installer, and today's gate honouring the system anchor in user-writable stores |
| R8 | Process confinement over files, IPC, signals, information and descriptors; monotonic and inherited | planned |
| R9 | The source archive and the pinned build environment | planned |
| R10 | The project-built toolchain for each supported developer host | planned |
| R11 | The userland targets, and build classification by purpose rather than by OS | planned |
| R12 | `std` for the userland targets | planned |
| R13 | The bundle tool: one composer and signer for `xtask`, for cargo on TAIRiX, and for third parties | planned |
| R14 | The toolchain's C runtime, written in Rust | blocked: the §1 decision on driving third-party C builds |
| R15 | Build sessions: confined build execution, out-of-process proc macros, build artefacts | planned |
| R16 | The native toolchain, shipped as the `cargo.app` command bundle | planned |
| R17 | Reproducible images: H10-REPRO, the personalisation split, path and provenance inputs | planned |
| R18 | TAIRiX builds TAIRiX: images built on TAIRiX match the reference host's | planned |
| R19 | The toolchain's Rust half rebuilds itself on TAIRiX to a fixed point | planned |
| R20 | Upstream: the targets, `std` and compiler changes offered to the Rust project | planned |
| R21 | Assurance: per-triple reproducibility and diverse double-compiles of both compiler seeds | planned |

Binding under `AGENTS.md`. Read first:

- `plans/UNIVERSAL.md`: how programs reach other machines; this plan adds a
  build host, not a channel.
- `plans/CCOMPAT.md`: the OS-provided C runtime, which this plan does not widen.
- `plans/APPDATA.md`: the publisher identity and the `Temp/` scope.
- `plans/SPAWN.md` and `plans/THREADS.md`: the process and thread model; R3
  and R5 extend both.
- `plans/FILELOCK.md` and `plans/SYMLINKS.md`: what cargo's locks and hard
  links land on.
- `plans/FIX-HARDWARE-FEATURES.md`: the build-time feature floor.
- `plans/FIX-PROTECTION.md`: the hardening R1 completes for user images.
- `plans/TIMESYNC.md`: the clock provenance R9's freshness rule relies on.

Order:

- R1–R5 and R8 are OS work that stands on its own. It gives every program a
  random base, shared libraries, thread-local storage and a richer spawn.
- R9 → R10 → R11 → R12, with R13 alongside R11.
- R6, R7 and R14, then R15 → R16.
- R17 → R18 → R19.
- R20 runs from R11 onward, and R21 from R10.

## 0. The end state

On a TAIRiX machine, offline, from the R9 archive and the installed toolchain:

1. `cargo xtask build` and `cargo xtask image` produce, for every Tier-1 target,
   images whose reproducible content (R17) is bit-identical to the reference
   host's.
2. The toolchain's Rust half rebuilds itself to a fixed point (R19).
3. The gate runs on TAIRiX, except five stages that stay on a host by design:
   - the QEMU verticals, because TAIRiX hosts no machine emulator;
   - the `node`-hosted wasm32 tests, because it hosts no JavaScript engine;
   - miri, which cannot interpret the syscall trap and has no TAIRiX shims;
   - loom, whose `generator` dependency compiles C against a C library programs
     do not get (decision 9);
   - coverage, whose profiler runtime is C that needs one too.

From then on, building TAIRiX needs nothing outside the project's own machines
and archive, and neither does rebuilding the toolchain's Rust half. Three things
still need a host or the network:

- rebuilding LLVM itself (R19);
- the five host-only stages;
- the weekly RustSec sync (R9).

## 1. Decisions (binding)

1. **The upstream compiler, never our own.** The charter scopes the compiler
   out of roll-your-own (§2.12, §19.9). Matching rustc's front end, trait
   solver, borrow checker, const evaluator and macro system is decades of work
   against a moving target. gccrs, a funded multi-year effort, is not yet a
   drop-in; mrustc can build rustc but performs no borrow checking. A young
   compiler would also be a weaker trust root than the one the whole ecosystem
   tests. Trust in the compiler binary comes from reproducibility and
   diverse double-compiles (R21), not from writing it.
2. **LLVM is the backend on every builder, TAIRiX included.** It is built with
   the X86, AArch64, RISCV and WebAssembly backends.
   - Self-hosting means reproducing the reference host's image, which only the
     same compiler with the same backend can do.
   - LLVM also emits wasm32 (a Tier-1 target).
   - It assembles `asm!` and `global_asm!` itself, and the kernel is full of
     both.
   - It produces the code §2.16 asks for.

   Rejected:
   - Cranelift: no wasm32 output, inline assembly only through an external
     assembler, different output bits, and a weaker optimiser;
   - the GCC backend: a second C/C++ compiler family;
   - any backend of our own.
3. **One upstream for the native toolchain.** rust-lang/rust at the pinned
   commit, with the LLVM it carries (rust-lang/llvm-project: LLVM, lld, clang,
   libc++, libc++abi, libunwind).
   - Rust is MIT OR Apache-2.0 and LLVM is Apache-2.0 WITH LLVM-exception. Both
     are irrevocable for the archived versions, so a relicensed or abandoned
     upstream costs at most a fork the project already knows how to build.
   - The pinned rust-lang/rust and cargo bring their own transitive
     dependencies, vendored into the archive; the project adds none of its own.

   Rejected as additions: the wild linker, Cranelift, relibc and wasi-sdk.
4. **Userland gets real OS targets; the kernel stays bare-metal.** The three
   triples `x86_64-unknown-tairix`, `aarch64-unknown-tairix` and
   `riscv64gc-unknown-tairix` have `target_os = "tairix"` and no
   `target_family`, because TAIRiX is not POSIX. Each target:
   - is PIE, with frame pointers and inline stack probes;
   - has `dynamic_linking`, which rustc requires to produce or load a
     `proc-macro` crate on a host (R2);
   - uses the TLS model R3 provides;
   - links through `tairix-ld` (R1);
   - defaults to `panic = abort`, and supports unwinding for the toolchain and
     for test builds (R4).

   Every userland program moves onto them, `no_std` or `std`, drivers and
   services included. The kernel, boot and QEMU test kernels stay on `*-none`
   (x86_64 on `x86_64-tairix-none`). The wasm32 app-tier target belongs to
   `plans/UNIVERSAL.md` U2.

   Rejected:
   - `target_family = "unix"`, which would put a POSIX personality under every
     Rust program, promise `fork`, `exec`, setuid and `/tmp` semantics that do
     not exist, and make R14's runtime OS-critical;
   - staying on `*-none`, where `std` is impossible and nothing can tell a
     TAIRiX program from bare metal.
5. **`std` sits on one minimal runtime core.**
   - **The core** is `lib/abi`, `lib/abi-trap`, `lib/path`, `lib/cmdres`, and the
     start-up, heap, thread and stream core factored out of `lib/rt`. The heap's
     reclaim policy reaches it through a hook rather than a dependency, so
     `std`'s closure does not pull in `lib/log`, `lib/crypto` and the crypto
     crates behind them. The core links through `rustc-dep-of-std`, the way
     Motor OS links `moto-rt` and Hermit links `hermit-abi`.
   - **One instance.** A `std` program reaches the runtime only through `std`
     and `std::os::tairix`. Linking `lib/rt`'s own entry point or global
     allocator into a `std` program is refused at link time, because two copies
     of the core would claim the same fixed heap arena.
   - **No drift.** Images never use a prebuilt `std`: every image compiles
     `std` against its own tree's `lib/*` (`-Z build-std`). The one prebuilt
     `std` is the native toolchain's host `std` (R16), which serves only what
     runs during a build and matches the running system. R1 takes each image's
     CFI tag from the `lib/abi` it actually links, so a mismatch is a load
     refusal, never a wrong syscall.
6. **The in-tree overlay is the definition; upstream is a convenience.**
   - Files added to the upstream tree (the target specs, `std`'s platform layer)
     live in-tree as reviewed Rust under `tools/toolchain/overlay/`. Edits to
     existing upstream files form a minimal patch series beside it.
   - Overlay code obeys the charter like any other tree code, cfg-check
     included: per-arch choices go through `lib/abi-trap`'s build-script cfgs,
     as the trap itself does.
   - R20 offers the overlay upstream as a Tier 3 target, with the project lead
     as named target maintainer. Nothing waits on acceptance, and nothing
     depends on rustup or static.rust-lang.org distributing anything for TAIRiX.
7. **Source sovereignty, build environment included.** Every input that shapes
   a build is held by hash in an archive the project controls (R9):
   - the rust-lang/rust source with its LLVM and vendored dependencies;
   - the workspace's vendored crates;
   - the Raspberry Pi firmware blobs that `cargo xtask image` fetches with
     `curl` today;
   - the stage0 compiler of each pin;
   - the build environment: the host compilers, CMake, Ninja, and Python as far
     as rust's bootstrap needs it at the pin;
   - the third-party tools the gate runs (cargo-deny, cargo-llvm-cov, mdBook);
   - a RustSec advisory snapshot.

   Builds are offline and hermetic; fetching is a separate, explicit, host-side
   sync.
8. **Native, not Wasm-hosted.** Running the toolchain as a U2 Wasm app is
   rejected:
   - a 4 GiB memory ceiling per compilation;
   - a Wasm-hosted rustc cannot load proc macros without deep compiler changes;
   - immature Wasm threading;
   - a dependency on U2.
9. **LLVM's C and C++ get a C runtime written in Rust, and a clang that is the
   toolchain's own.** LLVM, lld, clang and libc++ are third-party C++; §1
   forbids writing C or C++, not hosting it.
   - TAIRiX writes their C library in Rust (R14). Its headers are generated
     from its Rust source by the C-header generator, never hand-written.
   - Its scope is the toolchain's link closure. A symbol the toolchain links
     but never calls on TAIRiX is a typed refusal.
   - It links statically into the toolchain's binaries and into the test builds
     a session runs, and never into an installed program: the bundle tool
     refuses one that links it.
   - clang, from the same LLVM, is the toolchain's internal C/C++ compiler. It
     compiles rustc's C++ glue and compiler-rt (R19), and serves the `cc` crate
     inside build sessions.
   - LLVM builds as a generic POSIX host, with no other OS's macros. Where LLVM
     needs code for an OS it has no generic path for, the stage stops and asks:
     no C++ may be written, not even upstream.

   Rejected:
   - musl and newlib: C, with an OS layer we would write in C or fake as Linux;
   - relibc: Redox-shaped and external;
   - LLVM's libc: its OS layer is C++ we would write.
10. **Build execution is confined; the user's own commands are not.**
    - rustc, build scripts, proc macros and test binaries run under R8
      confinement. They may read the workspace, the vendored sources and the
      toolchain, and write the target directory, with no `CAP_NET`. IPC,
      signals and information queries are confined too.
    - Each build script is confined further, to read-only sources and its own
      `OUT_DIR`, so one cannot tamper with another package's outputs.
    - cargo itself, and what `cargo run` and `cargo install` start, run with the
      user's authority, through the bundle tool and signing (R13, R7).
11. **What a build produces runs only inside builds of its own target
    directory.** Executables and libraries that build execution writes are
    artefacts of that confinement (its target directory and owner). The binding
    is to the target directory rather than to one session, so an incremental
    build reuses a build script compiled yesterday. Artefacts cannot be installed
    or given more authority, and there is no switch that disables signature
    checks.
12. **Trust is scoped by store.**
    - The system app-signing anchor and the first-party publisher are honoured
      only for bundles under the system stores.
    - A user's own stores admit bundles signed by that user's developer key.
    - Release signatures are detached from the reproducible content, so
      deterministic first-party signing and secret release keys stop being in
      tension.
    - Bundle ids in the first-party namespace are reserved to the first-party
      publisher. The bundle tool gives a local build of a first-party program an
      id of its own, so `plans/APPDATA.md`'s pin never locks the system app out
      of its data.
    - The signing service signs digests for its user's attested sessions and is
      not a capability authority: the kernel stays the only one
      (`plans/NEW-SERVICEMANAGER.md`).
13. **cargo on TAIRiX is offline and lives inside the target directory.**
    - Sources come from the vendored set, and every build is `--locked`.
    - `CARGO_HOME` sits inside the target directory, because the fixed home
      layout (§16.3) forbids `~/.cargo`.
    - cargo is built with its network transports, its git support and its SQLite
      global-cache tracker compiled out: a patch, checked against the pin.
      Bringing in a new crate is an R9 sync.
14. **One toolchain bundle.** `cargo.app`, of kind `command`:
    - `Run` is cargo.
    - `Code/` holds rustc, rustdoc, clippy-driver, rustfmt, cargo-clippy,
      cargo-fmt, the proc-macro host, `tairix-ld`, rust-lld and clang.
    - `Libraries/` holds `librustc_driver`, LLVM, and the `std` dylib they link.
    - `Resources/` holds the target sysroots.

    It has an icon and `Help/en-US` like every bundle (§10, §16.5). It needs
    `Code/` spawning (R5), a gate that admits a bundle this size (R6), and
    shared text pages so parallel rustc processes share the driver's code (R6).
    rustc is reached through cargo, not shipped as a second bundle with a second
    copy of the driver. Images without the toolchain are unchanged.
15. **One codebase for the build tools on every host.**
    - `tools/xtask`, `tools/mkimage`, `tools/syshelp`, `tools/tzcompile` and the
      rest build for the userland targets too.
    - `xtask`'s QEMU-driving commands become a host-only feature, so it builds
      without `tools/qemu` (which uses `std::os::unix::net`).
    - Its tests spawn fixture programs rather than `sh`, `sleep`, `false` and
      `kill`.
    - The soak runner moves from bash into Rust.
16. **Reproducibility defines what "the same image" is (R17).**
    - An image's reproducible content excludes its personalisation (volume
      identities, machine id, keys), which a separate step draws after the
      content is fixed.
    - Builds take the source identity and the epoch as inputs, rather than
      reading git or the clock.
    - Paths are remapped, so the bytes do not depend on where the tree sits.

## 2. Stages

### R1 — `tairix-ld`

- The userland targets name `tairix-ld` as their linker. It runs rust-lld, then
  turns the ELF into `rxe`.
  - Relocations are kept as a table the kernel applies at a base it draws on
    every spawn. Today every `rxe` is relocated once, at build time, for the
    single `USER_IMAGE_BIAS`, so every program loads at the same address on
    every launch.
  - The CFI tag comes from a section `lib/abi` emits into every image. Today the
    converter stamps the tag from the tree it was built from.
- It replaces every converter: the test harness's `elf2rxe`, the image
  composer's conversion, and the kernel build's embedded fixtures.
- It ships in every toolchain build (R10, R16).
- Tests:
  - each malformed input is refused (a symbolic relocation in an executable, an
    RWX segment, a missing tag);
  - two spawns of one program get different bases;
  - an image linked against another `lib/abi` is refused at load.

### R2 — Dynamic linking

- The kernel grants an executable mapping of a file (an extension of
  `file_map`) only when it can vouch for the file: a file of a verified bundle
  (R6), or an artefact of the caller's own build confinement (R15). Anonymous
  memory never becomes executable (§19.2).
- A dynamic loader in the runtime maps `needed` libraries and loads proc-macro
  libraries on request. It handles symbolic relocations, symbol lookup, RELRO,
  init/fini arrays, and frame registration for R4.
- Library resolution keeps §16.4's policy, extended by the build-artefact case
  alone.
- It needs its own plan before code (§15.2), with a jump-sheet row.
- Tests:
  - an unverified file, an out-of-policy reference or a malformed object is
    refused;
  - initialisers run in order;
  - RELRO pages are read-only after load;
  - a loaded library's TLS and unwinding work (R3, R4).

### R3 — Thread-local storage

- `PT_TLS` support:
  - for the executable and for libraries loaded at start: a static block per
    thread, in the architecture's TLS variant;
  - for libraries loaded later: dynamic TLS through `__tls_get_addr`.
- The kernel sets the thread pointer at spawn and at `thread_create` on every
  port, x86_64's `FS_BASE` included. Today `plans/THREADS.md` decision 8 leaves
  storage to "the next layer up"; this stage is that layer, and it amends
  THREADS.md when it lands.
- Tests:
  - per-thread isolation and initialisation images;
  - a dynamically loaded module's TLS;
  - C++ `thread_local` through libc++abi.

### R4 — Unwinding

- The userland targets support `panic = unwind`, though installed programs stay
  abort (the bundle tool refuses an unwinding bundle).
- The toolchain unwinds because rustc's fatal-error path needs it
  (`FatalError::raise` → `resume_unwind`, caught by `catch_fatal_errors`). Test
  builds unwind because the workspace has `#[should_panic]` tests and
  `catch_unwind` calls.
- The unwinder is LLVM's libunwind from the pinned LLVM.
  - `.eh_frame` and `.eh_frame_hdr` are kept; `lib/rt/Run.ld` discards them
    today.
  - Frames are found through linker-defined symbols for the executable, and
    through R2's frame registration for loaded modules.
- Tests: a panic unwinds across frames and libraries; an abort build carries no
  unwinder.

### R5 — Spawn

- `spawn` accepts `…/<Name>.app/Code/<binary>` entry points. Today it parses
  only `…/<Name>.app/Run`.
  - Each entry is verified as part of its bundle, attested with the bundle's
    identity, and granted under the same ceiling.
  - Reconciled with `plans/UNIVERSAL.md` U1's arch-keyed `Code/` entries.
- Each child gets its own working directory. cargo sets one per process, from
  many threads, and today a child inherits its parent's.
- A bounded list of descriptors can be passed beyond fd 0–3: the jobserver's
  pipes, and R2's library descriptors. These `lib/abi` changes are made in
  place, and `plans/SPAWN.md` is amended with them.
- Tests:
  - a `Code/` entry outside its bundle, or unverified, is refused;
  - concurrent spawns keep their working directories apart;
  - the descriptor list's bounds hold.

### R6 — The load gate at toolchain scale

- The pinned toolchain exceeds every bound the gate has:

  | | Toolchain | Gate limit |
  |---|---|---|
  | `librustc_driver` | 83 MB | 16 MiB per file |
  | LLVM | 140 MB | 32 MiB per bundle |
  | rust-src | 3,616 files, 11 levels deep | 4,096 files, depth 8 |

  The gate reads and hashes each bundle whole into kernel memory, caches only
  system stores, and copies every segment into private frames. That cannot
  scale to the toolchain.
- **Proposed:**
  - **Verify on read.** The manifest signs a hash tree over each file; the gate
    checks the manifest and the roots, and each page is checked as it is read
    (the fs-verity model). No bound then has to admit a whole large bundle into
    kernel memory.
  - **Caching.** Verified user-store bundles are cached like system ones.
  - **Shared text.** Executable file mappings are shared between processes.
- Blocked: the bounds are §24.4 validation bounds, and changing what they bound
  is a charter decision.

### R7 — Trust

- Today the system app-signing and publisher seeds are public constants
  (deliberately, for reproducible signatures), and one anchor admits bundles
  from any path. Any process that can write a user store can therefore mint a
  bundle the kernel attests as first-party.
- Decision 12 is the change. It also adds:
  - **A signing service** that holds each user's developer key and never
    exposes it;
  - **Keys certified by the installation's key**, which Stage 8's installer
    mints;
  - **A provisioned test key** for debug and test images;
  - **Revocation** through a signed list the gate consults.
- It needs its own plan before code.
- Blocked: `userland/system/installer` is a placeholder; and today's gate
  honours the system anchor in user-writable stores, which must be fixed first.

### R8 — Process confinement

- A process may irreversibly restrict itself and every descendant. The
  restriction only narrows, `spawn` inherits it, and it applies in addition to
  every existing check (§5.3, §5.4), never instead of one. It covers:
  - **files:** subtrees with rights drawn from read, write, create, remove and
    execute. They are checked on the resolved node's place in the storage
    forest, after links and aliases resolve, so neither `..`, a link, an alias
    root, an `id::` path nor a view escapes (`docs/src/filesystem/drives.md`).
    Both ends of a rename or hard link must lie inside the rules;
  - **IPC:** only the confinement's own endpoints and an explicit allow-list
    may be called or sent to;
  - **signals:** only the confinement's own processes may be signalled;
  - **information:** queries see only its own processes;
  - **descriptors:** only those minted inside it may be redeemed.
- The model is Landlock's, applied to TAIRiX's forest of roots, and every
  refusal is audited (§19.4). A narrowed `/` view confines nothing, because
  alias and id routes bypass it.
- It needs its own plan before code, with a jump-sheet row.
- Tests:
  - an escape through each route above is refused;
  - a parent cannot widen a child;
  - the rules survive `spawn` and thread creation.

### R9 — The source archive and the build environment

- `tools/toolchain/sources.toml` pins every decision-7 input by SHA-256;
  `Cargo.lock`'s checksums remain the workspace crates' pins.
  - `cargo xtask archive sync` (host-side, networked, explicit) fills a
    content-addressed archive.
  - `cargo xtask archive verify` checks it offline, refusing a missing, extra or
    altered entry.
- The workspace builds from an empty cargo home with no network, and
  `cargo xtask image` reads the firmware blobs from the archive.
- **Build environment.**
  - clang is built from the pinned LLVM by the archived host compiler, then
    again by itself, so the shipped bits depend on clang's source rather than on
    the builder's compiler.
  - CMake, Ninja and the Python bootstrap needs are archived and pinned.
- **stage0.** Each pin's upstream `src/stage0` compiler is archived. Once R10
  lands, a pin bump steps at most one release cycle, so the previous project
  toolchain can serve as stage0 (R21 checks it against upstream's).
- **Gate tooling.** QEMU's source and build dependencies, the LLVM release
  `tools/cc` uses, and node, puppeteer and Chrome are archived where their
  licences allow, and otherwise listed. They serve only host-only stages.
- **Advisories.** The RustSec snapshot is synced weekly. The gate refuses a
  snapshot older than the shortest advisory SLA (seven days, §19.3), judged by a
  clock whose provenance `plans/TIMESYNC.md` trusts; a clock with no trusted
  provenance fails the stage closed.
- §3 gains `tools/toolchain/` in this stage.
- Tests: a tampered, truncated or missing entry is refused; an offline build
  from an empty cargo home succeeds.

### R10 — The project-built toolchain

- `cargo xtask toolchain build` runs rust's bootstrap (`./x dist`) on the
  archive, inside the R9 environment.
  - It produces one signed, reproducible tarball per host triple.
  - The tarball is keyed on the archive pin, the overlay, the series, the
    bootstrap configuration and the build environment.
  - It contains:
    - rustc and cargo;
    - `rust-std` for the bare-metal targets, and `rust-src`;
    - clippy and rustfmt;
    - miri;
    - `llvm-tools`;
    - `tairix-ld`;
    - rust-analyzer with its proc-macro server, which IDEs use to expand proc
      macros and which must match the compiler.
  - No prebuilt `std` ships for the TAIRiX targets (decision 5).
- **Hosts.** Linux (x86_64 and aarch64) is the reference builder; everything it
  needs is in the archive. macOS toolchains are built with Apple's SDK, which the
  archive cannot hold, so macOS is a convenience host and never the reference
  for a reproducibility check.
- **Pin.** `rust-toolchain.toml` and the CI workflow name this toolchain, and
  developers install the project's signed build.
  - The gate refuses a toolchain whose hash is not the pinned one.
  - A toolchain build runs only when its key changes.
- The first build has an empty overlay and series, which proves the pipeline
  before any TAIRiX change rides on it.
- Tests: two builds of the same key are bit-identical; a toolchain one byte off
  its pin is refused.

### R11 — The userland targets and build classification

- Built-in specs for the three triples, in the overlay (`compiler/rustc_target`)
  and registered in the target list, so they are known to `check-cfg`. They
  carry decision 4's properties. The feature floor stays `xtask`'s policy.
- Every userland program builds for them, and `pie_build`'s flag injection is
  deleted. The C-ABI fixture's `tools/cc` triples follow.
- **Classification by purpose.** `lib/buildcfg` gives two predicates:
  - bare-metal: `target_os = "none"`, for the kernel, boot and QEMU test kernels;
  - TAIRiX userland: `target_os = "tairix"`.

  They replace the single `freestanding`, which stops distinguishing a TAIRiX
  build from a host build once TAIRiX is itself a development host. The 28
  direct `target_os = "none"` checks in 11 `lib/*` crates move onto
  `lib/buildcfg`, and `lib/abi-trap`'s host test seam is chosen by test
  configuration rather than by target.
- The 92 program binaries become `test = false`; their logic is tested through
  their `[lib]`. That way, `cargo test --workspace` on a TAIRiX host builds no
  test harness for a `no_main` program.
- §1 gains the userland triples in this stage.
- Tests:
  - every bundle builds for each triple, and the QEMU verticals pass unchanged;
  - an image-build check refuses a userland ELF that is not PIE or carries an
    unexpected TLS model.

### R12 — `std`

- `library/std/src/sys/pal/tairix` and its siblings sit on decision 5's core:
  - threads on `thread_create`, `futex_wait` and `futex_wake`, through `std`'s
    futex primitives (as on Linux, Motor and Hermit);
  - time on the monotonic clock and `Time64`;
  - files, links and locks on the `fs_*` calls;
  - processes on R5's `spawn`, with the reserved `LOAD_*` exit statuses;
  - pipes, and sockets on the netstack;
  - randomness on the random syscall;
  - CPU-feature detection on TAIRiX's feature query, the userland half of
    `plans/OPEN-DEFECTS.md` D363.
- **Paths.** `Alias:/` and `id::` paths are absolute on TAIRiX, through the
  platform's rules for `is_absolute` and `join`. No new `std::path::Prefix`
  variant is added (that enum is stable, exhaustive and Windows-shaped).
  `components()` yields the alias spelling as an ordinary component, and that
  divergence is documented; upstream's UEFI port treats `FS0:` the same way.
- **Temporary files.** `env::temp_dir()` is `$TMPDIR` when set, and every
  session sets it.
  - Otherwise it names an absolute location the VFS refuses to create in, so a
    write fails closed instead of landing in the working directory. The
    location is chosen with `plans/APPDATA.md`'s owner.
  - Scratch space outside a session is the app-data `Temp/` scope, reached
    through `std::os::tairix`, never a path.
- **Build per image.** `-Z build-std` must honour patching `std`'s TAIRiX
  dependencies to the workspace's crates; this is verified at the pin, and the
  stage stops and asks if not.
- **Ecosystem.** An inventory of the rustc, cargo, `xtask` and gate-tool
  dependency graphs finds the crates with no TAIRiX backend, and each gets one,
  upstreamed where the owner accepts it and patched in the archive otherwise:
  - `parking_lot_core` parks on the futex (its fallback spins, which §2.23
    forbids);
  - `getrandom` uses the random syscall;
  - `memmap2` uses `file_map`;
  - `mio` uses wait-sets.
- Tests: `std`'s suite and the run-pass UI tests run on each triple in QEMU,
  through rust's `remote-test-server`. They run in test images that trust a
  test-signing anchor for the binaries the server receives; shipping images
  never trust it.

### R13 — The bundle tool

- One shipped composer and signer, with no QEMU dependency: a `lib/*` crate plus
  `cargo bundle`. It is extracted from `tools/xtask/src/commands/image_apps.rs`
  and from the test harness, where the conversion and the PIE recipe live today.
- `xtask`'s image build, `cargo install` and `cargo run` on TAIRiX, and
  third-party developers all use it.
- **The manifest** is the crate's `AppInfo.toml`.
  - There is no default manifest, and an absent capability list requests nothing.
  - An icon and `Help/en-US` are required, as for every bundle.
  - A `service` or `component` cannot go into a user store.
  - A local build of a first-party program gets an id of its own (decision 12).
- **Signing** uses a release key, a third party's publisher key, or, on TAIRiX,
  the user's developer key through R7's service.
- Tests:
  - a bundle with no manifest, icon or help is refused;
  - an unwinding bundle or one linking R14's runtime is refused;
  - a local build of a first-party program gets a fresh id;
  - each signing path verifies at the gate.

### R14 — The toolchain's C runtime

- A new `lib/*` crate in Rust (§3 and `PLAN.md` are updated in this stage). It
  exports exactly the toolchain's undefined-symbol closure:
  - `malloc` and friends, on the §25 heap;
  - `pthread_*`, on TAIRiX threads and futexes;
  - stdio, on fd 0–2;
  - files and directories, on `fs_*`;
  - time, on the clock and `Time64`;
  - `posix_spawn`, on R5's `spawn`;
  - `errno`, the string and conversion functions, `printf` and `strtod`;
  - the maths LLVM calls.

  It provides no `fork`, no `/tmp`, and no locale but `C`. Symbols that are
  linked but never called are typed refusals, each tested as one.
- Its headers are generated at toolchain build time and never committed. LLVM,
  lld, clang and libc++ build against it in the smallest configuration the
  pinned LLVM accepts.
- A `cargo xtask toolchain closure` stage fails if the toolchain needs a symbol
  the runtime lacks, or if the runtime exports one nothing needs.
- Its FFI `unsafe` is miri-enrolled for the code that does not trap.
  `printf`, `scanf` and `strtod` are fuzzed. musl's `libc-test` runs in QEMU,
  for the functions the runtime provides only.
- Blocked: §1 permits committed C build glue only for `tools/cc` and its
  fixture. Driving LLVM's CMake configuration and the `libc-test` corpus from
  `tools/toolchain/` needs a §1 decision.

### R15 — Build sessions

- **Confinement.** cargo, running with the user's authority, starts all build
  execution under R8:
  - rustc and its proc-macro hosts read the workspace, the vendored sources and
    the toolchain, and write the target directory;
  - each build script writes only its own `OUT_DIR` (a cargo patch in the
    series);
  - test binaries read the workspace and write `$TMPDIR`, which lies inside the
    target directory;
  - none of them holds `CAP_NET`, and resource limits (§24) bound them.
- **Lockfile and sources.**
  - Builds run `--locked`, so cargo never rewrites `Cargo.lock`.
  - `cargo fmt` runs no build code.
  - `cargo fix` compiles under the same confinement as a build, and only cargo's
    own process applies its edits.
- **Proc macros.** The series moves proc-macro loading out of the compiler.
  - `rustc_metadata` and `rustc_expand` learn a crate's macros from a host
    process instead of loading the library themselves. This is a real compiler
    change, offered upstream (R20).
  - The bridge runs over IPC. It already serialises every call into buffers,
    and already runs a client on another thread (`MaybeCrossThread`).
  - The host loads the library through R2. It runs confined, reading the
    workspace, because macros read files and environment variables, and it
    gets the environment rustc forwards.
  - The cost, an IPC round trip per bridge call, is measured on a macro-heavy
    fixture against the in-process strategy and recorded (§2.16).
- **Artefacts.** The kernel tags what build execution writes with its
  confinement (decision 11). Only processes under the same confinement may
  start or load those files (R2).
- This stage's kernel interfaces (artefact tagging, the build-artefact loader
  case) go into R2's and R8's plans.
- Tests: fixture build scripts, proc macros and tests are each refused and
  audited when they try to:
  - read outside the workspace;
  - open a socket;
  - write outside their allowance;
  - signal or call outside the confinement;
  - run an artefact from another target directory.

  The build reports each as a failure with its reason (§2.24).

### R16 — The native toolchain

- rust's bootstrap cross-builds R10's components, plus clang, with the host set
  to each userland triple, packaged as decision 14's `cargo.app`.
  - Its manifest requests the smallest capability set the stage can argue,
    each capability named with its use, and never `CAP_NET`.
  - Its host `std` matches the system it ships with (decision 5).
- The compiler's and cargo's platform code gets TAIRiX implementations:
  - stack growth (rust-lang/stacker and its `psm`), on `mem_map`;
  - the jobserver, on R5's descriptor list;
  - file locks, on `plans/FILELOCK.md`;
  - uplifting, on hard links;
  - freshness, on `Time64` mtimes;
  - sysroot discovery, mapped onto the bundle layout;
  - crate metadata, on `file_map`.
- **Allocation.** The §25 heap is one process-wide lock over first-fit spans.
  rustc and LLVM's allocation throughput is measured against it, under a stated
  budget. If it fails, the heap evolves in place (with §25 amended), never by
  adding a second allocator.
- Tests: in QEMU on each architecture, `cargo.app` builds:
  - a `std` program;
  - a `no_std` TAIRiX bundle;
  - a crate whose proc macros run (`zerocopy-derive` and
    `curve25519-dalek-derive` are already in the workspace's own graph).

### R17 — Reproducible images

- H10-REPRO (`PLAN.md`) is a prerequisite.
- **Build inputs.**
  - Builds take the source identity (a hash of the tree) and `SOURCE_DATE_EPOCH`
    as inputs; the kernel's build stops running `git`.
  - Every build remaps its paths (`--remap-path-prefix` or trim-paths for the
    workspace, the archive and the toolchain). Today nothing does, and
    `pie_build` puts an absolute path into `RUSTFLAGS`.
- **Personalisation.** `mkimage` draws volume identities, the machine id and
  keys from `/dev/urandom` today.
  - That work moves to a personalisation step that runs after the reproducible
    content is fixed.
  - Shipping images personalise from an RNG, so no two images share a secret.
  - The comparison covers everything except that step's output, which it lists.
- Tests: two builds, in different directories on different machines, compare
  equal.

### R18 — TAIRiX builds TAIRiX

- With decision 15 and the archive's offline inputs, `cargo xtask build` and
  `cargo xtask image` run on TAIRiX, first in QEMU and then on hardware.
- **Reproducibility.** The images' reproducible content is compared byte for
  byte with the reference host's. A difference is a reproducibility defect,
  fixed at its source (§19.3).
- **The gate.** The gate's third-party tools build from the archive.
  `cargo deny` reads the archived advisory snapshot, under R9's freshness rule.
  §0's host-only stages are listed in `docs/src/contributing.md`.
- **Resources.** Memory, disk and time are measured and documented, never
  assumed; a host's multi-target `target/` runs to well over 100 GB. The
  filesystem's share of the load (small files, hard links, locks, renames) is
  part of this stage.

### R19 — The toolchain's Rust half rebuilds itself

- On TAIRiX, rust's bootstrap rebuilds rustc, `std`, cargo and the tools
  against the LLVM libraries R16 shipped.
  - It runs as its Rust binary, directly. If Python is still required at the
    pin, the stage stops and asks: porting Python is not in this plan.
  - The toolchain's clang compiles rustc's C++ glue (`rustc_llvm`) and
    compiler-rt.
  - The compiler it produces rebuilds them bit-identically, and the result is
    archived as the next stage0.
- LLVM itself is rebuilt only on project hosts.
  - Its build is configured by CMake, whose process layers spawn with `fork`.
    TAIRiX does not provide `fork`, and CMake may not be patched in C.
  - LLVM's reproducibility is guarded by R21 instead.

### R20 — Upstream

- **Offered to the Rust project:**
  - the R11 targets and R12's `std`, as a Tier 3 target with the project lead as
    named maintainer;
  - the out-of-process proc-macro change;
  - the compiler's platform ports;
  - cargo's offline build and its per-build-script confinement hook.
- **Offered to the ecosystem crates' owners:** their TAIRiX ports.
- Upstream `std` cannot link path crates, so R12's dependencies are published to
  crates.io for that purpose, as Motor's and Hermit's are. TAIRiX's own builds
  read only the archive.
- A merged change leaves the overlay or series, and an unmerged one stays. Both
  are rebased at every toolchain pin bump. Where upstream's conventions differ
  from the charter, the submission adapts; the tree does not.

### R21 — Assurance

- **Reproducibility.** Two independent builders produce bit-identical toolchains
  for the same host triple, on different host operating systems. Toolchains are
  per host triple, so there is nothing to compare across triples. Images built
  from them have bit-identical reproducible content.
- **The Rust seed.** Building the pinned compiler from a full-source mrustc
  chain yields the same bits as building it from the archived stage0. This
  diverse double-compile (Wheeler) runs on a host, and mrustc and every
  intermediate source are archived.
- **The C++ seed.** The pinned clang is built twice, once by GCC and once by the
  archived host clang. Each result rebuilds clang, and the two rebuilt clangs
  must be bit-identical.

## 3. Out of scope (never build)

- A TAIRiX-written Rust compiler, codegen backend or linker. `tairix-ld`
  converts lld's output; it links nothing itself.
- A POSIX personality for Rust programs (`target_family = "unix"` on TAIRiX),
  or R14's runtime offered to installed programs.
- A switch that disables signature checks for developers.
- Network access from build execution.
- A dependency on rustup, static.rust-lang.org, crates.io or any hosting service
  for a TAIRiX build.
- A machine emulator hosted on TAIRiX.
- C or C++ written to make LLVM, CMake or any third-party component build on
  TAIRiX, upstream included.
