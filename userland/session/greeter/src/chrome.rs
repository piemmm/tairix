//! The machine's identity and the clock drawn along the top of the screen.

use alloc::format;
use alloc::string::{String, ToString};

use tairix_abi::sysinfo::{SystemIdentity, SYSTEM_NAME};
use tairix_abi::time::Time64;
use tairix_fsmeta::calendar::full_date;
use tairix_greeter::Chrome;
use tairix_tz::CivilClock;

/// The chrome as it was last told, so a reading in the minute already told
/// builds nothing.
///
/// The clock turns only with the minute and the identity not at all, so the
/// chrome is built afresh only once the minute told has ended: a screen
/// refreshed for every frame of an animation formats no date to learn that
/// nothing changed.
pub struct Teller {
    identity: String,
    civil: CivilClock<'static>,
    told: Told,
}

/// What the chrome was last told for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Told {
    Never,
    /// A screen holding no trusted time.
    Untimed,
    /// A reading at `since`, right until the zone's next minute `until`, or
    /// for good where the range holds no later one.
    Timed {
        since: Time64,
        until: Option<Time64>,
    },
}

impl Told {
    /// Whether a reading at `now` shows what was told: read no earlier than
    /// the reading told, and before its minute ended. A clock stepped back is
    /// told afresh.
    fn covers(self, now: Time64) -> bool {
        matches!(self, Self::Timed { since, until }
            if since <= now && until.is_none_or(|until| now < until))
    }
}

impl Teller {
    /// A teller naming the machine by `identity` and telling the time in
    /// `civil`, which has told nothing yet.
    #[must_use]
    pub const fn new(identity: String, civil: CivilClock<'static>) -> Self {
        Self {
            identity,
            civil,
            told: Told::Never,
        }
    }

    /// The chrome for `now`, or `None` when it is the chrome last told.
    pub fn tell(&mut self, now: Option<Time64>) -> Option<Chrome> {
        let told = match now {
            None if self.told == Told::Untimed => return None,
            None => Told::Untimed,
            Some(now) if self.told.covers(now) => return None,
            Some(now) => Told::Timed {
                since: now,
                until: self.next_minute(now),
            },
        };
        self.told = told;
        Some(chrome(now, &self.identity, &self.civil))
    }

    /// When a clock read at `now` next changes, or `None` where the range
    /// holds no later minute.
    #[must_use]
    pub fn next_minute(&self, now: Time64) -> Option<Time64> {
        Some(self.civil.zone().next_minute(now)).filter(|next| *next > now)
    }

    /// Forget what was told, so the next reading is told again: the surface
    /// it was told to has gone.
    pub fn forget(&mut self) {
        self.told = Told::Never;
    }
}

/// What the machine is, as far as `identity` says: the OS, its version, and
/// the machine's name.
///
/// No identity names the OS alone, and a name that is empty or is not text
/// is left out: the line never shows a version or a name it was not given.
#[must_use]
pub fn identity_line(identity: Option<&SystemIdentity>) -> String {
    let Some(identity) = identity else {
        return String::from(SYSTEM_NAME);
    };
    let version = identity.version();
    match core::str::from_utf8(identity.hostname_bytes()) {
        Ok(host) if !host.is_empty() => format!("{SYSTEM_NAME} {version} ({host})"),
        _ => format!("{SYSTEM_NAME} {version}"),
    }
}

/// Build the chrome from a wall-clock reading, told in `civil`, and the
/// machine's identity line.
///
/// `now` is `None` when no trusted time is held, and the clock is then simply
/// absent: a login screen showing an invented time would be worse than one
/// showing none.
#[must_use]
pub fn chrome(now: Option<Time64>, identity: &str, civil: &CivilClock<'_>) -> Chrome {
    Chrome {
        identity: identity.to_string(),
        clock: now.map(|now| clock_line(now, civil)).unwrap_or_default(),
    }
}

