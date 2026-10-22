//! zisofs, the transparent compression Rock Ridge discs may carry: a file
//! stored as a header, a table of block pointers, and one zlib stream per
//! uncompressed block (zisofs-tools' `zisofs` format, version 1).

use tairix_abi::DriverError;

use crate::susp::Zisofs;

/// The eight bytes every zisofs file opens with.
pub(crate) const MAGIC: [u8; 8] = [0x37, 0xE4, 0x53, 0x96, 0xC9, 0xDB, 0xD6, 0x07];

/// Bytes of the header [`Layout::parse`] reads.
pub(crate) const HEADER_LEN: usize = 16;

/// The block sizes the format allows, as log2: 32, 64 and 128 KiB.
const LOG2_BLOCKS: core::ops::RangeInclusive<u8> = 15..=17;

/// Bytes of one block pointer.
pub(crate) const POINTER_LEN: u64 = 4;

/// A validated zisofs file's layout.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    /// Log2 of an uncompressed block's bytes.
    pub log2_block: u8,
    /// Where the pointer table starts, in the stored bytes.
    pub table: u64,
    /// Uncompressed blocks.
    pub blocks: u64,
    /// Uncompressed bytes.
    pub size: u64,
}

impl Layout {
    /// The layout `header` declares, `None` unless it agrees with the
    /// record's `ZF` entry and its pointer table fits the `stored` bytes.
    pub(crate) fn parse(header: &[u8; HEADER_LEN], zf: Zisofs, stored: u64) -> Option<Self> {
        let size = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        let (header_words, log2_block) = (header[12], header[13]);
        let agrees = header[..8] == MAGIC
            && size == zf.size
            && header_words == zf.header_words
            && log2_block == zf.log2_block
            && usize::from(header_words) * 4 >= HEADER_LEN
            && LOG2_BLOCKS.contains(&log2_block);
        if !agrees {
            return None;
        }
        let size = u64::from(size);
        let blocks = size.div_ceil(1 << log2_block);
        let table = u64::from(header_words) * 4;
        let table_end = blocks
            .checked_add(1)?
            .checked_mul(POINTER_LEN)?
            .checked_add(table)?;
        (table_end <= stored).then_some(Self {
            log2_block,
            table,
            blocks,
            size,
        })
    }

    /// Bytes of one uncompressed block.
    pub(crate) const fn block_bytes(&self) -> u64 {
        1 << self.log2_block
    }

    /// Bytes block `index` decompresses to: a whole block but the last.
    pub(crate) fn block_len(&self, index: u64) -> u64 {
        let start = index * self.block_bytes();
        self.size.saturating_sub(start).min(self.block_bytes())
    }

    /// The most compressed bytes a block of `len` bytes may take: deflate's
    /// stored-block worst case, its zlib framing, and slack.
    pub(crate) const fn compressed_bound(len: u64) -> u64 {
        len + len / 16 + 64
    }
}

/// Decompress one block's zlib stream `compressed` into `out`, which it must
/// fill exactly.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a stream that fails to decode or
/// produces any other length.
pub(crate) fn inflate(compressed: &[u8], out: &mut [u8]) -> Result<(), DriverError> {
    match tairix_compress::zlib::decompress_into(compressed, out) {
        Ok(produced) if produced == out.len() => Ok(()),
        _ => Err(DriverError::DeviceFault),
    }
}
