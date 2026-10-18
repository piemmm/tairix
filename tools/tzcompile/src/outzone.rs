//! `zic` 2026e's `outzone`, `stringzone` and `writezone`, without the options
//! TAIRiX never passes: a zone's eras and rules resolved into the transitions
//! IANA's own compiler writes, the type in force before the first, and what
//! continues the zone after the last.

use core::cmp::Ordering;

use tairix_abi::time::{SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE};
use tairix_tz::rule::{leap_month_length, DayRule, Moment, TimeBase};

use crate::source::{Era, Rule, Until};

/// What a zone's clocks show from some instant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Type {
    /// Seconds east of UT.
    pub offset: i64,
    /// What the zone calls it.
    pub abbreviation: String,
    /// Whether daylight saving is in force.
    pub daylight: bool,
}

/// One of the two rules a zone alternates between for ever.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phase {
    /// Seconds added to standard time.
    pub save: i64,
    /// What the zone calls it.
    pub abbreviation: String,
    /// When in each year it begins.
    pub moment: Moment,
}

/// What follows a zone's last transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Future {
    /// No rule covers it: `zic` writes four centuries more and no TZ string.
    Unknown,
    /// Standard time all year.
    Standard(Type),
    /// Daylight saving all year.
    Daylight(Type),
    /// Two rules alternating.
    Alternating {
        /// The standard offset, seconds east of UT.
        stdoff: i64,
        /// The change into daylight saving.
        daylight: Phase,
        /// The change back to standard time.
        standard: Phase,
    },
}

/// A zone as `zic` writes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    /// The type before the first transition.
    pub initial: Type,
    /// Each transition, ascending.
    pub transitions: Vec<(i64, Type)>,
    /// What follows the last.
    pub future: Future,
}

/// What an era's saving follows.
#[derive(Clone, Copy, Debug)]
pub enum Saving<'a> {
    /// The rules of its named set.
    Rules(&'a [Rule]),
    /// One fixed saving.
    Fixed {
        /// Seconds added to standard time.
        save: i64,
        /// Whether it counts as daylight saving.
        daylight: bool,
    },
}

/// An era with its saving resolved.
#[derive(Clone, Copy, Debug)]
pub struct Plan<'a> {
    /// The era.
    pub era: &'a Era,
    /// What its saving follows.
    pub saving: Saving<'a>,
}

/// Why a zone could not be resolved, in `zic`'s own words where it has them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneError {
    /// The zone has no era.
    Empty,
    /// An era before the last has no end.
    Unended,
    /// Two rules take effect at one instant.
    SameInstant,
    /// Two transitions fall at one instant.
    SameTransition,
    /// No abbreviation can be determined just after an era's end.
    NoAbbreviation,
    /// A `%z` offset is 100 hours or more.
    LongOffset,
    /// A rule names a day its year lacks, or a time no `i64` reaches.
    Overflow,
    /// The zone's last transition is not the type that continues it.
    Disagrees,
    /// A rule continuing the zone names a day its TZ string cannot state: a
    /// weekday on or after the 29th, which `zic` writes as the month's last,
    /// or on or before the 6th, which it writes as a week that does not exist.
    Inexact,
}

/// The epoch's year.
const EPOCH_YEAR: i64 = 1970;

/// The years `zic` adds either side when no rule covers the future: one
/// Gregorian cycle and two.
const YEARS_OF_OBSERVATIONS: i64 = 402;

/// The hours a TZ string's offset or time must stay under.
const TZ_STRING_HOURS: u64 = 7 * 24;

/// A transition before `writezone` optimises it.
#[derive(Clone, Copy, Debug)]
struct Attype {
    at: i64,
    kind: usize,
    keep: bool,
}

/// What `outzone` accumulates.
#[derive(Default)]
struct Output {
    types: Vec<Type>,
    transitions: Vec<Attype>,
    default_type: Option<usize>,
    /// The latest transition the TZ string does not govern.
    beyond_tz: Option<(i64, usize)>,
}

