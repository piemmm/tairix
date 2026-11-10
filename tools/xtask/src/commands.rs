//! Subcommand implementations for `cargo xtask`.
//!
//! Each variant of [`Command`] corresponds to a single, named developer
//! workflow. Adding a new pipeline step means adding a new variant here —
//! never appending hidden behaviour to `ci`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tairix_fuzzseed::firmware::{PI_FIRMWARE_CACHE, PI_FIRMWARE_ENV};
use tairix_itest_harness::pie::{PieArch, Profile};
use tairix_procinfo::display::format_bytes;

use crate::{elapsed_line, output_or_reason, Context, LONG_BUILD_COMMAND_TIMEOUT};

mod abi_check;
mod artsheet;
mod bench;
mod board_check;
mod c_header;
mod cfg_check;
mod charter_cite;
mod ci_long;
mod deps_check;
mod devids;
mod driver_exits;
mod event_ids;
mod feature_lane;
mod font_atlas;
mod font_store;
mod fssoak;
mod fuzz;
mod help_lint;
mod host_fonts;
mod icons;
mod ignore_check;
mod image_apps;
mod image_drivers;
mod json;
mod keys;
pub(crate) mod libtest;
mod linkcheck;
mod loom;
mod miri;
mod model_check;
mod netpeer;
pub(crate) mod parallel;
mod pie_build;
mod proptest;
mod qemu_tests;
mod rngsoak;
mod sbom;
mod seed;
mod soak;
mod sound_encode;
mod source_text;
mod source_tree;
mod spec_review;
mod spirv;
mod supply_chain;
mod target_clippy;
mod unicode;
mod wasm_tests;

/// One sanctioned developer workflow.
#[derive(Copy, Clone, Debug)]
pub enum Command {
    Build,
    Clean,
    Keys,
    Test,
    Clippy,
    Fmt,
    DocsCheck,
    AbiCheck,
    CHeader,
    FontAtlas,
    SoundEncode,
    Artsheet,
    Icons,
    Devids,
    Unicode,
    Spirv,
    DepsCheck,
    CfgCheck,
    BoardCheck,
    EventIds,
    DriverExits,
    HelpLint,
    Coverage,
    Sbom,
    SupplyChain,
    Fuzz,
    Proptest,
    FsSoak,
    RngSoak,
    Miri,
    Loom,
    ModelCheck,
    SpecReview,
    IgnoreCheck,
    SourceText,
    CharterCite,
    Bench,
    Ci,
    CiLong,
    Image,
    Run,
}

impl Command {
    /// The full set of subcommands, in the order presented to users.
    pub const ALL: &'static [Command] = &[
        Command::Build,
        Command::Clean,
        Command::Keys,
        Command::Test,
        Command::Clippy,
        Command::Fmt,
        Command::DocsCheck,
        Command::AbiCheck,
        Command::CHeader,
        Command::FontAtlas,
        Command::SoundEncode,
        Command::Artsheet,
        Command::Icons,
        Command::Devids,
        Command::Unicode,
        Command::Spirv,
        Command::DepsCheck,
        Command::CfgCheck,
        Command::BoardCheck,
        Command::EventIds,
        Command::DriverExits,
        Command::HelpLint,
        Command::Coverage,
        Command::Sbom,
        Command::SupplyChain,
        Command::Fuzz,
        Command::Proptest,
        Command::FsSoak,
        Command::RngSoak,
        Command::Miri,
        Command::Loom,
        Command::ModelCheck,
        Command::SpecReview,
        Command::IgnoreCheck,
        Command::SourceText,
        Command::CharterCite,
        Command::Bench,
        Command::Ci,
        Command::CiLong,
        Command::Image,
        Command::Run,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "build" => Command::Build,
            "clean" => Command::Clean,
            "keys" => Command::Keys,
            "test" => Command::Test,
            "clippy" => Command::Clippy,
            "fmt" => Command::Fmt,
            "docs-check" => Command::DocsCheck,
            "abi-check" => Command::AbiCheck,
            "c-header" => Command::CHeader,
            "font-atlas" => Command::FontAtlas,
            "sound-encode" => Command::SoundEncode,
            "artsheet" => Command::Artsheet,
            "icons" => Command::Icons,
            "devids" => Command::Devids,
            "unicode" => Command::Unicode,
            "spirv" => Command::Spirv,
            "deps-check" => Command::DepsCheck,
            "cfg-check" => Command::CfgCheck,
            "board-check" => Command::BoardCheck,
            "event-ids" => Command::EventIds,
            "driver-exits" => Command::DriverExits,
            "help-lint" => Command::HelpLint,
            "coverage" => Command::Coverage,
            "sbom" => Command::Sbom,
            "supply-chain" => Command::SupplyChain,
            "fuzz" => Command::Fuzz,
            "proptest" => Command::Proptest,
            "fssoak" => Command::FsSoak,
            "rngsoak" => Command::RngSoak,
            "loom" => Command::Loom,
            "miri" => Command::Miri,
            "model-check" => Command::ModelCheck,
            "spec-review" => Command::SpecReview,
            "ignore-check" => Command::IgnoreCheck,
            "source-text" => Command::SourceText,
            "charter-cite" => Command::CharterCite,
            "bench" => Command::Bench,
            "ci" => Command::Ci,
            "ci-long" => Command::CiLong,
            "image" => Command::Image,
            "run" => Command::Run,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Command::Build => "build",
            Command::Clean => "clean",
            Command::Keys => "keys",
            Command::Test => "test",
            Command::Clippy => "clippy",
            Command::Fmt => "fmt",
            Command::DocsCheck => "docs-check",
            Command::AbiCheck => "abi-check",
            Command::CHeader => "c-header",
            Command::FontAtlas => "font-atlas",
            Command::SoundEncode => "sound-encode",
            Command::Artsheet => "artsheet",
            Command::Icons => "icons",
            Command::Devids => "devids",
            Command::Unicode => "unicode",
            Command::Spirv => "spirv",
            Command::DepsCheck => "deps-check",
            Command::CfgCheck => "cfg-check",
            Command::BoardCheck => "board-check",
            Command::EventIds => "event-ids",
            Command::DriverExits => "driver-exits",
            Command::HelpLint => "help-lint",
            Command::Coverage => "coverage",
            Command::Sbom => "sbom",
            Command::SupplyChain => "supply-chain",
            Command::Fuzz => "fuzz",
            Command::Proptest => "proptest",
            Command::FsSoak => "fssoak",
            Command::RngSoak => "rngsoak",
            Command::Loom => "loom",
            Command::Miri => "miri",
            Command::ModelCheck => "model-check",
            Command::SpecReview => "spec-review",
            Command::IgnoreCheck => "ignore-check",
            Command::SourceText => "source-text",
            Command::CharterCite => "charter-cite",
            Command::Bench => "bench",
            Command::Ci => "ci",
            Command::CiLong => "ci-long",
            Command::Image => "image",
            Command::Run => "run",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            Command::Build => "Compile every workspace crate for the host target.",
            Command::Clean => "Delete cargo build artefacts to reclaim target/ disk space.",
            Command::Keys => "Create the build's secret signing key set (`keys init`).",
            Command::Test => "Run host-side unit and integration tests.",
            Command::Clippy => {
                "Run clippy with warnings denied, for the host and every Tier-1 target."
            }
            Command::Fmt => "Check formatting (`--fix` to apply).",
            Command::DocsCheck => "Build rustdoc and the mdBook with link checking.",
            Command::AbiCheck => "Verify generated ABI artefacts match their source of truth.",
            Command::CHeader => {
                "Generate/verify the C ABI development header (`--write` to regenerate)."
            }
            Command::FontAtlas => {
                "Generate/verify the system glyph atlas (`--write` to regenerate)."
            }
            Command::SoundEncode => "Encode authored WAV masters into a sound theme's FLAC assets.",
            Command::Artsheet => {
                "Measure WinterSun's figure art against its bounds (`--write` to regenerate \
                 the ledger, `--sheets` to render the contact sheets)."
            }
            Command::Icons => {
                "Draw/verify the file-type icon masters (`--write` to regenerate, \
                 `--sheets` to render the contact sheet)."
            }
            Command::Devids => {
                "Verify the vetted PCI/USB ID-database tables (`--write` to regenerate, \
                 `--fetch` to import upstream; developer-run only)."
            }
            Command::Unicode => {
                "Verify the Unicode property tables against the pinned UCD snapshot \
                 (`--write` to regenerate)."
            }
            Command::Spirv => {
                "Verify the SPIR-V grammar tables against the pinned Khronos grammar \
                 (`--write` to regenerate)."
            }
            Command::DepsCheck => "Enforce the §17.4 modularity dependency graph.",
            Command::CfgCheck => "Reject target-conditional compilation outside the arch ports.",
            Command::BoardCheck => "Reject a board, SoC or board part named in generic code.",
            Command::EventIds => "Check every crate's event ids against the id registry.",
            Command::DriverExits => "Refuse a driver exit code declared outside the driver runtime.",
            Command::HelpLint => {
                "Lint the command apps' Help/ trees: completeness, drift, content policy."
            }
            Command::Coverage => "Produce a host-side coverage report via cargo-llvm-cov.",
            Command::Sbom => "Emit a CycloneDX SBOM from Cargo.lock (§19.3).",
            Command::SupplyChain => {
                "Verify source-hash pins against Cargo.lock and the advisory SLA (§19.3)."
            }
            Command::Fuzz => "Drive the in-tree fuzz harnesses for a wall-clock budget (§19.6).",
            Command::Proptest => {
                "Drive the §19.7 stateful capability models for a wall-clock budget."
            }
            Command::FsSoak => {
                "Soak arxfs/ext4/fat32 on a ≥1 GiB RAM volume for a wall-clock budget."
            }
            Command::RngSoak => {
                "Soak the random generators through the statistical battery for a budget."
            }
            Command::Loom => {
                "Model-check the enrolled concurrent protocols over every thread interleaving."
            }
            Command::Miri => {
                "Interpret the crates with a hand-written unsafe core under the UB oracle."
            }
            Command::ModelCheck => {
                "Exhaustively model-check the §19.7 Silver capability + IPC state machine."
            }
            Command::SpecReview => "Reject unreviewed AI draft markers in source (§19.7).",
            Command::IgnoreCheck => "Reject an ignored test: the gate runs every test it builds.",
            Command::SourceText => "Reject control bytes in source text.",
            Command::CharterCite => {
                "Reject a comment or description citing a charter section, not the reason (§2.11)."
            }
            Command::Bench => {
                "Time the raster and compositor families in ns/px and ns/frame (evidence, not a gate)."
            }
            Command::Ci => "Run the full pipeline a pull request must pass.",
            Command::CiLong => {
                "Run the `ci` checks, repeating every test 20x sequentially then 20x concurrently."
            }
            Command::Image => "Build platform images via tools/mkimage.",
            Command::Run => {
                "Build a platform image and boot it interactively in QEMU (display + keyboard/mouse)."
            }
        }
    }

    pub fn run(self, ctx: &Context, args: &[OsString]) -> Result<(), String> {
        match self {
            Command::Build => run_build(ctx, args),
            Command::Clean => run_clean(ctx, args),
            Command::Keys => keys::run(ctx, args),
            Command::Test => run_test(ctx, args),
            Command::Clippy => run_clippy(ctx, args),
            Command::Fmt => run_fmt(ctx, args),
            Command::DocsCheck => run_docs_check(ctx, args),
            Command::AbiCheck => run_abi_check(ctx, args),
            Command::CHeader => run_c_header(ctx, args),
            Command::FontAtlas => run_font_atlas(ctx, args),
            Command::SoundEncode => sound_encode::run(ctx, args),
            Command::Artsheet => run_artsheet(ctx, args),
            Command::Icons => icons::run(ctx, args),
            Command::Devids => devids::run(ctx, args),
            Command::Unicode => unicode::run(ctx, args),
            Command::Spirv => spirv::run(ctx, args),
            Command::DepsCheck => run_deps_check(ctx),
            Command::CfgCheck => run_cfg_check(ctx),
            Command::BoardCheck => run_board_check(ctx),
            Command::EventIds => run_event_ids(ctx),
            Command::DriverExits => run_driver_exits(ctx),
            Command::HelpLint => help_lint::run(ctx),
            Command::Coverage => run_coverage(ctx, args),
            Command::Sbom => run_sbom(ctx, args),
            Command::SupplyChain => run_supply_chain(ctx, args),
            Command::Fuzz => run_fuzz(ctx, args),
            Command::Proptest => run_proptest(ctx, args),
            Command::FsSoak => run_fssoak(ctx, args),
            Command::RngSoak => run_rngsoak(ctx, args),
            Command::Loom => loom::run(ctx, args),
            Command::Miri => miri::run(ctx, args),
            Command::ModelCheck => run_model_check(args),
            Command::SpecReview => run_spec_review(ctx),
            Command::IgnoreCheck => run_ignore_check(ctx),
            Command::SourceText => run_source_text(ctx),
            Command::CharterCite => run_charter_cite(ctx),
            Command::Bench => run_bench(args),
            Command::Ci => run_ci(ctx),
            Command::CiLong => run_ci_long(ctx, args),
            Command::Image => run_image(ctx, args),
            Command::Run => run_run(ctx, args),
        }
    }
}

fn run_build(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // `--target <image platform>` (e.g. `aarch64-rpi`) asks for a flashable
    // platform image rather than a host workspace build; that pipeline is
    // the `image` subcommand's, so delegate the whole argument list there
    // (PLAN.md Stage 8 / plans/PI.md P9 — `cargo xtask build --target
    // aarch64-rpi` and `cargo xtask image --target aarch64-rpi` are the
    // same build).
    if args
        .windows(2)
        .any(|w| w[0] == "--target" && w[1] == "aarch64-rpi")
    {
        return run_image(ctx, args);
    }

    // `--headless` builds the first-class headless configuration required
    // by: every `userland/gui/*` crate is excluded
    // from the image so the system must remain buildable without the
    // desktop. The flag is consumed here; everything else is forwarded.
    // A `--target` is named the way cargo knows the target and passed the way
    // cargo selects it, which for the first-party x86_64 spec is a path.
    let mut headless = false;
    let mut forward = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--headless" {
            headless = true;
        } else if a == "--target" {
            let name = it
                .next()
                .and_then(|v| v.to_str())
                .ok_or("build: --target requires a UTF-8 value")?;
            forward.extend(tairix_itest_harness::pie::cargo_target_args(name));
        } else {
            forward.push(a.clone());
        }
    }

    let mut cmd = ctx.cargo();
    cmd.args(["build", "--workspace", "--all-targets", "--locked"]);
    if headless {
        for gui in GUI_CRATES {
            cmd.arg("--exclude");
            cmd.arg(gui);
        }
    }
    cmd.args(&forward);
    ctx.run(
        if headless {
            "build --headless"
        } else {
            "build"
        },
        cmd,
    )
}

/// Remove cargo's build artefacts to reclaim `target/` disk space.
///
/// A full multi-arch workspace build is large: `-Z build-std` rebuilds the
/// whole standard library for each of the four bare-metal Tier-1 targets,
/// every crate is compiled with debug info, and the per-target profile
/// directories grow into tens of gigabytes apiece. Reclaiming that space is
/// a developer flow in its own right, so it is a *named* subcommand rather
/// than behaviour hidden inside another step (see the closed command set in
/// `main.rs`).
///
/// The work is delegated to `cargo clean`, exactly as `build`/`test`/`fmt`
/// delegate to their cargo subcommands: cargo owns the artefact layout and
/// honours the same `$CARGO_TARGET_DIR` resolution the rest of xtask relies
/// on (`Context::target_dir`), so re-implementing the deletion here would
/// only risk diverging from it. Any arguments are forwarded verbatim, so the
/// usual cargo selectors work: `--release`, `--doc`, `--target <triple>`,
/// and `-p <crate>` each scope the clean instead of wiping everything.
///
/// The reclaimed size is measured around the clean and reported, so the
/// operator sees how much of the multi-gigabyte tree was freed.
fn run_clean(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let target_dir = ctx.target_dir();
    let before = dir_size(&target_dir);

    let mut cmd = ctx.cargo();
    cmd.arg("clean");
    cmd.args(args);
    ctx.run("clean", cmd)?;

    let after = dir_size(&target_dir);
    noteln!(
        "xtask: [clean] reclaimed {} ({} -> {} in {})",
        format_bytes(before.saturating_sub(after)),
        format_bytes(before),
        format_bytes(after),
        relative(&ctx.workspace_root, &target_dir),
    );
    Ok(())
}

