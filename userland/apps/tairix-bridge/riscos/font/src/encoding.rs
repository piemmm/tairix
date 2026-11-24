//! Encoding files read, and a font's codes mapped to its glyphs through
//! them, as the UCS Font Manager reads them. A file names a glyph for each
//! code, as a run of `/name`s, one a code from 0, `%` opening a comment to
//! the line's end; or as the Adobe Glyph List's lines, `[XX;]XXXX;name;…`,
//! each its code in hexadecimal, `#` opening a comment line. A target
//! encoding's names are found in the base encoding, whose codes are the
//! font's glyphs in its files' order. `.notdef` names no glyph.

use alloc::vec::Vec;
use core::mem::size_of;

use crate::glyph_names::GlyphNames;
use crate::rom_encodings::BASE0;

/// The codes the map holds a table for; past them, a sorted list.
const LOW: usize = 256;
/// What a table entry holds for a code no glyph is.
const NONE: u32 = u32::MAX;
/// What the PRM names a code no glyph is.
pub(crate) const NOTDEF: &str = ".notdef";
/// The fewest digits an Adobe Glyph List line's code has; a shorter first
/// field comes before it.
const CODE_DIGITS: usize = 4;
/// The largest code a line may give.
const CODE_MOST: u32 = 0x7FFF_FFFF;
/// Unicode's private use area, where UTF-8 numbers RISC OS's own glyphs:
/// those whose names the glyph lists do not know, one after another from its
/// start in the base encoding's order.
pub(crate) const PRIVATE_USE: core::ops::RangeInclusive<u32> = 0xE000..=0xF8FF;

/// A code and where its name is in the file.
#[derive(Clone, Copy, Debug)]
struct Entry {
    code: u32,
    start: u32,
    end: u32,
}

/// An encoding file, read.
#[derive(Clone, Debug, Default)]
pub(crate) struct EncodingFile {
    bytes: Vec<u8>,
    /// Each code that names a glyph, in the file's order.
    entries: Vec<Entry>,
}

impl EncodingFile {
    /// The file `bytes`.
    pub(crate) fn read(bytes: Vec<u8>) -> Self {
        let mut entries = Vec::new();
        let mut position = 0u32;
        let mut at = 0;
        while at < bytes.len() {
            let end = bytes[at..]
                .iter()
                .position(|&b| b == b'\n' || b == b'\r')
                .map_or(bytes.len(), |e| at + e);
            let line = &bytes[at..end];
            let first = line.iter().position(|&b| b != b' ' && b != b'\t');
            match first.map(|f| line[f]) {
                Some(b'#') | None => {}
                Some(b) if b.is_ascii_hexdigit() && line.contains(&b';') => {
                    if let Some(entry) = glyph_list_line(line, at) {
                        entries.push(entry);
                    }
                }
                Some(_) => names_line(line, at, &mut position, &mut entries),
            }
            at = end + 1;
        }
        let notdef = NOTDEF.as_bytes();
        entries.retain(|entry| &bytes[entry.start as usize..entry.end as usize] != notdef);
        Self { bytes, entries }
    }

    fn name(&self, entry: Entry) -> &[u8] {
        &self.bytes[entry.start as usize..entry.end as usize]
    }

    /// Each code that names a glyph and the name, in the file's order.
    pub(crate) fn names(&self) -> impl Iterator<Item = (u32, &[u8])> {
        self.entries
            .iter()
            .map(|&entry| (entry.code, self.name(entry)))
    }
}

/// A run of `/name`s from `line`, which starts `at` in the file, each the
/// next position's; `%` ends the run.
fn names_line(line: &[u8], at: usize, position: &mut u32, entries: &mut Vec<Entry>) {
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'%' => return,
            b'/' => {
                let start = i + 1;
                let end = line[start..]
                    .iter()
                    .position(|&b| b <= b' ' || b == b'/' || b == b'%')
                    .map_or(line.len(), |e| start + e);
                if let (Ok(s), Ok(e)) = (u32::try_from(at + start), u32::try_from(at + end)) {
                    entries.push(Entry {
                        code: *position,
                        start: s,
                        end: e,
                    });
                }
                *position = position.saturating_add(1);
                i = end;
            }
            _ => i += 1,
        }
    }
}

/// An Adobe Glyph List line, which starts `at` in the file: its code and
/// name, a first field shorter than a code's passed over.
fn glyph_list_line(line: &[u8], at: usize) -> Option<Entry> {
    let mut fields = fields(line);
    let mut code = fields.next()?;
    if code.len() < CODE_DIGITS {
        code = fields.next()?;
    }
    let name = fields.next()?;
    let code = &line[code];
    if name.is_empty() || !code.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let code = u32::from_str_radix(core::str::from_utf8(code).ok()?, 16)
        .ok()
        .filter(|&c| c <= CODE_MOST)?;
    Some(Entry {
        code,
        start: u32::try_from(at + name.start).ok()?,
        end: u32::try_from(at + name.end).ok()?,
    })
}

