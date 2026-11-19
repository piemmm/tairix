//! The defect lists a hard disc's boot block opens with: the byte addresses
//! of defective sectors in the disc's first 512 MB, then, on a big disc, the
//! sector numbers of those past it. Each list ends with a word carrying the
//! check byte of the words before it.
//!
//! Reference: the RISC OS PRM's `FileCore` chapters (volumes 2 and 5a).

use crate::record::{boot_block_checksum, BOOT_BLOCK_SIZE, DISC_RECORD_IN_BOOT_BLOCK};
use crate::{Fault, Structure};

/// What ends the first list, and what every address it holds stays below.
pub const FIRST_END: u32 = 0x2000_0000;
/// What ends a big disc's second list.
pub const SECOND_END: u32 = 0x4000_0000;
/// The words the lists share, ahead of the boot block's disc record.
const WORDS: usize = DISC_RECORD_IN_BOOT_BLOCK / 4;
/// What a list's end carries above its check byte.
const END_MASK: u32 = !0xFF;
/// What a list that fails its checks is.
const BROKEN: Fault = Fault::Broken(Structure::DefectList);

/// A list's check byte: each word exclusive-ored into the running value
/// turned right 13 bits, the result's four bytes then exclusive-ored.
#[must_use]
pub fn check_byte(words: &[u32]) -> u8 {
    let sum = words
        .iter()
        .fold(0u32, |sum, &word| sum.rotate_right(13) ^ word);
    sum.to_le_bytes().iter().fold(0, |byte, &b| byte ^ b)
}

/// A boot block's defect lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Defects {
    /// The first list's addresses, then the second list's sector numbers.
    words: [u32; WORDS],
    first: usize,
    /// The second list's length, on a big disc.
    second: Option<usize>,
}

impl Defects {
    /// No defect recorded, a second list kept where `big`.
    #[must_use]
    pub const fn none(big: bool) -> Self {
        Self {
            words: [0; WORDS],
            first: 0,
            second: if big { Some(0) } else { None },
        }
    }

    /// The lists `block` opens with, a second list read where `big`.
    ///
    /// # Errors
    ///
    /// The defect list broken where a list has no end before the disc
    /// record, holds an address past the first list's reach, or ends with a
    /// check byte that fails.
    pub fn read(block: &[u8; BOOT_BLOCK_SIZE], big: bool) -> Result<Self, Fault> {
        let mut defects = Self::none(big);
        let mut words = block[..DISC_RECORD_IN_BOOT_BLOCK]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&raw| u32::from_le_bytes(raw));
        defects.first = defects.take(&mut words, 0, FIRST_END)?;
        if big {
            defects.second = Some(defects.take(&mut words, defects.first, SECOND_END)?);
        }
        Ok(defects)
    }

    /// The list `words` runs on with up to `end` and its check byte, held
    /// from `at`: its length.
    fn take(
        &mut self,
        words: &mut impl Iterator<Item = u32>,
        at: usize,
        end: u32,
    ) -> Result<usize, Fault> {
        for (len, word) in words.enumerate() {
            let held = &self.words[at..at + len];
            if word & END_MASK == end {
                return if u32::from(check_byte(held)) == word & 0xFF {
                    Ok(len)
                } else {
                    Err(BROKEN)
                };
            }
            if end == FIRST_END && word >= FIRST_END {
                return Err(BROKEN);
            }
            *self.words.get_mut(at + len).ok_or(BROKEN)? = word;
        }
        Err(BROKEN)
    }

    /// The first list's addresses.
    #[must_use]
    pub fn first(&self) -> &[u32] {
        &self.words[..self.first]
    }

    /// The second list's sector numbers; none on a disc that is not big.
    #[must_use]
    pub fn second(&self) -> &[u32] {
        &self.words[self.first..self.first + self.second.unwrap_or(0)]
    }

    /// `address` added to the first list, after every address below it:
    /// `false` where the list holds it already.
    ///
    /// # Errors
    ///
    /// * [`Fault::BadDefect`] for an address the first list cannot hold.
    /// * [`Fault::TooManyDefects`] where the lists already fill the words
    ///   before the disc record.
    pub fn insert(&mut self, address: u32) -> Result<bool, Fault> {
        if address >= FIRST_END {
            return Err(Fault::BadDefect);
        }
        if self.first().contains(&address) {
            return Ok(false);
        }
        let used = self.first + 1 + self.second.map_or(0, |len| len + 1);
        if used >= WORDS {
            return Err(Fault::TooManyDefects);
        }
        let at = self.first().partition_point(|&held| held < address);
        let end = self.first + self.second.unwrap_or(0);
        self.words.copy_within(at..end, at + 1);
        self.words[at] = address;
        self.first += 1;
        Ok(true)
    }

    /// The lists written into `block`, the words after them cleared up to the
    /// disc record, and the block's checksum resealed.
    pub fn write(&self, block: &mut [u8; BOOT_BLOCK_SIZE]) {
        let area = &mut block[..DISC_RECORD_IN_BOOT_BLOCK];
        area.fill(0);
        let mut at = 0;
        for &word in self.first() {
            put(area, &mut at, word);
        }
        put(
            area,
            &mut at,
            FIRST_END | u32::from(check_byte(self.first())),
        );
        if self.second.is_some() {
            for &word in self.second() {
                put(area, &mut at, word);
            }
            put(
                area,
                &mut at,
                SECOND_END | u32::from(check_byte(self.second())),
            );
        }
        block[BOOT_BLOCK_SIZE - 1] = boot_block_checksum(block);
    }
}

/// `word` at byte `at` of `area`, `at` moved past it.
fn put(area: &mut [u8], at: &mut usize, word: u32) {
    if let Some(slot) = area.get_mut(*at..*at + 4) {
        slot.copy_from_slice(&word.to_le_bytes());
    }
    *at += 4;
}

#[cfg(test)]
mod tests;
