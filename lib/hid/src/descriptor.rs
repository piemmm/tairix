//! The HID report-descriptor model (USB HID 1.11 §6.2.2).
//!
//! [`ReportDescriptor::parse`] turns a device's untrusted report descriptor
//! into every Input, Output and Feature item as a [`Field`] and every
//! collection as a [`Collection`], validated whole: a descriptor that breaks a
//! bound or the grammar is refused, never partly read. The application
//! decoders select fields from the model by usage and collection.

use alloc::vec::Vec;
use core::num::NonZeroU8;
use core::ops::Range;

/// Longest report descriptor parsed (Linux's `HID_MAX_DESCRIPTOR_SIZE`).
pub const MAX_DESCRIPTOR: usize = 4096;

const REPORT_BYTES: u16 = 4096;

/// Longest report, in bytes, its report ID included: a touchpad's
/// certification feature report alone is 256.
pub const MAX_REPORT: usize = REPORT_BYTES as usize;

const MAX_FIELDS: usize = 256;
const MAX_COLLECTIONS: usize = 128;
const MAX_USAGE_ENTRIES: usize = 1024;
const MAX_DEPTH: usize = 16;
const MAX_GLOBAL_STACK: usize = 8;
/// Widest element an item may declare (Linux's bound); reads are 32-bit.
const MAX_ELEMENT_BITS: u32 = 256;
const MAX_REPORT_BITS: u32 = REPORT_BYTES as u32 * 8;
const REPORT_ID_BITS: u32 = 8;

const ITEM_MAIN: u8 = 0;
const ITEM_GLOBAL: u8 = 1;
const ITEM_LOCAL: u8 = 2;
const LONG_ITEM_PREFIX: u8 = 0xFE;

const MAIN_INPUT: u8 = 0x8;
const MAIN_OUTPUT: u8 = 0x9;
const MAIN_COLLECTION: u8 = 0xA;
const MAIN_FEATURE: u8 = 0xB;
const MAIN_END_COLLECTION: u8 = 0xC;

const GLOBAL_USAGE_PAGE: u8 = 0x0;
const GLOBAL_LOGICAL_MIN: u8 = 0x1;
const GLOBAL_LOGICAL_MAX: u8 = 0x2;
const GLOBAL_PHYSICAL_MIN: u8 = 0x3;
const GLOBAL_PHYSICAL_MAX: u8 = 0x4;
const GLOBAL_UNIT_EXPONENT: u8 = 0x5;
const GLOBAL_UNIT: u8 = 0x6;
const GLOBAL_REPORT_SIZE: u8 = 0x7;
const GLOBAL_REPORT_ID: u8 = 0x8;
const GLOBAL_REPORT_COUNT: u8 = 0x9;
const GLOBAL_PUSH: u8 = 0xA;
const GLOBAL_POP: u8 = 0xB;

const LOCAL_USAGE: u8 = 0x0;
const LOCAL_USAGE_MIN: u8 = 0x1;
const LOCAL_USAGE_MAX: u8 = 0x2;
const LOCAL_DELIMITER: u8 = 0xA;

/// Why a report descriptor was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    /// Empty, or longer than [`MAX_DESCRIPTOR`].
    Length,
    /// An item runs past the end.
    Truncated,
    /// More fields, collections, usages, nesting or pushes than allowed.
    TooMany,
    /// An End Collection with none open, or a collection left open.
    Unbalanced,
    /// A Pop with nothing pushed.
    StackUnderflow,
    /// Report ID zero, which HID reserves.
    ReportIdZero,
    /// A report longer than [`MAX_REPORT`], or an element wider than 256 bits.
    ReportTooLong,
    /// A usage minimum above its maximum, or a range spanning pages.
    BadUsageRange,
    /// A delimiter opened inside another, or closed with none open.
    BadDelimiter,
    /// A data field outside every report ID in a descriptor that declares
    /// report IDs: every report then carries one, so the field's report
    /// cannot be told from another's.
    Undemuxable,
    /// No memory for the model.
    OutOfMemory,
}

