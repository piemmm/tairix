//! The UK territory's daylight saving rules (`plans/RISCOS-EMULATOR.md` §4.6):
//! the session's zone's, in the years genuine RISC OS 5's UK territory keeps
//! rules for. Daylight saving is in force wherever the zone's offset differs
//! from its standard offset, which is how RISC OS 5 reports British Standard
//! Time's years. A year's bounds are where it begins and ends within that
//! year, as its standard time reads it; a year it does not begin or end in
//! starts or ends with the year.

use core::ops::RangeInclusive;

use tairix_abi::time::Time64;
use tairix_tz::Zone;

use crate::error::Error;
use crate::os::{Fail, Os, Regs};
use crate::territory::calendar::year_start;
use crate::territory::UNKNOWN_SWI;

use super::{five_byte, instant, own};

/// The years genuine RISC OS 5's UK territory has rules for.
const RULED_YEARS: RangeInclusive<i64> = 1968..=2099;

/// "No rule for that time zone and year".
const NO_RULE: Error = Error {
    number: 0xAC2,
    token: "NoDaylightRule",
};

/// The most offset changes a zone makes in a year, a bound on the walk.
const CHANGES_PER_YEAR: usize = 16;

/// `Territory_DaylightRules`: 0 says the territory has the call, 1 gives
/// R3's year's bounds in time zone R2 to R4's and R5's five-byte blocks.
pub(super) fn daylight_rules(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[1] {
        0 => regs.r[0] = 1,
        1 => {
            let (standard, _) = os.uk_readings();
            let zone = os.world.zone;
            let year = i64::from(regs.r[3].cast_signed());
            let bounds = if regs.r[2] == 0 {
                bounds(zone, standard.offset, year)
            } else {
                None
            };
            let (start, end) = bounds.ok_or_else(|| os.own_error(own, NO_RULE, &[]))?;
            os.write_five(regs.r[4], start)?;
            os.write_five(regs.r[5], end)?;
            // The end's low word, which genuine RISC OS 5 leaves in R0.
            regs.r[0] = (end & 0xFFFF_FFFF) as u32;
        }
        _ => return Err(os.own_error(own, UNKNOWN_SWI, &[])),
    }
    Ok(())
}

/// The five-byte times daylight saving begins and ends within `year`; none
/// if it is never in force then, or the year has no rules.
fn bounds(zone: Zone<'static>, standard: i32, year: i64) -> Option<(u64, u64)> {
    if !RULED_YEARS.contains(&year) {
        return None;
    }
    let first = instant(year_start(year, standard)?);
    let last = instant(year_start(year.checked_add(1)?, standard)?);
    let dst = |offset: i32| offset != standard / 100;
    let (mut begins, mut ends) = (None, None);
    let mut at = first;
    for _ in 0..CHANGES_PER_YEAR {
        let Some(change) = zone
            .next_change(at)
            .filter(|change| change.at.secs() < last.secs())
        else {
            break;
        };
        match (dst(change.before.utc_offset), dst(change.after.utc_offset)) {
            (false, true) => {
                begins.get_or_insert(change.at);
            }
            (true, false) => {
                ends.get_or_insert(change.at);
            }
            _ => {}
        }
        at = change.at;
    }
    let begins = begins.or_else(|| dst(zone.offset_at(first).utc_offset).then_some(first))?;
    let ends = ends.or_else(|| {
        let end = Time64::from_secs(last.secs() - 1);
        dst(zone.offset_at(end).utc_offset).then_some(last)
    })?;
    Some((five_byte(begins)?, five_byte(ends)?))
}
