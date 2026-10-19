//! The authority's lockout on one account, counted down for a surface to
//! show.

use alloc::string::{String, ToString};

use tairix_abi::time::{Duration64, NANOS_PER_SEC};

/// The authority's lockout on one account, counted down against the monotonic
/// clock.
///
/// The authority meters each login name on its own, so a lockout belongs to
/// the account it was reported for: it is shown only while a surface asks
/// about that account, and a person who picks another is never held behind
/// the first one's wait. [`AuthSurface::set_cooldown`](crate::AuthSurface::set_cooldown)
/// presents a remaining span and reads no clock of its own, so the countdown
/// lives here; being monotonic, a wall-clock correction cannot shorten or
/// lengthen it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Cooldown {
    /// When the lockout ends, and the login name it holds back.
    lockout: Option<(u64, String)>,
}

impl Cooldown {
    /// Record the authority's answer about `account`: a lockout of
    /// `retry_after` starting now, or — for a zero or negative span — none.
    ///
    /// A lockout replaces whichever stood before it. An answer with none
    /// clears only that account's own, so a refusal for one account leaves
    /// another's countdown standing.
    pub fn start(&mut self, now_ns: u64, retry_after: Duration64, account: &str) {
        let span = retry_after.saturating_total_nanos();
        if span > 0 {
            self.lockout = Some((now_ns.saturating_add(span), account.to_string()));
        } else if self
            .lockout
            .as_ref()
            .is_some_and(|(_, held)| held == account)
        {
            self.lockout = None;
        }
    }

    /// How much of the lockout on `asking` — the account the surface is
    /// asking about, if any — is left: zero once it has run out, and for
    /// every other account.
    #[must_use]
    pub fn remaining(&self, now_ns: u64, asking: Option<&str>) -> Duration64 {
        match &self.lockout {
            Some((until, held)) if asking == Some(held.as_str()) => {
                Duration64::from_nanos(until.saturating_sub(now_ns))
            }
            _ => Duration64::ZERO,
        }
    }

    /// Nanoseconds until a lockout of `remaining` next reads differently —
    /// the whole second it shows turns over, or it ends — or [`None`] when
    /// there is none to show.
    ///
    /// What a surface parks until while one counts down, so the second
    /// changes on screen when it changes rather than up to a second late.
    #[must_use]
    pub fn tick_of(remaining: Duration64) -> Option<u64> {
        let left = remaining.saturating_total_nanos();
        let second = u64::from(NANOS_PER_SEC);
        (left > 0).then(|| match left % second {
            0 => second,
            part => part,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Cooldown;
    use tairix_abi::time::{Duration64, NANOS_PER_SEC};

    const SECOND: u64 = NANOS_PER_SEC as u64;

    /// The account every lockout below is reported for.
    const ANN: Option<&str> = Some("ann");

    #[test]
    fn a_lockout_counts_down_against_the_monotonic_clock() {
        let mut cooldown = Cooldown::default();
        assert_eq!(cooldown.remaining(0, ANN), Duration64::ZERO);

        cooldown.start(1_000, Duration64::from_secs(2), "ann");
        assert_eq!(cooldown.remaining(1_000, ANN), Duration64::from_secs(2));
        assert_eq!(
            cooldown.remaining(1_000 + SECOND, ANN),
            Duration64::from_secs(1)
        );
    }

    #[test]
    fn a_lockout_that_has_run_out_reports_zero() {
        let mut cooldown = Cooldown::default();
        cooldown.start(0, Duration64::from_secs(2), "ann");
        assert_eq!(cooldown.remaining(3 * SECOND, ANN), Duration64::ZERO);
    }

    #[test]
    fn a_zero_or_negative_span_is_not_a_lockout() {
        let mut cooldown = Cooldown::default();
        cooldown.start(0, Duration64::from_secs(5), "ann");
        cooldown.start(0, Duration64::ZERO, "ann");
        assert_eq!(cooldown.remaining(0, ANN), Duration64::ZERO);

        cooldown.start(0, Duration64::from_secs(5), "ann");
        cooldown.start(0, Duration64::from_secs(-5), "ann");
        assert_eq!(cooldown.remaining(0, ANN), Duration64::ZERO);
    }

    #[test]
    fn a_clock_that_ran_backwards_never_lengthens_a_lockout() {
        let mut cooldown = Cooldown::default();
        cooldown.start(10 * SECOND, Duration64::from_secs(1), "ann");
        assert_eq!(cooldown.remaining(0, ANN), Duration64::from_secs(11));
        assert_eq!(cooldown.remaining(u64::MAX, ANN), Duration64::ZERO);
    }

    /// The authority meters each login name on its own, so a lockout on one
    /// account is shown neither on another's prompt nor on the chooser.
    #[test]
    fn a_lockout_holds_back_only_the_account_it_was_reported_for() {
        let mut cooldown = Cooldown::default();
        cooldown.start(0, Duration64::from_secs(20), "ann");
        assert_eq!(cooldown.remaining(0, ANN), Duration64::from_secs(20));
        assert_eq!(cooldown.remaining(0, Some("bo")), Duration64::ZERO);
        assert_eq!(cooldown.remaining(0, None), Duration64::ZERO, "the chooser");
    }

    /// An answer about another account that carries no lockout leaves this
    /// one's standing; one that carries a lockout of its own replaces it.
    #[test]
    fn an_answer_about_another_account_leaves_a_lockout_standing() {
        let mut cooldown = Cooldown::default();
        cooldown.start(0, Duration64::from_secs(20), "ann");
        cooldown.start(0, Duration64::ZERO, "bo");
        assert_eq!(cooldown.remaining(0, ANN), Duration64::from_secs(20));

        cooldown.start(0, Duration64::from_secs(5), "bo");
        assert_eq!(cooldown.remaining(0, Some("bo")), Duration64::from_secs(5));
        assert_eq!(cooldown.remaining(0, ANN), Duration64::ZERO);
    }

    #[test]
    fn a_countdown_ticks_when_its_shown_second_turns_over() {
        assert_eq!(Cooldown::tick_of(Duration64::ZERO), None);
        assert_eq!(Cooldown::tick_of(Duration64::from_secs(30)), Some(SECOND));
        assert_eq!(
            Cooldown::tick_of(Duration64::from_nanos(2_900_000_000)),
            Some(900_000_000),
            "2.9 s shows as 3 until 2 s are left"
        );
        assert_eq!(
            Cooldown::tick_of(Duration64::from_nanos(400_000_000)),
            Some(400_000_000),
            "the last of one ends exactly when it ends"
        );
    }
}