/// Total size in bytes of every regular file at or below `path`.
///
/// Best-effort and side-effect free: it is only used to report how much
/// space a clean reclaimed, so unreadable entries and a missing directory
/// are skipped rather than treated as errors (a clean must never fail
/// because the report could not be computed). Symlinks are not followed, so
/// the same bytes are never counted twice and the walk cannot cycle.
fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// The `userland/gui/*` crates excluded from the headless image.
const GUI_CRATES: &[&str] = &["tairix-wm", "tairix-taskbar"];

/// How many times the test matrix repeats.
///
/// `ci` and `--count N` drive a fixed number of passes; a soak drives a
/// wall-clock budget instead. Either way the *whole* matrix (host, then
/// opt-in QEMU and wasm) is one pass, so a budget is not multiplied across
/// stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunBudget {
    /// Run the matrix exactly this many times (always ≥ 1).
    Count(u32),
    /// Repeat the matrix until the budget runs out. A span counts from the
    /// first pass; a window's close stays put, so the matrix build spends it.
    Soak(soak::Budget),
}

impl RunBudget {
    /// Run `body` once per matrix pass, passing the 1-based pass number.
    ///
    /// `Count(n)` runs exactly `n` passes (clamped to ≥ 1). `Soak` always
    /// runs one pass and starts another only while one as long as the last
    /// still fits before the budget's end: a pass of the whole matrix is long,
    /// so starting one that cannot finish overruns the budget by most of a
    /// pass. A pass in flight always finishes; the suite is never cut off
    /// mid-run.
    fn for_each<F>(self, mut body: F) -> Result<(), String>
    where
        F: FnMut(u64) -> Result<(), String>,
    {
        match self {
            RunBudget::Count(n) => {
                for pass in 1..=u64::from(n.max(1)) {
                    body(pass)?;
                }
                Ok(())
            }
            RunBudget::Soak(budget) => {
                let deadline = budget
                    .deadline()
                    .ok_or_else(|| "test: the soak ends past the clock's range".to_string())?;
                let mut pass = 0u64;
                loop {
                    pass += 1;
                    let began = Instant::now();
                    body(pass)?;
                    if !tairix_fuzzseed::room_for_another_pass(Some(deadline), began.elapsed()) {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Whether more than one pass is expected (gates per-pass logging).
    fn is_repeated(self) -> bool {
        !matches!(self, RunBudget::Count(1))
    }

    /// Human-readable budget for the `[test]` banner.
    fn describe(self) -> String {
        match self {
            RunBudget::Count(n) => format!("{n} pass(es)"),
            RunBudget::Soak(budget) => format!("soaking for {}", soak::describe(Some(budget))),
        }
    }
}

/// Parsed options for the `test` subcommand.
#[derive(Debug)]
struct TestOptions {
    /// How many times to repeat the whole matrix (host, QEMU, wasm).
    budget: RunBudget,
    /// Run the bare-metal QEMU integration matrix (`--qemu`).
    run_qemu: bool,
    /// Run the wasm32 verticals (`--wasm`).
    run_wasm: bool,
    /// Run the scene tier as well (`--scenes`): the long host tests of whole
    /// ray-traced scenes and realms, compiled only under
    /// [`SCENE_TESTS_FEATURE`].
    run_scenes: bool,
    /// Restrict the QEMU matrix to enrolments whose package name contains
    /// this substring (`--only`, requires `--qemu`) — the debugging filter
    /// for iterating on one vertical. The host `cargo test` stage is
    /// skipped so a `--only` run exercises exactly the named guests; it is
    /// never a substitute for the whole matrix.
    only: Option<String>,
    /// The order each host pass starts its tests in.
    order: PassOrder,
    /// Remaining arguments forwarded verbatim to `cargo test`.
    forward: Vec<OsString>,
}

/// The order a host pass starts its tests in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassOrder {
    /// The harness's own, alphabetical.
    Alphabetical,
    /// Randomised (`--shuffle`), so a suite that passes only in the harness's
    /// alphabetical order fails the gate instead of hiding until an unrelated
    /// change reshuffles it (`plans/OPEN-DEFECTS.md` D90): from a pinned seed
    /// (`--shuffle-seed N`) or, with none, a fresh one each pass.
    Shuffled(Option<u64>),
}

impl PassOrder {
    /// The order seed of pass `pass`, counted from one, which its label logs;
    /// `None` keeps the harness's order. A pinned seed is the first pass's own,
    /// so any pass a label names replays from its seed alone, and each later
    /// pass derives its own from it.
    fn seed(self, pass: u64) -> Option<u64> {
        match self {
            Self::Alphabetical => None,
            Self::Shuffled(Some(pinned)) if pass <= 1 => Some(pinned),
            Self::Shuffled(base) => Some(seed::job_seed(
                base,
                usize::try_from(pass).unwrap_or(usize::MAX),
            )),
        }
    }
}

impl TestOptions {
    /// The host lanes a pass runs after the workspace's: every one, less the
    /// scene tier unless it was asked for.
    fn host_lanes(&self) -> impl Iterator<Item = HostLane> + '_ {
        HostLane::ALL
            .into_iter()
            .filter(|lane| self.run_scenes || *lane != HostLane::Scenes)
    }
}

/// Parse the `test` subcommand arguments.
///
/// Recognises `--qemu`, `--wasm`, `--scenes`, `--count N` (alias
/// `--iterations N`), `--soak`, `--secs N`, `--until T`, `--shuffle`, and
/// `--shuffle-seed N`; everything else is forwarded verbatim to `cargo test`.
/// `--count` rejects a missing, non-numeric, or zero value rather than
/// silently defaulting, so a typo can never quietly collapse the matrix to a
/// single run. A fixed count and a wall-clock budget are mutually exclusive:
/// combining `--count` with `--soak`/`--secs`/`--until` is an error rather
/// than a silent precedence rule.
fn parse_test_options(args: &[OsString]) -> Result<TestOptions, String> {
    let mut forward = Vec::with_capacity(args.len());
    let mut run_qemu = false;
    let mut run_wasm = false;
    let mut run_scenes = false;
    let mut count: Option<u32> = None;
    let mut soak_flags = soak::Flags::default();
    let mut only: Option<String> = None;
    let mut order = PassOrder::Alphabetical;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if let Some(flag) = a.to_str() {
            if soak_flags.take_override("test", flag, &mut iter)? {
                continue;
            }
        }
        if a == "--qemu" {
            run_qemu = true;
        } else if a == "--wasm" {
            run_wasm = true;
        } else if a == "--scenes" {
            run_scenes = true;
        } else if a == "--soak" {
            soak_flags.set_mode("test", soak::Mode::Soak)?;
        } else if a == "--only" {
            let value = iter
                .next()
                .ok_or_else(|| "test: `--only` requires a package substring".to_string())?;
            let text = value
                .to_str()
                .ok_or_else(|| "test: `--only` value is not valid UTF-8".to_string())?;
            if text.is_empty() {
                return Err("test: `--only` requires a non-empty substring".to_string());
            }
            only = Some(text.to_string());
        } else if a == "--count" || a == "--iterations" {
            let value = iter.next().ok_or_else(|| {
                format!(
                    "test: `{}` requires a positive integer argument",
                    a.to_string_lossy()
                )
            })?;
            count = Some(parse_iteration_count(value)?);
        } else if a == "--shuffle" {
            if order == PassOrder::Alphabetical {
                order = PassOrder::Shuffled(None);
            }
        } else if a == "--shuffle-seed" {
            let value = iter
                .next()
                .ok_or_else(|| "test: `--shuffle-seed` requires a u64 argument".to_string())?;
            order = PassOrder::Shuffled(Some(parse_shuffle_seed(value)?));
        } else {
            forward.push(a.clone());
        }
    }

    // A wall-clock budget and a fixed pass count are two ways of saying the
    // same thing; allowing both would need an arbitrary precedence rule, so
    // fail closed instead.
    if soak_flags.given() && count.is_some() {
        return Err("test: `--count`/`--iterations` cannot be combined with \
             `--soak`/`--secs`/`--until`; choose a fixed pass count or a wall-clock budget"
            .to_string());
    }
    // `--only` scopes the QEMU matrix, so it is meaningless without it —
    // refuse rather than silently ignore a filter the caller relied on.
    if only.is_some() && !run_qemu {
        return Err("test: `--only` requires `--qemu`".to_string());
    }
    if only.is_some() && run_scenes {
        return Err(
            "test: `--scenes` cannot be combined with `--only`, which skips the host stage"
                .to_string(),
        );
    }
    let budget = match soak_flags.budget("test", soak::Mode::Once)? {
        Some(budget) => RunBudget::Soak(budget),
        None => RunBudget::Count(count.unwrap_or(1)),
    };

    Ok(TestOptions {
        budget,
        run_qemu,
        run_wasm,
        run_scenes,
        only,
        order,
        forward,
    })
}

/// Parse a `--shuffle-seed` value: any `u64`.
fn parse_shuffle_seed(value: &OsString) -> Result<u64, String> {
    let text = value
        .to_str()
        .ok_or_else(|| "test: `--shuffle-seed` value is not valid UTF-8".to_string())?;
    text.parse::<u64>().map_err(|_| {
        format!("test: invalid `--shuffle-seed` value {text:?}; expected an unsigned integer")
    })
}

/// Parse a `--count`/`--iterations` value: a positive (non-zero) integer.
fn parse_iteration_count(value: &OsString) -> Result<u32, String> {
    let text = value
        .to_str()
        .ok_or_else(|| "test: iteration count is not valid UTF-8".to_string())?;
    let count: u32 = text.parse().map_err(|_| {
        format!("test: invalid iteration count {text:?}; expected a positive integer")
    })?;
    if count == 0 {
        return Err("test: iteration count must be at least 1".to_string());
    }
    Ok(count)
}

fn run_test(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // `--qemu` opts in to the bare-metal QEMU integration tests in
    // `tests/integration/*`. Per they share the test
    // entry point (`cargo xtask test`) so a single command runs the
    // whole matrix; per the same section we never retry on failure
    // and every run has a strict, finite timeout.
    //
    // `--count N` (alias `--iterations N`) runs the whole matrix N times;
    // it defaults to one, and `ci` runs the matrix exactly once. The
    // flake-hunting repetition lives in the time-limited GitHub soaks:
    // `--soak` repeats the matrix for a wall-clock budget, tuned by
    // `--secs N` or ended at the close of the nightly workflow's window by
    // `--until T`. `--count N` remains for the orchestrator's own tests and
    // ad-hoc local repeat runs.
    //
    // `--shuffle` randomises each host pass's start order; see
    // [`host_order_args`] for why the gate wants it.
    //
    // `--scenes` adds the scene tier, which `ci` leaves to `ci-long`: the long
    // tests of whole ray-traced scenes cost more with every detail they gain.
    let opts = parse_test_options(args)?;

    // A `--only` run is a debugging pass over the named guests alone; the
    // whole-workspace host stage would drown the signal, so it is skipped (and
    // a `--only` run is never a substitute for the full matrix). The firmware
    // its tests read is staged first, so a run that cannot stage it fails
    // before building anything.
    let firmware = opts
        .only
        .is_none()
        .then(|| test_firmware(ctx))
        .transpose()?;

    // Build the opt-in matrices once, before any repeated passes, so a soak
    // re-runs the binaries rather than rebuilding them each pass. The host
    // `cargo test` invocation builds incrementally on its own.
    if opts.run_qemu {
        qemu_tests::build_all(ctx, opts.only.as_deref())?;
    }
    // `--wasm` runs the wasm32 verticals, two of them in a headless browser;
    // `ci` passes it, and the harnesses need node and the puppeteer
    // `tests/integration/package-lock.json` pins (see `commands/wasm_tests.rs`).
    if opts.run_wasm {
        wasm_tests::prepare(ctx)?;
    }

    if opts.budget.is_repeated() {
        noteln!("xtask: [test] {}", opts.budget.describe());
    }
    // A pass is the *whole* matrix: host, then QEMU, then wasm. Looping here
    // (rather than inside each stage) means a duration budget covers the
    // matrix as a unit instead of being spent in full on each stage.
    opts.budget.for_each(|pass| {
        if let Some(firmware) = &firmware {
            for step in host_pass(ctx, &opts, pass, firmware)? {
                ctx.run_accounted(&step.label, step.command, step.budget, step.account)?;
            }
        }
        if opts.run_qemu {
            qemu_tests::run_once(ctx, opts.only.as_deref())?;
        }
        if opts.run_wasm {
            wasm_tests::run_once(ctx)?;
        }
        Ok(())
    })
}

/// One host step of a test pass: a libtest command and what it must run.
struct HostStep {
    label: String,
    command: std::process::Command,
    budget: Duration,
    account: libtest::Account,
}

/// The host steps of `pass`: the workspace's tests, its doctests unless the
/// forwarded arguments select targets, then each host lane's, the device-tree
/// tests among them reading `firmware`, and each held to every test it is
/// given or, under forwarded arguments, to ignoring none of those they leave.
fn host_pass(
    ctx: &Context,
    opts: &TestOptions,
    pass: u64,
    firmware: &Path,
) -> Result<Vec<HostStep>, String> {
    let order = opts.order.seed(pass);
    let label = match (opts.budget.is_repeated(), order) {
        (true, Some(order)) => format!("test (pass {pass}, order seed {order})"),
        (true, None) => format!("test (pass {pass})"),
        (false, Some(order)) => format!("test (order seed {order})"),
        (false, None) => "test".to_string(),
    };
    let account = if opts.forward.is_empty() {
        libtest::Account::Everything
    } else {
        libtest::Account::Forwarded
    };
    let mut steps = vec![HostStep {
        label: label.clone(),
        command: host_command(ctx, HostRun::Workspace, &opts.forward, order, firmware),
        budget: crate::DEFAULT_COMMAND_TIMEOUT,
        account,
    }];
    if !selects_targets(&opts.forward) {
        steps.push(HostStep {
            label: format!("{label} (doctests)"),
            command: host_command(ctx, HostRun::Doctests, &opts.forward, order, firmware),
            budget: crate::DEFAULT_COMMAND_TIMEOUT,
            account,
        });
    }
    for lane in opts.host_lanes() {
        let selection = lane.selection(ctx)?;
        steps.push(HostStep {
            label: format!("{label} ({})", lane.label()),
            command: host_command(
                ctx,
                HostRun::Lane(&selection),
                &opts.forward,
                order,
                firmware,
            ),
            budget: lane.budget(),
            account,
        });
    }
    Ok(steps)
}

/// What one host test step runs.
#[derive(Clone, Copy)]
enum HostRun<'a> {
    /// Every host-testable crate's tests.
    Workspace,
    /// The workspace's doctests.
    Doctests,
    /// A host lane, by its selection.
    Lane(&'a [OsString]),
}

/// The command a host test step runs, in `ci`'s pass and `ci-long`'s hunt
/// alike: `forward` passed on, the order `order` seeds, and `firmware` named
/// to the device-tree tests.
fn host_command(
    ctx: &Context,
    run: HostRun<'_>,
    forward: &[OsString],
    order: Option<u64>,
    firmware: &Path,
) -> std::process::Command {
    let mut command = match run {
        HostRun::Workspace => host_test_command(ctx),
        HostRun::Doctests => host_doctest_command(ctx),
        HostRun::Lane(selection) => HostLane::command(ctx, "test", selection),
    };
    command.env(PI_FIRMWARE_ENV, firmware);
    command.args(forward);
    if let Some(order) = order {
        command.args(host_order_args(forward, order));
    }
    command
}

/// Whether `forward` selects cargo targets, which leaves the doctests out
/// of the selection, as cargo refuses a selector beside `--doc`.
fn selects_targets(forward: &[OsString]) -> bool {
    const SELECTORS: [&str; 10] = [
        "--lib",
        "--bins",
        "--bin",
        "--tests",
        "--test",
        "--examples",
        "--example",
        "--benches",
        "--bench",
        "--all-targets",
    ];
    forward
        .iter()
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str())
        .any(|arg| {
            let flag = arg.split_once('=').map_or(arg, |(flag, _)| flag);
            SELECTORS.contains(&flag)
        })
}

/// A host configuration the workspace pass builds none of: packages whose
/// gated code compiles only with the features the lane turns on in them, so
/// without the lane their gated tests would never run and never be linted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostLane {
    /// Every package the debug image's kernel diagnostics turn a feature on in.
    Diagnostics,
    /// Every package declaring [`SCENE_TESTS_FEATURE`]. `ci` lints it but
    /// does not run it; `ci-long` and `cargo xtask test --scenes` do.
    Scenes,
}

impl HostLane {
    const ALL: [Self; 2] = [Self::Diagnostics, Self::Scenes];

