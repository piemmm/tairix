//! The compiled store against IANA's own build: every canonical zone reads,
//! at every instant, what `zic` 2026e's slim `TZif` file for it reads.
//!
//! `reference/` holds those files, compiled by `zic -b slim` from the same
//! `tzdata.zi`. Each is read by its RFC 9636 layout and its TZ string by
//! POSIX's rules, independently of the store's own rule arithmetic, and the
//! two are compared just before and at every change either makes, through
//! the far future.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use tairix_abi::time::{days_from_civil, days_in_month, weekday_from_days, CivilTime, Time64};
use tairix_tz::write::{StoreInput, ZoneInput};
use tairix_tz::ZoneInfo;

/// The years whose rule-driven changes are compared; earlier instants are
/// compared at fixed points.
const FIRST_YEAR: i64 = 1800;
const LAST_YEAR: i64 = 2500;

/// What a reading says: seconds east of UT, the abbreviation, and whether
/// daylight saving is in force.
type Reading = (i32, String, bool);

/// One reference `TZif` file's 64-bit data and footer.
struct Reference {
    types: Vec<Reading>,
    transitions: Vec<(i64, usize)>,
    footer: Option<Posix>,
}

/// A big-endian reader over a `TZif` file.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn take(&mut self, len: usize) -> &[u8] {
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        head
    }

    fn u32(&mut self) -> usize {
        let word: [u8; 4] = self.take(4).try_into().expect("four bytes");
        usize::try_from(u32::from_be_bytes(word)).expect("a count fits")
    }

    fn i64(&mut self) -> i64 {
        i64::from_be_bytes(self.take(8).try_into().expect("eight bytes"))
    }

    /// A header's counts: `isut`, `isstd`, `leap`, `time`, `type`, `char`.
    fn header(&mut self) -> [usize; 6] {
        assert_eq!(self.take(4), b"TZif", "magic");
        assert!(self.take(1)[0] >= b'2', "a version-2 file or later");
        self.take(15);
        [
            self.u32(),
            self.u32(),
            self.u32(),
            self.u32(),
            self.u32(),
            self.u32(),
        ]
    }
}

fn read_reference(name: &str) -> Reference {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/reference")
        .join(name);
    let file = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut bytes = Bytes(&file);
    let [isut, isstd, leap, time, kind, chars] = bytes.header();
    bytes.take(time * 5 + kind * 6 + chars + leap * 8 + isstd + isut);
    let [isut, isstd, leap, time, kind, chars] = bytes.header();
    let ats: Vec<i64> = (0..time).map(|_| bytes.i64()).collect();
    let indices = bytes.take(time).to_vec();
    let infos: Vec<(i32, bool, usize)> = (0..kind)
        .map(|_| {
            let info = bytes.take(6);
            let offset = i32::from_be_bytes(info[..4].try_into().expect("four bytes"));
            (offset, info[4] == 1, usize::from(info[5]))
        })
        .collect();
    let designations = bytes.take(chars).to_vec();
    bytes.take(leap * 12 + isstd + isut);
    assert_eq!(leap, 0, "{name}: no leap seconds");
    let types = infos
        .iter()
        .map(|&(offset, daylight, at)| {
            let end = designations[at..]
                .iter()
                .position(|byte| *byte == 0)
                .expect("a terminated abbreviation");
            let abbreviation = std::str::from_utf8(&designations[at..at + end]).expect("text");
            (offset, String::from(abbreviation), daylight)
        })
        .collect();
    let footer = std::str::from_utf8(bytes.0)
        .expect("a text footer")
        .strip_prefix('\n')
        .and_then(|rest| rest.strip_suffix('\n'))
        .expect("a newline-framed footer");
    Reference {
        types,
        transitions: ats
            .into_iter()
            .zip(indices.into_iter().map(usize::from))
            .collect(),
        footer: (!footer.is_empty()).then(|| Posix::parse(footer)),
    }
}

