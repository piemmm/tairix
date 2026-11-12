//! A directory entry's stamps: when it was created — to the hundredth — last
//! written, and last read, the last a date alone. The date and time are
//! `lib/abi`'s [`DosTime`], read and written as UTC: FAT records no zone,
//! and TAIRiX keeps zones out of what it stores. A field that names no date
//! reads as the epoch, the no-stamp value, never a guessed one.

use tairix_abi::driver::filesystem::StampField;
use tairix_abi::time::{DosTime, Time64};
use tairix_abi::StampChange;

pub use tairix_abi::driver::filesystem::DOS_TIME as WRITTEN;

/// What a last-read date holds: the days a DOS date names, 1980 to 2107.
pub const ACCESSED: StampField = StampField::new(
    DosTime::EARLIEST,
    86_400_000_000_000,
    0..(DosTime::LATEST.secs() - DosTime::EARLIEST.secs()).unsigned_abs() as u128 / 86_400 + 1,
);

/// An instant as a directory entry holds it: a date, a time to two seconds,
/// and the hundredths within them a creation stamp adds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stamp {
    /// The date: day, month and year since 1980.
    pub date: u16,
    /// The time: seconds over two, minutes and hours.
    pub time: u16,
    /// Hundredths past the two seconds `time` counts, 0 to 199.
    pub centis: u8,
}

impl Stamp {
    /// No stamp: the zero date FAT reads as none.
    pub const NONE: Self = Self {
        date: 0,
        time: 0,
        centis: 0,
    };

    /// `time` as an entry holds it, or `None` outside the 1980–2107 the
    /// fields reach, never a clamped date.
    #[must_use]
    pub fn of(time: Time64) -> Option<Self> {
        let DosTime { date, time: clock } = DosTime::from_time64(time)?;
        let odd = u32::from(time.secs().rem_euclid(2) == 1);
        Some(Self {
            date,
            time: clock,
            centis: u8::try_from(odd * 100 + time.subsec_nanos() / 10_000_000).ok()?,
        })
    }

    /// The instant the stamp holds, to the hundredth, or the epoch for one
    /// that names no date and time.
    #[must_use]
    pub fn instant(self) -> Time64 {
        let Some(base) = (DosTime {
            date: self.date,
            time: self.time,
        })
        .to_time64() else {
            return Time64::UNIX_EPOCH;
        };
        if self.centis > 199 {
            return Time64::UNIX_EPOCH;
        }
        Time64::new(
            base.secs() + i64::from(self.centis / 100),
            u32::from(self.centis % 100) * 10_000_000,
        )
        .unwrap_or(Time64::UNIX_EPOCH)
    }
}

/// A short entry's three stamps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stamps {
    /// When the entry was created, to the hundredth.
    pub created: Stamp,
    /// The date it was last read.
    pub accessed: u16,
    /// When it was last written, to two seconds.
    pub written: Stamp,
}

impl Stamps {
    /// No stamps at all.
    pub const NONE: Self = Self {
        created: Stamp::NONE,
        accessed: 0,
        written: Stamp::NONE,
    };

    /// A new entry's stamps, every one `now` — the access date too — or
    /// none while the time is unknown or outside the fields' years.
    #[must_use]
    pub fn at(now: Option<Time64>) -> Self {
        let Some(stamp) = now.and_then(Stamp::of) else {
            return Self::NONE;
        };
        Self {
            created: stamp,
            accessed: stamp.date,
            written: Stamp { centis: 0, ..stamp },
        }
    }

    /// When the entry was created.
    #[must_use]
    pub fn created(&self) -> Time64 {
        self.created.instant()
    }

    /// When it was last written.
    #[must_use]
    pub fn written(&self) -> Time64 {
        Stamp {
            centis: 0,
            ..self.written
        }
        .instant()
    }

    /// These stamps with the last-read date and last-write stamp changed as
    /// `accessed` and `written` ask, a now reading `now`; an instant outside
    /// the fields, or a now while the clock is unset, records no stamp.
    #[must_use]
    pub fn changed(self, accessed: StampChange, written: StampChange, now: Option<Time64>) -> Self {
        let instant = |change| match change {
            StampChange::Keep => None,
            StampChange::Now => Some(now),
            StampChange::At(at) => Some(Some(at)),
        };
        let stamp = |at: Option<Time64>| at.and_then(Stamp::of).unwrap_or(Stamp::NONE);
        Self {
            accessed: instant(accessed).map_or(self.accessed, |at| stamp(at).date),
            written: instant(written).map_or(self.written, |at| Stamp {
                centis: 0,
                ..stamp(at)
            }),
            ..self
        }
    }

    /// The day it was last read, at midnight.
    #[must_use]
    pub fn accessed(&self) -> Time64 {
        Stamp {
            date: self.accessed,
            ..Stamp::NONE
        }
        .instant()
    }
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