    const fn label(self) -> &'static str {
        match self {
            Self::Diagnostics => "debug-image kernel diagnostics",
            Self::Scenes => "scene tier",
        }
    }

    /// Whether one run of the lane fills the machine on its own, every core
    /// and at the scene tier's peak some twenty gigabytes: such a run gets the
    /// long budget, and its replicas run one at a time.
    const fn fills_the_machine(self) -> bool {
        matches!(self, Self::Scenes)
    }

    /// The wall-clock budget one run of the lane gets.
    const fn budget(self) -> Duration {
        if self.fills_the_machine() {
            crate::LONG_BUILD_COMMAND_TIMEOUT
        } else {
            crate::DEFAULT_COMMAND_TIMEOUT
        }
    }

    /// The lane's packages, each with the features the lane turns on there.
    fn reach(self, ctx: &Context) -> Result<feature_lane::Reach, String> {
        let reach = match self {
            Self::Diagnostics => {
                let features: Vec<&str> = KERNEL_DIAGNOSTICS_FEATURES.split(',').collect();
                feature_lane::reach(&ctx.workspace_root, KERNEL_PACKAGE, &features)?
            }
            Self::Scenes => feature_lane::declared(&ctx.workspace_root, SCENE_TESTS_FEATURE)?,
        };
        // An empty selection would read as the whole workspace to cargo.
        if reach.is_empty() {
            return Err(format!("the {} lane selects no package", self.label()));
        }
        Ok(reach)
    }

    /// The `-p` and `--features` arguments selecting the lane's packages with
    /// its features on.
    fn selection(self, ctx: &Context) -> Result<Vec<OsString>, String> {
        let reach = self.reach(ctx)?;
        let mut args = Vec::with_capacity(2 * reach.len() + 2);
        for package in reach.keys() {
            args.push(OsString::from("-p"));
            args.push(OsString::from(package));
        }
        let enabled: Vec<String> = reach
            .iter()
            .flat_map(|(package, features)| {
                features
                    .iter()
                    .map(move |feature| format!("{package}/{feature}"))
            })
            .collect();
        args.push(OsString::from("--features"));
        args.push(OsString::from(enabled.join(",")));
        Ok(args)
    }

    /// `cargo <subcommand> --all-targets --locked` over a lane's `selection`.
    fn command(ctx: &Context, subcommand: &str, selection: &[OsString]) -> std::process::Command {
        let mut cmd = ctx.cargo();
        cmd.args([subcommand, "--all-targets", "--locked"]);
        cmd.args(selection);
        cmd
    }

    /// [`Self::command`] over this lane's own selection.
    fn cargo(self, ctx: &Context, subcommand: &str) -> Result<std::process::Command, String> {
        Ok(Self::command(ctx, subcommand, &self.selection(ctx)?))
    }
}

/// The libtest arguments that start a host pass in `seed`'s order.
///
/// A test suite is a set, not a sequence: one that passes only in the
/// harness's default alphabetical order is relying on whichever test happened
/// to reach a process-global first, and stays green until an unrelated change
/// renames or adds a test and reshuffles it. Ordering every gate run afresh
/// turns that latency into a failure the seed in the run's label replays
/// exactly (`plans/OPEN-DEFECTS.md` D90).
///
/// A `--` already in the forwarded arguments is the caller's own harness
/// separator, so a second one would read as a test-name filter rather than a
/// separator; the ordering flags then just join what the caller passed.
pub(super) fn host_order_args(forward: &[OsString], seed: u64) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::with_capacity(4);
    if !forward.iter().any(|a| a == "--") {
        args.push(OsString::from("--"));
    }
    args.push(OsString::from("-Z"));
    args.push(OsString::from("unstable-options"));
    args.push(OsString::from("--shuffle-seed"));
    args.push(OsString::from(seed.to_string()));
    args
}

/// Lint the workspace for the host, then each [`HostLane`], then once per
/// freestanding Tier-1 target.
///
/// The host pass alone lints almost none of the shipped code: a kernel,
/// driver, service or application body is compiled only when its crate is
/// built for a bare-metal triple, so the target passes are what make this gate
/// cover the system rather than its host stubs (see [`target_clippy`]). A
/// lane's gated code is built in no other host configuration.
fn run_clippy(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    for (label, cmd) in host_clippy(ctx, args)? {
        ctx.run(&label, cmd)?;
    }
    target_clippy::run(ctx, args)
}

/// The host clippy passes, labelled: the workspace, then each [`HostLane`].
fn host_clippy(
    ctx: &Context,
    args: &[OsString],
) -> Result<Vec<(String, std::process::Command)>, String> {
    let mut workspace = ctx.cargo();
    workspace.args(["clippy", "--workspace", "--all-targets", "--locked"]);
    let mut passes = vec![("clippy (host)".to_string(), workspace)];
    for lane in HostLane::ALL {
        passes.push((
            format!("clippy (host, {})", lane.label()),
            lane.cargo(ctx, "clippy")?,
        ));
    }
    for (_, cmd) in &mut passes {
        cmd.args(["--", "-D", "warnings"]);
        cmd.args(args);
    }
    Ok(passes)
}

fn run_fmt(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let apply = args.iter().any(|a| a == "--fix" || a == "--apply");
    let mut cmd = ctx.cargo();
    cmd.args(["fmt", "--all"]);
    if !apply {
        cmd.args(["--", "--check"]);
    }
    ctx.run(if apply { "fmt --fix" } else { "fmt --check" }, cmd)
}

const DOCS_RUSTDOCFLAGS: &str = "-D warnings";

fn run_docs_check(ctx: &Context, _args: &[OsString]) -> Result<(), String> {
    // rustdoc with warnings denied — broken intra-doc links fail the build.
    //
    // Cargo already schedules independent rustdoc units concurrently. Each
    // rustdoc therefore retains its single-threaded default: enabling all
    // host threads inside every unit multiplies Cargo's parallelism and can
    // exhaust memory on a clean build of this large workspace.
    //
    // `-Z rustdoc-mergeable-info` (a cargo `-Z` flag, RFC 3662) makes cargo
    // drive rustdoc's mergeable cross-crate-info: each crate writes its
    // partial cross-crate info (`doc.parts`) and a final cheap merge step
    // links them, instead of every `rustdoc` invocation loading and rewriting
    // the shared `target/doc` cross-crate index. That shared-mutable index is
    // O(crates) work per crate (so O(crates²) overall) and serialises doc
    // units on the doc root; the parts-then-merge model removes that
    // contention, which matters across this 167-crate workspace. Nightly-only
    // like the flags above and consistent with the pinned-nightly posture.
    let mut doc = ctx.cargo();
    doc.args([
        "doc",
        "--workspace",
        "--no-deps",
        "--locked",
        "--document-private-items",
        "-Z",
        "rustdoc-mergeable-info",
    ])
    .env("RUSTDOCFLAGS", DOCS_RUSTDOCFLAGS);
    ctx.run("docs-check (rustdoc)", doc)?;

    // mdBook build. The book lives in `docs/`.
    probe_version(ctx, "mdbook")
        .map_err(|why| format!("{why}; install mdbook with `cargo install --locked mdbook`"))?;
    let mut book = std::process::Command::new("mdbook");
    book.current_dir(ctx.workspace_root.join("docs"));
    book.args(["build"]);
    ctx.run("docs-check (mdbook)", book)?;

    // In-tree relative-link checker; see `commands/linkcheck.rs` for the
    // rationale for owning this rather than delegating to a preprocessor.
    let book_src = ctx.workspace_root.join("docs/src");
    noteln!("xtask: [docs-check (linkcheck)] {}", book_src.display());
    linkcheck::run(&book_src)?;
    Ok(())
}

fn run_abi_check(ctx: &Context, _args: &[OsString]) -> Result<(), String> {
    // Stage 2.7: real syscall ABI cross-check. `abi_check::check_sync`
    // enforces both the pair-existence rule and the
    // SHA-256 hash equality between the kernel-side table and the
    // `lib/abi` source of truth. Its unit tests exercise the desync
    // failure mode against a mutated fixture (see
    // `tools/xtask/src/commands/abi_check.rs`).
    let syscalls = ctx.workspace_root.join(abi_check::DEFAULT_SYSCALLS_PATH);
    let table = ctx.workspace_root.join(abi_check::DEFAULT_TABLE_PATH);
    noteln!(
        "xtask: [abi-check] {} ↔ {}",
        relative(&ctx.workspace_root, &syscalls),
        relative(&ctx.workspace_root, &table),
    );
    abi_check::check_sync(&ctx.workspace_root, &syscalls, &table)
}

fn run_c_header(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // The C development header is a generated view of the
    // `lib/abi` source of truth. With no arguments this verifies the
    // committed header is in sync (the `ci` drift guard); `--write`
    // regenerates it, reviewed by diff like the kernel syscall table.
    let mut write = false;
    for arg in args {
        if arg == "--write" {
            write = true;
        } else {
            return Err(format!(
                "c-header: unexpected argument {}; usage: cargo xtask c-header [--write]",
                arg.display()
            ));
        }
    }
    let include_dir = ctx.workspace_root.join(c_header::DEFAULT_INCLUDE_DIR);
    if write {
        noteln!(
            "xtask: [c-header --write] {}",
            relative(&ctx.workspace_root, &include_dir)
        );
        c_header::write(&ctx.workspace_root, &include_dir)
    } else {
        noteln!(
            "xtask: [c-header] {}",
            relative(&ctx.workspace_root, &include_dir)
        );
        c_header::check_sync(&ctx.workspace_root, &include_dir)
    }
}

fn run_font_atlas(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // The glyph atlas is a generated view of the committed system font faces.
    // With no arguments this verifies the committed atlas is in sync (the
    // `ci` drift guard); `--write` regenerates it, reviewed by diff like the
    // generated C header.
    let mut write = false;
    for arg in args {
        if arg == "--write" {
            write = true;
        } else {
            return Err(format!(
                "font-atlas: unexpected argument {}; usage: cargo xtask font-atlas [--write]",
                arg.display()
            ));
        }
    }
    if write {
        noteln!(
            "xtask: [font-atlas --write] {}",
            font_atlas::DEFAULT_ATLAS_RS_PATH
        );
        font_atlas::write(&ctx.workspace_root)
    } else {
        noteln!("xtask: [font-atlas] {}", font_atlas::DEFAULT_ATLAS_RS_PATH);
        font_atlas::check_sync(&ctx.workspace_root)
    }
}

fn run_artsheet(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // The figure art gate. With no arguments this measures the shipped
    // figure, holds every number against its bound, and verifies the
    // committed ledger has not drifted — which is what `ci` runs. `--write`
    // regenerates the ledger, reviewed by diff like the generated C header;
    // `--sheets` renders the pictures a human judges.
    let (mut write, mut sheets) = (false, false);
    for arg in args {
        match arg.to_str() {
            Some("--write") => write = true,
            Some("--sheets") => sheets = true,
            _ => {
                return Err(format!(
                    "artsheet: unexpected argument {}; usage: cargo xtask artsheet \
                     [--write] [--sheets]",
                    arg.display()
                ))
            }
        }
    }
    if write {
        noteln!("xtask: [artsheet --write] {}", artsheet::LEDGER_PATH);
        artsheet::write(&ctx.workspace_root)?;
    } else {
        noteln!("xtask: [artsheet] {}", artsheet::LEDGER_PATH);
        artsheet::check(&ctx.workspace_root)?;
    }
    if sheets {
        artsheet::sheets(&ctx.workspace_root)?;
    }
    Ok(())
}

fn run_deps_check(ctx: &Context) -> Result<(), String> {
    // walk the workspace dependency graph and reject any
    // layering violation, concrete-scheduler naming outside the sanctioned
    // crates, or a non-GUI crate reaching the optional desktop.
    noteln!("xtask: [deps-check] {}", ctx.workspace_root.display());
    deps_check::run(&ctx.workspace_root)
}

fn run_cfg_check(ctx: &Context) -> Result<(), String> {
    // reject target-conditional compilation outside the
    // architecture ports and the build glue.
    noteln!("xtask: [cfg-check] {}", ctx.workspace_root.display());
    cfg_check::run(&ctx.workspace_root, &ctx.children)
}

fn run_board_check(ctx: &Context) -> Result<(), String> {
    noteln!("xtask: [board-check] {}", ctx.workspace_root.display());
    board_check::run(&ctx.workspace_root, &ctx.children)
}

fn run_event_ids(ctx: &Context) -> Result<(), String> {
    noteln!("xtask: [event-ids] {}", ctx.workspace_root.display());
    event_ids::run(&ctx.workspace_root)
}

fn run_driver_exits(ctx: &Context) -> Result<(), String> {
    noteln!("xtask: [driver-exits] {}", ctx.workspace_root.display());
    driver_exits::run(&ctx.workspace_root)
}

fn run_coverage(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // `cargo-llvm-cov` is a cargo subcommand: its binary rejects a bare
    // `--version` and is only reachable as `cargo llvm-cov`. Probe it the
    // same way it is invoked below so the availability check matches reality.
    cargo_subcommand_available(ctx, "llvm-cov").map_err(|why| {
        format!("{why}; install cargo-llvm-cov with `cargo install cargo-llvm-cov --locked`")
    })?;
    let mut cmd = ctx.cargo();
    cmd.args(["llvm-cov", "--workspace", "--locked", "--summary-only"]);
    cmd.args(args);
    ctx.run("coverage", cmd)
}

fn run_sbom(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // emit a CycloneDX SBOM from the committed `Cargo.lock`. The
    // default is stdout (composes with redirection and signing); an
    // explicit `--output PATH` (or `-o PATH`) writes the document to disk,
    // creating any missing parent directories (e.g. the gitignored
    // `images/`). The generator itself lives in `commands/sbom.rs`.
    let mut output: Option<std::path::PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--output" || arg == "-o" {
            let path = iter
                .next()
                .ok_or_else(|| "sbom: `--output` requires a path argument".to_string())?;
            output = Some(std::path::PathBuf::from(path));
        } else {
            return Err(format!(
                "sbom: unexpected argument {}; usage: cargo xtask sbom [--output PATH]",
                arg.display()
            ));
        }
    }
    sbom::run(&ctx.workspace_root, output.as_deref())
}

fn run_supply_chain(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // verify the committed source-hash allow-list against
    // `Cargo.lock` and enforce the advisory SLA. `--write-pins`
    // regenerates the `[[source-pin]]` blocks from the lockfile
    // (reviewed by diff, like the lockfile itself); the default verifies.
    let mut write_pins = false;
    for arg in args {
        if arg == "--write-pins" {
            write_pins = true;
        } else {
            return Err(format!(
                "supply-chain: unexpected argument {}; usage: \
                 cargo xtask supply-chain [--write-pins]",
                arg.display()
            ));
        }
    }
    noteln!("xtask: [supply-chain] {}", ctx.workspace_root.display());
    supply_chain::run(&ctx.workspace_root, write_pins)
}

fn run_fuzz(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let opts = fuzz::parse(args)?;
    noteln!("xtask: [fuzz] {}", ctx.workspace_root.display());
    fuzz::run(ctx, &opts)
}

fn run_proptest(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let opts = proptest::parse(args)?;
    noteln!("xtask: [proptest] {}", ctx.workspace_root.display());
    proptest::run(ctx, &opts)
}

fn run_fssoak(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let opts = fssoak::parse(args)?;
    noteln!("xtask: [fssoak] {}", ctx.workspace_root.display());
    fssoak::run(ctx, &opts)
}

fn run_rngsoak(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    // `ci` gets its coverage from the harness's own fixed-seed smoke pass in
    // the host test phase.
    let opts = rngsoak::parse(args)?;
    noteln!("xtask: [rngsoak] {}", ctx.workspace_root.display());
    rngsoak::run(ctx, &opts)
}

fn run_model_check(args: &[OsString]) -> Result<(), String> {
    // Silver: exhaustively model-check the capability + IPC state
    // machine. The model and the explicit-state checker live in
    // `commands/model_check.rs`; the formal narrative is in
    // `docs/src/security/model/capability_ipc.md`. Fails closed on any
    // reachable state or transition that violates an invariant.
    let opts = model_check::parse(args)?;
    noteln!("xtask: [model-check] exhaustive capability + IPC state machine");
    model_check::run(&opts)
}

fn run_bench(args: &[OsString]) -> Result<(), String> {
    // Host microbenchmarks for the per-pixel desktop paths, measured through
    // `lib/cpuops`'s existing harness. Timings are load-dependent, so this is
    // evidence for a completion report and deliberately not a `ci` gate; the
    // families and the budget live in `commands/bench.rs`.
    let opts = bench::parse(args)?;
    noteln!("xtask: [bench] raster and compositor families");
    bench::run(&opts)
}

