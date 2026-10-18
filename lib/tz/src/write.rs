//! Assembling a store from resolved zones: what `tools/tzcompile` writes, and
//! what this crate's tests build their fixtures with, so the format has one
//! writer as it has one reader.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::layout::{
    seal, Header, NameRecord, PhaseRecord, PlaceRecord, RegionRecord, TrailingRecord,
    TransitionRecord, TypeRecord, ZoneRecord, HEADER_LEN, NO_TRAILING,
};
use crate::rule::Moment;
use crate::store::{StoreError, ZoneInfo};

/// One transition of a zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionInput {
    /// The UT second the offset begins; a zone's first is `i64::MIN`.
    pub at: i64,
    /// Seconds east of UT.
    pub offset: i32,
    /// What the zone calls it.
    pub abbreviation: String,
    /// Whether daylight saving is in force.
    pub daylight: bool,
}

/// One phase of a trailing rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseInput {
    /// Seconds added to standard time.
    pub save: i32,
    /// What the zone calls it.
    pub abbreviation: String,
    /// When in each year it begins.
    pub moment: Moment,
}

/// The rules a zone keeps changing between after its last transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrailingInput {
    /// The standard offset, seconds east of UT.
    pub stdoff: i32,
    /// The change into daylight saving.
    pub daylight: PhaseInput,
    /// The change back.
    pub standard: PhaseInput,
}

/// One canonical zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneInput {
    /// Its canonical name.
    pub name: String,
    /// Its transitions, ascending.
    pub transitions: Vec<TransitionInput>,
    /// The rules that continue it, if its offset keeps changing.
    pub trailing: Option<TrailingInput>,
}

/// One region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionInput {
    /// Its ISO 3166-1 alpha-2 code.
    pub code: String,
    /// Its English name.
    pub name: String,
    /// Its numeric date pattern.
    pub date: String,
    /// Its time-of-day pattern.
    pub time: String,
}

/// One place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceInput {
    /// The zone, by any of its names.
    pub zone: String,
    /// The region's code.
    pub region: String,
    /// What tells the region's zones apart.
    pub comment: String,
}

/// Everything a store holds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoreInput {
    /// The IANA release.
    pub release: String,
    /// The canonical zones.
    pub zones: Vec<ZoneInput>,
    /// Each alias and the canonical zone it names.
    pub aliases: Vec<(String, String)>,
    /// The regions.
    pub regions: Vec<RegionInput>,
    /// The places, in the order a list of them reads.
    pub places: Vec<PlaceInput>,
    /// The zone a reader who states none is in.
    pub default_zone: String,
    /// The region a reader who states none is in.
    pub default_region: String,
}

/// Why a store could not be built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuildError {
    /// Two zones or aliases claim the same name.
    Duplicate(String),
    /// A name refers to no zone.
    Unknown(String),
    /// The input outgrows the format's fields or bounds.
    TooLarge,
    /// A string holds the NUL its section ends each string with.
    Nul(String),
    /// The assembled store fails the reader's validation.
    Refused(StoreError),
}

/// The store `input` describes.
///
/// # Errors
///
/// [`BuildError`] for input no store can hold; whatever is returned has
/// passed [`ZoneInfo::parse`].
pub fn build(input: &StoreInput) -> Result<Vec<u8>, BuildError> {
    let mut zones: Vec<&ZoneInput> = input.zones.iter().collect();
    zones.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let names = name_table(&zones, &input.aliases)?;
    let mut assembly = Assembly::default();
    let release = assembly.strings.intern(&input.release)?;
    assembly.names(&names)?;
    assembly.zones(&zones)?;
    let mut regions: Vec<&RegionInput> = input.regions.iter().collect();
    regions.sort_by(|a, b| a.code.cmp(&b.code));
    assembly.regions(&regions)?;
    assembly.places(&input.places, &names)?;
    let count = |items: usize| u32::try_from(items).map_err(|_| BuildError::TooLarge);
    let body = assembly.body();
    let header = Header {
        release,
        zone_count: count(zones.len())?,
        name_count: count(names.len())?,
        transition_count: assembly.transitions,
        type_count: count(assembly.types.count)?,
        abbrev_count: count(assembly.abbrevs.count)?,
        trailing_count: assembly.trailing,
        region_count: count(regions.len())?,
        place_count: count(input.places.len())?,
        strings_len: count(assembly.strings.bytes.len())?,
        default_name: name_index(&names, &input.default_zone)?,
        default_region: code(&input.default_region)?,
        checksum: 0,
    };
    let mut store = Vec::with_capacity(HEADER_LEN + body.len());
    store.extend_from_slice(&header.encode());
    store.extend_from_slice(&body);
    if !seal(&mut store) {
        return Err(BuildError::TooLarge);
    }
    ZoneInfo::parse(&store).map_err(BuildError::Refused)?;
    Ok(store)
}

