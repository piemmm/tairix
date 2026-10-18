//! The IANA source text `zic` compiles — `Rule`, `Zone` and `Link` lines —
//! split, matched and converted as `zic` 2026e does, so a source means here
//! exactly what it means to IANA's own compiler.

use std::collections::BTreeSet;

use tairix_abi::time::{SECS_PER_HOUR, SECS_PER_MINUTE};
use tairix_tz::rule::{leap_month_length, DayRule, Moment, TimeBase};

/// One `Rule` line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    /// The rule set it belongs to.
    pub name: String,
    /// The first year it applies.
    pub from: i64,
    /// The last year it applies; [`i64::MAX`] for `max`.
    pub to: i64,
    /// Whether `to` was written as a year rather than `max` or `only`.
    pub to_is_year: bool,
    /// When in each year it takes effect.
    pub moment: Moment,
    /// Seconds added to standard time.
    pub save: i64,
    /// Whether the saving counts as daylight saving.
    pub daylight: bool,
    /// What its zone's `%s` is replaced by.
    pub letters: String,
}

/// When an era ends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Until {
    /// The year.
    pub year: i64,
    /// The month, day and time of day, and the clock they are read on.
    pub moment: Moment,
    /// The local seconds `moment` names in `year`, before any offset applies.
    pub local: i64,
}

/// One era of a zone: one standard offset and one saving.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Era {
    /// Seconds east of UT.
    pub stdoff: i64,
    /// The rule set it follows or a fixed saving, as written; empty for none.
    pub saving: String,
    /// Its abbreviations' format.
    pub format: String,
    /// When it ends; the last era has no end.
    pub until: Option<Until>,
}

/// One zone and its eras, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Zone {
    /// Its name.
    pub name: String,
    /// Its eras.
    pub eras: Vec<Era>,
}

/// Another name for a zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    /// The name it is another name for.
    pub target: String,
    /// The name itself.
    pub name: String,
}

/// A whole source, in the order its lines stand.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Source {
    /// Every rule.
    pub rules: Vec<Rule>,
    /// Every zone.
    pub zones: Vec<Zone>,
    /// Every link.
    pub links: Vec<Link>,
}

/// Why a source line was refused, in `zic`'s own words.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    /// The 1-based line.
    pub line: usize,
    /// What is wrong with it.
    pub reason: &'static str,
}

/// The longest line `zic` reads: its line buffer less the terminating NUL.
const MAX_LINE: usize = 2047;

/// The most fields any line may hold.
const MAX_FIELDS: usize = 10;

/// The year `minimum` stands for.
const MINIMUM_YEAR: i64 = 1900;

#[derive(Copy, Clone)]
enum LineCode {
    Rule,
    Zone,
    Link,
}

#[derive(Copy, Clone)]
enum EndYear {
    Maximum,
    Only,
}

const LINE_CODES: [(&str, LineCode); 3] = [
    ("Rule", LineCode::Rule),
    ("Zone", LineCode::Zone),
    ("Link", LineCode::Link),
];

const MONTHS: [(&str, u8); 12] = [
    ("January", 1),
    ("February", 2),
    ("March", 3),
    ("April", 4),
    ("May", 5),
    ("June", 6),
    ("July", 7),
    ("August", 8),
    ("September", 9),
    ("October", 10),
    ("November", 11),
    ("December", 12),
];

const WEEKDAYS: [(&str, u8); 7] = [
    ("Sunday", 0),
    ("Monday", 1),
    ("Tuesday", 2),
    ("Wednesday", 3),
    ("Thursday", 4),
    ("Friday", 5),
    ("Saturday", 6),
];

const LASTS: [(&str, u8); 7] = [
    ("last-Sunday", 0),
    ("last-Monday", 1),
    ("last-Tuesday", 2),
    ("last-Wednesday", 3),
    ("last-Thursday", 4),
    ("last-Friday", 5),
    ("last-Saturday", 6),
];

const BEGIN_YEARS: [(&str, ()); 1] = [("minimum", ())];

const END_YEARS: [(&str, EndYear); 2] = [("maximum", EndYear::Maximum), ("only", EndYear::Only)];

