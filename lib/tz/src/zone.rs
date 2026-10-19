//! A zone: the offset in force at any instant, and local civil time in both
//! directions.

use tairix_abi::time::{CivilTime, Time64, SECS_PER_MINUTE};

use crate::layout::{TrailingRecord, ZoneRecord, MAX_OFFSET};
use crate::store::ZoneInfo;

/// A time zone: a stored one by the name it was found by, or UTC where no
/// store says otherwise.
#[derive(Copy, Clone, Debug)]
pub struct Zone<'a> {
    stored: Option<Stored<'a>>,
}

#[derive(Copy, Clone, Debug)]
struct Stored<'a> {
    info: &'a ZoneInfo<'a>,
    index: u32,
    name: &'a str,
}

/// The offset a zone keeps at some instant.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZoneOffset<'a> {
    /// Seconds east of UT.
    pub utc_offset: i32,
    /// What the zone calls it.
    pub abbreviation: &'a str,
    /// Whether daylight saving is in force.
    pub daylight: bool,
}

impl ZoneOffset<'_> {
    /// Write the offset from UTC to the minute: `UTC+01:00`, `UTC-03:30`.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write_from_utc(&self, out: &mut dyn core::fmt::Write) -> core::fmt::Result {
        let sign = if self.utc_offset < 0 { '-' } else { '+' };
        let minutes = self.utc_offset.unsigned_abs() / 60;
        write!(out, "UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
    }
}

/// An instant as it reads in a zone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LocalTime<'a> {
    /// The local calendar fields.
    pub civil: CivilTime,
    /// The offset they were read at.
    pub offset: ZoneOffset<'a>,
}

/// Which instant a local time that names two is meant to name.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Disambiguation {
    /// Neither: such a time is an error.
    Reject,
    /// The earlier, before the clocks went back.
    Earlier,
    /// The later, after the clocks went back.
    Later,
}

/// Why local fields name no single instant.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LocalError {
    /// The fields are not a civil time at all.
    Invalid,
    /// The clocks skipped it: no instant reads so.
    Gap,
    /// The clocks passed it twice, and no choice was made between the two.
    Overlap,
}

/// The name UTC is shown by.
const UTC_NAME: &str = "UTC";

/// The offset UTC keeps, and every zone keeps where its store fails a read.
const UTC_OFFSET: ZoneOffset<'static> = ZoneOffset {
    utc_offset: 0,
    abbreviation: UTC_NAME,
    daylight: false,
};

impl<'a> Zone<'a> {
    /// Coordinated Universal Time.
    pub const UTC: Zone<'static> = Zone { stored: None };

    /// Zone `index` of `info`, found by `name`.
    pub(crate) const fn stored(info: &'a ZoneInfo<'a>, index: u32, name: &'a str) -> Self {
        Self {
            stored: Some(Stored { info, index, name }),
        }
    }