/// A usage: its page and id (HID Usage Tables).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Usage {
    /// The usage page.
    pub page: u16,
    /// The usage id on that page.
    pub id: u16,
}

impl Usage {
    /// The usage `id` on `page`.
    #[must_use]
    pub const fn new(page: u16, id: u16) -> Self {
        Self { page, id }
    }
}

/// The usages a field's elements carry, in order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageEntry {
    /// One usage.
    One(Usage),
    /// The usages `min..=max` on one page.
    Range {
        /// The page.
        page: u16,
        /// The first id.
        min: u16,
        /// The last id.
        max: u16,
    },
}

impl UsageEntry {
    /// How many usages the entry stands for.
    #[must_use]
    pub const fn len(self) -> u32 {
        match self {
            Self::One(_) => 1,
            Self::Range { min, max, .. } => max as u32 - min as u32 + 1,
        }
    }

    /// Whether the entry stands for none, which a parsed entry never does.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// The `index`th usage of the entry.
    #[must_use]
    pub fn nth(self, index: u32) -> Option<Usage> {
        match self {
            Self::One(usage) => (index == 0).then_some(usage),
            Self::Range { page, min, max } => {
                let id = u16::try_from(u32::from(min) + index).ok()?;
                (id <= max).then_some(Usage::new(page, id))
            }
        }
    }

    /// Where `usage` falls in the entry.
    #[must_use]
    pub fn position(self, usage: Usage) -> Option<u32> {
        match self {
            Self::One(one) => (one == usage).then_some(0),
            Self::Range { page, min, max } => (usage.page == page
                && (min..=max).contains(&usage.id))
            .then(|| u32::from(usage.id - min)),
        }
    }
}

/// The kind of report a field travels in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportKind {
    /// Device to host, on the interrupt channel.
    Input,
    /// Host to device.
    Output,
    /// Either way, on the control channel.
    Feature,
}

/// Which report a field belongs to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReportId {
    /// The descriptor declares no report IDs, so its reports carry none.
    #[default]
    Unprefixed,
    /// The report this ID prefixes.
    Prefixed(NonZeroU8),
}

impl ReportId {
    /// The ID byte the report starts with, if any.
    #[must_use]
    pub const fn id(self) -> Option<u8> {
        match self {
            Self::Unprefixed => None,
            Self::Prefixed(id) => Some(id.get()),
        }
    }

    /// Whether `report` is this report.
    #[must_use]
    pub fn matches(self, report: &[u8]) -> bool {
        match self {
            Self::Unprefixed => true,
            Self::Prefixed(id) => report.first() == Some(&id.get()),
        }
    }

    const fn prefix_bits(self) -> u32 {
        match self {
            Self::Unprefixed => 0,
            Self::Prefixed(_) => REPORT_ID_BITS,
        }
    }
}

/// A main item's data flags (HID 1.11 §6.2.2.5).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FieldFlags(u16);

impl FieldFlags {
    /// Constant rather than data.
    pub const CONSTANT: u16 = 1 << 0;
    /// One value per usage, rather than an array of usage selectors.
    pub const VARIABLE: u16 = 1 << 1;
    /// A change since the last report, rather than a position.
    pub const RELATIVE: u16 = 1 << 2;
    /// Wraps past its extremes.
    pub const WRAP: u16 = 1 << 3;
    /// Not linear in what it measures.
    pub const NON_LINEAR: u16 = 1 << 4;
    /// No preferred state to return to.
    pub const NO_PREFERRED: u16 = 1 << 5;
    /// Has a state meaning "no data", outside its logical range.
    pub const NULL_STATE: u16 = 1 << 6;
    /// May change without the host writing it (Output and Feature).
    pub const VOLATILE: u16 = 1 << 7;
    /// A stream of bytes rather than a value.
    pub const BUFFERED_BYTES: u16 = 1 << 8;

    /// The flags `bits` sets.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Whether every flag in `flag` is set.
    #[must_use]
    pub const fn contains(self, flag: u16) -> bool {
        self.0 & flag == flag
    }

    /// Whether the field is one value per usage.
    #[must_use]
    pub const fn is_variable(self) -> bool {
        self.contains(Self::VARIABLE)
    }

