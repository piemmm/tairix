//! The zone store's wire layout: the constants and fixed-width records the
//! compiler writes and [`ZoneInfo`](crate::ZoneInfo) reads, defined once.
//!
//! Every integer is little-endian and read with `from_le_bytes` over a slice,
//! so a store needs no alignment and reads alike on every target. The format
//! is specified in `docs/src/lib/tz.md`.

use crate::rule::{DayRule, Moment, TimeBase};
use crate::Weekday;

/// The first four bytes of every store.
pub const MAGIC: [u8; 4] = *b"TZX1";

/// The format version this reader understands.
pub const VERSION: u16 = 2;

/// Bytes in the header.
pub const HEADER_LEN: usize = 60;

/// Where the header's eleven count and offset words begin.
const WORDS_AT: usize = 8;

/// Where the header holds the default region's code.
const DEFAULT_REGION_AT: usize = 52;

/// Where the header holds the checksum: the one span of a store it does not
/// cover.
pub const CHECKSUM_AT: core::ops::Range<usize> = 56..60;

/// Bytes in one name record.
pub const NAME_LEN: usize = 8;

/// Bytes in one zone record.
pub const ZONE_LEN: usize = 16;

/// Bytes in one transition record.
pub const TRANSITION_LEN: usize = 10;

/// Bytes in one type record.
pub const TYPE_LEN: usize = 8;

/// Bytes in one abbreviation record.
pub const ABBREV_LEN: usize = 4;

/// Bytes in one trailing-rule record.
pub const TRAILING_LEN: usize = 36;

/// Bytes in one region record.
pub const REGION_LEN: usize = 16;

/// Bytes in one place record.
pub const PLACE_LEN: usize = 12;

/// A zone record's trailing index when its last offset holds for ever.
pub const NO_TRAILING: u32 = u32::MAX;

/// The largest store accepted. A security bound, not a capacity.
pub const MAX_STORE_LEN: usize = 4 << 20;

/// The longest abbreviation.
pub const MAX_ABBREV: usize = 16;

/// The longest region name.
pub const MAX_REGION_NAME: usize = 96;

/// The longest date or time pattern.
pub const MAX_PATTERN: usize = 16;

/// The longest place comment.
pub const MAX_COMMENT: usize = 128;

/// The longest release string.
pub const MAX_RELEASE: usize = 16;

/// The largest magnitude an offset or a saved amount may have, in seconds.
pub const MAX_OFFSET: i32 = 26 * 3600;

/// The largest magnitude a rule's time of day may have, in seconds: a rule
/// may name a time a few days either side of its date.
pub const MAX_RULE_TIME: i32 = 7 * 24 * 3600;

/// The store's header.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Header {
    /// Offset of the IANA release string.
    pub release: u32,
    /// Canonical zones.
    pub zone_count: u32,
    /// Names, canonical and alias.
    pub name_count: u32,
    /// Transitions, all zones.
    pub transition_count: u32,
    /// Distinct types: an offset, an abbreviation and a daylight flag.
    pub type_count: u32,
    /// Distinct abbreviations.
    pub abbrev_count: u32,
    /// Trailing rules.
    pub trailing_count: u32,
    /// Regions.
    pub region_count: u32,
    /// Places.
    pub place_count: u32,
    /// Bytes in the string section.
    pub strings_len: u32,
    /// Index of the name of the zone a reader with no preference is in.
    pub default_name: u32,
    /// The region a reader with no preference is in.
    pub default_region: [u8; 2],
    /// CRC-32C over every byte of the store but its own four.
    pub checksum: u32,
}

impl Header {
    /// The header's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0; HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        let words: [u32; 11] = [
            self.release,
            self.zone_count,
            self.name_count,
            self.transition_count,
            self.type_count,
            self.abbrev_count,
            self.trailing_count,
            self.region_count,
            self.place_count,
            self.strings_len,
            self.default_name,
        ];
        for (index, word) in words.iter().enumerate() {
            let at = WORDS_AT + index * 4;
            out[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        out[DEFAULT_REGION_AT..DEFAULT_REGION_AT + 2].copy_from_slice(&self.default_region);
        out[CHECKSUM_AT].copy_from_slice(&self.checksum.to_le_bytes());
        out
    }

    /// The header `bytes` open with, or `None` when it is short, carries the
    /// wrong magic or version, or sets a flag or a reserved byte.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let head = bytes.get(..HEADER_LEN)?;
        if head[0..4] != MAGIC || u16_at(head, 4) != VERSION || u16_at(head, 6) != 0 {
            return None;
        }
        if u16_at(head, DEFAULT_REGION_AT + 2) != 0 {
            return None;
        }
        let word = |index: usize| u32_at(head, WORDS_AT + index * 4);
        Some(Self {
            release: word(0),
            zone_count: word(1),
            name_count: word(2),
            transition_count: word(3),
            type_count: word(4),
            abbrev_count: word(5),
            trailing_count: word(6),
            region_count: word(7),
            place_count: word(8),
            strings_len: word(9),
            default_name: word(10),
            default_region: [head[DEFAULT_REGION_AT], head[DEFAULT_REGION_AT + 1]],
            checksum: u32_at(head, CHECKSUM_AT.start),
        })
    }
}