/// `text` as a source.
///
/// # Errors
///
/// The first line `zic` would refuse.
pub fn parse(text: &str) -> Result<Source, SyntaxError> {
    let mut source = Source::default();
    let mut zone_names = BTreeSet::new();
    let mut continuing = false;
    let mut rest = text;
    let mut line = 0;
    while !rest.is_empty() {
        line += 1;
        let refuse = |reason| SyntaxError { line, reason };
        let (body, after) = rest.split_once('\n').ok_or(refuse("unterminated line"))?;
        rest = after;
        if body.len() > MAX_LINE {
            return Err(refuse("line too long"));
        }
        if body.contains('\0') {
            return Err(refuse("NUL input byte"));
        }
        let fields = fields(body).map_err(refuse)?;
        if fields.is_empty() {
            continue;
        }
        continuing = if continuing {
            source.era(&fields, true).map_err(refuse)?
        } else {
            match byword(&fields[0], &LINE_CODES) {
                Some(LineCode::Rule) => {
                    source.rule(&fields).map_err(refuse)?;
                    false
                }
                Some(LineCode::Zone) => source.zone(&fields, &mut zone_names).map_err(refuse)?,
                Some(LineCode::Link) => {
                    source.link(&fields).map_err(refuse)?;
                    false
                }
                None => return Err(refuse("input line of unknown type")),
            }
        };
    }
    if continuing {
        return Err(SyntaxError {
            line,
            reason: "expected continuation line not found",
        });
    }
    Ok(source)
}

impl Source {
    fn rule(&mut self, fields: &[String]) -> Result<(), &'static str> {
        let [_, name, from, to, kind, month, day, time, save, letters] = fields else {
            return Err("wrong number of fields on Rule line");
        };
        if name
            .bytes()
            .next()
            .is_none_or(|first| is_space(first) || matches!(first, b'+' | b'-' | b'0'..=b'9'))
        {
            return Err("invalid rule name");
        }
        let (save, daylight) = saving(save).ok_or("invalid saved time")?;
        let years = years(from, to)?;
        if !kind.is_empty() {
            return Err("year type is unsupported; use \"-\" instead");
        }
        self.rules.push(Rule {
            name: name.clone(),
            from: years.from,
            to: years.to,
            to_is_year: years.to_is_year,
            moment: moment(month, day, time)?,
            save,
            daylight,
            letters: letters.clone(),
        });
        Ok(())
    }

    /// A `Zone` line; `Ok(true)` when a continuation line must follow.
    fn zone(
        &mut self,
        fields: &[String],
        named: &mut BTreeSet<String>,
    ) -> Result<bool, &'static str> {
        if !(5..=9).contains(&fields.len()) {
            return Err("wrong number of fields on Zone line");
        }
        let name = &fields[1];
        if !named.insert(name.clone()) {
            return Err("duplicate zone name");
        }
        check_name(name)?;
        self.zones.push(Zone {
            name: name.clone(),
            eras: Vec::new(),
        });
        self.era(&fields[2..], false)
    }

    /// An era's fields — its own line's, or a `Zone` line's after the name;
    /// `Ok(true)` when a continuation line must follow.
    fn era(&mut self, fields: &[String], continuation: bool) -> Result<bool, &'static str> {
        let ([stdoff, saving, format, ..], 3..=7) = (fields, fields.len()) else {
            return Err("wrong number of fields on Zone continuation line");
        };
        let zone = self
            .zones
            .last_mut()
            .ok_or("continuation line outside a zone")?;
        if let Some((_, after)) = format.split_once('%') {
            let specified = after.starts_with('s') || after.starts_with('z');
            if !specified || after[1..].contains('%') || format.contains('/') {
                return Err("invalid abbreviation format");
            }
        }
        let until = match fields.get(3) {
            None => None,
            Some(year) => {
                let field = |at: usize, default: &'static str| {
                    fields.get(at).map_or(default, String::as_str)
                };
                let years = years(year, "only")?;
                let moment = moment(field(4, "Jan"), field(5, "1"), field(6, "0"))?;
                let local = moment.local_seconds(years.from).ok_or(
                    if moment.month == 2 && moment.day == DayRule::Of(29) {
                        "use of 2/29 in non leap-year"
                    } else {
                        "time overflow"
                    },
                )?;
                if continuation
                    && zone
                        .eras
                        .last()
                        .and_then(|era| era.until.as_ref())
                        .is_some_and(|previous| previous.local >= local)
                {
                    return Err(
                        "Zone continuation line end time is not after end time of previous line",
                    );
                }
                Some(Until {
                    year: years.from,
                    moment,
                    local,
                })
            }
        };
        let ends = until.is_some();
        zone.eras.push(Era {
            stdoff: hms(stdoff).ok_or("invalid UT offset")?,
            saving: saving.clone(),
            format: format.clone(),
            until,
        });
        Ok(ends)
    }

    fn link(&mut self, fields: &[String]) -> Result<(), &'static str> {
        let [_, target, name] = fields else {
            return Err("wrong number of fields on Link line");
        };
        if target.is_empty() {
            return Err("blank TARGET field on Link line");
        }
        check_name(target)?;
        check_name(name)?;
        self.links.push(Link {
            target: target.clone(),
            name: name.clone(),
        });
        Ok(())
    }
}

