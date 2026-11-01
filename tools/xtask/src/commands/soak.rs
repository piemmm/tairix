//! How long a soaking job runs: the one budget the fuzz, proptest, fssoak and
//! rngsoak orchestrators and the repeated test matrix parse, hand their
//! harnesses, and bound their children by, and the one job each harness soaks
//! in ([`job`]).
//!
//! A budget is either a span each job gets from the moment its own run begins
//! (`--quick`, `--soak`, `--secs N`), or the close of a window (`--until T`):
//! whatever a job spends building, or queued behind the build lock, comes out
//! of the window rather than being added after it. One invocation soaks one
//! target to a window's close ([`one_target_per_window`]);
//! `tools/ci/soak.sh --window` hands every target a process of its own.

use std::ffi::OsString;
use std::process::Command;
use std::slice;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tairix_fuzzseed::SOAK_UNTIL_ENV;

use crate::commands::parallel::Job;
use crate::Context;

/// What `--quick` gives each job: the per-pull-request floor.
pub(crate) const QUICK: Duration = Duration::from_secs(5);

/// What `--soak` gives each job: the nightly floor.
pub(crate) const NIGHTLY: Duration = Duration::from_hours(24);

/// The budget a mode flag names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    /// `--once`: one smoke iteration per job with a fresh, logged seed — the
    /// per-pull-request `ci` gate.
    Once,
    /// `--quick`: [`QUICK`] per job.
    Quick,
    /// `--soak`: [`NIGHTLY`] per job.
    Soak,
}

impl Mode {
    const fn flag(self) -> &'static str {
        match self {
            Self::Once => "--once",
            Self::Quick => "--quick",
            Self::Soak => "--soak",
        }
    }

    /// The span the mode gives each job; `None` for one smoke iteration.
    #[must_use]
    pub(crate) const fn span(self) -> Option<Duration> {
        match self {
            Self::Once => None,
            Self::Quick => Some(QUICK),
            Self::Soak => Some(NIGHTLY),
        }
    }
}

/// When a soaking job stops.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Budget {
    /// This long after the job's own run begins, so each job gets all of it
    /// however long it waited to be built.
    For(Duration),
    /// At the close of a window: a harness stops soaking there however long
    /// its build took, and one built after it makes its one smoke iteration.
    Until(SystemTime),
}

impl Budget {
    /// What is left of the budget now.
    #[must_use]
    pub(crate) fn left(self) -> Duration {
        match self {
            Self::For(span) => span,
            Self::Until(close) => close
                .duration_since(SystemTime::now())
                .unwrap_or(Duration::ZERO),
        }
    }

    /// When a run beginning now must end; `None` past the monotonic clock's
    /// range.
    #[must_use]
    pub(crate) fn deadline(self) -> Option<Instant> {
        Instant::now().checked_add(self.left())
    }

    /// Hand the budget to a harness through its family's `budget_env`, with
    /// the window's close beside it so a harness that waited to be built still
    /// stops there. A span clears any close the environment carries, which
    /// would otherwise cut it short.
    fn export(self, cmd: &mut Command, budget_env: &str) {
        cmd.env(budget_env, self.left().as_secs().to_string());
        match self {
            Self::For(_) => cmd.env_remove(SOAK_UNTIL_ENV),
            Self::Until(close) => cmd.env(SOAK_UNTIL_ENV, unix_secs(close).to_string()),
        };
    }
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// How a job's label states `budget`.
#[must_use]
pub(crate) fn describe(budget: Option<Budget>) -> String {
    match budget {
        None => "1 iteration".to_string(),
        Some(Budget::For(span)) => format!("{} s", span.as_secs()),
        Some(window @ Budget::Until(_)) => {
            format!("{} s to the window's close", window.left().as_secs())
        }
    }
}

/// How long a job's child may run before it is killed as hung: what is left
/// of its budget and `allowance` past it, or the allowance alone for one smoke
/// iteration. The allowance is an ordinary step's, which the operator's floor
/// (`TAIRIX_XTASK_TIMEOUT_SECS`) raises, and it covers the child's own build
/// and its wait behind the build lock.
///
/// A soaking child is doing what it was asked right up to the end of its
/// budget, so a deadline any shorter kills working soaks and reports them as
/// hangs.
fn child_deadline(budget: Option<Budget>, allowance: Duration) -> Duration {
    budget.map_or(allowance, |budget| budget.left().saturating_add(allowance))
}

/// One soak harness: the test binary a job runs and the variable its budget
/// reaches it through.
pub(crate) struct Harness<'a> {
    /// The package the test binary belongs to.
    pub(crate) package: &'a str,
    /// The integration-test binary.
    pub(crate) test: &'a str,
    /// The binary's own arguments ahead of `--nocapture`, such as
    /// `--exact NAME`.
    pub(crate) filter: &'a [&'a str],
    /// The variable the budget is exported through.
    pub(crate) budget_env: &'a str,
}