/// The checksum a store's header carries: CRC-32C over every byte of `bytes`
/// but its own four, so neither the header nor the body changes undetected.
/// `None` for bytes shorter than a header.
#[must_use]
pub fn store_checksum(bytes: &[u8]) -> Option<u32> {
    let before = bytes.get(..CHECKSUM_AT.start)?;
    let after = bytes.get(CHECKSUM_AT.end..)?;
    Some(!tairix_crc32c::update(
        tairix_crc32c::update(u32::MAX, before),
        after,
    ))
}

/// Write into `bytes` the checksum [`store_checksum`] computes for it,
/// answering whether they held a header to write it into.
pub fn seal(bytes: &mut [u8]) -> bool {
    let checksum = store_checksum(bytes);
    match (checksum, bytes.get_mut(CHECKSUM_AT)) {
        (Some(checksum), Some(slot)) => {
            slot.copy_from_slice(&checksum.to_le_bytes());
            true
        }
        _ => false,
    }
}

/// One name: a canonical zone's own, or an alias of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NameRecord {
    /// Offset of the name string.
    pub name: u32,
    /// The zone it names.
    pub zone: u32,
}

impl NameRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; NAME_LEN] {
        let mut out = [0; NAME_LEN];
        out[0..4].copy_from_slice(&self.name.to_le_bytes());
        out[4..8].copy_from_slice(&self.zone.to_le_bytes());
        out
    }

    /// The record `bytes` hold.
    #[must_use]
    pub fn decode(bytes: &[u8; NAME_LEN]) -> Self {
        Self {
            name: u32_at(bytes, 0),
            zone: u32_at(bytes, 4),
        }
    }
}

/// One canonical zone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZoneRecord {
    /// Offset of its canonical name.
    pub name: u32,
    /// Index of its first transition.
    pub first: u32,
    /// Its transitions, the first at `i64::MIN`.
    pub count: u32,
    /// Index of its trailing rule, or [`NO_TRAILING`].
    pub trailing: u32,
}

impl ZoneRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; ZONE_LEN] {
        let mut out = [0; ZONE_LEN];
        for (index, word) in [self.name, self.first, self.count, self.trailing]
            .iter()
            .enumerate()
        {
            out[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    /// The record `bytes` hold.
    #[must_use]
    pub fn decode(bytes: &[u8; ZONE_LEN]) -> Self {
        Self {
            name: u32_at(bytes, 0),
            first: u32_at(bytes, 4),
            count: u32_at(bytes, 8),
            trailing: u32_at(bytes, 12),
        }
    }
}

/// The instant a zone takes on a type, which it keeps until its next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TransitionRecord {
    /// The UT second the type begins.
    pub at: i64,
    /// Index of the type.
    pub kind: u16,
}

impl TransitionRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; TRANSITION_LEN] {
        let mut out = [0; TRANSITION_LEN];
        out[0..8].copy_from_slice(&self.at.to_le_bytes());
        out[8..10].copy_from_slice(&self.kind.to_le_bytes());
        out
    }

    /// The record `bytes` hold.
    #[must_use]
    pub fn decode(bytes: &[u8; TRANSITION_LEN]) -> Self {
        Self {
            at: i64_at(bytes, 0),
            kind: u16_at(bytes, 8),
        }
    }
}

/// What a zone's clocks show from a transition: shared by every transition,
/// in every zone, that shows the same.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TypeRecord {
    /// Seconds east of UT.
    pub offset: i32,
    /// Index of its abbreviation.
    pub abbrev: u16,
    /// Whether daylight saving is in force.
    pub daylight: bool,
}

