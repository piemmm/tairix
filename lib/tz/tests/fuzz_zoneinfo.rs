//! Deterministic fuzz harness for the zone store's reader: `ZoneInfo::parse`
//! and everything an accepted store answers.
//!
//! The store is system-shipped and integrity-checked, but its reader is held
//! to the bar of an untrusted-input decoder all the same. The invariants:
//!
//! * whatever the writer accepts, the reader accepts — one writer, one reader;
//! * parsing any byte string — noise, or a valid store with bytes flipped and
//!   its checksum recomputed so the damage reaches the validation behind it —
//!   never panics, and an accepted store answers every query without
//!   panicking;
//! * in a generated store, every instant reads back through its local time.
//!
//! A plain `cargo test` runs [`SMOKE_ITERATIONS`] from a fresh, logged seed;
//! `cargo xtask fuzz` extends the loop to a wall-clock budget.

use tairix_abi::time::{CivilTime, Time64, SECS_PER_MINUTE};
use tairix_fuzzseed::Prng;
use tairix_tz::rule::{DayRule, Moment, TimeBase};
use tairix_tz::testkit::reseal;
use tairix_tz::write::{
    build, PhaseInput, PlaceInput, RegionInput, StoreInput, TrailingInput, TransitionInput,
    ZoneInput,
};
use tairix_tz::{resolve_region, resolve_zone, CivilClock, Disambiguation, ZoneInfo};

/// Fixed-iteration sweep run once by a plain `cargo test`.
const SMOKE_ITERATIONS: u64 = 2_000;

/// Largest noise string fed to the parser.
const MAX_NOISE: usize = 512;

/// The farthest year from the epoch the round trip is held to: the civil
/// calendar saturates far beyond it.
const ROUND_TRIP_YEARS: u64 = 10_000;

const NAMES: &[&str] = &[
    "Europe/London",
    "America/New_York",
    "Asia/Kolkata",
    "Etc/GMT+5",
    "Pacific/Chatham",
    "America/Argentina/Buenos_Aires",
    "UTC",
];

const ABBREVIATIONS: &[&str] = &["GMT", "BST", "+0545", "-03", "LMT", "EST", "EDT", "+1245"];

const REGIONS: &[(&str, &str, &str, &str)] = &[
    ("GB", "United Kingdom", "dd/MM/y", "HH:mm"),
    ("US", "United States", "M/d/y", "h:mm a"),
    ("KR", "South Korea", "y. M. d.", "a h:mm"),
    ("FI", "Finland", "d.M.y", "H.mm"),
];

fn small(rng: &mut Prng, below: usize) -> u8 {
    u8::try_from(rng.below(below)).expect("a small draw")
}

fn signed(rng: &mut Prng, magnitude: usize) -> i64 {
    i64::try_from(rng.below(2 * magnitude + 1)).expect("a small draw")
        - i64::try_from(magnitude).expect("a small bound")
}

/// An offset within fourteen hours either side, in quarter hours.
fn offset(rng: &mut Prng) -> i32 {
    i32::try_from(signed(rng, 56) * 900).expect("within a day")
}

fn moment(rng: &mut Prng) -> Moment {
    let day = match rng.below(3) {
        0 => DayRule::Of(1 + small(rng, 28)),
        1 => DayRule::OnOrAfter {
            weekday: small(rng, 7),
            day: 1 + small(rng, 22),
        },
        _ => DayRule::OnOrBefore {
            weekday: small(rng, 7),
            day: 7 + small(rng, 22),
        },
    };
    Moment {
        month: 1 + small(rng, 12),
        day,
        at: i32::try_from(signed(rng, 24) * 1800).expect("within a day"),
        base: *rng.pick(&[TimeBase::Wall, TimeBase::Standard, TimeBase::Universal]),
    }
}

/// A zone whose changes stand at least three days apart, as every real
/// zone's do.
fn zone(rng: &mut Prng, name: &str) -> ZoneInput {
    let abbreviation = |rng: &mut Prng| String::from(*rng.pick(ABBREVIATIONS));
    let mut transitions = vec![TransitionInput {
        at: i64::MIN,
        offset: offset(rng),
        abbreviation: abbreviation(rng),
        daylight: false,
    }];
    let mut at = -i64::try_from(rng.below(6_000_000_000)).expect("a small draw");
    for _ in 0..rng.at_most(12) {
        transitions.push(TransitionInput {
            at,
            offset: offset(rng),
            abbreviation: abbreviation(rng),
            daylight: rng.below(2) == 0,
        });
        at += 3 * 86_400 + i64::try_from(rng.below(600_000_000)).expect("a small draw");
    }
    let trailing = (rng.below(2) == 0).then(|| TrailingInput {
        stdoff: offset(rng),
        daylight: PhaseInput {
            save: *rng.pick(&[1800, 3600, -3600]),
            abbreviation: abbreviation(rng),
            moment: moment(rng),
        },
        standard: PhaseInput {
            save: 0,
            abbreviation: abbreviation(rng),
            moment: moment(rng),
        },
    });
    ZoneInput {
        name: String::from(name),
        transitions,
        trailing,
    }
}

