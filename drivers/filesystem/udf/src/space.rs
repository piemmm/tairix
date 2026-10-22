//! A partition's unallocated space bitmap (ECMA-167 4/14.12): one bit a
//! block, set while the block is free. Read and written a block of the
//! bitmap at a time, a bounded number held, so a volume of any size costs a
//! fixed amount of memory.

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_fsprobe::udf::{tag_checksum, TAG_LEN, TAG_SBD};

use crate::fields::Fields;
use crate::layout::LbAddr;

/// Bytes of the descriptor's header before the bitmap's bytes.
pub(crate) const HEADER: u64 = 24;

/// Bitmap blocks held at once: a fixed bound, not a capacity — a bitmap
/// block covers eight blocks per byte, so a handful spans a large region.
const HELD: usize = 8;

/// One bitmap block held, and whether it differs from the disc.
struct Page {
    index: u32,
    bytes: Vec<u8>,
    dirty: bool,
}

/// What a bitmap reads and writes its blocks through.
pub(crate) trait Pages {
    fn read_page(&mut self, at: LbAddr, out: &mut [u8]) -> Result<(), DriverError>;
    fn write_page(&mut self, at: LbAddr, bytes: &[u8]) -> Result<(), DriverError>;
}

/// A partition's space bitmap, paged.
pub(crate) struct Space {
    /// Where the bitmap's descriptor starts.
    start: LbAddr,
    block_size: u32,
    /// Blocks the bitmap covers: the partition's length.
    bits: u32,
    free: u64,
    /// Where the next search for free space starts.
    hint: u32,
    pages: Vec<Page>,
}

impl Space {
    /// The bitmap whose descriptor opens block `start`; its header must
    /// verify. `recorded` is the free count the volume's integrity
    /// descriptor keeps, which spares reading the whole bitmap to count.
    pub(crate) fn open(
        io: &mut impl Pages,
        start: LbAddr,
        block_size: u32,
        length: u32,
        recorded: Option<u32>,
    ) -> Result<Self, DriverError> {
        let mut first = crate::zeroed(block_size)?;
        io.read_page(start, &mut first)?;
        let tag: &[u8; TAG_LEN] = first
            .get(..TAG_LEN)
            .and_then(|t| t.try_into().ok())
            .ok_or(DriverError::DeviceFault)?;
        let f = Fields(&first);
        // The CRC of a bitmap spanning blocks covers no bitmap byte, so the
        // tag's own fields are what is checked.
        if tag_checksum(tag) != tag[4] || f.u16(0)? != TAG_SBD || f.u32(12)? != start.block {
            return Err(DriverError::DeviceFault);
        }
        let bits = f.u32(16)?;
        if bits < length || u64::from(f.u32(20)?) < u64::from(bits).div_ceil(8) {
            return Err(DriverError::DeviceFault);
        }
        let mut space = Self {
            start,
            block_size,
            bits: length,
            free: 0,
            hint: 0,
            pages: Vec::new(),
        };
        space.free = match recorded.filter(|&free| free <= length) {
            Some(free) => u64::from(free),
            None => space.count(io)?,
        };
        Ok(space)
    }

    /// Blocks free.
    pub(crate) fn free(&self) -> u64 {
        self.free
    }

    /// Count the free blocks, a bitmap block at a time.
    fn count(&mut self, io: &mut impl Pages) -> Result<u64, DriverError> {
        let mut free = 0u64;
        let mut bit = 0u32;
        while bit < self.bits {
            let (page, within) = self.locate(bit);
            let at = self.page(io, page)?;
            for &byte in &self.pages[at].bytes[within..] {
                let take = (self.bits - bit).min(8);
                free += u64::from((byte & low_bits(take)).count_ones());
                bit += take;
                if bit >= self.bits {
                    break;
                }
            }
        }
        Ok(free)
    }

