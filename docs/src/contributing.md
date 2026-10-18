# Contributing

All contributors — human or AI — must read the [`AGENTS.md`][agents] charter
before opening a pull request. It is binding; this page does not restate its
rules, it points to them.

## Workflow

0. Create the build's signing key set once per machine with
   `cargo xtask keys init`. Every build signs the kernel's drivers and the
   system's programs with it, the kernel trusts only what it signed, and a
   build without one fails: there is no shared fallback key. It is written
   outside the tree, readable by its owner alone, at
   `$XDG_DATA_HOME/tairix/signing-keys` (else
   `~/.local/share/tairix/signing-keys`); `TAIRIX_SIGNING_KEYS` names another,
   by absolute path. Images are reproducible given their key set.
1. Pick a stage from [`PLAN.md`][plan]. Do not begin a stage before its
   listed dependencies are complete.
2. Discuss non-trivial design changes in an issue first. Inventing public
   interfaces is forbidden; extend versioned ones instead.
3. Run `cargo xtask ci` locally before pushing. The same command runs in
   CI and must be green. It runs each test exactly once, on a developer
   machine and a CI runner alike ([§7][test]); the flake-hunting
   repetition lives in the time-limited GitHub soaks, not in `ci`.
4. Update documentation in the same commit as the code it describes —
   rustdoc on every public item and the relevant page in `docs/src/`.

## How long `cargo xtask ci` takes, and how to run it under a tool cap

Budget **about 52 minutes** on a warm `target/` and a two-dozen-core host —
the sum of the stage costs below — and substantially longer on a cold one,
where `-Z build-std` recompiles `core`/`alloc` per target and every image
profile links from scratch.

Every stage reports its own wall clock. `grep 'stage:' ci.log` gives the
per-stage totals the pipeline is ordered by; `grep 'done in' ci.log` gives the
finer per-command lines inside them — sequential steps and concurrent jobs
alike, so a job running at most of its budget is visible before a slower host
turns it into a kill. A job that is killed still prints the output it had
produced, which names the test it was in. A measured warm run:

| Stage | Cost | Shape |
| --- | --- | --- |
| `fmt --check` | 4 s | sequential, streams live |
| static gates (`deps-check`, `cfg-check`, `event-ids`, `driver-exits`, `charter-cite`, `source-text`, `spec-review`, `help-lint`, `devids`, `c-header`, `font-atlas`, `abi-check`, `model-check`, …) | 1 s makespan | concurrent group |
| `deny` | 1 s | sequential; reads `Cargo.lock`, compiles nothing |
| `proptest --once` | 3 s | one iteration, logged seed |
| `crypto-constant-time` | 5 s | `lib/crypto` re-run under release |
| `fuzz --once` | 10 s | one iteration per harness, logged seed |
| `loom` (the interleaving oracle over the sync primitives) | 10 s | one process per crate, concurrent |
| `docs-check` (rustdoc + mdBook + link check) | 68 s | sequential |
| `image` gate | 193 s over 319 spawns | sequential |
| `clippy` host + 2 feature-lane + 17 target passes | 330 s | sequential |
| `test --qemu` (host matrix + 226 guests + 3 fixture cross-compiles) | 681 s | guests concurrent, `nproc/3` weighted budget |
| `miri` (the UB oracle over the hand-written `unsafe` cores, the userland runtime and its C stubs, the three paging ports, `kernel/mem`, `kernel/core`, the DMA translation unit families, `lib/virtio` with the drivers that drive its rings, and the GENET frame carve) | 1836 s | one process per crate, concurrent; `lib/collections`'s and `kernel/core`'s libraries, `kernel/mem`, `lib/abi`, `lib/rt`, the aarch64 and x86_64 ports, the DMA translation API and its RISC-V, AMD-Vi and virtio-iommu families, and GENET dealt across the host's cores, at most 64 tests a shard |

Miri runs one interpreted thread at a time and reports a single CPU to the
program, so libtest takes a crate's tests one after another whatever the host
has. `kernel/mem`'s five hundred and more therefore came to twenty minutes in a single
single-core process — most of the stage, against a forty-five-minute per-job
budget it eventually overran on a slower runner, while the rest of the machine
idled. Such a target is `Spread::Sharded`: the stage enumerates it through the
test binary's own `--list` and deals the names round-robin across one process
per core, or more where that would put over 64 tests in one, so the makespan is
the heaviest shard rather than the sum. The partition comes from the binary
rather than a hand-kept list, so a test added later cannot fall outside every
shard and go uninterpreted.

