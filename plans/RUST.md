# RUST.md — The Rust toolchain on TAIRiX, and TAIRiX building itself

| ID | Item | Status |
|---|---|---|
| R1 | Source archive: every build input hash-pinned in an archive the project holds; builds run offline | planned |
| R2 | The TAIRiX toolchain: rustc, cargo and LLVM built from R1 by the project and used by every build | planned |
| R3 | The userland targets `x86_64-unknown-tairix`, `aarch64-unknown-tairix`, `riscv64gc-unknown-tairix`, with every userland program built on them | planned |
| R4 | `std` for the R3 targets, on the one TAIRiX runtime core | planned |
| R5 | Filesystem confinement: a process tree limited to granted subtrees, checked on the resolved node | planned |
| R6 | Build sessions: confined cargo, out-of-process proc macros, the build-artefact execution class | planned |
| R7 | Developer signing: a per-user publisher key and the user-store trust rule | planned |
| R8 | The toolchain's C runtime, written in Rust, and the pinned LLVM's libc++, libc++abi and libunwind | planned |
| R9 | The native toolchain, shipped as the `cargo.app` command bundle | planned |
| R10 | TAIRiX builds TAIRiX: images built on TAIRiX are bit-identical to the host's | planned |
| R11 | The toolchain rebuilds itself on TAIRiX to a fixed point | planned |
| R12 | Upstream: the targets, `std` and compiler changes offered to the Rust project | planned |
| R13 | Assurance: two-builder reproducibility and a diverse double-compile of the seed compiler | planned |

