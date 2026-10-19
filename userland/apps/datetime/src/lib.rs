//! The Date & Time dialog's host-tested engine: what the clock reads and
//! where that came from, when the zone's clocks next change, the instant a
//! hand-set date and time name, and the words every outcome is stated in.
//!
//! # No calendar or zone arithmetic of its own
//!
//! Local time is read and composed through `lib/tz` — the zone engine and the
//! region's spellings every clock on the desktop draws through — and dates
//! through `lib/fsmeta`'s calendar, so the dialog can never disagree with the
//! desktop clock beside it.
//!
//! # An unset clock shows nothing, never `1970-01-01`
//!
//! A machine whose wall time has not been established this boot reports
//! [`WallTimeState::Unset`], whose instant is the Unix-epoch placeholder and
//! means nothing. The dialog then says the clock is not set rather than
//! presenting the placeholder as a reading a user might "correct".
//!
//! # A local time names one instant, or is refused in words
//!
//! A hand-set date and time are local to the machine's zone, so they are
//! composed through [`Zone::instant_from_local`]: a time the clocks skip is
//! refused with the change that skips it, and one they pass twice is answered
//! only once the user has said which of its two offsets is meant. A time
//! outside the plausible window ([`is_plausible_wall_time`]) is confirmed
//! before it is set.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod calendar;
pub mod time_entry;
pub mod view;

use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::time::{
    days_from_civil, is_plausible_wall_time, CivilTime, Time64, WallClockReading, WallTimeState,
    SECS_PER_DAY,
};
use tairix_abi::zone::ZoneName;
use tairix_abi::Errno;
use tairix_fsmeta::calendar::{full_date, month_name, weekday_name};
use tairix_sysconfig::{Key, Switch, SystemConfig, DEFAULT_ZONE};
use tairix_tz::{CivilClock, Disambiguation, LocalError, LocalTime, Weekday, Zone, ZoneOffset};

use crate::calendar::Date;
use crate::time_entry::TimeOfDay;

/// Widest year the calendar offers in either direction.
///
/// A bound on what a person can pick, not on `Time64`: the instant is 64-bit
/// and the calendar proleptic, so this only keeps the steppers from walking
/// to a year no clock could mean.
pub const YEAR_MAX: i64 = 9999;

/// Narrowest year the calendar offers.
pub const YEAR_MIN: i64 = -9999;

/// The machine's time settings, as `system.conf` holds them.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MachineTime {
    /// Whether the clock is kept from the network (`time.network`).
    pub network: bool,
    /// The zone the machine keeps, or `None` for the zone store's own default
    /// (`time.zone`).
    pub zone: Option<ZoneName>,
}

impl MachineTime {
    /// The time settings `config` holds.
    #[must_use]
    pub fn of(config: &SystemConfig) -> Self {
        Self {
            network: config.time_network == Switch::On,
            zone: config.time_zone,
        }
    }

    /// The setting that turns network time `on` or off, as the settings
    /// service is asked for it.
    #[must_use]
    pub const fn network_setting(on: bool) -> (&'static str, &'static str) {
        let switch = if on { Switch::On } else { Switch::Off };
        (Key::TimeNetwork.name(), switch.as_str())
    }

    /// The setting that keeps the machine on `zone`, or on the store's own
    /// default for `None`.
    #[must_use]
    pub fn zone_setting(zone: Option<&ZoneName>) -> (&'static str, &str) {
        (
            Key::TimeZone.name(),
            zone.map_or(DEFAULT_ZONE, ZoneName::as_str),
        )
    }
}

/// What the machine's clock says, as far as the dialog knows it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Clock {
    /// The clock could not be read.
    Unreadable,
    /// The clock's reading, and when the network last set it, if ever.
    Read {
        /// The reading.
        reading: WallClockReading,
        /// When the time service last set the clock.
        last_sync: Option<Time64>,
    },
}

