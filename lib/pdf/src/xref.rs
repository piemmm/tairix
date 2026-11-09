//! The cross-reference (ISO 32000-2 §7.5.4–§7.5.8): where each object is,
//! read from its tables and streams through the chain of incremental updates,
//! or rebuilt by scanning a file whose cross-reference is damaged.

use alloc::vec::Vec;
use core::cell::Cell;

use tairix_collections::{OrderedMap, OrderedSet};
use tairix_util::fallible::{self, try_push};

use crate::error::tolerated;
use crate::filter::{self, Decoded};
use crate::lex::{is_regular, is_white, Lexer, Token};
use crate::limits::{MAX_LEX_PASSES, MAX_OBJECTS, MAX_OBJECT_STREAM_OBJECTS, MAX_XREF_SECTIONS};
use crate::object::{Dict, Name, ObjRef, Object};
use crate::parse::{self, Endstreams, Indirect};
use crate::PdfError;

/// Where one object is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Entry {
    /// Nowhere: freed, or never defined.
    Free,
    /// At this offset of the file.
    Offset(usize),
    /// The `index`th object of the object stream numbered `stream`.
    Compressed { stream: u32, index: u32 },
}

/// A file's cross-reference: each object's place, and the trailer.
#[derive(Default)]
pub(crate) struct Xref {
    entries: OrderedMap<u32, Entry>,
    /// The trailer: the newest section's, completed from older ones.
    pub(crate) trailer: Option<Dict>,
    /// The object streams a repair found, whose objects are placed only once
    /// their streams can be decrypted.
    pub(crate) unindexed: Vec<u32>,
}

impl Xref {
    /// Where object `num` is.
    pub(crate) fn entry(&self, num: u32) -> Entry {
        self.entries.get(&num).copied().unwrap_or(Entry::Free)
    }

    /// Record `entry` for `num` unless a newer section already did, the room
    /// it takes charged to `budget` where its row was decoded from a stream.
    fn define(
        &mut self,
        num: u32,
        entry: Entry,
        budget: Option<&Cell<u64>>,
    ) -> Result<(), PdfError> {
        if self.entries.contains_key(&num) {
            return Ok(());
        }
        if let Some(budget) = budget {
            return charged_insert(&mut self.entries, num, entry, budget);
        }
        self.entries.try_insert(num, entry)?;
        Ok(())
    }

    /// Place the objects of object stream `stream` a repair found, each
    /// where no object of its number lies later in the file: an update
    /// appends, so the later definition is the newer. The room their places
    /// take is charged to `budget`, as the stream's bytes were.
    pub(crate) fn index(
        &mut self,
        stream: u32,
        objects: &ObjectStream,
        budget: &Cell<u64>,
    ) -> Result<(), PdfError> {
        let Some(at) = self.offset_of(stream) else {
            return Ok(());
        };
        for (index, num) in (0u32..).zip(objects.numbers()) {
            let older = match self.entry(num) {
                Entry::Free => true,
                Entry::Offset(direct) => direct < at,
                Entry::Compressed { stream: other, .. } => {
                    self.offset_of(other).is_some_and(|other| other < at)
                }
            };
            if older {
                charged_insert(
                    &mut self.entries,
                    num,
                    Entry::Compressed { stream, index },
                    budget,
                )?;
            }
        }
        Ok(())
    }

    /// Where in the file object `num` is written, if it is written directly.
    fn offset_of(&self, num: u32) -> Option<usize> {
        match self.entry(num) {
            Entry::Offset(at) => Some(at),
            _ => None,
        }
    }

    /// The numbers of the objects it places, ascending.
    pub(crate) fn numbers(&self) -> impl Iterator<Item = u32> + '_ {
        self.entries
            .iter()
            .filter(|(_, entry)| **entry != Entry::Free)
            .map(|(&num, _)| num)
    }
}

/// `value` put at `key` in `map`, the room the map grows by charged to
/// `budget`, or nothing put where that is refused: what is built from what a
/// file's streams decode to is held to the budget they decode within.
pub(crate) fn charged_insert<K: Ord + Copy, V>(
    map: &mut OrderedMap<K, V>,
    key: K,
    value: V,
    budget: &Cell<u64>,
) -> Result<(), PdfError> {
    let before = map.allocated_bytes();
    map.try_insert(key, value)?;
    let charged = filter::charge(budget, map.allocated_bytes().saturating_sub(before));
    if charged.is_err() {
        map.remove(&key);
    }
    charged
}

