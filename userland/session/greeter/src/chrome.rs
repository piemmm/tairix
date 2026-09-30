//! The machine's identity and the clock drawn along the top of the screen.

use alloc::format;
use alloc::string::{String, ToString};

use tairix_abi::sysinfo::SystemIdentity;
use tairix_abi::time::{CivilTime, Time64};
use tairix_fsmeta::calendar::{full_date, hour_minute};
use tairix_greeter::Chrome;

/// Seconds in one minute.
const SECS_PER_MINUTE: i64 = 60;

/// The chrome as it was last told, so a reading in the minute already told
/// builds nothing.
///
/// The clock turns only with the minute and the identity not at all, so the
/// chrome is built afresh only when the minute differs from the one last
/// told: a screen refreshed for every frame of an animation formats no date
/// to learn that nothing changed.
pub struct Teller {
    identity: String,
    told: Told,
}

/// Which minute the chrome was last told for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Told {
    Never,
    /// This minute, or `None` for a screen holding no trusted time.
    Minute(Option<i64>),
}

impl Teller {
    /// A teller naming the machine by `identity`, which has told nothing yet.
    #[must_use]
    pub const fn new(identity: String) -> Self {
        Self {
            identity,
            told: Told::Never,
        }
    }

    /// The chrome for `now`, or `None` when it is the chrome last told.
    pub fn tell(&mut self, now: Option<Time64>) -> Option<Chrome> {
        let minute = Told::Minute(now.map(|now| now.secs().div_euclid(SECS_PER_MINUTE)));
        if minute == self.told {
            return None;
        }
        self.told = minute;
        Some(chrome(now, &self.identity))
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
        return String::from("TAIRiX");
    };
    let version = identity.version();
    match core::str::from_utf8(identity.hostname_bytes()) {
        Ok(host) if !host.is_empty() => format!("TAIRiX {version} ({host})"),
        _ => format!("TAIRiX {version}"),
    }
}

/// Build the chrome from a wall-clock reading and the machine's identity
/// line.
///
/// `now` is `None` when no trusted time is held, and the clock is then simply
/// absent: a login screen showing an invented time would be worse than one
/// showing none.
#[must_use]
pub fn chrome(now: Option<Time64>, identity: &str) -> Chrome {
    Chrome {
        identity: identity.to_string(),
        clock: now.map(clock_line).unwrap_or_default(),
    }
}

/// `now` as `Wednesday 30 September 2026 14:05`, in UTC like every clock on
/// the system.
fn clock_line(now: Time64) -> String {
    let civil = CivilTime::from_time64(now);
    format!("{} {}", full_date(&civil), hour_minute(&civil))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::sysinfo::MACHINE_ID_LEN;

    fn reported(host: &[u8]) -> SystemIdentity {
        SystemIdentity::new([0; MACHINE_ID_LEN], 0, 4, 12, host).expect("a short host name")
    }

    #[test]
    fn a_reading_becomes_the_day_written_out_and_the_minute() {
        let built = chrome(Some(Time64::from_secs(1_790_775_900)), "TAIRiX 0.4.12");
        assert_eq!(built.clock, "Wednesday 30 September 2026 13:45");
        assert_eq!(built.identity, "TAIRiX 0.4.12");
    }

    /// Either side of the epoch and of the 32-bit rollover alike.
    #[test]
    fn the_clock_reads_before_1970_and_after_2038() {
        let at = |secs: i64| chrome(Some(Time64::from_secs(secs)), "").clock;
        assert_eq!(at(-1), "Wednesday 31 December 1969 23:59");
        assert_eq!(at(2_147_483_648), "Tuesday 19 January 2038 03:14");
    }

    #[test]
    fn no_trusted_time_leaves_the_clock_empty() {
        let built = chrome(None, "TAIRiX 0.4.12");
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
        let mut teller = Teller::new("TAIRiX 0.4.12".to_string());
        let minute = Time64::from_secs(1_700_000_040);
        let told = teller
            .tell(Some(minute))
            .expect("the first reading is told");
        assert_eq!(told, chrome(Some(minute), "TAIRiX 0.4.12"));
        let later = Time64::from_secs(1_700_000_099);
        assert_eq!(teller.tell(Some(later)), None, "the same minute");
        let next = Time64::from_secs(1_700_000_100);
        assert_eq!(
            teller.tell(Some(next)),
            Some(chrome(Some(next), "TAIRiX 0.4.12"))
        );
    }

    #[test]
    fn a_teller_tells_a_clock_that_comes_and_goes() {
        let mut teller = Teller::new("TAIRiX".to_string());
        assert_eq!(teller.tell(None), Some(chrome(None, "TAIRiX")));
        assert_eq!(teller.tell(None), None);
        let set = Time64::from_secs(1_700_000_040);
        assert!(teller.tell(Some(set)).is_some(), "a clock that is set");
        assert!(teller.tell(None).is_some(), "and one that is lost again");
    }

    #[test]
    fn a_teller_that_forgot_tells_the_same_minute_again() {
        let mut teller = Teller::new("TAIRiX".to_string());
        let minute = Time64::from_secs(1_700_000_040);
        let _ = teller.tell(Some(minute));
        teller.forget();
        assert!(teller.tell(Some(minute)).is_some());
    }
}
