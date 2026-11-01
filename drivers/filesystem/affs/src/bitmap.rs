//! The allocation bitmap: one bit a block from the first after the boot
//! blocks, set while the block is free, kept in checksummed bitmap blocks
//! the root lists — 25 of them, then extension blocks of further pointers.
//!
//! Pages are read when first touched and held in a cache bounded by
//! [`CACHE_BYTES`], whose buffers are taken as the bitmap loads, so no bitmap
//! operation allocates; a page leaving the cache dirty is written as it goes.
//! What is kept whole is the bitmap's own block list, in its order and
//! sorted, a longword a block as the format records them: one longword each
//! for a floppy, about 4 MiB each for the largest volume the format can
//! number. The count of free blocks is taken by one scan the first time it is
//! asked for, or by a search that found no free block, and kept from then on.

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_fsprobe::affs::{be32, put_be32, seal, sums_to_zero, BITMAP_CHECKSUM, BOOT_BLOCKS};

use crate::header::{low32, within};

/// The bitmap the cache holds at most, in bytes of pages.
const CACHE_BYTES: usize = 64 * 1024;

/// One cached bitmap block.
struct Page {
    /// Which of the volume's bitmap blocks it is.
    index: usize,
    data: Vec<u8>,
    dirty: bool,
    used: u64,
}

/// Where a bitmap's pages live, and what reaches them.
pub(crate) trait Pages {
    /// Fill `buf` from block `block`.
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Write `buf` to block `block`.
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError>;
}

/// The allocation bitmap of one volume.
pub(crate) struct Bitmap {
    /// Each bitmap block, in order.
    pointers: Vec<u32>,
    block_size: usize,
    /// The volume's blocks, the boot blocks included.
    blocks: u64,
    /// Blocks no allocation may take whatever the bits say — the root and the
    /// bitmap's own — ascending.
    reserved: Vec<u32>,
    pages: Vec<Page>,
    clock: u64,
    free: Option<u64>,
    /// Where the last allocation ended, so the next looks from there.
    hint: u64,
}

