//! The compiled zone store: validated whole on parse, then read with no
//! allocation and no check that can fail.

use core::cmp::Ordering;
use core::fmt::{self, Write};

use crate::layout::{
    record, store_checksum, Header, NameRecord, PlaceRecord, RegionRecord, Sections,
    TrailingRecord, TransitionRecord, TypeRecord, ZoneRecord, ABBREV_LEN, MAX_ABBREV, MAX_COMMENT,
    MAX_OFFSET, MAX_REGION_NAME, MAX_RELEASE, MAX_RULE_TIME, MAX_STORE_LEN, NAME_LEN, NO_TRAILING,
    PLACE_LEN, REGION_LEN, TRAILING_LEN, TRANSITION_LEN, TYPE_LEN, ZONE_LEN,
};
use tairix_util::utf8::is_hidden;

use crate::pattern::Pattern;
use crate::zone::Zone;
use crate::Weekday;

/// Why a store was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    /// The header is short, foreign, of another version, or sets a reserved
    /// field.
    Header,
    /// The sections do not exactly fill the store, or the store is past its
    /// bound.
    Length,
    /// The checksum does not match the body.
    Checksum,
    /// A string is unterminated, not UTF-8, of the wrong shape, or past its
    /// bound.
    String,
    /// An index or offset points outside what it indexes.
    Index,
    /// A table is out of order or names something twice.
    Order,
    /// A record holds a value outside its domain.
    Value,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Header => "its header is not a zone store's",
            Self::Length => "its length does not match its sections",
            Self::Checksum => "its checksum does not match its contents",
            Self::String => "it holds a malformed string",
            Self::Index => "it points outside a table",
            Self::Order => "a table is out of order",
            Self::Value => "it holds a value out of range",
        })
    }
}

/// A validated zone store, borrowing its bytes.
#[derive(Copy, Clone)]
pub struct ZoneInfo<'a> {
    bytes: &'a [u8],
    header: Header,
    sections: Sections,
}

/// A store's validated layout without its bytes ([`ZoneInfo::validated`]).
#[cfg(feature = "rt")]
#[derive(Copy, Clone, Debug)]
pub(crate) struct Validated {
    header: Header,
    sections: Sections,
    len: usize,
}

/// The release and the table sizes, rather than every byte of the store.
impl fmt::Debug for ZoneInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZoneInfo")
            .field("release", &self.release())
            .field("zones", &self.header.zone_count)
            .field("names", &self.header.name_count)
            .field("regions", &self.header.region_count)
            .field("places", &self.header.place_count)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Two stores are equal when they hold the same bytes; a store compared with
/// itself costs a pointer comparison.
impl PartialEq for ZoneInfo<'_> {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.bytes, other.bytes) || self.bytes == other.bytes
    }
}

impl Eq for ZoneInfo<'_> {}

/// One region of the world and its conventions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Region<'a> {
    /// Its ISO 3166-1 alpha-2 code.
    pub code: &'a str,
    /// Its English name.
    pub name: &'a str,
    /// The day its calendar week begins on.
    pub first_weekday: Weekday,
    /// Its numeric date.
    pub date: Pattern<'a>,
    /// Its time of day.
    pub time: Pattern<'a>,
}

/// One zone a region's people keep, as the IANA zone table lists it.
#[derive(Copy, Clone, Debug)]
pub struct Place<'a> {
    /// The zone.
    pub zone: Zone<'a>,
    /// The region's code.
    pub region: &'a str,
    /// What tells the region's zones apart; empty for a region of one zone.
    pub comment: &'a str,
}

impl Place<'_> {
    /// Write the place's city: the last part of the name the table lists it
    /// by, each `_` read as a space.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write_city(&self, out: &mut dyn Write) -> fmt::Result {
        let name = self.zone.name();
        let city = name.rsplit('/').next().unwrap_or(name);
        for (index, word) in city.split('_').enumerate() {
            if index > 0 {
                out.write_char(' ')?;
            }
            out.write_str(word)?;
        }
        Ok(())
    }

    /// Write the place as a reader names it: its city, then the name of the
    /// region `info` holds it under.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write_label(&self, info: &ZoneInfo<'_>, out: &mut dyn Write) -> fmt::Result {
        self.write_city(out)?;
        match info.region(self.region) {
            Some(region) => write!(out, ", {}", region.name),
            None => Ok(()),
        }
    }
}