/// A file being read: its bytes, what its streams may decode to, and where
/// its `endstream` keywords lie.
#[derive(Clone, Copy)]
pub(crate) struct Source<'a> {
    pub(crate) file: &'a [u8],
    pub(crate) budget: &'a Cell<u64>,
    pub(crate) ends: &'a Endstreams,
}

/// The cross-reference `source`'s file states: its sections from `startxref`
/// back through `/Prev`, the newest definition of each object and the newest
/// value of each trailer key winning.
pub(crate) fn chained(source: Source<'_>) -> Result<Xref, PdfError> {
    let file = source.file;
    let mut xref = Xref::default();
    let mut trailers = Vec::new();
    let mut seen = OrderedSet::new();
    let mut next = Some(startxref(file)?);
    while let Some(offset) = next {
        if seen.len() >= MAX_XREF_SECTIONS {
            return Err(PdfError::TooComplex);
        }
        if !seen.try_insert(offset)? {
            return Err(PdfError::Malformed);
        }
        let trailer = section(source, offset, &mut xref)?;
        next = offset_entry(&trailer, b"Prev");
        try_push(&mut trailers, trailer)?;
    }
    xref.trailer = Some(merged(trailers.into_iter().rev())?);
    Ok(xref)
}

/// The trailer `trailers`, the oldest first, make together: the newest value
/// of each key, merged once however many there are.
fn merged(trailers: impl Iterator<Item = Dict>) -> Result<Dict, PdfError> {
    let mut entries = Vec::new();
    for trailer in trailers {
        let trailer = trailer.into_entries();
        entries.try_reserve(trailer.len())?;
        entries.extend(trailer);
    }
    Dict::from_written(entries)
}

/// The offset the last `startxref` names.
fn startxref(file: &[u8]) -> Result<usize, PdfError> {
    let tail = file.len().saturating_sub(STARTXREF_WINDOW);
    let at = parse::rfind(&file[tail..], b"startxref").ok_or(PdfError::Malformed)? + tail;
    match Lexer::new(file, at + b"startxref".len()).next()? {
        Some(Token::Integer(offset)) => file_offset(file, offset),
        _ => Err(PdfError::Malformed),
    }
}

/// How far from the end of the file `startxref` is looked for.
const STARTXREF_WINDOW: usize = 4096;

/// `value` as an offset within `file`.
fn file_offset(file: &[u8], value: i64) -> Result<usize, PdfError> {
    usize::try_from(value)
        .ok()
        .filter(|&offset| offset < file.len())
        .ok_or(PdfError::Malformed)
}

/// The offset `dict`'s entry `key` states.
fn offset_entry(dict: &Dict, key: &[u8]) -> Option<usize> {
    dict.get(key)
        .and_then(Object::as_int)
        .and_then(|at| usize::try_from(at).ok())
}

/// The section at `offset` — a table and its trailer, or a cross-reference
/// stream — its entries added to `xref`. Answers its trailer.
fn section(source: Source<'_>, offset: usize, xref: &mut Xref) -> Result<Dict, PdfError> {
    let mut lexer = Lexer::new(source.file, offset);
    if lexer.next()? != Some(Token::Keyword(b"xref")) {
        return stream_section(source, offset, xref);
    }
    let freed = table(&mut lexer, xref)?;
    if lexer.next()? != Some(Token::Keyword(b"trailer")) || lexer.next()? != Some(Token::DictOpen) {
        return Err(PdfError::Malformed);
    }
    let trailer = parse::dict_body(&mut lexer, 0)?;
    // A hybrid file's stream places what its table does not, the objects in
    // object streams among them, which the table marks free for readers that
    // predate them; so the table's free entries yield to it.
    if let Some(stream) = offset_entry(&trailer, b"XRefStm") {
        stream_section(source, stream, xref)?;
    }
    for num in freed {
        xref.define(num, Entry::Free, None)?;
    }
    Ok(trailer)
}

