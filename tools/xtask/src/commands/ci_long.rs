//! `cargo xtask ci-long` — the long-runner flake hunt.
//!
//! `ci` runs every test exactly once, so a rare, timing-dependent flake can
//! slip through a green pull-request run. This command is the deliberate
//! counterpart for a dedicated, long-lived runner: it runs the *same* checks
//! `ci` does, but every **test-executing** stage is run [`REPS`] times
//! sequentially and then [`REPS`] times concurrently before the next test, so
//! a flake that only appears under repetition or contention is forced into the
//! open. The orchestration lives in [`super::run_ci_long`]; this module owns
//! the repeat-and-parallelise engine and the set of tests it drives.
//!
//! # What is repeated, and what is not
//!
//! Only the stages that *execute test cases* are flake-hunted
//! ([`super::CiStage::flake_hunted`]): the host `cargo test` matrix, with the
//! scene tier `ci` leaves out, the release-profile crypto constant-time
//! tests, the QEMU integration tests, the wasm32 verticals, the fuzz
//! harnesses, and the proptest models. Every other `ci` stage is a pass/fail check whose result cannot
//! change between runs, so repeating it 2·[`REPS`] times would burn a
//! long-runner's time for no signal: it runs once, in `ci`'s order, ahead of
//! the hunt.
//!
//! # Sequential then concurrent, per test
//!
//! For each test the hunt first runs [`REPS`] passes strictly one at a time
//! (the timing profile of an isolated run), then submits [`REPS`] replicas to
//! the shared concurrency runner ([`super::parallel`]) at once (the timing
//! profile under contention). The two profiles catch different flakes: a
//! use-once race surfaces sequentially; a shared-resource or scheduling race
//! surfaces concurrently.
//!
//! The concurrency **budget** differs by test kind, deliberately:
//!
//! * host / crypto / wasm32 / fuzz / proptest replicas are deadline-free host
//!   processes — a slow run merely finishes later, it does not fail — so all
//!   [`REPS`] replicas run at once (budget = `reps`).
//! * a host lane one run of which fills the machine, the scene tier, runs its
//!   replicas one at a time: twenty at once would only exhaust the host's
//!   memory.
//! * QEMU replicas are weighted exactly as [`super::qemu_tests::run_once`]
//!   does — a uniprocessor guest charges two units, an SMP guest the whole
//!   budget (so it runs alone) — against a budget of one third of the host's
//!   logical CPUs. That headroom keeps the host under-subscribed so no guest
//!   is starved into missing its own internal real-time deadlines, and the
//!   guest deadline is itself an *inactivity* budget rather than a
//!   total-runtime one (`tairix_qemu`), so a replica that runs a little slower
//!   under contention keeps emitting output and is never falsely timed out —
//!   the very flakiness the charter forbids manufacturing. So the [`REPS`]
//!   QEMU replicas still all get submitted; they simply overlap as far as the
//!   headroom budget allows rather than all at once.

use std::path::Path;

use crate::commands::libtest::Account;
use crate::commands::parallel::Job;
use crate::commands::{fuzz, proptest, qemu_tests, seed};
use crate::Context;

/// How many times each test is run in each phase.
///
/// A test is run this many times sequentially and then this many times
/// concurrently, so the hunt executes each test `2 * REPS` times in total.
pub(crate) const REPS: u32 = 20;

/// One test the flake hunt repeats.
///
/// A unit pairs a human label with a **factory** that builds a fresh [`Job`]
/// each call (a `Job` is single-use), plus the concurrency budget the runner
/// admits its replicas under during the concurrent phase.
pub(crate) struct FlakeUnit<'a> {
    /// Human-readable name shown in the phase banners.
    label: String,
    /// Maximum in-flight job weight admitted while running this unit's
    /// replicas concurrently (see the module docs for why it differs by kind).
    concurrency_budget: usize,
    /// Builds a fresh, single-use job for the given repetition index. The
    /// index varies a fuzz/proptest PRNG seed and the host matrix's test
    /// start order; it is ignored otherwise.
    factory: Box<dyn Fn(usize) -> Job + 'a>,
}

