//! `cargo xtask test --wasm` implementation.
//!
//! The wasm32 counterpart of [`super::qemu_tests`]. Where the bare-metal
//! verticals boot under QEMU, these run on `wasm32-unknown-unknown` —
//! two in a real (headless) browser, because what they test *is* the
//! browser: this module builds each wasm32 vertical `cdylib`
//! for `wasm32-unknown-unknown` and launches its harness against the
//! compiled module. Each harness decides PASS/FAIL — from the kernel's
//! console markers where it drives a browser, or from the module's own
//! exports where the subject is pure computation and a browser would only
//! narrow where the vertical can run — and propagates its exit status
//! here. The
//! enrolled verticals are listed in [`VERTICALS`]; adding a wasm32
//! vertical is one row there (one driver, not a
//! per-vertical copy of the build/run glue).
//!
//! `ci` passes `test --wasm` and `ci-long`'s hunt repeats every vertical. The
//! harnesses need `node`, and the browser ones the puppeteer
//! `tests/integration/package-lock.json` pins, with the Chrome it installs;
//! a host lacking either fails loudly before the matrix rather than skipping.

use std::path::PathBuf;

use crate::Context;

/// Rust target every wasm32 vertical is built for.
const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Loads puppeteer and checks the browser the harnesses launch can be run:
/// the one `PUPPETEER_EXECUTABLE_PATH` names, else the one puppeteer installed.
const BROWSER_PROBE: &str = "import { accessSync, constants } from 'node:fs'; \
     const { default: puppeteer } = await import('puppeteer'); \
     accessSync(process.env.PUPPETEER_EXECUTABLE_PATH || await puppeteer.executablePath(), \
     constants.X_OK);";

/// One enrolled wasm32 vertical.
pub(crate) struct Vertical {
    /// Workspace package name.
    pub(crate) package: &'static str,
    /// File stem cargo emits for the `cdylib` (the crate name, `-` → `_`).
    artifact: &'static str,
    /// Harness runner, workspace-relative.
    harness: &'static str,
}

/// The wasm32 verticals `cargo xtask test --wasm` builds and runs, in
/// order. Each instantiates the compiled module and decides its own
/// verdict — from console markers in a headless browser, or from the
/// module's exports where no browser is needed.
pub(crate) const VERTICALS: &[Vertical] = &[
    // Stage 3d + W8: boot, per-worker isolation, live scheduler ticks,
    // multi-worker SMP + cross-context IPI.
    Vertical {
        package: "tairix-test-kernel-arch-boot-wasm32",
        artifact: "tairix_test_kernel_arch_boot_wasm32.wasm",
        harness: "tests/integration/kernel_arch_boot_wasm32/web/harness.mjs",
    },
    // WinterSun's world generator under a real WebAssembly engine
    // (`plans/WINTERSUN.md` WS2): the wasm32 leg of the cross-architecture
    // determinism vertical whose bare-metal siblings run under QEMU. Its
    // harness needs no browser — the subject is arithmetic, and wasm32 is
    // the only Tier-1 target with a 32-bit `usize`, so an index that had
    // quietly become part of an answer shows up here and nowhere else.
    Vertical {
        package: "tairix-test-world-determinism-wasm32",
        artifact: "tairix_test_world_determinism_wasm32.wasm",
        harness: "tests/integration/world_determinism_wasm32/web/harness.mjs",
    },
    // WinterSun's authoritative simulation under a real WebAssembly engine
    // (`plans/WINTERSUN.md` WS3): the wasm32 leg of the cross-architecture
    // determinism vertical whose bare-metal siblings run under QEMU. Its
    // harness needs no browser — the subject is a simulation, and wasm32 is
    // the only Tier-1 target with a 32-bit `usize`, so a length that had
    // quietly become part of an answer shows up here and nowhere else.
    Vertical {
        package: "tairix-test-rules-determinism-wasm32",
        artifact: "tairix_test_rules_determinism_wasm32.wasm",
        harness: "tests/integration/rules_determinism_wasm32/web/harness.mjs",
    },
    // WinterSun's figure engine under a real WebAssembly engine
    // (`plans/FIGURE.md` FG5): the wasm32 leg of the cross-architecture
    // determinism vertical whose bare-metal siblings run under QEMU. Its
    // harness needs no browser — the subject is arithmetic over a posed
    // figure — and wasm32 is the only Tier-1 target with a 32-bit `usize`,
    // so a length that had quietly become part of an answer shows up here
    // and nowhere else.
    Vertical {
        package: "tairix-test-figure-determinism-wasm32",
        artifact: "tairix_test_figure_determinism_wasm32.wasm",
        harness: "tests/integration/figure_determinism_wasm32/web/harness.mjs",
    },
    // WinterSun's client frame under a real WebAssembly engine
    // (`plans/WINTERSUN.md` WS5): the wasm32 leg of the cross-target
    // rendering vertical whose bare-metal siblings run under QEMU. Its
    // harness needs no browser — the subject is arithmetic over a frame —
    // and wasm32 is the only Tier-1 target with a 32-bit `usize`, so a
    // length or an index that had quietly become part of a pixel shows up
    // here and nowhere else.
    Vertical {
        package: "tairix-test-client-frame-wasm32",
        artifact: "tairix_test_client_frame_wasm32.wasm",
        harness: "tests/integration/client_frame_wasm32/web/harness.mjs",
    },
    // The `display`-row parity vertical: signed framebuffer `.rxe`
    // lifecycle presenting to a real canvas (`plans/WIRING.md`).
    Vertical {
        package: "tairix-test-framebuffer-display-wasm32",
        artifact: "tairix_test_framebuffer_display_wasm32.wasm",
        harness: "tests/integration/framebuffer_display_wasm32/web/harness.mjs",
    },
];

