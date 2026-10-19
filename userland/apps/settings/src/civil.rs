//! The zone store's regions and places as the Language & Region rows offer
//! them, and the reading the Date & Time pane states.
//!
//! The store is the session's delegation, so every list here is built from
//! what the desktop itself reads its clocks by.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::{CivilTime, Time64};
use tairix_controls::zone::{self, Minute};
use tairix_controls::{PlaceDialog, PlaceRow};
use tairix_geometry::{Rect, Region, Scale};
use tairix_theme::Theme;
use tairix_tz::listing::{label as zone_label, Listing};
use tairix_tz::{CivilClock, ZoneInfo};
use tairix_util::utf8::reader_order;
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
    regions: Vec<(String, RegionCode)>,
    places: Listing<'static>,
}

impl CivilChoices {
    /// The choices `zones` offers, its places' readings written as its own
    /// default region writes a time.
    pub(crate) fn of(zones: &'static ZoneInfo<'static>) -> Self {
        let mut regions: Vec<(String, RegionCode)> = zones
            .regions()
            .filter_map(|region| Some((String::from(region.name), RegionCode::new(region.code)?)))
            .collect();
        regions.sort_by(|a, b| reader_order(&a.0, &b.0));
        Self {
            zones,
            default_region: zones.default_region().map_or_else(
                || String::from(DEFAULT),
                |region| alloc::format!("{DEFAULT} ({})", region.name),
            ),
            regions,
            places: Listing::of(zones, zones.default_region()),
        }
    }
}

/// One row's choices: *Default* first, then — where the value held is one the
/// store does not offer — that value under its own spelling, so opening the
/// pane never changes it, then every offered value.
struct Ladder<'a, T> {
    default: alloc::borrow::Cow<'a, str>,
    held: Option<&'a T>,
    offered: &'a [(String, T)],
}

impl<T: PartialEq> Ladder<'_, T> {
    /// The labels in order, and the index of the one `current` holds.
    fn choices(&self, current: Option<&T>, spell: impl Fn(&T) -> &str) -> (Vec<String>, usize) {
        let mut labels = Vec::with_capacity(self.offered.len() + 2);
        labels.push(String::from(self.default.as_ref()));
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
        default: alloc::borrow::Cow::Borrowed(
            civil.map_or(DEFAULT, |civil| civil.default_region.as_str()),
        ),
        held: settings
            .region
            .as_ref()
            .filter(|code| !offered.iter().any(|(_, offered)| offered == *code)),
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

/// The zone the account follows when it has chosen none: the `machine`'s
/// own where the store holds it, else the store's default.
fn machine_zone_label(civil: Option<&CivilChoices>, machine: Option<&ZoneName>) -> String {
    let Some(civil) = civil else {
        return machine.map_or_else(|| String::from("UTC"), |zone| String::from(zone.as_str()));
    };
    let name = machine
        .map(ZoneName::as_str)
        .filter(|name| civil.zones.zone(name).is_some())
        .unwrap_or_else(|| civil.zones.default_zone().name());
    zone_label(civil.zones, name)
}

/// What the Time zone row says the account's clocks read: the zone it chose,
/// or the `machine`'s it follows.
pub(crate) fn zone_held(
    settings: &DesktopSettings,
    civil: Option<&CivilChoices>,
    machine: Option<&ZoneName>,
) -> String {
    match (&settings.time_zone, civil) {
        (Some(zone), Some(civil)) => zone_label(civil.zones, zone.as_str()),
        (Some(zone), None) => String::from(zone.as_str()),
        (None, _) => alloc::format!(
            "{} (this machine's zone)",
            machine_zone_label(civil, machine)
        ),
    }
}

/// The chooser's rows for the Time zone row: *This machine's zone* first,
/// then every place, each read at `now`; and the row `settings` holds, where
/// one does.
pub(crate) fn zone_rows(
    settings: &DesktopSettings,
    civil: &CivilChoices,
    machine: Option<&ZoneName>,
    now: Option<Time64>,
) -> (Vec<PlaceRow>, Option<usize>) {
    let mut rows = Vec::with_capacity(civil.places.entries().len() + 1);
    rows.push(PlaceRow {
        name: String::from(MACHINE_ZONE),
        region: machine_zone_label(Some(civil), machine),
        reading: machine_reading(civil, machine, now),
        key: String::from(tairix_sysconfig::DEFAULT_ZONE),
    });
    rows.extend(zone::rows(&civil.places, now));
    let held = match &settings.time_zone {
        None => Some(0),
        Some(zone) => civil.places.index_of(zone).map(|at| at + 1),
    };
    (rows, held)
}

/// Rewrite the zone chooser's readings once the minute `written` holds has
/// turned: the machine's row, then every place, reporting what moved in a
/// dialog drawn in `frame`'s bounds.
pub(crate) fn refresh_zone_readings(
    civil: &CivilChoices,
    machine: Option<&ZoneName>,
    (dialog, written): (&mut PlaceDialog, &mut Minute),
    now: Time64,
    frame: (Rect, Scale, &Theme),
    damage: &mut Region,
) {
    if !zone::refresh(dialog, written, (&civil.places, 1), now, frame, damage) {
        return;
    }
    let (bounds, scale, theme) = frame;
    if let Some(list) = dialog.chooser_rect(bounds, scale, theme) {
        let reading = machine_reading(civil, machine, Some(now));
        dialog
            .chooser_mut()
            .set_reading(0, reading, list, scale, theme, damage);
    }
}

/// The reading of the zone the machine keeps, or the store's default where
/// it names none the store holds.
fn machine_reading(
    civil: &CivilChoices,
    machine: Option<&ZoneName>,
    now: Option<Time64>,
) -> String {
    let followed = machine
        .copied()
        .filter(|zone| civil.zones.zone(zone.as_str()).is_some())
        .or_else(|| ZoneName::new(civil.zones.default_zone().name()));
    followed.map_or_else(String::new, |zone| zone::reading(&civil.places, &zone, now))
}

/// What a row of the zone chooser sets the account's zone to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ZoneChoice {
    /// Follow the machine's zone.
    Machine,
    /// Keep this zone.
    Own(ZoneName),
}

impl ZoneChoice {
    /// The `time_zone` setting the choice writes.
    pub(crate) const fn setting(self) -> Option<ZoneName> {
        match self {
            Self::Machine => None,
            Self::Own(zone) => Some(zone),
        }
    }
}

/// What the zone chooser's `row` sets the account's zone to, or `None` for a
/// row the chooser was not built with.
pub(crate) fn zone_of_row(civil: &CivilChoices, row: usize) -> Option<ZoneChoice> {
    match row.checked_sub(1) {
        None => Some(ZoneChoice::Machine),
        Some(place) => civil.places.zone(place).map(ZoneChoice::Own),
    }
}

/// The chooser's first row: the account follows the machine's zone.
const MACHINE_ZONE: &str = "This machine's zone";

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
    // A `String` takes any write, so there is no failure to keep.
    line.push_str(" (");
    let _ = offset.write_from_utc(&mut line);
    line.push(')');
    line
}

#[cfg(test)]
#[path = "civil_tests.rs"]
mod tests;