Dealt one per core alone, a shard's share grew with the suite and with every
core the host lacked while its budget stayed fixed: `kernel/core`'s library,
dealt 24 ways on a developer machine, went 16 ways on the runner, and four of
its shards overran there (`plans/OPEN-DEFECTS.md` D1054). The bound gives a
large suite the same shards on every host with fewer cores than it has shards,
so the shard a runner overruns is the one a developer replays. The deal is still
blind to cost, so a test is kept
cheap to interpret. One that reaches its state through tens of thousands of
operations where a few dozen reach the same state is restructured — the frame
allocator's zone tests drain a zone a block at a time, not a frame at a time —
and a fixture is carried rather than made where making it is the cost: one
PBKDF2 derivation at the minimum accepted cost takes minutes interpreted, so
the account suites decode precomputed password records and derive only where a
login is the assertion, and those few leave the interpreter. A slower runner
found the cost again: `lib/collections` still ran whole in one process and
overran, and single tests took up to ten minutes interpreted, a shard holding
one spending four fifths of its job budget there (`plans/OPEN-DEFECTS.md`
D1154).

The cost that remains is the aliasing model, not the code: a zero-on-free clear
is a volatile write per byte, and the same `dma` test costs 716 s under Stacked
Borrows, 208 s under Tree Borrows and 44 s with the model off. Stacked Borrows
stands — it is the stricter of the two, and the one intrusive pointer code is
likeliest to violate — so where the interpreted extent is a sample rather than
the assertion it is scaled under `cfg(miri)`. Where the extent is the assertion
and the `unsafe` it reaches is reached elsewhere, the test is excluded by name
instead — a capacity, a whole one-mebibyte transfer, a poll budget spun out, a
batch of samples, a login's derivation, an AMD-Vi device table rewritten for
interrupt remapping — and so is a test that reaches no `unsafe` at all, such as
a port's scan of its own assembly sources. A fixture built a byte at a time,
free natively but minutes interpreted, is built in bulk instead. The heaviest were `kernel/mem`'s two
`dma` capacity tests, one streaming thirteen 32-page device regions through a
full-gigabyte window (four hours interpreted) and one carving a page past the
largest 32 MiB buddy block (some twenty times that), then `lib/abi`'s
single-byte sweep of the 2.8 KiB machine report and the aarch64 port's
redistributor that never wakes, half an hour each through safe code. The
registry names every exclusion and carries its reason.

Figures in this table are wall clock, taken from outside. Miri's own clock is
virtual: the `finished in …` line a test binary prints under the interpreter is
**not** wall time and can exceed it severalfold, so the runner's own per-job
line is the figure to read, never libtest's.

The order is that table, cheapest first, and it is maintained against
*measured* cost rather than a guess about which gate usually trips. A cheap
stage placed behind an expensive one makes every one of its failures pay the
expensive stage first for nothing: `deny` at one second once sat behind the
417-second test phase. Re-measure before reordering; that is what the
`stage:` lines are for.

The `image` gate's position is the one that looks like an exception and is
not. It reads like terminal assembly — the thing you do once what it packages
holds — but the gate only proves the image *builds*; it ships nothing, so
there is no untested-artefact risk to weigh against making every
image-breaking change pay 28 minutes of QEMU first.

The pipeline **cannot** be squeezed under ten minutes: the QEMU phase's
theoretical floor alone is around five minutes, and the stages above it are
serialised by cargo's build-directory lock, which makes concurrent `cargo`
invocations against one target directory wait rather than overlap.

That matters when an AI agent runs the gate, because an agent harness caps a
single tool call — ten minutes in Claude Code. A foreground call is **killed
mid-pipeline** at the cap and writes no exit status at all, so it is strictly
worse than useless: it burns the ten minutes and tells you nothing. Run it so
the process is tracked to exit and its status is recorded:

```sh
{ cargo xtask ci > ci.log 2>&1; echo "CI-RC=$?" >> ci.log; }
```

Then read `CI-RC=` from the log. That value is written only after the process
exits, so it is the real status — a wrapper's or a shell's exit code may be the
`echo`'s, and partial log output is not evidence of anything. Confirm the run
reached the end (the stage list finishes at `[miri]`, and the enrolled
and completed QEMU counts match) rather than judging by elapsed time.

