//! Daylight saving (`plans/RISCOS-EMULATOR.md` §4.6): `Territory_DaylightSaving`,
//! the automatic changeover and `Service_TimeZoneChanged`.
//!
//! The Manager keeps no rules of its own. It asks the territory's
//! `Territory_DaylightRules` entry for a year's bounds, as genuine RISC OS 5's
//! does: a territory that cannot answer has daylight saving in force at no
//! time, and has no bounds for any year.

use crate::clock::FIVE_BYTES;
use crate::error::Error;
use crate::os::{Fail, Os, Regs};

use super::calendar::{standard_year, time_of, year_start};
use super::{entry, own, SCRATCH_AT, UNKNOWN_SWI, UNKNOWN_TERRITORY};

/// "No rule for given timezone".
pub(super) const NO_RULE: Error = Error {
    number: 0x197,
    token: "NoDaylightRule",
};

/// `Service_TimeZoneChanged`.
const TIME_ZONE_CHANGED: u32 = 0x8_1080;

/// The state flags: daylight saving in force, and kept by the rules.
const ON: u32 = 1;
pub(super) const AUTOMATIC: u32 = 2;

/// The CMOS that keeps automatic daylight saving: bit 0 of &10.
const AUTO_CMOS: u8 = crate::cmos::DBTB;
const AUTO_BIT: u8 = 1;

/// `Territory_DaylightRules`' reasons: whether there are rules, a year's
/// bounds.
const HAS_RULES: u32 = 0;
const YEAR_BOUNDS: u32 = 1;

/// The current territory, and the time zone its rules are kept for.
const CURRENT: u32 = u32::MAX;
const FIRST_ZONE: u32 = 0;

/// Whether `time` lies where a year's `bounds` put daylight saving in force:
/// between them, or outside them where they end before they begin.
fn within((start, end): (u64, u64), time: u64) -> bool {
    if start <= end {
        (start..end).contains(&time)
    } else {
        time >= start || time < end
    }
}