    /// Whether the field carries changes rather than positions.
    #[must_use]
    pub const fn is_relative(self) -> bool {
        self.contains(Self::RELATIVE)
    }
}

/// What a collection groups (HID 1.11 §6.2.2.6).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionKind {
    /// Data collected at one geometric point.
    Physical,
    /// A top-level item an application uses: a keyboard, a touch pad.
    Application,
    /// Interrelated data: one finger of a touch pad.
    Logical,
    /// A report's fields.
    Report,
    /// An array of selectors.
    NamedArray,
    /// Modifies the meaning of the usage it holds.
    UsageSwitch,
    /// Modifies the meaning of the usage it is attached to.
    UsageModifier,
    /// Reserved or vendor-defined (the raw code).
    Other(u8),
}

impl CollectionKind {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Physical,
            1 => Self::Application,
            2 => Self::Logical,
            3 => Self::Report,
            4 => Self::NamedArray,
            5 => Self::UsageSwitch,
            6 => Self::UsageModifier,
            other => Self::Other(other),
        }
    }
}

/// A collection's place in the model.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CollectionIndex(u16);

impl CollectionIndex {
    /// The index of the collection at `position` in
    /// [`ReportDescriptor::collections`].
    #[must_use]
    pub fn new(position: usize) -> Option<Self> {
        u16::try_from(position).ok().map(Self)
    }

    /// The index as a position in [`ReportDescriptor::collections`].
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// One collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Collection {
    /// What it groups.
    pub kind: CollectionKind,
    /// The usage that names it.
    pub usage: Usage,
    /// The collection it sits in, none at the top level.
    pub parent: Option<CollectionIndex>,
}

/// One Input, Output or Feature item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    /// The kind of report it travels in.
    pub kind: ReportKind,
    /// The report it travels in.
    pub report: ReportId,
    /// Bit offset of its first element from the start of the report as
    /// sent, the report ID byte included.
    pub offset: u32,
    /// Bits per element.
    pub size: u16,
    /// Elements.
    pub count: u16,
    /// Its main-item flags.
    pub flags: FieldFlags,
    /// Its logical range, inclusive.
    pub logical: (i64, i64),
    /// Its physical range, inclusive: the logical one where none is declared.
    pub physical: (i64, i64),
    /// Its unit (HID 1.11 §6.2.2.7), zero for none.
    pub unit: u32,
    /// The power of ten its physical values are scaled by.
    pub unit_exponent: i8,
    /// The innermost collection it was declared in.
    pub collection: Option<CollectionIndex>,
    usages: Range<u16>,
}

impl Field {
    /// The raw bits of element `element` in `report`, if it is this field's
    /// report and long enough. `None` for an element wider than 32 bits.
    #[must_use]
    pub fn raw(&self, report: &[u8], element: u16) -> Option<u32> {
        if element >= self.count || !self.report.matches(report) {
            return None;
        }
        let size = u32::from(self.size);
        let offset = self
            .offset
            .checked_add(u32::from(element).checked_mul(size)?)?;
        read_bits(report, offset, size)
    }

    /// Element `element` of `report` as a logical value: signed when the
    /// logical range is, unsigned otherwise.
    #[must_use]
    pub fn value(&self, report: &[u8], element: u16) -> Option<i64> {
        let raw = self.raw(report, element)?;
        Some(if self.logical.0 < 0 {
            i64::from(sign_extend(raw, self.size))
        } else {
            i64::from(raw)
        })
    }

    /// Whether `value` lies in the logical range, so names a state rather
    /// than "no data".
    #[must_use]
    pub const fn in_range(&self, value: i64) -> bool {
        value >= self.logical.0 && value <= self.logical.1
    }
}

/// One report's length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReportLength {
    kind: ReportKind,
    id: ReportId,
    bits: u32,
}

/// A parsed report descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportDescriptor {
    fields: Vec<Field>,
    collections: Vec<Collection>,
    usages: Vec<UsageEntry>,
    reports: Vec<ReportLength>,
    report_ids: bool,
}

