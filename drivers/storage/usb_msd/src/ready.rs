//! Whether a logical unit can serve, as a timed state machine
//! (`docs/src/drivers/block.md`).
//!
//! A unit's answer is classified by its sense (key, ASC, ASCQ) into a
//! [`Readiness`], and a [`ReadyWait`] turns a run of them into when the unit
//! is next tested, whether that test is a start, and how the wait ends. It
//! holds no clock and issues no command; the serve loop supplies the time and
//! runs the test, so the whole policy is proven host-side.

use tairix_abi::Errno;

use crate::scsi::Sense;

/// How long a unit that is coming ready is left before it is tested again.
pub const READY_POLL_NS: u64 = 1_000_000_000;

/// How often a unit with no medium is tested for one being inserted: the
/// interval Linux polls removable media at, short enough that a card shows up
/// promptly, long enough that an empty slot costs one command in two seconds.
pub const MEDIA_POLL_NS: u64 = 2_000_000_000;

/// Sense key NOT READY.
const NOT_READY: u8 = 0x02;
/// Sense key UNIT ATTENTION.
const UNIT_ATTENTION: u8 = 0x06;
/// ASC LOGICAL UNIT NOT READY.
const ASC_NOT_READY: u8 = 0x04;
/// ASCQ: initializing command required.
const ASCQ_START_REQUIRED: u8 = 0x02;
/// ASCQ: manual intervention required.
const ASCQ_INTERVENTION: u8 = 0x03;
/// ASC MEDIUM NOT PRESENT.
const ASC_NO_MEDIUM: u8 = 0x3A;
/// ASC NOT READY TO READY CHANGE, MEDIUM MAY HAVE CHANGED.
const ASC_MEDIUM_CHANGED: u8 = 0x28;

/// How many unit attentions in a row are answered with an immediate retest
/// before the next waits out [`READY_POLL_NS`]: a unit reports a pending
/// attention once, so more in a row is a unit that keeps raising them.
const IMMEDIATE_RETESTS: u8 = 3;

/// What a unit's answer says about whether it can serve (SPC-5 §4.4, the
/// assignments in its Annex F).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Readiness {
    /// It can serve.
    Ready,
    /// It is coming ready by itself: a disk spinning up (02/04/01, and any
    /// other "not ready" whose cause is not one below).
    BecomingReady,
    /// It needs an initializing command, START STOP UNIT (02/04/02).
    NeedsStart,
    /// It needs a person (02/04/03): never retried.
    NeedsIntervention,
    /// It has no medium (02/3A).
    NoMedium,
    /// Its medium may have changed (06/28): what it holds must be read again.
    MediumChanged,
    /// It was reset or powered on (06/29), or another unit attention: the
    /// attention is consumed, so it is tested again at once.
    Attention,
    /// It refused for a reason that is not about readiness.
    Refused(Sense),
}

impl Readiness {
    /// Classify the sense of a command a unit failed.
    #[must_use]
    pub const fn of_sense(sense: Sense) -> Self {
        match (sense.key, sense.asc, sense.ascq) {
            (NOT_READY, ASC_NO_MEDIUM, _) => Self::NoMedium,
            (NOT_READY, ASC_NOT_READY, ASCQ_START_REQUIRED) => Self::NeedsStart,
            (NOT_READY, ASC_NOT_READY, ASCQ_INTERVENTION) => Self::NeedsIntervention,
            (NOT_READY, _, _) => Self::BecomingReady,
            (UNIT_ATTENTION, ASC_MEDIUM_CHANGED, _) => Self::MediumChanged,
            (UNIT_ATTENTION, _, _) => Self::Attention,
            _ => Self::Refused(sense),
        }
    }
}

/// Why a unit was given up on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NotReady {
    /// It did not come ready within its ready budget.
    Budget,
    /// It needs manual intervention.
    Intervention,
    /// It answered START STOP UNIT with a refusal, or refused for another
    /// reason.
    Refused(Sense),
    /// It answered ready, but what it reports of itself (its capacity, its
    /// write policy) could not be read, or could not be honest.
    Unusable(Errno),
}