Binding under `AGENTS.md`. Read first: `plans/UNIVERSAL.md` (how programs reach
other machines; this plan adds a build host, not a channel), `plans/CCOMPAT.md`
(the OS-provided C runtime, which this plan does not widen), `plans/APPDATA.md`
(the publisher identity R7 reuses), `plans/SPAWN.md` and `plans/THREADS.md` (the
process and thread model `std` maps onto), `plans/FILELOCK.md` and
`plans/SYMLINKS.md` (what cargo's locks and hard links land on), and
`plans/FIX-HARDWARE-FEATURES.md` (the build-time feature floor).

Order: R1 → R2 → R3 → R4; R5, R7 and R8 alongside R4; R6 lands with R9, though
its compiler half (the proc-macro strategy) is testable on a host first; then
R10 → R11. R12 runs from R3 onward, R13 from R2.

## 0. The end state

On a TAIRiX machine, offline, from the R1 archive and the installed toolchain:

1. `cargo xtask build` and `cargo xtask image` produce, for every Tier-1 target,
   images bit-identical to the CI host's (§19.3).
2. The toolchain rebuilds itself from the archive to a fixed point: the
   compiler it builds builds a bit-identical compiler (R11).
3. Every stage of `cargo xtask ci` that needs no machine emulator, JavaScript
   engine or C compiler is green on TAIRiX. Four stay on a host by design: the
   QEMU verticals, the `node`-hosted wasm32 tests, the C-ABI conformance test
   (`tools/cc`), and loom (its `generator` dependency compiles C).

From then on, nothing outside the project's own machines and archive is needed
to go on building TAIRiX.

## 1. Decisions (binding)

1. **The upstream compiler, never our own.** The charter scopes the compiler out
   of roll-your-own (§2.12, §19.9). Matching rustc's front end, trait solver,
   borrow checker, const evaluator and macro system is decades of work against a
   moving target; gccrs, a funded multi-year effort, is not yet a drop-in, and
   mrustc can build rustc but performs no borrow checking. A young compiler is also a
   weaker trust root than the one the whole ecosystem tests. Trust in the
   compiler binary comes from reproducibility and a diverse double-compile
   (R13), not from writing it.
2. **LLVM is the backend on every builder, TAIRiX included.** Self-hosting means
   reproducing the host's image bit for bit, which only the same compiler with
   the same backend can do. LLVM also emits wasm32 (a Tier-1 target), assembles
   `asm!` and `global_asm!` itself (the kernel is full of both), and produces the
   code §2.16 asks for. Rejected: Cranelift (no wasm32 output, inline assembly
   only through an external assembler, different output bits, a weaker
   optimiser), the GCC backend (a second C/C++ compiler family), and any backend
   of our own.
3. **One upstream for the native toolchain.** rust-lang/rust at the pinned
   commit, with the LLVM it carries (rust-lang/llvm-project: LLVM, lld, libc++,
   libc++abi, libunwind). Rust is MIT OR Apache-2.0 and LLVM is Apache-2.0 WITH
   LLVM-exception; both are irrevocable for the archived versions, so a
   relicensed or abandoned upstream costs at most a fork the project already
   knows how to build. No other compiler, linker or runtime project is
   load-bearing. Rejected as dependencies: the wild linker and gitoxide (each
   essentially one maintainer), Cranelift (decision 2), relibc and wasi-sdk
   (decisions 8 and 9).
4. **Userland gets real OS targets; the kernel stays bare-metal.** The three R3
   triples have `target_os = "tairix"` and no `target_family`, because TAIRiX is
   not POSIX. They are PIE, `panic = abort`, with frame pointers, `rust-lld`, the
   TLS model the per-thread pointer supports, and inline stack probes. Every
   userland program moves, `no_std` or `std`, drivers and services included; the
   kernel and boot stay on `*-none` (x86_64 on `x86_64-tairix-none`). The target
   states once what `tools/xtask/src/commands/pie_build.rs` injects as flags
   today, and an honest OS identity lets ecosystem crates detect TAIRiX: a TAIRiX
   backend for CPU-feature detection is the userland half of
   `plans/OPEN-DEFECTS.md` D363. The wasm32 app-tier target is
   `plans/UNIVERSAL.md` U2's. Rejected: `target_family = "unix"`, which would put
   a POSIX personality under every Rust program, promise `fork`, `exec`, setuid
   and `/tmp` semantics that do not exist, and make R8's runtime OS-critical; and
   staying on `*-none`, where `std` is impossible and nothing can tell a TAIRiX
   program from bare metal.
5. **`std` sits on the one runtime.** `std`'s platform layer is built on the
   crates every TAIRiX program already uses:
   - `lib/abi`: syscall numbers and layouts;
   - `lib/abi-trap`: the one trap;
   - the runtime core of `lib/rt`: start-up, the §25 heap, threads, and the fd 0–3 streams of §20;
   - `lib/path`: path spelling;
   - `lib/cmdres`: program lookup (§16.8).

   They link through `rustc-dep-of-std`, the way Motor OS links `moto-rt` and
   Hermit links `hermit-abi`. No second heap, trap, start path, path grammar or
   lookup order exists. `std::os::tairix` carries what has no portable spelling:
   fd 3 `stdinfo`, aliases, capabilities and wait-sets. `lib/*` crates stay
   `no_std`.
6. **The in-tree overlay is the definition; upstream is a convenience.** Files
   the series adds to the upstream tree (the target specs, the `std` platform
   layer) live in-tree as reviewed Rust under `tools/toolchain/overlay/`. Edits
   to existing upstream files form a minimal patch series beside it. R12 offers
   both upstream as a Tier 3 target, with the project lead as its named target
   maintainer. Nothing waits on acceptance, and nothing depends on rustup or
   static.rust-lang.org distributing anything for TAIRiX: if upstream rejects
   the target or later removes it, no TAIRiX build changes.
7. **Source sovereignty.** Every build input is held by hash in an archive the
   project controls (R1):
   - the rust-lang/rust source, with its LLVM and its vendored dependencies;
   - the workspace's vendored crates;
   - the Raspberry Pi firmware blobs that `cargo xtask image` fetches with `curl` today;
   - the third-party tools the gate runs (cargo-deny, cargo-llvm-cov, mdBook),
     and a snapshot of the RustSec advisory database `cargo deny` reads;
   - the stage0 compiler.

   Builds are offline and hermetic; fetching is a separate, explicit, host-side
   sync. Once R2 lands, each stage0 is the project's own previous toolchain.
8. **Native, not Wasm-hosted.** Running the toolchain as a U2 Wasm app is
   rejected:
   - a 4 GiB memory ceiling per compilation;
   - a Wasm-hosted rustc cannot load proc macros without deep compiler changes;
   - Wasm threading is immature;
   - it would depend on U2.
9. **LLVM's C and C++ get a C runtime written in Rust.** LLVM, lld and libc++
   are third-party C++, and §1 forbids writing C or C++, not hosting it. They
   need a C library, which TAIRiX writes in Rust (R8):
   - its headers are generated from its Rust source by the existing C-header
     generator, so no header is hand-written (§1, §9);
   - its scope is the toolchain's link closure: the toolchain's undefined-symbol
     set is its specification;
   - it is linked statically into the toolchain's own binaries;
   - libc++, libc++abi and libunwind come from the pinned LLVM.

   It is a library the toolchain brings itself (§16.5), not an OS-provided class.
   `plans/CCOMPAT.md`'s minimal OS runtime stands, and offering this runtime to
   other programs would be a separate §16.4 decision. Rejected: musl and newlib
   (C, with an OS layer we would write in C or fake as Linux), relibc
   (Redox-shaped and external), and LLVM's libc (its OS layer is C++ we would
   write).
