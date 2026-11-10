//! Build orchestration for the TAIRiX workspace.
//!
//! `cargo xtask <command>` is the only sanctioned way to build, test, lint,
//! document, and audit TAIRiX. Driving every check through one entry point
//! keeps local developer flows and CI identical: when CI is green, a
//! contributor running the same command locally on a clean clone gets the
//! same result.
//!
//! ## Commands
//!
//! - `build`        — `cargo build --workspace --all-targets`;
//!   `--target aarch64-rpi` instead builds the flashable platform image
//!   (the `image` pipeline below)
//! - `clean`        — `cargo clean` to reclaim `target/` disk space (a full
//!   multi-arch `-Z build-std` tree runs to tens of GB per target); cargo
//!   selectors are forwarded (`--release`, `--doc`, `--target <triple>`,
//!   `-p <crate>`) and the reclaimed size is reported
//! - `keys`         — create the build's secret signing key set (`keys init`),
//!   kept outside the tree
//! - `test`         — `cargo test --workspace --all-targets`, then the
//!   doctests, each binary held to running every test it was given, the
//!   device-tree tests reading the pinned Pi firmware staged first; `--count N`
//!   (alias `--iterations N`) repeats the whole matrix N times to surface
//!   flaky tests, defaulting to one run; `--soak` instead repeats it for a
//!   wall-clock budget — 24 h, `--secs N`, or until the `--until T` close of a
//!   soak window — starting another pass only while one as long as the last
//!   still fits.
//!   `--shuffle` starts each host pass in a randomised order (the seed is in
//!   the step's label; `--shuffle-seed N` pins it) so a suite that passes
//!   only in the harness's alphabetical order fails instead of hiding — `ci`
//!   and `ci-long` both run the host tests this way. `--scenes` adds the
//!   scene tier, the long whole-scene ray-tracing tests built only under the
//!   `scene-tests` feature, which `ci` leaves out and `ci-long` runs
//! - `clippy`       — `-D warnings` for the host
//!   (`--workspace --all-targets`), each host feature lane (the debug image's
//!   kernel diagnostics, the scene tier) **and once per Tier-1 target**: the three
//!   freestanding triples over the cross-compiled product tree, each QEMU
//!   triple over its guests (which lints the payload programs they embed), and
//!   `wasm32` over the browser verticals. A host-only pass lints none of the
//!   `freestanding` bodies that actually ship — see `commands::target_clippy`
//! - `fmt`          — `cargo fmt --all -- --check` (pass `--fix` to apply)
//! - `docs-check`   — rustdoc (deny warnings) + mdBook build + link check
//! - `abi-check`    — verifies the generated kernel syscall table matches
//!   its source of truth in `lib/abi`
//! - `c-header`     — generates (`--write`) or verifies the C ABI
//!   development header in `include/` from the `lib/abi` source of truth
//! - `font-atlas`   — generates (`--write`) or verifies the system font
//!   glyph atlas in `lib/font/src/` from the committed faces in
//!   `lib/font/assets/`
//! - `sound-encode` — encodes authored WAV masters, or with `--synthesised` the
//!   masters its kept synthesiser makes, into a sound theme's FLAC assets in
//!   `lib/soundtheme/assets/`, each decoded back sample for sample
//! - `artsheet`     — measures `WinterSun`'s figure art against its bounds and
//!   the committed ledger (`--write` to regenerate the ledger, `--sheets` to
//!   render the contact sheets)
//! - `icons`        — draws the file-type icon masters in `lib/icon/assets/`
//!   and verifies the committed ones are exactly their drawings (`--write` to
//!   regenerate, `--sheets` to render the contact sheet)
//! - `devids`       — verifies the compact PCI, USB and IEEE OUI tables (each
//!   inside its consuming bundle's `Resources/`) against the vetted
//!   snapshots in
//!   `lib/devids/assets/` (`--write` to regenerate; `--fetch` — developer-run
//!   only, never CI — to import and vet the upstream databases)
//! - `unicode`      — verifies the Unicode property tables in
//!   `lib/unicode/src/tables.rs` against the pinned UCD snapshot in
//!   `lib/unicode/assets/` (`--write` to regenerate)
//! - `spirv`        — verifies the SPIR-V grammar tables in
//!   `lib/spirv/src/tables.rs` against the pinned Khronos grammar in
//!   `lib/spirv/assets/` (`--write` to regenerate)
//! - `shaders`      — verifies the pinned shader set in `lib/shaders/spv/`
//!   against the Rust sources that build it (`--write` to regenerate)
//! - `deps-check` — enforces the modularity dependency graph
//!   (layering, concrete-scheduler naming, optional-desktop boundary)
//! - `cfg-check` — rejects target-conditional compilation
//!   outside the architecture ports and build glue
//! - `board-check` — rejects a board, `SoC` or board part named in
//!   generic code
//! - `event-ids` — every crate's event ids from its range in
//!   `tairix_log::registry`, and no id defined by two crates
//! - `driver-exits` — no crate built on the driver runtime declares an exit
//!   code or exit module of its own
//! - `help-lint` — lints every command app's discovered `Help/` tree
//!   (plans/APPS.md §8.1): canonical `en-US/` presence, required-locale completeness,
//!   structural bounds, cross-locale `OPTIONS` switch-key drift, the
//!   content-policy screen, and per-command coverage
//! - `coverage`     — `cargo llvm-cov` report for the host-testable subset
//! - `sbom` — emit a `CycloneDX` SBOM from `Cargo.lock`:
//!   every workspace and external crate with its version, source URL,
//!   and pinned source checksum
//! - `supply-chain` — verify the committed source-hash allow-list
//!   against `Cargo.lock` and enforce the RUSTSEC advisory SLA;
//!   `--write-pins` regenerates the pins from the lockfile
//! - `fuzz`         — drive the in-tree fuzz harnesses: `--once` (one
//!   iteration each, the `ci` gate), `--quick` (≥ 5 s each) or `--soak`
//!   (≥ 24 h each), any budget overridden by `--secs N`, or by `--until T`
//!   for one harness named with `--target`, soaking it to a window's close
//!   (`tools/ci/soak.sh --window` gives every harness a process of its own)
//! - `proptest`     — drive the stateful capability models, with the same
//!   `--once` (the `ci` stage), `--quick`, `--soak`, `--secs N` and
//!   `--until T` budgets as `fuzz`
//! - `fssoak`       — soak arxfs, ext4 and fat32 on a RAM volume of at least
//!   1 GiB for a wall-clock budget
//! - `rngsoak`      — soak the random generators through the statistical
//!   battery for a wall-clock budget
//! - `source-text`  — reject a control byte other than tab and newline in
//!   source text, which `grep` and its kin then read as binary
//! - `charter-cite` — reject a comment or package description that cites a
//!   charter section number in place of the reason it states; a reference to
//!   another document (a plan, a `docs/` page, an RFC, a hardware manual)
//!   passes when that document is named beside the section number
//! - `miri`         — interpret the crates whose safety rests on a
//!   hand-written `unsafe` core (the registry in `commands/miri.rs`) under
//!   Miri, the undefined-behaviour oracle a test suite cannot be: it checks that a
//!   raw pointer stayed in bounds, that a slot was initialised before it was
//!   read, and that two `&mut` never aliased
//! - `loom`         — model-check the concurrent protocols `commands/loom.rs`
//!   enrols over every thread interleaving, the orderings a test run never
//!   happened to pick
//! - `model-check` — exhaustively model-check the Silver capability +
//!   IPC state machine (an in-tree explicit-state checker; the TLA+
//!   equivalent), failing closed on any invariant counterexample
//! - `spec-review`  — reject an unreviewed AI-drafted-artefact marker that
//!   reached the tree
//! - `ignore-check` — reject an ignored test in any spelling, since libtest
//!   reports one as skipped under a green gate
//! - `bench`        — host microbenchmarks for the per-pixel desktop paths:
//!   `Surface::blit`, rounded-rect fill, `box_blur`/`frost_region`/
//!   `frost_halved_from`,
//!   `resample`, the scan-out channel encode, and the window manager's
//!   whole-frame composite over a representative window stack. Reports ns/px
//!   and ns/frame for a `--iters N` / `--rounds N` budget, one family at a
//!   time with `--filter <substring>`. Wall-clock timings are load-dependent,
//!   so this is evidence for a completion report and is deliberately **not**
//!   a `ci` gate; CI may run it only as a smoke check that every family still
//!   produces a number
//! - `ci`           — the full pipeline a PR must pass, each stage cheaper
//!   than the next so a failing PR fails fast: `fmt --check`; `static gates`,
//!   the read-only, non-compiling checks run at once so their costs overlap;
//!   `deny`; `proptest --once`; `crypto-constant-time`, the crypto tests under
//!   release optimisation; `fuzz --once`; `loom`, the interleaving oracle;
//!   `docs-check`; `image`, every delivered image profile; `clippy`;
//!   `test --qemu --wasm`, each test once in a shuffled order; and `miri`,
//!   the undefined-behaviour oracle
//! - `ci-long`      — the same checks as `ci`, but for a dedicated long-lived
//!   runner: every test-executing stage (the host test matrix, the scene tier
//!   `ci` leaves out, the release crypto constant-time tests, the QEMU
//!   integration tests, the fuzz harnesses, and the proptest models) is run
//!   20× sequentially and then 20× concurrently, per test, to force out
//!   timing- and contention-dependent flakes — the scene tier's concurrent
//!   replicas one at a time, since one run fills the machine; every other
//!   `ci` stage runs once, in `ci`'s order, ahead of the hunt. `--dry-run`
//!   prints the plan without running anything
//! - `image`        — build platform images via `tools/mkimage`
//!   (`--target aarch64-rpi`: the Raspberry Pi 4 SD image; the pinned
//!   firmware blobs are staged build inputs, see
//!   `tools/mkimage/firmware.lock` and `docs/src/install/raspberry_pi.md`)
//! - `run`          — build a platform image and boot it interactively in
//!   QEMU, with a display, keyboard and mouse
//!
//! The set above is closed: every subsystem documented in `AGENTS.md` and
//! `PLAN.md` is reachable through exactly one of these subcommands. New
//! pipeline steps belong in a *named* subcommand, never inlined into `ci`.