/// The wasm32 verticals and the triple they build for.
///
/// The lint gate (`super::target_clippy`) reads the same enrolment table the
/// build reads, so a newly enrolled vertical is linted without being listed
/// twice.
pub fn packages() -> (&'static str, Vec<&'static str>) {
    (WASM_TARGET, VERTICALS.iter().map(|v| v.package).collect())
}

/// Check the harness toolchain is present and build every wasm32
/// vertical once.
///
/// Call this before the (possibly repeated) [`run_once`] passes. A host
/// lacking `node` fails loudly here rather than skipping (never silently skip a test).
pub fn prepare(ctx: &Context) -> Result<(), String> {
    noteln!("xtask: [test --wasm] building the wasm32 verticals");

    super::probe_version(ctx, "node").map_err(|why| {
        format!(
            "{why}; the wasm32 harnesses need Node.js on PATH, and the browser \
             ones the puppeteer tests/integration/package-lock.json pins"
        )
    })?;
    // Resolved as the harnesses resolve it, from beside them, down to the
    // browser they launch, so a missing one fails here with its remedy rather
    // than mid-matrix.
    let mut puppeteer = std::process::Command::new("node");
    puppeteer
        .current_dir(ctx.workspace_root.join("tests").join("integration"))
        .args(["--input-type=module", "-e", BROWSER_PROBE]);
    ctx.run("test --wasm (puppeteer)", puppeteer)
        .map_err(|why| {
            format!(
                "{why}; the browser harnesses need the puppeteer \
             tests/integration/package-lock.json pins: run `npm ci --prefix tests/integration`"
            )
        })?;

    for vertical in VERTICALS {
        build(ctx, vertical)?;
    }
    Ok(())
}

/// Run every enrolled wasm32 harness once.
///
/// The caller ([`super::run_test`]) owns the repeat loop so a duration
/// budget covers the whole matrix as a unit; this runs exactly one pass.
pub fn run_once(ctx: &Context) -> Result<(), String> {
    for vertical in VERTICALS {
        run_harness(ctx, vertical)?;
    }
    Ok(())
}

fn build(ctx: &Context, vertical: &Vertical) -> Result<(), String> {
    let mut cmd = ctx.cargo();
    cmd.args([
        "build",
        "--locked",
        "-p",
        vertical.package,
        "--target",
        WASM_TARGET,
    ]);
    ctx.run(&format!("test --wasm (build {})", vertical.package), cmd)
}

fn run_harness(ctx: &Context, vertical: &Vertical) -> Result<(), String> {
    ctx.run(
        &format!("test --wasm (harness {})", vertical.package),
        harness_command(ctx, vertical),
    )
}

/// The harness command that runs `vertical` against its built module.
pub(crate) fn harness_command(ctx: &Context, vertical: &Vertical) -> std::process::Command {
    let wasm: PathBuf = ctx
        .target_dir()
        .join(WASM_TARGET)
        .join("debug")
        .join(vertical.artifact);
    let mut cmd = std::process::Command::new("node");
    cmd.current_dir(&ctx.workspace_root)
        .arg(ctx.workspace_root.join(vertical.harness))
        .arg("--wasm")
        .arg(&wasm);
    cmd
}

#[cfg(test)]
mod tests {
    use super::VERTICALS;
    use crate::commands::feature_lane::workspace_manifests;

    /// Every wasm32 vertical crate is enrolled, so none is linted and run by
    /// no stage.
    #[test]
    fn every_wasm32_vertical_in_the_workspace_is_enrolled() {
        let root = crate::workspace_root_from_manifest_dir().expect("the workspace root");
        let unenrolled: Vec<String> = workspace_manifests(&root)
            .expect("the workspace manifests")
            .into_iter()
            .filter(|(_, (dir, _))| {
                dir.starts_with("tests/integration/") && dir.ends_with("_wasm32")
            })
            .map(|(package, _)| package)
            .filter(|package| !VERTICALS.iter().any(|vertical| vertical.package == package))
            .collect();
        assert!(
            unenrolled.is_empty(),
            "wasm32 verticals no stage runs: {unenrolled:?}"
        );
    }
}