10. **Builds are confined, not trusted.** On Linux and macOS a build script or
    proc macro runs with the user's whole authority, and that is a live
    supply-chain attack. On TAIRiX a build session (R6) may read the workspace,
    the archive and the toolchain and write its target directory, and nothing
    else: no `CAP_NET`. Every process it starts holds at most that (R5, §5.2).
    Proc macros run in a capability-empty host (the `CAP_SANDBOX_SPAWN` shape)
    that speaks rustc's proc-macro bridge over IPC, never inside the compiler.
11. **What a build produces runs only inside that build.** Build scripts, test
    binaries and proc-macro libraries are build artefacts. They are unsigned and
    are started only by their own session, from its target directory, as a
    descriptor, holding at most its authority. Anything that runs outside a
    session (`cargo install`, `cargo run`) is signed first (R7). No developer
    mode switches signature checks off.
12. **Locally built programs are signed by the user's developer key.** The key
    is a per-user publisher key, held by a key service and never readable by a
    program, and certified by the installation's local capability authority
    (§11). The load gate admits such a bundle only from that user's own stores,
    launched as that user, under that user's grant ceiling. The system stores
    keep admitting only the build's anchor (`docs/src/lib/appload.md`).
    APPDATA's publisher pin applies unchanged, so another publisher claiming the
    same bundle id cannot read its data.
13. **cargo on TAIRiX never touches the network.** Sources come from the
    archive's vendored set. cargo is built for TAIRiX with its network transports
    compiled out, so curl, libgit2, libssh2 and OpenSSL are never built for
    TAIRiX; that is a patch in the series, offered upstream. Bringing in a new
    crate is an R1 sync, not a build step.
14. **One toolchain bundle.** `cargo.app`, kind `command`:
    - `Run` is cargo;
    - `Code/` holds rustc, rustdoc, clippy-driver, rustfmt, cargo-clippy, cargo-fmt, the proc-macro host and rust-lld;
    - `Libraries/` holds the `librustc_driver` they share and the `std` it links;
    - `Resources/` holds the target sysroots.

    Each `Code/` binary loads through the same gate as `Run` (§16.5). rustc is
    reached through cargo (`cargo rustc`, and the `RUSTC` cargo exports), not as
    a second bundle carrying a second copy of the driver. An image includes the
    toolchain only when built with it; images without it are unchanged.
15. **One codebase for the build tools on every host.** `tools/xtask`,
    `tools/mkimage`, `tools/syshelp`, `tools/tzcompile` and the rest also build
    for the R3 targets. Their Unix-only parts, such as the process-group kill in
    `xtask`'s parallel runner, move behind `std::os::tairix`. The build path
    spawns no shell and fetches nothing.

## 2. Stages

### R1 — The source archive

- `tools/toolchain/sources.toml` holds the SHA-256 of every decision-7 input,
  one entry each; `Cargo.lock`'s checksums remain the workspace crates' pins.
  `cargo xtask archive sync` (host-side, networked, explicit) fills a local
  content-addressed archive. `cargo xtask archive verify` checks it offline and
  refuses a missing, extra or altered entry.
