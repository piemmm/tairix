//! `Territory_ConvertTimeFormats` (`plans/RISCOS-EMULATOR.md` §4.6): a time
//! converted between ordinals and five-byte forms, and between local time and
//! UTC by a territory's time zone and its daylight saving rules.
//!
//! A local time is read as standard time to ask the rules whether daylight
//! saving is in force, so a time the spring change skips reads as summer time
//! and one the autumn change repeats as standard time, as genuine RISC OS 5
//! reads them.

use crate::clock::FIVE_BYTES;
use crate::error::Error;
use crate::os::{Fail, Os, Regs};

use super::calendar::time_of;
use super::{own, BAD_TIME, NO_TERRITORY, UNKNOWN_SWI};

/// The forms: local ordinals, a local five-byte time, UTC ordinals, a UTC
/// five-byte time.
const LOCAL_ORDINALS: u32 = 0;
const UTC_ORDINALS: u32 = 2;
const UTC_FIVE: u32 = 3;

/// The flags: daylight saving taken as in force; standard time where the
/// territory keeps no rules; daylight saving as its rules say; R4 an
/// adjustment in centiseconds rather than a time zone.
pub(super) const AS_SUMMER: u32 = 1 << 16;
pub(super) const STANDARD_UNRULED: u32 = 1 << 17;
pub(super) const BY_THE_RULES: u32 = 1 << 18;
pub(super) const ADJUSTMENT: u32 = 1 << 19;
/// The flags past those.
const RESERVED: u32 = !(ADJUSTMENT | (ADJUSTMENT - 1));

/// "No such time zone".
const NO_SUCH_ZONE: Error = Error {
    number: 0x196,
    token: "UnknownTimeZone",
};

const fn is_local(form: u32) -> bool {
    form < UTC_ORDINALS
}

const fn is_ordinals(form: u32) -> bool {
    form == LOCAL_ORDINALS || form == UTC_ORDINALS
}

/// `time` moved by `by` centiseconds, if the result is a five-byte time.
fn shifted(time: u64, by: i64) -> Option<u64> {
    time.checked_add_signed(by).filter(|&t| t < FIVE_BYTES)
}

impl Os<'_> {
    /// `Territory_ConvertTimeFormats`: R1's time in the form R3's bits 0 to 7
    /// name, written to R2 in the form its bits 8 to 15 name.
    pub(crate) fn convert_time_formats(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let flags = regs.r[3];
        let (from, to) = (flags & 0xFF, flags >> 8 & 0xFF);
        let both = BY_THE_RULES | ADJUSTMENT;
        if from > UTC_FIVE || to > UTC_FIVE || flags & RESERVED != 0 || flags & both == both {
            return Err(self.own_error(own, UNKNOWN_SWI, &[]));
        }
        let time = if is_ordinals(from) {
            let words = self.ordinal_words(regs.r[1])?;
            time_of(words).ok_or_else(|| self.own_error(own, BAD_TIME, &[]))?
        } else {
            self.read_five(regs.r[1])?
        };
        let converted = match (is_local(from), is_local(to)) {
            (true, false) => {
                let offset = self.local_offset(regs, time, true)?;
                shifted(time, -offset)
            }
            (false, true) => {
                let offset = self.local_offset(regs, time, false)?;
                shifted(time, offset)
            }
            _ => Some(time),
        };
        let converted = converted.ok_or_else(|| self.own_error(own, BAD_TIME, &[]))?;
        if is_ordinals(to) {
            self.write_ordinals(regs.r[2], converted)
        } else {
            self.write_five(regs.r[2], converted)
        }
    }

    /// Local time's offset from UTC, in centiseconds, at `time`, which is
    /// local where `local` says and UTC otherwise.
    fn local_offset(&mut self, regs: &Regs, time: u64, local: bool) -> Result<i64, Fail> {
        let (territory, flags, zone) = (regs.r[0], regs.r[3], regs.r[4]);
        if flags & ADJUSTMENT != 0 {
            let adjustment = i64::from(zone.cast_signed());
            if flags & AS_SUMMER == 0 {
                return Ok(adjustment);
            }
            let (standard, summer) = self.offsets(territory)?;
            return Ok(adjustment + i64::from(summer) - i64::from(standard));
        }
        let (standard, summer) = self.offsets(territory)?;
        if zone != 0 {
            return Err(self.own_error(own, NO_SUCH_ZONE, &[]));
        }
        let summer_time = if flags & BY_THE_RULES == 0 {
            flags & AS_SUMMER != 0
        } else if self.has_rules(territory)? {
            let instant = if local {
                shifted(time, -i64::from(standard))
            } else {
                Some(time)
            };
            match instant {
                Some(instant) => self.in_force_by(territory, zone, instant, standard)?,
                None => false,
            }
        } else if flags & STANDARD_UNRULED != 0 {
            false
        } else {
            return Err(self.own_error(own, super::dst::NO_RULE, &[]));
        };
        Ok(i64::from(if summer_time { summer } else { standard }))
    }

    /// `territory`'s standard and summer offsets, refused for a territory
    /// not loaded.
    fn offsets(&mut self, territory: u32) -> Result<(i32, i32), Fail> {
        self.zone_offsets(territory)?
            .ok_or_else(|| self.own_error(own, NO_TERRITORY, &[]))
    }
}