impl Clock {
    /// The instant the clock holds, where it holds one.
    #[must_use]
    pub fn now(&self) -> Option<Time64> {
        match self {
            Self::Read { reading, .. } if reading.state().is_set() => Some(reading.time()),
            _ => None,
        }
    }
}

/// What the dialog's first block states about the clock now.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Readout {
    /// The time of day to the second, or what stands in for one.
    pub time: String,
    /// The date written out, or why there is none.
    pub date: String,
    /// The zone: the place, and its abbreviation and offset now.
    pub zone: String,
    /// Where the clock's time came from.
    pub source: String,
    /// When the zone's clocks next change.
    pub change: String,
}

/// What the dialog states about `clock`, read by `civil` in the zone named
/// `place`, on a machine keeping network time when `network`.
#[must_use]
pub fn readout(clock: Clock, network: bool, place: &str, civil: &CivilClock<'_>) -> Readout {
    let Clock::Read { reading, last_sync } = clock else {
        return Readout {
            time: String::from("Unknown"),
            date: String::from("The machine's clock could not be read."),
            zone: String::from(place),
            ..Readout::default()
        };
    };
    if !reading.state().is_set() {
        return Readout {
            time: String::from("Not set"),
            date: String::from("The clock has not been set since the machine started."),
            zone: String::from(place),
            source: String::from(if network {
                "Waiting for a time server on the network to answer."
            } else {
                "Set the date and time below, or turn on network time."
            }),
            change: String::new(),
        };
    }
    let now = reading.time();
    let local = civil.local(now);
    let mut time = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = civil.write_time_with_seconds(&local, &mut time);
    Readout {
        time,
        date: full_date(&local.civil),
        zone: zone_line(place, local.offset),
        source: source(reading.state(), last_sync, now, civil),
        change: next_change(civil, now),
    }
}

/// The zone as the first block names it: the place, then its abbreviation and
/// offset now where they say anything the place does not.
#[must_use]
pub fn zone_line(place: &str, offset: ZoneOffset<'_>) -> String {
    let mut line = String::from(place);
    let numeric = offset.abbreviation.starts_with(['+', '-']);
    if offset.abbreviation == place || numeric {
        if offset.utc_offset == 0 && offset.abbreviation == place {
            return line;
        }
        line.push_str(" \u{b7} ");
    } else {
        line.push_str(" \u{b7} ");
        line.push_str(offset.abbreviation);
        line.push_str(", ");
    }
    // A `String` takes any write, so there is no failure to keep.
    let _ = offset.write_from_utc(&mut line);
    line
}

/// Where the clock's time came from, in words.
fn source(
    state: WallTimeState,
    last_sync: Option<Time64>,
    now: Time64,
    civil: &CivilClock<'_>,
) -> String {
    match state {
        WallTimeState::Unset => String::from("Not set."),
        WallTimeState::Firmware => {
            String::from("From the clock chip, read when the machine started.")
        }
        WallTimeState::Adjusted => String::from("Set by hand."),
        WallTimeState::Trusted => match last_sync.filter(|at| *at <= now) {
            Some(at) => {
                let mut line = String::from("Set automatically from the network ");
                when(at, now, civil, &mut line);
                line.push('.');
                line
            }
            None => String::from("Set automatically from the network."),
        },
    }
}

/// When the zone's clocks next change, in words.
fn next_change(civil: &CivilClock<'_>, now: Time64) -> String {
    let Some(change) = civil.zone().next_change(now) else {
        return String::from("No change of the clocks is scheduled here.");
    };
    let delta = change.after.utc_offset - change.before.utc_offset;
    let mut line = String::from(if delta > 0 {
        "Clocks go forward "
    } else {
        "Clocks go back "
    });
    amount(delta.unsigned_abs(), &mut line);
    line.push(' ');
    let before = LocalTime {
        civil: CivilTime::at_offset(change.at.secs(), change.before.utc_offset),
        offset: change.before,
    };
    on_day(&before.civil, &civil.local(now).civil, &mut line);
    line.push_str(" at ");
    // A `String` takes any write, so there is no failure to keep.
    let _ = civil.write_time(&before, &mut line);
    line.push('.');
    line
}