impl Reference {
    fn reading(&self, at: i64) -> Reading {
        let before = self.transitions.partition_point(|(time, _)| *time <= at);
        let after_last = self.transitions.last().is_none_or(|(last, _)| *last < at);
        match (&self.footer, before) {
            (Some(footer), _) if after_last => footer.reading(at),
            (_, 0) => self.types[0].clone(),
            _ => self.types[self.transitions[before - 1].1].clone(),
        }
    }
}

/// A POSIX TZ string: standard time, and the rules of any daylight saving.
struct Posix {
    standard: (String, i32),
    daylight: Option<Daylight>,
}

struct Daylight {
    name: String,
    offset: i32,
    start: PosixRule,
    end: PosixRule,
}

/// A date and a time of day on the clock in force before the change.
struct PosixRule {
    date: PosixDate,
    time: i64,
}

enum PosixDate {
    /// `Jn`: day `n` of a year whose February always has 28 days.
    Julian(i64),
    /// `n`: day `n` counted from 0, leap days included.
    Zero(i64),
    /// `Mm.w.d`: weekday `d` of week `w` of month `m`, week 5 the last.
    Week { month: u32, week: u32, weekday: u32 },
}

/// A cursor over a TZ string.
struct Text<'a>(&'a str);

impl Text<'_> {
    fn eat(&mut self, prefix: char) -> bool {
        self.0.strip_prefix(prefix).is_some_and(|rest| {
            self.0 = rest;
            true
        })
    }

    fn number(&mut self) -> i64 {
        let length = self.0.bytes().take_while(u8::is_ascii_digit).count();
        let (digits, rest) = self.0.split_at(length);
        self.0 = rest;
        digits.parse().expect("a number")
    }

    fn name(&mut self) -> String {
        let (name, rest) = if self.eat('<') {
            self.0.split_once('>').expect("a closed quoted name")
        } else {
            let length = self.0.bytes().take_while(u8::is_ascii_alphabetic).count();
            self.0.split_at(length)
        };
        self.0 = rest;
        String::from(name)
    }

    /// `[+-]hh[:mm[:ss]]` in seconds.
    fn hms(&mut self) -> i64 {
        let sign = if self.eat('-') {
            -1
        } else {
            self.eat('+');
            1
        };
        let mut seconds = self.number() * 3600;
        if self.eat(':') {
            seconds += self.number() * 60;
            if self.eat(':') {
                seconds += self.number();
            }
        }
        sign * seconds
    }

    fn rule(&mut self) -> PosixRule {
        let date = if self.eat('J') {
            PosixDate::Julian(self.number())
        } else if self.eat('M') {
            let month = self.number();
            assert!(self.eat('.'));
            let week = self.number();
            assert!(self.eat('.'));
            let weekday = self.number();
            assert!(
                (1..=12).contains(&month) && (1..=5).contains(&week) && (0..=6).contains(&weekday),
                "M{month}.{week}.{weekday} names no weekday of a month"
            );
            let small = |value: i64| u32::try_from(value).expect("a small field");
            PosixDate::Week {
                month: small(month),
                week: small(week),
                weekday: small(weekday),
            }
        } else {
            PosixDate::Zero(self.number())
        };
        let time = if self.eat('/') { self.hms() } else { 7200 };
        PosixRule { date, time }
    }
}

impl Posix {
    fn parse(text: &str) -> Self {
        let mut text = Text(text);
        let utc = |posix: i64| i32::try_from(-posix).expect("an offset");
        let standard_name = text.name();
        let standard = text.hms();
        if text.0.is_empty() {
            return Self {
                standard: (standard_name, utc(standard)),
                daylight: None,
            };
        }
        let name = text.name();
        let daylight = if text.0.starts_with(',') {
            standard - 3600
        } else {
            text.hms()
        };
        assert!(text.eat(','), "a start rule");
        let start = text.rule();
        assert!(text.eat(','), "an end rule");
        let end = text.rule();
        assert!(text.0.is_empty(), "nothing after the rules");
        Self {
            standard: (standard_name, utc(standard)),
            daylight: Some(Daylight {
                name,
                offset: utc(daylight),
                start,
                end,
            }),
        }
    }