// Output goes through `note` and `out`, which a broken stream cannot panic.
#![deny(clippy::print_stderr, clippy::print_stdout, clippy::dbg_macro)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Output, Stdio};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

/// Writes `text` to stderr, waiting out a full stream and giving up on a dead
/// one: a hung-up terminal or a closed pipe must not end xtask while children
/// it supervises still run.
#[cfg(not(test))]
fn note(text: &str) {
    let _ = write_whole(&mut std::io::stderr().lock(), text.as_bytes());
}

/// [`note`] in a test build, through the print macro libtest captures.
#[cfg(test)]
#[allow(clippy::print_stderr)] // libtest captures only what the print macros write.
fn note(text: &str) {
    eprint!("{text}");
}

/// `eprintln!` through [`note`].
macro_rules! noteln {
    ($($arg:tt)*) => {
        $crate::note(&format!("{}\n", format_args!($($arg)*)))
    };
}

/// Writes `text` to stdout, the output a command exists to produce, failing
/// the command where it cannot.
#[cfg(not(test))]
fn out(text: &str) -> Result<(), String> {
    write_whole(&mut std::io::stdout().lock(), text.as_bytes())
        .map_err(|err| format!("cannot write to stdout: {err}"))
}

/// [`out`] in a test build, through the print macro libtest captures.
#[cfg(test)]
#[allow(clippy::print_stdout)] // libtest captures only what the print macros write.
fn out(text: &str) -> Result<(), String> {
    print!("{text}");
    std::io::Write::flush(&mut std::io::stdout())
        .map_err(|err| format!("cannot write to stdout: {err}"))
}

