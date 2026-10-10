//! A span of time, the frames it holds at a rate, and how it reads as a clock.

use alloc::format;
use alloc::string::String;

/// A span of time, to the nanosecond.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Span {
    nanos: u64,
}

const NANOS_PER_SECOND: u128 = 1_000_000_000;

impl Span {
    /// No time at all.
    pub const ZERO: Self = Self { nanos: 0 };

    /// The span of `nanos` nanoseconds.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    /// The span in nanoseconds.
    #[must_use]
    pub const fn nanos(self) -> u64 {
        self.nanos
    }

    /// The whole frames this span holds at `hz`, rounded down.
    #[must_use]
    pub fn frames_at(self, hz: u32) -> u64 {
        let frames = u128::from(self.nanos) * u128::from(hz) / NANOS_PER_SECOND;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// The span `frames` cover at `hz`, rounded down.
    #[must_use]
    pub fn of_frames(frames: u64, hz: u32) -> Self {
        if hz == 0 {
            return Self::ZERO;
        }
        let nanos = u128::from(frames) * NANOS_PER_SECOND / u128::from(hz);
        Self::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// Whole seconds.
    #[must_use]
    pub const fn seconds(self) -> u64 {
        self.nanos / 1_000_000_000
    }

    /// This span and `other` together, saturating.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self::from_nanos(self.nanos.saturating_add(other.nanos))
    }

    /// This span less `other`, never below zero.
    #[must_use]
    pub const fn less(self, other: Self) -> Self {
        Self::from_nanos(self.nanos.saturating_sub(other.nanos))
    }

    /// The span as `m:ss`, or `h:mm:ss` past the hour.
    #[must_use]
    pub fn clock(self) -> String {
        let seconds = self.seconds();
        let (hours, minutes, seconds) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn a_span_counts_whole_frames_at_a_rate() {
        let one_and_a_half = Span::from_nanos(1_500_000_000);
        assert_eq!(one_and_a_half.frames_at(48_000), 72_000);
        assert_eq!(Span::from_nanos(1).frames_at(48_000), 0);
        assert_eq!(Span::of_frames(72_000, 48_000), one_and_a_half);
        assert_eq!(Span::of_frames(1, 0), Span::ZERO);
        assert_eq!(
            Span::from_nanos(u64::MAX).frames_at(768_000),
            14_167_099_448_608_935,
            "the product is carried wide, so the longest span is counted, not wrapped"
        );
        assert_eq!(Span::of_frames(u64::MAX, 1), Span::from_nanos(u64::MAX));
    }

    #[test]
    fn a_span_reads_as_a_clock() {
        assert_eq!(Span::from_nanos(0).clock(), "0:00");
        assert_eq!(Span::from_nanos(83_900_000_000).clock(), "1:23");
        assert_eq!(Span::from_nanos(3_725_000_000_000).clock(), "1:02:05");
    }

    #[test]
    fn sums_and_differences_saturate() {
        let max = Span::from_nanos(u64::MAX);
        assert_eq!(max.plus(Span::from_nanos(1)), max);
        assert_eq!(Span::ZERO.less(Span::from_nanos(1)), Span::ZERO);
        assert_eq!(
            Span::from_nanos(5).less(Span::from_nanos(3)),
            Span::from_nanos(2)
        );
    }
}