/// Where each of `line`'s `;`-separated fields is, spaces either side left
/// off.
fn fields(line: &[u8]) -> impl Iterator<Item = core::ops::Range<usize>> + '_ {
    let mut start = 0;
    core::iter::from_fn(move || {
        if start > line.len() {
            return None;
        }
        let end = line[start..]
            .iter()
            .position(|&b| b == b';')
            .map_or(line.len(), |e| start + e);
        let mut field = start..end;
        start = end + 1;
        while field.start < field.end && line[field.start].is_ascii_whitespace() {
            field.start += 1;
        }
        while field.end > field.start && line[field.end - 1].is_ascii_whitespace() {
            field.end -= 1;
        }
        Some(field)
    })
}

/// A font's codes mapped to the glyphs its files hold them at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeMap {
    low: [u32; LOW],
    /// The codes past the table and their glyphs, by code.
    high: Vec<(u32, u32)>,
    /// One past the last code mapped.
    len: u32,
}

impl CodeMap {
    /// The map `pairs` give, the first pair for a code its glyph.
    fn of(pairs: impl Iterator<Item = (u32, u32)>) -> Self {
        let mut low = [NONE; LOW];
        let mut high = Vec::new();
        for (code, glyph) in pairs {
            match usize::try_from(code).ok().and_then(|c| low.get_mut(c)) {
                Some(slot) if *slot == NONE => *slot = glyph,
                Some(_) => {}
                None => high.push((code, glyph)),
            }
        }
        high.sort_by_key(|&(code, _)| code);
        high.dedup_by_key(|&mut (code, _)| code);
        let len = high.last().map_or_else(
            || {
                (1u32..)
                    .zip(&low)
                    .filter(|&(_, &glyph)| glyph != NONE)
                    .last()
                    .map_or(0, |(end, _)| end)
            },
            |&(code, _)| code + 1,
        );
        Self { low, high, len }
    }

    /// Each of `target`'s codes mapped to the glyph `base` gives its name.
    pub(crate) fn based(target: &EncodingFile, base: &EncodingFile) -> Self {
        let mut index: Vec<(&[u8], u32)> = base.names().map(|(code, name)| (name, code)).collect();
        index.sort_by(|a, b| a.0.cmp(b.0));
        index.dedup_by_key(|entry| entry.0);
        Self::of(target.names().filter_map(|(code, name)| {
            let found = index.binary_search_by(|entry| entry.0.cmp(name)).ok()?;
            Some((code, index[found].1))
        }))
    }

    /// UTF-8's map of `base`'s glyphs: each the characters its name stands
    /// for, a glyph whose name the lists do not know the next character of
    /// the private use area, then those RISC OS's own name table gives a
    /// glyph it reaches from more than one, where no glyph's name has them.
    /// A name opening with a full stop names no character.
    pub(crate) fn utf8(base: &EncodingFile, names: &GlyphNames) -> Self {
        let mut unnamed = *PRIVATE_USE.start();
        let mut direct: Vec<(u32, u32)> = Vec::new();
        for (glyph, name) in base.names().filter(|(_, name)| name.first() != Some(&b'.')) {
            let characters = names.characters(name);
            if characters.is_empty() {
                direct.push((unnamed, glyph));
                unnamed = unnamed.saturating_add(1);
            }
            direct.extend(characters.into_iter().map(|c| (c, glyph)));
        }
        let doubled: Vec<(u32, u32)> = direct
            .iter()
            .flat_map(|&(character, glyph)| doubles(character).map(move |c| (c, glyph)))
            .collect();
        Self::of(direct.into_iter().chain(doubled))
    }

    /// The glyph `code` shows; none for a code no glyph is.
    pub(crate) fn glyph(&self, code: u32) -> Option<u32> {
        match usize::try_from(code).ok().and_then(|c| self.low.get(c)) {
            Some(&glyph) => Some(glyph).filter(|&g| g != NONE),
            None => self
                .high
                .binary_search_by_key(&code, |&(c, _)| c)
                .ok()
                .map(|at| self.high[at].1),
        }
    }

    /// The first code past `code` that shows a glyph.
    pub(crate) fn next(&self, code: u32) -> Option<u32> {
        let from = code.saturating_add(1);
        if let Some((found, _)) = (0u32..)
            .zip(&self.low)
            .skip_while(|&(c, _)| c < from)
            .find(|&(_, &glyph)| glyph != NONE)
        {
            return Some(found);
        }
        let at = self.high.partition_point(|&(c, _)| c < from);
        self.high.get(at).map(|&(c, _)| c)
    }

    /// One past the last code that shows a glyph.
    pub(crate) const fn len(&self) -> u32 {
        self.len
    }

    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>() + self.high.len() * size_of::<(u32, u32)>()
    }

    /// Each code that shows a glyph below `limit`, and the glyph.
    pub(crate) fn below(&self, limit: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        let low = (0u32..)
            .zip(&self.low)
            .take_while(move |&(code, _)| code < limit)
            .filter(|&(_, &glyph)| glyph != NONE)
            .map(|(code, &glyph)| (code, glyph));
        low.chain(
            self.high
                .iter()
                .copied()
                .take_while(move |&(code, _)| code < limit),
        )
    }
}

/// The further characters RISC OS's own name table gives the glyph that
/// `character` first names: those base encoding 0 reaches it from beyond
/// its first.
fn doubles(character: u32) -> impl Iterator<Item = u32> {
    BASE0
        .iter()
        .filter(move |characters| characters.first() == Some(&character))
        .flat_map(|characters| characters[1..].iter().copied())
}

#[cfg(test)]
mod tests;
