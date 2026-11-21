//! The pixels the VDU drivers write: a screen's lines in memory, the top
//! line first. Every write reaches a page's words through one lookup of the
//! page, and a span is written a word at a time as RISC OS's colour blocks
//! have it, `(old OR ora) EOR eor` under a mask.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_kernel::memory::layout::HIGH_ZERO_PAGE;
use tairix_riscos_kernel::os::{len32, Fail, Os};
use tairix_riscos_kernel::output::workspace;

/// A colour block: the OR and exclusive-OR words for each of a pattern's
/// eight rows, as zero page holds them, with the row offset the ECF origin
/// gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) rows: [[u32; 2]; 8],
    pub(crate) y_offset: u32,
}

impl Block {
    /// The block zero page holds at `offset`, one of
    /// [`workspace::FOREGROUND`], [`workspace::BACKGROUND`] and
    /// [`workspace::BACKGROUND_STORE`].
    pub(crate) fn read(os: &mut Os<'_>, offset: u32) -> Result<Self, Fail> {
        let (words, at) = os.page_words(HIGH_ZERO_PAGE + offset)?;
        let mut rows = [[0; 2]; 8];
        for (i, row) in rows.iter_mut().enumerate() {
            *row = [words[at + 2 * i], words[at + 2 * i + 1]];
        }
        let y_offset = os.read_word(HIGH_ZERO_PAGE + workspace::ECF_Y_OFFSET)?;
        Ok(Self { rows, y_offset })
    }

    /// A block of one colour word under one action throughout.
    pub(crate) const fn solid(ora: u32, eor: u32) -> Self {
        Self {
            rows: [[ora, eor]; 8],
            y_offset: 0,
        }
    }

    /// The words for the line `row` from the screen's top.
    pub(crate) const fn for_line(&self, row: u32) -> (u32, u32) {
        let index = (row.wrapping_sub(self.y_offset) & 7) as usize;
        (self.rows[index][0], self.rows[index][1])
    }
}

/// A screen in memory as the drivers write it: its lines one after
/// another from its top line's first byte, as the screen's area, mapped
/// twice, holds them wherever a hardware scroll has moved its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Canvas {
    /// The top line's first byte.
    pub(crate) start: u32,
    /// The bytes a line takes.
    pub(crate) line: u32,
    /// Pixels across and down.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The bits a pixel takes, as a power of two.
    pub(crate) log2_bpc: u32,
}

impl Canvas {
    /// The address of byte `byte` of line `row`: a screen holds at most a
    /// few mebibytes, so the sum is far within a word.
    pub(crate) const fn address(&self, row: u32, byte: u32) -> u32 {
        self.start.wrapping_add(row * self.line + byte)
    }

    /// The line `y` pixels up from the bottom is, from the top; `y` is on
    /// the screen.
    pub(crate) const fn row_of(&self, y: i32) -> u32 {
        self.height - 1 - y.cast_unsigned()
    }

    /// Writes the pixels `x0` to `x1` of line `row` as `(old OR ora) EOR eor`.
    /// The span is within the screen.
    pub(crate) fn span(
        &self,
        os: &mut Os<'_>,
        row: u32,
        x0: u32,
        x1: u32,
        ora: u32,
        eor: u32,
    ) -> Result<(), Fail> {
        let start = self.address(row, 0);
        let lead = 8 * (start & 3);
        let first = lead + (x0 << self.log2_bpc);
        let end = lead + ((x1 + 1) << self.log2_bpc);
        let mut word = first / 32;
        let last = (end - 1) / 32;
        let base = start & !3;
        while word <= last {
            let address = base.wrapping_add(4 * word);
            let (words, at) = os.page_words_mut(address)?;
            let room = (PAGE_SIZE - (address & (PAGE_SIZE - 1))) / 4;
            let here = (last - word + 1).min(room);
            for i in 0..here {
                let bit = 32 * (word + i);
                let mask = mask_between(first.saturating_sub(bit), (end - bit).min(32));
                let old = words[at + i as usize];
                words[at + i as usize] = (old & !mask) | (((old | ora) ^ eor) & mask);
            }
            word += here;
        }
        Ok(())
    }

    /// The pixel `x` of line `row`.
    pub(crate) fn pixel(&self, os: &mut Os<'_>, row: u32, x: u32) -> Result<u32, Fail> {
        let bits = x << self.log2_bpc;
        let address = self.address(row, bits / 8);
        let unit = 1u32 << self.log2_bpc;
        if unit >= 8 {
            let mut bytes = [0; 4];
            let length = (unit / 8) as usize;
            os.read_bytes(address, &mut bytes[..length])?;
            return Ok(u32::from_le_bytes(bytes));
        }
        let byte = os.read_byte(address)?;
        Ok((u32::from(byte) >> (bits % 8)) & ((1 << unit) - 1))
    }

    /// Writes `bytes` at byte `byte` of line `row`.
    pub(crate) fn put(
        &self,
        os: &mut Os<'_>,
        row: u32,
        byte: u32,
        bytes: &[u8],
    ) -> Result<(), Fail> {
        os.write_bytes(self.address(row, byte), bytes)
    }

    /// Fills bytes `byte` to `byte + length` of lines `rows` with `value`.
    pub(crate) fn fill_bytes(
        &self,
        os: &mut Os<'_>,
        rows: core::ops::Range<u32>,
        byte: u32,
        length: u32,
        value: u32,
    ) -> Result<(), Fail> {
        let pattern = value.to_le_bytes();
        let mut chunk = [0; CHUNK as usize];
        for (slot, i) in chunk.iter_mut().zip(byte % 4..) {
            *slot = pattern[(i % 4) as usize];
        }
        for row in rows {
            let mut done = 0;
            while done < length {
                let here = (length - done).min(CHUNK);
                self.put(os, row, byte + done, &chunk[..here as usize])?;
                done += here;
            }
        }
        Ok(())
    }