impl<'a> FlakeUnit<'a> {
    fn new(
        label: impl Into<String>,
        concurrency_budget: usize,
        factory: impl Fn(usize) -> Job + 'a,
    ) -> Self {
        Self {
            label: label.into(),
            concurrency_budget,
            factory: Box::new(factory),
        }
    }
}

/// Run every unit `reps` times sequentially, then `reps` times concurrently,
/// before moving on to the next unit, each batch through `run`: the parallel
/// runner, in a real hunt. Fails closed: the first sequential pass or
/// concurrent batch that fails aborts the hunt and returns the failure (there
/// is no retry — a failure is a flake found, which is the point).
pub(crate) fn flake_hunt(
    units: Vec<FlakeUnit<'_>>,
    reps: u32,
    run: impl Fn(Vec<Job>, usize) -> Result<(), String>,
) -> Result<(), String> {
    let reps = reps.max(1);
    for unit in units {
        noteln!("xtask: [ci-long] {} — {reps}x sequential", unit.label);
        for pass in 1..=reps {
            let job = (unit.factory)((pass - 1) as usize);
            // Budget one: the single job's output is live and it runs
            // strictly alone, giving the isolated-run timing profile.
            run(vec![job], 1).map_err(|e| {
                format!(
                    "ci-long: {} failed on sequential pass {pass}/{reps}: {e}",
                    unit.label
                )
            })?;
        }
        noteln!("xtask: [ci-long] {} — {reps}x concurrent", unit.label);
        let jobs: Vec<Job> = (0..reps).map(|i| (unit.factory)(i as usize)).collect();
        run(jobs, unit.concurrency_budget).map_err(|e| {
            format!(
                "ci-long: {} failed running {reps}x concurrently: {e}",
                unit.label
            )
        })?;
    }
    Ok(())
}

/// The ordered set of tests the flake hunt drives: the host test matrix, its
/// device-tree tests reading the firmware in `firmware`, the QEMU integration
/// tests, the wasm32 verticals, the fuzz harnesses, and the proptest models.
/// The QEMU binaries and wasm32 modules must already be built
/// ([`super::qemu_tests::build_all`], [`super::wasm_tests::prepare`]).
///
/// # Errors
///
/// A host lane `ci` runs whose packages cannot be read from the manifests.
pub(crate) fn all_units<'a>(
    ctx: &'a Context,
    reps: u32,
    firmware: &Path,
) -> Result<Vec<FlakeUnit<'a>>, String> {
    let mut units = host_units(ctx, reps, firmware)?;
    units.extend(qemu_units(ctx));
    units.extend(wasm_units(ctx, reps));
    units.extend(fuzz_units(ctx, reps));
    units.extend(proptest_units(ctx, reps));
    Ok(units)
}

