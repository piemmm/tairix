//! The host-only compiler of the civil-time zone store,
//! `/System/Zoneinfo/zones.tzx` (`plans/TIMEZONES.md`).
//!
//! Its inputs are vendored under `data/`, each pinned by its SHA-256: IANA
//! tzdb's rules (`tzdata.zi`, the main data form its release's
//! `make tzdata.zi` builds) and zone table (`zone.tab`), and each region's
//! conventions (`regions.tsv`, derived from Unicode CLDR). The rules are read
//! and resolved with `zic` 2026e's own semantics, and the oracle test holds
//! every zone to what `zic` writes for it. The store is written by
//! `tairix_tz`'s one writer, which refuses whatever its reader would.

mod outzone;
mod source;
mod tables;

use std::collections::BTreeMap;
use std::fmt;

use tairix_abi::time::Time64;
use tairix_crypto::{sha256, SHA256_OUTPUT_LEN};
use tairix_tz::write::{
    build, BuildError, PhaseInput, StoreInput, TrailingInput, TransitionInput, ZoneInput,
};
use tairix_tz::ZoneInfo;
use tairix_util::hex;

pub use outzone::ZoneError;

use outzone::{Future, Phase, Plan, Saving, Type};
use source::{Rule, Source, Zone};

/// The region a reader who has stated none is in.
const DEFAULT_REGION: &str = "GB";

/// The zone a reader who has stated none is in.
const DEFAULT_ZONE: &str = "Europe/London";

/// A vendored input and the SHA-256 it must hash to.
struct Pinned {
    path: &'static str,
    bytes: &'static [u8],
    sha256: &'static str,
}

const TZDATA: Pinned = Pinned {
    path: "data/tzdb/tzdata.zi",
    bytes: include_bytes!("../data/tzdb/tzdata.zi"),
    sha256: "a84099714ecad44d6316b2434f6fa507020fbd77590952d6e08388879a7d5bfd",
};

const ZONE_TAB: Pinned = Pinned {
    path: "data/tzdb/zone.tab",
    bytes: include_bytes!("../data/tzdb/zone.tab"),
    sha256: "e2e8e31c4e10f44576c79345e76060591dbb3eef6ce14e006afc8a373caa9593",
};

const REGIONS: Pinned = Pinned {
    path: "data/cldr/regions.tsv",
    bytes: include_bytes!("../data/cldr/regions.tsv"),
    sha256: "0e3b5bfbc001d790cf6b35c8c42956549f475ba48f135cf50bf1b17c8b3d1719",
};