/// How far a change moves the clocks: `an hour`, `30 minutes`.
fn amount(secs: u32, out: &mut String) {
    let (hours, minutes) = (secs / 3600, secs % 3600 / 60);
    // A `String` takes any write, so there is no failure to keep.
    match hours {
        0 => {}
        1 => out.push_str("an hour"),
        _ => {
            let _ = write!(out, "{hours} hours");
        }
    }
    if hours > 0 && minutes > 0 {
        out.push_str(" and ");
    }
    match minutes {
        0 if hours == 0 => out.push_str("no time"),
        0 => {}
        1 => out.push_str("a minute"),
        _ => {
            let _ = write!(out, "{minutes} minutes");
        }
    }
}

/// `at` as a reader places it from `now`: `today at 14:02`, `yesterday at
/// 14:02`, `on Saturday 17 October at 14:02`.
fn when(at: Time64, now: Time64, civil: &CivilClock<'_>, out: &mut String) {
    let local = civil.local(at);
    on_day(&local.civil, &civil.local(now).civil, out);
    out.push_str(" at ");
    // A `String` takes any write, so there is no failure to keep.
    let _ = civil.write_time(&local, out);
}

/// `day` named from `today`: `today`, `tomorrow`, `yesterday`, else `on` the
/// date, its year only where it is not this one.
fn on_day(day: &CivilTime, today: &CivilTime, out: &mut String) {
    let days = |civil: &CivilTime| days_from_civil(civil.year, civil.month, civil.day);
    let (Some(of_day), Some(of_today)) = (days(day), days(today)) else {
        return;
    };
    match of_day - of_today {
        0 => out.push_str("today"),
        1 => out.push_str("tomorrow"),
        -1 => out.push_str("yesterday"),
        _ => {
            // A `String` takes any write, so there is no failure to keep.
            let _ = write!(
                out,
                "on {} {} {}",
                weekday_name(Weekday::of_days(of_day).iso()),
                day.day,
                month_name(day.month)
            );
            if day.year != today.year {
                let _ = write!(out, " {}", day.year);
            }
        }
    }
}

/// What a hand-set date and time came to in a zone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Composed<'a> {
    /// The one instant they name.
    At(Time64),
    /// The clocks skip them: they go forward at `from` to `to`, local.
    Skipped {
        /// The local time the clocks leave.
        from: LocalTime<'a>,
        /// The local time they arrive at.
        to: LocalTime<'a>,
    },
    /// The clocks pass them twice, first at `earlier` and then at `later`,
    /// going back at `at`.
    Repeated {
        /// The offset in force the first time round.
        earlier: ZoneOffset<'a>,
        /// The offset in force the second time.
        later: ZoneOffset<'a>,
        /// The local time the clocks go back from.
        at: LocalTime<'a>,
    },
}

/// The instant `date` at `time` names in `zone`, `choice` deciding between
/// two.
#[must_use]
pub fn compose(
    zone: Zone<'_>,
    date: Date,
    time: TimeOfDay,
    choice: Disambiguation,
) -> Composed<'_> {
    let local = civil_of(date, time);
    match zone.instant_from_local(&local, choice) {
        Ok(at) => Composed::At(at),
        Err(LocalError::Overlap) => repeated(zone, &local),
        Err(LocalError::Gap | LocalError::Invalid) => skipped(zone, &local),
    }
}

/// The fields of `date` at `time`.
fn civil_of(date: Date, time: TimeOfDay) -> CivilTime {
    CivilTime {
        year: date.year,
        month: date.month,
        day: date.day,
        hour: time.hour,
        minute: time.minute,
        second: time.second,
    }
}