/// `zic`'s `getfields`: `line` split at unquoted white space, stopping at an
/// unquoted `#`, with quotes removed and a lone `-` read as an empty field.
fn fields(line: &str) -> Result<Vec<String>, &'static str> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        while bytes.get(at).copied().is_some_and(is_space) {
            at += 1;
        }
        if bytes.get(at).is_none_or(|byte| *byte == b'#') {
            return Ok(out);
        }
        let mut field = Vec::new();
        while let Some(&byte) = bytes.get(at) {
            at += 1;
            if byte == b'"' {
                loop {
                    let quoted = *bytes.get(at).ok_or("Odd number of quotation marks")?;
                    at += 1;
                    if quoted == b'"' {
                        break;
                    }
                    field.push(quoted);
                }
            } else {
                field.push(byte);
            }
            if bytes
                .get(at)
                .is_none_or(|next| *next == b'#' || is_space(*next))
            {
                break;
            }
        }
        if out.len() == MAX_FIELDS {
            return Err("Too many input fields");
        }
        let field = String::from_utf8(field).map_err(|_| "a field is not UTF-8")?;
        out.push(if field == "-" { String::new() } else { field });
    }
}

/// `zic`'s white space, which is also the C locale's that `sscanf` skips.
const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\x0c' | b'\n' | b'\r' | b'\t' | b'\x0b')
}

/// `zic`'s `byword`: `word` matched against `table` exactly, ignoring ASCII
/// case, else as a prefix of exactly one entry.
fn byword<T: Copy>(word: &str, table: &[(&str, T)]) -> Option<T> {
    if let Some((_, value)) = table
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(word))
    {
        return Some(*value);
    }
    let mut found = None;
    for (name, value) in table {
        let prefix = name
            .get(..word.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(word));
        if prefix {
            if found.is_some() {
                return None;
            }
            found = Some(*value);
        }
    }
    found
}

/// The weekday `word` names as a month's last: `lastSun` and its
/// abbreviations, or the `last-Sunday` spelling `zic` still accepts.
fn last_weekday(word: &str) -> Option<u8> {
    let named = word
        .get(..4)
        .filter(|head| head.eq_ignore_ascii_case("last"))
        .and_then(|_| word.get(4..))
        .filter(|rest| !rest.is_empty() && !rest.starts_with('-'));
    match named {
        Some(weekday) => byword(weekday, &WEEKDAYS),
        None => byword(word, &LASTS),
    }
}

struct Years {
    from: i64,
    to: i64,
    to_is_year: bool,
}

fn years(from: &str, to: &str) -> Result<Years, &'static str> {
    let from = match byword(from, &BEGIN_YEARS) {
        Some(()) => MINIMUM_YEAR,
        None => integer(from).ok_or("invalid starting year")?,
    };
    let (to, to_is_year) = match byword(to, &END_YEARS) {
        Some(EndYear::Maximum) => (i64::MAX, false),
        Some(EndYear::Only) => (from, false),
        None => (integer(to).ok_or("invalid ending year")?, true),
    };
    if from > to {
        return Err("starting year greater than ending year");
    }
    Ok(Years {
        from,
        to,
        to_is_year,
    })
}