fn run_spec_review(ctx: &Context) -> Result<(), String> {
    // fail closed if any unreviewed AI-drafted artefact marker
    // reaches the tree. The scanner lives in `commands/spec_review.rs`.
    noteln!("xtask: [spec-review] {}", ctx.workspace_root.display());
    spec_review::run(&ctx.workspace_root, &ctx.children)
}

fn run_ignore_check(ctx: &Context) -> Result<(), String> {
    noteln!("xtask: [ignore-check] {}", ctx.workspace_root.display());
    ignore_check::run(&ctx.workspace_root, &ctx.children)
}

fn run_source_text(ctx: &Context) -> Result<(), String> {
    noteln!("xtask: [source-text] {}", ctx.workspace_root.display());
    source_text::run(&ctx.workspace_root, &ctx.children)
}

fn run_charter_cite(ctx: &Context) -> Result<(), String> {
    // A comment carries the reason, not a pointer to the rule. The scanner
    // lives in `commands/charter_cite.rs`.
    noteln!("xtask: [charter-cite] {}", ctx.workspace_root.display());
    charter_cite::run(&ctx.workspace_root, &ctx.children)
}

/// Time one pipeline stage, reporting its wall clock in every step's
/// [`elapsed_line`] shape.
///
/// A stage that fans out into concurrent jobs reports only per-job lines, so
/// its own cost never reaches the log — and a stage whose cost cannot be
/// grepped cannot be ordered against the others on evidence. The `stage:`
/// prefix keeps these totals distinguishable from the per-command lines they
/// contain.
fn stage(label: &str, run: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let started = Instant::now();
    let outcome = run();
    crate::note(&elapsed_line(
        &format!("stage: {label}"),
        outcome.is_ok(),
        started.elapsed(),
    ));
    outcome
}

/// One stage of `ci`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CiStage {
    /// The first and cheapest gate. It streams `cargo fmt` output, so it runs
    /// alone rather than with the static gates.
    FmtCheck,
    /// [`STATIC_GATES`], run at once: none compiles, so their wall-clock costs
    /// overlap instead of summing, and they still gate every compiling stage.
    StaticGates,
    /// `cargo deny check` reads `Cargo.lock` and the advisory database and
    /// compiles nothing; it streams its output, so it runs alone.
    Deny,
    /// One iteration of each stateful capability model with a fresh logged
    /// seed. The wall-clock coverage is `proptest --soak`, outside `ci`.
    Proptest,
    /// `lib/crypto`'s tests under release optimisation, which can break
    /// constant time where the debug profile the matrix uses cannot.
    CryptoConstantTime,
    /// One iteration of each fuzz harness with a fresh logged seed. The
    /// wall-clock coverage is `fuzz --soak`, outside `ci`.
    Fuzz,
    /// The interleaving oracle: the matrix runs whichever ordering the host
    /// scheduler picked, and only the model checker covers the rest.
    Loom,
    /// Only a doc build, so a broken link or a denied rustdoc warning surfaces
    /// before any compile-heavy stage.
    DocsCheck,
    /// Every delivered image profile is built, so an image-breaking change
    /// cannot land green. It needs the pinned Pi firmware blobs, staged
    /// (`--firmware`, `$TAIRIX_PI_FIRMWARE`) or fetched by `curl`.
    Image,
    /// Every target, warnings denied.
    Clippy,
    /// The whole test matrix once, the host passes in a freshly seeded order so
    /// an order-dependent suite fails. The QEMU tests need QEMU for every
    /// Tier-1 target (`docs/src/platform/x86_64.md`), the wasm32 ones Node.js
    /// and the browser `tests/integration/package-lock.json` pins, and the
    /// device-tree tests the pinned Pi firmware, staged as the image stage
    /// stages it; the repetition lives in the soaks.
    Test,
    /// The undefined-behaviour oracle over the crates with a hand-written
    /// `unsafe` core. With the matrix it is the dearest stage, so it closes
    /// the pipeline.
    Miri,
}

/// Every stage `ci` runs, in order: each is cheaper than the next by measured
/// wall clock (`grep 'stage:'` over a run's log), so a failing PR fails as
/// early as its cost allows.
const CI_STAGES: [CiStage; 12] = [
    CiStage::FmtCheck,
    CiStage::StaticGates,
    CiStage::Deny,
    CiStage::Proptest,
    CiStage::CryptoConstantTime,
    CiStage::Fuzz,
    CiStage::Loom,
    CiStage::DocsCheck,
    CiStage::Image,
    CiStage::Clippy,
    CiStage::Test,
    CiStage::Miri,
];

impl CiStage {
    /// The label the stage reports under.
    const fn label(self) -> &'static str {
        match self {
            Self::FmtCheck => "fmt --check",
            Self::StaticGates => "static gates",
            Self::Deny => "deny",
            Self::Proptest => "proptest --once",
            Self::CryptoConstantTime => "crypto-constant-time",
            Self::Fuzz => "fuzz --once",
            Self::Loom => "loom",
            Self::DocsCheck => "docs-check",
            Self::Image => "image",
            Self::Clippy => "clippy",
            Self::Test => "test --qemu --wasm",
            Self::Miri => "miri",
        }
    }

    fn run(self, ctx: &Context) -> Result<(), String> {
        match self {
            Self::FmtCheck => run_fmt(ctx, &[]),
            Self::StaticGates => run_static_gates(ctx),
            Self::Deny => run_deny(ctx),
            Self::Proptest => run_proptest(ctx, &[OsString::from("--once")]),
            Self::CryptoConstantTime => run_crypto_constant_time(ctx),
            Self::Fuzz => run_fuzz(ctx, &[OsString::from("--once")]),
            Self::Loom => loom::run(ctx, &[]),
            Self::DocsCheck => run_docs_check(ctx, &[]),
            Self::Image => run_image_gate(ctx),
            Self::Clippy => run_clippy(ctx, &[]),
            Self::Test => run_test(ctx, &CI_TEST_ARGS.map(OsString::from)),
            Self::Miri => miri::run(ctx, &[]),
        }
    }

    /// Whether `ci-long` repeats the stage in its flake hunt: it executes test
    /// cases, whose result a repeat can change.
    const fn flake_hunted(self) -> bool {
        matches!(
            self,
            Self::Proptest | Self::CryptoConstantTime | Self::Fuzz | Self::Test
        )
    }
}

/// The `test` arguments `ci` passes: the QEMU and wasm32 matrices, in a fresh
/// order, and no scene tier.
const CI_TEST_ARGS: [&str; 3] = ["--qemu", "--wasm", "--shuffle"];

fn run_ci(ctx: &Context) -> Result<(), String> {
    // Every compiling stage signs or derives an anchor from the key set, so a
    // machine without one fails here rather than a stage in.
    ctx.signing_keys()?;
    for ci_stage in CI_STAGES {
        stage(ci_stage.label(), || ci_stage.run(ctx))?;
    }
    Ok(())
}

/// A gate [`STATIC_GATES`] lists.
type StaticGate = fn(&Context) -> Result<(), String>;

/// The deterministic, non-compiling gates [`CiStage::StaticGates`] runs at
/// once: each a read-only source or metadata scan or an in-xtask cross-check,
/// so they share no mutable state.
const STATIC_GATES: &[(&str, StaticGate)] = &[
    // A workspace dependency-graph walk and a source scan.
    ("deps-check", run_deps_check),
    ("cfg-check", run_cfg_check),
    // Generic code names no board, SoC or board part.
    ("board-check", run_board_check),
    // Every crate's ids from its registered range, none defined twice.
    ("event-ids", run_event_ids),
    // Every driver ends through the one exit vocabulary.
    ("driver-exits", run_driver_exits),
    // The command apps' discovered `Help/` trees (plans/APPS.md §8.1).
    ("help-lint", help_lint::run),
    // No unreviewed AI-drafted artefact marker reached the tree.
    ("spec-review", run_spec_review),
    // No test the gate builds is ignored, so every one of them runs.
    ("ignore-check", run_ignore_check),
    // No comment cites a charter section number in place of the reason.
    ("charter-cite", run_charter_cite),
    // No control byte in source text, which tools then misread as binary.
    ("source-text", run_source_text),
    // The source-hash allow-list against `Cargo.lock`, and the advisory SLA.
    ("supply-chain", |c| run_supply_chain(c, &[])),
    // The syscall table and the C header against the `lib/abi` compiled into
    // xtask itself.
    ("abi-check", |c| run_abi_check(c, &[])),
    ("c-header", |c| run_c_header(c, &[])),
    // The generated tables, each regenerated in-process from its pinned
    // source and compared byte for byte, with no build and no network.
    ("font-atlas", |c| run_font_atlas(c, &[])),
    ("devids", |c| devids::run(c, &[])),
    ("unicode", |c| unicode::run(c, &[])),
    ("spirv", |c| spirv::run(c, &[])),
    // The shipped figure art against its bounds and the committed ledger
    // (plans/FIGURE.md FG5).
    ("artsheet", |c| run_artsheet(c, &[])),
    // The file-type icon masters, each redrawn and compared byte for byte.
    ("icons", |c| icons::run(c, &[])),
    // Exhaustively model-check the capability + IPC state machine.
    ("model-check", |_| run_model_check(&[])),
];

/// Run [`STATIC_GATES`] concurrently, failing closed: every gate runs to
/// completion and each failure is named, so a non-conforming tree reports
/// every cheap failure together.
fn run_static_gates(ctx: &Context) -> Result<(), String> {
    let jobs: Vec<parallel::Job> = STATIC_GATES
        .iter()
        .map(|&(label, gate)| static_gate(label, ctx, gate))
        .collect();
    let budget = parallel::default_concurrency(jobs.len());
    parallel::run(&ctx.children, jobs, budget)
}

/// Wrap one deterministic gate as a concurrency [`parallel::Job`], handing the
/// worker its own owned [`Context`] so the closure is `'static`.
fn static_gate(label: &str, ctx: &Context, gate: StaticGate) -> parallel::Job {
    let ctx = ctx.clone();
    parallel::Job::closure(label.to_string(), 1, move || gate(&ctx))
}

/// One step of `ci-long`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LongStep {
    /// A `ci` stage whose result cannot change between runs, run once.
    Once(CiStage),
    /// The flake hunt over the stages [`CiStage::flake_hunted`] names.
    FlakeHunt,
}

/// `ci-long`'s steps: every `ci` stage the hunt does not repeat, once and in
/// `ci`'s order, then the hunt, the dearest step, last.
fn ci_long_plan() -> impl Iterator<Item = LongStep> {
    CI_STAGES
        .into_iter()
        .filter(|ci_stage| !ci_stage.flake_hunted())
        .map(LongStep::Once)
        .chain(std::iter::once(LongStep::FlakeHunt))
}

/// The long-runner flake hunt: `ci`'s stages, but every one that executes test
/// cases is run [`ci_long::REPS`] times sequentially and then
/// [`ci_long::REPS`] times concurrently, per test ([`ci_long_plan`]).
///
/// `--dry-run` prints the plan without running anything, so an operator can
/// gauge a run's shape before committing a long-runner to it.
fn run_ci_long(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    if let Some(bad) = args.iter().find(|a| *a != "--dry-run") {
        return Err(format!(
            "ci-long: unexpected argument {}; usage: cargo xtask ci-long [--dry-run]",
            bad.display()
        ));
    }
    if args.iter().any(|a| a == "--dry-run") {
        return ci_long::print_plan(ctx);
    }
    ctx.signing_keys()?;
    for step in ci_long_plan() {
        match step {
            LongStep::Once(ci_stage) => stage(ci_stage.label(), || ci_stage.run(ctx))?,
            LongStep::FlakeHunt => stage("flake hunt", || {
                // Every QEMU enrolment and wasm32 vertical is built, and the
                // firmware the host tests read staged, once, so the repeated
                // runs re-execute rather than rebuild.
                let firmware = test_firmware(ctx)?;
                qemu_tests::build_all(ctx, None)?;
                wasm_tests::prepare(ctx)?;
                ci_long::flake_hunt(
                    ci_long::all_units(ctx, ci_long::REPS, &firmware)?,
                    ci_long::REPS,
                    |jobs, budget| parallel::run(&ctx.children, jobs, budget),
                )
            })?,
        }
    }
    Ok(())
}

/// Build every delivered image profile as part of the `ci` gate. The image
/// platform set follows PLAN.md Stage 8: `aarch64-rpi` is the only platform
/// delivered so far; new platforms join this gate as they land.
fn run_image_gate(ctx: &Context) -> Result<(), String> {
    for profile in ["debug", "installer"] {
        noteln!("xtask: [ci] image aarch64-rpi profile {profile}");
        run_image(
            ctx,
            &[
                OsString::from("--target"),
                OsString::from("aarch64-rpi"),
                OsString::from("--profile"),
                OsString::from(profile),
            ],
        )?;
    }
    Ok(())
}

/// run `lib/crypto`'s unit tests under the release profile so the
/// constant-time comparison tests are exercised at `-C opt-level=3`. A
/// data-dependent branch introduced by the optimiser would surface here, in
/// the `constant_time` module's full-traversal assertions, rather than at
/// the debug optimisation level the main test phase uses.
fn run_crypto_constant_time(ctx: &Context) -> Result<(), String> {
    parallel::run(&ctx.children, vec![crypto_constant_time_job(ctx)], 1)
}

/// The release-profile crypto tests, held to running every one: the job
/// `ci-long`'s hunt repeats as well.
fn crypto_constant_time_job(ctx: &Context) -> parallel::Job {
    let mut cmd = ctx.cargo();
    cmd.args(["test", "--release", "--locked", "-p", "tairix-crypto"]);
    parallel::Job::new("crypto-constant-time (release)", cmd)
        .accounted(libtest::Account::Everything)
}

/// The workspace host test pass, which `ci-long`'s hunt repeats.
fn host_test_command(ctx: &Context) -> std::process::Command {
    let mut cmd = ctx.cargo();
    cmd.args(["test", "--workspace", "--all-targets", "--locked"]);
    cmd
}

/// The workspace's doctests, which `--all-targets` leaves out and cargo will
/// not run beside it; `ci-long`'s hunt repeats them too.
fn host_doctest_command(ctx: &Context) -> std::process::Command {
    let mut cmd = ctx.cargo();
    cmd.args(["test", "--workspace", "--doc", "--locked"]);
    cmd
}

fn run_deny(ctx: &Context) -> Result<(), String> {
    cargo_subcommand_available(ctx, "deny").map_err(|why| {
        format!("{why}; install cargo-deny with `cargo install cargo-deny --locked`")
    })?;
    let mut cmd = ctx.cargo();
    cmd.args(["deny", "--all-features", "check"]);
    ctx.run("deny", cmd)
}

/// Which stage stages the pinned Pi firmware, for the messages it fails with.
#[derive(Clone, Copy)]
enum FirmwareFor {
    Image,
    Tests,
}

impl FirmwareFor {
    const fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Tests => "test",
        }
    }

    /// How an operator stages the blobs for this stage instead.
    fn remedy(self) -> String {
        match self {
            Self::Image => format!("pass --firmware <dir> (or set ${PI_FIRMWARE_ENV})"),
            Self::Tests => format!("set ${PI_FIRMWARE_ENV} to their directory"),
        }
    }
}

/// The build cache the pinned Pi firmware is fetched into.
fn firmware_cache(ctx: &Context) -> PathBuf {
    ctx.target_dir().join(PI_FIRMWARE_CACHE)
}

/// The directory holding the pinned Pi firmware, verified against
/// `tools/mkimage/firmware.lock`: the operator's (`--firmware`, or
/// [`PI_FIRMWARE_ENV`], the hands-free form for air-gapped and pre-staged
/// builds) as it stands, else the build cache, with whatever it lacks fetched
/// first.
fn staged_pi_firmware(
    ctx: &Context,
    operator: Option<PathBuf>,
    stage: FirmwareFor,
) -> Result<(PathBuf, Vec<tairix_mkimage::firmware::FirmwareFile>), String> {
    let label = stage.label();
    let manifest_path = ctx
        .workspace_root
        .join("tools")
        .join("mkimage")
        .join("firmware.lock");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{label}: cannot read {}: {e}", manifest_path.display()))?;
    let manifest = tairix_mkimage::firmware::FirmwareManifest::parse(&manifest_text)
        .map_err(|e| format!("{label}: {e}"))?;
    let dir = if let Some(dir) = operator {
        dir
    } else {
        let cache = firmware_cache(ctx);
        fetch_missing_firmware(ctx, &manifest, &cache, stage)?;
        cache
    };
    let firmware = manifest
        .load_dir(&dir)
        .map_err(|e| format!("{label}: {e}"))?;
    Ok((dir, firmware))
}