    /// The name it was found by, which may be an alias; `UTC` for UTC.
    #[must_use]
    pub fn name(&self) -> &'a str {
        self.stored.map_or(UTC_NAME, |stored| stored.name)
    }

    /// The offset in force at `time`.
    #[must_use]
    pub fn offset_at(&self, time: Time64) -> ZoneOffset<'a> {
        self.offset_at_secs(time.secs())
    }

    /// `time` as it reads in this zone.
    #[must_use]
    pub fn local(&self, time: Time64) -> LocalTime<'a> {
        let offset = self.offset_at(time);
        LocalTime {
            civil: CivilTime::at_offset(time.secs(), offset.utc_offset),
            offset,
        }
    }

    /// The instant `local` names in this zone, `choice` deciding between two.
    ///
    /// # Errors
    ///
    /// [`LocalError::Invalid`] for fields no calendar holds,
    /// [`LocalError::Gap`] for a time the clocks skipped, and
    /// [`LocalError::Overlap`] for one they passed twice when `choice` is
    /// [`Disambiguation::Reject`].
    pub fn instant_from_local(
        &self,
        local: &CivilTime,
        choice: Disambiguation,
    ) -> Result<Time64, LocalError> {
        let wall = local.to_unix_secs().ok_or(LocalError::Invalid)?;
        // An instant reading as `wall` lies within the largest offset of it,
        // so walking every stretch of one offset across that span finds them
        // all, in order.
        let reach = i64::from(MAX_OFFSET);
        let to = wall.saturating_add(reach);
        let mut start = wall.saturating_sub(reach);
        let (mut earliest, mut latest) = (None, None);
        loop {
            let (offset, end) = self.stretch(start);
            if let Some(candidate) = wall.checked_sub(i64::from(offset.utc_offset)) {
                if candidate >= start && end.is_none_or(|end| candidate < end) {
                    earliest.get_or_insert(candidate);
                    latest = Some(candidate);
                }
            }
            match end {
                Some(next) if next <= to => start = next,
                _ => break,
            }
        }
        match (earliest, latest) {
            (Some(first), Some(last)) if first == last => Ok(Time64::from_secs(first)),
            (Some(first), Some(last)) => match choice {
                Disambiguation::Earlier => Ok(Time64::from_secs(first)),
                Disambiguation::Later => Ok(Time64::from_secs(last)),
                Disambiguation::Reject => Err(LocalError::Overlap),
            },
            _ => Err(LocalError::Gap),
        }
    }

    fn offset_at_secs(&self, at: i64) -> ZoneOffset<'a> {
        self.locate(at)
            .and_then(|located| located.offset(at))
            .unwrap_or(UTC_OFFSET)
    }

    /// The first instant after `time` at which a clock showing this zone's
    /// minutes changes: its next local minute, or its next change of offset
    /// where that comes first.
    ///
    /// Local, so a zone whose offset is not a whole number of minutes turns
    /// its minutes on its own seconds. Later than `time` everywhere short of
    /// the end of the range.
    #[must_use]
    pub fn next_minute(&self, time: Time64) -> Time64 {
        let at = time.secs();
        let (offset, change) = self.stretch(at);
        let into_minute = (at.rem_euclid(SECS_PER_MINUTE) + i64::from(offset.utc_offset))
            .rem_euclid(SECS_PER_MINUTE);
        let minute = at.saturating_add(SECS_PER_MINUTE - into_minute);
        Time64::from_secs(change.map_or(minute, |change| change.min(minute)))
    }

    /// The first change of this zone's offset from UTC after `time`, or
    /// `None` where it keeps its offset for good.
    ///
    /// A change of abbreviation or daylight flag alone moves no clock, so it
    /// is passed over: Europe/Dublin calls its winter time daylight saving.
    #[must_use]
    pub fn next_change(&self, time: Time64) -> Option<OffsetChange<'a>> {
        let (mut before, mut next) = self.stretch(time.secs());
        for _ in 0..MAX_UNMOVED_CHANGES {
            let at = next?;
            let (after, following) = self.stretch(at);
            if after.utc_offset != before.utc_offset {
                return Some(OffsetChange {
                    at: Time64::from_secs(at),
                    before,
                    after,
                });
            }
            (before, next) = (after, following);
        }
        None
    }

    /// The offset in force at `at`, and the first instant after it at which
    /// that may change: one search for both.
    fn stretch(&self, at: i64) -> (ZoneOffset<'a>, Option<i64>) {
        self.locate(at).map_or((UTC_OFFSET, None), |located| {
            (
                located.offset(at).unwrap_or(UTC_OFFSET),
                located.next_change(at),
            )
        })
    }

    /// Where `at` falls in this zone's transitions; `None` for UTC.
    fn locate(&self, at: i64) -> Option<Located<'a>> {
        let Stored { info, index, .. } = self.stored?;
        let zone = info.zone_record(index)?;
        let last = zone.first.checked_add(zone.count)?.checked_sub(1)?;
        Some(Located {
            info,
            zone,
            position: last_at_or_before(info, zone.first, last, at),
            last,
        })
    }
}

/// A change of a zone's offset from UTC.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OffsetChange<'a> {
    /// The instant the new offset begins.
    pub at: Time64,
    /// The offset in force until then.
    pub before: ZoneOffset<'a>,
    /// The offset in force from then.
    pub after: ZoneOffset<'a>,
}