impl Bitmap {
    /// The bitmap whose blocks are `pointers`, chained through the extension
    /// blocks `extensions`, for a volume of `blocks` blocks of `block_size`
    /// bytes whose root is `root`.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for too few bitmap blocks to cover the
    /// volume, a pointer outside it, or a block named twice among the root,
    /// the bitmap's blocks and its extensions; [`DriverError::OutOfMemory`].
    pub(crate) fn new(
        pointers: Vec<u32>,
        extensions: &[u32],
        block_size: usize,
        blocks: u64,
        root: u64,
    ) -> Result<Self, DriverError> {
        let bits = bits_per_page(block_size);
        let needed = (blocks - BOOT_BLOCKS).div_ceil(bits);
        let in_volume = |&pointer: &u32| within(u64::from(pointer), blocks);
        if (pointers.len() as u64) < needed || !pointers.iter().chain(extensions).all(in_volume) {
            return Err(DriverError::BadMagic);
        }
        let mut reserved = Vec::new();
        reserved
            .try_reserve(1 + pointers.len() + extensions.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        reserved.push(low32(root));
        reserved.extend(pointers.iter().chain(extensions));
        reserved.sort_unstable();
        if reserved.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DriverError::BadMagic);
        }
        let capacity = (CACHE_BYTES / block_size).clamp(2, 64).min(pointers.len());
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(capacity)
            .map_err(|_| DriverError::OutOfMemory)?;
        for _ in 0..capacity {
            pages.push(Page {
                index: usize::MAX,
                data: tairix_util::fallible::filled(block_size, 0)
                    .ok_or(DriverError::OutOfMemory)?,
                dirty: false,
                used: 0,
            });
        }
        Ok(Self {
            pointers,
            block_size,
            blocks,
            reserved,
            pages,
            clock: 0,
            free: None,
            hint: root,
        })
    }

    fn bits(&self) -> u64 {
        bits_per_page(self.block_size)
    }

    /// The cached page `index`, reading it in — and writing out the page it
    /// displaces — as needed.
    fn page(&mut self, io: &mut dyn Pages, index: usize) -> Result<usize, DriverError> {
        self.clock += 1;
        if let Some(at) = self.pages.iter().position(|p| p.index == index) {
            self.pages[at].used = self.clock;
            return Ok(at);
        }
        let block = u64::from(*self.pointers.get(index).ok_or(DriverError::BadMagic)?);
        // A slot holding no page was never used, so it goes first.
        let slot = self
            .pages
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| p.used)
            .map(|(at, _)| at)
            .ok_or(DriverError::BadMagic)?;
        self.write_page(io, slot)?;
        let page = &mut self.pages[slot];
        page.index = index;
        page.dirty = false;
        page.used = self.clock;
        let read = match io.read(block, &mut page.data) {
            Ok(()) if sums_to_zero(&page.data) => Ok(slot),
            Ok(()) => Err(DriverError::BadMagic),
            Err(err) => Err(err),
        };
        if read.is_err() {
            page.index = usize::MAX;
            page.used = 0;
        }
        read
    }

    fn write_page(&mut self, io: &mut dyn Pages, at: usize) -> Result<(), DriverError> {
        let page = &mut self.pages[at];
        if !page.dirty {
            return Ok(());
        }
        let block = u64::from(self.pointers[page.index]);
        seal(&mut page.data, BITMAP_CHECKSUM);
        io.write(block, &page.data)?;
        page.dirty = false;
        Ok(())
    }

    /// Write every page changed since the last commit.
    pub(crate) fn commit(&mut self, io: &mut dyn Pages) -> Result<(), DriverError> {
        for at in 0..self.pages.len() {
            self.write_page(io, at)?;
        }
        Ok(())
    }

    /// Where block `block`'s bit lies: its page, the longword in it and the
    /// bit in that.
    fn locate(&self, block: u64) -> Result<(usize, usize, u32), DriverError> {
        if !within(block, self.blocks) {
            return Err(DriverError::BadMagic);
        }
        let bit = block - BOOT_BLOCKS;
        let page = usize::try_from(bit / self.bits()).map_err(|_| DriverError::BadMagic)?;
        let within = bit % self.bits();
        let long = usize::try_from(within / 32).map_err(|_| DriverError::BadMagic)?;
        Ok((page, 4 + 4 * long, 1 << (within % 32)))
    }

    /// Whether block `block` is free.
    pub(crate) fn is_free(&mut self, io: &mut dyn Pages, block: u64) -> Result<bool, DriverError> {
        let (page, at, mask) = self.locate(block)?;
        let slot = self.page(io, page)?;
        Ok(be32(&self.pages[slot].data, at) & mask != 0)
    }

    /// Mark block `block` free (`free`) or in use.
    fn set(&mut self, io: &mut dyn Pages, block: u64, free: bool) -> Result<(), DriverError> {
        let (page, at, mask) = self.locate(block)?;
        let slot = self.page(io, page)?;
        let page = &mut self.pages[slot];
        let long = be32(&page.data, at);
        if (long & mask != 0) == free {
            // A block freed twice, or taken while in use: the volume's bitmap
            // and its tree disagree.
            return Err(DriverError::BadMagic);
        }
        put_be32(&mut page.data, at, long ^ mask);
        page.dirty = true;
        if let Some(count) = self.free.as_mut() {
            *count = if free {
                count.saturating_add(1)
            } else {
                count.saturating_sub(1)
            };
        }
        Ok(())
    }

    /// Give block `block` back: never the root or a block of the bitmap's
    /// own, which only damage names as anything else.
    pub(crate) fn release(&mut self, io: &mut dyn Pages, block: u64) -> Result<(), DriverError> {
        if self.reserves(block, 1) {
            return Err(DriverError::BadMagic);
        }
        self.set(io, block, true)
    }

    /// Whether any of the `count` blocks from `first` is the root or a block of
    /// the bitmap's own.
    pub(crate) fn reserves(&self, first: u64, count: u64) -> bool {
        let at = self
            .reserved
            .partition_point(|&block| u64::from(block) < first);
        self.reserved
            .get(at)
            .is_some_and(|&block| u64::from(block) < first.saturating_add(count))
    }

    /// The free block an allocation would take: the first at or after
    /// `near`, or after the last block taken, wrapping to the volume's start;
    /// `None` when the volume is full, which a volume counted full answers
    /// without a scan. Changes no bit.
    pub(crate) fn find(
        &mut self,
        io: &mut dyn Pages,
        near: Option<u64>,
    ) -> Result<Option<u64>, DriverError> {
        if self.free == Some(0) {
            return Ok(None);
        }
        let start = near
            .filter(|&near| within(near, self.blocks))
            .unwrap_or(self.hint);
        let found = self.find_free(io, start)?;
        if found.is_none() {
            // The search spanned every block the count does.
            self.free = Some(0);
        }
        Ok(found)
    }

    /// Take block `block`, which [`Self::find`] answered.
    pub(crate) fn take(&mut self, io: &mut dyn Pages, block: u64) -> Result<(), DriverError> {
        self.set(io, block, false)?;
        self.hint = block + 1;
        Ok(())
    }

    /// The first free, unreserved block at or after `start`, wrapping once.
    fn find_free(&mut self, io: &mut dyn Pages, start: u64) -> Result<Option<u64>, DriverError> {
        let span = self.blocks - BOOT_BLOCKS;
        let first = start.saturating_sub(BOOT_BLOCKS).min(span - 1);
        for (from, to) in [(first, span), (0, first)] {
            if let Some(bit) = self.scan(io, from, to)? {
                return Ok(Some(bit + BOOT_BLOCKS));
            }
        }
        Ok(None)
    }

    /// The first set, unreserved bit in `[from, to)`, a longword at a time.
    fn scan(&mut self, io: &mut dyn Pages, from: u64, to: u64) -> Result<Option<u64>, DriverError> {
        let bits = self.bits();
        let mut bit = from;
        while bit < to {
            let page = bit / bits;
            let slot = self.page(
                io,
                usize::try_from(page).map_err(|_| DriverError::BadMagic)?,
            )?;
            let page_end = ((page + 1) * bits).min(to);
            while bit < page_end {
                let within = bit % bits;
                let at = 4 + 4 * usize::try_from(within / 32).map_err(|_| DriverError::BadMagic)?;
                let room = (32 - within % 32).min(page_end - bit);
                let mut long = be32(&self.pages[slot].data, at) >> (within % 32);
                if room < 32 {
                    long &= (1 << room) - 1;
                }
                if long == 0 {
                    bit += room;
                    continue;
                }
                let candidate = bit + u64::from(long.trailing_zeros());
                if !self.reserves(candidate + BOOT_BLOCKS, 1) {
                    return Ok(Some(candidate));
                }
                bit = candidate + 1;
            }
        }
        Ok(None)
    }

    /// The free blocks, counted by one scan the first time and kept since.
    pub(crate) fn free(&mut self, io: &mut dyn Pages) -> Result<u64, DriverError> {
        if let Some(free) = self.free {
            return Ok(free);
        }
        let bits = self.bits();
        let span = self.blocks - BOOT_BLOCKS;
        let pages = usize::try_from(span.div_ceil(bits)).map_err(|_| DriverError::BadMagic)?;
        let mut free = 0u64;
        for page in 0..pages {
            let slot = self.page(io, page)?;
            let mut left = (span - page as u64 * bits).min(bits);
            for long in self.pages[slot].data[4..].as_chunks::<4>().0 {
                if left == 0 {
                    break;
                }
                let mut value = u32::from_be_bytes(*long);
                if left < 32 {
                    value &= (1 << left) - 1;
                }
                free += u64::from(value.count_ones());
                left = left.saturating_sub(32);
            }
        }
        // A damaged bitmap may mark a structural block free; it is never
        // handed out, so it is not counted as free either.
        for at in 0..self.reserved.len() {
            if self.is_free(io, u64::from(self.reserved[at]))? {
                free = free.saturating_sub(1);
            }
        }
        self.free = Some(free);
        Ok(free)
    }
}