/// `println!` through [`out`], answering whether it was written.
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::out(&format!("{}\n", format_args!($($arg)*)))
    };
}

/// Writes all of `bytes` to `stream` and flushes it, waiting for room where it
/// is full: a stream left non-blocking by whoever shares it is slow, not dead.
fn write_whole<S: std::io::Write + std::os::fd::AsFd>(
    stream: &mut S,
    mut bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::ErrorKind;

    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) if err.kind() == ErrorKind::WouldBlock => writable(stream.as_fd())?,
            Err(err) => return Err(err),
        }
    }
    loop {
        match stream.flush() {
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) if err.kind() == ErrorKind::WouldBlock => writable(stream.as_fd())?,
            flushed => return flushed,
        }
    }
}

/// Wait until `fd` can take a write, or has failed for good for the next
/// write to report.
fn writable(fd: std::os::fd::BorrowedFd<'_>) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;

    let mut ready = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        // SAFETY: `ready` is an initialised `pollfd` that outlives the call,
        // which reads and writes it alone.
        if unsafe { libc::poll(&raw mut ready, 1, -1) } >= 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

mod children;
mod commands;
mod floor;
#[cfg(test)]
#[path = "../tests/support/process.rs"]
mod process;
mod rustflags;
mod timeout_env;

use children::Children;
use commands::Command as Subcommand;
use timeout_env::TIMEOUT_ENV_VAR;

fn main() -> ExitCode {
    let argv: Vec<OsString> = env::args_os().skip(1).collect();
    let Some((name, rest)) = argv.split_first() else {
        noteln!("{}", usage());
        return ExitCode::from(2);
    };

    let Some(name_str) = name.to_str() else {
        noteln!("xtask: command name is not valid UTF-8");
        return ExitCode::from(2);
    };

    if matches!(name_str, "-h" | "--help" | "help") {
        return match outln!("{}", usage()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                noteln!("xtask: {err}");
                ExitCode::FAILURE
            }
        };
    }

    let Some(command) = Subcommand::parse(name_str) else {
        noteln!("xtask: unknown command `{name_str}`\n\n{}", usage());
        return ExitCode::from(2);
    };

    let ctx = match timeout_override().and_then(Context::discover) {
        Ok(ctx) => ctx,
        Err(err) => {
            noteln!("xtask: {err}");
            return ExitCode::FAILURE;
        }
    };
    let supervised = children::supervise(
        &ctx.children,
        children::KILL_REAP_GRACE,
        children::REPORT_GRACE,
        || command.run(&ctx, rest),
    );
    match supervised.and_then(|outcome| outcome) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            noteln!("xtask: {err}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    let mut out = String::from("usage: cargo xtask <command> [args]\n\ncommands:\n");
    for cmd in Subcommand::ALL {
        // `write!` into a `String` is infallible; the result is discarded.
        let _ = writeln!(out, "  {:<12} {}", cmd.name(), cmd.summary());
    }
    out
}

/// Repository-wide paths discovered once per invocation, and the children
/// spawned under them.
///
/// `Clone` is derived so a concurrent pipeline stage can hand each worker its
/// own owned copy (every field is cheap to clone), letting the worker closures
/// be `'static` for the shared concurrency runner.
#[derive(Clone)]
pub struct Context {
    /// Absolute path to the workspace root (the directory containing the
    /// top-level `Cargo.toml`).
    pub workspace_root: PathBuf,
    /// Path to the `cargo` executable the parent shell used to invoke us.
    /// Honouring `$CARGO` keeps custom toolchains (rustup overrides, CI
    /// matrix shims) consistent across nested calls.
    pub cargo: OsString,
    /// Every child xtask spawns, and the floor their budgets are raised to.
    pub(crate) children: Arc<Children>,
}

impl Context {
    /// The workspace's context, its children's budgets raised to `floor`.
    fn discover(floor: Option<Duration>) -> Result<Self, String> {
        let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        let workspace_root = workspace_root_from_manifest_dir()?;
        Ok(Self {
            workspace_root,
            cargo,
            children: Arc::new(Children::new(floor)),
        })
    }

    /// The build's secret signing key set (`tools/signkeys`).
    ///
    /// # Errors
    ///
    /// Why there is no key set to sign with, and how to create one.
    pub fn signing_keys(&self) -> Result<tairix_signkeys::SigningKeys, String> {
        tairix_signkeys::load(&self.workspace_root)
            .map(|(_, keys)| keys)
            .map_err(|err| err.to_string())
    }

    /// Builds a `cargo` command rooted at the workspace.
    #[must_use]
    pub fn cargo(&self) -> Command {
        let mut cmd = Command::new(&self.cargo);
        cmd.current_dir(&self.workspace_root);
        cmd
    }

    /// Absolute path to the directory cargo writes build artifacts into.
    ///
    /// This is **not** unconditionally `workspace_root/target`: cargo honours
    /// `$CARGO_TARGET_DIR` (CI points it at a runner-local cache outside the
    /// checkout so a multi-GB `target/` survives `actions/checkout` wiping the
    /// workspace). Resolving artifact paths against the same directory cargo
    /// actually built into is the only way a post-build step (`test --qemu`,
    /// `test --wasm`) can find the binary it just asked cargo to produce.
    #[must_use]
    pub fn target_dir(&self) -> PathBuf {
        tairix_fuzzseed::firmware::target_dir(
            &self.workspace_root,
            env::var_os("CARGO_TARGET_DIR").as_deref(),
        )
    }

    /// Runs `cmd` inheriting stdio, under [`DEFAULT_COMMAND_TIMEOUT`].
    ///
    /// This is the budget every step gets unless the phase is known ahead of
    /// time to legitimately outrun it (see [`run_with_timeout`]).
    ///
    /// Returns an error describing the failure if the child cannot be
    /// spawned, exits with a non-zero status, or is killed for exceeding its
    /// budget.
    ///
    /// [`run_with_timeout`]: Context::run_with_timeout
    pub fn run(&self, label: &str, cmd: Command) -> Result<(), String> {
        self.run_with_timeout(label, cmd, DEFAULT_COMMAND_TIMEOUT)
    }

    /// [`run_with_timeout`](Self::run_with_timeout), holding the command's
    /// tests to `account`, settled from the lines that frame each libtest run
    /// as its output is relayed.
    pub fn run_accounted(
        &self,
        label: &str,
        cmd: Command,
        budget: Duration,
        account: commands::libtest::Account,
    ) -> Result<(), String> {
        commands::parallel::run(
            &self.children,
            vec![commands::parallel::Job::new(label, cmd)
                .with_budget(budget)
                .accounted(account)],
            1,
        )
    }

    /// Runs `cmd` inheriting stdio, killing it with everything it forked if
    /// it is still running after `budget`, which `$TAIRIX_XTASK_TIMEOUT_SECS`
    /// can only raise.
    ///
    /// It is the parallel runner's run of one job, so a step run alone and
    /// one run beside others are bounded, killed and reported alike. A
    /// timeout is an error naming the step and its budget, never retried.
    pub fn run_with_timeout(
        &self,
        label: &str,
        cmd: Command,
        budget: Duration,
    ) -> Result<(), String> {
        commands::parallel::run(
            &self.children,
            vec![commands::parallel::Job::new(label, cmd).with_budget(budget)],
            1,
        )
    }
}

/// Runs `cmd` as the leader of its own group in `children` within `budget`,
/// answering what `wait` makes of the child where `exited` reads a success
/// from it, and otherwise why not, with whatever the wait yielded.
pub(crate) fn run_within<T: Send + 'static>(
    children: &Children,
    label: &str,
    cmd: &mut Command,
    budget: Duration,
    wait: impl FnOnce(Child) -> std::io::Result<T> + Send + 'static,
    exited: impl FnOnce(&T) -> ExitStatus,
) -> Result<T, WaitFailure<T>> {
    let child = children.spawn(cmd).map_err(|err| WaitFailure {
        message: format!("{label} could not be spawned: {err}"),
        reaped: None,
        overran: false,
    })?;
    let pid = child.id();
    let waited = await_within(label, pid, budget, children::KILL_REAP_GRACE, move || {
        wait(child)
    });
    children.release(pid);
    let value = waited?;
    let status = exited(&value);
    if status.success() {
        Ok(value)
    } else {
        Err(WaitFailure {
            message: format!("{label} failed with {status}"),
            reaped: Some(value),
            overran: false,
        })
    }
}