/// The directory `$TAIRIX_PI_FIRMWARE` names, made absolute against the
/// working directory, since each test runs in its own package's.
fn operator_firmware() -> Result<Option<PathBuf>, String> {
    std::env::var_os(PI_FIRMWARE_ENV)
        .map(|dir| {
            std::path::absolute(&dir).map_err(|e| {
                format!(
                    "test: ${PI_FIRMWARE_ENV} {}: {e}",
                    Path::new(&dir).display()
                )
            })
        })
        .transpose()
}

/// Where the pinned firmware the host tests check discovery against is: the
/// operator's directory, else the build cache. Naming it stages nothing, so a
/// run's plan can be shown without fetching.
fn test_firmware_dir(ctx: &Context) -> Result<PathBuf, String> {
    Ok(operator_firmware()?.unwrap_or_else(|| firmware_cache(ctx)))
}

/// [`test_firmware_dir`], staged once for a run to name to each pass, so a
/// test handed it cannot pass having read nothing.
fn test_firmware(ctx: &Context) -> Result<PathBuf, String> {
    staged_pi_firmware(ctx, operator_firmware()?, FirmwareFor::Tests).map(|(dir, _)| dir)
}

/// Parsed `image` arguments. The flags mirror `tools/mkimage`'s CLI so the
/// two entry points stay interchangeable.
struct ImageArgs {
    /// Operator-staged firmware directory (`--firmware` /
    /// `$TAIRIX_PI_FIRMWARE`); `None` means fetch into the build cache.
    firmware_dir: Option<PathBuf>,
    kind: tairix_mkimage::ImageKind,
    out: Option<PathBuf>,
}

/// Parse `image` arguments: `--target <name>` (required; only
/// `aarch64-rpi` exists today), `--firmware <dir>` (or
/// `$TAIRIX_PI_FIRMWARE`; optional — missing blobs are fetched from the
/// manifest's pinned source otherwise), `--profile debug|installer`
/// (default `debug` — the development image seeds the test `root` account;
/// the installer image seeds none), `--encrypt-root` (a debug image's only:
/// its root encrypted under `tairix_mkimage::DEBUG_PASSPHRASE`, the root
/// otherwise plaintext), `--out <path>`, and `--headless`.
fn parse_image_args(args: &[OsString]) -> Result<ImageArgs, String> {
    let mut target: Option<String> = None;
    let mut firmware_dir: Option<PathBuf> = None;
    let mut profile: Option<tairix_mkimage::ImageProfile> = None;
    let mut root = tairix_mkimage::VolumeProtection::Plaintext;
    let mut out: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let Some(flag) = flag.to_str() else {
            return Err("image: arguments must be valid UTF-8".to_string());
        };
        match flag {
            "--target" => {
                target = Some(
                    it.next()
                        .and_then(|v| v.to_str().map(str::to_owned))
                        .ok_or("image: --target requires a value")?,
                );
            }
            "--firmware" => {
                firmware_dir = Some(PathBuf::from(
                    it.next().ok_or("image: --firmware requires a value")?,
                ));
            }
            "--profile" => {
                let name = it
                    .next()
                    .and_then(|v| v.to_str())
                    .ok_or("image: --profile requires a value")?;
                profile = Some(tairix_mkimage::ImageProfile::from_label(name).ok_or_else(
                    || format!("image: unknown profile {name:?}; expected `debug` or `installer`"),
                )?);
            }
            "--out" => {
                out = Some(PathBuf::from(
                    it.next().ok_or("image: --out requires a value")?,
                ));
            }
            "--encrypt-root" => root = tairix_mkimage::VolumeProtection::Encrypted,
            // The image carries the kernel and the root skeleton only; the
            // desktop ships as installable userland later, so the headless
            // image is byte-identical today. Accepted so the headless
            // invocation works unchanged once the contents diverge.
            "--headless" => {}
            other => return Err(format!("image: unknown argument {other}")),
        }
    }
    let target = target.ok_or("image: --target <platform> is required (aarch64-rpi)")?;
    if target != "aarch64-rpi" {
        return Err(format!(
            "image: unsupported target {target}; `aarch64-rpi` is the only \
             image platform delivered so far (PLAN.md Stage 8 / plans/PI.md P9)"
        ));
    }
    let firmware_dir =
        firmware_dir.or_else(|| std::env::var_os(PI_FIRMWARE_ENV).map(PathBuf::from));
    let profile = profile.unwrap_or(tairix_mkimage::ImageProfile::Debug);
    let kind = tairix_mkimage::ImageKind::new(profile, root)
        .map_err(|refused| format!("image: --encrypt-root: {refused}"))?;
    Ok(ImageArgs {
        firmware_dir,
        kind,
        out,
    })
}

/// Fetch every pinned firmware blob missing from `cache` from the
/// manifest's pinned HTTPS source, then prove the cache complete.
///
/// The blobs are third-party build inputs: each download
/// lands beside the cache as `<name>.part` and is renamed in only after
/// `missing_in` — the same pinned size + SHA-256 check `load_dir` applies —
/// stops reporting it. A blob that still mismatches after its fetch is
/// deleted and the build fails closed.
fn fetch_missing_firmware(
    ctx: &Context,
    manifest: &tairix_mkimage::firmware::FirmwareManifest,
    cache: &Path,
    stage: FirmwareFor,
) -> Result<(), String> {
    let missing = manifest.missing_in(cache);
    if missing.is_empty() {
        return Ok(());
    }
    let label = stage.label();
    std::fs::create_dir_all(cache)
        .map_err(|e| format!("{label}: cannot create {}: {e}", cache.display()))?;
    for entry in &missing {
        let url = format!("{}/{}", manifest.source(), entry.name);
        let dest = cache.join(&entry.name);
        // A nested pin (e.g. `overlays/disable-bt.dtbo`) lands in a cache
        // subdirectory that must exist before curl writes the `.part`.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{label}: cannot create {}: {e}", parent.display()))?;
        }
        let part = cache.join(format!("{}.part", entry.name));
        let mut curl = std::process::Command::new("curl");
        curl.args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--proto", "=https", "--max-redirs", "4"])
            .arg("--output")
            .arg(&part)
            .arg(&url);
        ctx.run(
            &format!("{label}: fetching pinned firmware blob {url}"),
            curl,
        )
        .map_err(|why| {
            let _ = std::fs::remove_file(&part);
            format!(
                "{why}; to run without fetching, stage the blobs per \
                     tools/mkimage/firmware.lock and {}",
                stage.remedy()
            )
        })?;
        std::fs::rename(&part, cache.join(&entry.name))
            .map_err(|e| format!("{label}: cannot stage {}: {e}", entry.name))?;
    }
    let still_missing = manifest.missing_in(cache);
    if !still_missing.is_empty() {
        for entry in &still_missing {
            let _ = std::fs::remove_file(cache.join(&entry.name));
        }
        let names: Vec<&str> = still_missing.iter().map(|e| e.name.as_str()).collect();
        return Err(format!(
            "{label}: fetched firmware failed the pinned checksum gate \
             (tools/mkimage/firmware.lock): {}",
            names.join(", ")
        ));
    }
    Ok(())
}

/// The Cargo profile an image's kernel and user-space programs compile in.
///
/// The kernel cannot read `cfg!(debug_assertions)` from the image it is
/// planted in, so its boot-log routing (`kernel/arch/aarch64/src/serial.rs`:
/// a debug build logs to the UART, a release build to the screen) is right
/// only when each image builds in the matching profile: the non-shippable
/// `debug` image in `dev`, the shippable `installer` image `--release`.
pub(crate) const fn cargo_profile(profile: tairix_mkimage::ImageProfile) -> Profile {
    match profile {
        tairix_mkimage::ImageProfile::Debug => Profile::Dev,
        tairix_mkimage::ImageProfile::Installer => Profile::Release,
    }
}

/// The kernel features that make up the debug image's diagnostics: the
/// lockup-watchdog aids (`watchdog-diagnostics`, `plans/WATCHDOG.md`).
pub(crate) const KERNEL_DIAGNOSTICS_FEATURES: &str = "watchdog-diagnostics";

/// The package the kernel image is built from, where its diagnostics are
/// switched on.
pub(crate) const KERNEL_PACKAGE: &str = "tairix-kernel";

/// The feature compiling a package's long host tests of whole ray-traced
/// scenes or whole realms, whose cost grows with every detail they gain.
pub(crate) const SCENE_TESTS_FEATURE: &str = "scene-tests";

/// The extra `cargo` arguments that turn [`KERNEL_DIAGNOSTICS_FEATURES`] on
/// for the **non-shippable** `debug` image and leave them fully compiled out
/// of the shippable `installer` image.
///
/// This is the single selection point for the gate, paired with
/// [`cargo_profile`] so the diagnostics track the same image profile
/// the console UART routing already keys on: the `debug` image (a
/// `debug_assertions`-on kernel that diverts the log stream to the UART)
/// also compiles in the address-bearing developer aids, and the `installer`
/// image (optimised, screen console) pays nothing for them — no hot-path
/// breadcrumb atomics on the syscall/dispatch/fault paths, no stack-walk
/// code, no address strings. The gate is a feature, not `debug_assertions`,
/// so CI can build and test both states deterministically.
fn kernel_diag_feature_args(profile: tairix_mkimage::ImageProfile) -> &'static [&'static str] {
    match profile {
        tairix_mkimage::ImageProfile::Debug => &["--features", KERNEL_DIAGNOSTICS_FEATURES],
        tairix_mkimage::ImageProfile::Installer => &[],
    }
}

fn run_image(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let parsed = parse_image_args(args)?;
    build_platform_image(ctx, parsed).map(|_| ())
}

/// Signed driver bundles paired with their `/System/Drivers/` store paths.
type DriverBundles = Vec<(&'static [&'static [u8]], Vec<u8>)>;

/// Cross-compiles and signs one driver's bundle for an image profile.
type DriverBundleBuilder =
    fn(&Context, PieArch, tairix_mkimage::ImageProfile) -> Result<Vec<u8>, String>;

/// Every `/System/Drivers/` store path the platform image ships, paired with
/// the builder that cross-compiles and signs its bundle.
///
/// They all run in user space (the floor stays storage-only), so `devmgr`
/// autoloads each against its discovered node — and the bus chain is
/// recursive: the PCIe root-complex
/// driver binds the discovered `brcm,bcm2711-pcie` node and emits the
/// VL805 PCI function; the VL805 driver binds that, reloads the controller
/// firmware over the mailbox, and emits the `usb,xhci` node; the xHCI
/// **host-controller driver** binds that, enumerates the device, and emits
/// one per-interface node; the keyboard **class** driver binds that and
/// pumps key edges into the input arbiter over the URB transport
/// (`plans/USB.md` U3b/U4); the mass-storage **class** driver binds a
/// mass-storage interface node the same way and serves each logical unit
/// as a block-service endpoint behind a per-LUN storage node
/// (`plans/DEVICES.md` D2). The GENET NIC needs no bus chain: it hangs
/// straight off the platform bus, so its driver binds the discovered
/// `brcm,bcm2711-genet-v5` node and hands `netstack` its frame channel. The
/// virtio-net bundle rides alongside it for the same reason the virtio
/// keyboard does: an emulated or virtualised boot of this image
/// (`cargo xtask run`) presents a virtio-net NIC rather than a GENET, and
/// only one of the two is ever discovered, so whichever it is binds and the
/// other bundle stays unbound. The Pi RTC hangs off no bus at all — the
/// firmware owns it — so its driver binds the discovered `raspberrypi,rpi-rtc` node and reaches the chip
/// through the mailbox service; on a Pi 3 or Pi 4 there is no such node and
/// the bundle simply stays unbound. The firmware-clocks driver hangs off no
/// bus either: it binds the discovered `raspberrypi,firmware-clocks` node,
/// takes the kernel's frequency mechanism role for the ARM clock, and serves
/// the display's clocks, all over the same mailbox service. The native
/// display engine binds the HVS node, which carries the boot console's
/// surface, and drives each HDMI block through the transmitter driver bound
/// to it, over `encoder-v1`; an emulated boot's display binds the generic
/// framebuffer service on a `ramfb` surface, or the virtio-gpu driver on a
/// virtio GPU. The Broadcom DMA
/// driver binds the discovered `brcm,bcm2835-dma` and `brcm,bcm2711-dma` nodes
/// and serves their
/// channels to the peripheral drivers whose nodes name request lines on it,
/// as the clock manager driver serves the audio blocks' clocks to theirs and
/// the DVP driver the HDMI blocks' audio-clock gates, and the DDC controllers'
/// driver serves the bus each HDMI port reads its sink's EDID over; the
/// headphone jack's and
/// the I²S interface's drivers are the audio blocks' consumers, and
/// a USB audio device behind the bus chain gets the USB Audio Class driver.
/// Both codec drivers ship, so an I²S HAT carrying either part works with no
/// rebuild. The GPIO controller's driver binds the discovered GPIO block and
/// lends its header's lines to the cooling service, and the AVS sensor's
/// driver serves the processor temperature it switches the fan by.
const PLATFORM_IMAGE_DRIVER_STORE: &[(&[&[u8]], DriverBundleBuilder)] = &[
    (
        image_drivers::VCMAILBOX_STORE_PATH,
        image_drivers::build_vcmailbox_bundle,
    ),
    (
        image_drivers::PCIE_BRCM_STORE_PATH,
        image_drivers::build_pcie_brcm_bundle,
    ),
    (
        image_drivers::VL805_STORE_PATH,
        image_drivers::build_vl805_bundle,
    ),
    (
        image_drivers::USB_XHCI_STORE_PATH,
        image_drivers::build_xhci_bundle,
    ),
    (
        image_drivers::USB_HID_STORE_PATH,
        image_drivers::build_usb_hid_bundle,
    ),
    (
        image_drivers::VIRTIO_KBD_STORE_PATH,
        image_drivers::build_virtio_kbd_bundle,
    ),
    (
        image_drivers::GENET_STORE_PATH,
        image_drivers::build_genet_bundle,
    ),
    (
        image_drivers::VIRTIO_NET_STORE_PATH,
        image_drivers::build_virtio_net_bundle,
    ),
    (
        image_drivers::FRAMEBUFFER_STORE_PATH,
        image_drivers::build_framebuffer_bundle,
    ),
    (
        image_drivers::VIRTIO_GPU_STORE_PATH,
        image_drivers::build_virtio_gpu_bundle,
    ),
    (
        image_drivers::BCM2711_HVS_STORE_PATH,
        image_drivers::build_bcm2711_hvs_bundle,
    ),
    (
        image_drivers::BCM2711_HDMI_STORE_PATH,
        image_drivers::build_bcm2711_hdmi_bundle,
    ),
    (
        image_drivers::RPI_RTC_STORE_PATH,
        image_drivers::build_rpi_rtc_bundle,
    ),
    (
        image_drivers::RPI_FIRMWARE_CLOCKS_STORE_PATH,
        image_drivers::build_rpi_firmware_clocks_bundle,
    ),
    (
        image_drivers::I2C_BCM2835_STORE_PATH,
        image_drivers::build_i2c_bcm2835_bundle,
    ),
    (
        image_drivers::I2C_BRCMSTB_STORE_PATH,
        image_drivers::build_i2c_brcmstb_bundle,
    ),
    (
        image_drivers::DMA_BCM2835_STORE_PATH,
        image_drivers::build_dma_bcm2835_bundle,
    ),
    (
        image_drivers::IRQ_BRCMSTB_L2_STORE_PATH,
        image_drivers::build_irq_brcmstb_l2_bundle,
    ),
    (
        image_drivers::GPIO_BCM2835_STORE_PATH,
        image_drivers::build_gpio_bcm2835_bundle,
    ),
    (
        image_drivers::SENSOR_BCM2711_THERMAL_STORE_PATH,
        image_drivers::build_sensor_bcm2711_thermal_bundle,
    ),
    (
        image_drivers::CPRMAN_BCM2711_STORE_PATH,
        image_drivers::build_cprman_bcm2711_bundle,
    ),
    (
        image_drivers::DVP_BCM2711_STORE_PATH,
        image_drivers::build_dvp_bcm2711_bundle,
    ),
    (
        image_drivers::PWM_AUDIO_STORE_PATH,
        image_drivers::build_pwm_audio_bundle,
    ),
    (
        image_drivers::I2S_AUDIO_STORE_PATH,
        image_drivers::build_i2s_audio_bundle,
    ),
    (
        image_drivers::USB_UAC_STORE_PATH,
        image_drivers::build_usb_uac_bundle,
    ),
    (
        image_drivers::PCM5102A_STORE_PATH,
        image_drivers::build_pcm5102a_bundle,
    ),
    (
        image_drivers::PCM5122_STORE_PATH,
        image_drivers::build_pcm5122_bundle,
    ),
    (
        image_drivers::DS3231_STORE_PATH,
        image_drivers::build_ds3231_bundle,
    ),
    (
        image_drivers::PCF8523_STORE_PATH,
        image_drivers::build_pcf8523_bundle,
    ),
    (
        image_drivers::PCF85063A_STORE_PATH,
        image_drivers::build_pcf85063a_bundle,
    ),
    (
        image_drivers::USB_MSD_STORE_PATH,
        image_drivers::build_usb_msd_bundle,
    ),
    (
        image_drivers::VOLMGR_STORE_PATH,
        image_drivers::build_volmgr_bundle,
    ),
    (
        image_drivers::RAID_MEMBER_STORE_PATH,
        image_drivers::build_raid_member_bundle,
    ),
    (
        image_drivers::RAID_STORE_PATH,
        image_drivers::build_raid_bundle,
    ),
];

