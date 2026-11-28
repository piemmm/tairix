//! What a session is started with beside its memory
//! (`plans/RISCOS-EMULATOR.md` §15.3): the real time in RISC OS's measure,
//! and the zone the desktop's clocks read, where its store holds it.

use tairix_abi::time::Time64;
use tairix_riscos_kernel::os::UNIX_EPOCH;
use tairix_riscos_session::protocol::Zoned;

/// Nanoseconds in a centisecond.
const CENTISECOND_NS: u32 = 10_000_000;

/// `time` as RISC OS reads the real time: centiseconds since 1900, UTC;
/// held to nought for a time before 1900.
#[must_use]
pub fn real_time(time: Time64) -> u64 {
    let centiseconds = i128::from(time.secs()) * 100
        + i128::from(time.subsec_nanos() / CENTISECOND_NS)
        + i128::from(UNIX_EPOCH);
    u64::try_from(centiseconds.max(0)).unwrap_or(u64::MAX)
}

/// The zone `name` of `store`, where both are known and the store holds the
/// zone: otherwise none, and the session reads UTC.
#[must_use]
pub fn zone<'a>(store: Option<&'a [u8]>, name: Option<&'a str>) -> Option<Zoned<'a>> {
    let (store, name) = (store?, name?);
    let info = tairix_tz::ZoneInfo::parse(store).ok()?;
    info.zone(name)?;
    Some(Zoned { store, name })
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::*;

    #[test]
    fn the_real_time_is_centiseconds_since_1900() {
        let unix = Time64::new(0, 0).unwrap();
        assert_eq!(real_time(unix), UNIX_EPOCH);
        let later = Time64::new(86_400, 990_000_000).unwrap();
        assert_eq!(real_time(later), UNIX_EPOCH + 8_640_099);
        let early = Time64::new(-(UNIX_EPOCH / 100).cast_signed() - 1, 0).unwrap();
        assert_eq!(real_time(early), 0, "held to 1900");
    }

    #[test]
    fn the_zone_goes_only_where_its_store_holds_it() {
        let store = tairix_tz::testkit::store();
        let name = String::from("Europe/London");
        let zoned = zone(Some(&store), Some(&name)).unwrap();
        assert_eq!(zoned.name, "Europe/London");
        assert!(zone(Some(&store), Some("Atlantis/Capital")).is_none());
        assert!(zone(None, Some(&name)).is_none(), "no store: UTC");
        assert!(zone(Some(&store), None).is_none(), "no zone named");
        assert!(zone(Some(b"not a store"), Some(&name)).is_none());
    }
}