impl TypeRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; TYPE_LEN] {
        let mut out = [0; TYPE_LEN];
        out[0..4].copy_from_slice(&self.offset.to_le_bytes());
        out[4..6].copy_from_slice(&self.abbrev.to_le_bytes());
        out[6] = u8::from(self.daylight);
        out
    }

    /// The record `bytes` hold, or `None` for a flag or pad byte the format
    /// does not define.
    #[must_use]
    pub fn decode(bytes: &[u8; TYPE_LEN]) -> Option<Self> {
        if bytes[6] > 1 || bytes[7] != 0 {
            return None;
        }
        Some(Self {
            offset: i32_at(bytes, 0),
            abbrev: u16_at(bytes, 4),
            daylight: bytes[6] == 1,
        })
    }
}

/// One of a trailing rule's two annual changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PhaseRecord {
    /// Seconds added to standard time once it takes effect.
    pub save: i32,
    /// Index of its abbreviation.
    pub abbrev: u16,
    /// When in each year it takes effect.
    pub moment: Moment,
}

/// The two rules a zone keeps changing between after its last transition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TrailingRecord {
    /// The zone's standard offset, seconds east of UT.
    pub stdoff: i32,
    /// The change into daylight saving.
    pub daylight: PhaseRecord,
    /// The change back to standard time.
    pub standard: PhaseRecord,
}

impl TrailingRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; TRAILING_LEN] {
        let mut out = [0; TRAILING_LEN];
        out[0..4].copy_from_slice(&self.stdoff.to_le_bytes());
        encode_phase(&self.daylight, &mut out[4..20]);
        encode_phase(&self.standard, &mut out[20..36]);
        out
    }

    /// The record `bytes` hold, or `None` for a field outside its domain.
    #[must_use]
    pub fn decode(bytes: &[u8; TRAILING_LEN]) -> Option<Self> {
        Some(Self {
            stdoff: i32_at(bytes, 0),
            daylight: decode_phase(&bytes[4..20])?,
            standard: decode_phase(&bytes[20..36])?,
        })
    }
}

/// Day codes in a phase record.
const DAY_OF_MONTH: u8 = 0;
const ON_OR_AFTER: u8 = 1;
const ON_OR_BEFORE: u8 = 2;

fn encode_phase(phase: &PhaseRecord, out: &mut [u8]) {
    let (code, day, weekday) = match phase.moment.day {
        DayRule::Of(day) => (DAY_OF_MONTH, day, 0),
        DayRule::OnOrAfter { weekday, day } => (ON_OR_AFTER, day, weekday),
        DayRule::OnOrBefore { weekday, day } => (ON_OR_BEFORE, day, weekday),
    };
    let base = match phase.moment.base {
        TimeBase::Wall => 0,
        TimeBase::Standard => 1,
        TimeBase::Universal => 2,
    };
    out[0..4].copy_from_slice(&phase.save.to_le_bytes());
    out[4..6].copy_from_slice(&phase.abbrev.to_le_bytes());
    out[6] = phase.moment.month;
    out[7] = code;
    out[8] = day;
    out[9] = weekday;
    out[10] = base;
    out[11] = 0;
    out[12..16].copy_from_slice(&phase.moment.at.to_le_bytes());
}

fn decode_phase(bytes: &[u8]) -> Option<PhaseRecord> {
    let (day, weekday) = (*bytes.get(8)?, *bytes.get(9)?);
    let day = match *bytes.get(7)? {
        DAY_OF_MONTH if weekday == 0 => DayRule::Of(day),
        ON_OR_AFTER => DayRule::OnOrAfter { weekday, day },
        ON_OR_BEFORE => DayRule::OnOrBefore { weekday, day },
        _ => return None,
    };
    let base = match *bytes.get(10)? {
        0 => TimeBase::Wall,
        1 => TimeBase::Standard,
        2 => TimeBase::Universal,
        _ => return None,
    };
    if *bytes.get(11)? != 0 {
        return None;
    }
    Some(PhaseRecord {
        save: i32_at(bytes, 0),
        abbrev: u16_at(bytes, 4),
        moment: Moment {
            month: *bytes.get(6)?,
            day,
            at: i32_at(bytes, 12),
            base,
        },
    })
}

/// One region of the world and its conventions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RegionRecord {
    /// Its ISO 3166-1 alpha-2 code.
    pub code: [u8; 2],
    /// The day its calendar week begins on.
    pub first_weekday: Weekday,
    /// Offset of its English name.
    pub name: u32,
    /// Offset of its numeric date pattern.
    pub date: u32,
    /// Offset of its time-of-day pattern.
    pub time: u32,
}