/// The [`Job`] that soaks `harness` for `budget`, with `env` set, killed as
/// hung an ordinary step's allowance after its budget ([`child_deadline`]).
#[must_use]
pub(crate) fn job(
    ctx: &Context,
    label: String,
    harness: &Harness<'_>,
    budget: Option<Budget>,
    env: &[(&str, String)],
) -> Job {
    let mut cmd = ctx.cargo();
    cmd.args([
        "test",
        "-p",
        harness.package,
        "--test",
        harness.test,
        "--locked",
        "--",
    ]);
    cmd.args(harness.filter);
    cmd.arg("--nocapture");
    if let Some(budget) = budget {
        budget.export(&mut cmd, harness.budget_env);
    }
    cmd.envs(env.iter().map(|(key, value)| (key, value)));
    Job::new(label, cmd).with_budget(child_deadline(budget, ctx.children.ordinary_budget()))
}

/// Refuse a window over more than one target: one invocation starts its
/// targets in turn or as slots free, so a later one reaches the window shut,
/// makes its one smoke iteration and passes.
///
/// # Errors
///
/// A `--until` budget over `targets` above one, naming the remedy.
pub(crate) fn one_target_per_window(
    cmd: &str,
    budget: Option<Budget>,
    targets: usize,
) -> Result<(), String> {
    match budget {
        Some(Budget::Until(_)) if targets > 1 => Err(format!(
            "{cmd}: `--until` soaks one target to the window's close, not {targets}; \
             name it with `--target`, or run `tools/ci/soak.sh --window`"
        )),
        _ => Ok(()),
    }
}

/// The budget flags of one invocation, gathered as its parser meets them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Flags {
    mode: Option<Mode>,
    secs: Option<Duration>,
    until: Option<SystemTime>,
}

impl Flags {
    /// Take `flag` if it is `--quick`, `--soak`, or an override
    /// ([`Self::take_override`]); `Ok(false)` leaves it to the caller.
    ///
    /// # Errors
    /// A mode other than one already chosen, or an override's bad value.
    pub(crate) fn take(
        &mut self,
        cmd: &str,
        flag: &str,
        rest: &mut slice::Iter<'_, OsString>,
    ) -> Result<bool, String> {
        match flag {
            "--quick" => self.set_mode(cmd, Mode::Quick)?,
            "--soak" => self.set_mode(cmd, Mode::Soak)?,
            _ => return self.take_override(cmd, flag, rest),
        }
        Ok(true)
    }