/// A table's subsections, from after `xref` up to `trailer`: the objects in
/// use placed in `xref`, and the numbers of those free answered.
fn table(lexer: &mut Lexer<'_>, xref: &mut Xref) -> Result<Vec<u32>, PdfError> {
    let mut freed = Vec::new();
    loop {
        let before = *lexer;
        let first = match lexer.next()? {
            Some(Token::Integer(first)) => object_number(first)?,
            Some(Token::Keyword(b"trailer")) => {
                *lexer = before;
                return Ok(freed);
            }
            _ => return Err(PdfError::Malformed),
        };
        let Some(Token::Integer(count)) = lexer.next()? else {
            return Err(PdfError::Malformed);
        };
        for num in subsection(first, count)? {
            let (Some(Token::Integer(offset)), Some(Token::Integer(_)), Some(Token::Keyword(kind))) =
                (lexer.next()?, lexer.next()?, lexer.next()?)
            else {
                return Err(PdfError::Malformed);
            };
            match kind {
                b"n" => {
                    let offset = usize::try_from(offset).map_err(|_| PdfError::Malformed)?;
                    xref.define(num, Entry::Offset(offset), None)?;
                }
                b"f" => try_push(&mut freed, num)?,
                _ => return Err(PdfError::Malformed),
            }
        }
    }
}

/// The object numbers of the subsection of `count` from `first`: past the
/// most a file may number, a table is damaged and the file is rebuilt.
fn subsection(first: u32, count: i64) -> Result<core::ops::Range<u32>, PdfError> {
    let count = u32::try_from(count).map_err(|_| PdfError::Malformed)?;
    match first.checked_add(count) {
        Some(end) if end <= MAX_OBJECTS + 1 => Ok(first..end),
        _ => Err(PdfError::Malformed),
    }
}

/// `value` as an object number.
pub(crate) fn object_number(value: i64) -> Result<u32, PdfError> {
    u32::try_from(value)
        .ok()
        .filter(|&num| num <= MAX_OBJECTS)
        .ok_or(PdfError::Malformed)
}

/// The cross-reference stream at `offset`, its entries added to `xref`.
/// Answers its dictionary, which is its section's trailer.
fn stream_section(source: Source<'_>, offset: usize, xref: &mut Xref) -> Result<Dict, PdfError> {
    let Source { file, budget, ends } = source;
    let (_, Indirect::Stream { dict, data }) = parse::indirect(&mut Lexer::new(file, offset))?
    else {
        return Err(PdfError::Malformed);
    };
    if !dict.is_type(b"XRef") {
        return Err(PdfError::Malformed);
    }
    let length = offset_entry(&dict, b"Length");
    let (start, end) = parse::stream_span(file, data, length, ends)?.ok_or(PdfError::Malformed)?;
    let decoded = decode_direct(&file[start..end], &dict, budget)?;
    let widths = field_widths(&dict)?;
    let row = widths.iter().sum::<usize>();
    if row == 0 {
        return Err(PdfError::Malformed);
    }
    let mut rows = decoded.bytes.chunks_exact(row);
    for (first, count) in stream_ranges(&dict)? {
        for num in subsection(first, count)? {
            let Some(row) = rows.next() else {
                return Ok(dict);
            };
            let (kind, rest) = row.split_at(widths[0]);
            let (field, last) = rest.split_at(widths[1]);
            // An absent type field means type 1.
            let kind = if widths[0] == 0 { 1 } else { big_endian(kind) };
            let entry = match kind {
                1 => Entry::Offset(
                    usize::try_from(big_endian(field)).map_err(|_| PdfError::Malformed)?,
                ),
                2 => Entry::Compressed {
                    stream: object_number(
                        i64::try_from(big_endian(field)).map_err(|_| PdfError::Malformed)?,
                    )?,
                    index: u32::try_from(big_endian(last)).map_err(|_| PdfError::Malformed)?,
                },
                // Type 0 is free, and any other type a reference to the null
                // object.
                _ => Entry::Free,
            };
            xref.define(num, entry, Some(budget))?;
        }
    }
    Ok(dict)
}

/// The byte widths of a cross-reference stream's three fields, `/W`.
fn field_widths(dict: &Dict) -> Result<[usize; 3], PdfError> {
    let widths = dict
        .get(b"W")
        .and_then(Object::as_array)
        .ok_or(PdfError::Malformed)?;
    let [kind, field, last] = widths else {
        return Err(PdfError::Malformed);
    };
    let width = |width: &Object| {
        width
            .as_int()
            .and_then(|width| usize::try_from(width).ok())
            .filter(|&width| width <= 8)
            .ok_or(PdfError::Malformed)
    };
    Ok([width(kind)?, width(field)?, width(last)?])
}