    fn reading(&self, at: i64) -> Reading {
        let (name, offset) = &self.standard;
        let Some(daylight) = &self.daylight else {
            return (*offset, name.clone(), false);
        };
        let year = CivilTime::from_unix_secs(at.saturating_add(i64::from(*offset))).year;
        let mut latest: Option<(i64, bool)> = None;
        for year in year - 1..=year + 1 {
            for (change, into_daylight) in daylight.changes(year, *offset) {
                // Of two changes at one instant the later rule decides: a
                // string for daylight time all year ends each year as the
                // next begins.
                if change <= at && latest.is_none_or(|(best, _)| change >= best) {
                    latest = Some((change, into_daylight));
                }
            }
        }
        match latest {
            Some((_, true)) => (daylight.offset, daylight.name.clone(), true),
            _ => (*offset, name.clone(), false),
        }
    }
}

impl Daylight {
    /// The year's change into daylight saving, read on standard time, and
    /// back out, read on daylight time.
    fn changes(&self, year: i64, standard: i32) -> [(i64, bool); 2] {
        [
            (self.start.local(year) - i64::from(standard), true),
            (self.end.local(year) - i64::from(self.offset), false),
        ]
    }
}

impl PosixRule {
    fn local(&self, year: i64) -> i64 {
        let leap = days_in_month(year, 2) == 29;
        let days = match self.date {
            PosixDate::Julian(day) => {
                days_from_civil(year, 1, 1).expect("a representable date") + day - 1
                    + i64::from(leap && day >= 60)
            }
            PosixDate::Zero(day) => {
                days_from_civil(year, 1, 1).expect("a representable date") + day
            }
            PosixDate::Week {
                month,
                week,
                weekday,
            } => {
                let first = days_from_civil(year, month, 1).expect("a representable date");
                let first_weekday = weekday_from_days(first) % 7;
                let mut day =
                    i64::from((weekday + 7 - first_weekday) % 7) + i64::from(week - 1) * 7;
                while day >= i64::from(days_in_month(year, month)) {
                    day -= 7;
                }
                first + day
            }
        };
        days * 86_400 + self.time
    }
}

fn store() -> (StoreInput, Vec<u8>) {
    let input = tairix_tzcompile::store_input().expect("the store's input resolves");
    let bytes = tairix_tzcompile::compile().expect("the store compiles");
    (input, bytes)
}

/// The years after `last`, the last explicit transition of one side, in
/// which that side's rules may change its reading.
fn rule_years(last: i64) -> core::ops::RangeInclusive<i64> {
    let year = CivilTime::from_unix_secs(last).year;
    (year - 1).max(FIRST_YEAR)..=LAST_YEAR
}

/// Every instant either side's rules change it at: the reference's TZ string
/// from its last transition, and the zone's trailing rule from its own.
fn rule_changes(reference: &Reference, zone: &ZoneInput) -> Vec<i64> {
    let mut changes = Vec::new();
    if let Some(Posix {
        standard: (_, offset),
        daylight: Some(daylight),
    }) = &reference.footer
    {
        let last = reference.transitions.last().map_or(i64::MIN, |(at, _)| *at);
        for year in rule_years(last) {
            changes.extend(daylight.changes(year, *offset).map(|(at, _)| at));
        }
    }
    if let Some(rule) = &zone.trailing {
        let last = zone
            .transitions
            .last()
            .map_or(i64::MIN, |transition| transition.at);
        for year in rule_years(last) {
            let into_daylight = rule
                .daylight
                .moment
                .instant(year, rule.stdoff, rule.standard.save);
            let into_standard = rule
                .standard
                .moment
                .instant(year, rule.stdoff, rule.daylight.save);
            changes.extend(into_daylight.into_iter().chain(into_standard));
        }
    }
    changes
}

