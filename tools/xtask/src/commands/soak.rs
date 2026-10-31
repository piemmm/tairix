//! How long a soaking job runs: the one budget the fuzz, proptest, fssoak and
//! rngsoak orchestrators and the repeated test matrix parse, hand their
//! harnesses, and bound their children by.
//!
//! A budget is either a span each job gets from the moment its own run begins
//! (`--quick`, `--soak`, `--secs N`), or the close of a window every job
//! shares (`--until T`). The window is what gives `tools/ci/soak.sh --window`
//! a fixed length: whatever a job spends building, or queued behind the build
//! lock, comes out of the window rather than being added after it.

use std::ffi::OsString;
use std::process::Command;
use std::slice;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tairix_fuzzseed::SOAK_UNTIL_ENV;

use crate::DEFAULT_COMMAND_TIMEOUT;

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
    /// At the close of a window every job shares, so each is finished by then
    /// whatever its build cost.
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
    pub(crate) fn export(self, cmd: &mut Command, budget_env: &str) {
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
/// of its budget and an ordinary step's allowance past it, or the allowance
/// alone for one smoke iteration.
///
/// A soaking child is doing what it was asked right up to the end of its
/// budget, so a deadline any shorter kills working soaks and reports them as
/// hangs.
#[must_use]
pub(crate) fn child_deadline(budget: Option<Budget>) -> Duration {
    budget.map_or(DEFAULT_COMMAND_TIMEOUT, |budget| {
        budget.left().saturating_add(DEFAULT_COMMAND_TIMEOUT)
    })
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
    use super::{child_deadline, describe, Budget, Flags, Mode, NIGHTLY, QUICK};
    use crate::DEFAULT_COMMAND_TIMEOUT;
    use std::ffi::OsString;
    use std::path::Path;
    use std::process::Command;
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
        assert_eq!(child_deadline(None), DEFAULT_COMMAND_TIMEOUT);
        for span in [QUICK, Duration::from_hours(7), NIGHTLY] {
            assert_eq!(
                child_deadline(Some(Budget::For(span))),
                span + DEFAULT_COMMAND_TIMEOUT
            );
        }
        assert_eq!(
            child_deadline(Some(Budget::For(Duration::MAX))),
            Duration::MAX
        );
    }

    #[test]
    fn a_windowed_child_is_killed_an_ordinary_step_after_the_close() {
        // A child that sat behind the build lock for most of the window is
        // still given only until the close, not a whole budget after it.
        let deadline = child_deadline(Some(Budget::Until(SystemTime::now() + HOUR)));
        assert!(deadline <= HOUR + DEFAULT_COMMAND_TIMEOUT);
        assert!(deadline > MINUTE_SHORT_OF_AN_HOUR + DEFAULT_COMMAND_TIMEOUT);
        assert_eq!(
            child_deadline(Some(Budget::Until(at(1)))),
            DEFAULT_COMMAND_TIMEOUT
        );
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

    /// The nightly workflow's job timeout is only a backstop: it has to outlast
    /// the window and the ordinary step a wedged job is given past its close,
    /// or GitHub cancels the job before xtask can kill the wedged one and name
    /// it — the report a soak exists to produce.
    #[test]
    fn the_nightly_workflow_outlasts_its_window_and_the_wind_down() {
        let workflow = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask sits two levels below the workspace root")
            .join(".github/workflows/soak.yml");
        let yaml = std::fs::read_to_string(&workflow).expect("soak.yml is readable");
        let setting = |key: &str| -> u64 {
            let mut values = yaml
                .lines()
                .filter_map(|line| line.trim().strip_prefix(key))
                .map(|value| value.trim().parse::<u64>().expect("a whole number"));
            let value = values
                .next()
                .unwrap_or_else(|| panic!("soak.yml sets `{key}`"));
            assert!(values.next().is_none(), "soak.yml sets `{key}` once");
            value
        };
        let window = Duration::from_secs(setting("TAIRIX_SOAK_WINDOW_SECS:"));
        let timeout = Duration::from_secs(setting("timeout-minutes:") * 60);
        assert!(
            timeout > window + DEFAULT_COMMAND_TIMEOUT,
            "a {timeout:?} timeout cannot cover a {window:?} window and the \
             {DEFAULT_COMMAND_TIMEOUT:?} a wedged job is given past its close"
        );
    }
}
