//! `cargo xtask rngsoak` — drive the statistical soak over the kernel
//! random subsystem's generators.
//!
//! The battery itself, and why every one of its tests is held against a
//! known-bad control, is documented in `tests/integration/rng_soak`. This
//! orchestrator is the single place that runs one generator's battery for a
//! wall-clock budget, mirroring the [`fuzz`](crate::commands),
//! [`proptest`](crate::commands), and `fssoak` orchestrators so a PR smoke
//! and a nightly soak share one definition of the target set.
//!
//! Each [`Target`] names a `#[test]` entry point in the
//! `tairix-test-rng-soak` integration binary (`tests/rng_soak.rs`). The
//! orchestrator exports the budget and byte-count seams the harness reads
//! ([`RNGSOAK_BUDGET_ENV`] and [`RNGSOAK_BYTES_ENV`], named once in
//! `tairix-fuzzseed` so both sides cannot drift); a plain `cargo test` leaves
//! both unset and runs a single fixed-seed smoke pass instead. A generator
//! any statistic rejects fails the command — the soak fails closed.
//!
//! A soaking target is meant to occupy its whole budget, so each child is
//! bounded by [`soak::child_deadline`], which outlasts what is left of it.
//!
//! The per-target fan-out into parallel jobs is `tools/ci/soak.sh`'s job
//! (like fuzz/proptest/fssoak); this command runs one target at a time.
//!
//! Adding a generator means adding a [`Target`] here, never teaching `ci`
//! about it directly.

use std::ffi::OsString;

use tairix_fuzzseed::{RNGSOAK_BUDGET_ENV, RNGSOAK_BYTES_ENV};

use crate::commands::soak::{self, Budget, Flags, Mode};
use crate::Context;

/// One generator the soak orchestrator knows how to run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Target {
    /// Short, unique selector used by `--target` and `soak.sh`.
    pub name: &'static str,
    /// `#[test]` entry point in the soak integration binary.
    pub test_fn: &'static str,
    /// One-line description shown by `cargo xtask rngsoak --list`.
    pub description: &'static str,
}

/// Workspace package owning the soak harness (`cargo test -p`).
const PACKAGE: &str = "tairix-test-rng-soak";

/// Integration-test binary (the `tests/<name>.rs` file stem).
const TEST_BIN: &str = "rng_soak";

/// Bytes per generator one soak pass draws and tests.
///
/// A pass is the granularity the budget is spent in — the harness will not
/// start one it cannot finish — so this bounds how far a run can overshoot a
/// short budget. It does not bound the run's *sensitivity*: the verdict is
/// reached over every pass at once, so a long budget simply accumulates more
/// of them.
const SOAK_PASS_BYTES: u64 = 16 * 1024 * 1024;

/// The closed set of soaked generators, in run order. Mirrors the
/// `tairix_test_rng_soak::TARGETS` registry the harness owns.
///
/// The predictable `NonCryptoRng` is deliberately absent: it is invertible by
/// design, so a battery is the wrong instrument for judging it and a passing
/// verdict would say nothing anyone should rely on.
pub const TARGETS: &[Target] = &[
    Target {
        name: "fast",
        test_fn: "soak_fast",
        description: "FastRng: buffered ChaCha12, fast key erasure",
    },
    Target {
        name: "csprng",
        test_fn: "soak_csprng",
        description: "CsRng: NIST SP 800-90A HMAC-SHA256 DRBG",
    },
];

/// Parsed `rngsoak` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// How long each generator soaks; `None` (a zero `--secs`) is one pass.
    pub budget: Option<Budget>,
    /// Optional target filter (`--target <name>`); runs all when `None`.
    pub only: Option<String>,
    /// `--list`: print the registry and exit without running anything.
    pub list: bool,
}