/// Build every bundle in [`PLATFORM_IMAGE_DRIVER_STORE`] and every served
/// filesystem driver, a volume on removable media being served by its
/// format's, each paired with its store path, or fail on the first that does
/// not.
fn build_image_driver_bundles(
    ctx: &Context,
    profile: tairix_mkimage::ImageProfile,
) -> Result<DriverBundles, String> {
    // The flashable Raspberry Pi image is an aarch64 target, so every bundle
    // is cross-compiled for that arch.
    let arch = PieArch::Aarch64;
    PLATFORM_IMAGE_DRIVER_STORE
        .iter()
        .map(|(path, build)| build(ctx, arch, profile).map(|bytes| (*path, bytes)))
        .chain(image_drivers::SERVED_FS_DRIVERS.iter().map(|driver| {
            image_drivers::build_served_fs_bundle(ctx, arch, driver, profile)
                .map(|bytes| (driver.store_path, bytes))
        }))
        .collect()
}

/// Build the platform image and return the written whole-disk image's
/// path (consumed by `run` to boot the image it just built).
/// Write an encrypted root's volume key beside the image at `out`, or take an
/// earlier build's away from a plaintext one, answering what the build
/// reports of its root.
fn place_root_key(
    out: &Path,
    unlock: Option<&tairix_mkimage::RootUnlock>,
) -> Result<String, String> {
    let key_out = out.with_extension("rootkey");
    if let Some(unlock) = unlock {
        tairix_mkimage::write_root_key(&key_out, unlock)
            .map_err(|e| format!("image: cannot write {}: {e}", key_out.display()))?;
        return Ok(format!("root volume key: {}", key_out.display()));
    }
    let stale = tairix_mkimage::remove_stale_root_key(&key_out)
        .map_err(|e| format!("image: cannot remove {}: {e}", key_out.display()))?;
    Ok(if stale {
        "root volume plaintext; removed the earlier build's root-key file".to_owned()
    } else {
        "root volume plaintext".to_owned()
    })
}

fn build_platform_image(ctx: &Context, args: ImageArgs) -> Result<PathBuf, String> {
    let ImageArgs {
        firmware_dir,
        kind,
        out,
    } = args;
    let profile = kind.profile();

    // 1. Build the freestanding aarch64 production kernel (PI.md P1) in
    //    the Cargo profile that matches the image profile (see
    //    `cargo_profile`): the `debug` image gets a
    //    `debug_assertions`-on kernel so the console diverts the boot-log
    //    stream to the UART, the shippable `installer` image gets an
    //    optimised `--release` kernel that renders the log on screen.
    let (build_profile_args, kernel_profile_dir) =
        (cargo_profile(profile).args(), cargo_profile(profile).dir());
    // The flashable image is the universal ARM media, so its kernel builds
    // against that image's CPU floor. Injecting the floor's `rustflags` via
    // `CARGO_ENCODED_RUSTFLAGS` *replaces* the shared `.cargo/config.toml`
    // block, so the floor carries the base flags too (`CpuFloor::rustflags`);
    // a baseline floor reproduces the config byte-for-byte.
    let floor = crate::floor::floor_for_image(crate::floor::ImageKind::AArch64Generic);
    let mut cmd = ctx.cargo();
    cmd.arg("build").arg("--locked");
    cmd.args(build_profile_args);
    cmd.args(["-p", KERNEL_PACKAGE, "--target", "aarch64-unknown-none"]);
    cmd.args(kernel_diag_feature_args(profile));
    cmd.env("CARGO_ENCODED_RUSTFLAGS", floor.encoded_rustflags());
    // A bare-metal kernel build from a clean `target/` can legitimately
    // outrun an incremental host compile pass; see `LONG_BUILD_COMMAND_TIMEOUT`.
    ctx.run_with_timeout(
        &format!("image: kernel build (aarch64-unknown-none, {kernel_profile_dir})"),
        cmd,
        LONG_BUILD_COMMAND_TIMEOUT,
    )?;

    // 2. Resolve the pinned firmware inputs, then assemble the image.
    let (_, firmware) = staged_pi_firmware(ctx, firmware_dir, FirmwareFor::Image)?;

    let kernel_path = ctx
        .target_dir()
        .join("aarch64-unknown-none")
        .join(kernel_profile_dir)
        .join("tairix-kernel");
    let kernel_elf = std::fs::read(&kernel_path).map_err(|e| {
        format!(
            "image: cannot read kernel ELF {}: {e}",
            kernel_path.display()
        )
    })?;

    // Cross-compile and sign the autoloaded `/System/Drivers/` bundles the
    // image ships, then install them into the read-only `/System` store.
    let bundles = build_image_driver_bundles(ctx, profile)?;
    let drivers: Vec<(&[&[u8]], &[u8])> = bundles
        .iter()
        .map(|(path, bytes)| (*path, bytes.as_slice()))
        .collect();

    // Compose the self-contained application bundles the read-only system
    // app/service stores ship — every discovered program's signed `AppInfo`
    // + `Run` planted beside its `Help/` tree (`plans/APPS.md` deliverable
    // 8). Discovery walks the userland `AppInfo.toml` sources; no per-bundle
    // list exists here.
    let apps = image_apps::app_store_files(ctx, PieArch::Aarch64, profile)?;

    let built = image_apps::with_plant_refs(apps, |app_files| {
        tairix_mkimage::build_rpi_image(
            &kernel_elf,
            &firmware,
            &mut tairix_mkimage::HostEntropy,
            kind,
            &drivers,
            app_files,
            &image_drivers::platform_network_conf(),
        )
    })
    .map_err(|e| format!("image: {e}"))?;

    // 3. Write the image, and an encrypted root's volume key, under
    //    `images/` (built images are output, never committed).
    let out = out.unwrap_or_else(|| {
        ctx.workspace_root
            .join("images")
            .join(format!("tairix-aarch64-rpi-{}.img", profile.label()))
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("image: cannot create {}: {e}", parent.display()))?;
    }
    tairix_mkimage::replace_file(&out, &built.image)
        .map_err(|e| format!("image: cannot write {}: {e}", out.display()))?;
    let root = place_root_key(&out, built.unlock.as_ref())?;
    noteln!(
        "xtask: [image] wrote {} profile {} ({} bytes); {root}",
        out.display(),
        profile.label(),
        built.image.len(),
    );
    Ok(out)
}

/// Default emulated CPU count for an interactive `run` session — enough
/// to exercise the SMP scheduler while staying cheap under TCG.
const DEFAULT_RUN_CPUS: u32 = 4;

/// The machine an interactive session presents: its CPUs and, where `--ram`
/// sized it, its RAM.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct RunMachine {
    cpus: u32,
    /// Guest RAM in mebibytes; `None` keeps the board's default.
    ram_mib: Option<u32>,
}

/// Split `run` arguments into the runner's own `--cpus <n>` and
/// `--ram <size>` and the argument tail forwarded to the `image` grammar
/// (`--target`, `--profile`, `--firmware`, `--out`), so the image-building
/// half of `run` and the `image` subcommand can never drift apart.
fn parse_run_args(args: &[OsString]) -> Result<(RunMachine, Vec<OsString>), String> {
    let mut machine = RunMachine {
        cpus: DEFAULT_RUN_CPUS,
        ram_mib: None,
    };
    let mut image_args = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--cpus" {
            machine.cpus = it
                .next()
                .and_then(|v| v.to_str())
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|&n| n >= 1)
                .ok_or("run: --cpus requires a positive integer")?;
        } else if flag == "--ram" {
            let size = it
                .next()
                .and_then(|v| v.to_str())
                .ok_or("run: --ram requires a size such as 512M or 2G")?;
            machine.ram_mib = Some(parse_ram(size)?);
        } else {
            image_args.push(flag.clone());
        }
    }
    Ok((machine, image_args))
}

/// `--ram`'s `size` in mebibytes: a whole number of mebibytes, bare or
/// suffixed `M`, or of gibibytes suffixed `G`, as QEMU's `-m` spells them.
fn parse_ram(size: &str) -> Result<u32, String> {
    let (count, scale) = match size.as_bytes().last() {
        Some(b'M' | b'm') => (&size[..size.len() - 1], 1),
        Some(b'G' | b'g') => (&size[..size.len() - 1], 1024),
        _ => (size, 1),
    };
    count
        .parse::<u32>()
        .ok()
        .and_then(|count| count.checked_mul(scale))
        .filter(|&mib| mib >= 1)
        .ok_or_else(|| format!("run: --ram takes a whole size such as 512M or 2G, not `{size}`"))
}

/// Build the QEMU-`virt`-board form of the production kernel for
/// `profile` as the arm64 `Image` the interactive session loads.
///
/// The Pi-linked kernel inside the platform image loads at `0x20_0000`,
/// which is not RAM on the `virt` board, and QEMU's ELF `-kernel` path
/// passes no DTB — so `run` builds the same production crate against
/// the `virt` linker script (`TAIRIX_KERNEL_BOARD=virt`, its own target
/// directory so the Pi build stays cached) and boots its flat form, an
/// `Image` by the header its trampoline opens with, which QEMU loads at the
/// `virt` link address with the generated device tree in `x0`.
fn build_virt_run_kernel(
    ctx: &Context,
    profile: tairix_mkimage::ImageProfile,
) -> Result<PathBuf, String> {
    let (build_profile_args, kernel_profile_dir) =
        (cargo_profile(profile).args(), cargo_profile(profile).dir());
    let target_dir = ctx.target_dir().join("virt-kernel");
    // The `run` development kernel targets the QEMU `virt` board (not shipped
    // hardware); it builds against that image's CPU floor, injected the same
    // way as the flashable image's kernel so both go through one definition.
    let floor = crate::floor::floor_for_image(crate::floor::ImageKind::AArch64Virt);
    let mut cmd = ctx.cargo();
    cmd.arg("build").arg("--locked");
    cmd.args(build_profile_args);
    cmd.args([
        "-p",
        "tairix-kernel",
        "--target",
        "aarch64-unknown-none",
        "--target-dir",
    ]);
    cmd.arg(&target_dir);
    cmd.args(kernel_diag_feature_args(profile));
    cmd.env("TAIRIX_KERNEL_BOARD", "virt");
    cmd.env("CARGO_ENCODED_RUSTFLAGS", floor.encoded_rustflags());
    // Same clean-rebuild cost as the flashable image's kernel build above.
    ctx.run_with_timeout(
        &format!("run: virt kernel build (aarch64-unknown-none, {kernel_profile_dir})"),
        cmd,
        LONG_BUILD_COMMAND_TIMEOUT,
    )?;
    let elf_path = target_dir
        .join("aarch64-unknown-none")
        .join(kernel_profile_dir)
        .join("tairix-kernel");
    let elf = std::fs::read(&elf_path).map_err(|e| {
        format!(
            "run: cannot read virt kernel ELF {}: {e}",
            elf_path.display()
        )
    })?;
    let boot_image =
        tairix_mkimage::elfflat::elf_to_flat(&elf, tairix_mkimage::elfflat::VIRT_KERNEL_LOAD_ADDR)
            .map_err(|e| format!("run: {e}"))?;
    let out = target_dir.join(format!("tairix-kernel-virt-{}.img", profile.label()));
    tairix_mkimage::replace_file(&out, &boot_image)
        .map_err(|e| format!("run: cannot write {}: {e}", out.display()))?;
    Ok(out)
}

/// The QEMU machine an interactive session presents: the image as the
/// virtio-blk root disk, a `ramfb` scan-out surface for the windowed
/// display, and a virtio-net NIC on QEMU's user-mode network pinned to
/// the MAC the image's shipped `network.conf` binds its `vwan` interface
/// by ([`image_drivers::VIRT_SESSION_NIC_MAC`]).
///
/// Pure, so the shape a human's session gets is a checkable contract
/// rather than something only a windowed run reveals.
fn run_session_spec(kernel: &Path, disk_image: &Path, machine: RunMachine) -> tairix_qemu::Spec {
    let spec = tairix_qemu::Spec::for_aarch64_kernel(kernel)
        .with_cpus(machine.cpus)
        .with_virtio_blk(disk_image)
        .with_display(tairix_qemu::Display::Ramfb)
        .with_virtio_net_user(image_drivers::VIRT_SESSION_NIC_MAC)
        .windowed_interactive();
    match machine.ram_mib {
        Some(ram_mib) => spec.with_ram_mib(ram_mib),
        None => spec,
    }
}

/// Build the platform image for the requested profile, then boot it as
/// an **interactive** QEMU `virt` session: a windowed display driven by
/// the kernel's ramfb boot console, virtio keyboard + mouse devices for
/// input from the window, a virtio-net NIC on QEMU's user-mode network,
/// and the image attached as the virtio-blk root disk (the same
/// root mount → store-scan → driver-autoload chain the
/// `-M virt` verticals prove). The kernel itself boots as the
/// `virt`-board build of the same production crate
/// ([`build_virt_run_kernel`]), so QEMU hands it the real runtime
/// device tree.
///
/// The NIC's MAC is pinned to [`image_drivers::VIRT_SESSION_NIC_MAC`],
/// which is the identity the image's own shipped `network.conf` binds its
/// `vwan` interface by — so the autoloaded virtio-net driver's device is
/// the one `netstack` was configured to address, and the session leases an
/// address from QEMU's own DHCP server and reaches the outside world
/// through its NAT. The board's GENET interface in that same document
/// stays unbound here, as this NIC does on real hardware.
///
/// The invoking terminal is the guest's serial console, where an
/// `--encrypt-root` image's passphrase is typed. The session runs in the foreground
/// with no deadline and ends when the user closes the QEMU window or
/// the guest powers off.
fn run_run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let (machine, image_args) = parse_run_args(args)?;
    let parsed = parse_image_args(&image_args)?;
    let kind = parsed.kind;
    let disk_image = build_platform_image(ctx, parsed)?;
    let virt_kernel = build_virt_run_kernel(ctx, kind.profile())?;
    let spec = run_session_spec(&virt_kernel, &disk_image, machine);
    let unlock = match kind.root() {
        tairix_mkimage::VolumeProtection::Plaintext => {
            "the root mounts with no passphrase".to_owned()
        }
        tairix_mkimage::VolumeProtection::Encrypted => format!(
            "the root's passphrase is `{}`",
            String::from_utf8_lossy(tairix_mkimage::DEBUG_PASSPHRASE)
        ),
    };
    noteln!(
        "xtask: [run] booting {} ({}) on qemu-system-aarch64 -M virt with {} \
         CPUs and {} MiB of RAM; this terminal is the guest serial console \
         ({unlock})",
        disk_image.display(),
        kind.profile().label(),
        spec.cpus,
        spec.ram_mib(),
    );
    let status = tairix_qemu::Runner::run_interactive(&spec, &*ctx.children)
        .map_err(|e| format!("run: {e}"))?;
    if status != 0 {
        return Err(format!("run: QEMU exited with status {status}"));
    }
    Ok(())
}

/// Probe for `program` on `PATH` through its `--version`, answering why it
/// is not there where it is not.
pub(crate) fn probe_version(ctx: &Context, program: &str) -> Result<(), String> {
    let mut probe = std::process::Command::new(program);
    probe.arg("--version");
    output_or_reason(
        &ctx.children,
        &format!("{program} --version"),
        &mut probe,
        ctx.children.ordinary_budget(),
    )
    .map(drop)
}

/// Probe for a cargo subcommand (`cargo <sub>`), answering why it is not
/// there where it is not. Unlike a plain binary, a cargo-subcommand
/// executable expects its subcommand name as the first argument, so it must
/// be reached through `cargo` rather than invoked directly with `--version`.
pub(crate) fn cargo_subcommand_available(ctx: &Context, sub: &str) -> Result<(), String> {
    let mut probe = ctx.cargo();
    probe.args([sub, "--version"]);
    output_or_reason(
        &ctx.children,
        &format!("cargo {sub} --version"),
        &mut probe,
        ctx.children.ordinary_budget(),
    )
    .map(drop)
}