impl RegionRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; REGION_LEN] {
        let mut out = [0; REGION_LEN];
        out[0..2].copy_from_slice(&self.code);
        out[2] = u8::try_from(self.first_weekday.iso()).unwrap_or(0);
        out[4..8].copy_from_slice(&self.name.to_le_bytes());
        out[8..12].copy_from_slice(&self.date.to_le_bytes());
        out[12..16].copy_from_slice(&self.time.to_le_bytes());
        out
    }

    /// The record `bytes` hold, or `None` for a weekday no day is numbered
    /// or a set pad byte.
    #[must_use]
    pub fn decode(bytes: &[u8; REGION_LEN]) -> Option<Self> {
        if bytes[3] != 0 {
            return None;
        }
        Some(Self {
            code: [bytes[0], bytes[1]],
            first_weekday: Weekday::from_iso(u32::from(bytes[2]))?,
            name: u32_at(bytes, 4),
            date: u32_at(bytes, 8),
            time: u32_at(bytes, 12),
        })
    }
}

/// One zone a region's people keep, as the IANA zone table lists it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PlaceRecord {
    /// Index of the name the table lists it by, which may be an alias.
    pub name: u32,
    /// The region's code.
    pub region: [u8; 2],
    /// Offset of the table's comment, which tells a region's zones apart.
    pub comment: u32,
}

impl PlaceRecord {
    /// The record's bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; PLACE_LEN] {
        let mut out = [0; PLACE_LEN];
        out[0..4].copy_from_slice(&self.name.to_le_bytes());
        out[4..6].copy_from_slice(&self.region);
        out[8..12].copy_from_slice(&self.comment.to_le_bytes());
        out
    }

    /// The record `bytes` hold, or `None` for a set pad byte.
    #[must_use]
    pub fn decode(bytes: &[u8; PLACE_LEN]) -> Option<Self> {
        if u16_at(bytes, 6) != 0 {
            return None;
        }
        Some(Self {
            name: u32_at(bytes, 0),
            region: [bytes[4], bytes[5]],
            comment: u32_at(bytes, 8),
        })
    }
}

/// Where each section begins, from the header's counts. `None` when they
/// overflow or describe more than [`MAX_STORE_LEN`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Sections {
    /// The name records.
    pub names: usize,
    /// The zone records.
    pub zones: usize,
    /// The transition records.
    pub transitions: usize,
    /// The type records.
    pub types: usize,
    /// The abbreviation records.
    pub abbrevs: usize,
    /// The trailing-rule records.
    pub trailing: usize,
    /// The region records.
    pub regions: usize,
    /// The place records.
    pub places: usize,
    /// The string section.
    pub strings: usize,
    /// One past the last byte.
    pub end: usize,
}

impl Sections {
    /// The section starts `header` implies.
    #[must_use]
    pub fn of(header: &Header) -> Option<Self> {
        let span = |count: u32, len: usize| usize::try_from(count).ok()?.checked_mul(len);
        let names = HEADER_LEN;
        let zones = names.checked_add(span(header.name_count, NAME_LEN)?)?;
        let transitions = zones.checked_add(span(header.zone_count, ZONE_LEN)?)?;
        let types = transitions.checked_add(span(header.transition_count, TRANSITION_LEN)?)?;
        let abbrevs = types.checked_add(span(header.type_count, TYPE_LEN)?)?;
        let trailing = abbrevs.checked_add(span(header.abbrev_count, ABBREV_LEN)?)?;
        let regions = trailing.checked_add(span(header.trailing_count, TRAILING_LEN)?)?;
        let places = regions.checked_add(span(header.region_count, REGION_LEN)?)?;
        let strings = places.checked_add(span(header.place_count, PLACE_LEN)?)?;
        let end = strings.checked_add(usize::try_from(header.strings_len).ok()?)?;
        (end <= MAX_STORE_LEN).then_some(Self {
            names,
            zones,
            transitions,
            types,
            abbrevs,
            trailing,
            regions,
            places,
            strings,
            end,
        })
    }
}

/// The `N` bytes at `at` of `bytes`, or `None` past its end.
#[must_use]
pub fn record<const N: usize>(bytes: &[u8], at: usize) -> Option<&[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

fn i32_at(bytes: &[u8], at: usize) -> i32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    i32::from_le_bytes(word)
}

fn i64_at(bytes: &[u8], at: usize) -> i64 {
    let mut word = [0; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    i64::from_le_bytes(word)
}
