//! The civil-time keys: the region whose conventions the desktop's clocks are
//! written in, and the zone they read.
//!
//! Each is shape-checked when read, never against a zone store: whether the
//! store holds a code or a zone is the reader's question, since a choice
//! outlives the image that shipped it. [`civil_clock`] answers it once for
//! every reader of these settings.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::zone::ZoneName;
use tairix_tz::{resolve_region, resolve_zone, CivilClock, Decided, ZoneInfo};

use crate::DesktopSettings;

/// The clock `settings` ask for: their zone and region where `zones` holds
/// them, else `fallback` for the zone — the machine's own, for a reader who
/// chose none — else the store's own, else UTC with ISO 8601 dates; with what
/// decided each.
#[must_use]
pub fn civil_clock<'a>(
    zones: Option<&'a ZoneInfo<'a>>,
    settings: &DesktopSettings,
    fallback: Option<&str>,
) -> (CivilClock<'a>, (Decided, Decided)) {
    let zone: Vec<&str> = settings
        .time_zone
        .as_ref()
        .map(ZoneName::as_str)
        .into_iter()
        .chain(fallback)
        .collect();
    let region = settings.region.as_ref().map(RegionCode::as_str);
    let (zone, zone_decided) = resolve_zone(zones, &zone);
    let (region, region_decided) = resolve_region(zones, region.as_slice());
    (
        CivilClock::new(zone, region.as_ref()),
        (zone_decided, region_decided),
    )
}

/// The ISO 3166-1 alpha-2 code of a region the user chose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionCode(String);

impl RegionCode {
    /// `code` as a region code, or `None` unless it has one's shape.
    #[must_use]
    pub fn new(code: &str) -> Option<Self> {
        tairix_tz::is_region_code(code).then(|| Self(String::from(code)))
    }

    /// The code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    fn zones() -> &'static ZoneInfo<'static> {
        let bytes: &'static [u8] = Box::leak(tairix_tz::testkit::store().into_boxed_slice());
        Box::leak(Box::new(
            ZoneInfo::parse(bytes).expect("the fixture parses"),
        ))
    }

    fn settings(zone: Option<&str>, region: Option<&str>) -> DesktopSettings {
        DesktopSettings {
            time_zone: zone.and_then(ZoneName::new),
            region: region.and_then(RegionCode::new),
            ..DesktopSettings::default()
        }
    }

    #[test]
    fn a_choice_the_store_holds_is_read_and_says_so() {
        let (clock, decided) = civil_clock(
            Some(zones()),
            &settings(Some("America/New_York"), Some("US")),
            Some("Asia/Kolkata"),
        );
        assert_eq!(clock.zone().name(), "America/New_York");
        assert_eq!(decided, (Decided::Preference(0), Decided::Preference(0)));
        let (clock, decided) = civil_clock(Some(zones()), &settings(None, None), None);
        assert_eq!(clock.zone().name(), "Europe/London");
        assert_eq!(decided, (Decided::StoreDefault, Decided::StoreDefault));
    }

    #[test]
    fn a_reader_who_chose_no_zone_reads_the_fallback_before_the_stores_default() {
        let (clock, decided) =
            civil_clock(Some(zones()), &settings(None, None), Some("Asia/Kolkata"));
        assert_eq!(clock.zone().name(), "Asia/Kolkata");
        assert_eq!(decided.0, Decided::Preference(0));
        let (clock, decided) = civil_clock(
            Some(zones()),
            &settings(Some("Mars/Olympus"), None),
            Some("Asia/Kolkata"),
        );
        assert_eq!(clock.zone().name(), "Asia/Kolkata");
        assert_eq!(
            decided.0,
            Decided::Preference(1),
            "past a choice the store lacks"
        );
        let (clock, _) = civil_clock(Some(zones()), &settings(None, None), Some("Mars/Olympus"));
        assert_eq!(clock.zone().name(), "Europe/London");
    }

    #[test]
    fn a_choice_the_store_lacks_reads_the_stores_default_and_no_store_reads_utc() {
        let (clock, decided) = civil_clock(
            Some(zones()),
            &settings(Some("Mars/Olympus"), Some("ZZ")),
            None,
        );
        assert_eq!(clock.zone().name(), "Europe/London");
        assert_eq!(decided, (Decided::StoreDefault, Decided::StoreDefault));
        let (clock, decided) = civil_clock(
            None,
            &settings(Some("America/New_York"), Some("US")),
            Some("Asia/Kolkata"),
        );
        assert_eq!(clock.zone().name(), "UTC");
        assert_eq!(decided, (Decided::NoStore, Decided::NoStore));
    }
}