/// A rule's or an era end's month, day and time of day.
fn moment(month: &str, day: &str, time: &str) -> Result<Moment, &'static str> {
    let month = byword(month, &MONTHS).ok_or("invalid month name")?;
    let (time, base) = match time.as_bytes().last().map(u8::to_ascii_lowercase) {
        Some(b's') => (&time[..time.len() - 1], TimeBase::Standard),
        Some(b'w') => (&time[..time.len() - 1], TimeBase::Wall),
        Some(b'g' | b'u' | b'z') => (&time[..time.len() - 1], TimeBase::Universal),
        _ => (time, TimeBase::Wall),
    };
    let at = hms(time).ok_or("invalid time of day")?;
    Ok(Moment {
        month,
        day: day_rule(day, leap_month_length(month))?,
        at: i32::try_from(at).map_err(|_| "time of day out of range")?,
        base,
    })
}

/// A day of a month whose leap-year length is `leap_length`: `lastSun`,
/// `Sun>=8`, `Sun<=25` or a plain day.
fn day_rule(day: &str, leap_length: u8) -> Result<DayRule, &'static str> {
    if let Some(weekday) = last_weekday(day) {
        return Ok(DayRule::OnOrBefore {
            weekday,
            day: leap_length,
        });
    }
    let (weekday, number, before) = match day.find('<').or_else(|| day.find('>')) {
        None => (None, day, false),
        Some(split) => {
            let number = day[split + 1..]
                .strip_prefix('=')
                .ok_or("invalid day of month")?;
            let weekday = byword(&day[..split], &WEEKDAYS).ok_or("invalid weekday name")?;
            (Some(weekday), number, day.as_bytes()[split] == b'<')
        }
    };
    let number = integer(number)
        .and_then(|number| u8::try_from(number).ok())
        .filter(|number| (1..=leap_length).contains(number))
        .ok_or("invalid day of month")?;
    Ok(match weekday {
        None => DayRule::Of(number),
        Some(weekday) if before => DayRule::OnOrBefore {
            weekday,
            day: number,
        },
        Some(weekday) => DayRule::OnOrAfter {
            weekday,
            day: number,
        },
    })
}

/// `zic`'s `getsave`: a saving and whether it counts as daylight saving, a
/// trailing `d` or `s` saying so explicitly.
pub fn saving(field: &str) -> Option<(i64, bool)> {
    let (amount, stated) = match field.as_bytes().last() {
        Some(b'd') => (&field[..field.len() - 1], Some(true)),
        Some(b's') => (&field[..field.len() - 1], Some(false)),
        _ => (field, None),
    };
    let save = hms(amount)?;
    Some((save, stated.unwrap_or(save != 0)))
}

/// A whole decimal integer as `sscanf("%d%c")` reads one alone: an optional
/// sign, digits, and nothing after.
fn integer(text: &str) -> Option<i64> {
    let mut scan = Scan(text.as_bytes());
    let value = scan.int()?;
    scan.0.is_empty().then_some(value)
}

/// `zic`'s `gethms`: `[-]h[:mm[:ss[.fraction]]]` in seconds, the fraction
/// rounded half to even; an empty string is zero.
fn hms(text: &str) -> Option<i64> {
    if text.is_empty() {
        return Some(0);
    }
    let (sign, text) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text),
    };
    let fields = Hms::scan(text.as_bytes());
    let separated = |at: Option<u8>, separator| at == Some(separator);
    let ok = match fields.count {
        8 => {
            fields.next_digit.is_ascii_digit()
                && separated(fields.second_mark, b'.')
                && separated(fields.minute_mark, b':')
                && separated(fields.hour_mark, b':')
        }
        7 => {
            separated(fields.second_mark, b'.')
                && separated(fields.minute_mark, b':')
                && separated(fields.hour_mark, b':')
        }
        5 => separated(fields.minute_mark, b':') && separated(fields.hour_mark, b':'),
        3 => separated(fields.hour_mark, b':'),
        1 => true,
        _ => false,
    };
    if !ok
        || fields.hours < 0
        || !(0..60).contains(&fields.minutes)
        || !(0..=60).contains(&fields.seconds)
    {
        return None;
    }
    let exactly_half_even = i64::from(fields.next_digit == b'0') & ((fields.seconds ^ 1) & 1);
    let seconds = fields.seconds + i64::from(5 + exactly_half_even <= fields.tenths);
    let whole = fields.hours.checked_mul(sign * SECS_PER_HOUR)?;
    let total = whole.checked_add(sign * (fields.minutes * SECS_PER_MINUTE + seconds))?;
    total.checked_mul(4).map(|_| total)
}