/// The subsections a cross-reference stream's rows describe: `/Index`, or
/// every object below `/Size`.
fn stream_ranges(dict: &Dict) -> Result<Vec<(u32, i64)>, PdfError> {
    let mut ranges = Vec::new();
    if let Some(index) = dict.get(b"Index").and_then(Object::as_array) {
        for pair in index.chunks(2) {
            let [first, count] = pair else {
                return Err(PdfError::Malformed);
            };
            let first = object_number(first.as_int().ok_or(PdfError::Malformed)?)?;
            try_push(
                &mut ranges,
                (first, count.as_int().ok_or(PdfError::Malformed)?),
            )?;
        }
    } else {
        let size = dict
            .get(b"Size")
            .and_then(Object::as_int)
            .ok_or(PdfError::Malformed)?;
        try_push(&mut ranges, (0, size))?;
    }
    Ok(ranges)
}

/// `bytes` read as a big-endian number.
fn big_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0, |value, &byte| value << 8 | u64::from(byte))
}

/// `data` decoded through the filters `dict` names directly, which is all a
/// cross-reference stream may use: it is never encrypted, and its entries are
/// never indirect.
fn decode_direct(data: &[u8], dict: &Dict, budget: &Cell<u64>) -> Result<Decoded, PdfError> {
    let chain = filter::chain(dict.get(b"Filter"), dict.get(b"DecodeParms"))?;
    let decoded = filter::decode(alloc::borrow::Cow::Borrowed(data), &chain, budget)?;
    if decoded.image.is_some() {
        return Err(PdfError::Malformed);
    }
    Ok(decoded)
}

/// An object stream's decoded bytes and where each of its objects begins.
pub(crate) struct ObjectStream {
    data: Vec<u8>,
    objects: Vec<(u32, usize)>,
    /// Its objects' places in `objects`, by object number: how one is found
    /// where a damaged cross-reference has its index wrong.
    by_number: Vec<u32>,
}

impl ObjectStream {
    /// The object stream of dictionary `dict` whose bytes decoded to `data`,
    /// the index it keeps of them charged to `budget`, as its bytes were.
    pub(crate) fn new(dict: &Dict, data: Vec<u8>, budget: &Cell<u64>) -> Result<Self, PdfError> {
        let count = dict
            .get(b"N")
            .and_then(Object::as_int)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or(PdfError::Malformed)?;
        if count > MAX_OBJECT_STREAM_OBJECTS {
            return Err(PdfError::TooComplex);
        }
        let first = offset_entry(dict, b"First")
            .filter(|&first| first <= data.len())
            .ok_or(PdfError::Malformed)?;
        // Each object's place, by order and by number.
        filter::charge(
            budget,
            count
                .saturating_mul(core::mem::size_of::<(u32, usize)>() + core::mem::size_of::<u32>()),
        )?;
        let mut objects = Vec::new();
        objects.try_reserve_exact(count)?;
        let mut lexer = Lexer::new(&data[..first], 0);
        for _ in 0..count {
            let (Some(Token::Integer(num)), Some(Token::Integer(at))) =
                (lexer.next()?, lexer.next()?)
            else {
                return Err(PdfError::Malformed);
            };
            let at = usize::try_from(at)
                .ok()
                .and_then(|at| first.checked_add(at))
                .filter(|&at| at < data.len())
                .ok_or(PdfError::Malformed)?;
            objects.push((object_number(num)?, at));
        }
        let mut by_number =
            fallible::collected(objects.len(), 0u32..).ok_or(PdfError::OutOfMemory)?;
        by_number.sort_unstable_by_key(|&place| (objects[place as usize].0, place));
        Ok(Self {
            data,
            objects,
            by_number,
        })
    }