/// The change that skips `local`, found among those near it.
fn skipped<'a>(zone: Zone<'a>, local: &CivilTime) -> Composed<'a> {
    let wall = local.to_unix_secs().unwrap_or(0);
    let mut after = Time64::from_secs(wall.saturating_sub(SECS_PER_DAY));
    for _ in 0..NEARBY_CHANGES {
        let Some(change) = zone.next_change(after) else {
            break;
        };
        let from = change
            .at
            .secs()
            .saturating_add(i64::from(change.before.utc_offset));
        let to = change
            .at
            .secs()
            .saturating_add(i64::from(change.after.utc_offset));
        if (from..to).contains(&wall) {
            return Composed::Skipped {
                from: LocalTime {
                    civil: CivilTime::at_offset(change.at.secs(), change.before.utc_offset),
                    offset: change.before,
                },
                to: LocalTime {
                    civil: CivilTime::at_offset(change.at.secs(), change.after.utc_offset),
                    offset: change.after,
                },
            };
        }
        after = change.at;
    }
    let offset = zone.offset_at(Time64::from_secs(wall));
    let local = LocalTime {
        civil: *local,
        offset,
    };
    Composed::Skipped {
        from: local,
        to: local,
    }
}

/// The two offsets `local` reads at, the earlier first, and the change
/// between them.
fn repeated<'a>(zone: Zone<'a>, local: &CivilTime) -> Composed<'a> {
    let first = zone.instant_from_local(local, Disambiguation::Earlier);
    let second = zone.instant_from_local(local, Disambiguation::Later);
    let (Ok(first), Ok(second)) = (first, second) else {
        return skipped(zone, local);
    };
    let Some(change) = zone.next_change(first).filter(|change| change.at <= second) else {
        return skipped(zone, local);
    };
    Composed::Repeated {
        earlier: zone.offset_at(first),
        later: zone.offset_at(second),
        at: LocalTime {
            civil: CivilTime::at_offset(change.at.secs(), change.before.utc_offset),
            offset: change.before,
        },
    }
}

/// How many changes of offset [`skipped`] looks through: those within a day
/// either side of a time are far fewer.
const NEARBY_CHANGES: usize = 8;

/// Why `time`, skipped, cannot be set, in words.
#[must_use]
pub fn skipped_reason(
    date: Date,
    time: TimeOfDay,
    from: &LocalTime<'_>,
    to: &LocalTime<'_>,
    civil: &CivilClock<'_>,
) -> String {
    let local = LocalTime {
        civil: civil_of(date, time),
        offset: from.offset,
    };
    let mut line = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = civil.write_time_with_seconds(&local, &mut line);
    line.push_str(" does not happen on ");
    on_date(&local.civil, &mut line);
    line.push_str(": the clocks go forward from ");
    let _ = civil.write_time(from, &mut line);
    line.push_str(" to ");
    let _ = civil.write_time(to, &mut line);
    line.push('.');
    line
}

/// The question a twice-passed `time` asks: its headline and detail, and its
/// two answers' labels, the earlier first. The clocks go back at `at`.
#[must_use]
pub fn repeated_question(
    date: Date,
    time: TimeOfDay,
    (earlier, later, at): (&ZoneOffset<'_>, &ZoneOffset<'_>, &LocalTime<'_>),
    civil: &CivilClock<'_>,
) -> (String, String, [String; 2]) {
    let local = LocalTime {
        civil: civil_of(date, time),
        offset: *earlier,
    };
    let mut spelled = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = civil.write_time_with_seconds(&local, &mut spelled);
    let mut headline = spelled.clone();
    headline.push_str(" happens twice on ");
    on_date(&local.civil, &mut headline);
    let mut detail = String::from("The clocks go back ");
    amount(
        (earlier.utc_offset - later.utc_offset).unsigned_abs(),
        &mut detail,
    );
    detail.push_str(" at ");
    let _ = civil.write_time(at, &mut detail);
    let _ = write!(
        detail,
        ", so {spelled} comes round twice: once before the change and once after. Which do \
         you mean?"
    );
    let label = |first: bool, offset: &ZoneOffset<'_>| {
        let mut label = String::from(if first { "Before: " } else { "After: " });
        if !offset.abbreviation.starts_with(['+', '-']) {
            label.push_str(offset.abbreviation);
            label.push_str(", ");
        }
        let _ = offset.write_from_utc(&mut label);
        label
    };
    (
        headline,
        detail,
        [label(true, earlier), label(false, later)],
    )
}