    /// Take `--secs N` or `--until T`, drawing the value from `rest`;
    /// `Ok(false)` leaves any other flag to the caller.
    ///
    /// # Errors
    /// A missing or unreadable value, or one past the clock's range.
    pub(crate) fn take_override(
        &mut self,
        cmd: &str,
        flag: &str,
        rest: &mut slice::Iter<'_, OsString>,
    ) -> Result<bool, String> {
        match flag {
            "--secs" => self.secs = Some(parse_secs(cmd, rest.next())?),
            "--until" => self.until = Some(parse_until(cmd, rest.next())?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Choose `mode`, refusing a different one already chosen.
    ///
    /// # Errors
    /// A second, different mode.
    pub(crate) fn set_mode(&mut self, cmd: &str, mode: Mode) -> Result<(), String> {
        match self.mode {
            Some(chosen) if chosen != mode => Err(format!(
                "{cmd}: `{}` and `{}` are mutually exclusive",
                chosen.flag(),
                mode.flag()
            )),
            _ => {
                self.mode = Some(mode);
                Ok(())
            }
        }
    }

    /// Whether any budget flag was given.
    #[must_use]
    pub(crate) fn given(self) -> bool {
        self != Self::default()
    }

    /// The budget the flags name, `default` standing in for an absent mode:
    /// an override replaces the mode's own span, and a zero `--secs` is one
    /// smoke iteration.
    ///
    /// # Errors
    /// `--secs` and `--until` together, which name two different ends.
    pub(crate) fn budget(self, cmd: &str, default: Mode) -> Result<Option<Budget>, String> {
        match (self.secs, self.until) {
            (Some(_), Some(_)) => Err(format!(
                "{cmd}: `--secs` and `--until` are mutually exclusive"
            )),
            (None, Some(close)) => Ok(Some(Budget::Until(close))),
            (Some(span), None) => Ok((!span.is_zero()).then_some(Budget::For(span))),
            (None, None) => Ok(self.mode.unwrap_or(default).span().map(Budget::For)),
        }
    }
}

/// Read the whole number `flag` carries.
///
/// # Errors
/// A missing value, or one that is not a `u64`.
pub(crate) fn parse_u64(cmd: &str, flag: &str, value: Option<&OsString>) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("{cmd}: `{flag}` requires a number"))?;
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| format!("{cmd}: `{flag}` expects a u64, got {}", value.display()))
}

/// Read `--secs`: a span the monotonic clock can reach from now.
fn parse_secs(cmd: &str, value: Option<&OsString>) -> Result<Duration, String> {
    let span = Duration::from_secs(parse_u64(cmd, "--secs", value)?);
    match Instant::now().checked_add(span) {
        Some(_) => Ok(span),
        None => Err(format!(
            "{cmd}: `--secs` {} is past the clock's range",
            span.as_secs()
        )),
    }
}

/// Read `--until`: whole seconds since the Unix epoch. The close may already
/// have passed, leaving each job the least it can do: one smoke iteration, or
/// one pass.
fn parse_until(cmd: &str, value: Option<&OsString>) -> Result<SystemTime, String> {
    let secs = parse_u64(cmd, "--until", value)?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .filter(|close| Budget::Until(*close).deadline().is_some())
        .ok_or_else(|| format!("{cmd}: `--until` {secs} is past the clock's range"))
}

#[cfg(test)]
mod tests {
    use super::{
        child_deadline, describe, job, one_target_per_window, Budget, Flags, Harness, Mode,
        NIGHTLY, QUICK,
    };
    use crate::children::Children;
    use crate::commands::parallel;
    use crate::{Context, DEFAULT_COMMAND_TIMEOUT};
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tairix_fuzzseed::SOAK_UNTIL_ENV;

    const CMD: &str = "soak-test";

