//! The zone store's regions and places as the Language & Region rows offer
//! them, and the reading the Date & Time pane states.
//!
//! The store is the session's delegation, so every list here is built from
//! what the desktop itself reads its clocks by.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::Write;

use tairix_abi::time::{CivilTime, Time64};
use tairix_tz::{CivilClock, Place, ZoneInfo};
use tairix_wallpaper::{DesktopSettings, RegionCode, ZoneName};

/// The instant a region's patterns are shown at: a day and a month no reader
/// can mistake for each other, and an hour after noon, so the order of every
/// field and the clock's style both show.
const EXAMPLE: CivilTime = CivilTime {
    year: 2026,
    month: 12,
    day: 31,
    hour: 23,
    minute: 30,
    second: 0,
};

/// What a row offers before the store has been delegated, or where it could
/// not be.
const DEFAULT: &str = "Default";

/// The store's regions and places with the labels the rows show, in the order
/// a reader looks them up: built once, when the store arrives, so a choice
/// costs no sort.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CivilChoices {
    /// The store they come from.
    pub(crate) zones: &'static ZoneInfo<'static>,
    default_region: String,
    default_zone: String,
    regions: Vec<(String, RegionCode)>,
    places: Vec<(String, ZoneName)>,
}

impl CivilChoices {
    /// The choices `zones` offers.
    pub(crate) fn of(zones: &'static ZoneInfo<'static>) -> Self {
        let mut regions: Vec<(String, RegionCode)> = zones
            .regions()
            .filter_map(|region| Some((String::from(region.name), RegionCode::new(region.code)?)))
            .collect();
        regions.sort_by(|a, b| name_order(&a.0, &b.0));
        let mut places: Vec<(String, ZoneName)> = zones
            .places()
            .filter_map(|place| {
                Some((
                    place_label(zones, &place),
                    ZoneName::new(place.zone.name())?,
                ))
            })
            .collect();
        places.sort_by(|a, b| name_order(&a.0, &b.0));
        Self {
            zones,
            default_region: zones.default_region().map_or_else(
                || String::from(DEFAULT),
                |region| alloc::format!("{DEFAULT} ({})", region.name),
            ),
            default_zone: alloc::format!(
                "{DEFAULT} ({})",
                zone_label(zones, zones.default_zone().name())
            ),
            regions,
            places,
        }
    }
}

/// One row's choices: *Default* first, then — where the value held is one the
/// store does not offer — that value under its own spelling, so opening the
/// pane never changes it, then every offered value.
struct Ladder<'a, T> {
    default: &'a str,
    held: Option<&'a T>,
    offered: &'a [(String, T)],
}

impl<T: PartialEq> Ladder<'_, T> {
    /// The labels in order, and the index of the one `current` holds.
    fn choices(&self, current: Option<&T>, spell: impl Fn(&T) -> &str) -> (Vec<String>, usize) {
        let mut labels = Vec::with_capacity(self.offered.len() + 2);
        labels.push(String::from(self.default));
        labels.extend(self.held.map(|held| String::from(spell(held))));
        let skip = labels.len();
        labels.extend(self.offered.iter().map(|(label, _)| label.clone()));
        let at = match (current, self.held) {
            (None, _) => 0,
            (Some(_), Some(_)) => 1,
            (Some(value), None) => self
                .offered
                .iter()
                .position(|(_, offered)| offered == value)
                .map_or(0, |at| at + skip),
        };
        (labels, at)
    }

    /// What the choice at `index` sets, or `None` for an index naming no
    /// choice.
    fn value(&self, index: usize) -> Option<Choice<'_, T>> {
        let Some(index) = index.checked_sub(1) else {
            return Some(Choice::Default);
        };
        let offered = match self.held {
            Some(held) => match index.checked_sub(1) {
                None => return Some(Choice::Value(held)),
                Some(index) => index,
            },
            None => index,
        };
        self.offered
            .get(offered)
            .map(|(_, value)| Choice::Value(value))
    }
}

/// What one choice of a row sets.
enum Choice<'a, T> {
    /// The system's own.
    Default,
    /// This value.
    Value(&'a T),
}

impl<T: Clone> Choice<'_, T> {
    /// The setting this choice writes.
    fn setting(self) -> Option<T> {
        match self {
            Self::Default => None,
            Self::Value(value) => Some(value.clone()),
        }
    }
}

/// The Region row's ladder for `settings` over `civil`.
fn region_ladder<'a>(
    settings: &'a DesktopSettings,
    civil: Option<&'a CivilChoices>,
) -> Ladder<'a, RegionCode> {
    let offered = civil.map_or(&[][..], |civil| civil.regions.as_slice());
    Ladder {
        default: civil.map_or(DEFAULT, |civil| civil.default_region.as_str()),
        held: settings
            .region
            .as_ref()
            .filter(|code| !offered.iter().any(|(_, offered)| offered == *code)),
        offered,
    }
}

/// The Time zone row's ladder for `settings` over `civil`.
fn zone_ladder<'a>(
    settings: &'a DesktopSettings,
    civil: Option<&'a CivilChoices>,
) -> Ladder<'a, ZoneName> {
    let offered = civil.map_or(&[][..], |civil| civil.places.as_slice());
    Ladder {
        default: civil.map_or(DEFAULT, |civil| civil.default_zone.as_str()),
        held: settings
            .time_zone
            .as_ref()
            .filter(|name| !offered.iter().any(|(_, offered)| offered == *name)),
        offered,
    }
}