    /// The first free block in `from..to`, scanning a byte at a time.
    fn find_free(
        &mut self,
        io: &mut impl Pages,
        from: u32,
        to: u32,
    ) -> Result<Option<u32>, DriverError> {
        // Counted wide: the byte after the last of a partition ending at
        // `u32::MAX` is past what a block number holds.
        let mut bit = u64::from(from);
        let to = u64::from(to);
        while bit < to {
            let (page, within) =
                self.locate(u32::try_from(bit).map_err(|_| DriverError::DeviceFault)?);
            let at = self.page(io, page)?;
            for &byte in &self.pages[at].bytes[within..] {
                let here = byte & (u8::MAX << (bit % 8));
                if here != 0 {
                    let found = bit - bit % 8 + u64::from(here.trailing_zeros());
                    return Ok(u32::try_from(found).ok().filter(|_| found < to));
                }
                bit = bit - bit % 8 + 8;
                if bit >= to {
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    /// The bitmap block and byte within it that bit `bit` lies in.
    fn locate(&self, bit: u32) -> (u32, usize) {
        let byte = HEADER + u64::from(bit / 8);
        let bs = u64::from(self.block_size);
        (
            u32::try_from(byte / bs).unwrap_or(u32::MAX),
            usize::try_from(byte % bs).unwrap_or(0),
        )
    }

    fn page(&mut self, io: &mut impl Pages, index: u32) -> Result<usize, DriverError> {
        if let Some(at) = self.pages.iter().position(|page| page.index == index) {
            return Ok(at);
        }
        if self.pages.len() >= HELD {
            let page = self.pages.remove(0);
            if page.dirty {
                io.write_page(self.addr(page.index)?, &page.bytes)?;
            }
        }
        let mut bytes = crate::zeroed(self.block_size)?;
        io.read_page(self.addr(index)?, &mut bytes)?;
        if !tairix_util::fallible::reserve(&mut self.pages, 1) {
            return Err(DriverError::OutOfMemory);
        }
        self.pages.push(Page {
            index,
            bytes,
            dirty: false,
        });
        Ok(self.pages.len() - 1)
    }

    fn addr(&self, index: u32) -> Result<LbAddr, DriverError> {
        Ok(LbAddr {
            block: self
                .start
                .block
                .checked_add(index)
                .ok_or(DriverError::DeviceFault)?,
            partition: self.start.partition,
        })
    }

    fn byte(&mut self, io: &mut impl Pages, page: u32, within: usize) -> Result<u8, DriverError> {
        let at = self.page(io, page)?;
        self.pages[at]
            .bytes
            .get(within)
            .copied()
            .ok_or(DriverError::DeviceFault)
    }

    fn is_free(&mut self, io: &mut impl Pages, block: u32) -> Result<bool, DriverError> {
        let (page, within) = self.locate(block);
        Ok(self.byte(io, page, within)? & (1 << (block % 8)) != 0)
    }

    fn set(&mut self, io: &mut impl Pages, block: u32, free: bool) -> Result<(), DriverError> {
        let (page, within) = self.locate(block);
        let at = self.page(io, page)?;
        let page = &mut self.pages[at];
        let byte = page.bytes.get_mut(within).ok_or(DriverError::DeviceFault)?;
        let mask = 1 << (block % 8);
        if (*byte & mask != 0) == free {
            // Freeing a free block, or taking a taken one, is a map that
            // disagrees with the files: damage, never silently absorbed.
            return Err(DriverError::DeviceFault);
        }
        *byte ^= mask;
        page.dirty = true;
        Ok(())
    }

    /// Take up to `want` free blocks in one run, searching on from where the
    /// last allocation ended, near `goal` when it names a block: the run's
    /// first block and length, its length never zero.
    pub(crate) fn allocate(
        &mut self,
        io: &mut impl Pages,
        want: u32,
        goal: Option<u32>,
    ) -> Result<(u32, u32), DriverError> {
        if self.free == 0 || want == 0 {
            return Err(DriverError::NoSpace);
        }
        let start = goal.filter(|&goal| goal < self.bits).unwrap_or(self.hint);
        let first = match self.find_free(io, start, self.bits)? {
            Some(first) => first,
            None => self.find_free(io, 0, start)?.ok_or(DriverError::NoSpace)?,
        };
        let mut len = 1;
        while len < want && first + len < self.bits && self.is_free(io, first + len)? {
            len += 1;
        }
        for block in first..first + len {
            self.set(io, block, false)?;
            self.free = self.free.saturating_sub(1);
        }
        self.hint = (first + len) % self.bits;
        Ok((first, len))
    }

    /// Return `count` blocks from `first` to the free pool: all of them, or
    /// — one already free — none.
    pub(crate) fn release(
        &mut self,
        io: &mut impl Pages,
        first: u32,
        count: u32,
    ) -> Result<(), DriverError> {
        self.mark(io, first, count, true)?;
        self.free += u64::from(count);
        Ok(())
    }

    /// Take back `count` blocks from `first` a release just freed: all of
    /// them, or — one already taken — none.
    pub(crate) fn retake(
        &mut self,
        io: &mut impl Pages,
        first: u32,
        count: u32,
    ) -> Result<(), DriverError> {
        self.mark(io, first, count, false)?;
        self.free = self.free.saturating_sub(u64::from(count));
        Ok(())
    }

    /// Mark a run free or taken, every block first checked to be the other.
    fn mark(
        &mut self,
        io: &mut impl Pages,
        first: u32,
        count: u32,
        free: bool,
    ) -> Result<(), DriverError> {
        let end = first
            .checked_add(count)
            .filter(|&end| end <= self.bits)
            .ok_or(DriverError::DeviceFault)?;
        for block in first..end {
            if self.is_free(io, block)? == free {
                // Freeing a free block, or taking a taken one, is a map that
                // disagrees with the files: damage, never silently absorbed.
                return Err(DriverError::DeviceFault);
            }
        }
        for block in first..end {
            self.set(io, block, free)?;
        }
        Ok(())
    }

    /// Write every held block that differs from the disc.
    pub(crate) fn write_back(&mut self, io: &mut impl Pages) -> Result<(), DriverError> {
        for at in 0..self.pages.len() {
            if self.pages[at].dirty {
                let addr = self.addr(self.pages[at].index)?;
                io.write_page(addr, &self.pages[at].bytes)?;
                self.pages[at].dirty = false;
            }
        }
        Ok(())
    }
}

/// A byte with its low `count` bits set.
fn low_bits(count: u32) -> u8 {
    u8::try_from((1u16 << count) - 1).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bitmap every block of which is taken, its header in block 0.
    struct Taken {
        bits: u32,
    }

    impl Pages for Taken {
        fn read_page(&mut self, at: LbAddr, out: &mut [u8]) -> Result<(), DriverError> {
            out.fill(0);
            if at.block == 0 {
                out[0..2].copy_from_slice(&TAG_SBD.to_le_bytes());
                out[16..20].copy_from_slice(&self.bits.to_le_bytes());
                out[20..24].copy_from_slice(&self.bits.div_ceil(8).to_le_bytes());
                let tag: [u8; TAG_LEN] = out[..TAG_LEN].try_into().expect("a tag");
                out[4] = tag_checksum(&tag);
            }
            Ok(())
        }

        fn write_page(&mut self, _at: LbAddr, _bytes: &[u8]) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn a_search_reaching_the_last_block_a_partition_can_hold_ends_there() {
        let mut io = Taken { bits: u32::MAX };
        let start = LbAddr {
            block: 0,
            partition: 0,
        };
        let mut space = Space::open(&mut io, start, 512, u32::MAX, Some(0)).expect("opens");
        assert_eq!(space.find_free(&mut io, u32::MAX - 20, u32::MAX), Ok(None));
    }
}
