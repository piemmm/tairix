//! What a run of libtest binaries reports it ran, read back from the summary
//! each binary prints, so a job that runs tests is held to having run every
//! one it was given rather than judged by its exit status alone.

/// The opening of the line libtest starts each binary's run with:
/// `running 3 tests`.
const RUNNING: &str = "running ";

/// The opening of the summary line libtest prints as each binary ends.
const SUMMARY: &str = "test result: ";

/// The tests a run's libtest binaries report, summed over their summaries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    /// Summaries read, one per binary that ran.
    pub binaries: u64,
    /// Tests that passed.
    pub passed: u64,
    /// Tests that failed.
    pub failed: u64,
    /// Tests libtest skipped as ignored.
    pub ignored: u64,
    /// Benchmarks measured.
    pub measured: u64,
    /// Tests a filter or skip passed over.
    pub filtered: u64,
}

impl Tally {
    /// The summaries in `output`, a run's standard output, each closing the
    /// run libtest opened it with.
    ///
    /// # Errors
    ///
    /// A summary that does not read as one, opened no run, or counts other
    /// than the tests its run opened with: libtest's format has changed, or
    /// the line came from something else writing to the same output, and a
    /// count taken from either could pass a run that skipped tests. Also a
    /// run libtest opened and never closed: a binary that exits cleanly mid-run,
    /// as one whose test calls `exit(0)` does, skipped the rest of its tests.
    pub fn of(output: &str) -> Result<Self, String> {
        let mut sum = Self::default();
        let mut opened = None;
        for line in output.lines() {
            if let Some(tests) = running(line) {
                if let Some(unclosed) = opened.replace(tests) {
                    return Err(unclosed_run(unclosed));
                }
                continue;
            }
            let Some(counts) = line.strip_prefix(SUMMARY) else {
                continue;
            };
            let one = summary(counts).ok_or_else(|| {
                format!("printed a libtest summary it could not be read from: `{line}`")
            })?;
            let tests = opened
                .take()
                .ok_or_else(|| format!("printed a libtest summary no run opened: `{line}`"))?;
            if tests != one.passed + one.failed + one.ignored + one.measured {
                return Err(format!(
                    "printed a libtest summary counting other than the {tests} tests its \
                     run opened with: `{line}`"
                ));
            }
            sum = sum.plus(one);
        }
        opened.map_or(Ok(sum), |unclosed| Err(unclosed_run(unclosed)))
    }

    fn plus(self, other: Self) -> Self {
        Self {
            binaries: self.binaries + other.binaries,
            passed: self.passed + other.passed,
            failed: self.failed + other.failed,
            ignored: self.ignored + other.ignored,
            measured: self.measured + other.measured,
            filtered: self.filtered + other.filtered,
        }
    }
}

/// Whether `line` opens or closes a libtest run, the only lines a job's
/// account is settled from.
pub fn frames_a_run(line: &str) -> bool {
    line.starts_with(RUNNING) || line.starts_with(SUMMARY)
}

fn unclosed_run(tests: u64) -> String {
    format!("opened a libtest run of {tests} tests that printed no summary, so it ended early")
}

/// The tests a [`RUNNING`] line opens a binary's run with, a shuffled run's
/// seed after them: `running 1 test (shuffle seed: 42)`.
fn running(line: &str) -> Option<u64> {
    let (count, rest) = line.strip_prefix(RUNNING)?.split_once(' ')?;
    let rest = rest
        .strip_prefix("tests")
        .or_else(|| rest.strip_prefix("test"))?;
    if !rest.is_empty() && !rest.starts_with(" (") {
        return None;
    }
    count.parse().ok()
}

/// One binary's counts, from what follows [`SUMMARY`]:
/// `ok. 3 passed; 0 failed; 1 ignored; 0 measured; 2 filtered out; finished in 0.01s`.
fn summary(counts: &str) -> Option<Tally> {
    let (_, counts) = counts.split_once(". ")?;
    let mut one = Tally {
        binaries: 1,
        ..Tally::default()
    };
    let mut read = 0;
    for part in counts.split("; ") {
        let (count, what) = part.split_once(' ')?;
        let field = match what {
            "passed" => &mut one.passed,
            "failed" => &mut one.failed,
            "ignored" => &mut one.ignored,
            "measured" => &mut one.measured,
            "filtered out" => &mut one.filtered,
            _ => continue,
        };
        *field = count.parse().ok()?;
        read += 1;
    }
    (read == 5).then_some(one)
}

/// What a test job must have run, settled against its [`Tally`] once it ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Account {
    /// Every test of every binary it runs, and at least one: none ignored, none
    /// filtered out.
    Everything,
    /// Every test its own filter and skips leave, and at least one: none
    /// ignored.
    Selected,
    /// Whatever arguments a caller forwarded leave, none ignored: one filter
    /// forwarded to every step of a pass may match none of a step's tests.
    Forwarded,
    /// Exactly this many tests, none ignored: those a shard was dealt.
    Exactly(u64),
}