/// Every name — each zone's own, then each alias — with the index of the zone
/// it names, sorted as the store's name table is.
fn name_table<'a>(
    zones: &[&'a ZoneInput],
    aliases: &'a [(String, String)],
) -> Result<Vec<(&'a str, u32)>, BuildError> {
    let mut names: Vec<(&str, u32)> = Vec::with_capacity(zones.len() + aliases.len());
    for (index, zone) in zones.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| BuildError::TooLarge)?;
        names.push((zone.name.as_str(), index));
    }
    names.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if let Some(pair) = names.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(BuildError::Duplicate(String::from(pair[0].0)));
    }
    let canonical = names.clone();
    for (alias, target) in aliases {
        names.push((alias.as_str(), zone_named(&canonical, target)?));
    }
    names.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if let Some(pair) = names.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(BuildError::Duplicate(String::from(pair[0].0)));
    }
    Ok(names)
}

/// The zone `name` names in the sorted `names`.
fn zone_named(names: &[(&str, u32)], name: &str) -> Result<u32, BuildError> {
    names
        .binary_search_by(|(candidate, _)| candidate.as_bytes().cmp(name.as_bytes()))
        .map(|found| names[found].1)
        .map_err(|_| BuildError::Unknown(String::from(name)))
}

/// Where `name` stands in the sorted `names`.
fn name_index(names: &[(&str, u32)], name: &str) -> Result<u32, BuildError> {
    let found = names
        .binary_search_by(|(candidate, _)| candidate.as_bytes().cmp(name.as_bytes()))
        .map_err(|_| BuildError::Unknown(String::from(name)))?;
    u32::try_from(found).map_err(|_| BuildError::TooLarge)
}

/// The sections as they are written.
#[derive(Default)]
struct Assembly {
    strings: Strings,
    abbrevs: Abbrevs,
    types: Types,
    names: Vec<u8>,
    zones: Vec<u8>,
    transition_records: Vec<u8>,
    trailing_records: Vec<u8>,
    regions: Vec<u8>,
    places: Vec<u8>,
    transitions: u32,
    trailing: u32,
}

impl Assembly {
    fn names(&mut self, names: &[(&str, u32)]) -> Result<(), BuildError> {
        for (name, zone) in names {
            let record = NameRecord {
                name: self.strings.intern(name)?,
                zone: *zone,
            };
            self.names.extend_from_slice(&record.encode());
        }
        Ok(())
    }

    fn zones(&mut self, zones: &[&ZoneInput]) -> Result<(), BuildError> {
        for zone in zones {
            let count = u32::try_from(zone.transitions.len()).map_err(|_| BuildError::TooLarge)?;
            let trailing = match &zone.trailing {
                None => NO_TRAILING,
                Some(rule) => {
                    let record = TrailingRecord {
                        stdoff: rule.stdoff,
                        daylight: self.phase(&rule.daylight)?,
                        standard: self.phase(&rule.standard)?,
                    };
                    self.trailing_records.extend_from_slice(&record.encode());
                    self.trailing += 1;
                    self.trailing - 1
                }
            };
            let record = ZoneRecord {
                name: self.strings.intern(&zone.name)?,
                first: self.transitions,
                count,
                trailing,
            };
            self.zones.extend_from_slice(&record.encode());
            for transition in &zone.transitions {
                let kind = TypeRecord {
                    offset: transition.offset,
                    abbrev: self
                        .abbrevs
                        .index(&transition.abbreviation, &mut self.strings)?,
                    daylight: transition.daylight,
                };
                let record = TransitionRecord {
                    at: transition.at,
                    kind: self.types.index(kind)?,
                };
                self.transition_records.extend_from_slice(&record.encode());
            }
            self.transitions = self
                .transitions
                .checked_add(count)
                .ok_or(BuildError::TooLarge)?;
        }
        Ok(())
    }