/// Parse `rngsoak` arguments. `--quick` is the default when no budget flag is
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
                "rngsoak: argument {} is not valid UTF-8",
                arg.display()
            ));
        };
        if budget.take("rngsoak", flag, &mut iter)? {
            continue;
        }
        match flag {
            "--list" => list = true,
            "--target" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "rngsoak: `--target` requires a generator name".to_string())?;
                let name = value
                    .to_str()
                    .ok_or_else(|| "rngsoak: `--target` value is not valid UTF-8".to_string())?;
                only = Some(name.to_string());
            }
            other => {
                return Err(format!(
                    "rngsoak: unexpected argument {other:?}; usage: \
                     cargo xtask rngsoak [--quick | --soak] [--target NAME] \
                     [--secs N | --until T] [--list]"
                ));
            }
        }
    }

    Ok(Options {
        budget: budget.budget("rngsoak", Mode::Quick)?,
        only,
        list,
    })
}

/// Resolve the targets an [`Options`] selects, preserving registry order.
///
/// # Errors
/// Returns an error if `--target` names a generator that is not registered.
pub fn selected(opts: &Options) -> Result<Vec<Target>, String> {
    let Some(name) = opts.only.as_deref() else {
        return Ok(TARGETS.to_vec());
    };
    match TARGETS.iter().find(|t| t.name == name) {
        Some(t) => Ok(vec![*t]),
        None => Err(format!(
            "rngsoak: unknown generator `{name}`; known: {}",
            TARGETS
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Run the selected generators' batteries for their budget.
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
            budget.export(&mut cmd, RNGSOAK_BUDGET_ENV);
        }
        cmd.env(RNGSOAK_BYTES_ENV, SOAK_PASS_BYTES.to_string());
        let label = format!("rngsoak {} ({})", t.name, soak::describe(opts.budget));
        ctx.run_with_timeout(&label, cmd, soak::child_deadline(opts.budget))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse, selected, SOAK_PASS_BYTES, TARGETS};
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
    fn until_soaks_every_generator_to_the_window_close() {
        let opts = parse(&argv(&["--soak", "--until", "1700000000"])).expect("until parses");
        assert_eq!(
            opts.budget,
            Some(Budget::Until(
                UNIX_EPOCH + Duration::from_secs(1_700_000_000)
            ))
        );
    }

    #[test]
    fn target_filter_selects_one_known_generator() {
        let opts = parse(&argv(&["--target", "csprng"])).expect("target parses");
        let chosen = selected(&opts).expect("known target");
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].test_fn, "soak_csprng");
    }

    #[test]
    fn unknown_target_fails_closed() {
        let opts = parse(&argv(&["--target", "mt19937"])).expect("flag parses");
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

    /// The two unpredictable generators are what this soak exists to
    /// exercise; the predictable one must stay out of it.
    #[test]
    fn registry_covers_both_unpredictable_generators_and_nothing_else() {
        for required in ["fast", "csprng"] {
            assert!(
                TARGETS.iter().any(|t| t.name == required),
                "missing required soak target {required}"
            );
        }
        assert!(
            !TARGETS.iter().any(|t| t.name == "noncrypto"),
            "the predictable generator must not be soaked as if it were secure"
        );
        assert_eq!(TARGETS.len(), 2);
    }

    #[test]
    fn every_registered_target_has_a_unique_name() {
        for (i, a) in TARGETS.iter().enumerate() {
            for b in &TARGETS[i + 1..] {
                assert_ne!(a.name, b.name, "duplicate soak target name");
            }
        }
    }

    /// A soak pass must draw enough sequences for the two-level rule to
    /// conclude anything, or the soak would report an inconclusive verdict as
    /// a failure every night.
    #[test]
    fn a_soak_pass_clears_the_decision_minimum() {
        let sequences = SOAK_PASS_BYTES / tairix_test_rng_soak::SEQUENCE_BYTES as u64;
        assert!(
            sequences >= tairix_test_rng_soak::MINIMUM_SEQUENCES,
            "a pass of {sequences} sequences cannot reach a verdict"
        );
    }
}