/// The host `cargo test` matrix — the workspace pass and every host lane, the
/// scene tier `ci` leaves out among them — and the release-profile crypto
/// constant-time tests. Deadline-free, so every replica runs at once, but for
/// a lane that fills the machine.
fn host_units<'a>(
    ctx: &'a Context,
    reps: u32,
    firmware: &Path,
) -> Result<Vec<FlakeUnit<'a>>, String> {
    let budget = reps as usize;
    let firmware = firmware.to_path_buf();
    // Each replica starts in an order of its own, so the hunt covers test
    // *ordering* as well as timing: a suite that only passes in the harness's
    // alphabetical order is a flake like any other. The seed is in the
    // replica's label, so a hit replays.
    let named = firmware.clone();
    let mut units = vec![FlakeUnit::new("host cargo test", budget, move |replica| {
        let order = seed::job_seed(None, replica);
        let cmd = super::host_command(ctx, super::HostRun::Workspace, &[], Some(order), &named);
        // This one job compiles *and* runs every host-testable crate in
        // the workspace, so from a cold `target/` it legitimately runs far
        // longer than an ordinary step. It gets the long budget so a slow
        // machine is never mistaken for a hung one, while a genuine hang
        // is still stopped rather than left for an operator to notice.
        Job::new(format!("host cargo test (order seed {order})"), cmd)
            .with_budget(crate::LONG_BUILD_COMMAND_TIMEOUT)
            .accounted(Account::Everything)
    })];
    let named = firmware.clone();
    units.push(FlakeUnit::new("host doctests", budget, move |replica| {
        let order = seed::job_seed(None, replica);
        let cmd = super::host_command(ctx, super::HostRun::Doctests, &[], Some(order), &named);
        Job::new(format!("host doctests (order seed {order})"), cmd)
            .with_budget(crate::LONG_BUILD_COMMAND_TIMEOUT)
            .accounted(Account::Everything)
    }));
    for lane in super::HostLane::ALL {
        let selection = lane.selection(ctx)?;
        let label = format!("host cargo test ({})", lane.label());
        let concurrency = if lane.fills_the_machine() { 1 } else { budget };
        let named = firmware.clone();
        units.push(FlakeUnit::new(label.clone(), concurrency, move |replica| {
            let order = seed::job_seed(None, replica);
            let run = super::HostRun::Lane(&selection);
            let cmd = super::host_command(ctx, run, &[], Some(order), &named);
            Job::new(format!("{label} (order seed {order})"), cmd)
                .with_budget(lane.budget())
                .accounted(Account::Everything)
        }));
    }
    units.push(FlakeUnit::new(
        "crypto constant-time (release)",
        budget,
        move |_| super::crypto_constant_time_job(ctx),
    ));
    Ok(units)
}

/// One unit per QEMU integration test, using the same host-capacity admission
/// as the pull-request matrix: uniprocessor replicas charge two units and SMP
/// replicas the whole budget (running alone), against the one-third-of-logical
/// -CPUs headroom budget, so no guest is starved of TCG time (see the module
/// docs).
/// One unit per wasm32 vertical: its harness against the module
/// [`super::wasm_tests::prepare`] built.
fn wasm_units(ctx: &Context, reps: u32) -> Vec<FlakeUnit<'_>> {
    super::wasm_tests::VERTICALS
        .iter()
        .map(|vertical| {
            let label = format!("wasm32 {}", vertical.package);
            FlakeUnit::new(label.clone(), reps as usize, move |replica| {
                Job::new(
                    format!("{label} (replica {replica})"),
                    super::wasm_tests::harness_command(ctx, vertical),
                )
            })
        })
        .collect()
}

fn qemu_units(ctx: &Context) -> Vec<FlakeUnit<'_>> {
    let budget = qemu_tests::qemu_host_budget();
    qemu_tests::enrolments()
        .into_iter()
        .map(|enrol| {
            let target_dir = ctx.target_dir();
            let children = std::sync::Arc::clone(&ctx.children);
            let label = format!("qemu {}", enrol.package);
            let job_label = label.clone();
            let weight = qemu_tests::qemu_job_weight(enrol.cpus, budget);
            // `replica` is the hunt's own repetition index. The concurrent
            // batch runs these simultaneously and each re-plants its guest's
            // backing image, so it has to reach the run: without it every
            // replica would plant and report to one path and rewrite a live
            // sibling's disk underneath it (`qemu_tests::sidecar_path`).
            FlakeUnit::new(label, budget, move |replica| {
                let target_dir = target_dir.clone();
                let children = std::sync::Arc::clone(&children);
                let job_label = job_label.clone();
                // Resolve this enrolment's memoised (`'static`) per-arch
                // `/System`-store bundle set off the job closure so the
                // `'static` closure captures the resolved `StoreSet` by
                // value; `build_all` pre-warmed it, so this is a lookup, and
                // a memoised composition failure fails the job closed.
                let stores = enrol.stores(ctx);
                Job::closure(job_label, weight, move || {
                    enrol.run(&children, &target_dir, replica, &stores.clone()?)
                })
            })
        })
        .collect()
}