/// The regions the Region row offers, and which `settings` holds.
pub(crate) fn region_choices(
    settings: &DesktopSettings,
    civil: Option<&CivilChoices>,
) -> (Vec<String>, usize) {
    region_ladder(settings, civil).choices(settings.region.as_ref(), RegionCode::as_str)
}

/// Take the region at `index` of the Region row's choices onto `settings`,
/// answering whether it named one; fails closed on any other.
pub(crate) fn choose_region(
    index: usize,
    settings: &mut DesktopSettings,
    civil: Option<&CivilChoices>,
) -> bool {
    let Some(value) = region_ladder(settings, civil)
        .value(index)
        .map(Choice::setting)
    else {
        return false;
    };
    settings.region = value;
    true
}

/// The zones the Time zone row offers, and which `settings` holds.
pub(crate) fn zone_choices(
    settings: &DesktopSettings,
    civil: Option<&CivilChoices>,
) -> (Vec<String>, usize) {
    zone_ladder(settings, civil).choices(settings.time_zone.as_ref(), ZoneName::as_str)
}

/// Take the zone at `index` of the Time zone row's choices onto `settings`,
/// answering whether it named one; fails closed on any other.
pub(crate) fn choose_zone(
    index: usize,
    settings: &mut DesktopSettings,
    civil: Option<&CivilChoices>,
) -> bool {
    let Some(value) = zone_ladder(settings, civil)
        .value(index)
        .map(Choice::setting)
    else {
        return false;
    };
    settings.time_zone = value;
    true
}

/// How `clock`'s region writes a date and a time, shown at [`EXAMPLE`].
pub(crate) fn written_example(clock: &CivilClock<'_>) -> String {
    let local = tairix_tz::LocalTime {
        civil: EXAMPLE,
        offset: tairix_tz::ZoneOffset {
            utc_offset: 0,
            abbreviation: "",
            daylight: false,
        },
    };
    let mut date = String::new();
    let mut time = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = clock.write_date(&local, &mut date);
    let _ = clock.write_time(&local, &mut time);
    alloc::format!("Dates are written {date} and times {time}.")
}

/// `now` as `clock` reads it: the day written out, and the time as its
/// region writes it.
pub(crate) fn reading(now: Time64, clock: &CivilClock<'_>) -> String {
    let local = clock.local(now);
    let mut line = tairix_fsmeta::calendar::full_date(&local.civil);
    line.push(' ');
    // A `String` takes any write, so there is no failure to keep.
    let _ = clock.write_time(&local, &mut line);
    line
}

/// The zone `clock` reads, as a reader names it, with its abbreviation and
/// its offset from UTC at `now`; the name alone where no time is held, since
/// both are the zone's own only at an instant.
pub(crate) fn zone_reading<'a>(
    now: Option<Time64>,
    clock: &CivilClock<'_>,
    zones: Option<&'a ZoneInfo<'a>>,
) -> String {
    let zone = clock.zone();
    let mut line = zones.map_or_else(
        || String::from(zone.name()),
        |info| zone_label(info, zone.name()),
    );
    let Some(now) = now else {
        return line;
    };
    let offset = zone.offset_at(now);
    if offset.abbreviation == line && offset.utc_offset == 0 {
        return line;
    }
    if offset.abbreviation != line {
        line.push_str(", ");
        line.push_str(offset.abbreviation);
    }
    let secs = offset.utc_offset;
    let sign = if secs < 0 { '-' } else { '+' };
    let minutes = secs.unsigned_abs() / 60;
    // A `String` takes any write, so there is no failure to keep.
    let _ = write!(line, " (UTC{sign}{:02}:{:02})", minutes / 60, minutes % 60);
    line
}

/// The place `name` is, as a reader names it: the city and region the zone
/// table lists it by, else the name itself.
fn zone_label<'a>(info: &'a ZoneInfo<'a>, name: &str) -> String {
    info.places()
        .find(|place| place.zone.name() == name)
        .map_or_else(|| String::from(name), |place| place_label(info, &place))
}

/// `place` as `City, Region`.
fn place_label(info: &ZoneInfo<'_>, place: &Place<'_>) -> String {
    let mut label = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = place.write_city(&mut label);
    if let Some(region) = info.region(place.region) {
        label.push_str(", ");
        label.push_str(region.name);
    }
    label
}

/// The order a reader looks a name up in: letters without case or accent,
/// then the bytes, so distinct names never compare equal.
fn name_order(a: &str, b: &str) -> Ordering {
    a.chars()
        .map(unaccented)
        .cmp(b.chars().map(unaccented))
        .then_with(|| a.cmp(b))
}

/// `ch` for ordering: a Latin-1 letter folded to the letter it is written on,
/// in lower case, as a region's English name can carry one.
const fn unaccented(ch: char) -> char {
    match ch {
        'À'..='Å' | 'à'..='å' => 'a',
        'Ç' | 'ç' => 'c',
        'È'..='Ë' | 'è'..='ë' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' => 'i',
        'Ñ' | 'ñ' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' => 'u',
        'Ý' | 'ý' | 'ÿ' => 'y',
        other => other.to_ascii_lowercase(),
    }
}

#[cfg(test)]
#[path = "civil_tests.rs"]
mod tests;
