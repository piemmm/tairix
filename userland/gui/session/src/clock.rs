//! The icon bar clock's reading — the time and date in the user's zone,
//! written in their region's conventions — and its once-a-minute tick.
//!
//! The bar holds only the lines it draws ([`tairix_taskbar::Clock`]) and
//! carries no time ABI; this is the session's side. A wall clock never set
//! this boot reads [`UNSET_LABEL`] with no date: `00:00` would be a
//! fabricated time, and an empty clock would hide the menu a time is set from.
//!
//! The clock arms one deadline, the next minute boundary of its local time,
//! folded into the session's park through [`park_within`], and is read only on
//! the wakes that deadline shortened the park for ([`SessionClock::is_due`]).
//! A background session folds no deadlines, so its clock does not tick.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::time::{WallClockReading, NANOS_PER_SEC, SECS_PER_MINUTE};
use tairix_taskbar::clock::{ClockLines, UNSET_LABEL};
use tairix_tz::{CivilClock, Decided, LocalTime, ZoneInfo, ZoneOffset, WIDEST};

use crate::switchuser::park_within;

/// A minute in nanoseconds: how long a tick waits when its next minute
/// cannot be had.
const MINUTE_NS: u64 = SECS_PER_MINUTE.unsigned_abs() * NANOS_PER_SEC as u64;

/// The zone store the session read at bring-up, and the descriptor it read
/// it through, held open for the life of the session.
///
/// The descriptor is what the desktop's Settings application is delegated
/// the store from, so a delegation costs no filesystem operation on the serve
/// loop.
#[derive(Copy, Clone, Debug)]
pub struct HeldZones {
    /// The store.
    pub info: &'static ZoneInfo<'static>,
    /// The descriptor it was read through.
    pub fd: u32,
}

/// What adopting a zone and region decided.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CivilAdoption {
    /// What decided the zone.
    pub zone: Decided,
    /// What decided the region.
    pub region: Decided,
    /// The bar's clock keeps room of a new shape, a repaint the caller
    /// presents.
    pub reshaped: bool,
}

/// The session's clock: what it reads in, the lines it last spelled, and
/// when they go stale.
#[derive(Clone, Debug)]
pub struct SessionClock {
    civil: CivilClock<'static>,
    lines: ClockLines,
    heading: String,
    /// The monotonic instant the lines stop being right, or `None` before the
    /// first reading — there is then nothing to go stale.
    stale_at_ns: Option<u64>,
}

impl SessionClock {
    /// A clock reading in `civil` that has read nothing yet.
    #[must_use]
    pub const fn new(civil: CivilClock<'static>) -> Self {
        Self {
            civil,
            lines: ClockLines {
                time: String::new(),
                date: String::new(),
            },
            heading: String::new(),
            stale_at_ns: None,
        }
    }

    /// The zone and conventions it reads in.
    #[must_use]
    pub const fn civil(&self) -> CivilClock<'static> {
        self.civil
    }

    /// Read in `civil` from now on. The clock is due at once, so a changed
    /// zone or region shows on the next wake rather than at the next minute.
    pub fn set_civil(&mut self, civil: CivilClock<'static>) {
        self.civil = civil;
        self.stale_at_ns = Some(0);
    }

    /// The lines it last spelled; empty before the first reading.
    #[must_use]
    pub const fn lines(&self) -> &ClockLines {
        &self.lines
    }

    /// The reading as the clock's menu states it: the date, the time and the
    /// zone's abbreviation.
    #[must_use]
    pub fn heading(&self) -> &str {
        &self.heading
    }

    /// Each line at its widest in the clock's conventions, for the bar to
    /// keep room for.
    #[must_use]
    pub fn widest(&self) -> Vec<ClockLines> {
        WIDEST
            .iter()
            .map(|civil| {
                let local = LocalTime {
                    civil: *civil,
                    offset: ZoneOffset {
                        utc_offset: 0,
                        abbreviation: "",
                        daylight: false,
                    },
                };
                ClockLines {
                    time: written(|out| self.civil.write_time(&local, out)),
                    date: written(|out| self.civil.write_date(&local, out)),
                }
            })
            .collect()
    }

    /// Whether the wall clock is worth reading again: nothing has been read
    /// yet, or the minute the lines were right for has turned.
    #[must_use]
    pub fn is_due(&self, now_ns: u64) -> bool {
        self.stale_at_ns.is_none_or(|stale| now_ns >= stale)
    }

    /// Adopt `reading` as of monotonic instant `now_ns`, answering whether the
    /// lines changed and so owe the bar a repaint.
    ///
    /// The next minute boundary is the local clock's, measured against the
    /// *monotonic* clock the park is armed on: a wall clock stepped while the
    /// desktop waits moves the lines at the next tick rather than moving the
    /// deadline under them.
    pub fn adopt(&mut self, reading: WallClockReading, now_ns: u64) -> bool {
        let time = reading.time();
        let (lines, heading) = if reading.state().is_set() {
            let local = self.civil.local(time);
            let lines = ClockLines {
                time: written(|out| self.civil.write_time(&local, out)),
                date: written(|out| self.civil.write_date(&local, out)),
            };
            let heading = format!(
                "{} {} {}",
                lines.date, lines.time, local.offset.abbreviation
            );
            (lines, heading)
        } else {
            let lines = ClockLines {
                time: String::from(UNSET_LABEL),
                date: String::new(),
            };
            (lines, String::from(UNSET_LABEL))
        };
        // Even an unset reading arms the tick: the wall clock may be set
        // while the desktop is up. Only the end of the range has no later
        // minute, and a deadline of now would spin the park.
        let until = self
            .civil
            .zone()
            .next_minute(time)
            .saturating_duration_since(time)
            .saturating_total_nanos();
        self.stale_at_ns = Some(now_ns.saturating_add(if until == 0 { MINUTE_NS } else { until }));
        if lines == self.lines && heading == self.heading {
            return false;
        }
        self.lines = lines;
        self.heading = heading;
        true
    }

    /// A reading the tick owed could not be had at `now_ns`: ask again a
    /// minute from now, rather than on every wake from here on.
    ///
    /// The stale deadline is otherwise already past, and a past deadline
    /// folded into the park parks for nothing — the loop would spin.
    pub fn missed(&mut self, now_ns: u64) {
        self.stale_at_ns = Some(now_ns.saturating_add(MINUTE_NS));
    }

    /// `park_ns` shortened to the moment the lines go stale, or left exactly
    /// as it is before the first reading.
    ///
    /// Lines already stale shorten the park to nothing rather than leaving it
    /// alone: the tick is owed, and only [`adopt`](Self::adopt) pays it.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        park_within(
            park_ns,
            self.stale_at_ns
                .map(|stale| stale.saturating_sub(now_ns).min(park_ns)),
        )
    }
}

/// What `write` writes into a fresh string.
fn written(write: impl FnOnce(&mut String) -> fmt::Result) -> String {
    let mut out = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = write(&mut out);
    out
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