    /// Object `num`, which the cross-reference places `index`th; found by its
    /// number where a damaged cross-reference has the index wrong, the first
    /// of that number then. What reading it lexes is charged to `lexing`.
    pub(crate) fn object(
        &self,
        index: u32,
        num: u32,
        lexing: &Cell<u64>,
    ) -> Result<Object, PdfError> {
        let listed = usize::try_from(index)
            .ok()
            .and_then(|index| self.objects.get(index))
            .filter(|&&(held, _)| held == num);
        let found = || {
            let first = self
                .by_number
                .partition_point(|&place| self.objects[place as usize].0 < num);
            self.by_number
                .get(first)
                .map(|&place| &self.objects[place as usize])
                .filter(|&&(held, _)| held == num)
        };
        let &(_, at) = listed.or_else(found).ok_or(PdfError::Malformed)?;
        parse::object(&mut Lexer::charged(&self.data, at, lexing), 0)
    }

    /// The object numbers it holds, in order.
    fn numbers(&self) -> impl Iterator<Item = u32> + '_ {
        self.objects.iter().map(|&(num, _)| num)
    }
}

/// What a repair may still lex.
struct Lexing(usize);

impl Lexing {
    fn of(file: &[u8]) -> Self {
        Self(file.len().saturating_mul(MAX_LEX_PASSES))
    }

    /// Spend `bytes`.
    fn spend(&mut self, bytes: usize) -> Result<(), PdfError> {
        self.0 = self.0.checked_sub(bytes).ok_or(PdfError::TooComplex)?;
        Ok(())
    }
}

/// The cross-reference rebuilt by scanning `file`: every `N G obj` it holds
/// outside an object read before it, the last of a number winning, the
/// object streams among them left to be indexed, and for its trailer every
/// trailer and cross-reference stream naming what a trailer does, merged as
/// a chain of updates is.
pub(crate) fn repair(file: &[u8]) -> Result<Xref, PdfError> {
    let mut lexing = Lexing::of(file);
    let ends = Endstreams::default();
    let mut direct = OrderedMap::new();
    let mut trailers = Vec::new();
    let mut encryption = None;
    let (mut at, mut covered) = (0, 0);
    while let Some(found) = parse::find(&file[at..], b"obj") {
        let keyword = at + found;
        at = keyword + b"obj".len();
        if file.get(at).is_some_and(|&byte| is_regular(byte)) {
            continue;
        }
        let Some((num, start)) = object_head(file, keyword) else {
            continue;
        };
        // A head within an object read already is that object's bytes.
        if start < covered {
            continue;
        }
        let (read, stopped) = dict_at(file, start);
        lexing.spend(stopped - start)?;
        let mut object_stream = false;
        match read? {
            Some((dict, true)) => {
                covered = ends.at_or_after(file, stopped)?.unwrap_or(stopped);
                object_stream = dict.is_type(b"ObjStm");
                // A cross-reference stream is its section's trailer.
                if dict.is_type(b"XRef") && is_trailer(&dict) {
                    try_push(&mut trailers, (start, dict))?;
                }
            }
            Some((dict, false)) => {
                covered = stopped;
                if is_encryption(&dict) {
                    encryption = Some((num, start));
                }
            }
            None => {}
        }
        direct.try_insert(num, (start, object_stream))?;
    }
    trailers_in(file, &mut lexing, &mut trailers)?;
    let mut xref = Xref::default();
    for (&num, &(offset, object_stream)) in &direct {
        xref.entries.try_insert(num, Entry::Offset(offset))?;
        if object_stream {
            try_push(&mut xref.unindexed, num)?;
        }
    }
    trailers.sort_unstable_by_key(|&(offset, _)| offset);
    xref.trailer = if trailers.is_empty() {
        // With no trailer left, an encryption dictionary the scan found still
        // says the file is encrypted, so it is never read as plaintext.
        encryption
            .filter(|(num, start)| direct.get(num).is_some_and(|&(at, _)| at == *start))
            .map(|(num, _)| encrypted_by(num))
            .transpose()?
    } else {
        Some(merged(trailers.into_iter().map(|(_, dict)| dict))?)
    };
    Ok(xref)
}

/// The object number and offset of the indirect object whose `obj` keyword
/// is at `keyword`, where a number and a generation precede it.
fn object_head(file: &[u8], keyword: usize) -> Option<(u32, usize)> {
    let (_, gen_start) = number_before(file, keyword)?;
    let (num, num_start) = number_before(file, gen_start)?;
    if num_start > 0 && is_regular(file[num_start - 1]) {
        return None;
    }
    Some((object_number(i64::try_from(num).ok()?).ok()?, num_start))
}