impl ReportDescriptor {
    /// Parse `bytes`.
    ///
    /// # Errors
    ///
    /// The [`DescriptorError`] naming the first rule `bytes` breaks.
    pub fn parse(bytes: &[u8]) -> Result<Self, DescriptorError> {
        if bytes.is_empty() || bytes.len() > MAX_DESCRIPTOR {
            return Err(DescriptorError::Length);
        }
        let mut parser = Parser::default();
        let mut items = Items { bytes, at: 0 };
        while let Some(item) = items.next()? {
            parser.item(item)?;
        }
        parser.finish()
    }

    /// Every field, in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Every collection, in declaration order.
    #[must_use]
    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    /// The collection at `index`.
    #[must_use]
    pub fn collection(&self, index: CollectionIndex) -> Option<&Collection> {
        self.collections.get(index.get())
    }

    /// Whether any report carries a report ID.
    #[must_use]
    pub const fn uses_report_ids(&self) -> bool {
        self.report_ids
    }

    /// The top-level collection `index` sits in (itself, at the top level).
    #[must_use]
    pub fn top_level(&self, index: CollectionIndex) -> CollectionIndex {
        let mut at = index;
        while let Some(parent) = self.collection(at).and_then(|collection| collection.parent) {
            at = parent;
        }
        at
    }

    /// Whether `outer` is `inner` or encloses it.
    #[must_use]
    pub fn encloses(&self, outer: CollectionIndex, inner: Option<CollectionIndex>) -> bool {
        let mut at = inner;
        while let Some(index) = at {
            if index == outer {
                return true;
            }
            at = self
                .collection(index)
                .and_then(|collection| collection.parent);
        }
        false
    }

    /// The usage entries `field` carries, in order.
    #[must_use]
    pub fn usages(&self, field: &Field) -> &[UsageEntry] {
        let range = usize::from(field.usages.start)..usize::from(field.usages.end);
        self.usages.get(range).unwrap_or(&[])
    }

    /// The usage of element `element` of a variable `field`: its place in the
    /// usage list, the last usage standing for every element past the list.
    #[must_use]
    pub fn element_usage(&self, field: &Field, element: u16) -> Option<Usage> {
        if element >= field.count {
            return None;
        }
        let entries = self.usages(field);
        let mut remaining = u32::from(element);
        for entry in entries {
            if remaining < entry.len() {
                return entry.nth(remaining);
            }
            remaining -= entry.len();
        }
        let last = entries.last()?;
        last.nth(last.len() - 1)
    }

    /// The usage an array `field` selects with `value`: the usage at
    /// `value`'s offset into the logical range, none past the list.
    #[must_use]
    pub fn array_usage(&self, field: &Field, value: i64) -> Option<Usage> {
        if !field.in_range(value) {
            return None;
        }
        let mut remaining = u32::try_from(value - field.logical.0).ok()?;
        for entry in self.usages(field) {
            if remaining < entry.len() {
                return entry.nth(remaining);
            }
            remaining -= entry.len();
        }
        None
    }

    /// Element of a variable `field` that carries `usage`.
    #[must_use]
    pub fn element_of(&self, field: &Field, usage: Usage) -> Option<u16> {
        let mut before = 0u32;
        for entry in self.usages(field) {
            if let Some(at) = entry.position(usage) {
                let element = u16::try_from(before + at).ok()?;
                return (element < field.count).then_some(element);
            }
            before += entry.len();
        }
        None
    }

    /// Whether `field` carries `usage` at all.
    #[must_use]
    pub fn carries(&self, field: &Field, usage: Usage) -> bool {
        self.usages(field)
            .iter()
            .any(|entry| entry.position(usage).is_some())
    }

    /// The length in bytes of the `kind` report `id` names, its ID included.
    #[must_use]
    pub fn report_len(&self, kind: ReportKind, id: ReportId) -> Option<usize> {
        self.reports
            .iter()
            .find(|report| report.kind == kind && report.id == id)
            .and_then(|report| usize::try_from(report.bits.div_ceil(8)).ok())
    }