impl Account {
    /// Whether `tally` is the run this account requires.
    ///
    /// # Errors
    ///
    /// What the run fell short of, worded to follow the job's label.
    pub fn settle(self, tally: &Tally) -> Result<(), String> {
        if tally.binaries == 0 {
            return Err("printed no libtest summary, so what it ran cannot be counted".to_string());
        }
        if tally.ignored > 0 {
            return Err(format!(
                "ignored {} tests, where the gate runs every test",
                tally.ignored
            ));
        }
        match self {
            Self::Exactly(dealt) if tally.passed != dealt => Err(format!(
                "ran {} of the {dealt} tests dealt to it",
                tally.passed
            )),
            Self::Everything if tally.filtered > 0 => Err(format!(
                "filtered out {} tests while naming no filter",
                tally.filtered
            )),
            Self::Everything | Self::Selected if tally.passed == 0 => {
                Err("ran no test".to_string())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Account, Tally};

    const SHARD: &str = "\nrunning 3 tests\ntest a ... ok\ntest b ... ok\ntest c ... ok\n\n\
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.10s\n";

    #[test]
    fn a_run_is_counted_from_the_summary_each_binary_prints() {
        let two = format!(
            "{SHARD}\nrunning 1 test\ntest d ... ok\n\ntest result: ok. 1 passed; 0 failed; \
             0 ignored; 0 measured; 0 filtered out; finished in 0.00s\nrunning 7 tests\n\
             test result: FAILED. 1 passed; 2 failed; 4 ignored; 0 measured; 0 filtered out; \
             finished in 1.5s\n"
        );
        assert_eq!(
            Tally::of(&two),
            Ok(Tally {
                binaries: 3,
                passed: 5,
                failed: 2,
                ignored: 4,
                measured: 0,
                filtered: 9,
            })
        );
        assert_eq!(
            Tally::of(
                "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; \
                       0 filtered out; finished in 0.00s\n"
            ),
            Ok(Tally {
                binaries: 1,
                ..Tally::default()
            })
        );
        let shuffled = "running 1 test (shuffle seed: 42)\ntest result: ok. 1 passed; \
                        0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(Tally::of(shuffled).map(|tally| tally.passed), Ok(1));
    }

    #[test]
    fn a_summary_that_does_not_read_as_one_fails_the_count() {
        for summary in [
            "test result: ok. 3 passed; 0 failed; 0 measured; 0 filtered out; finished in 0s",
            "test result: ok. three passed; 0 failed; 0 ignored; 0 measured; 0 filtered out",
            "test result: ok",
        ] {
            assert!(
                Tally::of(&format!("running 3 tests\n{summary}")).is_err(),
                "{summary}"
            );
        }
    }

    /// A summary libtest did not open a run for came from something else
    /// sharing the output, and one counting more than its run would pad a
    /// short run's count, so neither is taken.
    #[test]
    fn a_summary_that_closes_no_run_of_its_own_fails_the_count() {
        const STRAY: &str =
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s";
        assert!(Tally::of(STRAY).is_err(), "no run opened");
        assert!(
            Tally::of(&format!("{SHARD}{STRAY}\n")).is_err(),
            "a second summary for one run"
        );
        assert!(
            Tally::of(&format!("running 1 test\n{STRAY}\n")).is_err(),
            "more than its run opened with"
        );
    }

    /// A binary that exits cleanly before its summary, as one whose test calls
    /// `exit(0)` does, ran none of the tests after that one.
    #[test]
    fn a_run_left_open_fails_the_count() {
        assert!(
            Tally::of("running 5 tests\ntest a ... ok\n").is_err(),
            "open at the end"
        );
        assert!(
            Tally::of(&format!("running 5 tests\ntest a ... ok\n{SHARD}")).is_err(),
            "open when the next binary starts"
        );
    }

    fn settled(account: Account, passed: u64, ignored: u64, filtered: u64) -> Result<(), String> {
        account.settle(&Tally {
            binaries: 1,
            passed,
            ignored,
            filtered,
            ..Tally::default()
        })
    }

    #[test]
    fn a_shard_settles_only_on_every_test_it_was_dealt() {
        assert_eq!(settled(Account::Exactly(3), 3, 0, 9), Ok(()));
        assert!(
            settled(Account::Exactly(3), 2, 0, 10).is_err(),
            "one dropped"
        );
        assert!(
            settled(Account::Exactly(3), 3, 1, 9).is_err(),
            "one ignored"
        );
    }

    #[test]
    fn a_whole_run_settles_only_on_every_test_and_at_least_one() {
        assert_eq!(settled(Account::Everything, 5, 0, 0), Ok(()));
        assert!(settled(Account::Everything, 5, 0, 1).is_err(), "filtered");
        assert!(settled(Account::Everything, 5, 1, 0).is_err(), "ignored");
        assert!(settled(Account::Everything, 0, 0, 0).is_err(), "empty");
        assert_eq!(settled(Account::Selected, 5, 0, 7), Ok(()));
        assert!(settled(Account::Selected, 5, 1, 7).is_err(), "ignored");
        assert!(settled(Account::Selected, 0, 0, 7).is_err(), "empty");
        assert!(
            Account::Everything.settle(&Tally::default()).is_err(),
            "no summary at all"
        );
        assert_eq!(
            settled(Account::Forwarded, 0, 0, 7),
            Ok(()),
            "matched nothing here"
        );
        assert!(settled(Account::Forwarded, 3, 1, 7).is_err(), "ignored");
        assert!(
            Account::Forwarded.settle(&Tally::default()).is_err(),
            "no summary at all"
        );
    }
}