/// `now` read in `civil`: the day written out, and the time as its region
/// writes it — `Wednesday 30 September 2026 14:05`.
fn clock_line(now: Time64, civil: &CivilClock<'_>) -> String {
    let local = civil.local(now);
    let mut line = format!("{} ", full_date(&local.civil));
    // A `String` takes any write, so there is no failure to keep.
    let _ = civil.write_time(&local, &mut line);
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::sysinfo::MACHINE_ID_LEN;

    fn reported(host: &[u8]) -> SystemIdentity {
        SystemIdentity::new([0; MACHINE_ID_LEN], 0, 4, 12, host).expect("a short host name")
    }

    const UTC: CivilClock<'static> = CivilClock::UTC;

    #[test]
    fn a_reading_becomes_the_day_written_out_and_the_minute() {
        let built = chrome(
            Some(Time64::from_secs(1_790_775_900)),
            "TAIRiX 0.4.12",
            &UTC,
        );
        assert_eq!(built.clock, "Wednesday 30 September 2026 13:45");
        assert_eq!(built.identity, "TAIRiX 0.4.12");
    }

    #[test]
    fn the_clock_reads_the_stores_default_zone_and_region() {
        let bytes = tairix_tz::testkit::store();
        let zones = tairix_tz::ZoneInfo::parse(&bytes).expect("the fixture parses");
        let london = CivilClock::new(zones.default_zone(), zones.default_region().as_ref());
        let built = chrome(Some(Time64::from_secs(1_790_775_900)), "", &london);
        assert_eq!(
            built.clock, "Wednesday 30 September 2026 14:45",
            "summer time"
        );
        let us = zones.region("US");
        let new_york = CivilClock::new(
            zones.zone("America/New_York").expect("New York"),
            us.as_ref(),
        );
        let built = chrome(Some(Time64::from_secs(1_790_775_900)), "", &new_york);
        assert_eq!(built.clock, "Wednesday 30 September 2026 9:45 AM");
    }

    /// Either side of the epoch and of the 32-bit rollover alike.
    #[test]
    fn the_clock_reads_before_1970_and_after_2038() {
        let at = |secs: i64| chrome(Some(Time64::from_secs(secs)), "", &UTC).clock;
        assert_eq!(at(-1), "Wednesday 31 December 1969 23:59");
        assert_eq!(at(2_147_483_648), "Tuesday 19 January 2038 03:14");
    }

    #[test]
    fn no_trusted_time_leaves_the_clock_empty() {
        let built = chrome(None, "TAIRiX 0.4.12", &UTC);
        assert!(built.clock.is_empty());
        assert_eq!(built.identity, "TAIRiX 0.4.12");
    }

    #[test]
    fn the_identity_names_the_version_and_the_machine() {
        assert_eq!(
            identity_line(Some(&reported(b"lovelace"))),
            "TAIRiX 0.4.12 (lovelace)"
        );
    }

    /// An unprovisioned machine has no name, and a name that is not text is
    /// not shown as one.
    #[test]
    fn a_nameless_machine_is_named_by_its_version_alone() {
        assert_eq!(identity_line(Some(&reported(b""))), "TAIRiX 0.4.12");
        assert_eq!(
            identity_line(Some(&reported(&[0xff, 0xfe]))),
            "TAIRiX 0.4.12"
        );
    }

    #[test]
    fn an_unreadable_identity_names_the_system_alone() {
        assert_eq!(identity_line(None), "TAIRiX");
    }

    #[test]
    fn a_teller_builds_the_chrome_once_a_minute() {
        let mut teller = Teller::new("TAIRiX 0.4.12".to_string(), UTC);
        let minute = Time64::from_secs(1_700_000_040);
        let told = teller
            .tell(Some(minute))
            .expect("the first reading is told");
        assert_eq!(told, chrome(Some(minute), "TAIRiX 0.4.12", &UTC));
        let later = Time64::from_secs(1_700_000_099);
        assert_eq!(teller.tell(Some(later)), None, "the same minute");
        let next = Time64::from_secs(1_700_000_100);
        assert_eq!(
            teller.tell(Some(next)),
            Some(chrome(Some(next), "TAIRiX 0.4.12", &UTC))
        );
    }

    #[test]
    fn a_teller_tells_a_clock_that_comes_and_goes() {
        let mut teller = Teller::new("TAIRiX".to_string(), UTC);
        assert_eq!(teller.tell(None), Some(chrome(None, "TAIRiX", &UTC)));
        assert_eq!(teller.tell(None), None);
        let set = Time64::from_secs(1_700_000_040);
        assert!(teller.tell(Some(set)).is_some(), "a clock that is set");
        assert!(teller.tell(None).is_some(), "and one that is lost again");
    }

    /// A clock stepped back is told again, though it reads in a minute
    /// already told.
    #[test]
    fn a_teller_tells_a_clock_stepped_back() {
        let mut teller = Teller::new("TAIRiX".to_string(), UTC);
        let _ = teller.tell(Some(Time64::from_secs(1_700_000_070)));
        assert!(teller
            .tell(Some(Time64::from_secs(1_700_000_050)))
            .is_some());
        assert_eq!(teller.tell(Some(Time64::from_secs(1_700_000_060))), None);
    }

    /// The minute is the zone's own: local mean time turns on its own
    /// seconds, and only the end of the range has no later minute.
    #[test]
    fn a_teller_turns_on_its_zones_own_minute() {
        let bytes: &'static [u8] =
            alloc::boxed::Box::leak(tairix_tz::testkit::store().into_boxed_slice());
        let zones = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            tairix_tz::ZoneInfo::parse(bytes).expect("the fixture parses"),
        ));
        let mut teller = Teller::new(
            String::new(),
            CivilClock::new(zones.default_zone(), zones.default_region().as_ref()),
        );
        // 00:00:30 UT on 1 January 1800 reads 23:59:15 local mean time.
        let mean = Time64::from_secs(tairix_tz::testkit::utc(1800, 1, 1, 0, 0) + 30);
        let turns = Time64::from_secs(mean.secs() + 45);
        assert_eq!(teller.next_minute(mean), Some(turns));
        let _ = teller.tell(Some(mean));
        assert_eq!(teller.tell(Some(Time64::from_secs(turns.secs() - 1))), None);
        assert!(teller.tell(Some(turns)).is_some());
        let last = Time64::new(i64::MAX, 5).expect("a canonical instant");
        assert_eq!(Teller::new(String::new(), UTC).next_minute(last), None);
    }

    #[test]
    fn a_teller_that_forgot_tells_the_same_minute_again() {
        let mut teller = Teller::new("TAIRiX".to_string(), UTC);
        let minute = Time64::from_secs(1_700_000_040);
        let _ = teller.tell(Some(minute));
        teller.forget();
        assert!(teller.tell(Some(minute)).is_some());
    }
}