- The workspace builds from an empty cargo home with no network, through
  `--offline` and source replacement onto the vendored set. `cargo xtask image`
  reads the firmware blobs from the archive instead of fetching them.
- §3 gains `tools/toolchain/` in this stage.
- Tests: a tampered, truncated or missing entry is refused; an offline build
  from an empty cargo home succeeds.

### R2 — The TAIRiX toolchain

- `cargo xtask toolchain build` runs rust's bootstrap (`./x dist`) on the
  archived source with the archived stage0. It produces one signed,
  reproducible tarball, keyed by the hash of its inputs (archive pin, overlay,
  patch series, bootstrap configuration), containing:
  - rustc and cargo;
  - `rust-std` for every target the tree builds, and `rust-src`;
  - clippy, rustfmt and miri;
  - `llvm-tools`.
- `rust-toolchain.toml` names this toolchain instead of an upstream nightly.
  Developers install the project's signed build from the archive rather than
  compiling LLVM themselves. The gate refuses a toolchain whose hash is not the
  pinned one, and a toolchain build runs only when its input hash changes.
- The stage ships with an empty overlay and series, proving the pipeline before
  any TAIRiX change rides on it.
- Tests: two builds of the same inputs are bit-identical; a toolchain one byte
  off its pin is refused.

### R3 — The userland targets

- Built-in target specs for the three triples, in the overlay
  (`compiler/rustc_target`), registered in the target list and therefore in
  `check-cfg`. Each carries decision 4's properties and the per-arch baseline
  its ABI needs. The build-time feature floor stays `xtask`'s policy
  (`plans/FIX-HARDWARE-FEATURES.md`).
- Every userland program builds for them: `image_apps` and `pie_build` select
  the triple, and `pie_build`'s flag injection is deleted. The C-ABI fixture's
  `tools/cc` triples follow.
- `lib/buildcfg` learns `tairix`, so the classification keeps its one home;
  `freestanding` keeps meaning "built for TAIRiX rather than the development
  host".
- §1 gains the userland triples in this stage.
- Tests: every bundle builds for each triple; the QEMU verticals pass unchanged;
  an image-build check refuses a userland ELF that is not PIE or carries an
  unexpected TLS model.

### R4 — `std`

- `library/std/src/sys/pal/tairix` and its siblings, on the crates of
  decision 5:
  - threads on `thread_create`, `futex_wait` and `futex_wake`, through `std`'s
    futex `Mutex`, `Condvar`, `RwLock` and `Once` (as on Linux, Motor and Hermit);
  - time on the monotonic clock and `Time64`;
  - files, directories, links and locks on the `fs_*` calls;
  - processes on `spawn`, with fd wiring and the reserved `LOAD_*` exit statuses;
  - pipes, and sockets on the netstack;
  - randomness on the random syscall;
  - CPU-feature detection on TAIRiX's feature query.
- Paths follow `docs/src/filesystem/drives.md` through `lib/path`: `Alias:/` and
  `id::` spellings are path prefixes, so `is_absolute`, `components` and `join`
  agree with the VFS.
- `env::temp_dir()` never names `/tmp` (§16.1). It is `$TMPDIR` when set, which
  every build session sets; otherwise it is a per-user, per-boot scratch
  location this stage defines with the owner of `plans/APPDATA.md`.
- `lib/rt`'s runtime core is factored so `std` and `no_std` programs share one
  start path, heap, thread start and stream model; `no_std` programs keep
  `tairix_rt::entry!`.
- ABI gaps that `std` exposes are closed in `lib/abi`, in place.
- Tests: `std`'s own test suite and the run-pass UI tests run on each triple in
  QEMU through rust's `remote-test-server`; host tests check the path rules
  against the VFS's own fixtures.

### R5 — Filesystem confinement

- A process may irreversibly restrict itself and every descendant to a set of
  rules, each a subtree with rights drawn from read, write, create, remove and
  execute.
  - Rules only narrow, and `spawn` inherits them.
  - They apply in addition to the owner/mode/ACL/`required_cap` model (§5.3),
    never instead of it.