    /// The longest report of `kind`, in bytes.
    #[must_use]
    pub fn longest_report(&self, kind: ReportKind) -> usize {
        self.reports
            .iter()
            .filter(|report| report.kind == kind)
            .filter_map(|report| usize::try_from(report.bits.div_ceil(8)).ok())
            .max()
            .unwrap_or(0)
    }
}

/// One short item.
#[derive(Clone, Copy)]
struct Item {
    kind: u8,
    tag: u8,
    data: u32,
    /// Bytes of data: 0, 1, 2 or 4.
    len: u8,
}

impl Item {
    /// The data read as a two's-complement number of its width.
    fn signed(self) -> i64 {
        i64::from(sign_extend(self.data, u16::from(self.len) * 8))
    }
}

/// The items of a descriptor, long items consumed and skipped.
struct Items<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Items<'_> {
    fn next(&mut self) -> Result<Option<Item>, DescriptorError> {
        loop {
            let Some(&prefix) = self.bytes.get(self.at) else {
                return Ok(None);
            };
            self.at += 1;
            if prefix == LONG_ITEM_PREFIX {
                let data = usize::from(*self.bytes.get(self.at).ok_or(DescriptorError::Truncated)?);
                self.at = self
                    .at
                    .checked_add(2 + data)
                    .ok_or(DescriptorError::Truncated)?;
                if self.at > self.bytes.len() {
                    return Err(DescriptorError::Truncated);
                }
                continue;
            }
            let len = match prefix & 0x03 {
                3 => 4u8,
                n => n,
            };
            let end = self.at + usize::from(len);
            let bytes = self
                .bytes
                .get(self.at..end)
                .ok_or(DescriptorError::Truncated)?;
            let data = bytes
                .iter()
                .rev()
                .fold(0u32, |data, &byte| (data << 8) | u32::from(byte));
            self.at = end;
            return Ok(Some(Item {
                kind: (prefix >> 2) & 0x03,
                tag: prefix >> 4,
                data,
                len,
            }));
        }
    }
}

/// A raw minimum or maximum, kept until its partner decides its sign.
#[derive(Clone, Copy, Debug, Default)]
struct Bound {
    item: Option<(u32, u8)>,
}

impl Bound {
    fn set(&mut self, item: Item) {
        self.item = Some((item.data, item.len));
    }

    fn signed(self) -> i64 {
        self.item.map_or(0, |(data, len)| {
            i64::from(sign_extend(data, u16::from(len) * 8))
        })
    }

    fn unsigned(self) -> i64 {
        self.item.map_or(0, |(data, _)| i64::from(data))
    }

    const fn declared(self) -> bool {
        self.item.is_some()
    }
}

/// The inclusive range a minimum and maximum declare: the maximum reads
/// unsigned unless the minimum is negative (HID 1.11 §6.2.2.7), so a one-byte
/// `Logical Maximum (255)` is 255.
fn range(min: Bound, max: Bound) -> (i64, i64) {
    let low = min.signed();
    let high = if low < 0 {
        max.signed()
    } else {
        max.unsigned()
    };
    (low, high)
}

/// Global state, saved and restored by Push and Pop with the report ID.
#[derive(Clone, Copy, Debug, Default)]
struct Globals {
    page: u16,
    logical_min: Bound,
    logical_max: Bound,
    physical_min: Bound,
    physical_max: Bound,
    unit_exponent: i8,
    unit: u32,
    size: u32,
    count: u32,
    report: ReportId,
}

/// A local usage as written: its page when the item named one.
#[derive(Clone, Copy, Debug)]
struct LocalUsage {
    page: Option<u16>,
    id: u16,
}

impl LocalUsage {
    const fn from_item(item: Item) -> Self {
        Self {
            page: if item.len == 4 {
                Some((item.data >> 16) as u16)
            } else {
                None
            },
            id: (item.data & 0xFFFF) as u16,
        }
    }

    /// The usage, its page the one in force at the main item where it named
    /// none (HID 1.11 §6.2.2.8).
    fn resolve(self, page: u16) -> Usage {
        Usage::new(self.page.unwrap_or(page), self.id)
    }
}