impl Os<'_> {
    /// `Territory_DaylightSaving`.
    pub(crate) fn daylight_saving(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let territory = regs.r[0];
        match regs.r[1] {
            0 => {
                self.current_territory_only(territory)?;
                regs.r[0] = self.dst_state()?;
            }
            1 => {
                self.current_territory_only(territory)?;
                self.set_dst_state(regs.r[2])?;
            }
            2 => regs.r[0] = u32::from(self.has_rules(territory)?),
            3 => {
                let words = self.ordinal_words(regs.r[3])?;
                // Ordinals naming no time, as RISC OS 5 reads them, are a time
                // daylight saving is not in force at.
                let on = match time_of(words) {
                    Some(time) => self.in_force(territory, regs.r[2], time)?,
                    None => false,
                };
                regs.r[0] = u32::from(on);
            }
            4 => {
                let time = self.read_five(regs.r[3])?;
                regs.r[0] = u32::from(self.in_force(territory, regs.r[2], time)?);
            }
            5 => {
                let year = match regs.r[3].cast_signed() {
                    -1 => {
                        let standard = self.standard_time(territory)?;
                        standard_year(self.real_time() % FIVE_BYTES, standard)
                    }
                    year => Some(i64::from(year)),
                };
                let ruled = match year {
                    Some(year) => {
                        self.year_bounds(territory, regs.r[2], year, regs.r[4], regs.r[5])?
                    }
                    None => false,
                };
                if !ruled {
                    return Err(self.own_error(own, NO_RULE, &[]));
                }
            }
            _ => return Err(self.own_error(own, UNKNOWN_SWI, &[])),
        }
        Ok(())
    }

    /// Refuses all but the current territory, as the reasons that change or
    /// read the settings do.
    fn current_territory_only(&mut self, territory: u32) -> Result<(), Fail> {
        if territory == CURRENT || territory == self.world.territory.current {
            Ok(())
        } else {
            Err(self.own_error(own, UNKNOWN_TERRITORY, &[]))
        }
    }

    /// Calls `territory`'s entry `index`: whether it answered. An error it
    /// gives is an answer, not a failure.
    pub(super) fn ask_territory(
        &mut self,
        territory: u32,
        index: usize,
        regs: &mut Regs,
    ) -> Result<bool, Fail> {
        match self.call_territory(territory, index, regs) {
            Ok(()) => Ok(true),
            Err(Fail::Error(_)) => Ok(false),
            Err(unwind) => Err(unwind),
        }
    }

    /// Whether `territory` answers for daylight saving rules: its
    /// `Territory_DaylightRules` says it has the call, though it may have
    /// rules for no year.
    pub(super) fn has_rules(&mut self, territory: u32) -> Result<bool, Fail> {
        let mut regs = Regs::default();
        regs.r[1] = HAS_RULES;
        Ok(self.ask_territory(territory, entry::DAYLIGHT_RULES, &mut regs)? && regs.r[0] != 0)
    }

    /// `territory`'s standard and summer offsets from UTC, in centiseconds,
    /// as its `Territory_ReadTimeZones` gives them; none if it gives none.
    pub(super) fn zone_offsets(&mut self, territory: u32) -> Result<Option<(i32, i32)>, Fail> {
        let mut regs = Regs::default();
        let answered = self.ask_territory(territory, entry::READ_TIME_ZONES, &mut regs)?;
        Ok(answered.then(|| (regs.r[2].cast_signed(), regs.r[3].cast_signed())))
    }

    /// `territory`'s standard time, in centiseconds east of UTC: UTC if it
    /// gives none.
    fn standard_time(&mut self, territory: u32) -> Result<i32, Fail> {
        Ok(self
            .zone_offsets(territory)?
            .map_or(0, |(standard, _)| standard))
    }

    /// `territory`'s bounds for `year` in its time zone `zone`, written to
    /// the five-byte blocks at `start` and `end`: whether it has them.
    fn year_bounds(
        &mut self,
        territory: u32,
        zone: u32,
        year: i64,
        start: u32,
        end: u32,
    ) -> Result<bool, Fail> {
        let Ok(year) = i32::try_from(year) else {
            return Ok(false);
        };
        let mut regs = Regs::default();
        regs.r[1..6].copy_from_slice(&[YEAR_BOUNDS, zone, year.cast_unsigned(), start, end]);
        self.ask_territory(territory, entry::DAYLIGHT_RULES, &mut regs)
    }

    /// `territory`'s bounds for `year` in time zone `zone`, if it has them,
    /// asked into the Manager's scratch block.
    fn bounds(&mut self, territory: u32, zone: u32, year: i64) -> Result<Option<(u64, u64)>, Fail> {
        let Some(block) = self.world.territory.workspace.map(|w| w + SCRATCH_AT) else {
            return Ok(None);
        };
        if !self.year_bounds(territory, zone, year, block, block + 8)? {
            return Ok(None);
        }
        Ok(Some((self.read_five(block)?, self.read_five(block + 8)?)))
    }

    /// Whether `territory`'s rules for time zone `zone` put daylight saving
    /// in force at the five-byte time `time`, by its year's bounds.
    pub(super) fn in_force(&mut self, territory: u32, zone: u32, time: u64) -> Result<bool, Fail> {
        let standard = self.standard_time(territory)?;
        self.in_force_by(territory, zone, time, standard)
    }

    /// As [`Self::in_force`], the territory's standard time known: `standard`
    /// centiseconds east of UTC, which decides the year `time` is in.
    pub(super) fn in_force_by(
        &mut self,
        territory: u32,
        zone: u32,
        time: u64,
        standard: i32,
    ) -> Result<bool, Fail> {
        let Some(year) = standard_year(time, standard) else {
            return Ok(false);
        };
        Ok(self
            .bounds(territory, zone, year)?
            .is_some_and(|bounds| within(bounds, time)))
    }

    /// What the current territory's rules say at the five-byte time `now`:
    /// whether daylight saving is in force, and the next time they may change
    /// that — one of this year's bounds, or the next year's start, when its
    /// rules take over; none where the territory does not answer for rules.
    fn rules_now(&mut self, now: u64) -> Result<(bool, Option<u64>), Fail> {
        let standard = self.standard_time(CURRENT)?;
        let Some(year) = standard_year(now, standard) else {
            return Ok((false, None));
        };
        let bounds = self.bounds(CURRENT, FIRST_ZONE, year)?;
        let on = bounds.is_some_and(|bounds| within(bounds, now));
        if !self.has_rules(CURRENT)? {
            return Ok((on, None));
        }
        let next_year = year
            .checked_add(1)
            .and_then(|next| year_start(next, standard));
        let due = bounds
            .into_iter()
            .flat_map(|(start, end)| [start, end])
            .filter(|&bound| bound > now)
            .chain(next_year)
            .min();
        Ok((on, due))
    }

    /// The state flags, as CMOS keeps them.
    pub(super) fn dst_state(&mut self) -> Result<u32, Fail> {
        let on = self.read_cmos(super::DST_CMOS)? & super::DST_BIT != 0;
        let automatic = self.read_cmos(AUTO_CMOS)? & AUTO_BIT != 0;
        Ok(u32::from(on) | if automatic { AUTOMATIC } else { 0 })
    }

    /// Sets the state: kept by the rules, which put daylight saving in or
    /// out of force now, or in or out of force as `flags` says.
    pub(super) fn set_dst_state(&mut self, flags: u32) -> Result<(), Fail> {
        let automatic = flags & AUTOMATIC != 0;
        let now = self.real_time() % FIVE_BYTES;
        let (on, due) = if automatic {
            self.rules_now(now)?
        } else {
            (flags & ON != 0, None)
        };
        let before = self.dst_state()?;
        let auto = self.read_cmos(AUTO_CMOS)?;
        let auto_new = if automatic {
            auto | AUTO_BIT
        } else {
            auto & !AUTO_BIT
        };
        if auto_new != auto {
            self.write_cmos(AUTO_CMOS, auto_new)?;
        }
        let old = self.read_cmos(super::DST_CMOS)?;
        let new = if on {
            old | super::DST_BIT
        } else {
            old & !super::DST_BIT
        };
        if new != old {
            self.write_cmos(super::DST_CMOS, new)?;
        }
        self.await_changeover(now, due);
        if self.dst_state()? != before {
            self.time_zone_changed()?;
        }
        Ok(())
    }

    /// Daylight saving put in or out of force as the rules say now, and the
    /// next changeover awaited, while CMOS says the rules keep it.
    pub(crate) fn dst_changeover(&mut self) -> Result<(), Fail> {
        if self.read_cmos(AUTO_CMOS)? & AUTO_BIT == 0 {
            return Ok(());
        }
        self.set_dst_state(AUTOMATIC)
    }

    /// Arms the clock for the rules' next changeover while CMOS says they
    /// keep daylight saving; disarms it otherwise.
    pub(super) fn arm_changeover(&mut self) -> Result<(), Fail> {
        let now = self.real_time() % FIVE_BYTES;
        let due = if self.world.cmos.read(AUTO_CMOS) & AUTO_BIT != 0 {
            self.rules_now(now)?.1
        } else {
            None
        };
        self.await_changeover(now, due);
        Ok(())
    }

    /// The clock armed for the five-byte time `due`, `now` being the time now,
    /// or disarmed for none.
    fn await_changeover(&mut self, now: u64, due: Option<u64>) {
        self.world.clock.zone_due = due.map(|at| self.now().saturating_add(at.saturating_sub(now)));
    }

    /// `Service_TimeZoneChanged`, with the state and the offset in force.
    pub(super) fn time_zone_changed(&mut self) -> Result<(), Fail> {
        let state = self.dst_state()?;
        let mut zone = Regs::default();
        self.read_current_time_zone(&mut zone)?;
        let mut service = Regs::default();
        service.r[1..4].copy_from_slice(&[TIME_ZONE_CHANGED, state, zone.r[1]]);
        self.service(&mut service)?;
        Ok(())
    }
}