/// [`run_within`] with stdin closed and the child's output captured, as
/// `Command::output` has them, but awaited within `budget`.
pub(crate) fn output_within(
    children: &Children,
    label: &str,
    cmd: &mut Command,
    budget: Duration,
) -> Result<Output, WaitFailure<Output>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_within(
        children,
        label,
        cmd,
        budget,
        Child::wait_with_output,
        |output| output.status,
    )
}

/// [`output_within`], failing with why and then what the child wrote to
/// stderr, where it wrote anything.
pub(crate) fn output_or_reason(
    children: &Children,
    label: &str,
    cmd: &mut Command,
    budget: Duration,
) -> Result<Output, String> {
    output_within(children, label, cmd, budget).map_err(|failure| {
        let stderr = failure
            .reaped
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.stderr));
        match stderr.as_deref().map(str::trim) {
            Some(stderr) if !stderr.is_empty() => format!("{}: {stderr}", failure.message),
            _ => failure.message,
        }
    })
}

/// Why a bounded wait ended without the value it was waiting for.
pub(crate) struct WaitFailure<T> {
    /// What happened, naming the step.
    pub(crate) message: String,
    /// What the wait yielded where it yielded anything: a failed child's, or
    /// a timed-out group's once it was killed and reaped. For a captured job
    /// that is the output it had produced, which names the work its budget
    /// ran out on.
    pub(crate) reaped: Option<T>,
    /// Whether the budget ran out and the group was killed for it, rather
    /// than the child failing on its own.
    pub(crate) overran: bool,
}