/// Why the store could not be compiled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    /// A vendored input does not hash to its pin.
    Pin(&'static str),
    /// A vendored input is not UTF-8, or the rules state no release.
    Text(&'static str),
    /// A line of an input is malformed.
    Syntax {
        /// The input.
        path: &'static str,
        /// The 1-based line.
        line: usize,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A zone names no rule set and no saving.
    Saving {
        /// The zone.
        zone: String,
        /// What it names.
        saving: String,
    },
    /// A zone without rules formats its abbreviation with `%s`.
    RulelessLetters(String),
    /// A zone could not be resolved.
    Zone {
        /// The zone.
        zone: String,
        /// Why.
        error: ZoneError,
    },
    /// A link is part of a cycle.
    LinkCycle(String),
    /// A zone's offset outgrows the store's fields.
    Range(String),
    /// The store refused what it was built from.
    Build(BuildError),
    /// A zone's continuing rules disagree with its last transition.
    Continues(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pin(path) => write!(f, "{path} does not hash to its pinned SHA-256"),
            Self::Text(path) => write!(f, "{path} is not the text it should be"),
            Self::Syntax { path, line, reason } => write!(f, "{path}:{line}: {reason}"),
            Self::Saving { zone, saving } => {
                write!(f, "{zone}: {saving:?} names no rule set and no saving")
            }
            Self::RulelessLetters(zone) => write!(f, "{zone}: %s in a ruleless era"),
            Self::Zone { zone, error } => write!(f, "{zone}: {error:?}"),
            Self::LinkCycle(name) => write!(f, "link {name} is part of a cycle"),
            Self::Range(zone) => write!(f, "{zone}: an offset outgrows the store"),
            Self::Build(error) => write!(f, "the store refuses its input: {error:?}"),
            Self::Continues(zone) => {
                write!(f, "{zone}: its rules disagree with its last transition")
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Everything the store is built from, resolved.
///
/// # Errors
///
/// A [`CompileError`] for an input that fails its pin or that `zic` would
/// refuse.
pub fn store_input() -> Result<StoreInput, CompileError> {
    let rules = text(&TZDATA)?;
    let release = rules
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# version "))
        .ok_or(CompileError::Text(TZDATA.path))?;
    let source = source::parse(rules).map_err(|error| CompileError::Syntax {
        path: TZDATA.path,
        line: error.line,
        reason: error.reason,
    })?;
    let places = tables::places(text(&ZONE_TAB)?).map_err(|error| CompileError::Syntax {
        path: ZONE_TAB.path,
        line: error.line,
        reason: error.reason,
    })?;
    let regions = tables::regions(text(&REGIONS)?).map_err(|error| CompileError::Syntax {
        path: REGIONS.path,
        line: error.line,
        reason: error.reason,
    })?;
    let book = rule_book(&source.rules);
    let zones = source
        .zones
        .iter()
        .map(|zone| compile_zone(zone, &book))
        .collect::<Result<_, _>>()?;
    Ok(StoreInput {
        release: String::from(release),
        zones,
        aliases: aliases(&source)?,
        regions,
        places,
        default_zone: String::from(DEFAULT_ZONE),
        default_region: String::from(DEFAULT_REGION),
    })
}

/// The store.
///
/// # Errors
///
/// As [`store_input`], or a [`CompileError::Build`] for input the store
/// refuses, or a [`CompileError::Continues`] for a zone the store would read
/// differently just after its last transition than at it.
pub fn compile() -> Result<Vec<u8>, CompileError> {
    let input = store_input()?;
    let store = build(&input).map_err(CompileError::Build)?;
    check_continuations(&input, &store)?;
    Ok(store)
}

/// `pinned`'s text, once its hash is its pin.
fn text(pinned: &Pinned) -> Result<&'static str, CompileError> {
    let mut expected = [0; SHA256_OUTPUT_LEN];
    if !hex::decode_into(pinned.sha256, &mut expected) || sha256(pinned.bytes) != expected {
        return Err(CompileError::Pin(pinned.path));
    }
    core::str::from_utf8(pinned.bytes).map_err(|_| CompileError::Text(pinned.path))
}

/// Every rule set by name.
fn rule_book(rules: &[Rule]) -> BTreeMap<&str, Vec<Rule>> {
    let mut book: BTreeMap<&str, Vec<Rule>> = BTreeMap::new();
    for rule in rules {
        book.entry(rule.name.as_str())
            .or_default()
            .push(rule.clone());
    }
    book
}

/// `zic`'s `associate`: each of `zone`'s eras with the rule set it names,
/// or the fixed saving it states instead.
fn plans<'a>(
    zone: &'a Zone,
    book: &'a BTreeMap<&str, Vec<Rule>>,
) -> Result<Vec<Plan<'a>>, CompileError> {
    zone.eras
        .iter()
        .map(|era| {
            if let Some(rules) = book.get(era.saving.as_str()) {
                return Ok(Plan {
                    era,
                    saving: Saving::Rules(rules),
                });
            }
            let (save, daylight) =
                source::saving(&era.saving).ok_or_else(|| CompileError::Saving {
                    zone: zone.name.clone(),
                    saving: era.saving.clone(),
                })?;
            if era.format.contains("%s") {
                return Err(CompileError::RulelessLetters(zone.name.clone()));
            }
            Ok(Plan {
                era,
                saving: Saving::Fixed { save, daylight },
            })
        })
        .collect()
}

/// One zone resolved into what the store holds of it.
fn compile_zone(zone: &Zone, book: &BTreeMap<&str, Vec<Rule>>) -> Result<ZoneInput, CompileError> {
    let plans = plans(zone, book)?;
    let resolved = outzone::resolve(&plans).map_err(|error| CompileError::Zone {
        zone: zone.name.clone(),
        error,
    })?;
    let range = || CompileError::Range(zone.name.clone());
    let offset = |seconds: i64| i32::try_from(seconds).map_err(|_| range());
    let transition = |at: i64, kind: &Type| -> Result<TransitionInput, CompileError> {
        Ok(TransitionInput {
            at,
            offset: offset(kind.offset)?,
            abbreviation: kind.abbreviation.clone(),
            daylight: kind.daylight,
        })
    };
    let phase = |phase: &Phase| -> Result<PhaseInput, CompileError> {
        Ok(PhaseInput {
            save: offset(phase.save)?,
            abbreviation: phase.abbreviation.clone(),
            moment: phase.moment,
        })
    };
    let mut transitions = Vec::with_capacity(resolved.transitions.len() + 1);
    transitions.push(transition(i64::MIN, &resolved.initial)?);
    for (at, kind) in &resolved.transitions {
        transitions.push(transition(*at, kind)?);
    }
    let trailing = match &resolved.future {
        Future::Alternating {
            stdoff,
            daylight,
            standard,
        } => Some(TrailingInput {
            stdoff: offset(*stdoff)?,
            daylight: phase(daylight)?,
            standard: phase(standard)?,
        }),
        Future::Unknown | Future::Standard(_) | Future::Daylight(_) => None,
    };
    Ok(ZoneInput {
        name: zone.name.clone(),
        transitions,
        trailing,
    })
}

/// Each link and the zone it finally names, a link to a link followed.
fn aliases(source: &Source) -> Result<Vec<(String, String)>, CompileError> {
    let targets: BTreeMap<&str, &str> = source
        .links
        .iter()
        .map(|link| (link.name.as_str(), link.target.as_str()))
        .collect();
    source
        .links
        .iter()
        .map(|link| {
            let mut target = link.target.as_str();
            for _ in 0..targets.len() {
                match targets.get(target) {
                    Some(next) => target = next,
                    None => return Ok((link.name.clone(), String::from(target))),
                }
            }
            Err(CompileError::LinkCycle(link.name.clone()))
        })
        .collect()
}

/// Every zone whose rules continue it reads, just after its last transition,
/// what that transition set.
fn check_continuations(input: &StoreInput, store: &[u8]) -> Result<(), CompileError> {
    let info =
        ZoneInfo::parse(store).map_err(|error| CompileError::Build(BuildError::Refused(error)))?;
    for zone in input.zones.iter().filter(|zone| zone.trailing.is_some()) {
        let continues = zone.transitions.last().is_some_and(|last| {
            info.zone(&zone.name).is_some_and(|stored| {
                let next = stored.offset_at(Time64::from_secs(last.at.saturating_add(1)));
                (next.utc_offset, next.abbreviation, next.daylight)
                    == (last.offset, last.abbreviation.as_str(), last.daylight)
            })
        });
        if !continues {
            return Err(CompileError::Continues(zone.name.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