/// What `gethms`'s `sscanf` assigns, and how many conversions it made.
struct Hms {
    count: u8,
    hours: i64,
    hour_mark: Option<u8>,
    minutes: i64,
    minute_mark: Option<u8>,
    seconds: i64,
    second_mark: Option<u8>,
    tenths: i64,
    next_digit: u8,
}

impl Hms {
    /// `sscanf(text, "%d%c%d%c%d%c%1d%*[0]%c%*[0-9]%c", …)`.
    fn scan(text: &[u8]) -> Self {
        let mut fields = Self {
            count: 0,
            hours: 0,
            hour_mark: None,
            minutes: 0,
            minute_mark: None,
            seconds: 0,
            second_mark: None,
            tenths: 0,
            next_digit: b'0',
        };
        fields.convert(&mut Scan(text));
        fields
    }

    /// Each conversion in turn, stopping at the first that fails.
    fn convert(&mut self, scan: &mut Scan<'_>) -> Option<()> {
        self.hours = scan.int()?;
        self.count = 1;
        self.hour_mark = Some(scan.byte()?);
        self.count = 2;
        self.minutes = scan.int()?;
        self.count = 3;
        self.minute_mark = Some(scan.byte()?);
        self.count = 4;
        self.seconds = scan.int()?;
        self.count = 5;
        self.second_mark = Some(scan.byte()?);
        self.count = 6;
        self.tenths = scan.digit()?;
        self.count = 7;
        scan.zeros().then_some(())?;
        self.next_digit = scan.byte()?;
        self.count = 8;
        scan.digits().then_some(())?;
        scan.byte()?;
        self.count = 9;
        Some(())
    }
}

/// The input of a `sscanf` emulation.
struct Scan<'a>(&'a [u8]);

impl Scan<'_> {
    /// `%c`.
    fn byte(&mut self) -> Option<u8> {
        let (&first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(first)
    }

    /// `%d`: white space, an optional sign and at least one digit.
    fn int(&mut self) -> Option<i64> {
        self.skip(is_space);
        let (negative, unsigned) = match self.0.split_first() {
            Some((b'-', rest)) => (true, rest),
            Some((b'+', rest)) => (false, rest),
            _ => (false, self.0),
        };
        let length = unsigned.iter().take_while(|b| b.is_ascii_digit()).count();
        if length == 0 {
            return None;
        }
        let mut value: i64 = 0;
        for digit in &unsigned[..length] {
            value = value
                .checked_mul(10)?
                .checked_add(i64::from(digit - b'0'))?;
        }
        self.0 = &unsigned[length..];
        Some(if negative { -value } else { value })
    }

    /// `%1d`: white space and one digit.
    fn digit(&mut self) -> Option<i64> {
        self.skip(is_space);
        let (&first, rest) = self.0.split_first()?;
        first.is_ascii_digit().then(|| {
            self.0 = rest;
            i64::from(first - b'0')
        })
    }

    /// `%*[0]`: at least one `0`.
    fn zeros(&mut self) -> bool {
        self.skip(|byte| byte == b'0')
    }

    /// `%*[0123456789]`: at least one digit.
    fn digits(&mut self) -> bool {
        self.skip(|byte| byte.is_ascii_digit())
    }

    fn skip(&mut self, matches: impl Fn(u8) -> bool) -> bool {
        let length = self.0.iter().take_while(|byte| matches(**byte)).count();
        self.0 = &self.0[length..];
        length > 0
    }
}

/// `zic`'s `namecheck`: no empty component and none that is `.` or `..`.
fn check_name(name: &str) -> Result<(), &'static str> {
    for component in name.split('/') {
        if component.is_empty() {
            return Err("file name has an empty component");
        }
        if component == "." || component == ".." {
            return Err("file name contains a '.' or '..' component");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