/// Races `wait` against `budget`, killing the process group led by `pid` when
/// the budget expires first and then waiting at most `reap_grace` for its
/// reap.
///
/// `std::process::Child` has no timed wait, so the blocking wait runs on a
/// dedicated thread while this thread races it against the deadline with
/// `Receiver::recv_timeout` — no polling loop, no sleeping and re-checking
/// `try_wait()`.
///
/// Generic over what the wait yields so the one implementation serves both a
/// step that inherits the terminal (waiting for an exit status) and one whose
/// output is captured for orderly printing (waiting for the output too);
/// a second copy of this race would be a second thing to get wrong.
fn await_within<T: Send + 'static>(
    label: &str,
    pid: u32,
    budget: Duration,
    reap_grace: Duration,
    wait: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, WaitFailure<T>> {
    let failure = |message: String| WaitFailure {
        message,
        reaped: None,
        overran: false,
    };
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = thread::Builder::new().spawn(move || {
        // The receiver may already have moved on past a timeout; a send with
        // no one listening is simply discarded.
        let _ = done_tx.send(wait());
    });
    let waiter = match waiter {
        Ok(handle) => handle,
        Err(err) => {
            children::kill(pid);
            return Err(failure(format!(
                "{label}: could not start the wait thread ({err}); \
                 pid {pid} was killed and is not waited for"
            )));
        }
    };

    match done_rx.recv_timeout(budget) {
        Ok(Ok(value)) => {
            let _ = waiter.join();
            Ok(value)
        }
        Ok(Err(err)) => {
            let _ = waiter.join();
            Err(failure(format!("{label} could not be awaited: {err}")))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = waiter.join();
            Err(failure(format!(
                "{label}: the wait thread ended without reporting a status"
            )))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            children::kill(pid);
            // A further bounded wait for the background thread to reap the
            // killed process, which one wedged in the kernel never is.
            let answer = done_rx.recv_timeout(reap_grace);
            let unreaped = if answer.is_err() {
                format!("; pid {pid} was not reaped within {reap_grace:?}")
            } else {
                String::new()
            };
            Err(WaitFailure {
                message: format!(
                    "{label} exceeded its {budget:?} timeout and was killed \
                     (SIGTERM, then SIGKILL, to its process group){unreaped}"
                ),
                reaped: answer.ok().and_then(Result::ok),
                overran: true,
            })
        }
    }
}

/// Wall-clock budget [`Context::run`] grants an ordinary step: a `cargo`
/// invocation that compiles and/or tests some slice of the workspace once,
/// incrementally, on the host.
///
/// A capacity like this must scale with the machine, not be a number picked
/// for whoever happened to write it, so it is layered rather than frozen:
/// this is the floor every ordinary step gets, `run_with_timeout` lets a
/// step that is known ahead of time to need more ask for it explicitly (see
/// [`LONG_BUILD_COMMAND_TIMEOUT`]), and `$TAIRIX_XTASK_TIMEOUT_SECS` lets an
/// operator on a slower machine raise *every* budget, including the longer
/// ones, without editing either constant. Forty-five minutes comfortably
/// covers an incremental `cargo test --workspace --all-targets` pass while
/// still catching a genuinely hung process well inside a CI job's own outer
/// deadline.
pub(crate) const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_mins(45);

/// Wall-clock budget for a step that legitimately outruns
/// [`DEFAULT_COMMAND_TIMEOUT`]: cross-compiling a bare-metal or PIE target
/// from a clean `target/` rebuilds `core`/`alloc`/`compiler_builtins` from
/// source (`-Z build-std`) or links many packages in one `cargo build`
/// (the QEMU integration-test matrix), either of which can legitimately run
/// longer than an incremental host compile/test pass. Used explicitly by the
/// image gate's kernel and driver/app cross-compiles and by the QEMU matrix
/// build, never by default.
pub(crate) const LONG_BUILD_COMMAND_TIMEOUT: Duration = Duration::from_hours(2);

/// One step's closing wall-clock line, newline included.
///
/// Every step reports its cost in this one shape, sequential and concurrent
/// alike, so a single grep over a pipeline log profiles the whole run — and so
/// a job quietly running at most of its budget is visible before the next
/// slower host turns it into a kill. `docs/src/contributing.md` documents the
/// grep, which is why the wording lives here rather than at each caller.
pub(crate) fn elapsed_line(label: &str, ok: bool, elapsed: Duration) -> String {
    format!(
        "xtask: [{label}] {} in {elapsed:?}\n",
        if ok { "done" } else { "FAILED" }
    )
}

/// The floor `$TAIRIX_XTASK_TIMEOUT_SECS` raises every budget to, where the
/// variable is set.
///
/// The override only ever raises a budget, never lowers one: a step that
/// asked for [`LONG_BUILD_COMMAND_TIMEOUT`] already knows it needs more than
/// the default, and an operator's "my machine is slow" knob must not undo
/// that. A set-but-unparsable value, one that is not even UTF-8 included,
/// fails closed — returning an error rather than silently keeping the
/// built-in budget — because that silence would hide a typo behind the exact
/// kind of unbounded hang this mechanism exists to prevent.
fn timeout_override() -> Result<Option<Duration>, String> {
    env::var_os(TIMEOUT_ENV_VAR)
        .map(|raw| parse_timeout_env_override(&raw))
        .transpose()
}