/// The question a time outside the plausible window asks before it is set:
/// its headline and detail.
#[must_use]
pub fn implausible_question(at: Time64, civil: &CivilClock<'_>) -> Option<(String, String)> {
    if is_plausible_wall_time(at) {
        return None;
    }
    let local = civil.local(at);
    let mut headline = String::from("Set the clock to ");
    headline.push_str(&full_date(&local.civil));
    headline.push('?');
    let detail = if at < Time64::from_secs(tairix_abi::time::RELEASE_EPOCH_SECS) {
        "That is before this system was made, so certificates will not be trusted and new \
         files will look older than files made before them."
    } else {
        "That is more than a century after this system was made, which no working clock reads, \
         so certificates will not be trusted and files will look as if they come from the future."
    };
    Some((headline, String::from(detail)))
}

/// `civil`'s date as a person reads it: `Sunday 29 March 2026`.
fn on_date(civil: &CivilTime, out: &mut String) {
    out.push_str(&full_date(civil));
}

/// What stepping the clock by hand came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClockSet {
    /// The kernel's answer to the step.
    pub clock: Result<(), Errno>,
    /// The clock chip's answer, where the step was made and the chip asked.
    pub chip: Option<Result<(), Errno>>,
}

impl ClockSet {
    /// The headline and detail an alert states a refused step in, or `None`
    /// where the clock was set.
    #[must_use]
    pub fn refusal(&self) -> Option<(&'static str, String)> {
        let Err(err) = self.clock else {
            return None;
        };
        let detail = if err == Errno::PermissionDenied {
            String::from("This account may not set the clock. Nothing was changed.")
        } else {
            alloc::format!("The system refused it ({err}). Nothing was changed.")
        };
        Some(("The clock was not set", detail))
    }

    /// What the dialog states after a step that took but whose clock chip
    /// did not, or `None` where there is nothing to add.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        match (self.clock, self.chip?) {
            (Ok(()), Err(Errno::NotFound)) => Some(String::from(
                "The clock was set. This machine has no clock chip to keep it, so it is lost at \
                 the next restart unless network time is on.",
            )),
            (Ok(()), Err(err)) => Some(alloc::format!(
                "The clock was set, but the clock chip refused it ({err}), so the time may be \
                 wrong after a restart."
            )),
            _ => None,
        }
    }
}

/// A change to the machine's time settings the dialog asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingChange {
    /// Turn network time on or off.
    Network(bool),
    /// Keep the machine on this zone, or the store's default for `None`.
    Zone(Option<ZoneName>),
}

impl SettingChange {
    /// The setting the settings service is asked for.
    #[must_use]
    pub fn setting(&self) -> (&'static str, &str) {
        match self {
            Self::Network(on) => MachineTime::network_setting(*on),
            Self::Zone(zone) => MachineTime::zone_setting(zone.as_ref()),
        }
    }

    /// The headline and detail an alert states a refused change in.
    #[must_use]
    pub fn refusal(&self, err: Errno) -> (&'static str, String) {
        let headline = match self {
            Self::Network(true) => "Network time was not turned on",
            Self::Network(false) => "Network time was not turned off",
            Self::Zone(_) => "The time zone was not changed",
        };
        let detail = match err {
            Errno::PermissionDenied => {
                String::from("This account may not change the machine's settings.")
            }
            Errno::NotFound => String::from("The settings service is not running."),
            Errno::BadMagic => String::from(
                "The machine's settings could not be read, so they were left as they are.",
            ),
            err => alloc::format!("The settings service refused it ({err})."),
        };
        (headline, detail)
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
