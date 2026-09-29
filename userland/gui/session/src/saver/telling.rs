//! What a clock screensaver tells: the icon bar's own reading and spelling of
//! the time, the date as the face spells it, and when the minute next turns.

use alloc::string::String;

use tairix_abi::time::{Time64, WallClockReading};

use crate::clock::SessionClock;
use crate::switchuser::NO_DEADLINE_NS;

/// How a face spells the date beneath its time.
pub(super) type DateSpelling = fn(Time64) -> String;

/// The time a clock face tells, and when it next changes.
pub(super) struct Telling {
    clock: SessionClock,
    /// The date's spelling, or `None` for a face that tells no date.
    spelling: Option<DateSpelling>,
    date: String,
    /// When the minute next turns.
    tick_ns: u64,
}

impl Telling {
    /// A telling of `wall` as of `now_ns`, spelling the date with `spelling`.
    pub(super) fn new(
        spelling: Option<DateSpelling>,
        (wall, now_ns): (Option<WallClockReading>, u64),
    ) -> Self {
        let mut telling = Self {
            clock: SessionClock::new(),
            spelling,
            date: String::new(),
            tick_ns: now_ns,
        };
        telling.read(wall, now_ns);
        telling
    }

    /// The time as the icon bar spells it.
    pub(super) fn time(&self) -> &str {
        self.clock.label()
    }

    /// The date, empty when it is not told or the clock is not set.
    pub(super) fn date(&self) -> &str {
        &self.date
    }

    /// When the minute next turns, and the face is owed a reading.
    pub(super) const fn tick_ns(&self) -> u64 {
        self.tick_ns
    }

    /// Adopt the wall-clock `reading` as of `now_ns`, or ask again a minute on
    /// when there is none, and settle when the minute next turns.
    pub(super) fn read(&mut self, reading: Option<WallClockReading>, now_ns: u64) {
        match reading {
            Some(reading) => {
                let _ = self.clock.adopt(reading, now_ns);
                self.date = match self.spelling {
                    Some(spell) if reading.state().is_set() => spell(reading.time()),
                    _ => String::new(),
                };
            }
            None => self.clock.missed(now_ns),
        }
        self.tick_ns = now_ns.saturating_add(self.clock.park_deadline_ns(now_ns, NO_DEADLINE_NS));
    }
}
