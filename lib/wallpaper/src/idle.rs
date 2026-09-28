//! The idle policy the desktop document carries: when the screensaver starts,
//! when the display is switched off, and when the screen locks.
//!
//! The screensaver and the lock count from the last input; switching the
//! display off counts from the moment the screensaver starts, so it is part
//! of the screensaver and a desktop with none never switches its display
//! off. The document spells each wait in whole minutes, or `never`.

use alloc::format;
use alloc::string::{String, ToString};

use tairix_abi::time::Duration64;

use crate::input::parse_decimal;

/// The longest wait any idle setting may name: a day.
pub const MAX_WAIT_MINUTES: u16 = 24 * 60;

/// A wait of whole minutes, at least `LEAST`, or never.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IdleWait<const LEAST: u16> {
    /// It never elapses.
    Never,
    /// This many whole minutes, within `LEAST..=`[`MAX_WAIT_MINUTES`].
    Minutes(u16),
}

/// How long the desktop may sit idle before an idle action, or never.
pub type IdleAfter = IdleWait<1>;

/// How long after the screensaver starts the display is switched off, or
/// never; nought switches it off as the screensaver starts.
pub type DisplayOffAfter = IdleWait<0>;

impl<const LEAST: u16> IdleWait<LEAST> {
    const NEVER: &'static str = "never";

    /// The wait as a span, or `None` for [`Self::Never`].
    #[must_use]
    pub fn span(self) -> Option<Duration64> {
        match self {
            Self::Never => None,
            Self::Minutes(minutes) => Some(Duration64::from_secs(i64::from(minutes) * 60)),
        }
    }

    /// Decode a value spelling: `never`, or a whole number of minutes.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        if value == Self::NEVER {
            return Some(Self::Never);
        }
        let minutes = u16::try_from(parse_decimal(value)?).ok()?;
        (LEAST..=MAX_WAIT_MINUTES)
            .contains(&minutes)
            .then_some(Self::Minutes(minutes))
    }

    /// The canonical value spelling.
    #[must_use]
    pub fn render_value(self) -> String {
        match self {
            Self::Never => Self::NEVER.to_string(),
            Self::Minutes(minutes) => format!("{minutes}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::time::Duration64;

    use super::{DisplayOffAfter, IdleAfter};

    #[test]
    fn an_idle_wait_is_never_or_a_bounded_number_of_minutes() {
        assert_eq!(IdleAfter::from_value("never"), Some(IdleAfter::Never));
        assert_eq!(IdleAfter::from_value("5"), Some(IdleAfter::Minutes(5)));
        assert_eq!(
            IdleAfter::from_value("1440"),
            Some(IdleAfter::Minutes(1440))
        );
        for bad in ["0", "1441", "Never", "5m", "-5", ""] {
            assert_eq!(IdleAfter::from_value(bad), None, "{bad:?}");
        }
        for wait in [
            IdleAfter::Never,
            IdleAfter::Minutes(1),
            IdleAfter::Minutes(90),
        ] {
            assert_eq!(IdleAfter::from_value(&wait.render_value()), Some(wait));
        }
    }

    #[test]
    fn an_idle_wait_is_its_span_in_whole_minutes() {
        assert_eq!(IdleAfter::Never.span(), None);
        assert_eq!(
            IdleAfter::Minutes(5).span(),
            Some(Duration64::from_secs(300))
        );
    }

    /// Zero is meaningful here, where it is not for an idle wait: it switches
    /// the display off the moment the screensaver starts.
    #[test]
    fn a_display_off_wait_may_be_immediate_and_is_otherwise_bounded() {
        assert_eq!(
            DisplayOffAfter::from_value("0"),
            Some(DisplayOffAfter::Minutes(0))
        );
        assert_eq!(DisplayOffAfter::Minutes(0).span(), Some(Duration64::ZERO));
        assert_eq!(
            DisplayOffAfter::from_value("never"),
            Some(DisplayOffAfter::Never)
        );
        assert_eq!(DisplayOffAfter::Never.span(), None);
        assert_eq!(
            DisplayOffAfter::Minutes(120).span(),
            Some(Duration64::from_secs(7_200))
        );
        for bad in ["1441", "-1", "1h", "Never", ""] {
            assert_eq!(DisplayOffAfter::from_value(bad), None, "{bad:?}");
        }
        for wait in [
            DisplayOffAfter::Never,
            DisplayOffAfter::Minutes(0),
            DisplayOffAfter::Minutes(1440),
        ] {
            assert_eq!(
                DisplayOffAfter::from_value(&wait.render_value()),
                Some(wait)
            );
        }
    }
}