impl Output {
    /// `addtype`: the index of `kind`, added if new.
    fn kind(&mut self, kind: Type) -> usize {
        if let Some(found) = self.types.iter().position(|known| *known == kind) {
            return found;
        }
        self.types.push(kind);
        self.types.len() - 1
    }

    fn add(&mut self, at: i64, kind: usize) {
        self.transitions.push(Attype {
            at,
            kind,
            keep: false,
        });
    }

    fn not_governed_by_tz(&mut self, at: i64, kind: usize) {
        if self.beyond_tz.is_none_or(|(latest, _)| latest < at) {
            self.beyond_tz = Some((at, kind));
        }
    }
}

/// Which letters an abbreviation's `%s` takes.
#[derive(Clone, Copy)]
enum Letters<'a> {
    /// None named: `%s` stays as written.
    Unnamed,
    /// These.
    Named(&'a str),
    /// None may: the abbreviation is empty unless the format is `%z` or has
    /// a slash.
    Disabled,
}

/// `zic`'s `doabbr`.
fn abbreviation(
    era: &Era,
    letters: Letters<'_>,
    daylight: bool,
    save: i64,
) -> Result<String, ZoneError> {
    let format = era.format.as_str();
    if let Some((standard, saved)) = format.split_once('/') {
        return Ok(String::from(if daylight { saved } else { standard }));
    }
    if format.contains("%z") {
        return Ok(format.replacen("%z", &offset_abbreviation(era.stdoff + save)?, 1));
    }
    Ok(match letters {
        Letters::Unnamed => String::from(format),
        Letters::Named(letters) => format.replacen("%s", letters, 1),
        Letters::Disabled => String::new(),
    })
}

/// `zic`'s `abbroffset`: `+hh`, `+hhmm` or `+hhmmss`.
fn offset_abbreviation(offset: i64) -> Result<String, ZoneError> {
    if offset.unsigned_abs() >= 100 * SECS_PER_HOUR.unsigned_abs() {
        return Err(ZoneError::LongOffset);
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let magnitude = offset.unsigned_abs();
    let (hour, minute) = (SECS_PER_HOUR.unsigned_abs(), SECS_PER_MINUTE.unsigned_abs());
    let (hours, minutes, seconds) = (
        magnitude / hour,
        magnitude / minute % 60,
        magnitude % minute,
    );
    Ok(match (minutes, seconds) {
        (0, 0) => format!("{sign}{hours:02}"),
        (_, 0) => format!("{sign}{hours:02}{minutes:02}"),
        _ => format!("{sign}{hours:02}{minutes:02}{seconds:02}"),
    })
}

/// `rule_cmp`: which of two rules is likelier to decide future times.
fn rule_cmp(a: Option<&Rule>, b: Option<&Rule>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) if a.to != b.to => a.to.cmp(&b.to),
        (Some(a), Some(_)) if a.to == i64::MAX => Ordering::Equal,
        (Some(a), Some(b)) => a
            .moment
            .month
            .cmp(&b.moment.month)
            .then(day_number(a.moment.day).cmp(&day_number(b.moment.day))),
    }
}

/// The day of the month a rule states.
const fn day_number(day: DayRule) -> u8 {
    match day {
        DayRule::Of(day) | DayRule::OnOrAfter { day, .. } | DayRule::OnOrBefore { day, .. } => day,
    }
}

/// Whether a TZ string can state an offset or time of `seconds`.
const fn fits_tz_string(seconds: i64) -> bool {
    seconds.unsigned_abs() / SECS_PER_HOUR.unsigned_abs() < TZ_STRING_HOURS
}

/// Whether `stringrule` can state `rule` in a TZ string whose daylight
/// saving is `save` and standard offset `stdoff`.
fn rule_fits_tz_string(rule: &Rule, save: i64, stdoff: i64) -> bool {
    let moment = rule.moment;
    let mut at = i64::from(moment.at);
    match moment.day {
        DayRule::Of(29) if moment.month == 2 => return false,
        DayRule::Of(_) => {}
        DayRule::OnOrAfter { day, .. } => {
            let Some(after_first) = day.checked_sub(1) else {
                return false;
            };
            at += i64::from(after_first % 7) * SECS_PER_DAY;
        }
        DayRule::OnOrBefore { day, .. } => {
            if day != leap_month_length(moment.month) {
                at += i64::from(day % 7) * SECS_PER_DAY;
            }
        }
    }
    if moment.base == TimeBase::Universal {
        at += stdoff;
    }
    if moment.base != TimeBase::Wall && !rule.daylight {
        at += save;
    }
    at == 2 * SECS_PER_HOUR || fits_tz_string(at)
}