This is the case [§7][test] names in "watch the gate to completion and report
only its real exit status". It is not licence to start the gate and move on:
finish every source and documentation edit first, so the run covers the tree you
will report on, do no other work while it runs, and stop the run if an edit
becomes necessary — its result would no longer describe the tree you report on.

## Flaky tests are defects — fix them, never re-run them

A test that fails intermittently is a **bug**, and it is fixed like any other
bug ([§7][test], [§2.5][agents], [§2.18][agents]). This is binding and has no
exceptions:

- **"Machine load" is never an excuse.** Do **not** dismiss a failure as
  "flaky because the machine was busy", "CPU contention", "an oversubscribed
  host", "a slow CI runner", or "it passes when I run it on its own".
  Re-running a failed test in isolation until it goes green is **not** an
  investigation and **not** a fix — it is the exact get-out the charter forbids.
- **Load exposes real bugs; it does not cause false failures.** Every time a
  failure in this project has been blamed on machine load, it has turned out to
  be a genuine defect — a race, an unsynchronised wait, an unbounded queue, a
  budget sized to an idle host, a missing completion signal — that the load
  merely revealed. Treat a failure that appears under load as a confirmed
  defect and find its root cause.
- **A green re-run is not evidence the defect is gone.** It proves only that
  the failure is intermittent, which is precisely the bug. Diagnose the *why*,
  fix the code or the test so it cannot recur under any load, and add a
  regression test ([§7][test]).
- **A load-dependent timeout is fixed structurally**, not retried: size the
  budget to the actual work, bound concurrency so guests do not oversubscribe
  the host, or add a completion signal ([§7][test]). See the CI soak notes in
  `tools/ci/README.md`.

Do not report work as done while any test has failed even once during the
change. If the real fix is genuinely too large for the current change, stop and
ask — never wave the failure through as transient, load, or environment.

## What `cargo xtask ci` runs

| Step          | What it does                                                |
| ------------- | ----------------------------------------------------------- |
| `fmt`         | `cargo fmt --all -- --check`                                |
| `clippy`      | `-D warnings` for the host **and once per Tier-1 target** (see below) |
| `deps-check`  | Enforces the [§17.4 modularity graph][modularity]           |
| `cfg-check`   | Rejects target-conditional `cfg` outside the arch ports, inside a freestanding port one that omits `target_os`, and anywhere but `tools/xtask/` an attribute `cfg` naming `miri` |
| `event-ids`   | Rejects an event id outside its crate's range in `tairix_log::registry`, a crate defining ids with no range, and one id defined by two crates |
| `driver-exits`| Rejects an exit code or `exit` module declared by a crate built on the driver runtime: every driver ends through `tairix_drvrt::exit` |
| `charter-cite`| Rejects a comment or package description citing a charter section instead of the reason ([§2.11][cite]) |
| `source-text` | Rejects a control byte other than tab and newline in source and documents, outside the planning and charter files |
| `test`        | `cargo test --workspace --all-targets`, then the packages the debug image's kernel diagnostics turn a feature on in, with those features (read from the manifests), then the QEMU matrix, run once ([§7][test]); each host pass starts in a randomised order, seed logged (see below); the scene tier is left to `ci-long` (see below) |
| `docs-check`  | `cargo doc` (deny warnings) + `mdbook build` (link checked) |
| `deny`        | `cargo deny --all-features check` (license + advisory)      |
| `supply-chain`| Source-hash allow-list + RUSTSEC advisory SLA ([§19.3][sc]) |
| `fuzz --once` | Runs each fuzz harness once, fresh+logged seed ([§19.6][fz]) |
| `loom`        | Model-checks the `lib/sync` primitives over every thread interleaving |
| `abi-check`   | Cross-checks the kernel syscall table against `lib/abi`     |
| `image`       | Builds every delivered image profile end-to-end (`debug` and `installer` for each image platform), so an image-breaking change cannot land green |

## The host tests run in a randomised order

A test suite is a set, not a sequence. The harness's default is alphabetical,
so a test that reads process-global state another test writes stays green for
as long as the two happen to start the right way round — and then fails when an
unrelated change renames or adds a test and reshuffles them. `kernel/core`'s
suite had accumulated more than a dozen such tests, four of which *hung*
rather than failed (`plans/OPEN-DEFECTS.md` D90).

The `test` step therefore starts each host pass in a fresh random order
(`cargo xtask test --shuffle`, which `ci` passes), so an order-dependent suite
fails the gate that introduces it. The seed is in the step's label:

