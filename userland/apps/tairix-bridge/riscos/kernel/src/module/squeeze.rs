//! Squeezed modules, expanded as genuine RISC OS 5.30 is observed to expand
//! them (`tests/corpus/squeeze.scenarios`).
//!
//! A squeezed module keeps its header whole, its initialisation word's top
//! bit set and its low bits the squeezed length. Five words end that length:
//! the expanded size, then the bytes of codes, the bytes of tables, and the
//! entries in the short and the long table of common words.
//!
//! - The tables lie just before the five words, read forwards, short table
//!   first. Each entry is a delta from the one before it, the first from
//!   -1: code 0 and the whole delta (four bytes for the short table, three
//!   for the long), 1 to 9 that many entries each one past the last, 10 to 91
//!   the code less 10, 92 to 173 a byte more, 174 and up two bytes more.
//! - The codes lie just before the tables, read backwards: a byte whose low
//!   nibble codes the first of two words and whose high nibble the second,
//!   then each word's bytes. Nibble 0 is a zero word; 1 four bytes, lowest
//!   first; 2 to 8 a long-table entry, by the nibble and one more byte, above
//!   a low byte; 9 to 15 a short-table entry by the nibble and one more byte.
//!   The pairs fill the module from its end down.
//!
//! Codes that stop short of the expanded size make the module. A trailer off
//! its word, an expanded size no multiple of eight, codes reaching past their
//! bytes or past the module's start and an entry no table holds are refused,
//! and so is a table longer than its nibbles can name, which bounds what one
//! costs.

use alloc::vec::Vec;

/// An initialisation word's bit marking a squeezed module.
const SQUEEZED: u32 = 1 << 31;
/// The five words that end a squeezed module.
const TRAILER: usize = 20;
/// The first word the codes may lie in: past the start and length words.
const CODES_FROM: usize = 8;
/// The most entries a table's nibbles and index byte can name.
const TABLE_MOST: usize = 7 << 8;

/// Why a squeezed module could not be expanded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsqueezed {
    /// Its squeezed length reaches past the module given.
    PastItsEnd,
    /// Its sizes, tables or codes do not describe a module.
    Malformed,
    /// It expands past the size the caller allows, or past what memory holds.
    TooLarge,
}

/// Whether `module`'s initialisation word marks it squeezed.
#[must_use]
pub fn is_squeezed(module: &[u8]) -> bool {
    word(module, 4).is_some_and(|init| init & SQUEEZED != 0)
}

/// Squeezed `module` expanded, at most `most` bytes long.
pub fn unsqueeze(module: &[u8], most: usize) -> Result<Vec<u8>, Unsqueezed> {
    let length = word(module, 4).ok_or(Unsqueezed::Malformed)? & !SQUEEZED;
    let length = usize::try_from(length).map_err(|_| Unsqueezed::PastItsEnd)?;
    if length > module.len() {
        return Err(Unsqueezed::PastItsEnd);
    }
    if !length.is_multiple_of(4) || length < CODES_FROM + TRAILER {
        return Err(Unsqueezed::Malformed);
    }
    let trailer = length - TRAILER;
    let field = |index: usize| {
        word(module, trailer + 4 * index)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(Unsqueezed::Malformed)
    };
    let (expanded, codes, tables) = (field(0)?, field(1)?, field(2)?);
    let (shorts, longs) = (field(3)?, field(4)?);
    if expanded > most {
        return Err(Unsqueezed::TooLarge);
    }
    if !expanded.is_multiple_of(8) || shorts > TABLE_MOST || longs > TABLE_MOST {
        return Err(Unsqueezed::Malformed);
    }
    let tables_from = trailer.checked_sub(tables).ok_or(Unsqueezed::Malformed)?;
    let codes_from = tables_from
        .checked_sub(codes)
        .filter(|&from| from >= CODES_FROM)
        .ok_or(Unsqueezed::Malformed)?;
    let mut entries = Forwards {
        bytes: &module[tables_from..trailer],
        at: 0,
    };
    let short = entries.table(shorts, 4)?;
    let long = entries.table(longs, 3)?;
    let mut out = Vec::new();
    out.try_reserve_exact(expanded)
        .map_err(|_| Unsqueezed::TooLarge)?;
    out.resize(expanded, 0);
    let mut codes = Backwards {
        bytes: &module[codes_from..tables_from],
    };
    let mut filled = expanded;
    while let Some(pair) = codes.byte() {
        let first = codes.word(pair & 0xF, &short, &long)?;
        let second = codes.word(pair >> 4, &short, &long)?;
        filled = filled.checked_sub(8).ok_or(Unsqueezed::Malformed)?;
        out[filled..filled + 4].copy_from_slice(&first.to_le_bytes());
        out[filled + 4..filled + 8].copy_from_slice(&second.to_le_bytes());
    }
    out.drain(..filled);
    Ok(out)
}

/// The little-endian word at `at`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// The tables' bytes, read from their start.
struct Forwards<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Forwards<'_> {
    fn byte(&mut self) -> Result<u32, Unsqueezed> {
        let byte = self.bytes.get(self.at).ok_or(Unsqueezed::Malformed)?;
        self.at += 1;
        Ok(u32::from(*byte))
    }

    /// A table of `count` entries, a whole delta `width` bytes long.
    fn table(&mut self, count: usize, width: u32) -> Result<Vec<u32>, Unsqueezed> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| Unsqueezed::TooLarge)?;
        let mut last = u32::MAX;
        while entries.len() < count {
            let code = self.byte()?;
            let delta = match code {
                0 => {
                    let mut delta = 0;
                    for shift in (0..width).map(|byte| 8 * byte) {
                        delta |= self.byte()? << shift;
                    }
                    delta
                }
                1..=9 => {
                    for _ in 0..code {
                        if entries.len() == count {
                            break;
                        }
                        last = last.wrapping_add(1);
                        entries.push(last);
                    }
                    continue;
                }
                10..=91 => code - 10,
                92..=173 => (code - 92) << 8 | self.byte()?,
                _ => {
                    let low = self.byte()?;
                    (code - 174) << 16 | self.byte()? << 8 | low
                }
            };
            last = last.wrapping_add(delta);
            entries.push(last);
        }
        Ok(entries)
    }
}

/// The codes' bytes, read from their end.
struct Backwards<'a> {
    bytes: &'a [u8],
}

impl Backwards<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&last, rest) = self.bytes.split_last()?;
        self.bytes = rest;
        Some(last)
    }

    fn next(&mut self) -> Result<u32, Unsqueezed> {
        self.byte().map(u32::from).ok_or(Unsqueezed::Malformed)
    }

    /// The word `nibble` codes.
    fn word(&mut self, nibble: u8, short: &[u32], long: &[u32]) -> Result<u32, Unsqueezed> {
        let entry = |table: &[u32], page: u8, index: u32| {
            table
                .get(usize::from(page) << 8 | index as usize)
                .copied()
                .ok_or(Unsqueezed::Malformed)
        };
        Ok(match nibble {
            0 => 0,
            1 => {
                let mut value = 0;
                for shift in [0, 8, 16, 24] {
                    value |= self.next()? << shift;
                }
                value
            }
            2..=8 => {
                let high = entry(long, nibble - 2, self.next()?)?;
                high << 8 | self.next()?
            }
            _ => entry(short, nibble - 9, self.next()?)?,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