fn store_input(rng: &mut Prng) -> StoreInput {
    let mut names: Vec<&str> = NAMES.to_vec();
    let mut zones = Vec::new();
    for _ in 0..=rng.at_most(3) {
        let name = names.swap_remove(rng.below(names.len()));
        zones.push(zone(rng, name));
    }
    let aliases = match names.pop() {
        Some(alias) if rng.below(2) == 0 => {
            vec![(String::from(alias), zones[0].name.clone())]
        }
        _ => Vec::new(),
    };
    let regions: Vec<RegionInput> = REGIONS[..=rng.at_most(REGIONS.len() - 1)]
        .iter()
        .map(|&(code, name, date, time)| RegionInput {
            code: String::from(code),
            name: String::from(name),
            date: String::from(date),
            time: String::from(time),
        })
        .collect();
    let places = (0..rng.at_most(5))
        .map(|_| PlaceInput {
            zone: rng.pick(&zones).name.clone(),
            region: rng.pick(&regions).code.clone(),
            comment: String::from(*rng.pick(&["", "Eastern (most areas)", "Île"])),
        })
        .collect();
    StoreInput {
        release: String::from("2026e"),
        default_zone: zones[0].name.clone(),
        default_region: regions[0].code.clone(),
        zones,
        aliases,
        regions,
        places,
    }
}

fn instant(rng: &mut Prng) -> i64 {
    match rng.below(6) {
        0 => i64::MIN,
        1 => i64::MAX,
        2 => i64::try_from(rng.next_u64() >> 1).expect("a non-negative draw"),
        _ => signed(rng, 4_000_000_000_000).saturating_mul(80),
    }
}

/// Every query an accepted store answers, none of which may panic.
fn exercise(info: &ZoneInfo<'_>, rng: &mut Prng) {
    let mut text = String::new();
    let _ = info.release();
    for zone in info.zones().chain(core::iter::once(info.default_zone())) {
        let _ = zone.name();
        for _ in 0..4 {
            let at = Time64::from_secs(instant(rng));
            let _ = zone.offset_at(at);
            let reading = zone.local(at);
            let _ = zone.instant_from_local(&reading.civil, Disambiguation::Reject);
            let next = zone.next_minute(at);
            let ahead = next.secs().saturating_sub(at.secs());
            assert!(
                (1..=SECS_PER_MINUTE).contains(&ahead) || at.secs() > i64::MAX - SECS_PER_MINUTE,
                "{}: the minute after {at:?} turns at {next:?}",
                zone.name()
            );
        }
        let civil = CivilTime {
            year: signed(rng, 3000),
            month: u32::from(small(rng, 14)),
            day: u32::from(small(rng, 33)),
            hour: u32::from(small(rng, 26)),
            minute: u32::from(small(rng, 61)),
            second: 0,
        };
        for choice in [
            Disambiguation::Reject,
            Disambiguation::Earlier,
            Disambiguation::Later,
        ] {
            let _ = zone.instant_from_local(&civil, choice);
        }
    }
    for region in info.regions() {
        let clock = CivilClock::new(info.default_zone(), Some(&region));
        let reading = clock.local(Time64::from_secs(instant(rng)));
        text.clear();
        let _ = clock.write_date(&reading, &mut text);
        let _ = clock.write_time(&reading, &mut text);
        let _ = info.region(region.code);
    }
    for place in info.places() {
        text.clear();
        let _ = place.write_city(&mut text);
    }
    let _ = info.default_region();
    let preference = *rng.pick(NAMES);
    let _ = resolve_zone(Some(info), &[preference, "Mars/Olympus"]);
    let _ = resolve_region(Some(info), &["ZZ", rng.pick(REGIONS).0]);
    let _ = info.zone(preference);
}

/// Every instant of a generated store's zones reads back through its local
/// time.
fn round_trips(info: &ZoneInfo<'_>, rng: &mut Prng) {
    let bound = ROUND_TRIP_YEARS * 31_556_952;
    for zone in info.zones() {
        for _ in 0..8 {
            let at = signed(rng, usize::try_from(bound).expect("a small bound"));
            let reading = zone.local(Time64::from_secs(at));
            let back = zone
                .instant_from_local(&reading.civil, Disambiguation::Earlier)
                .unwrap_or_else(|error| {
                    panic!(
                        "{} at {at}: {:?} reads back as {error:?}",
                        zone.name(),
                        reading.civil
                    )
                });
            assert_eq!(
                zone.local(back).civil,
                reading.civil,
                "{} at {at}",
                zone.name()
            );
        }
    }
}

#[test]
fn any_bytes_parse_or_are_refused_and_an_accepted_store_answers_everything() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_bytes_parse_or_are_refused_and_an_accepted_store_answers_everything",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        if let Ok(store) = build(&store_input(&mut rng)) {
            let info = ZoneInfo::parse(&store).expect("whatever the writer accepts parses");
            exercise(&info, &mut rng);
            round_trips(&info, &mut rng);
            let mut damaged = store.clone();
            for _ in 0..=rng.at_most(6) {
                let at = rng.below(damaged.len());
                damaged[at] ^= rng.next_u8() | 1;
            }
            if rng.below(4) == 0 {
                damaged.truncate(rng.below(damaged.len()));
            }
            if rng.below(2) == 0 {
                reseal(&mut damaged);
            }
            if let Ok(info) = ZoneInfo::parse(&damaged) {
                exercise(&info, &mut rng);
            }
        }
        let mut noise = vec![0; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        if noise.len() >= 4 {
            noise[..4].copy_from_slice(b"TZX1");
        }
        reseal(&mut noise);
        if let Ok(info) = ZoneInfo::parse(&noise) {
            exercise(&info, &mut rng);
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