```text
xtask: [test (order seed 1788601047185464875)] ...
```

Feed it back to replay exactly that order:

```sh
cargo xtask test --shuffle-seed 1788601047185464875
```

`cargo xtask ci-long`'s flake hunt gives each of its replicas an order of its
own too, so ordering is hunted alongside timing.

A test that depends on start order is a defect, not a configuration: fix the
test so it owns what it reads, exercise the whole lifecycle in one test, or
sequence the dependency explicitly — never by hoping for an order.

## The scene tier runs in `ci-long`, not `ci`

The ray tracer's long tests that compose or prepare a whole scene, the
screensaver's that prepare or trace one, and `WinterSun`'s that survey whole
realms cost more with every detail the scenes and the land gain. They build
only with their package's `scene-tests` feature, so neither the workspace pass
nor `ci` runs them. `ci-long` does, its concurrent
replicas one at a time since one run fills the machine, and so does:

```sh
cargo xtask test --scenes                             # the host matrix, then the tier
cargo test -p tairix-raytrace --features scene-tests  # one package's tier
```

The tier's packages are whichever declare the feature. `ci` still lints them
with it on, so the tier keeps compiling. A whole-scene test taking more than a
few seconds carries `#[cfg(feature = "scene-tests")]`, as does a survey of a
whole realm and the helpers only it uses; a quick one, and a test of one piece
built by hand, stays in `ci`.

## A test owns the ids it keys process-global state on

Shuffling finds an order dependence; it cannot find a *parallel* one. Several
registries the reclaim path scrubs are process-global and keyed on a task id —
the call-endpoint registry by endpoint owner and by call poster, the wait-set
table, the shared-region table — and `exit` reaches all of them, from tests
that hold no registry guard and need none. A test that hand-picks an id a
sibling reclaims therefore loses its own in-flight state mid-assert, at an
interleaving far too rare for a re-run to reproduce
(`plans/OPEN-DEFECTS.md` D147).

A `kernel/core` test therefore draws every id it keys such state on from its
own claim: `test_boot::claim_task` for the first principal and
`test_boot::claim_peer_task` for each further one. The claim issues a block of
ids from a range reserved far above every hand-written one, so the collision is
unrepresentable rather than a note each new test has to remember — and the
`registry_guard` helpers, which serialise a registry's *residents*, do not and
cannot cover it.

Per-CPU state is the same hazard keyed on a CPU: the preempt latch, the
published live space, running stack and resume handle, and the watchdog stamps
share one table, and every unpinned `TestArch` reports CPU 0. A test that pins
a CPU takes it from `test_boot::claim_cpu`, which never hands a slot out twice,
and runs on it through `TestArch::on_cpu`. The test publication helpers refuse
a CPU no test claimed, so a hand-picked one fails every run rather than one in
thousands.

## `clippy` lints every target, not just the host

A host-only `cargo clippy --workspace --all-targets` lints almost none of the
code that actually ships. A kernel subsystem, an architecture backend, a
driver, a system service and an application body are compiled only when their
crate is built for a bare-metal triple — most of them behind the `freestanding`
cfg each crate's `build.rs` sets when the target OS is `none`, whose host arm is
an inert stub. The image and QEMU stages then compile those bodies but never
lint them, so a lint in shipped code could not fail CI.

`clippy` therefore runs the same `-D warnings` pass once per target:

| Pass | What it covers |
| ---- | -------------- |
| host | `--workspace --all-targets`, including every unit-test target |
| host, **each feature lane** | `--all-targets` over the packages the debug image's kernel diagnostics reach, then over the scene tier, each with its lane's features on: configurations no other host pass builds |
| each of the three freestanding Tier-1 triples, **once per stratum** | `kernel/*`, then `lib/*`, then `drivers/*` + `userland/*` — every workspace member the image pipeline cross-compiles, less host-only `tools/*`, less `tests/*`, and less a foreign `kernel/arch/<other>` |
| each freestanding triple, **the kernel with the debug image's diagnostics** | the kernel stratum again with `KERNEL_DIAGNOSTICS_FEATURES` on, the only configuration those bodies are built in |
| each freestanding triple, **its backend alone** | `kernel/arch/<target>` with its default features, as a QEMU guest built by itself links it — without the `sched-arch` the kernel pass unifies in, so its always-compiled core must stand on its own |
| `wasm32-unknown-unknown` | `kernel/arch/wasm32` + `kernel/arch/api` (the only product code the browser target builds), and the browser verticals |

