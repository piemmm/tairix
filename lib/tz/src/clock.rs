//! What a reader sees: which zone and region apply to them, and the one
//! renderer of a reading's date and time.

use core::fmt::{self, Write};

use tairix_abi::time::Time64;

use crate::pattern::{Pattern, ISO_DATE, ISO_TIME};
use crate::store::{Region, ZoneInfo};
use crate::zone::{LocalTime, Zone};
use crate::Weekday;

/// What decided a resolution.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Decided {
    /// The preference at this position, the first the store holds.
    Preference(usize),
    /// The store's own default, no preference being one it holds.
    StoreDefault,
    /// No store: UTC, and no region's conventions.
    NoStore,
}

/// The zone `preferences` name, in order, else the store's default, else UTC.
/// A name the store does not hold is passed over, never guessed at.
#[must_use]
pub fn resolve_zone<'a>(
    store: Option<&'a ZoneInfo<'a>>,
    preferences: &[&str],
) -> (Zone<'a>, Decided) {
    let Some(store) = store else {
        return (Zone::UTC, Decided::NoStore);
    };
    preferences
        .iter()
        .enumerate()
        .find_map(|(position, name)| {
            store
                .zone(name)
                .map(|zone| (zone, Decided::Preference(position)))
        })
        .unwrap_or_else(|| (store.default_zone(), Decided::StoreDefault))
}

/// The region `preferences` name, in order, else the store's default; `None`
/// with no store.
#[must_use]
pub fn resolve_region<'a>(
    store: Option<&ZoneInfo<'a>>,
    preferences: &[&str],
) -> (Option<Region<'a>>, Decided) {
    let Some(store) = store else {
        return (None, Decided::NoStore);
    };
    preferences
        .iter()
        .enumerate()
        .find_map(|(position, code)| {
            store
                .region(code)
                .map(|region| (Some(region), Decided::Preference(position)))
        })
        .unwrap_or_else(|| (store.default_region(), Decided::StoreDefault))
}

/// How a time of day counts its hours.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HourCycle {
    /// `0` to `23`.
    TwentyFour,
    /// `12`, then `1` to `11`, twice a day, each half named `AM` or `PM`.
    Twelve,
}

/// A zone read with a region's conventions: the one renderer of a reading's
/// date and time every clock on the desktop draws through, and the one answer
/// to how a person there counts hours and weeks.
#[derive(Copy, Clone, Debug)]
pub struct CivilClock<'a> {
    zone: Zone<'a>,
    date: Pattern<'a>,
    time: Pattern<'a>,
    first_weekday: Weekday,
}

impl<'a> CivilClock<'a> {
    /// UTC, with ISO 8601's date, a 24-hour clock and weeks from Monday: what
    /// a machine with no store shows.
    pub const UTC: CivilClock<'static> = CivilClock {
        zone: Zone::UTC,
        date: ISO_DATE,
        time: ISO_TIME,
        first_weekday: Weekday::Monday,
    };

    /// `zone` read with `region`'s conventions, or ISO 8601's where there is
    /// no region.
    #[must_use]
    pub fn new(zone: Zone<'a>, region: Option<&Region<'a>>) -> Self {
        Self {
            zone,
            date: region.map_or(ISO_DATE, |region| region.date),
            time: region.map_or(ISO_TIME, |region| region.time),
            first_weekday: region.map_or(Weekday::Monday, |region| region.first_weekday),
        }
    }

    /// The zone it reads.
    #[must_use]
    pub const fn zone(&self) -> Zone<'a> {
        self.zone
    }

    /// How its time of day counts hours.
    #[must_use]
    pub fn hour_cycle(&self) -> HourCycle {
        if self.time.twelve_hour() {
            HourCycle::Twelve
        } else {
            HourCycle::TwentyFour
        }
    }

    /// The day its calendar week begins on.
    #[must_use]
    pub const fn first_weekday(&self) -> Weekday {
        self.first_weekday
    }

    /// `time` as this clock reads it.
    #[must_use]
    pub fn local(&self, time: Time64) -> LocalTime<'a> {
        self.zone.local(time)
    }

    /// Write `local`'s numeric date.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write_date(&self, local: &LocalTime<'_>, out: &mut dyn Write) -> fmt::Result {
        self.date.write(&local.civil, out)
    }

    /// Write `local`'s time of day.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write_time(&self, local: &LocalTime<'_>, out: &mut dyn Write) -> fmt::Result {
        self.time.write(&local.civil, out)
    }

    /// Write `local`'s time of day to the second.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write_time_with_seconds(
        &self,
        local: &LocalTime<'_>,
        out: &mut dyn Write,
    ) -> fmt::Result {
        self.time.write_with_seconds(&local.civil, out)
    }
}

impl Default for CivilClock<'_> {
    fn default() -> Self {
        CivilClock::UTC
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
