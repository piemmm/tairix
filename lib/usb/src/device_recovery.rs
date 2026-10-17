//! The recovery ladder of one endpoint whose transfers halt: a device's
//! interrupt-IN endpoint, or a hub's status-change endpoint.
//!
//! An incident opens at a halt and closes only when a transfer next completes,
//! so a recovery that seemed to work but halts again is a failed one. Each
//! incident climbs a bounded ladder — in place at once, then deferred retries,
//! then a port reset — and an owner a reset produced is given up rather than
//! reset again, so no endpoint that never works can loop.

/// Deferred retries a failed endpoint recovery is owed, each once its path has
/// had time to settle, before its port is reset and enumerated afresh. A
/// transaction translator disturbed for a moment fails a recovery as surely as
/// an unplug does; the settled port tells the two apart. A recovery bound, not
/// a scalable capacity.
pub(crate) const RECOVERY_RETRIES: u8 = 2;

/// Where one endpoint's recovery stands.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Ladder {
    /// The deferred retries the open incident has left, `None` while none is
    /// open.
    open: Option<u8>,
    /// A deferred retry is owed.
    due: bool,
    /// The owner was enumerated by a reset this ladder escalated to and has
    /// completed no transfer since.
    from_reset: bool,
}

/// How a halting completion is answered at once.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum OnHalt {
    /// Recover the endpoint in place now.
    Recover,
    /// Leave it: the incident's in-place recovery did not hold, so a deferred
    /// retry decides.
    Defer,
}

/// What a deferred retry does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Rung {
    /// Recover the endpoint in place.
    Recover,
    /// Climb past every retry.
    Escalate(Escalation),
}

/// What is left once an incident's retries are spent.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Escalation {
    /// Free the owner, reset its port and enumerate what answers there.
    Reset,
    /// Free the owner and leave its port unserved until a re-plug.
    GiveUp,
}

impl Ladder {
    /// The ladder of an owner a reset this ladder escalated to just
    /// enumerated.
    pub(super) const fn after_reset() -> Self {
        Self {
            open: None,
            due: false,
            from_reset: true,
        }
    }

    /// A transfer completed: the endpoint works, so nothing is owed.
    pub(super) fn completed(&mut self) {
        *self = Self::default();
    }

    /// A halting completion reached the top-level recovery point.
    pub(super) fn halted(&mut self) -> OnHalt {
        if self.open.is_some() {
            self.due = true;
            return OnHalt::Defer;
        }
        self.open = Some(RECOVERY_RETRIES);
        OnHalt::Recover
    }

    /// The in-place recovery [`Self::halted`] asked for failed.
    pub(super) fn failed(&mut self) {
        self.due = true;
    }

    /// Whether a deferred retry is owed.
    pub(super) const fn owed(self) -> bool {
        self.due
    }

    /// Take the owed retry: in place while the incident has retries left,
    /// else the escalation.
    pub(super) fn climb(&mut self) -> Rung {
        self.due = false;
        match self.open {
            Some(left) if left > 0 => {
                self.open = Some(left - 1);
                Rung::Recover
            }
            _ => Rung::Escalate(self.escalation()),
        }
    }

    /// The in-place retry [`Self::climb`] asked for failed: `None` when
    /// another is owed, else the escalation to run at once.
    pub(super) fn retry_failed(&mut self) -> Option<Escalation> {
        if self.open == Some(0) {
            return Some(self.escalation());
        }
        self.due = true;
        None
    }

    /// The rung past every retry: a reset, or giving up an owner one already
    /// produced.
    pub(super) const fn escalation(self) -> Escalation {
        if self.from_reset {
            Escalation::GiveUp
        } else {
            Escalation::Reset
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Escalation, Ladder, OnHalt, Rung, RECOVERY_RETRIES};

    #[test]
    fn a_halt_on_a_working_endpoint_is_recovered_at_once_and_owes_nothing() {
        let mut ladder = Ladder::default();
        assert_eq!(ladder.halted(), OnHalt::Recover);
        assert!(!ladder.owed());
        ladder.completed();
        assert_eq!(ladder, Ladder::default());
    }

    #[test]
    fn failed_retries_escalate_to_a_reset_in_the_round_that_spends_them() {
        let mut ladder = Ladder::default();
        assert_eq!(ladder.halted(), OnHalt::Recover);
        ladder.failed();
        for _ in 1..RECOVERY_RETRIES {
            assert!(ladder.owed());
            assert_eq!(ladder.climb(), Rung::Recover);
            assert_eq!(ladder.retry_failed(), None);
        }
        assert!(ladder.owed());
        assert_eq!(ladder.climb(), Rung::Recover);
        assert_eq!(ladder.retry_failed(), Some(Escalation::Reset));
    }

    #[test]
    fn an_endpoint_that_halts_however_often_it_is_recovered_reaches_the_reset() {
        // Each recovery works, the endpoint halts again before a transfer
        // completes: no recovery held, so the incident climbs regardless.
        let mut ladder = Ladder::default();
        assert_eq!(ladder.halted(), OnHalt::Recover);
        for _ in 0..RECOVERY_RETRIES {
            assert_eq!(ladder.halted(), OnHalt::Defer);
            assert_eq!(ladder.climb(), Rung::Recover);
        }
        assert_eq!(ladder.halted(), OnHalt::Defer);
        assert_eq!(ladder.climb(), Rung::Escalate(Escalation::Reset));
    }

    #[test]
    fn an_owner_a_reset_produced_is_given_up_rather_than_reset_again() {
        let mut ladder = Ladder::after_reset();
        assert_eq!(ladder.escalation(), Escalation::GiveUp);
        assert_eq!(ladder.halted(), OnHalt::Recover);
        ladder.failed();
        assert_eq!(ladder.climb(), Rung::Recover);
        assert_eq!(ladder.retry_failed(), None);
        assert_eq!(ladder.climb(), Rung::Recover);
        assert_eq!(ladder.retry_failed(), Some(Escalation::GiveUp));
    }

    #[test]
    fn a_completed_transfer_closes_the_incident_and_cancels_an_owed_retry() {
        let mut ladder = Ladder::after_reset();
        assert_eq!(ladder.halted(), OnHalt::Recover);
        assert_eq!(ladder.halted(), OnHalt::Defer);
        assert!(ladder.owed());
        ladder.completed();
        assert!(!ladder.owed());
        assert_eq!(
            ladder.escalation(),
            Escalation::Reset,
            "proven, so resettable"
        );
        assert_eq!(ladder.halted(), OnHalt::Recover, "a fresh incident");
    }
}