fn relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        cargo_profile, cargo_subcommand_available, ci_long_plan, dir_size, host_clippy,
        host_command, host_order_args, host_pass, image_drivers, kernel_diag_feature_args, libtest,
        parse_image_args, parse_run_args, parse_test_options, run_session_spec, CiStage, Command,
        HostLane, HostRun, HostStep, LongStep, PassOrder, Path, RunBudget, RunMachine, CI_STAGES,
        CI_TEST_ARGS, DEFAULT_RUN_CPUS, DOCS_RUSTDOCFLAGS, PI_FIRMWARE_ENV,
        PLATFORM_IMAGE_DRIVER_STORE, STATIC_GATES,
    };
    use crate::commands::soak::{Budget, NIGHTLY};
    use crate::Context;
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::time::{Duration, UNIX_EPOCH};

    fn image_args(args: &[&str]) -> Result<tairix_mkimage::ImageKind, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse_image_args(&args).map(|parsed| parsed.kind)
    }

    #[test]
    fn an_image_root_is_plaintext_unless_a_debug_image_asks_for_encryption() {
        let plain = image_args(&["--target", "aarch64-rpi"]).expect("parses");
        assert_eq!(plain.root(), tairix_mkimage::VolumeProtection::Plaintext);
        let encrypted = image_args(&["--target", "aarch64-rpi", "--encrypt-root"]).expect("parses");
        assert_eq!(
            encrypted.root(),
            tairix_mkimage::VolumeProtection::Encrypted
        );
        let refused = image_args(&[
            "--target",
            "aarch64-rpi",
            "--profile",
            "installer",
            "--encrypt-root",
        ]);
        assert!(
            refused.is_err_and(|why| why.contains("--encrypt-root")),
            "an encrypted installer image is never authored"
        );
    }

    /// Both oracles are in the pipeline, each stage once, the dearest last.
    ///
    /// A reordering once dropped the `miri` call while keeping its comment,
    /// so `ci` passed with nothing interpreting any `unsafe`.
    #[test]
    fn the_pipeline_roster_names_both_oracles_exactly_once() {
        for oracle in [CiStage::Miri, CiStage::Loom] {
            assert_eq!(
                CI_STAGES.iter().filter(|s| **s == oracle).count(),
                1,
                "{oracle:?} must appear in the pipeline exactly once"
            );
        }
        let mut labels: Vec<&str> = CI_STAGES.iter().map(|s| s.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), CI_STAGES.len(), "a stage is named twice");
        assert_eq!(
            CI_STAGES.last(),
            Some(&CiStage::Miri),
            "the most expensive stage runs last"
        );
    }

    /// The backticked spans of `text` that are `ci` stage labels, in order.
    fn stage_spans(text: &str) -> Vec<&str> {
        text.split('`')
            .skip(1)
            .step_by(2)
            .filter(|span| CI_STAGES.iter().any(|s| s.label() == *span))
            .collect()
    }

    /// The crate docs' `ci` bullet names every stage, each once, in the order
    /// the pipeline runs them.
    #[test]
    fn the_crate_docs_name_every_ci_stage_in_order() {
        const MAIN: &str = include_str!("main.rs");
        let (_, rest) = MAIN
            .split_once("//! - `ci` ")
            .expect("main.rs documents `ci`");
        let bullet = rest
            .split_once("\n//! - `")
            .map_or(rest, |(bullet, _)| bullet);
        let labels: Vec<&str> = CI_STAGES.iter().map(|s| s.label()).collect();
        assert_eq!(stage_spans(bullet), labels);
    }

    /// The contributing page describes every `ci` stage and static gate, and
    /// nothing `ci` does not run.
    #[test]
    fn the_contributing_page_describes_every_ci_stage_and_gate() {
        const PAGE: &str = include_str!("../../../docs/src/contributing.md");
        let (_, rest) = PAGE
            .split_once("\n## What `cargo xtask ci` runs\n")
            .expect("the page describes what ci runs");
        let section = rest
            .split_once("\n## ")
            .map_or(rest, |(section, _)| section);
        let described: BTreeSet<&str> = section
            .lines()
            .filter(|line| line.starts_with('|'))
            .filter_map(|row| row.split('`').nth(1))
            .collect();
        let run: BTreeSet<&str> = CI_STAGES
            .iter()
            .filter(|s| **s != CiStage::StaticGates)
            .map(|s| s.label())
            .chain(STATIC_GATES.iter().map(|(label, _)| *label))
            .collect();
        let missing: Vec<&&str> = run.difference(&described).collect();
        let extra: Vec<&&str> = described.difference(&run).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "the table lacks {missing:?} and describes {extra:?}, which ci does not run"
        );
    }

    /// `ci-long` runs every `ci` stage: each the hunt does not repeat, once, and
    /// the hunt last.
    #[test]
    fn ci_long_runs_every_ci_stage() {
        let plan: Vec<LongStep> = ci_long_plan().collect();
        for ci_stage in CI_STAGES {
            let once = plan
                .iter()
                .filter(|step| **step == LongStep::Once(ci_stage))
                .count();
            assert_eq!(once, usize::from(!ci_stage.flake_hunted()), "{ci_stage:?}");
        }
        assert_eq!(
            plan.iter()
                .filter(|step| **step == LongStep::FlakeHunt)
                .count(),
            1
        );
        assert_eq!(plan.last(), Some(&LongStep::FlakeHunt));
        assert!(plan.contains(&LongStep::Once(CiStage::Loom)));
    }

    /// A host pass runs the workspace's tests, its doctests and every lane's,
    /// each held to its account and the device-tree tests handed the firmware.
    #[test]
    fn a_host_pass_runs_every_host_test_each_held_to_its_account() {
        let ctx = Context {
            workspace_root: Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .expect("the workspace root")
                .to_path_buf(),
            cargo: OsString::from("true"),
            children: std::sync::Arc::new(crate::children::Children::new(None)),
        };
        let firmware = Path::new("/staged/pi-firmware");
        let args = |step: &HostStep| -> Vec<String> {
            step.command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let names_firmware = |step: &HostStep| {
            step.command
                .get_envs()
                .any(|(key, value)| key == PI_FIRMWARE_ENV && value == Some(firmware.as_os_str()))
        };

        let opts = parse_test_options(&[]).expect("no arguments");
        let steps = host_pass(&ctx, &opts, 1, firmware).expect("the pass");
        assert_eq!(steps.len(), 2 + opts.host_lanes().count());
        assert!(args(&steps[0]).contains(&"--all-targets".to_string()));
        assert!(names_firmware(&steps[0]));
        assert!(
            args(&steps[1]).contains(&"--doc".to_string()),
            "the doctests"
        );
        for lane in &steps[2..] {
            assert!(names_firmware(lane), "{}", lane.label);
        }
        assert!(steps
            .iter()
            .all(|step| step.account == libtest::Account::Everything));

        let filtered = parse_test_options(&[OsString::from("a_filter")]).expect("a filter");
        let steps = host_pass(&ctx, &filtered, 1, firmware).expect("the pass");
        assert!(steps
            .iter()
            .all(|step| step.account == libtest::Account::Forwarded));
        assert!(
            args(&steps[1]).contains(&"a_filter".to_string()),
            "the doctests filtered too"
        );

        let selected = parse_test_options(&[OsString::from("--lib")]).expect("a selector");
        let steps = host_pass(&ctx, &selected, 1, firmware).expect("the pass");
        assert!(steps
            .iter()
            .all(|step| !args(step).contains(&"--doc".to_string())));
    }

    /// Every host step is handed the firmware and runs the forwarded
    /// arguments before the order's, whichever run it is.
    #[test]
    fn a_host_command_names_the_firmware_and_orders_after_the_caller() {
        let ctx = Context {
            workspace_root: Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf(),
            cargo: OsString::from("true"),
            children: std::sync::Arc::new(crate::children::Children::new(None)),
        };
        let firmware = Path::new("/staged/pi-firmware");
        let selection = [OsString::from("-p"), OsString::from("lane")];
        for run in [
            HostRun::Workspace,
            HostRun::Doctests,
            HostRun::Lane(&selection),
        ] {
            let command = host_command(&ctx, run, &[OsString::from("a_filter")], Some(7), firmware);
            assert!(command
                .get_envs()
                .any(|(key, value)| key == PI_FIRMWARE_ENV && value == Some(firmware.as_os_str())));
            let args: Vec<_> = command.get_args().collect();
            let filter = args
                .iter()
                .position(|arg| *arg == "a_filter")
                .expect("forwarded");
            let seed = args
                .iter()
                .position(|arg| *arg == "7")
                .expect("the order seed");
            assert!(filter < seed);
        }
    }

    /// The crate docs list exactly the subcommands xtask has.
    #[test]
    fn the_crate_docs_list_every_subcommand() {
        const MAIN: &str = include_str!("main.rs");
        let listed: BTreeSet<&str> = MAIN
            .lines()
            .filter_map(|line| line.strip_prefix("//! - `")?.split_once('`'))
            .map(|(name, _)| name)
            .collect();
        let commands: BTreeSet<&str> = Command::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(listed, commands);
    }

    /// `clean` is a first-class, parseable subcommand listed in the closed
    /// command set, so `cargo xtask clean` reaches `run_clean` and the
    /// generated `--help`/usage lists it.
    #[test]
    fn clean_is_a_registered_subcommand() {
        assert!(
            matches!(Command::parse("clean"), Some(Command::Clean)),
            "`clean` must parse to the Clean subcommand"
        );
        assert!(
            Command::ALL.iter().any(|c| c.name() == "clean"),
            "`clean` must appear in the closed command set"
        );
    }

    /// `run` is a first-class, parseable subcommand listed in the closed
    /// command set, so `cargo xtask run` reaches the interactive QEMU
    /// session and the generated `--help`/usage lists it.
    #[test]
    fn run_is_a_registered_subcommand() {
        assert!(
            matches!(Command::parse("run"), Some(Command::Run)),
            "`run` must parse to the Run subcommand"
        );
        assert!(
            Command::ALL.iter().any(|c| c.name() == "run"),
            "`run` must appear in the closed command set"
        );
    }

    /// The session `run` boots when it is asked for nothing but an image.
    const DEFAULT_MACHINE: RunMachine = RunMachine {
        cpus: DEFAULT_RUN_CPUS,
        ram_mib: None,
    };

    /// `run` consumes only its own `--cpus` and `--ram`; every other argument
    /// is forwarded verbatim to the shared `image` grammar so the two entry
    /// points cannot drift apart.
    #[test]
    fn run_args_default_the_machine_and_forward_the_rest() {
        let args = argv(&["--target", "aarch64-rpi", "--profile", "installer"]);
        let (machine, rest) = parse_run_args(&args).expect("defaults parse");
        assert_eq!(machine, DEFAULT_MACHINE);
        assert_eq!(rest, args);
    }

    #[test]
    fn run_cpus_flag_overrides_the_default() {
        let args = argv(&["--cpus", "2", "--target", "aarch64-rpi"]);
        let (machine, rest) = parse_run_args(&args).expect("--cpus parses");
        assert_eq!(machine.cpus, 2);
        assert_eq!(rest, argv(&["--target", "aarch64-rpi"]));
    }

    /// `--ram` sizes the session's guest, which the kernel then finds in the
    /// device tree QEMU generates for it.
    #[test]
    fn run_ram_flag_sizes_the_guest() {
        for (size, mib) in [
            ("512M", 512),
            ("512m", 512),
            ("768", 768),
            ("2G", 2048),
            ("1g", 1024),
        ] {
            let args = argv(&["--ram", size, "--target", "aarch64-rpi"]);
            let (machine, rest) = parse_run_args(&args).expect("--ram parses");
            assert_eq!(machine.ram_mib, Some(mib), "--ram {size}");
            assert_eq!(rest, argv(&["--target", "aarch64-rpi"]));
        }
        let sized = run_session_spec(
            Path::new("/tmp/kernel.img"),
            Path::new("/tmp/disk.img"),
            RunMachine {
                ram_mib: Some(512),
                ..DEFAULT_MACHINE
            },
        );
        assert_eq!(sized.ram_mib(), 512);
        let unsized_spec = run_session_spec(
            Path::new("/tmp/kernel.img"),
            Path::new("/tmp/disk.img"),
            DEFAULT_MACHINE,
        );
        assert_eq!(
            unsized_spec.ram_mib(),
            tairix_qemu::aarch64::DEFAULT_RAM_MIB,
            "no --ram keeps the board's default"
        );
    }

    /// A `--ram` that is missing, not a whole size, nothing, or more mebibytes
    /// than the runner can name is refused rather than defaulted (fail closed).
    #[test]
    fn run_ram_rejects_nothing_garbage_fractions_and_overflow() {
        for size in [
            "0",
            "0G",
            "",
            "M",
            "512X",
            "1.5G",
            "-1G",
            "4294967296",
            "4194304G",
        ] {
            assert!(
                parse_run_args(&argv(&["--ram", size])).is_err(),
                "--ram {size} was accepted"
            );
        }
        assert!(parse_run_args(&argv(&["--ram"])).is_err());
    }

    /// A human's session gets a NIC, and it is the one the image it boots
    /// was configured for.
    ///
    /// The session once attached no network device at all, which the
    /// windowed run could not report — the guest simply had no interface
    /// and nothing said so. Three things have to agree for it to work, and
    /// all three are asserted here: a NIC is attached, its backing answers
    /// (user-mode, not a dgram wire whose harness peer only exists in a
    /// vertical), and its MAC is the identity the shipped `network.conf`
    /// binds by.
    #[test]
    fn the_interactive_session_attaches_a_nic_the_shipped_config_claims() {
        let spec = run_session_spec(
            Path::new("/tmp/kernel.img"),
            Path::new("/tmp/disk.img"),
            DEFAULT_MACHINE,
        );
        assert_eq!(spec.net_devices.len(), 1, "exactly one NIC");
        assert_eq!(
            spec.net_devices[0].backend,
            tairix_qemu::NetBackend::User,
            "a session's wire must answer: QEMU's own DHCP/DNS/NAT, not a \
             harness peer that is not running"
        );
        let mac = spec.net_devices[0]
            .mac
            .as_deref()
            .expect("the MAC is pinned, not left to QEMU");
        assert_eq!(mac, image_drivers::VIRT_SESSION_NIC_MAC);
        let config =
            tairix_netconfig::NetworkConfig::parse(&image_drivers::platform_network_conf())
                .expect("the shipped config parses");
        assert_eq!(
            config
                .interface(image_drivers::PLATFORM_VIRT_ALIAS)
                .and_then(|i| i.match_mac)
                .map(|m| m.render()),
            Some(mac.to_string()),
            "the interface the guest configures must be the device the \
             session creates"
        );
    }

    /// The shipped driver store carries a driver for the NIC of either way
    /// this image boots — the board's GENET and the virtio-net an emulated
    /// or virtualised boot presents — so whichever is discovered binds.
    #[test]
    fn the_platform_driver_store_covers_both_nics() {
        for path in [
            image_drivers::GENET_STORE_PATH,
            image_drivers::VIRTIO_NET_STORE_PATH,
        ] {
            assert!(
                PLATFORM_IMAGE_DRIVER_STORE.iter().any(|(p, _)| *p == path),
                "the image must ship the driver for every NIC it declares"
            );
        }
    }

    /// The shipped driver store carries the Pi's audio paths: the headphone
    /// jack, an I2S HAT's interface and either codec, and a USB audio device
    /// behind the bus chain.
    #[test]
    fn the_platform_driver_store_carries_the_pis_audio_drivers() {
        for path in [
            image_drivers::PWM_AUDIO_STORE_PATH,
            image_drivers::I2S_AUDIO_STORE_PATH,
            image_drivers::PCM5102A_STORE_PATH,
            image_drivers::PCM5122_STORE_PATH,
            image_drivers::USB_UAC_STORE_PATH,
        ] {
            assert!(
                PLATFORM_IMAGE_DRIVER_STORE.iter().any(|(p, _)| *p == path),
                "the image must ship every audio driver the board can use"
            );
        }
    }

    /// A missing, non-numeric, or zero `--cpus` value is rejected rather
    /// than silently defaulted (fail closed).
    #[test]
    fn run_cpus_rejects_zero_missing_and_garbage() {
        assert!(parse_run_args(&argv(&["--cpus", "0"])).is_err());
        assert!(parse_run_args(&argv(&["--cpus", "many"])).is_err());
        assert!(parse_run_args(&argv(&["--cpus"])).is_err());
    }

    #[test]
    fn docs_rustdoc_does_not_multiply_cargo_parallelism() {
        assert_eq!(DOCS_RUSTDOCFLAGS, "-D warnings");
    }

    /// `dir_size` sums regular files recursively and treats an absent
    /// directory as empty so the post-clean report never fails.
    #[test]
    fn dir_size_sums_regular_files_and_tolerates_missing() {
        let root = std::env::temp_dir().join(format!(
            "tairix-xtask-clean-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        assert_eq!(dir_size(&root), 0, "a missing directory is empty");

        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested dirs");
        std::fs::write(root.join("top.bin"), [0u8; 100]).expect("write top file");
        std::fs::write(nested.join("deep.bin"), [0u8; 23]).expect("write nested file");

        assert_eq!(dir_size(&root), 123, "sizes sum across the whole subtree");

        std::fs::remove_dir_all(&root).expect("clean up temp tree");
    }

    /// The image profile dictates the kernel's Cargo build profile so the
    /// console's `cfg!(debug_assertions)` boot-log routing is correct: the
    /// `debug` image must build a `dev`-profile kernel (assertions on →
    /// log to UART) and the shippable `installer` image a `--release`
    /// kernel (assertions off → log to screen). Regression guard for the
    /// defect where both images shared a single `--release` kernel and the
    /// debug log never reached the UART.
    #[test]
    fn an_image_builds_in_the_cargo_profile_its_profile_names() {
        assert_eq!(
            cargo_profile(tairix_mkimage::ImageProfile::Debug),
            tairix_itest_harness::pie::Profile::Dev,
            "the debug image must build in Cargo's dev profile (no --release)"
        );
        assert_eq!(
            cargo_profile(tairix_mkimage::ImageProfile::Installer),
            tairix_itest_harness::pie::Profile::Release,
            "the installer image must build optimised"
        );
    }

    /// The kernel diagnostics are gated to the non-shippable `debug` image:
    /// its kernel build gets the lockup-watchdog aids, and the shippable
    /// `installer` build gets nothing, so the address-bearing developer aids
    /// and their hot-path recording are compiled entirely out of any
    /// shippable kernel.
    #[test]
    fn kernel_diagnostics_are_gated_to_the_debug_image() {
        assert_eq!(
            kernel_diag_feature_args(tairix_mkimage::ImageProfile::Debug),
            &["--features", "watchdog-diagnostics"],
            "the debug image must compile in the kernel diagnostics"
        );
        assert!(
            kernel_diag_feature_args(tairix_mkimage::ImageProfile::Installer).is_empty(),
            "the shippable installer image must compile the diagnostics out entirely"
        );
    }

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// The availability probe must fail closed: an unknown cargo subcommand
    /// is reported absent rather than mistakenly present. This guards the
    /// regression that motivated the probe — checking a cargo-subcommand
    /// binary with a bare `--version` (e.g. `cargo-llvm-cov --version`)
    /// errors out, so the probe routes through `cargo <sub>` instead.
    #[test]
    fn cargo_subcommand_probe_fails_closed_for_unknown_subcommand() {
        let ctx = Context::discover(None).expect("workspace context");
        let absent = cargo_subcommand_available(&ctx, "definitely-not-a-real-cargo-subcommand")
            .expect_err("reported absent");
        assert!(
            absent.contains("cargo definitely-not-a-real-cargo-subcommand --version failed with"),
            "naming the probe: {absent}"
        );
    }

    #[test]
    fn test_options_default_to_a_single_run() {
        let opts = parse_test_options(&[]).expect("empty args parse");
        assert_eq!(opts.budget, RunBudget::Count(1));
        assert!(!opts.run_qemu);
        assert!(!opts.run_wasm);
        assert!(!opts.run_scenes);
        assert_eq!(opts.order, PassOrder::Alphabetical);
        assert!(opts.forward.is_empty());
    }

    #[test]
    fn scenes_flag_adds_the_scene_tier() {
        let opts = parse_test_options(&argv(&["--scenes", "--shuffle"])).expect("parse");
        assert!(opts.run_scenes);
        assert!(opts.forward.is_empty(), "the flag is not forwarded");
    }

    /// `--only` skips the host stage, so a scene tier asked for beside it
    /// would silently not run.
    #[test]
    fn scenes_are_refused_beside_only() {
        let err = parse_test_options(&argv(&["--qemu", "--only", "audio", "--scenes"]))
            .expect_err("conflict rejected");
        assert!(err.contains("`--scenes`"), "unexpected error: {err}");
    }

    /// `ci` runs every host lane but the scene tier, which `--scenes` adds.
    #[test]
    fn ci_runs_every_host_lane_but_the_scene_tier() {
        let ci = parse_test_options(&argv(&CI_TEST_ARGS)).expect("ci's arguments parse");
        assert!(ci.run_qemu);
        assert!(ci.run_wasm, "the gate runs the wasm32 verticals too");
        assert_eq!(ci.order, PassOrder::Shuffled(None));
        assert_eq!(ci.host_lanes().collect::<Vec<_>>(), [HostLane::Diagnostics]);
        let scenes = parse_test_options(&argv(&["--scenes"])).expect("parse");
        assert_eq!(scenes.host_lanes().collect::<Vec<_>>(), HostLane::ALL);
    }

    /// Host clippy lints the workspace and then every lane, the scene tier
    /// included: a lane's gated code builds in no other host configuration.
    #[test]
    fn host_clippy_lints_the_workspace_then_every_lane() {
        let ctx = Context::discover(None).expect("workspace context");
        let passes = host_clippy(&ctx, &[]).expect("the passes");
        assert_eq!(passes.len(), 1 + HostLane::ALL.len());
        for lane in HostLane::ALL {
            assert!(
                passes.iter().any(|(label, _)| label.contains(lane.label())),
                "{lane:?} is not linted"
            );
        }
        for (label, cmd) in &passes {
            let args: Vec<String> = cmd
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.ends_with(&["--", "-D", "warnings"].map(String::from)),
                "{label}"
            );
        }
    }

    /// Every host lane selects workspace packages, each with a feature it
    /// turns on, so neither its test pass nor its clippy pass can widen into
    /// the whole workspace or build nothing gated.
    #[test]
    fn every_host_lane_selects_its_packages_with_their_features() {
        let ctx = Context::discover(None).expect("workspace context");
        for lane in HostLane::ALL {
            let cmd = lane.cargo(&ctx, "clippy").expect("the lane's packages");
            let args: Vec<String> = cmd
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(args[..3], ["clippy", "--all-targets", "--locked"]);
            let features = args
                .iter()
                .skip_while(|arg| *arg != "--features")
                .nth(1)
                .expect("the lane's features");
            let packages: Vec<&String> = args
                .windows(2)
                .filter(|pair| pair[0] == "-p")
                .map(|pair| &pair[1])
                .collect();
            assert!(!packages.is_empty(), "{lane:?} selects no package");
            for package in packages {
                assert!(
                    features
                        .split(',')
                        .any(|feature| feature.starts_with(&format!("{package}/"))),
                    "{lane:?}: {package} is selected with no feature of its own"
                );
            }
        }
    }

    /// A pinned order seed selects shuffling too, so `--shuffle-seed N` alone
    /// replays a reported failure.
    #[test]
    fn a_pinned_order_seed_implies_shuffling() {
        let opts = parse_test_options(&argv(&["--shuffle-seed", "1234"])).expect("parse");
        assert_eq!(opts.order, PassOrder::Shuffled(Some(1234)));
        assert!(opts.forward.is_empty(), "the flags are not forwarded");
        let after =
            parse_test_options(&argv(&["--shuffle-seed", "1234", "--shuffle"])).expect("parse");
        assert_eq!(after.order, opts.order, "a later `--shuffle` keeps the pin");
    }

    #[test]
    fn shuffle_alone_leaves_the_seed_fresh_per_pass() {
        let opts = parse_test_options(&argv(&["--shuffle"])).expect("parse");
        assert_eq!(opts.order, PassOrder::Shuffled(None));
    }

    /// A seed read from a pass's label, pinned, starts the first pass in that
    /// very order, and each later pass in one of its own derived from it.
    #[test]
    fn a_logged_order_seed_replays_the_pass_it_names() {
        let logged = 1_788_601_047_185_464_875;
        let order = parse_test_options(&argv(&["--shuffle-seed", "1788601047185464875"]))
            .expect("parse")
            .order;
        assert_eq!(order.seed(1), Some(logged));
        let second = order.seed(2).expect("a shuffled pass");
        assert_ne!(second, logged);
        assert_eq!(
            order.seed(2),
            Some(second),
            "a derived seed is reproducible"
        );
        assert_eq!(PassOrder::Alphabetical.seed(1), None);
        assert!(PassOrder::Shuffled(None).seed(1).is_some());
    }

    #[test]
    fn a_non_numeric_order_seed_is_rejected() {
        let err = parse_test_options(&argv(&["--shuffle-seed", "later"]))
            .expect_err("non-numeric rejected");
        assert!(
            err.contains("invalid `--shuffle-seed`"),
            "unexpected error: {err}"
        );
        let err =
            parse_test_options(&argv(&["--shuffle-seed"])).expect_err("missing value rejected");
        assert!(err.contains("requires a u64"), "unexpected error: {err}");
    }

    /// The ordering flags reach the harness, and supply the `--` separator
    /// only when the caller did not already pass one.
    #[test]
    fn order_args_supply_the_harness_separator_exactly_once() {
        assert_eq!(
            host_order_args(&[], 7),
            argv(&["--", "-Z", "unstable-options", "--shuffle-seed", "7"])
        );
        assert_eq!(
            host_order_args(&argv(&["--", "--nocapture"]), 7),
            argv(&["-Z", "unstable-options", "--shuffle-seed", "7"])
        );
    }

    #[test]
    fn count_flag_sets_the_iteration_total() {
        let opts = parse_test_options(&argv(&["--qemu", "--count", "100"])).expect("parse");
        assert_eq!(opts.budget, RunBudget::Count(100));
        assert!(opts.run_qemu);
    }

    #[test]
    fn iterations_alias_matches_count() {
        let opts = parse_test_options(&argv(&["--iterations", "7"])).expect("parse");
        assert_eq!(opts.budget, RunBudget::Count(7));
    }

    #[test]
    fn unrecognised_arguments_are_forwarded_to_cargo_test() {
        let opts =
            parse_test_options(&argv(&["--count", "3", "--", "--nocapture"])).expect("parse");
        assert_eq!(opts.budget, RunBudget::Count(3));
        assert_eq!(opts.forward, argv(&["--", "--nocapture"]));
    }

    /// `--soak` with no override selects the nightly floor.
    #[test]
    fn soak_flag_selects_the_twenty_four_hour_budget() {
        let opts = parse_test_options(&argv(&["--qemu", "--soak"])).expect("parse");
        assert_eq!(opts.budget, RunBudget::Soak(Budget::For(NIGHTLY)));
        assert!(opts.run_qemu);
    }

    /// `--secs` tunes the soak budget down for smoke runs.
    #[test]
    fn secs_overrides_the_soak_budget() {
        let opts = parse_test_options(&argv(&["--soak", "--secs", "120"])).expect("parse");
        assert_eq!(
            opts.budget,
            RunBudget::Soak(Budget::For(Duration::from_secs(120)))
        );
    }

    /// `--secs` alone is enough to select a duration budget.
    #[test]
    fn secs_without_soak_sets_a_duration_budget() {
        let opts = parse_test_options(&argv(&["--secs", "30"])).expect("parse");
        assert_eq!(
            opts.budget,
            RunBudget::Soak(Budget::For(Duration::from_secs(30)))
        );
    }

    /// `--until` ends the soak at the window's close, the form
    /// `tools/ci/soak.sh --window` passes.
    #[test]
    fn until_ends_the_soak_at_the_window_close() {
        let opts = parse_test_options(&argv(&["--qemu", "--soak", "--until", "1700000000"]))
            .expect("parse");
        assert_eq!(
            opts.budget,
            RunBudget::Soak(Budget::Until(
                UNIX_EPOCH + Duration::from_secs(1_700_000_000)
            ))
        );
        assert!(
            opts.forward.is_empty(),
            "nothing is forwarded to cargo test"
        );
    }

    #[test]
    fn secs_and_until_conflict_is_rejected() {
        let err = parse_test_options(&argv(&["--secs", "30", "--until", "1700000000"]))
            .expect_err("two ends rejected");
        assert!(
            err.contains("mutually exclusive"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn count_and_until_conflict_is_rejected() {
        let err = parse_test_options(&argv(&["--count", "2", "--until", "1700000000"]))
            .expect_err("conflict rejected");
        assert!(
            err.contains("cannot be combined"),
            "unexpected error: {err}"
        );
    }

    /// A fixed count and a wall-clock budget are mutually exclusive; rather
    /// than pick a silent winner, combining them is an error.
    #[test]
    fn count_and_soak_conflict_is_rejected() {
        let err =
            parse_test_options(&argv(&["--count", "5", "--soak"])).expect_err("conflict rejected");
        assert!(
            err.contains("cannot be combined"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn non_numeric_secs_is_rejected() {
        let err = parse_test_options(&argv(&["--secs", "soon"])).expect_err("non-numeric rejected");
        assert!(
            err.contains("`--secs` expects a u64"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn secs_without_a_value_is_rejected() {
        let err = parse_test_options(&argv(&["--secs"])).expect_err("missing value rejected");
        assert!(err.contains("requires a number"), "unexpected error: {err}");
    }

    /// `Count` runs exactly the requested number of passes, in order.
    #[test]
    fn run_budget_count_runs_exactly_n_passes() {
        let mut passes = Vec::new();
        RunBudget::Count(3)
            .for_each(|pass| {
                passes.push(pass);
                Ok(())
            })
            .expect("count budget runs");
        assert_eq!(passes, vec![1, 2, 3]);
    }

    /// A spent budget still runs one full pass: the matrix is never cut off
    /// before a run, and a soak that ran nothing would report a pass for it.
    #[test]
    fn run_budget_soak_runs_at_least_one_pass() {
        for budget in [Budget::For(Duration::ZERO), Budget::Until(UNIX_EPOCH)] {
            let mut passes = 0u64;
            RunBudget::Soak(budget)
                .for_each(|_| {
                    passes += 1;
                    Ok(())
                })
                .expect("soak budget runs");
            assert_eq!(passes, 1, "{budget:?}");
        }
    }

    /// The nightly window's regression: the clock was read only after a pass,
    /// so a pass started just inside the budget overran it by most of a pass.
    #[test]
    fn run_budget_soak_starts_no_pass_it_cannot_finish() {
        // Each pass takes at least 600 ms, so after the first at most 400 ms
        // of the one-second budget remain: too few for another.
        let mut passes = 0u64;
        RunBudget::Soak(Budget::For(Duration::from_secs(1)))
            .for_each(|_| {
                passes += 1;
                std::thread::sleep(Duration::from_millis(600));
                Ok(())
            })
            .expect("soak budget runs");
        assert_eq!(passes, 1);
    }

    #[test]
    fn run_budget_soak_starts_a_pass_that_fits() {
        // An instant pass leaves nearly the whole hour, so a second starts;
        // failing it ends the loop without waiting the hour out.
        let mut passes = 0u64;
        let err = RunBudget::Soak(Budget::For(Duration::from_secs(3_600)))
            .for_each(|pass| {
                passes += 1;
                if pass == 2 {
                    Err("stop".to_string())
                } else {
                    Ok(())
                }
            })
            .expect_err("the second pass stops the loop");
        assert_eq!(err, "stop");
        assert_eq!(passes, 2);
    }

    /// A failing pass aborts the loop immediately and propagates the error
    /// (no retry).
    #[test]
    fn run_budget_stops_on_first_failure() {
        let mut passes = 0u64;
        let err = RunBudget::Count(5)
            .for_each(|pass| {
                passes += 1;
                if pass == 2 {
                    Err("boom".to_string())
                } else {
                    Ok(())
                }
            })
            .expect_err("failure propagates");
        assert_eq!(err, "boom");
        assert_eq!(passes, 2);
    }

    /// A zero count must fail closed rather than silently collapse the
    /// matrix to no runs — the whole point of the flag is to *repeat*.
    #[test]
    fn zero_count_is_rejected() {
        let err = parse_test_options(&argv(&["--count", "0"])).expect_err("zero rejected");
        assert!(err.contains("at least 1"), "unexpected error: {err}");
    }

    #[test]
    fn non_numeric_count_is_rejected() {
        let err =
            parse_test_options(&argv(&["--count", "lots"])).expect_err("non-numeric rejected");
        assert!(
            err.contains("invalid iteration count"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn count_without_a_value_is_rejected() {
        let err = parse_test_options(&argv(&["--count"])).expect_err("missing value rejected");
        assert!(
            err.contains("requires a positive integer"),
            "unexpected error: {err}"
        );
    }
}