    fn phase(&mut self, input: &PhaseInput) -> Result<PhaseRecord, BuildError> {
        Ok(PhaseRecord {
            save: input.save,
            abbrev: self.abbrevs.index(&input.abbreviation, &mut self.strings)?,
            moment: input.moment,
        })
    }

    fn regions(&mut self, regions: &[&RegionInput]) -> Result<(), BuildError> {
        for region in regions {
            let record = RegionRecord {
                code: code(&region.code)?,
                name: self.strings.intern(&region.name)?,
                date: self.strings.intern(&region.date)?,
                time: self.strings.intern(&region.time)?,
            };
            self.regions.extend_from_slice(&record.encode());
        }
        Ok(())
    }

    fn places(&mut self, places: &[PlaceInput], names: &[(&str, u32)]) -> Result<(), BuildError> {
        for place in places {
            let record = PlaceRecord {
                name: name_index(names, &place.zone)?,
                region: code(&place.region)?,
                comment: self.strings.intern(&place.comment)?,
            };
            self.places.extend_from_slice(&record.encode());
        }
        Ok(())
    }

    /// Every section after the header, in the store's order.
    fn body(&self) -> Vec<u8> {
        [
            self.names.as_slice(),
            &self.zones,
            &self.transition_records,
            &self.types.records,
            &self.abbrevs.offsets,
            &self.trailing_records,
            &self.regions,
            &self.places,
            &self.strings.bytes,
        ]
        .concat()
    }
}

/// `text` as a region code's two bytes.
fn code(text: &str) -> Result<[u8; 2], BuildError> {
    let bytes = text.as_bytes();
    match bytes {
        [first, second] => Ok([*first, *second]),
        _ => Err(BuildError::Unknown(String::from(text))),
    }
}

/// The string section, each distinct string once.
#[derive(Default)]
struct Strings {
    bytes: Vec<u8>,
    at: BTreeMap<String, u32>,
}

impl Strings {
    fn intern(&mut self, text: &str) -> Result<u32, BuildError> {
        if text.contains('\0') {
            return Err(BuildError::Nul(String::from(text)));
        }
        if let Some(offset) = self.at.get(text) {
            return Ok(*offset);
        }
        let offset = u32::try_from(self.bytes.len()).map_err(|_| BuildError::TooLarge)?;
        self.bytes.extend_from_slice(text.as_bytes());
        self.bytes.push(0);
        self.at.insert(String::from(text), offset);
        Ok(offset)
    }
}

/// The type table, each distinct type once.
#[derive(Default)]
struct Types {
    records: Vec<u8>,
    index: BTreeMap<(i32, u16, bool), u16>,
    count: usize,
}

impl Types {
    fn index(&mut self, kind: TypeRecord) -> Result<u16, BuildError> {
        let key = (kind.offset, kind.abbrev, kind.daylight);
        if let Some(index) = self.index.get(&key) {
            return Ok(*index);
        }
        let index = u16::try_from(self.count).map_err(|_| BuildError::TooLarge)?;
        self.records.extend_from_slice(&kind.encode());
        self.index.insert(key, index);
        self.count += 1;
        Ok(index)
    }
}

/// The abbreviation table, each distinct abbreviation once.
#[derive(Default)]
struct Abbrevs {
    offsets: Vec<u8>,
    index: BTreeMap<String, u16>,
    count: usize,
}

impl Abbrevs {
    fn index(&mut self, text: &str, strings: &mut Strings) -> Result<u16, BuildError> {
        if let Some(index) = self.index.get(text) {
            return Ok(*index);
        }
        let index = u16::try_from(self.count).map_err(|_| BuildError::TooLarge)?;
        self.offsets
            .extend_from_slice(&strings.intern(text)?.to_le_bytes());
        self.index.insert(String::from(text), index);
        self.count += 1;
        Ok(index)
    }
}
