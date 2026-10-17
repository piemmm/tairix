//! Rates from the cumulative counters the System Information API serves.
//!
//! The service reports tallies, never rates (`plans/FIX-IO.md`), so every
//! surface that shows throughput divides two readings by the interval between
//! them. This is that division, once, so a device reads at one rate on every
//! surface that samples it over the same interval.

/// Nanoseconds in a second, at the width the division is done in.
const NANOS_PER_SEC: u128 = tairix_abi::time::NANOS_PER_SEC as u128;

/// A per-second rate from two readings of one cumulative counter and the
/// interval between them, or `None` when the interval is unmeasured or zero:
/// there is nothing to divide by.
///
/// A counter that went *backwards* — which a cumulative counter should never
/// do — yields `0` rather than a wrapped, enormous rate: the delta saturates,
/// so a bad reading understates rather than invents. The arithmetic is in
/// [`u128`], so a fast device over a short interval cannot overflow before the
/// division brings it back into range.
#[must_use]
pub fn rate_per_sec(now: u64, previous: u64, elapsed_ns: Option<u64>) -> Option<u64> {
    let elapsed_ns = elapsed_ns.filter(|ns| *ns > 0)?;
    let delta = u128::from(now.saturating_sub(previous));
    let per_sec = delta.saturating_mul(NANOS_PER_SEC) / u128::from(elapsed_ns);
    Some(u64::try_from(per_sec).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::rate_per_sec;

    const SECOND: u64 = 1_000_000_000;

    #[test]
    fn a_rate_is_the_delta_over_the_interval() {
        assert_eq!(rate_per_sec(3_000, 1_000, Some(SECOND)), Some(2_000));
        assert_eq!(rate_per_sec(3_000, 1_000, Some(SECOND / 2)), Some(4_000));
        assert_eq!(rate_per_sec(1_000, 1_000, Some(SECOND)), Some(0));
    }

    #[test]
    fn an_unmeasured_interval_has_no_rate() {
        assert_eq!(rate_per_sec(3_000, 1_000, None), None);
        assert_eq!(rate_per_sec(3_000, 1_000, Some(0)), None);
    }

    #[test]
    fn a_counter_that_went_backwards_understates_rather_than_wraps() {
        assert_eq!(rate_per_sec(1_000, 3_000, Some(SECOND)), Some(0));
    }

    #[test]
    fn a_fast_device_over_a_short_interval_saturates_rather_than_overflows() {
        assert_eq!(rate_per_sec(u64::MAX, 0, Some(1)), Some(u64::MAX));
        assert_eq!(rate_per_sec(u64::MAX, 0, Some(SECOND)), Some(u64::MAX));
        assert_eq!(
            rate_per_sec(u64::MAX, 0, Some(2 * SECOND)),
            Some(u64::MAX / 2)
        );
    }
}