    fn parse(args: &[&str]) -> Result<Flags, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut flags = Flags::default();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            let flag = arg.to_str().expect("test flags are UTF-8");
            if !flags.take(CMD, flag, &mut rest)? {
                return Err(format!("unexpected {flag}"));
            }
        }
        Ok(flags)
    }

    fn budget(args: &[&str]) -> Result<Option<Budget>, String> {
        parse(args)?.budget(CMD, Mode::Quick)
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn the_floors_meet_the_charter() {
        assert!(QUICK >= Duration::from_secs(5));
        assert!(NIGHTLY >= Duration::from_hours(24));
    }

    #[test]
    fn each_mode_names_its_floor() {
        assert_eq!(budget(&[]), Ok(Some(Budget::For(QUICK))));
        assert_eq!(budget(&["--soak"]), Ok(Some(Budget::For(NIGHTLY))));
        let mut once = Flags::default();
        once.set_mode(CMD, Mode::Once).expect("first mode");
        assert_eq!(once.budget(CMD, Mode::Quick), Ok(None));
    }

    #[test]
    fn an_override_replaces_the_mode() {
        assert_eq!(
            budget(&["--soak", "--secs", "3"]),
            Ok(Some(Budget::For(Duration::from_secs(3))))
        );
        assert_eq!(budget(&["--secs", "0"]), Ok(None));
        assert_eq!(
            budget(&["--soak", "--until", "1700000000"]),
            Ok(Some(Budget::Until(at(1_700_000_000))))
        );
    }

    #[test]
    fn two_modes_or_two_ends_are_refused() {
        assert!(parse(&["--quick", "--soak"]).is_err());
        assert!(parse(&["--quick", "--quick"]).is_ok());
        assert!(budget(&["--secs", "3", "--until", "1700000000"]).is_err());
    }

    #[test]
    fn a_value_that_is_not_a_count_of_seconds_is_refused() {
        for args in [
            &["--secs"][..],
            &["--secs", "soon"],
            &["--until"],
            &["--until", "-1"],
            &["--until", "1.5"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn a_value_past_the_clock_is_refused() {
        assert!(parse(&["--secs", "18446744073709551615"]).is_err());
        assert!(parse(&["--until", "18446744073709551615"]).is_err());
    }

    #[test]
    fn another_flag_is_left_to_the_caller() {
        let mut flags = Flags::default();
        let rest: Vec<OsString> = Vec::new();
        assert_eq!(flags.take(CMD, "--target", &mut rest.iter()), Ok(false));
        assert!(!flags.given());
        assert!(parse(&["--soak"]).expect("parses").given());
    }

    const HOUR: Duration = Duration::from_hours(1);
    const MINUTE_SHORT_OF_AN_HOUR: Duration = Duration::from_mins(59);

    #[test]
    fn a_window_leaves_what_remains_of_it() {
        assert_eq!(Budget::Until(at(1)).left(), Duration::ZERO);
        let left = Budget::Until(SystemTime::now() + HOUR).left();
        assert!(left <= HOUR && left > MINUTE_SHORT_OF_AN_HOUR);
    }

    #[test]
    fn a_soaking_child_outlives_its_budget_by_an_ordinary_step() {
        for allowance in [DEFAULT_COMMAND_TIMEOUT, Duration::from_hours(2)] {
            assert_eq!(child_deadline(None, allowance), allowance);
            for span in [QUICK, Duration::from_hours(7), NIGHTLY] {
                assert_eq!(
                    child_deadline(Some(Budget::For(span)), allowance),
                    span + allowance
                );
            }
            assert_eq!(
                child_deadline(Some(Budget::For(Duration::MAX)), allowance),
                Duration::MAX
            );
        }
    }

    #[test]
    fn a_windowed_child_is_killed_an_ordinary_step_after_the_close() {
        // A child that sat behind the build lock for most of the window is
        // still given only until the close, not a whole budget after it.
        let allowance = DEFAULT_COMMAND_TIMEOUT;
        let deadline = child_deadline(Some(Budget::Until(SystemTime::now() + HOUR)), allowance);
        assert!(deadline <= HOUR + allowance);
        assert!(deadline > MINUTE_SHORT_OF_AN_HOUR + allowance);
        assert_eq!(
            child_deadline(Some(Budget::Until(at(1))), allowance),
            allowance
        );
    }

    /// The operator's floor reaches a soaking child: a soak's allowance past
    /// its budget is the raised ordinary step, so a slow host's floor is not
    /// lost on the soaks it most applies to.
    #[test]
    fn a_soaking_child_takes_the_operators_floor_as_its_allowance() {
        let floor = Duration::from_hours(2);
        let ctx = Context {
            workspace_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            cargo: OsString::from("true"),
            children: Arc::new(Children::new(Some(floor))),
        };
        let harness = Harness {
            package: "soaked",
            test: "soaked",
            filter: &[],
            budget_env: "TAIRIX_SOAK_TEST_BUDGET",
        };
        let soaked = job(
            &ctx,
            "soaked".to_string(),
            &harness,
            Some(Budget::For(NIGHTLY)),
            &[],
        );
        let (outcome, log) = parallel::run_recorded(vec![soaked], 1, &ctx.children);
        assert_eq!(outcome, Ok(()), "{log}");
        let expected = (NIGHTLY + floor).as_secs();
        assert!(
            log.contains(&format!("(timeout {expected}s)")),
            "a {NIGHTLY:?} soak under a {floor:?} floor is given {expected} s: {log}"
        );
    }

    #[test]
    fn a_window_soaks_one_target_and_a_span_any_number() {
        let window = Some(Budget::Until(SystemTime::now() + HOUR));
        assert_eq!(one_target_per_window("soak-test", window, 1), Ok(()));
        let refused = one_target_per_window("soak-test", window, 2);
        assert!(refused.is_err_and(|why| why.contains("--target")));
        for budget in [None, Some(Budget::For(QUICK))] {
            assert_eq!(one_target_per_window("soak-test", budget, 130), Ok(()));
        }
    }

    fn exported(budget: Budget) -> Vec<(String, Option<String>)> {
        let mut cmd = Command::new("true");
        budget.export(&mut cmd, "TAIRIX_SOAK_TEST_BUDGET");
        cmd.get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn a_span_reaches_the_harness_with_no_close() {
        let env = exported(Budget::For(Duration::from_secs(5)));
        assert!(env.contains(&("TAIRIX_SOAK_TEST_BUDGET".into(), Some("5".into()))));
        assert!(env.contains(&(SOAK_UNTIL_ENV.into(), None)));
    }

    #[test]
    fn a_window_reaches_the_harness_with_its_close() {
        let close = at(4_102_444_800);
        let env = exported(Budget::Until(close));
        assert!(env.contains(&(SOAK_UNTIL_ENV.into(), Some("4102444800".into()))));
        let budget = env
            .iter()
            .find(|(key, _)| key == "TAIRIX_SOAK_TEST_BUDGET")
            .and_then(|(_, value)| value.as_deref()?.parse::<u64>().ok())
            .expect("the budget is exported");
        assert!(budget > 0, "an open window soaks");
    }

    #[test]
    fn a_label_states_what_the_job_was_given() {
        assert_eq!(describe(None), "1 iteration");
        assert_eq!(describe(Some(Budget::For(QUICK))), "5 s");
        assert_eq!(
            describe(Some(Budget::Until(at(1)))),
            "0 s to the window's close"
        );
    }

    /// A file under the workspace root.
    fn workspace_file(path: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask sits two levels below the workspace root");
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|_| panic!("{path} is readable"))
    }

    /// The scheduled window `tools/ci/soak.sh` defines, once.
    fn scheduled_window() -> Duration {
        let script = workspace_file("tools/ci/soak.sh");
        let mut values = script
            .lines()
            .filter_map(|line| line.strip_prefix("scheduled_window="))
            .map(|value| value.parse::<u64>().expect("whole seconds"));
        let window = values.next().expect("soak.sh defines the scheduled window");
        assert!(values.next().is_none(), "soak.sh defines it once");
        Duration::from_secs(window)
    }

    /// The nightly workflow's job timeout is only a backstop: it has to outlast
    /// the window and the ordinary step a wedged job is given past its close,
    /// or GitHub cancels the job before xtask can kill the wedged one and name
    /// it — the report a soak exists to produce.
    #[test]
    fn the_nightly_workflow_outlasts_its_window_and_the_wind_down() {
        let yaml = workspace_file(".github/workflows/soak.yml");
        let mut timeouts = yaml
            .lines()
            .filter_map(|line| line.trim().strip_prefix("timeout-minutes:"))
            .map(|value| value.trim().parse::<u64>().expect("a whole number"));
        let timeout = Duration::from_secs(timeouts.next().expect("soak.yml sets a timeout") * 60);
        assert!(timeouts.next().is_none(), "soak.yml sets one timeout");
        assert!(yaml.contains("tools/ci/soak.sh \"$KIND\" --scheduled"));
        let window = scheduled_window();
        assert!(
            timeout > window + DEFAULT_COMMAND_TIMEOUT,
            "a {timeout:?} timeout cannot cover a {window:?} window and the \
             {DEFAULT_COMMAND_TIMEOUT:?} a wedged job is given past its close"
        );
    }

    /// The soak lines of the three scheduler samples, and their gate's.
    fn sample_lines() -> [(&'static str, String, String); 3] {
        let systemd = |unit: &str| {
            workspace_file(&format!("tools/ci/systemd/{unit}"))
                .lines()
                .find_map(|line| line.strip_prefix("ExecStart=").map(str::to_string))
                .unwrap_or_else(|| panic!("{unit} runs something"))
        };
        let launchd = |agent: &str| {
            let plist = workspace_file(&format!("tools/ci/launchd/{agent}"));
            let (_, arguments) = plist
                .split_once("<key>ProgramArguments</key>")
                .unwrap_or_else(|| panic!("{agent} runs something"));
            let (arguments, _) = arguments.split_once("</array>").expect("its arguments end");
            arguments
                .split("<string>")
                .skip(1)
                .filter_map(|tail| tail.split_once("</string>"))
                .map(|(argument, _)| argument)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let crontab = workspace_file("tools/ci/crontab.sample");
        let setting = |key: &str| {
            crontab
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| panic!("crontab.sample sets {key}"))
                .to_string()
        };
        let job = |script: &str| {
            crontab
                .lines()
                .filter(|line| !line.starts_with('#'))
                .find(|line| line.contains(script))
                .unwrap_or_else(|| panic!("crontab.sample runs {script}"))
                .replace("$SOAK_REPO", &setting("SOAK_REPO="))
                .replace("$REPO", &setting("REPO="))
        };
        [
            (
                "systemd",
                systemd("tairix-soak.service"),
                systemd("tairix-ci.service"),
            ),
            (
                "launchd",
                launchd("com.tairix.soak.plist.sample"),
                launchd("com.tairix.ci.plist.sample"),
            ),
            ("cron", job("tools/ci/soak.sh"), job("ci-run.sh ci")),
        ]
    }

    /// Every standalone scheduler runs the scheduled window, which ends inside
    /// the day the next run starts.
    #[test]
    fn the_daily_schedulers_run_the_scheduled_window() {
        for (scheduler, soak, _) in sample_lines() {
            assert!(
                soak.contains("tools/ci/soak.sh all --scheduled"),
                "{scheduler}: {soak}"
            );
        }
        assert!(scheduled_window() + DEFAULT_COMMAND_TIMEOUT < Duration::from_hours(24));
    }

    /// A scheduled soak runs from a checkout of its own: the gate resets its
    /// checkout every 30 minutes, which would change the tree under the soak.
    #[test]
    fn the_scheduled_soak_runs_from_a_checkout_of_its_own() {
        for (scheduler, soak, gate) in sample_lines() {
            let checkout = |line: &str| {
                let (path, _) = line
                    .split_once("/tools/ci/")
                    .unwrap_or_else(|| panic!("{scheduler}: {line} runs a CI script"));
                path.split_whitespace().last().unwrap_or(path).to_string()
            };
            assert_ne!(checkout(&soak), checkout(&gate), "{scheduler}");
        }
    }

    /// The nightly workflow's dispatch offers every kind `soak.sh` takes.
    #[test]
    fn the_dispatch_offers_every_kind_soak_sh_takes() {
        let script = workspace_file("tools/ci/soak.sh");
        let kinds: BTreeSet<&str> = script
            .lines()
            .filter_map(|line| line.trim().strip_suffix(") kind=\"$1\" ;;"))
            .flat_map(|arm| arm.split('|').map(str::trim))
            .collect();
        let yaml = workspace_file(".github/workflows/soak.yml");
        let offered: BTreeSet<&str> = yaml
            .lines()
            .find_map(|line| line.trim().strip_prefix("options: ["))
            .and_then(|options| options.strip_suffix(']'))
            .expect("the dispatch offers kinds")
            .split(',')
            .map(str::trim)
            .collect();
        assert!(!kinds.is_empty());
        assert_eq!(offered, kinds);
    }
}
