//! `cargo xtask fssoak` — drive the in-RAM filesystem soak.
//!
//! `docs/src/filesystem/soak.md` specifies a filesystem soak that formats
//! a ≥ 1 GiB RAM volume with each first-party formatter and exercises it
//! for integrity and the fail-closed extremes, for `arxfs`, `ext4`, and
//! `fat32` **in parallel**. This orchestrator is the single place that
//! runs each filesystem's soak for a wall-clock budget, mirroring the
//! [`fuzz`](crate::commands) and
//! [`proptest`](crate::commands) orchestrators so a PR smoke and a
//! nightly soak share one definition of the target set.
//!
//! Each [`Target`] names a `#[test]` entry point in the
//! `tairix-test-fs-soak` integration binary (`tests/fs_soak.rs`). The
//! orchestrator exports the budget and device-size seams the harness
//! reads ([`FSSOAK_BUDGET_ENV`] and [`FSSOAK_BYTES_ENV`], named once in
//! `tairix-fuzzseed` so both sides cannot drift); a plain `cargo test`
//! leaves both unset and runs a single smoke iteration on a smaller
//! device instead. A target that finds an inconsistency, hangs, or
//! otherwise fails its invariant fails the command — the soak fails
//! closed.
//!
//! A soaking target is meant to occupy its whole budget, so each child is
//! bounded by [`soak::child_deadline`], which outlasts what is left of it.
//!
//! The per-target fan-out into parallel jobs is `tools/ci/soak.sh`'s job
//! (like fuzz/proptest); this command runs one target at a time.
//!
//! Adding a filesystem means adding a [`Target`] here, never teaching
//! `ci` about it directly.

use std::ffi::OsString;

use tairix_fuzzseed::{FSSOAK_BUDGET_ENV, FSSOAK_BYTES_ENV};

use crate::commands::soak::{self, Budget, Flags, Mode};
use crate::Context;

/// One filesystem the soak orchestrator knows how to run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Target {
    /// Short, unique selector used by `--target` and `soak.sh`.
    pub name: &'static str,
    /// `#[test]` entry point in the soak integration binary.
    pub test_fn: &'static str,
    /// One-line description shown by `cargo xtask fssoak --list`.
    pub description: &'static str,
}

/// Workspace package owning the soak harness (`cargo test -p`).
const PACKAGE: &str = "tairix-test-fs-soak";

/// Integration-test binary (the `tests/<name>.rs` file stem).
const TEST_BIN: &str = "fs_soak";

/// Device size, in bytes, the soak formats: the 1 GiB minimum from
/// `docs/src/filesystem/soak.md`. `--quick` keeps the full size and
/// simply runs fewer iterations (budget-bounded) rather than shrinking
/// below the spec'd minimum.
const SOAK_DEVICE_BYTES: u64 = 1024 * 1024 * 1024;

/// The closed set of soak filesystems, in run order. Mirrors the
/// `tairix_test_fs_soak::TARGETS` registry the harness owns.
pub const TARGETS: &[Target] = &[
    Target {
        name: "arxfs",
        test_fn: "soak_arxfs",
        description: "encrypted arxfs: format + integrity + extremes",
    },
    Target {
        name: "ext4",
        test_fn: "soak_ext4",
        description: "ext4: multi-group format + integrity + extremes",
    },
    Target {
        name: "fat32",
        test_fn: "soak_fat32",
        description: "fat32: format + integrity + extremes",
    },
    Target {
        name: "arxfs-random",
        test_fn: "soak_arxfs_random",
        description: "encrypted arxfs: randomized, model-checked op mix (new path each run)",
    },
    Target {
        name: "arxfs-plaintext",
        test_fn: "soak_arxfs_plaintext",
        description: "plaintext arxfs: format + integrity + extremes",
    },
    Target {
        name: "arxfs-plaintext-random",
        test_fn: "soak_arxfs_plaintext_random",
        description: "plaintext arxfs: randomized, model-checked op mix",
    },
];

/// Parsed `fssoak` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// How long each filesystem soaks; `None` (a zero `--secs`) is one pass.
    pub budget: Option<Budget>,
    /// Optional target filter (`--target <name>`); runs all when `None`.
    pub only: Option<String>,
    /// `--list`: print the registry and exit without running anything.
    pub list: bool,
}

/// Parse `fssoak` arguments. `--quick` is the default when no budget flag is
/// given.
///
/// # Errors
/// Returns a usage error for an unknown flag, a missing or unreadable flag
/// value, or two budget flags that contradict each other.
pub fn parse(args: &[OsString]) -> Result<Options, String> {
    let mut budget = Flags::default();
    let mut only: Option<String> = None;
    let mut list = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let Some(flag) = arg.to_str() else {
            return Err(format!(
                "fssoak: argument {} is not valid UTF-8",
                arg.display()
            ));
        };
        if budget.take("fssoak", flag, &mut iter)? {
            continue;
        }
        match flag {
            "--list" => list = true,
            "--target" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "fssoak: `--target` requires a filesystem name".to_string())?;
                let name = value
                    .to_str()
                    .ok_or_else(|| "fssoak: `--target` value is not valid UTF-8".to_string())?;
                only = Some(name.to_string());
            }
            other => {
                return Err(format!(
                    "fssoak: unexpected argument {other:?}; usage: \
                     cargo xtask fssoak [--quick | --soak] [--target NAME] \
                     [--secs N | --until T] [--list]"
                ));
            }
        }
    }

    Ok(Options {
        budget: budget.budget("fssoak", Mode::Quick)?,
        only,
        list,
    })
}