/// The most changes that move no clock [`Zone::next_change`] passes over
/// before it answers that none does: a bound on the walk, far past the few
/// renamings any zone makes between two changes of offset.
const MAX_UNMOVED_CHANGES: usize = 64;

/// An instant's place in a stored zone: the last of the zone's transitions at
/// or before it, and the zone's last transition.
struct Located<'a> {
    info: &'a ZoneInfo<'a>,
    zone: ZoneRecord,
    position: u32,
    last: u32,
}

impl<'a> Located<'a> {
    /// The offset in force at `at`: the transition's type, or past the last
    /// transition the zone's trailing rule.
    fn offset(&self, at: i64) -> Option<ZoneOffset<'a>> {
        let transition = self.info.transition(self.position)?;
        if self.position == self.last && transition.at < at {
            if let Some(rule) = self.info.trailing(self.zone.trailing) {
                let daylight = daylight_at(&rule, at);
                let phase = if daylight {
                    &rule.daylight
                } else {
                    &rule.standard
                };
                return Some(ZoneOffset {
                    utc_offset: rule.stdoff.saturating_add(phase.save),
                    abbreviation: self.info.abbrev(phase.abbrev),
                    daylight,
                });
            }
        }
        let kind = self.info.type_record(u32::from(transition.kind))?;
        Some(ZoneOffset {
            utc_offset: kind.offset,
            abbreviation: self.info.abbrev(kind.abbrev),
            daylight: kind.daylight,
        })
    }

    /// The first instant after `after` at which the offset may change.
    fn next_change(&self, after: i64) -> Option<i64> {
        if self.position < self.last {
            return self.info.transition(self.position + 1).map(|next| next.at);
        }
        let rule = self.info.trailing(self.zone.trailing)?;
        let last_at = self.info.transition(self.last)?.at;
        if after == last_at {
            // The rule takes over a second after the last transition.
            return last_at.checked_add(1);
        }
        changes_near(&rule, after)
            .map(|(at, _)| at)
            .filter(|at| *at > after)
            .min()
    }
}

/// The index of the last of transitions `first..=last` at or before `at`;
/// `first` when every one is later, which the store's `i64::MIN` first
/// transition rules out.
fn last_at_or_before(info: &ZoneInfo<'_>, first: u32, last: u32, at: i64) -> u32 {
    let (mut low, mut high) = (first, last);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if info
            .transition(middle)
            .is_some_and(|transition| transition.at <= at)
        {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

/// The changes `rule` makes in `year`, each with whether it begins daylight
/// saving; a change no instant can name is absent.
fn changes(rule: &TrailingRecord, year: i64) -> impl Iterator<Item = (i64, bool)> {
    let into_daylight = rule
        .daylight
        .moment
        .instant(year, rule.stdoff, rule.standard.save);
    let into_standard = rule
        .standard
        .moment
        .instant(year, rule.stdoff, rule.daylight.save);
    [(into_daylight, true), (into_standard, false)]
        .into_iter()
        .filter_map(|(at, daylight)| Some((at?, daylight)))
}

/// The changes `rule` makes from two years before `at`'s to two years after.
///
/// A rule's time may move a change most of a week past its date, so both of
/// a year's changes can land in the next January, and the latest change
/// before an instant there is then two years back; the window holds that and
/// the first change after any instant.
fn changes_near(rule: &TrailingRecord, at: i64) -> impl Iterator<Item = (i64, bool)> + '_ {
    let year = CivilTime::at_offset(at, rule.stdoff).year;
    (year - 2..=year + 2).flat_map(move |year| changes(rule, year))
}

/// Whether `rule` has daylight saving in force at `at`: the latest change at
/// or before it decides.
fn daylight_at(rule: &TrailingRecord, at: i64) -> bool {
    changes_near(rule, at)
        .filter(|(change, _)| *change <= at)
        .max_by_key(|(change, _)| *change)
        .is_some_and(|(_, daylight)| daylight)
}

#[cfg(test)]
#[path = "zone_tests.rs"]
mod tests;