/// How a wait for a unit ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WaitEnd {
    /// It is ready.
    Ready,
    /// It has no medium.
    NoMedium,
    /// It was given up on.
    GiveUp(NotReady),
}

/// The wait for one unit to come ready, bounded by its ready budget: when it
/// is next tested, and whether that test is START STOP UNIT.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReadyWait {
    deadline_ns: u64,
    due_ns: u64,
    start_owed: bool,
    started: bool,
    attentions: u8,
}

impl ReadyWait {
    /// A wait begun at `now_ns`, tested at once, that gives up `ready_ns`
    /// later.
    #[must_use]
    pub const fn new(now_ns: u64, ready_ns: u64) -> Self {
        Self {
            deadline_ns: now_ns.saturating_add(ready_ns),
            due_ns: now_ns,
            start_owed: false,
            started: false,
            attentions: 0,
        }
    }

    /// The wait for a unit that refused a command at `now_ns` as coming
    /// ready: started at once when it asked for an initializing command
    /// (`start`), else tested after the poll interval.
    #[must_use]
    pub const fn refused(now_ns: u64, ready_ns: u64, start: bool) -> Self {
        let wait = Self::new(now_ns, ready_ns);
        Self {
            due_ns: if start { now_ns } else { wait.paced(now_ns) },
            start_owed: start,
            started: start,
            ..wait
        }
    }

    /// The instant the wait gives up.
    #[must_use]
    pub const fn deadline_ns(&self) -> u64 {
        self.deadline_ns
    }

    /// The instant the unit is next tested.
    #[must_use]
    pub const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Whether the next test is START STOP UNIT rather than TEST UNIT READY.
    #[must_use]
    pub const fn owes_start(&self) -> bool {
        self.start_owed
    }

    /// Fold the unit's answer, at `now_ns`, to the test that was due: how the
    /// wait ended, or `None` with the next test armed.
    ///
    /// A unit is started at most once per wait, and tested again no sooner
    /// than [`READY_POLL_NS`] later unless the answer itself was consumed (a
    /// unit attention, a few in a row at most), so tests never run back to
    /// back for long.
    pub fn answer(&mut self, readiness: Readiness, now_ns: u64) -> Option<WaitEnd> {
        let attention = matches!(readiness, Readiness::Attention | Readiness::MediumChanged);
        self.attentions = if attention {
            self.attentions.saturating_add(1)
        } else {
            0
        };
        self.start_owed = false;
        let end = match readiness {
            Readiness::Ready => WaitEnd::Ready,
            Readiness::NoMedium => WaitEnd::NoMedium,
            Readiness::NeedsIntervention => WaitEnd::GiveUp(NotReady::Intervention),
            Readiness::Refused(sense) => WaitEnd::GiveUp(NotReady::Refused(sense)),
            _ if now_ns >= self.deadline_ns => WaitEnd::GiveUp(NotReady::Budget),
            _ => {
                let start = matches!(readiness, Readiness::NeedsStart) && !self.started;
                self.started |= start;
                self.start_owed = start;
                self.due_ns = if start || (attention && self.attentions <= IMMEDIATE_RETESTS) {
                    now_ns
                } else {
                    self.paced(now_ns)
                };
                return None;
            }
        };
        Some(end)
    }

    /// The unit answered ready at `now_ns` but could not be served, for
    /// `why`: `None` with the next test armed within its budget, or `why`
    /// once the budget is spent, so a unit that keeps answering ready while
    /// it cannot serve is not retried for ever.
    pub fn retry(&mut self, why: NotReady, now_ns: u64) -> Option<NotReady> {
        self.attentions = 0;
        self.start_owed = false;
        if now_ns >= self.deadline_ns {
            return Some(why);
        }
        self.due_ns = self.paced(now_ns);
        None
    }

    /// The poll interval after `now_ns`, never past the deadline, so the
    /// last test lands on it.
    const fn paced(&self, now_ns: u64) -> u64 {
        let at = now_ns.saturating_add(READY_POLL_NS);
        if at < self.deadline_ns {
            at
        } else {
            self.deadline_ns
        }
    }
}

#[cfg(test)]
#[path = "ready_tests.rs"]
mod tests;