/// Resolve the targets an [`Options`] selects, preserving registry order.
///
/// # Errors
/// Returns an error if `--target` names a filesystem that is not
/// registered.
pub fn selected(opts: &Options) -> Result<Vec<Target>, String> {
    let Some(name) = opts.only.as_deref() else {
        return Ok(TARGETS.to_vec());
    };
    match TARGETS.iter().find(|t| t.name == name) {
        Some(t) => Ok(vec![*t]),
        None => Err(format!(
            "fssoak: unknown filesystem `{name}`; known: {}",
            TARGETS
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Run the selected filesystems' soaks for their budget.
pub fn run(ctx: &Context, opts: &Options) -> Result<(), String> {
    if opts.list {
        for t in TARGETS {
            println!("{:<8} {}", t.name, t.description);
        }
        return Ok(());
    }

    let targets = selected(opts)?;
    for t in &targets {
        let mut cmd = ctx.cargo();
        cmd.args([
            "test",
            "-p",
            PACKAGE,
            "--test",
            TEST_BIN,
            "--locked",
            "--",
            "--exact",
            t.test_fn,
            "--nocapture",
        ]);
        if let Some(budget) = opts.budget {
            budget.export(&mut cmd, FSSOAK_BUDGET_ENV);
        }
        cmd.env(FSSOAK_BYTES_ENV, SOAK_DEVICE_BYTES.to_string());
        let label = format!("fssoak {} ({})", t.name, soak::describe(opts.budget));
        ctx.run_with_timeout(&label, cmd, soak::child_deadline(opts.budget))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse, selected, TARGETS};
    use crate::commands::soak::{Budget, NIGHTLY, QUICK};
    use std::ffi::OsString;
    use std::time::{Duration, UNIX_EPOCH};

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn defaults_to_quick_running_every_target() {
        let opts = parse(&argv(&[])).expect("empty args parse");
        assert_eq!(opts.budget, Some(Budget::For(QUICK)));
        assert!(opts.only.is_none());
        assert!(!opts.list);
        assert_eq!(selected(&opts).expect("all targets").len(), TARGETS.len());
    }

    #[test]
    fn soak_flag_selects_the_soak_budget() {
        let opts = parse(&argv(&["--soak"])).expect("soak parses");
        assert_eq!(opts.budget, Some(Budget::For(NIGHTLY)));
    }

    #[test]
    fn until_soaks_every_filesystem_to_the_window_close() {
        let opts = parse(&argv(&["--soak", "--until", "1700000000"])).expect("until parses");
        assert_eq!(
            opts.budget,
            Some(Budget::Until(
                UNIX_EPOCH + Duration::from_secs(1_700_000_000)
            ))
        );
    }

    #[test]
    fn once_is_not_a_filesystem_soak_mode() {
        assert!(parse(&argv(&["--once"])).is_err());
    }

    #[test]
    fn target_filter_selects_one_known_filesystem() {
        let opts = parse(&argv(&["--target", "ext4"])).expect("target parses");
        let chosen = selected(&opts).expect("known target");
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].test_fn, "soak_ext4");
    }

    #[test]
    fn unknown_target_fails_closed() {
        let opts = parse(&argv(&["--target", "zfs"])).expect("flag parses");
        assert!(selected(&opts).is_err());
    }

    #[test]
    fn conflicting_budget_flags_are_rejected() {
        assert!(parse(&argv(&["--quick", "--soak"])).is_err());
    }

    #[test]
    fn secs_override_parses() {
        let opts = parse(&argv(&["--secs", "3"])).expect("secs parses");
        assert_eq!(opts.budget, Some(Budget::For(Duration::from_secs(3))));
    }

    #[test]
    fn secs_requires_a_number() {
        assert!(parse(&argv(&["--secs", "soon"])).is_err());
        assert!(parse(&argv(&["--secs"])).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse(&argv(&["--turbo"])).is_err());
    }

    #[test]
    fn list_flag_is_parsed() {
        let opts = parse(&argv(&["--list"])).expect("list parses");
        assert!(opts.list);
    }

    #[test]
    fn registry_covers_every_soak_target() {
        for required in [
            "arxfs",
            "ext4",
            "fat32",
            "arxfs-random",
            "arxfs-plaintext",
            "arxfs-plaintext-random",
        ] {
            assert!(
                TARGETS.iter().any(|t| t.name == required),
                "missing required soak target {required}"
            );
        }
    }

    #[test]
    fn every_registered_target_has_a_unique_name() {
        for (i, a) in TARGETS.iter().enumerate() {
            for b in &TARGETS[i + 1..] {
                assert_ne!(a.name, b.name, "duplicate soak target name");
            }
        }
    }
}