Every selection is *derived* — from the workspace member list, the manifests'
features and the wasm vertical table — so a new crate or vertical is linted
without being added to a second list. `--all-targets` is absent from the target
passes because a bare-metal target has no test harness to link one against; the
host passes cover those.

The enrolled **QEMU guests** under `tests/integration/` are test support rather
than product and are deliberately *not* in this gate; that gap is staged in
`plans/CODEVERIFY.md`.

The stratum split is load-bearing, not cosmetic. Cargo unifies features across
every package named in one invocation, so naming the kernel binary alongside
the userland programs turns on the `program` features of their shared
dependencies and links `lib/rt`'s `#[global_allocator]` and `#[panic_handler]`
into the kernel — a duplicate `panic_impl` lang item. The image pipeline builds
the kernel and the programs separately for the same reason.

## Every step is time-limited

No pipeline step can run forever. Every external command `xtask` spawns is
given a wall-clock budget; when a step overruns it, its whole process group is
signalled — `SIGTERM`, a short grace period, then `SIGKILL` — and the step
fails, naming itself and its budget.

Killing the *group* rather than the direct child is the point: a `cargo` step
is really the rustc, test-binary and QEMU processes it spawns, and killing only
the child would leave those running, holding the build lock and the terminal.

An overrun is a hard failure. It is never retried and never folds into a
passing result, because a step that hangs is a defect in exactly the way the
previous section describes — an unbounded wait, a missing completion signal, a
budget sized to an idle host — not a nuisance to paper over.

Ordinary steps share one default budget; a step known in advance to need
longer (the image gate, the QEMU matrix build) asks for a larger one
explicitly. A slow machine can raise every budget at once with
`TAIRIX_XTASK_TIMEOUT_SECS=<seconds>`; the override only ever *raises* a
budget, so it cannot silently shorten one, and a malformed value is rejected
rather than quietly ignored. The QEMU verticals keep their own per-guest
budgets — this is an outer backstop, not a replacement for them.

A guest killed for falling silent, or for reaching its runtime ceiling, is
interrogated over its QEMU monitor first: every vCPU's registers are read and
the addresses named against the kernel ELF's symbol table, then reported inline
and kept beside the transcript as `<binary>.hang.txt`. That is what separates a
machine whose cores are all halted in a wait-for-interrupt — nothing runnable,
so a wake-up was lost — from one still executing inside an interrupt-masked
section. Without it a hang can only be diagnosed by re-running it, which
[§7][test] forbids as a diagnosis. The transcript then ends with a
`tairix-qemu:` line naming where each scripted exchange stopped: the step each
cursor was on and how often that step's marker had appeared. A choreography
that waited on a witness that never came names it in the report itself.

Other subcommands (`build`, `clean`, `prune`, `coverage`) exist for
development and release flows; they are documented by `cargo xtask --help`.

`cargo xtask clean` reclaims the `target/` directory, which grows into
tens of gigabytes per target because `-Z build-std` rebuilds the whole
standard library for each of the four bare-metal Tier-1 targets. It
delegates to `cargo clean` (honouring `$CARGO_TARGET_DIR`), forwards the
usual cargo selectors (`--release`, `--doc`, `--target <triple>`,
`-p <crate>`) to scope the clean, and reports how much space was freed.

`cargo xtask prune` reclaims only the *superseded* build-script output
that dominates that growth. The `tairix-kernel` build script compiles the
embedded userland programs — each a roughly 1 GB `-Z build-std` tree —
into an `OUT_DIR` cargo keys by build-script fingerprint, so every
`build.rs` change strands the previous tree under
`target/<triple>/<profile>/build/` forever. `prune` keeps the newest
`build/<pkg>-<hash>` directory per package (the live one) and removes the
older siblings and their `.fingerprint` entries. Unlike `clean` it never
touches the current build, so the next compile stays incremental — which
is why it runs automatically before every `build` and `image`.

[agents]: https://github.com/tairix-project/tairix/blob/main/AGENTS.md
[plan]: https://github.com/tairix-project/tairix/blob/main/PLAN.md
[modularity]: ./architecture/modularity.md
[sc]: ./security/supply_chain.md
[fz]: ./security/fuzzing.md
[cite]: https://github.com/tairix-project/tairix/blob/main/AGENTS.md
[test]: https://github.com/tairix-project/tairix/blob/main/AGENTS.md
