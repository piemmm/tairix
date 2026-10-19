//! The places a store lists, as a reader looks one up: by its city, told
//! apart by where it is, each with the local time there.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::zone::ZoneName;
use tairix_util::utf8::reader_order;

use crate::clock::CivilClock;
use crate::store::{Place, Region, ZoneInfo};

/// One place as a list names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listed {
    /// Its city.
    pub city: String,
    /// Its region, and whatever tells it from the region's other zones.
    pub region: String,
    /// The zone it keeps.
    pub zone: ZoneName,
}

/// Every place a store lists, by city, then by region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listing<'a> {
    info: &'a ZoneInfo<'a>,
    /// The conventions a reading is written in.
    region: Option<Region<'a>>,
    entries: Vec<Listed>,
}

impl<'a> Listing<'a> {
    /// Every place `info` lists, its readings written as `region` writes a
    /// time, or as ISO 8601 does without one.
    #[must_use]
    pub fn of(info: &'a ZoneInfo<'a>, region: Option<Region<'a>>) -> Self {
        let mut entries: Vec<Listed> = info
            .places()
            .filter_map(|place| listed(info, &place))
            .collect();
        entries.sort_by(|a, b| {
            reader_order(&a.city, &b.city).then_with(|| reader_order(&a.region, &b.region))
        });
        Self {
            info,
            region,
            entries,
        }
    }

    /// The places, in order.
    #[must_use]
    pub fn entries(&self) -> &[Listed] {
        &self.entries
    }

    /// The zone the place at `index` keeps.
    #[must_use]
    pub fn zone(&self, index: usize) -> Option<ZoneName> {
        self.entries.get(index).map(|entry| entry.zone)
    }

    /// Where the place keeping `zone` stands.
    #[must_use]
    pub fn index_of(&self, zone: &ZoneName) -> Option<usize> {
        self.entries.iter().position(|entry| entry.zone == *zone)
    }

    /// The local time `zone` reads at `now`, and its offset from UTC:
    /// `14:02 · UTC+01:00`; empty for a zone the store does not hold.
    #[must_use]
    pub fn reading(&self, zone: &ZoneName, now: Time64) -> String {
        let Some(zone) = self.info.zone(zone.as_str()) else {
            return String::new();
        };
        let clock = CivilClock::new(zone, self.region.as_ref());
        let local = clock.local(now);
        let mut reading = String::new();
        // A `String` takes any write, so there is no failure to keep.
        let _ = clock.write_time(&local, &mut reading);
        reading.push_str(" \u{b7} ");
        let _ = local.offset.write_from_utc(&mut reading);
        reading
    }
}

/// The place `name` is, as a reader names it — its city and region where the
/// zone table lists it — else the name itself.
#[must_use]
pub fn label(info: &ZoneInfo<'_>, name: &str) -> String {
    let mut label = String::new();
    // A `String` takes any write, so there is no failure to keep.
    match info.place(name) {
        Some(place) => {
            let _ = place.write_label(info, &mut label);
        }
        None => label.push_str(name),
    }
    label
}

/// `place` as a list names it, or `None` for a name no zone name can hold.
fn listed(info: &ZoneInfo<'_>, place: &Place<'_>) -> Option<Listed> {
    let zone = ZoneName::new(place.zone.name())?;
    let mut city = String::new();
    // A `String` takes any write, so there is no failure to keep.
    let _ = place.write_city(&mut city);
    let mut region = String::from(
        info.region(place.region)
            .map_or(place.region, |region| region.name),
    );
    if !place.comment.is_empty() {
        region.push_str(" \u{b7} ");
        region.push_str(place.comment);
    }
    Some(Listed { city, region, zone })
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