/// Parses a `$TAIRIX_XTASK_TIMEOUT_SECS` value into a duration.
///
/// A pure function so the parsing (and its fail-closed rejection of a
/// malformed value) is unit-testable without touching real process
/// environment state.
fn parse_timeout_env_override(raw: &OsStr) -> Result<Duration, String> {
    let malformed = || {
        format!(
            "{TIMEOUT_ENV_VAR}=\"{}\" is not a positive integer number of seconds",
            raw.display()
        )
    };
    let secs: u64 = raw
        .to_str()
        .ok_or_else(malformed)?
        .trim()
        .parse()
        .map_err(|_| malformed())?;
    if secs == 0 {
        return Err(format!("{TIMEOUT_ENV_VAR}=0 is not a valid timeout"));
    }
    Ok(Duration::from_secs(secs))
}

/// Resolve the workspace root from the manifest directory of the xtask crate.
///
/// We deliberately avoid shelling out to `cargo locate-project` so this works
/// even when the cargo cache is cold or when the xtask binary is invoked
/// from a packaged release tarball.
fn workspace_root_from_manifest_dir() -> Result<PathBuf, String> {
    // `CARGO_MANIFEST_DIR` is set by cargo when xtask is invoked through the
    // `cargo xtask` alias. When the binary is run directly we walk up from
    // the executable looking for `Cargo.toml` containing `[workspace]`.
    if let Some(dir) = env::var_os("CARGO_MANIFEST_DIR") {
        let path = PathBuf::from(dir);
        // `tools/xtask` → `tools` → workspace root.
        let root = path
            .ancestors()
            .find(|p| is_workspace_root(p))
            .ok_or_else(|| {
                format!(
                    "could not locate the workspace root above {}",
                    path.display()
                )
            })?;
        return Ok(root.to_path_buf());
    }

    let mut dir = env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    loop {
        if is_workspace_root(&dir) {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("no workspace Cargo.toml found in any parent directory".to_string());
        }
    }
}