/// The digits most a number before `obj` is spelled with: an object number
/// has seven, leading zeros aside.
const MOST_HEAD_DIGITS: usize = 10;

/// The number that ends, before white space, at `end`, and where it begins.
fn number_before(file: &[u8], end: usize) -> Option<(u64, usize)> {
    let mut at = end;
    while at > 0 && is_white(file[at - 1]) {
        at -= 1;
    }
    let digits_end = at;
    if digits_end == end {
        return None;
    }
    while at > 0 && file[at - 1].is_ascii_digit() && digits_end - at < MOST_HEAD_DIGITS {
        at -= 1;
    }
    if at == digits_end || (at > 0 && file[at - 1].is_ascii_digit()) {
        return None;
    }
    let value = file[at..digits_end]
        .iter()
        .fold(0u64, |value, &digit| value * 10 + u64::from(digit - b'0'));
    Some((value, at))
}

/// The dictionary of the indirect object at `offset`, if it is one, and
/// whether a stream follows it; and where reading it stopped. What does not
/// parse is no dictionary; only a bound passed or the heap's refusal is an
/// error, since passing over those would read a different file.
fn dict_at(file: &[u8], offset: usize) -> (Result<Option<(Dict, bool)>, PdfError>, usize) {
    let mut lexer = Lexer::new(file, offset);
    let read = (|| {
        for _ in 0..3 {
            if tolerated(lexer.next())?.is_none() {
                return Ok(None);
            }
        }
        if tolerated(lexer.next())?.flatten() != Some(Token::DictOpen) {
            return Ok(None);
        }
        let Some(dict) = tolerated(parse::dict_body(&mut lexer, 0))? else {
            return Ok(None);
        };
        let is_stream = tolerated(lexer.next())?.flatten() == Some(Token::Keyword(b"stream"));
        Ok(Some((dict, is_stream)))
    })();
    (read, lexer.at())
}

/// Every `trailer` dictionary in `file` naming what a trailer does, each with
/// where it is, onto `found`: each read from its keyword on, those within
/// one read already passed over, and what is lexed spent from `lexing`.
fn trailers_in(
    file: &[u8],
    lexing: &mut Lexing,
    found: &mut Vec<(usize, Dict)>,
) -> Result<(), PdfError> {
    let (mut at, mut covered) = (0, 0);
    while let Some(offset) = parse::find(&file[at..], b"trailer") {
        let keyword = at + offset;
        at = keyword + b"trailer".len();
        if keyword < covered {
            continue;
        }
        let mut lexer = Lexer::new(file, at);
        let read = match tolerated(lexer.next()) {
            Ok(Some(Some(Token::DictOpen))) => tolerated(parse::dict_body(&mut lexer, 0)),
            Ok(_) => Ok(None),
            Err(error) => Err(error),
        };
        // Charged whatever follows the keyword, so a token run that never
        // opens a dictionary is not lexed again for free at every keyword in it.
        lexing.spend(lexer.at() - at)?;
        let Some(dict) = read? else {
            continue;
        };
        covered = lexer.at();
        if is_trailer(&dict) {
            try_push(found, (keyword, dict))?;
        }
    }
    Ok(())
}

/// Whether `dict`, a trailer or a cross-reference stream's, names what a
/// repair takes from one: the catalog, or the encryption dictionary.
fn is_trailer(dict: &Dict) -> bool {
    dict.get(b"Root").and_then(Object::as_reference).is_some() || dict.get(b"Encrypt").is_some()
}

/// Whether `dict` is an encryption dictionary (ISO 32000-2 §7.6.2): it names
/// a security handler and holds what the standard handler's or a public-key
/// handler's must.
fn is_encryption(dict: &Dict) -> bool {
    let holds = |key: &[u8]| dict.get(key).is_some();
    dict.name(b"Filter").is_some()
        && ((holds(b"O") && holds(b"U")) || holds(b"Recipients") || holds(b"CF"))
}

/// A trailer naming object `num` the file's encryption dictionary.
fn encrypted_by(num: u32) -> Result<Dict, PdfError> {
    let mut entries = Vec::new();
    try_push(
        &mut entries,
        (Name::of(b"Encrypt")?, Object::Ref(ObjRef { num, gen: 0 })),
    )?;
    Dict::from_written(entries)
}

#[cfg(test)]
#[path = "xref_tests.rs"]
mod tests;