/// One unit per fuzz harness, each running its single smoke iteration (as
/// `fuzz --once` does) with a fresh seed per repetition. Deadline-free, so
/// every replica runs at once.
fn fuzz_units(ctx: &Context, reps: u32) -> Vec<FlakeUnit<'_>> {
    let budget = reps as usize;
    fuzz::TARGETS
        .iter()
        .map(|target| {
            FlakeUnit::new(format!("fuzz {}", target.test), budget, move |index| {
                fuzz::job_for(ctx, target, None, None, index)
            })
        })
        .collect()
}

/// One unit per proptest model, each running its single smoke iteration (as
/// `proptest --once` does) with a fresh seed per repetition. Deadline-free, so
/// every replica runs at once.
fn proptest_units(ctx: &Context, reps: u32) -> Vec<FlakeUnit<'_>> {
    let budget = reps as usize;
    proptest::MODELS
        .iter()
        .map(|model| {
            FlakeUnit::new(format!("proptest {}", model.name), budget, move |index| {
                proptest::job_for(ctx, model, None, None, index)
            })
        })
        .collect()
}

/// Print the planned hunt without running anything (`ci-long --dry-run`): the
/// test set and the per-test repetition counts, so an operator can see the
/// shape and cost of a run before committing a long-runner to it.
///
/// # Errors
///
/// As [`all_units`], or where stdout cannot take the plan.
pub(crate) fn print_plan(ctx: &Context) -> Result<(), String> {
    let once: Vec<&str> = super::ci_long_plan()
        .filter_map(|step| match step {
            super::LongStep::Once(ci_stage) => Some(ci_stage.label()),
            super::LongStep::FlakeHunt => None,
        })
        .collect();
    outln!("ci-long plan: once, in ci's order: {}", once.join(", "))?;
    let units = all_units(ctx, REPS, &super::test_firmware_dir(ctx)?)?;
    outln!(
        "then {} test(s), each run {REPS}x sequential then {REPS}x concurrent:",
        units.len()
    )?;
    for unit in &units {
        let alone = if unit.concurrency_budget == 1 {
            " (its concurrent replicas one at a time)"
        } else {
            ""
        };
        outln!("  - {}{alone}", unit.label)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{all_units, flake_hunt, FlakeUnit, REPS};
    use crate::commands::libtest::Account;
    use crate::commands::parallel::{run_unlogged, Job};
    use crate::Context;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A counting unit whose factory records how many jobs it built and, via
    /// each job's own closure, how many actually ran.
    fn counting_unit(
        built: &AtomicUsize,
        ran: Arc<AtomicUsize>,
        fail_on_run: Option<usize>,
    ) -> FlakeUnit<'_> {
        FlakeUnit::new("counter", 8, move |_| {
            built.fetch_add(1, Ordering::SeqCst);
            let ran = Arc::clone(&ran);
            Job::closure("counter", 1, move || {
                let n = ran.fetch_add(1, Ordering::SeqCst);
                match fail_on_run {
                    Some(fail_at) if n == fail_at => Err("boom".to_string()),
                    _ => Ok(()),
                }
            })
        })
    }

    #[test]
    fn reps_is_the_documented_twenty() {
        assert_eq!(REPS, 20);
    }

    #[test]
    fn flake_hunt_runs_each_unit_reps_times_sequentially_then_concurrently() {
        let built = AtomicUsize::new(0);
        let ran = Arc::new(AtomicUsize::new(0));
        let unit = counting_unit(&built, Arc::clone(&ran), None);
        flake_hunt(vec![unit], 3, run_unlogged).expect("all passes succeed");
        // 3 sequential + 3 concurrent = 6 jobs built and run.
        assert_eq!(built.load(Ordering::SeqCst), 6);
        assert_eq!(ran.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn flake_hunt_fails_closed_and_stops_on_a_sequential_failure() {
        let built = AtomicUsize::new(0);
        let ran = Arc::new(AtomicUsize::new(0));
        // Fail the second sequential run (0-based index 1).
        let unit = counting_unit(&built, Arc::clone(&ran), Some(1));
        let err =
            flake_hunt(vec![unit], 4, run_unlogged).expect_err("a failing pass must fail the hunt");
        assert!(
            err.contains("sequential pass 2/4"),
            "unexpected error: {err}"
        );
        // Stopped after the failing pass: the third/fourth sequential passes
        // and the whole concurrent phase never ran.
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn flake_hunt_treats_zero_reps_as_one() {
        let built = AtomicUsize::new(0);
        let ran = Arc::new(AtomicUsize::new(0));
        let unit = counting_unit(&built, Arc::clone(&ran), None);
        flake_hunt(vec![unit], 0, run_unlogged).expect("zero clamps to one");
        // 1 sequential + 1 concurrent.
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }

    /// Every libtest replica is held to its account; a wasm32 harness, which
    /// libtest does not run, decides its own verdict, as a QEMU guest does,
    /// whose job is not built here because that resolves its whole store.
    #[test]
    fn every_libtest_replica_is_held_to_its_account() {
        let ctx = Context::discover(None).expect("workspace context");
        let units =
            all_units(&ctx, 2, std::path::Path::new("/staged/pi-firmware")).expect("the units");
        for unit in units.iter().filter(|unit| !unit.label.starts_with("qemu ")) {
            let account = (unit.factory)(0).account();
            if unit.label.starts_with("wasm32 ") {
                assert_eq!(account, None, "{}", unit.label);
            } else if unit.label.starts_with("fuzz ") || unit.label.starts_with("proptest ") {
                assert!(account.is_some(), "{}", unit.label);
            } else {
                assert_eq!(account, Some(Account::Everything), "{}", unit.label);
            }
        }
    }

    /// Every test source is hunted: each host lane, the scene tier `ci` leaves
    /// out among them, its replicas one at a time because one run fills the
    /// machine.
    #[test]
    fn all_units_covers_every_test_source() {
        let ctx = Context::discover(None).expect("workspace context");
        let units =
            all_units(&ctx, 2, std::path::Path::new("/staged/pi-firmware")).expect("the units");
        let labels: Vec<&str> = units.iter().map(|u| u.label.as_str()).collect();
        assert!(labels.contains(&"host cargo test"));
        assert!(labels.contains(&"host doctests"));
        assert_eq!(
            labels.iter().filter(|l| l.starts_with("wasm32 ")).count(),
            crate::commands::wasm_tests::VERTICALS.len()
        );
        assert!(labels.contains(&"host cargo test (debug-image kernel diagnostics)"));
        let budget = |label: &str| {
            units
                .iter()
                .find(|u| u.label == label)
                .map(|u| u.concurrency_budget)
        };
        assert_eq!(budget("host cargo test (scene tier)"), Some(1));
        assert_eq!(
            budget("host cargo test (debug-image kernel diagnostics)"),
            Some(2)
        );
        assert!(labels.contains(&"crypto constant-time (release)"));
        // Every fuzz harness and proptest model is its own unit.
        assert_eq!(
            labels.iter().filter(|l| l.starts_with("fuzz ")).count(),
            crate::commands::fuzz::TARGETS.len()
        );
        assert_eq!(
            labels.iter().filter(|l| l.starts_with("proptest ")).count(),
            crate::commands::proptest::MODELS.len()
        );
        // At least one QEMU enrolment is present.
        assert!(labels.iter().any(|l| l.starts_with("qemu ")));
    }
}