- Rules are checked on the resolved node's place in the storage forest, after
  links and aliases resolve, so no spelling escapes: not `..`, a link, an alias
  root, an `id::` path, or a view (`docs/src/filesystem/drives.md`).
  - Both ends of a rename or hard link must lie inside the rules, so nothing can
    be moved or linked out.
  - Every refusal is audited (§19.4).
- The model is Landlock's (path-beneath rights, self-imposed, monotonic),
  applied to TAIRiX's forest of roots. It is not view projection: a narrowed `/`
  view confines nothing, because alias and id routes bypass it.
- This is a new kernel interface, so the stage writes its own plan before any
  code (§15.2) and adds that plan's jump-sheet row.
- Tests:
  - an escape through each route above is refused;
  - a confined parent cannot widen a child;
  - the rules survive `spawn` and thread creation.

### R6 — Build sessions

- `cargo.app` confines itself (R5) at start:
  - the workspace: read;
  - the vendored sources and the toolchain: read and execute;
  - the target directory, with `$TMPDIR` inside it: read, write, create and remove.

  It requests no `CAP_NET`, and resource limits (§24) bound its processes and
  memory.
- **Proc macros.** The series adds a cross-process `ExecutionStrategy` to
  `proc_macro`'s bridge.
  - The bridge already serialises every call into buffers and already runs a
    client on another thread over a message pipe (`MaybeCrossThread`); upstream
    keeps the trait public for strategies like this ("such as wasm
    proc-macros").
  - The compiler starts the proc-macro host capability-empty, with the macro
    library as a spawn-time descriptor, and dispatches the bridge over IPC. That
    descriptor library is the loader's one such case, honoured for that host
    alone.
  - There is no runtime `dlopen`, and a macro never shares the compiler's
    address space.
  - The cost is an IPC round trip per bridge call. It is measured on a
    macro-heavy fixture against the in-process strategy and recorded (§2.16):
    the price of keeping macros out of the compiler's address space.
- **Build artefacts.** The kernel tags the executables and libraries a confined
  session writes as that session's artefacts.
  - Only that session may start them, by descriptor, holding at most its
    authority.
  - They cannot be started from outside it, installed, or given more authority
    by anything they start.
- Tests: fixture build scripts and proc macros that try to read outside the
  workspace, open a socket, write outside the target directory, start an
  unconfined process, or escalate are each refused and audited. The build
  reports them as failures with their reasons (§2.24).

### R7 — Developer signing

- The local capability authority mints a per-user developer publisher key on
  first use and signs bundle digests for that user's attested sessions; the
  private key never leaves it. `cargo install` and `cargo run` sign into that
  user's own stores (§16.3).
- The load gate's trust rule gains one case: a bundle signed by user U's
  developer key loads only from U's stores, and only when U launches it. The
  system stores' rule is unchanged.
- Tests: refused from a system store, for another user, with altered contents,
  or under a revoked key; accepted in its owner's store.

### R8 — The toolchain's C runtime

- A new `lib/*` crate, in Rust (§3 and `PLAN.md` are updated in this stage),
  exporting exactly the toolchain's undefined-symbol closure:
  - `malloc` and friends on the §25 heap;
  - `pthread_*` on TAIRiX threads and futexes;
  - stdio on fd 0–2;
  - files and directories on `fs_*`;
  - time on the clock and `Time64`;
  - `posix_spawn` on `spawn`;
  - `errno`, the string and conversion functions, `printf` and `strtod`;
  - the maths LLVM calls.

  It provides no `fork`, no `/tmp`, no locale but `C`, and no signals beyond
  those the pinned LLVM cannot be configured without.
- Its headers are generated at toolchain build time from the crate by the
  C-header generator, and are never committed or hand-edited. LLVM, lld and
  libc++ build against it in the smallest configuration the pinned LLVM accepts:
  crash handlers, localization, wide characters and the filesystem library are
  off wherever it builds without them.
- A `cargo xtask toolchain closure` stage fails if the toolchain needs a symbol
  the runtime lacks, or if the runtime exports one nothing needs (§2.3).
- Its FFI `unsafe` is miri-enrolled wherever the host can run it (§19.11).
  `printf`, `scanf` and `strtod` have fuzz harnesses (§19.6). musl's `libc-test`
  runs as an external conformance corpus, on the host and on TAIRiX.

### R9 — The native toolchain

- rust's bootstrap cross-builds the R2 components with host set to each R3
  triple, packaged as decision 14's `cargo.app`. Its manifest requests the
  smallest capability set the stage can argue, each capability named with its
  use, and never `CAP_NET`.
- The compiler's and cargo's platform code gains TAIRiX implementations:
  - stack growth (rust-lang/stacker and its `psm`) on `mem_map`;
  - the jobserver on inherited pipes;
  - file locks on `plans/FILELOCK.md`;
  - artefact uplifting on hard links;
  - freshness on `Time64` mtimes;
  - sysroot discovery mapped onto the bundle layout;
  - crate-metadata reading on what TAIRiX's memory interface offers.
- Tests: in QEMU on each architecture, `cargo.app` builds a `std` program, a
  `no_std` TAIRiX bundle, and a crate whose proc macros run (`zerocopy-derive`
  and `curve25519-dalek-derive` are already in the workspace's own graph).

### R10 — TAIRiX builds TAIRiX

- Decision 15 plus the archive's offline inputs: `cargo xtask build` and
  `cargo xtask image` run on TAIRiX, first in QEMU and then on hardware. Their
  images are compared byte for byte with the CI host's, and any difference is a
  reproducibility defect, fixed at its source (§19.3).
- Its resource needs (memory, disk, time) are measured and documented, never
  assumed; a host's multi-target `target/` runs to well over 100 GB. The
  filesystem's share of the load (hundreds of thousands of small files, hard
  links, locks, renames) is exercised as part of the stage.
- The gate's third-party tools build from the archive for the R3 targets.
  `cargo deny` reads the archived advisory snapshot, and the stage fails closed
  on a snapshot older than the shortest advisory SLA (seven days, §19.3), so an
  offline gate cannot hide an advisory by going stale.
- §0's host-only remainder is listed in `docs/src/contributing.md`.

### R11 — The fixed point

- On TAIRiX, the installed toolchain builds the toolchain from the archive, and
  the compiler it produces builds it again; the two builds are bit-identical.
  The result is archived as the next stage0.

### R12 — Upstream

- The changes are offered to their owners:
  - to the Rust project: the R3 targets and R4's `std` as a Tier 3 target, with
    the project lead as named maintainer;
  - also to the Rust project: the cross-process proc-macro strategy, the
    compiler's platform ports, and cargo's offline build;
  - to each ecosystem crate's owner: its TAIRiX port.
- Upstream `std` cannot link path crates, so R4's dependencies are published to
  crates.io for that purpose, as Motor's and Hermit's are. TAIRiX's own builds
  still read only the archive.
- A merged change leaves the overlay or series; an unmerged one stays. Both are
  rebased at every toolchain pin bump, the cadence of today's
  `rust-toolchain.toml` bumps.

### R13 — Assurance

- Two independent builders, on different host operating systems, produce
  bit-identical toolchains and images (§19.3).
- Building the pinned compiler from a full-source mrustc bootstrap chain yields
  the same bits as building it from the archived stage0. This diverse
  double-compile (Wheeler) rules out a self-propagating compromise in the seed
  binary. It runs on a host; mrustc and every intermediate source are archived,
  and nothing else depends on mrustc.

## 3. Out of scope (never build)

- A TAIRiX-written Rust compiler, codegen backend or linker.
- A POSIX personality for Rust programs (`target_family = "unix"` on TAIRiX),
  or R8's runtime offered to programs as one.
- A switch that disables signature checks for developers.
- Network access from inside a build session.
- A dependency on rustup, static.rust-lang.org, crates.io or any hosting service
  for a TAIRiX build.
- A machine emulator hosted on TAIRiX so the QEMU verticals can run there.
