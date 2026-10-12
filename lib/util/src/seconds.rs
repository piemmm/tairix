//! The decimal-seconds operand a network tool's timing option takes — `ping
//! -i 0.2`, `traceroute -w 2.5` — as nanoseconds.
//!
//! Whole seconds with up to nine fractional digits; no sign, no exponent, no
//! unit suffix. A value is bounded to a year, so its nanosecond form cannot
//! overflow and a typo cannot become a multi-day wait.

/// Nanoseconds in a second.
const NANOS_PER_SEC: u64 = tairix_abi::time::NANOS_PER_SEC as u64;

/// The largest operand, in whole seconds.
pub const MAX_SECONDS: u64 = 31_536_000;

/// `value` as nanoseconds, or [`None`] when it is not a decimal number of
/// seconds or exceeds [`MAX_SECONDS`].
#[must_use]
pub fn parse_seconds(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|b| b.is_ascii_digit())
        || fraction.len() > 9
    {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    if whole > MAX_SECONDS {
        return None;
    }
    let mut nanos: u64 = 0;
    let mut scale = NANOS_PER_SEC;
    for digit in fraction.bytes() {
        scale /= 10;
        nanos += u64::from(digit - b'0') * scale;
    }
    whole.checked_mul(NANOS_PER_SEC)?.checked_add(nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_and_fractional_seconds_become_nanoseconds() {
        assert_eq!(parse_seconds("1"), Some(1_000_000_000));
        assert_eq!(parse_seconds("0.5"), Some(500_000_000));
        assert_eq!(parse_seconds(".25"), Some(250_000_000));
        assert_eq!(parse_seconds("2."), Some(2_000_000_000));
        assert_eq!(parse_seconds("0.000000001"), Some(1));
        assert_eq!(parse_seconds("0"), Some(0));
    }

    #[test]
    fn anything_but_a_plain_decimal_is_refused() {
        for bad in [
            "",
            ".",
            "-1",
            "+1",
            "1e3",
            "1.2.3",
            "1s",
            " 1",
            "0.0000000001",
            "١",
        ] {
            assert_eq!(parse_seconds(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_value_past_a_year_is_refused() {
        assert_eq!(parse_seconds("31536000"), Some(MAX_SECONDS * NANOS_PER_SEC));
        assert_eq!(parse_seconds("31536001"), None);
        assert_eq!(parse_seconds("99999999999999999999999"), None);
    }
}