#[test]
fn every_zone_reads_at_every_instant_what_zics_own_build_reads() {
    let (input, bytes) = store();
    let info = ZoneInfo::parse(&bytes).expect("the store parses");
    assert_eq!(info.release(), "2026e");
    let mut compared = 0usize;
    for zone in &input.zones {
        let reference = read_reference(&zone.name);
        let stored = info.zone(&zone.name).expect("every zone is stored");
        let ours = |at: i64| -> Reading {
            let offset = stored.offset_at(Time64::from_secs(at));
            (
                offset.utc_offset,
                String::from(offset.abbreviation),
                offset.daylight,
            )
        };
        let mut instants: Vec<i64> = zone
            .transitions
            .iter()
            .skip(1)
            .map(|transition| transition.at)
            .chain(reference.transitions.iter().map(|(at, _)| *at))
            .chain(rule_changes(&reference, zone))
            .collect();
        instants.extend([
            i64::MIN / 2,
            days_from_civil(1800, 1, 1).expect("a representable date") * 86_400,
            days_from_civil(3000, 7, 1).expect("a representable date") * 86_400,
            days_from_civil(9999, 1, 15).expect("a representable date") * 86_400,
        ]);
        for at in instants {
            for at in [at.saturating_sub(1), at] {
                assert_eq!(ours(at), reference.reading(at), "{} at {at}", zone.name);
                compared += 1;
            }
        }
    }
    assert_eq!(input.zones.len(), 345, "every canonical zone of 2026e");
    assert!(compared > 100_000, "{compared} readings compared");
}

/// Each link the vendored rules state, its target followed to a zone: read
/// from the rules' own `L` lines, not taken from the compiler.
fn links() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/tzdb/tzdata.zi");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let stated: BTreeMap<String, String> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            (fields.next() == Some("L")).then(|| {
                let target = fields.next().expect("a link names its target");
                let name = fields.next().expect("a link names itself");
                (String::from(name), String::from(target))
            })
        })
        .collect();
    stated
        .iter()
        .map(|(name, target)| {
            let mut target = target;
            for _ in 0..stated.len() {
                match stated.get(target) {
                    Some(next) => target = next,
                    None => return (name.clone(), target.clone()),
                }
            }
            panic!("link {name} is part of a cycle")
        })
        .collect()
}

#[test]
fn every_link_names_its_targets_zone_and_keeps_its_own_name() {
    let (input, bytes) = store();
    let info = ZoneInfo::parse(&bytes).expect("the store parses");
    let links = links();
    assert_eq!(links.len(), 253, "every link of 2026e");
    let compiled: BTreeMap<String, String> = input.aliases.iter().cloned().collect();
    assert_eq!(compiled, links, "the compiler's links are the rules' own");
    for (alias, target) in &links {
        let aliased = info.zone(alias).expect("every alias is stored");
        let canonical = info.zone(target).expect("its target is a zone");
        assert_eq!(aliased.name(), alias);
        for at in [
            0,
            days_from_civil(2026, 7, 1).expect("a representable date") * 86_400,
        ] {
            let at = Time64::from_secs(at);
            assert_eq!(aliased.offset_at(at), canonical.offset_at(at), "{alias}");
        }
    }
}

/// The reference build is the one this test was written against: every file
/// `zic` wrote, by path and SHA-256, hashes to the pinned manifest.
#[test]
fn the_reference_files_are_the_pinned_build() {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(&path, root, files);
            } else {
                let name = path
                    .strip_prefix(root)
                    .expect("beneath the root")
                    .components()
                    .map(|part| part.as_os_str().to_str().expect("a text name"))
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((name, std::fs::read(&path).expect("a readable file")));
            }
        }
    }
    let hex = |digest: [u8; tairix_crypto::SHA256_OUTPUT_LEN]| -> String {
        digest.iter().fold(String::new(), |mut text, byte| {
            // A `String` takes any write, so there is no failure to keep.
            let _ = write!(text, "{byte:02x}");
            text
        })
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference");
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    let manifest = files.iter().fold(String::new(), |mut text, (name, bytes)| {
        let _ = writeln!(text, "{}  {name}", hex(tairix_crypto::sha256(bytes)));
        text
    });
    assert_eq!(files.len(), 345, "one file per canonical zone");
    assert_eq!(
        hex(tairix_crypto::sha256(manifest.as_bytes())),
        "8f086fd89e0dc9df8755fb38bb65b9f10cc9049307b9736d47589dc788941ca5"
    );
}