/// Local state, cleared at every main item.
#[derive(Clone, Debug, Default)]
struct Locals {
    entries: Vec<LocalEntry>,
    minimum: Option<LocalUsage>,
    delimiter: Delimiter,
}

#[derive(Clone, Copy, Debug)]
enum LocalEntry {
    One(LocalUsage),
    Range(LocalUsage, LocalUsage),
}

/// Where a parse is within a delimiter set of alternative usages, of which
/// only the first is taken.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Delimiter {
    #[default]
    None,
    Open {
        taken: bool,
    },
}

#[derive(Default)]
struct Parser {
    globals: Globals,
    stack: Vec<Globals>,
    locals: Locals,
    open: Vec<CollectionIndex>,
    model: Model,
    /// A data field was declared outside every report ID.
    unprefixed_data: bool,
}

#[derive(Default)]
struct Model {
    fields: Vec<Field>,
    collections: Vec<Collection>,
    usages: Vec<UsageEntry>,
    reports: Vec<ReportLength>,
}

fn push<T>(list: &mut Vec<T>, value: T, bound: usize) -> Result<(), DescriptorError> {
    if list.len() >= bound {
        return Err(DescriptorError::TooMany);
    }
    list.try_reserve(1)
        .map_err(|_| DescriptorError::OutOfMemory)?;
    list.push(value);
    Ok(())
}

impl Parser {
    fn item(&mut self, item: Item) -> Result<(), DescriptorError> {
        match item.kind {
            ITEM_MAIN => self.main(item),
            ITEM_GLOBAL => self.global(item),
            ITEM_LOCAL => self.local(item),
            _ => Ok(()),
        }
    }

    fn main(&mut self, item: Item) -> Result<(), DescriptorError> {
        match item.tag {
            MAIN_INPUT => self.field(ReportKind::Input, item.data)?,
            MAIN_OUTPUT => self.field(ReportKind::Output, item.data)?,
            MAIN_FEATURE => self.field(ReportKind::Feature, item.data)?,
            MAIN_COLLECTION => self.open_collection(item.data)?,
            MAIN_END_COLLECTION => {
                self.open.pop().ok_or(DescriptorError::Unbalanced)?;
            }
            _ => {}
        }
        if self.locals.delimiter != Delimiter::None {
            return Err(DescriptorError::BadDelimiter);
        }
        self.locals.entries.clear();
        self.locals.minimum = None;
        Ok(())
    }