/// Blocks one bitmap page maps: every longword after its checksum.
pub(crate) fn bits_per_page(block_size: usize) -> u64 {
    (block_size as u64 / 4 - 1) * 32
}

#[cfg(test)]
mod tests {
    use tairix_abi::DriverError;

    use super::{bits_per_page, Bitmap, Pages};

    /// Bitmap pages that mark every block in use, counting their reads.
    struct Full {
        reads: usize,
    }

    impl Pages for Full {
        fn read(&mut self, _block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            self.reads += 1;
            buf.fill(0);
            Ok(())
        }

        fn write(&mut self, _block: u64, _buf: &[u8]) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn a_volume_found_full_answers_an_allocation_without_another_scan() {
        let blocks = 2 + 100 * bits_per_page(512);
        let pointers = (10..110).collect();
        let mut bitmap = Bitmap::new(pointers, &[], 512, blocks, blocks / 2).expect("a bitmap");
        let mut io = Full { reads: 0 };
        assert_eq!(bitmap.find(&mut io, None), Ok(None));
        assert!(io.reads >= 100, "the first search reads every page");
        io.reads = 0;
        assert_eq!(bitmap.find(&mut io, Some(blocks - 1)), Ok(None));
        assert_eq!(bitmap.free(&mut io), Ok(0));
        assert_eq!(io.reads, 0, "counted full, so answered without a scan");
        bitmap.release(&mut io, 5000).expect("released");
        assert_eq!(bitmap.find(&mut io, None), Ok(Some(5000)));
    }
}