/// `stringzone`: what continues the zone after its last era's transitions,
/// as `zic` states it in the TZ string it writes.
fn future(last: &Plan<'_>) -> Result<Future, ZoneError> {
    let era = last.era;
    let stdoff = era.stdoff;
    let (standard, daylight, order) = match last.saving {
        Saving::Rules(rules) => {
            let mut latest: [Option<&Rule>; 2] = [None, None];
            for rule in rules {
                let slot = &mut latest[usize::from(rule.daylight)];
                match rule_cmp(*slot, Some(rule)) {
                    Ordering::Less => *slot = Some(rule),
                    Ordering::Equal => return Ok(Future::Unknown),
                    Ordering::Greater => {}
                }
            }
            let [standard, daylight] = latest;
            (standard, daylight, rule_cmp(daylight, standard))
        }
        Saving::Fixed { daylight, .. } => (
            None,
            None,
            if daylight {
                Ordering::Greater
            } else {
                Ordering::Less
            },
        ),
    };
    match order {
        Ordering::Less => {
            if !fits_tz_string(stdoff) {
                return Ok(Future::Unknown);
            }
            Ok(Future::Standard(Type {
                offset: stdoff,
                abbreviation: abbreviation(era, letters(standard), false, 0)?,
                daylight: false,
            }))
        }
        Ordering::Greater => {
            let save = match (daylight, last.saving) {
                (Some(rule), _) => rule.save,
                (None, Saving::Fixed { save, .. }) => save,
                (None, Saving::Rules(_)) => 0,
            };
            let faked = if save >= 0 { stdoff + 2 * save } else { stdoff };
            let end = SECS_PER_DAY - save.abs();
            if !fits_tz_string(faked) || !fits_tz_string(stdoff + save) || !fits_tz_string(end) {
                return Ok(Future::Unknown);
            }
            Ok(Future::Daylight(Type {
                offset: stdoff + save,
                abbreviation: abbreviation(era, letters(daylight), true, save)?,
                daylight: true,
            }))
        }
        Ordering::Equal => {
            let (Some(standard), Some(daylight)) = (standard, daylight) else {
                return Ok(Future::Unknown);
            };
            let stated = fits_tz_string(stdoff)
                && (daylight.save == SECS_PER_HOUR || fits_tz_string(stdoff + daylight.save))
                && rule_fits_tz_string(daylight, daylight.save, stdoff)
                && rule_fits_tz_string(standard, daylight.save, stdoff);
            if !stated {
                return Ok(Future::Unknown);
            }
            if !(states_exactly(daylight.moment) && states_exactly(standard.moment)) {
                return Err(ZoneError::Inexact);
            }
            // The TZ string states standard time as the bare standard offset,
            // whatever saving its rule names.
            let phase = |rule: &Rule, save: i64| -> Result<Phase, ZoneError> {
                Ok(Phase {
                    save,
                    abbreviation: abbreviation(
                        era,
                        Letters::Named(&rule.letters),
                        rule.daylight,
                        save,
                    )?,
                    moment: rule.moment,
                })
            };
            Ok(Future::Alternating {
                stdoff,
                daylight: phase(daylight, daylight.save)?,
                standard: phase(standard, 0)?,
            })
        }
    }
}

/// Whether `stringrule` writes `moment`'s day as the day it is: a week of
/// the month counted from its first day, or its last week for a weekday on or
/// before the month's last day.
const fn states_exactly(moment: Moment) -> bool {
    match moment.day {
        DayRule::Of(_) => true,
        DayRule::OnOrAfter { day, .. } => day <= 28,
        DayRule::OnOrBefore { day, .. } => day >= 7,
    }
}