    /// Copies `length` bytes of `count` lines, from byte `from.1` of line
    /// `from.0` on to byte `to.1` of line `to.0` on, the line nearest the
    /// destination first, so lines that overlap are copied whole.
    pub(crate) fn copy_lines(
        &self,
        os: &mut Os<'_>,
        from: (u32, u32),
        to: (u32, u32),
        count: u32,
        length: u32,
    ) -> Result<(), Fail> {
        let mut line = Vec::new();
        if line.try_reserve_exact(length as usize).is_err() {
            return Err(os.no_memory());
        }
        line.resize(length as usize, 0);
        for i in 0..count {
            let i = if to.0 > from.0 { count - 1 - i } else { i };
            os.read_bytes(self.address(from.0 + i, from.1), &mut line)?;
            os.write_bytes(self.address(to.0 + i, to.1), &line)?;
        }
        Ok(())
    }

    /// Appends the pixels `x0` to `x1` of line `row` to `out`, each in a word
    /// of its own.
    pub(crate) fn pixels(
        &self,
        os: &mut Os<'_>,
        row: u32,
        x0: u32,
        x1: u32,
        out: &mut Vec<u32>,
    ) -> Result<(), Fail> {
        if out.try_reserve((x1 - x0 + 1) as usize).is_err() {
            return Err(os.no_memory());
        }
        let unit = 1u32 << self.log2_bpc;
        let mut chunk = [0; CHUNK as usize];
        let mut x = x0;
        while x <= x1 {
            let (first, bytes, upto) = self.chunk(x, x1 + 1, &mut chunk);
            os.read_bytes(self.address(row, first), bytes)?;
            out.extend((x..upto).map(|px| bits_at(bytes, (px << self.log2_bpc) - 8 * first, unit)));
            x = upto;
        }
        Ok(())
    }

    /// Writes `pixels`, each in a word of its own, from pixel `x0` of line
    /// `row`.
    pub(crate) fn put_pixels(
        &self,
        os: &mut Os<'_>,
        row: u32,
        x0: u32,
        pixels: &[u32],
    ) -> Result<(), Fail> {
        let unit = 1u32 << self.log2_bpc;
        let end = x0 + len32(pixels.len());
        let mut chunk = [0; CHUNK as usize];
        let mut x = x0;
        while x < end {
            let (first, bytes, upto) = self.chunk(x, end, &mut chunk);
            if unit < 8 {
                os.read_bytes(self.address(row, first), bytes)?;
            }
            let held = &pixels[(x - x0) as usize..(upto - x0) as usize];
            for (px, &pixel) in (x..upto).zip(held) {
                set_bits_at(bytes, (px << self.log2_bpc) - 8 * first, unit, pixel);
            }
            os.write_bytes(self.address(row, first), bytes)?;
            x = upto;
        }
        Ok(())
    }

    /// The bytes from pixel `x` on that one pass through `chunk` covers,
    /// short of pixel `end`: the line's byte they start at, `chunk` cut to
    /// them, and the pixel after the last they hold whole.
    fn chunk<'c>(
        &self,
        x: u32,
        end: u32,
        chunk: &'c mut [u8; CHUNK as usize],
    ) -> (u32, &'c mut [u8], u32) {
        let first = (x << self.log2_bpc) / 8;
        let last = (end << self.log2_bpc).div_ceil(8).min(first + CHUNK);
        let upto = ((8 * last) >> self.log2_bpc).min(end);
        (first, &mut chunk[..(last - first) as usize], upto)
    }
}

/// The bytes of a line read or written at a time: a whole number of any
/// pixel's bytes, so no pixel is split between two.
const CHUNK: u32 = 256;

/// A mask of the bits from `low` up to, not including, `high`, within a
/// word.
pub(crate) const fn mask_between(low: u32, high: u32) -> u32 {
    let below_high = if high >= 32 {
        u32::MAX
    } else {
        (1 << high) - 1
    };
    let below_low = if low >= 32 { u32::MAX } else { (1 << low) - 1 };
    below_high & !below_low
}

/// The `unit` bits at bit `bit` of `bytes`, lowest first.
fn bits_at(bytes: &[u8], bit: u32, unit: u32) -> u32 {
    if unit >= 8 {
        let at = (bit / 8) as usize;
        let mut word = [0; 4];
        let length = (unit / 8) as usize;
        word[..length].copy_from_slice(&bytes[at..at + length]);
        return u32::from_le_bytes(word);
    }
    (u32::from(bytes[(bit / 8) as usize]) >> (bit % 8)) & ((1 << unit) - 1)
}

fn set_bits_at(bytes: &mut [u8], bit: u32, unit: u32, value: u32) {
    if unit >= 8 {
        let at = (bit / 8) as usize;
        let length = (unit / 8) as usize;
        bytes[at..at + length].copy_from_slice(&value.to_le_bytes()[..length]);
        return;
    }
    let mask = ((1u32 << unit) - 1) << (bit % 8);
    let byte = &mut bytes[(bit / 8) as usize];
    *byte = ((u32::from(*byte) & !mask) | ((value << (bit % 8)) & mask)).to_le_bytes()[0];
}

#[cfg(test)]
mod tests;