fn is_workspace_root(dir: &Path) -> bool {
    let manifest = dir.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return false;
    };
    text.contains("[workspace]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::WHILE_ITS_PARENT_RUNS;

    /// xtask keeps its own lint tables only to leave `missing_docs` out, so
    /// its clippy lints are the workspace's, line for line: a default the
    /// workspace changes cannot leave xtask failing behind it.
    #[test]
    fn clippy_lints_are_the_workspaces() {
        let table = |manifest: &'static str, header: &str| -> Vec<&'static str> {
            let start = manifest.find(header).expect("the table") + header.len();
            let body = &manifest[start..];
            body[..body.find("\n[").unwrap_or(body.len())]
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .collect()
        };
        assert_eq!(
            table(include_str!("../Cargo.toml"), "\n[lints.clippy]\n"),
            table(
                include_str!("../../../Cargo.toml"),
                "\n[workspace.lints.clippy]\n"
            ),
        );
    }

    /// A killed job's output is the only record of what it was doing when its
    /// budget ran out — for a test binary, the name of the test in progress.
    #[test]
    fn a_timed_out_wait_hands_back_what_the_killed_group_produced() {
        use std::io::{BufRead as _, BufReader};

        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("echo test reached::the_hang ... ; echo started >&2; {WHILE_ITS_PARENT_RUNS}"),
        ]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Children::default().spawn(&mut cmd).expect("sh spawns");
        // Wait on the child's own word that the line is in the pipe, so the
        // budget below cannot expire before it is written however busy the
        // host is.
        let mut started = String::new();
        BufReader::new(child.stderr.take().expect("stderr is piped"))
            .read_line(&mut started)
            .expect("the child reports that it started");
        assert_eq!(started.trim(), "started");

        let pid = child.id();
        let failure = await_within(
            "unit-test-partial-output",
            pid,
            Duration::from_millis(200),
            children::KILL_REAP_GRACE,
            move || child.wait_with_output(),
        )
        .expect_err("a command that outlives its budget must fail, never pass");
        assert!(
            failure.message.contains("unit-test-partial-output")
                && failure.message.contains("timeout"),
            "unexpected message: {}",
            failure.message
        );
        assert!(failure.overran, "killed for its budget");
        let output = failure
            .reaped
            .expect("a killed group is reaped within the grace");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test reached::the_hang ..."),
            "the output before the kill must survive it: {output:?}"
        );
    }

    /// A shell script leading a grandchild that runs while the test does, its
    /// pid the script's first line.
    pub(crate) fn with_a_grandchild() -> String {
        format!("({WHILE_ITS_PARENT_RUNS}) & echo $!; wait")
    }

    /// The first line `child` writes, its stdout piped.
    pub(crate) fn first_line(child: &mut Child) -> String {
        use std::io::{BufRead as _, BufReader};

        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().expect("stdout is piped"))
            .read_line(&mut line)
            .expect("the child writes a line");
        line.trim().to_owned()
    }

    /// Long enough for a call that must wait to have shown it does not.
    pub(crate) const NO_ANSWER: Duration = Duration::from_millis(200);

    /// Set or clear `O_NONBLOCK` on `pipe`.
    fn nonblocking(pipe: &std::io::PipeWriter, on: bool) {
        use std::os::fd::AsRawFd as _;

        let fd = pipe.as_raw_fd();
        // SAFETY: `fcntl` on a descriptor this test owns takes and returns
        // scalars and touches no memory of ours.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        // SAFETY: as above.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    }

    /// Fill `pipe` until it takes no more, leaving it blocking, so whatever
    /// writes to it next waits until it is read.
    pub(crate) fn fill(pipe: &std::io::PipeWriter) {
        use std::io::Write as _;

        nonblocking(pipe, true);
        let mut stream = pipe;
        while stream.write(&[0_u8; 4096]).is_ok() {}
        nonblocking(pipe, false);
    }

    /// This test binary, to run its test `name` alone as a process of its
    /// own.
    pub(crate) fn this_test(name: &str) -> Command {
        let mut cmd = Command::new(env::current_exe().expect("this test binary"));
        cmd.args(["--exact", name, "--test-threads=1"]);
        cmd
    }

    /// Whether `pid` names a live process: neither gone nor a zombie awaiting
    /// its reaper, as a killed grandchild may briefly be.
    pub(crate) fn running(pid: u32) -> bool {
        let stat = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps runs");
        stat.status.success()
            && !String::from_utf8_lossy(&stat.stdout)
                .trim()
                .starts_with('Z')
    }

    /// A `cargo` step is the processes it forks, so a timeout kills the
    /// child's whole group, not the child alone.
    #[test]
    fn a_timed_out_wait_kills_everything_the_group_forked() {
        let mut cmd = Command::new("sh");
        // The grandchild runs as long as this test does, so it is dead
        // afterwards only if the kill reached the whole group.
        cmd.args(["-c", &with_a_grandchild()]);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped());
        let mut child = Children::default().spawn(&mut cmd).expect("sh spawns");
        // Read before the budget below starts, so the grandchild exists however
        // busy the host is.
        let grandchild: u32 = first_line(&mut child).parse().expect("a pid");
        assert!(running(grandchild), "the grandchild runs before the kill");

        let leader = child.id();
        let failure = await_within(
            "unit-test-grandchild",
            leader,
            Duration::from_millis(200),
            children::KILL_REAP_GRACE,
            move || child.wait(),
        )
        .expect_err("a command that outlives its budget must fail, never pass");
        assert!(failure.message.contains("killed"), "{}", failure.message);
        assert!(
            !running(grandchild),
            "grandchild {grandchild} outlived its group's kill"
        );
    }

    /// A wait the kill cannot end, as one on a process wedged in the kernel
    /// is, leaves that process named in the failure.
    #[test]
    fn a_kill_never_reaped_names_the_pid_it_left() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", WHILE_ITS_PARENT_RUNS]).stdin(Stdio::null());
        let mut child = Children::default().spawn(&mut cmd).expect("sh spawns");
        let pid = child.id();
        let (unwedge, wedged) = mpsc::channel::<()>();
        let reap_grace = Duration::from_millis(200);
        let failure = await_within(
            "unit-test-wedged",
            pid,
            Duration::from_millis(200),
            reap_grace,
            move || {
                let _ = wedged.recv();
                Ok(())
            },
        )
        .expect_err("a command that outlives its budget must fail, never pass");
        drop(unwedge);
        let _ = child.wait();
        assert!(
            failure
                .message
                .ends_with(&format!("; pid {pid} was not reaped within {reap_grace:?}")),
            "{}",
            failure.message
        );
        assert!(failure.reaped.is_none());
    }

    /// A stream whose first write and first flush find it full.
    struct Full {
        room: (std::io::PipeReader, std::io::PipeWriter),
        written: Vec<u8>,
        /// Whether the next write, and the next flush, find no room.
        no_room: [bool; 2],
    }

    impl Full {
        fn new() -> Self {
            Self {
                room: std::io::pipe().expect("a pipe"),
                written: Vec::new(),
                no_room: [true; 2],
            }
        }
    }

    impl std::io::Write for Full {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if std::mem::take(&mut self.no_room[0]) {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if std::mem::take(&mut self.no_room[1]) {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            Ok(())
        }
    }

    /// An empty pipe stands in for the stream's descriptor, so a wait for
    /// room returns at once.
    impl std::os::fd::AsFd for Full {
        fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            self.room.1.as_fd()
        }
    }

    /// Only the retry is pinned here; the wait for room is pinned on a full
    /// pipe below.
    #[test]
    fn a_full_stream_is_waited_out_rather_than_given_up_on() {
        let mut stream = Full::new();
        assert!(write_whole(&mut stream, b"every byte").is_ok());
        assert_eq!(stream.written, b"every byte");
        assert_eq!(
            std::io::Write::write_all(&mut Full::new(), b"every byte").map_err(|err| err.kind()),
            Err(std::io::ErrorKind::WouldBlock),
            "where std's own writer gives up"
        );
    }

    /// A stream that says each time it is written to, as far as one unread
    /// word allows.
    struct Counted {
        pipe: std::io::PipeWriter,
        tries: mpsc::SyncSender<()>,
    }

    impl std::io::Write for Counted {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.tries.try_send(());
            self.pipe.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.pipe.flush()
        }
    }

    impl std::os::fd::AsFd for Counted {
        fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            self.pipe.as_fd()
        }
    }

    #[test]
    fn a_full_stream_is_waited_on_rather_than_spun_on() {
        use std::io::Read as _;

        let (mut reader, writer) = std::io::pipe().expect("a pipe");
        fill(&writer);
        nonblocking(&writer, true);
        let (says, tries) = mpsc::sync_channel(1);
        let writing = thread::spawn(move || {
            write_whole(
                &mut Counted {
                    pipe: writer,
                    tries: says,
                },
                b"every byte",
            )
        });
        let first = tries.recv_timeout(process::MINUTE).is_ok();
        // Nothing reads the full pipe yet, so a writer that waits for room
        // makes no second try.
        let again = tries.recv_timeout(NO_ANSWER).is_ok();
        let drained = process::within(move || {
            let mut written = Vec::new();
            let read = reader.read_to_end(&mut written).is_ok();
            (read, writing.join(), written)
        });
        assert!(first, "it tried");
        assert!(!again, "and waited for room rather than trying again");
        let Some((read, wrote, written)) = drained else {
            panic!("the writer never woke once its pipe had room");
        };
        let wrote = wrote.expect("the writer");
        assert!(read && wrote.is_ok(), "{wrote:?}");
        assert!(
            written.ends_with(b"every byte"),
            "every byte, once there was room"
        );
    }

    /// Set to run [`noting_helper`] as the process the capture test starts.
    const NOTING: &str = "TAIRIX_XTASK_NOTING_HELPER";

    /// What [`noting_helper`] notes.
    const NOTED: &str = "xtask: a note its test's harness keeps";

    /// Not a test of its own: the process the capture test starts, doing
    /// nothing unless that test started it.
    #[test]
    fn noting_helper() {
        if env::var_os(NOTING).is_some() {
            noteln!("{NOTED}");
        }
    }

    #[test]
    fn a_note_from_code_under_test_is_kept_by_its_harness() {
        let output = this_test("tests::noting_helper")
            .env(NOTING, "1")
            .env_remove("RUST_TEST_NOCAPTURE")
            .stdin(Stdio::null())
            .output()
            .expect("the helper runs");
        assert!(output.status.success(), "{output:?}");
        let shown = [&output.stdout, &output.stderr]
            .map(|stream| String::from_utf8_lossy(stream).contains(NOTED));
        assert_eq!(shown, [false, false], "{output:?}");
    }

    #[test]
    fn a_child_that_fails_or_cannot_start_is_a_failure_naming_it() {
        let children = Children::default();
        let status = |label: &str, program: &str| {
            run_within(
                &children,
                label,
                &mut Command::new(program),
                process::MINUTE,
                |mut child| child.wait(),
                |status| *status,
            )
        };
        assert!(status("unit-test-ok", "true").is_ok());
        let failed = status("unit-test-false", "false").expect_err("a non-zero exit fails");
        assert!(
            failed.message.contains("unit-test-false failed with"),
            "{}",
            failed.message
        );
        assert!(!failed.overran, "failing on its own");
        assert!(
            failed.reaped.is_some_and(|status| !status.success()),
            "with the status it exited with"
        );
        let unspawned = status("unit-test-missing", "/nonexistent/xtask-unit-test")
            .expect_err("nothing to run");
        assert!(
            unspawned
                .message
                .contains("unit-test-missing could not be spawned"),
            "{}",
            unspawned.message
        );
        assert!(unspawned.reaped.is_none());
        assert!(children.live_children().is_empty(), "every group released");
    }

    #[test]
    fn a_captured_failure_says_why_and_then_what_it_wrote_to_stderr() {
        let budget = process::MINUTE;
        let children = Children::default();
        let script = |text: &str| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", text]);
            cmd
        };
        let output = output_or_reason(
            &children,
            "unit-test-ok",
            &mut script("echo listed"),
            budget,
        )
        .expect("a successful child's output");
        assert_eq!(output.stdout, b"listed\n");
        let said = output_or_reason(
            &children,
            "unit-test-said",
            &mut script("echo refused >&2; exit 3"),
            budget,
        )
        .expect_err("a non-zero exit fails");
        assert!(
            said.contains("unit-test-said failed with") && said.ends_with(": refused"),
            "{said}"
        );
        let silent = output_or_reason(&children, "unit-test-silent", &mut script("exit 3"), budget)
            .expect_err("a non-zero exit fails");
        assert!(
            silent.ends_with("exit status: 3"),
            "nothing after why: {silent}"
        );
        // A probe that hangs is not a tool that is missing: it says so.
        let hung = output_or_reason(
            &children,
            "unit-test-hung",
            &mut script(WHILE_ITS_PARENT_RUNS),
            Duration::from_millis(200),
        )
        .expect_err("killed at its budget");
        assert!(
            hung.starts_with("unit-test-hung exceeded its 200ms timeout and was killed"),
            "{hung}"
        );
    }

    #[test]
    fn timeout_env_override_parses_a_positive_integer() {
        assert_eq!(
            parse_timeout_env_override(OsStr::new("120")).expect("120 is a valid override"),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn timeout_env_override_rejects_zero() {
        assert!(
            parse_timeout_env_override(OsStr::new("0")).is_err(),
            "a zero-second override must fail closed, not disable the timeout"
        );
    }

    #[test]
    fn timeout_env_override_rejects_malformed_values() {
        for bad in ["", "soon", "-5", "5s", "1.5"] {
            assert!(
                parse_timeout_env_override(OsStr::new(bad)).is_err(),
                "{bad:?} should be rejected, not silently ignored"
            );
        }
    }

    /// Set but not UTF-8, as a no-break space pasted from a Latin-1 terminal
    /// leaves it, a value is as malformed as any typo.
    #[test]
    fn timeout_env_override_rejects_a_value_that_is_not_utf_8() {
        use std::os::unix::ffi::OsStrExt as _;

        assert!(parse_timeout_env_override(OsStr::from_bytes(b"7200\xa0")).is_err());
    }
}