/// The letters `rule` names, if there is one.
fn letters(rule: Option<&Rule>) -> Letters<'_> {
    rule.map_or(Letters::Unnamed, |rule| Letters::Named(&rule.letters))
}

/// `rpytime`: the local seconds `moment` names in `year`.
fn local(moment: &Moment, year: i64) -> Result<i64, ZoneError> {
    moment.local_seconds(year).ok_or(ZoneError::Overflow)
}

/// The UT instant an era's end names, with `stdoff` and `save` in force.
fn until_instant(until: &Until, stdoff: i64, save: i64) -> Result<i64, ZoneError> {
    let mut at = until.local;
    if until.moment.base != TimeBase::Universal {
        at = at.checked_sub(stdoff).ok_or(ZoneError::Overflow)?;
    }
    if until.moment.base == TimeBase::Wall {
        at = at.checked_sub(save).ok_or(ZoneError::Overflow)?;
    }
    Ok(at)
}

/// The zone `plans` describe, era by era, as `zic` writes it.
///
/// # Errors
///
/// A [`ZoneError`] where `zic` would refuse the zone, or where its last
/// transition disagrees with what continues it.
pub fn resolve(plans: &[Plan<'_>]) -> Result<Resolved, ZoneError> {
    let last = plans.last().ok_or(ZoneError::Empty)?;
    let (mut min_year, mut max_year) = (EPOCH_YEAR, EPOCH_YEAR);
    let mut widen = |year: i64| {
        min_year = min_year.min(year);
        max_year = max_year.max(year);
    };
    for (index, plan) in plans.iter().enumerate() {
        if index + 1 < plans.len() {
            widen(plan.era.until.as_ref().ok_or(ZoneError::Unended)?.year);
        }
        if let Saving::Rules(rules) = plan.saving {
            for rule in rules {
                widen(rule.from);
                if rule.to_is_year {
                    widen(rule.to);
                }
            }
        }
    }
    let future = future(last)?;
    let extend = future == Future::Unknown;
    if extend {
        min_year = min_year.saturating_sub(YEARS_OF_OBSERVATIONS);
        max_year = max_year.saturating_add(YEARS_OF_OBSERVATIONS);
    }
    let years = (min_year, max_year);
    let mut out = Output::default();
    let mut start_time = 0;
    for (index, plan) in plans.iter().enumerate() {
        let until = if index + 1 < plans.len() {
            plan.era.until.as_ref()
        } else {
            None
        };
        start_time = era(plan, index > 0, until, start_time, years, &mut out)?;
    }
    let default_type = out.default_type.unwrap_or(0);
    let initial = out
        .types
        .get(default_type)
        .cloned()
        .ok_or(ZoneError::Empty)?;
    if extend {
        claim_observed_years(&mut out, max_year, default_type)?;
    } else {
        keep_until_tz_string(&mut out, &future);
    }
    let transitions = optimise(&mut out)?;
    let resolved = Resolved {
        initial,
        transitions: transitions
            .into_iter()
            .map(|transition| (transition.at, out.types[transition.kind].clone()))
            .collect(),
        future,
    };
    let last_type = resolved
        .transitions
        .last()
        .map_or(&resolved.initial, |(_, kind)| kind);
    match &resolved.future {
        Future::Standard(kind) | Future::Daylight(kind) if kind != last_type => {
            Err(ZoneError::Disagrees)
        }
        _ => Ok(resolved),
    }
}

/// Where an era starts, while its first transition is still being found.
struct Start {
    /// Whether the start is yet to be written.
    pending: bool,
    /// The UT instant it starts at.
    time: i64,
    /// The offset in force from then.
    offset: i64,
    /// The abbreviation in force from then, once known.
    abbreviation: String,
}

/// One era of `outzone`'s loop; the instant the next era starts.
fn era(
    plan: &Plan<'_>,
    pending: bool,
    until: Option<&Until>,
    start_time: i64,
    years: (i64, i64),
    out: &mut Output,
) -> Result<i64, ZoneError> {
    let era = plan.era;
    let mut start = Start {
        pending,
        time: start_time,
        offset: era.stdoff,
        abbreviation: String::new(),
    };
    let save = match plan.saving {
        Saving::Fixed { save, daylight } => {
            let kind = out.kind(Type {
                offset: era.stdoff + save,
                abbreviation: abbreviation(era, Letters::Unnamed, daylight, save)?,
                daylight,
            });
            if start.pending {
                out.add(start.time, kind);
                out.not_governed_by_tz(start.time, kind);
                start.pending = false;
            } else {
                out.default_type = Some(kind);
            }
            save
        }
        Saving::Rules(rules) => ruled(era, rules, until, years, &mut start, out)?,
    };
    if start.pending {
        let daylight = start.offset != era.stdoff;
        if start.abbreviation.is_empty() {
            start.abbreviation = abbreviation(era, Letters::Disabled, daylight, save)?;
        }
        if start.abbreviation.is_empty() {
            return Err(ZoneError::NoAbbreviation);
        }
        let kind = out.kind(Type {
            offset: start.offset,
            abbreviation: start.abbreviation,
            daylight,
        });
        if out.default_type.is_none() && !daylight {
            out.default_type = Some(kind);
        }
        out.add(start.time, kind);
    }
    match until {
        Some(until) => until_instant(until, era.stdoff, save),
        None => Ok(start_time),
    }
}

/// An era following rules: every year's changes in turn, until the era
/// ends; the saving in force at its end.
fn ruled(
    era: &Era,
    rules: &[Rule],
    until: Option<&Until>,
    (min_year, max_year): (i64, i64),
    start: &mut Start,
    out: &mut Output,
) -> Result<i64, ZoneError> {
    let stdoff = era.stdoff;
    let mut save = 0;
    let mut pending = vec![false; rules.len()];
    let mut local_at = vec![0; rules.len()];
    let mut year = min_year;
    let mut try_next_year = true;
    while try_next_year {
        if until.is_some_and(|until| year > until.year) {
            break;
        }
        try_next_year = false;
        let mut next_year = max_year;
        for (index, rule) in rules.iter().enumerate() {
            if year < rule.to {
                let next = (year + 1).max(rule.from);
                if next <= next_year {
                    next_year = next;
                    try_next_year = true;
                }
            }
            pending[index] = rule.from <= year && year <= rule.to;
            if pending[index] {
                local_at[index] = local(&rule.moment, year)?;
            }
        }
        loop {
            let until_time = until
                .map(|until| until_instant(until, stdoff, save))
                .transpose()?;
            let Some((index, at)) = earliest(rules, &pending, &local_at, stdoff, save)? else {
                break;
            };
            let rule = &rules[index];
            pending[index] = false;
            let named = Letters::Named(&rule.letters);
            if until_time.is_some_and(|end| at >= end) {
                if start.abbreviation.is_empty() && stdoff + rule.save == start.offset {
                    start.abbreviation = abbreviation(era, named, rule.daylight, rule.save)?;
                }
                break;
            }
            save = rule.save;
            if start.pending && at == start.time {
                start.pending = false;
            }
            if start.pending {
                if at < start.time {
                    start.offset = stdoff + save;
                    start.abbreviation = abbreviation(era, named, rule.daylight, save)?;
                    continue;
                }
                if start.abbreviation.is_empty() && start.offset == stdoff + save {
                    start.abbreviation = abbreviation(era, named, rule.daylight, save)?;
                }
            }
            let kind = out.kind(Type {
                offset: stdoff + save,
                abbreviation: abbreviation(era, named, rule.daylight, save)?,
                daylight: rule.daylight,
            });
            if out.default_type.is_none() && !rule.daylight {
                out.default_type = Some(kind);
            }
            out.add(at, kind);
            if until.is_some() || rule.to != i64::MAX {
                out.not_governed_by_tz(at, kind);
            }
        }
        year = next_year;
    }
    Ok(save)
}

/// The rule still to do this year that takes effect first, and its UT
/// instant with `stdoff` and `save` in force.
fn earliest(
    rules: &[Rule],
    pending: &[bool],
    local_at: &[i64],
    stdoff: i64,
    save: i64,
) -> Result<Option<(usize, i64)>, ZoneError> {
    let mut earliest: Option<(usize, i64)> = None;
    for (index, rule) in rules.iter().enumerate() {
        if !pending[index] {
            continue;
        }
        let shift = match rule.moment.base {
            TimeBase::Universal => 0,
            TimeBase::Standard => stdoff,
            TimeBase::Wall => stdoff + save,
        };
        let at = local_at[index]
            .checked_sub(shift)
            .ok_or(ZoneError::Overflow)?;
        match earliest {
            Some((_, soonest)) if at == soonest => return Err(ZoneError::SameInstant),
            Some((_, soonest)) if at > soonest => {}
            _ => earliest = Some((index, at)),
        }
    }
    Ok(earliest)
}

/// `outzone`'s slim cut: drop the transitions the TZ string implies, keeping
/// the first it governs so a reader knows where it takes over.
fn keep_until_tz_string(out: &mut Output, future: &Future) {
    let (limit, limit_kind) = out
        .beyond_tz
        .map_or((i64::MIN, None), |(at, kind)| (at, Some(kind)));
    let tz_start = out
        .transitions
        .iter()
        .map(|transition| transition.at)
        .filter(|at| *at > limit)
        .min()
        .unwrap_or(limit);
    let has_rules = !matches!(future, Future::Standard(_));
    out.transitions
        .retain(|transition| transition.at <= tz_start);
    for transition in &mut out.transitions {
        transition.keep =
            transition.at == tz_start && (limit_kind != Some(transition.kind) || has_rules);
    }
}

/// With no rule for the future, claim the four extended centuries by a
/// transition at their end unless one already lies near it.
fn claim_observed_years(
    out: &mut Output,
    max_year: i64,
    default_type: usize,
) -> Result<(), ZoneError> {
    let new_year = Moment {
        month: 1,
        day: DayRule::Of(1),
        at: 0,
        base: TimeBase::Wall,
    };
    let latest = out
        .transitions
        .iter()
        .copied()
        .reduce(|latest, transition| {
            if transition.at > latest.at {
                transition
            } else {
                latest
            }
        });
    let horizon = local(&new_year, max_year - 1)?;
    if latest.is_none_or(|latest| latest.at < horizon) {
        out.transitions.push(Attype {
            at: local(&new_year, max_year + 1)?,
            kind: latest.map_or(default_type, |latest| latest.kind),
            keep: true,
        });
    }
    Ok(())
}

/// `writezone`'s sort and optimisation: a transition whose local time is not
/// after its predecessor's replaces it, and one that changes nothing is
/// dropped unless it is to be kept.
fn optimise(out: &mut Output) -> Result<Vec<Attype>, ZoneError> {
    out.transitions.sort_by_key(|transition| transition.at);
    if out
        .transitions
        .windows(2)
        .any(|pair| pair[0].at == pair[1].at)
    {
        return Err(ZoneError::SameTransition);
    }
    let offset = |kind: usize| out.types[kind].offset;
    let mut kept: Vec<Attype> = Vec::with_capacity(out.transitions.len());
    for &transition in &out.transitions {
        if let Some(previous) = kept.last().copied() {
            let before = if kept.len() == 1 {
                0
            } else {
                kept[kept.len() - 2].kind
            };
            let local = transition.at.saturating_add(offset(previous.kind));
            if local <= previous.at.saturating_add(offset(before)) {
                if transition.kind == before {
                    kept.pop();
                } else if let Some(previous) = kept.last_mut() {
                    previous.kind = transition.kind;
                }
                continue;
            }
        }
        if kept.last().is_none_or(|previous| {
            transition.keep || out.types[previous.kind] != out.types[transition.kind]
        }) {
            kept.push(transition);
        }
    }
    Ok(kept)
}

#[cfg(test)]
#[path = "outzone_tests.rs"]
mod tests;