    fn global(&mut self, item: Item) -> Result<(), DescriptorError> {
        let globals = &mut self.globals;
        match item.tag {
            GLOBAL_USAGE_PAGE => globals.page = (item.data & 0xFFFF) as u16,
            GLOBAL_LOGICAL_MIN => globals.logical_min.set(item),
            GLOBAL_LOGICAL_MAX => globals.logical_max.set(item),
            GLOBAL_PHYSICAL_MIN => globals.physical_min.set(item),
            GLOBAL_PHYSICAL_MAX => globals.physical_max.set(item),
            // A nibble, as HID 1.11 states, or a whole signed byte, as many
            // devices write it (Linux accepts both).
            GLOBAL_UNIT_EXPONENT => {
                let exponent = if item.data <= 0xF {
                    i64::from(sign_extend(item.data, 4))
                } else {
                    item.signed()
                };
                globals.unit_exponent =
                    i8::try_from(exponent).map_err(|_| DescriptorError::ReportTooLong)?;
            }
            GLOBAL_UNIT => globals.unit = item.data,
            GLOBAL_REPORT_SIZE => globals.size = item.data,
            GLOBAL_REPORT_COUNT => globals.count = item.data,
            GLOBAL_REPORT_ID => {
                let id = u8::try_from(item.data).map_err(|_| DescriptorError::ReportIdZero)?;
                let id = NonZeroU8::new(id).ok_or(DescriptorError::ReportIdZero)?;
                globals.report = ReportId::Prefixed(id);
            }
            GLOBAL_PUSH => push(&mut self.stack, *globals, MAX_GLOBAL_STACK)?,
            GLOBAL_POP => {
                *globals = self.stack.pop().ok_or(DescriptorError::StackUnderflow)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn local(&mut self, item: Item) -> Result<(), DescriptorError> {
        match item.tag {
            LOCAL_USAGE => {
                if let Delimiter::Open { taken } = &mut self.locals.delimiter {
                    if *taken {
                        return Ok(());
                    }
                    *taken = true;
                }
                push(
                    &mut self.locals.entries,
                    LocalEntry::One(LocalUsage::from_item(item)),
                    MAX_USAGE_ENTRIES,
                )
            }
            LOCAL_USAGE_MIN => {
                self.locals.minimum = Some(LocalUsage::from_item(item));
                Ok(())
            }
            LOCAL_USAGE_MAX => {
                let min = self
                    .locals
                    .minimum
                    .take()
                    .ok_or(DescriptorError::BadUsageRange)?;
                push(
                    &mut self.locals.entries,
                    LocalEntry::Range(min, LocalUsage::from_item(item)),
                    MAX_USAGE_ENTRIES,
                )
            }
            LOCAL_DELIMITER => {
                self.locals.delimiter = match (self.locals.delimiter, item.data) {
                    (Delimiter::None, 1) => Delimiter::Open { taken: false },
                    (Delimiter::Open { .. }, 0) => Delimiter::None,
                    _ => return Err(DescriptorError::BadDelimiter),
                };
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A local usage entry resolved against the page in force now.
    fn resolve(&self, entry: LocalEntry) -> Result<UsageEntry, DescriptorError> {
        let page = self.globals.page;
        Ok(match entry {
            LocalEntry::One(usage) => UsageEntry::One(usage.resolve(page)),
            LocalEntry::Range(min, max) => {
                let (min, max) = (min.resolve(page), max.resolve(page));
                if min.page != max.page || min.id > max.id {
                    return Err(DescriptorError::BadUsageRange);
                }
                UsageEntry::Range {
                    page: min.page,
                    min: min.id,
                    max: max.id,
                }
            }
        })
    }

    fn open_collection(&mut self, code: u32) -> Result<(), DescriptorError> {
        if self.open.len() >= MAX_DEPTH {
            return Err(DescriptorError::TooMany);
        }
        let usage = match self.locals.entries.first() {
            Some(&entry) => self.resolve(entry)?.nth(0).unwrap_or_default(),
            None => Usage::default(),
        };
        let index = CollectionIndex(
            u16::try_from(self.model.collections.len()).map_err(|_| DescriptorError::TooMany)?,
        );
        push(
            &mut self.model.collections,
            Collection {
                kind: CollectionKind::from_code((code & 0xFF) as u8),
                usage,
                parent: self.open.last().copied(),
            },
            MAX_COLLECTIONS,
        )?;
        push(&mut self.open, index, MAX_DEPTH)
    }

    fn field(&mut self, kind: ReportKind, data: u32) -> Result<(), DescriptorError> {
        let globals = self.globals;
        if globals.size > MAX_ELEMENT_BITS {
            return Err(DescriptorError::ReportTooLong);
        }
        let report = globals.report;
        let start = self.report_end(kind, report);
        let width = globals
            .size
            .checked_mul(globals.count)
            .filter(|&width| width <= MAX_REPORT_BITS)
            .ok_or(DescriptorError::ReportTooLong)?;
        let end = start
            .checked_add(width)
            .filter(|&end| end <= MAX_REPORT_BITS)
            .ok_or(DescriptorError::ReportTooLong)?;
        self.set_report_end(kind, report, end)?;
        let flags = FieldFlags::from_bits((data & 0x1FF) as u16);
        // A field naming no usage is padding: it only takes room.
        if self.locals.entries.is_empty() || width == 0 {
            return Ok(());
        }
        if !flags.contains(FieldFlags::CONSTANT) && report == ReportId::Unprefixed {
            self.unprefixed_data = true;
        }
        let first = u16::try_from(self.model.usages.len()).map_err(|_| DescriptorError::TooMany)?;
        for index in 0..self.locals.entries.len() {
            let entry = self.resolve(self.locals.entries[index])?;
            push(&mut self.model.usages, entry, MAX_USAGE_ENTRIES)?;
        }
        let last = u16::try_from(self.model.usages.len()).map_err(|_| DescriptorError::TooMany)?;
        let logical = range(globals.logical_min, globals.logical_max);
        let physical = if globals.physical_min.declared() || globals.physical_max.declared() {
            range(globals.physical_min, globals.physical_max)
        } else {
            logical
        };
        push(
            &mut self.model.fields,
            Field {
                kind,
                report,
                offset: start,
                size: u16::try_from(globals.size).map_err(|_| DescriptorError::ReportTooLong)?,
                count: u16::try_from(globals.count).map_err(|_| DescriptorError::ReportTooLong)?,
                flags,
                logical,
                physical,
                unit: globals.unit,
                unit_exponent: globals.unit_exponent,
                collection: self.open.last().copied(),
                usages: first..last,
            },
            MAX_FIELDS,
        )
    }

    fn report_end(&self, kind: ReportKind, id: ReportId) -> u32 {
        self.model
            .reports
            .iter()
            .find(|report| report.kind == kind && report.id == id)
            .map_or(id.prefix_bits(), |report| report.bits)
    }

    fn set_report_end(
        &mut self,
        kind: ReportKind,
        id: ReportId,
        bits: u32,
    ) -> Result<(), DescriptorError> {
        if let Some(report) = self
            .model
            .reports
            .iter_mut()
            .find(|report| report.kind == kind && report.id == id)
        {
            report.bits = bits;
            return Ok(());
        }
        // One per kind and report ID: the bound is the ID space.
        push(
            &mut self.model.reports,
            ReportLength { kind, id, bits },
            3 * 256,
        )
    }

    fn finish(self) -> Result<ReportDescriptor, DescriptorError> {
        if !self.open.is_empty() {
            return Err(DescriptorError::Unbalanced);
        }
        let report_ids = self
            .model
            .reports
            .iter()
            .any(|report| report.id != ReportId::Unprefixed);
        if report_ids && self.unprefixed_data {
            return Err(DescriptorError::Undemuxable);
        }
        Ok(ReportDescriptor {
            fields: self.model.fields,
            collections: self.model.collections,
            usages: self.model.usages,
            reports: self.model.reports,
            report_ids,
        })
    }
}

/// Read `size` (1..=32) little-endian bits at bit `offset` of `data`; `None`
/// past its end.
pub(crate) fn read_bits(data: &[u8], offset: u32, size: u32) -> Option<u32> {
    if size == 0 || size > 32 {
        return None;
    }
    let end = offset.checked_add(size)?;
    if usize::try_from(end).ok()? > data.len().checked_mul(8)? {
        return None;
    }
    let first = usize::try_from(offset / 8).ok()?;
    let last = usize::try_from((end - 1) / 8).ok()?;
    let mut window = 0u64;
    for (shift, &byte) in data[first..=last].iter().enumerate() {
        window |= u64::from(byte) << (8 * shift);
    }
    let value = (window >> (offset % 8)) & ((1u64 << size) - 1);
    u32::try_from(value).ok()
}

/// Write the low `size` (1..=32) bits of `value` at bit `offset` of `data`,
/// every other bit left as it was; `None` past its end.
pub(crate) fn write_bits(data: &mut [u8], offset: u32, size: u32, value: u32) -> Option<()> {
    if size == 0 || size > 32 {
        return None;
    }
    let end = offset.checked_add(size)?;
    if usize::try_from(end).ok()? > data.len().checked_mul(8)? {
        return None;
    }
    for bit in 0..size {
        let at = offset + bit;
        let byte = data.get_mut(usize::try_from(at / 8).ok()?)?;
        let mask = 1u8 << (at % 8);
        if (value >> bit) & 1 == 1 {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
    Some(())
}

/// `value`'s low `bits` read as two's complement.
#[allow(clippy::cast_possible_wrap)] // Reinterpreting the top bit as the sign is the point.
pub(crate) const fn sign_extend(value: u32, bits: u16) -> i32 {
    if bits == 0 || bits >= 32 {
        return value as i32;
    }
    let shift = 32 - bits as u32;
    ((value << shift) as i32) >> shift
}

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod tests;