pub use tairix_abi::zone::is_zone_name;

/// Whether `text` is an ISO 3166-1 alpha-2 code's shape.
#[must_use]
pub fn is_region_code(text: &str) -> bool {
    text.len() == 2 && text.bytes().all(|byte| byte.is_ascii_uppercase())
}

impl<'a> ZoneInfo<'a> {
    /// `bytes` as a store, validated whole: its layout, checksum, every string
    /// it references and every index, the order of every table, and every
    /// value's domain.
    ///
    /// # Errors
    ///
    /// The first rule the store breaks; nothing of a refused store is read.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, StoreError> {
        if bytes.len() > MAX_STORE_LEN {
            return Err(StoreError::Length);
        }
        let header = Header::decode(bytes).ok_or(StoreError::Header)?;
        let sections = Sections::of(&header).ok_or(StoreError::Length)?;
        if sections.end != bytes.len() {
            return Err(StoreError::Length);
        }
        if store_checksum(bytes) != Some(header.checksum) {
            return Err(StoreError::Checksum);
        }
        let info = Self {
            bytes,
            header,
            sections,
        };
        info.validate()?;
        Ok(info)
    }

    /// What validation found of this store, held apart from its bytes so a
    /// reader can move them without validating them again.
    #[cfg(feature = "rt")]
    pub(crate) const fn validated(&self) -> Validated {
        Validated {
            header: self.header,
            sections: self.sections,
            len: self.bytes.len(),
        }
    }

    /// The store `validated` describes, over `bytes`: the very bytes it was
    /// parsed from, moved. `None` for bytes of another length.
    #[cfg(feature = "rt")]
    pub(crate) fn from_validated(validated: Validated, bytes: &'a [u8]) -> Option<Self> {
        (bytes.len() == validated.len).then_some(Self {
            bytes,
            header: validated.header,
            sections: validated.sections,
        })
    }

    fn validate(&self) -> Result<(), StoreError> {
        let h = &self.header;
        let release = self.string(h.release).ok_or(StoreError::String)?;
        if release.is_empty()
            || release.len() > MAX_RELEASE
            || !release.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(StoreError::String);
        }
        if h.zone_count == 0
            || h.name_count < h.zone_count
            || h.transition_count < h.zone_count
            || h.type_count == 0
            || h.type_count > u32::from(u16::MAX) + 1
            || h.abbrev_count > u32::from(u16::MAX) + 1
            || h.region_count == 0
            || h.default_name >= h.name_count
        {
            return Err(StoreError::Value);
        }
        self.validate_abbrevs()?;
        self.validate_types()?;
        self.validate_names()?;
        self.validate_zones()?;
        self.validate_trailing()?;
        self.validate_regions()?;
        self.validate_places()?;
        let default = core::str::from_utf8(&h.default_region).map_err(|_| StoreError::Value)?;
        self.region(default).map(|_| ()).ok_or(StoreError::Index)
    }

    fn validate_abbrevs(&self) -> Result<(), StoreError> {
        for index in 0..self.header.abbrev_count {
            let offset = self.abbrev_offset(index).ok_or(StoreError::Length)?;
            let text = self.string(offset).ok_or(StoreError::String)?;
            if text.is_empty()
                || text.len() > MAX_ABBREV
                || !text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-'))
            {
                return Err(StoreError::String);
            }
        }
        Ok(())
    }

    fn validate_types(&self) -> Result<(), StoreError> {
        for index in 0..self.header.type_count {
            let kind = self.type_record(index).ok_or(StoreError::Value)?;
            if kind.offset.unsigned_abs() > MAX_OFFSET.unsigned_abs()
                || u32::from(kind.abbrev) >= self.header.abbrev_count
            {
                return Err(StoreError::Value);
            }
        }
        Ok(())
    }

    fn validate_names(&self) -> Result<(), StoreError> {
        let mut previous: Option<&str> = None;
        for index in 0..self.header.name_count {
            let entry = self.name_record(index).ok_or(StoreError::Length)?;
            let name = self.string(entry.name).ok_or(StoreError::String)?;
            if !is_zone_name(name) {
                return Err(StoreError::String);
            }
            if entry.zone >= self.header.zone_count {
                return Err(StoreError::Index);
            }
            if previous.is_some_and(|earlier| earlier.as_bytes() >= name.as_bytes()) {
                return Err(StoreError::Order);
            }
            previous = Some(name);
        }
        Ok(())
    }

    fn validate_zones(&self) -> Result<(), StoreError> {
        let mut next_first = 0u32;
        let mut previous_name: Option<&str> = None;
        for index in 0..self.header.zone_count {
            let zone = self.zone_record(index).ok_or(StoreError::Length)?;
            let name = self.string(zone.name).ok_or(StoreError::String)?;
            if self.find(name).map(|entry| entry.zone) != Some(index) {
                return Err(StoreError::Index);
            }
            if previous_name.is_some_and(|earlier| earlier.as_bytes() >= name.as_bytes()) {
                return Err(StoreError::Order);
            }
            previous_name = Some(name);
            if zone.first != next_first || zone.count == 0 {
                return Err(StoreError::Order);
            }
            next_first = zone
                .first
                .checked_add(zone.count)
                .filter(|end| *end <= self.header.transition_count)
                .ok_or(StoreError::Index)?;
            if zone.trailing != NO_TRAILING && zone.trailing >= self.header.trailing_count {
                return Err(StoreError::Index);
            }
            let mut previous: Option<i64> = None;
            for at in zone.first..next_first {
                let transition = self.transition(at).ok_or(StoreError::Length)?;
                let ordered = match previous {
                    None => transition.at == i64::MIN,
                    Some(earlier) => earlier < transition.at,
                };
                if !ordered {
                    return Err(StoreError::Order);
                }
                if u32::from(transition.kind) >= self.header.type_count {
                    return Err(StoreError::Index);
                }
                previous = Some(transition.at);
            }
        }
        if next_first == self.header.transition_count {
            Ok(())
        } else {
            Err(StoreError::Length)
        }
    }

    fn validate_trailing(&self) -> Result<(), StoreError> {
        for index in 0..self.header.trailing_count {
            let rule = self.trailing(index).ok_or(StoreError::Value)?;
            let in_range =
                |value: i64, bound: i32| value.unsigned_abs() <= bound.unsigned_abs().into();
            // Each phase's whole offset is bounded as a type's is: a reader
            // searches only that far either side of a local time.
            let valid = in_range(i64::from(rule.stdoff), MAX_OFFSET)
                && [rule.daylight, rule.standard].iter().all(|phase| {
                    in_range(i64::from(rule.stdoff) + i64::from(phase.save), MAX_OFFSET)
                        && in_range(i64::from(phase.save), MAX_OFFSET)
                        && in_range(i64::from(phase.moment.at), MAX_RULE_TIME)
                        && phase.moment.is_well_formed()
                        && u32::from(phase.abbrev) < self.header.abbrev_count
                });
            if !valid {
                return Err(StoreError::Value);
            }
        }
        Ok(())
    }

    fn validate_regions(&self) -> Result<(), StoreError> {
        let mut previous: Option<[u8; 2]> = None;
        for index in 0..self.header.region_count {
            let region = self.region_record(index).ok_or(StoreError::Value)?;
            let code = core::str::from_utf8(&region.code).map_err(|_| StoreError::Value)?;
            if !is_region_code(code) {
                return Err(StoreError::Value);
            }
            if previous.is_some_and(|earlier| earlier >= region.code) {
                return Err(StoreError::Order);
            }
            previous = Some(region.code);
            let name = self.string(region.name).ok_or(StoreError::String)?;
            if name.is_empty() || name.len() > MAX_REGION_NAME || name.chars().any(is_hidden) {
                return Err(StoreError::String);
            }
            let date = self.string(region.date).ok_or(StoreError::String)?;
            let time = self.string(region.time).ok_or(StoreError::String)?;
            if Pattern::date(date).is_none() || Pattern::time(time).is_none() {
                return Err(StoreError::String);
            }
        }
        Ok(())
    }

    fn validate_places(&self) -> Result<(), StoreError> {
        for index in 0..self.header.place_count {
            let place = self.place_record(index).ok_or(StoreError::Value)?;
            if place.name >= self.header.name_count {
                return Err(StoreError::Index);
            }
            let code = core::str::from_utf8(&place.region).map_err(|_| StoreError::Value)?;
            if self.region(code).is_none() {
                return Err(StoreError::Index);
            }
            let comment = self.string(place.comment).ok_or(StoreError::String)?;
            if comment.len() > MAX_COMMENT || comment.chars().any(is_hidden) {
                return Err(StoreError::String);
            }
        }
        Ok(())
    }

    /// The IANA release the store was compiled from.
    #[must_use]
    pub fn release(&self) -> &'a str {
        self.string(self.header.release).unwrap_or_default()
    }

    /// The zone named `name`, canonically or by an alias; it keeps the name
    /// it was found by.
    #[must_use]
    pub fn zone(&'a self, name: &str) -> Option<Zone<'a>> {
        let entry = self.find(name)?;
        Some(Zone::stored(self, entry.zone, self.string(entry.name)?))
    }

    /// Every canonical zone, in the order their names sort.
    pub fn zones(&'a self) -> impl Iterator<Item = Zone<'a>> + 'a {
        (0..self.header.zone_count)
            .map(move |index| Zone::stored(self, index, self.zone_name(index)))
    }

    /// The zone a reader who has stated none is in.
    #[must_use]
    pub fn default_zone(&'a self) -> Zone<'a> {
        self.named(self.header.default_name).unwrap_or(Zone::UTC)
    }

    /// The region coded `code`.
    #[must_use]
    pub fn region(&self, code: &str) -> Option<Region<'a>> {
        self.region_at(self.region_index(code.as_bytes())?)
    }

    /// Every region, in code order.
    pub fn regions(&self) -> impl Iterator<Item = Region<'a>> + '_ {
        (0..self.header.region_count).filter_map(move |index| self.region_at(index))
    }

    /// The region a reader who has stated none is in.
    #[must_use]
    pub fn default_region(&self) -> Option<Region<'a>> {
        let code = core::str::from_utf8(&self.header.default_region).ok()?;
        self.region(code)
    }

    /// The place the zone table lists zone `name` as, by that name; `None`
    /// for a zone it lists nowhere, such as a link or `Etc/UTC`.
    #[must_use]
    pub fn place(&'a self, name: &str) -> Option<Place<'a>> {
        self.places().find(|place| place.zone.name() == name)
    }

    /// Every place, as the IANA zone table lists them.
    pub fn places(&'a self) -> impl Iterator<Item = Place<'a>> + 'a {
        (0..self.header.place_count).filter_map(move |index| {
            let place = self.place_record(index)?;
            Some(Place {
                zone: self.named(place.name)?,
                region: self.stored_code(self.region_index(&place.region)?)?,
                comment: self.string(place.comment)?,
            })
        })
    }

    /// The zone name `index` of the name table names, by that name.
    fn named(&'a self, index: u32) -> Option<Zone<'a>> {
        let entry = self.name_record(index)?;
        Some(Zone::stored(self, entry.zone, self.string(entry.name)?))
    }

    /// The canonical name of zone `index`.
    fn zone_name(&self, index: u32) -> &'a str {
        self.zone_record(index)
            .and_then(|zone| self.string(zone.name))
            .unwrap_or_default()
    }

    /// The record of zone `index`.
    pub(crate) fn zone_record(&self, index: u32) -> Option<ZoneRecord> {
        let at = position(self.sections.zones, index, ZONE_LEN)?;
        record::<ZONE_LEN>(self.bytes, at).map(ZoneRecord::decode)
    }

    /// Transition `index` of the whole table.
    pub(crate) fn transition(&self, index: u32) -> Option<TransitionRecord> {
        let at = position(self.sections.transitions, index, TRANSITION_LEN)?;
        record::<TRANSITION_LEN>(self.bytes, at).map(TransitionRecord::decode)
    }

    /// Type `index`.
    pub(crate) fn type_record(&self, index: u32) -> Option<TypeRecord> {
        let at = position(self.sections.types, index, TYPE_LEN)?;
        record::<TYPE_LEN>(self.bytes, at).and_then(TypeRecord::decode)
    }

    /// Abbreviation `index`.
    pub(crate) fn abbrev(&self, index: u16) -> &'a str {
        self.abbrev_offset(u32::from(index))
            .and_then(|offset| self.string(offset))
            .unwrap_or_default()
    }

    /// Trailing rule `index`.
    pub(crate) fn trailing(&self, index: u32) -> Option<TrailingRecord> {
        let at = position(self.sections.trailing, index, TRAILING_LEN)?;
        record::<TRAILING_LEN>(self.bytes, at).and_then(TrailingRecord::decode)
    }

    /// The name record spelling `name`.
    fn find(&self, name: &str) -> Option<NameRecord> {
        let key = name.as_bytes();
        let (mut low, mut high) = (0, self.header.name_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let entry = self.name_record(middle)?;
            match self.string(entry.name)?.as_bytes().cmp(key) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Some(entry),
            }
        }
        None
    }

    fn region_at(&self, index: u32) -> Option<Region<'a>> {
        let region = self.region_record(index)?;
        Some(Region {
            code: self.stored_code(index)?,
            name: self.string(region.name)?,
            first_weekday: region.first_weekday,
            date: Pattern::date(self.string(region.date)?)?,
            time: Pattern::time(self.string(region.time)?)?,
        })
    }

    /// The index of the region coded `code`, by binary search of the region
    /// table, which validation has sorted by code.
    fn region_index(&self, code: &[u8]) -> Option<u32> {
        let (mut low, mut high) = (0, self.header.region_count);
        while low < high {
            let middle = low + (high - low) / 2;
            match self.stored_code(middle)?.as_bytes().cmp(code) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Some(middle),
            }
        }
        None
    }

    /// The code of region `index`, as text borrowed from the store.
    fn stored_code(&self, index: u32) -> Option<&'a str> {
        let at = position(self.sections.regions, index, REGION_LEN)?;
        core::str::from_utf8(self.bytes.get(at..at.checked_add(2)?)?).ok()
    }

    fn name_record(&self, index: u32) -> Option<NameRecord> {
        let at = position(self.sections.names, index, NAME_LEN)?;
        record::<NAME_LEN>(self.bytes, at).map(NameRecord::decode)
    }

    fn region_record(&self, index: u32) -> Option<RegionRecord> {
        let at = position(self.sections.regions, index, REGION_LEN)?;
        record::<REGION_LEN>(self.bytes, at).and_then(RegionRecord::decode)
    }

    fn place_record(&self, index: u32) -> Option<PlaceRecord> {
        let at = position(self.sections.places, index, PLACE_LEN)?;
        record::<PLACE_LEN>(self.bytes, at).and_then(PlaceRecord::decode)
    }

    fn abbrev_offset(&self, index: u32) -> Option<u32> {
        let at = position(self.sections.abbrevs, index, ABBREV_LEN)?;
        record::<ABBREV_LEN>(self.bytes, at).map(|bytes| u32::from_le_bytes(*bytes))
    }

    /// The NUL-terminated UTF-8 string at `offset` of the string section.
    fn string(&self, offset: u32) -> Option<&'a str> {
        let start = self
            .sections
            .strings
            .checked_add(usize::try_from(offset).ok()?)?;
        let tail = self.bytes.get(start..self.sections.end)?;
        let end = tail.iter().position(|byte| *byte == 0)?;
        core::str::from_utf8(tail.get(..end)?).ok()
    }
}

/// The byte offset of record `index` of a section starting at `start`.
fn position(start: usize, index: u32, len: usize) -> Option<usize> {
    start.checked_add(usize::try_from(index).ok()?.checked_mul(len)?)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
