//! A file's data: the pointers its header and extension blocks hold, filled
//! from each table's end, and the data blocks they name — bare on FFS, headed
//! and chained on OFS.

use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::NodeId;
use tairix_abi::DriverError;
use tairix_fsprobe::affs::{be32, put_be32, sums_to_zero, T_DATA};

use crate::guard::Guard;
use crate::header::{low32, within, Extension, Header, Kind, CHECKSUM};
use crate::{Affs, Chain};

/// Bytes of an OFS data block's header: type, owner, sequence number, bytes
/// held, next block, checksum.
pub(crate) const OFS_HEADER: usize = 24;

/// The most blocks one transfer moves.
const RUN_BLOCKS: usize = 128;

/// Where data block `index`'s pointer lies: in the header's table, or in
/// extension block `.0`'s, at slot `.1`.
fn slot_of(index: u64, table_len: u64) -> (Option<u64>, usize) {
    let slot = |at: u64| usize::try_from(table_len - 1 - at % table_len).unwrap_or(0);
    if index < table_len {
        (None, slot(index))
    } else {
        let rest = index - table_len;
        (Some(rest / table_len), slot(rest))
    }
}

/// The extension block a store is filling, and whether it changed.
struct Filling {
    number: u64,
    ext: Extension,
    dirty: bool,
}

/// What a store carries from one block to the next: the extension block the
/// last pointer was read from, the one new pointers are placed in, where the
/// next block is looked for, and the file's data blocks before the store.
struct Cursor {
    held: Option<(u64, Extension)>,
    filling: Option<Filling>,
    near: u64,
    old: u64,
    /// The last block the store took, and its index: an OFS store asks for
    /// the next block ahead of writing this one.
    taken: Option<(u64, u64)>,
}

/// One data block of a store: its index and block, the block an OFS chain
/// takes it on to, and the file's data blocks and size before and its size
/// after.
struct Placed {
    index: u64,
    block: u64,
    next: u64,
    old: u64,
    old_size: u64,
    size: u64,
}

impl<B: Block> Affs<B> {
    fn table_len(&self) -> u64 {
        self.at.table_len() as u64
    }

    /// Data blocks a file of `size` bytes takes.
    fn data_blocks(&self, size: u64) -> u64 {
        size.div_ceil(self.data_size() as u64)
    }

    /// Extension blocks a file of `blocks` data blocks takes.
    fn extension_blocks(&self, blocks: u64) -> u64 {
        blocks
            .saturating_sub(self.table_len())
            .div_ceil(self.table_len())
    }

    /// The pointers a table holding the pointers of data blocks
    /// `first..first + count` of a file of `blocks` must record.
    fn table_count(&self, first: u64, blocks: u64) -> u32 {
        low32(blocks.saturating_sub(first).min(self.table_len()))
    }

    /// The block number of extension block `number` of `file`, walking its
    /// chain from as far as an earlier call reached.
    fn extension_block(&mut self, file: &Header, number: u64) -> Result<u64, DriverError> {
        if self
            .chain
            .as_ref()
            .is_none_or(|chain| chain.file != file.block)
        {
            self.chain = Some(Chain {
                file: file.block,
                extensions: Vec::new(),
                guard: Guard::new(file.block, self.disk.blocks),
            });
        }
        loop {
            let (known, last) = match &self.chain {
                Some(chain) => (
                    chain.extensions.len() as u64,
                    chain.extensions.last().copied(),
                ),
                None => return Err(DriverError::BadMagic),
            };
            if number < known {
                let at = usize::try_from(number).map_err(|_| DriverError::BadMagic)?;
                return Ok(self
                    .chain
                    .as_ref()
                    .map_or(0, |chain| u64::from(chain.extensions[at])));
            }
            let next = match last {
                None => file.extension(),
                Some(last) => self.read_extension(file, u64::from(last))?.next(),
            };
            if next == 0 {
                return Err(DriverError::BadMagic);
            }
            let Some(chain) = self.chain.as_mut() else {
                return Err(DriverError::BadMagic);
            };
            chain.guard.step(u64::from(next))?;
            if !tairix_util::fallible::push(&mut chain.extensions, next) {
                return Err(DriverError::OutOfMemory);
            }
        }
    }

    fn read_extension(&mut self, file: &Header, block: u64) -> Result<Extension, DriverError> {
        let mut buf = self.disk.buffer()?;
        self.disk.read(block, &mut buf)?;
        Extension::parse(block, buf, self.at, file.block, self.disk.blocks)
    }

    /// Extension block `number` of `file`, checked to hold at least as many
    /// pointers as a file of `blocks` data blocks puts in it. A growth writes
    /// its extensions before the header that commits it, so one holding more
    /// is read only as far as the header's size.
    fn extension(
        &mut self,
        file: &Header,
        number: u64,
        blocks: u64,
    ) -> Result<Extension, DriverError> {
        let block = self.extension_block(file, number)?;
        let ext = self.read_extension(file, block)?;
        let first = self.table_len() * (number + 1);
        if ext.high_seq() < self.table_count(first, blocks) {
            return Err(DriverError::BadMagic);
        }
        Ok(ext)
    }

    /// Check `file`'s header holds as many pointers as its size needs.
    fn check_header(&self, file: &Header) -> Result<u64, DriverError> {
        let blocks = self.data_blocks(u64::from(file.byte_size()));
        let extended = self.extension_blocks(blocks) != 0;
        if file.high_seq() != self.table_count(0, blocks) || (file.extension() != 0) != extended {
            return Err(DriverError::BadMagic);
        }
        Ok(blocks)
    }

    /// Data block `index`'s number, `held` keeping the extension block the
    /// last call read.
    fn pointer(
        &mut self,
        file: &Header,
        index: u64,
        blocks: u64,
        held: &mut Option<(u64, Extension)>,
    ) -> Result<u64, DriverError> {
        let (ext, slot) = slot_of(index, self.table_len());
        let pointer = match ext {
            None => file.table(slot),
            Some(number) => {
                if held.as_ref().is_none_or(|(at, _)| *at != number) {
                    *held = Some((number, self.extension(file, number, blocks)?));
                }
                held.as_ref().map_or(0, |(_, ext)| ext.table(slot))
            }
        };
        if !within(u64::from(pointer), self.disk.blocks) {
            return Err(DriverError::BadMagic);
        }
        Ok(u64::from(pointer))
    }

    /// Check an OFS data block read as data block `index` of `file`, of a
    /// file `size` bytes long, and answer the bytes of it the size covers. A
    /// growth rewrites the old last block before the header that commits it,
    /// so a block holding more is read only as far as the size.
    fn ofs_held(
        &self,
        block: &[u8],
        file: u64,
        index: u64,
        size: u64,
    ) -> Result<usize, DriverError> {
        let ds = self.data_size() as u64;
        let expected = (size - index * ds).min(ds);
        let held = u64::from(be32(block, 12));
        if be32(block, 0) != T_DATA
            || u64::from(be32(block, 4)) != file
            || u64::from(be32(block, 8)) != index + 1
            || held < expected
            || held > ds
            || !sums_to_zero(block)
        {
            return Err(DriverError::BadMagic);
        }
        usize::try_from(expected).map_err(|_| DriverError::BadMagic)
    }

    /// Read up to `buf.len()` bytes of `file` from `offset`, `held` keeping
    /// the extension block the last pointer came from.
    pub(crate) fn read_file(
        &mut self,
        file: &Header,
        offset: u64,
        buf: &mut [u8],
        held: &mut Option<(u64, Extension)>,
    ) -> Result<usize, DriverError> {
        let size = u64::from(file.byte_size());
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let blocks = self.check_header(file)?;
        let want = usize::try_from(size - offset).map_or(buf.len(), |left| left.min(buf.len()));
        let ds = self.data_size();
        let bs = self.disk.block_size;
        let fast = self.layout.dos_type.is_fast();
        let mut scratch = self.disk.buffer()?;
        let mut done = 0usize;
        while done < want {
            let at = offset + done as u64;
            let index = at / ds as u64;
            let within = usize::try_from(at % ds as u64).unwrap_or(0);
            let block = self.pointer(file, index, blocks, held)?;
            if fast && within == 0 && want - done >= bs {
                let mut run = 1usize;
                while run < RUN_BLOCKS
                    && index + (run as u64) < blocks
                    && want - done >= (run + 1) * bs
                    && self.pointer(file, index + run as u64, blocks, held)? == block + run as u64
                {
                    run += 1;
                }
                let len = run * bs;
                self.disk.read(block, &mut buf[done..done + len])?;
                done += len;
                continue;
            }
            self.disk.read(block, &mut scratch)?;
            let (payload, stored) = if fast {
                (&scratch[..], ds)
            } else {
                let held = self.ofs_held(&scratch, file.block, index, size)?;
                (&scratch[OFS_HEADER..], held)
            };
            let take = (stored - within).min(want - done);
            buf[done..done + take].copy_from_slice(&payload[within..within + take]);
            done += take;
        }
        Ok(want)
    }

    /// Write `data` at `offset` of the file `id`.
    pub(crate) fn write_file(
        &mut self,
        id: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        if data.is_empty() {
            self.node(id)?;
            return Ok(0);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(DriverError::OutOfRange)?;
        self.change(|fs| {
            let mut file = fs.node(id)?;
            if file.kind != Kind::File {
                return Err(DriverError::Unsupported);
            }
            let size = u64::from(file.byte_size()).max(end);
            fs.store(&mut file, offset, data, size)?;
            Ok(data.len())
        })
    }

    /// Set the length of the file `id` to `size`.
    pub(crate) fn truncate_file(&mut self, id: NodeId, size: u64) -> Result<(), DriverError> {
        self.change(|fs| {
            let mut file = fs.node(id)?;
            if file.kind != Kind::File {
                return Err(DriverError::Unsupported);
            }
            let old = u64::from(file.byte_size());
            match size.cmp(&old) {
                core::cmp::Ordering::Greater => fs.store(&mut file, old, &[], size),
                core::cmp::Ordering::Less => fs.shrink(&mut file, size),
                core::cmp::Ordering::Equal => Ok(()),
            }
        })
    }

    /// Make `file` `size` bytes long, `data` written at `offset` within it
    /// and every byte neither the file nor `data` held zero.
    ///
    /// The blocks a growth needs are counted against the free space before
    /// anything is written. Each block the change reaches is then written
    /// once, in order — a run of whole blocks the data covers in one
    /// transfer — and the header last.
    fn store(
        &mut self,
        file: &mut Header,
        offset: u64,
        data: &[u8],
        size: u64,
    ) -> Result<(), DriverError> {
        if size > u64::from(u32::MAX) {
            return Err(DriverError::NoSpace);
        }
        let old_size = u64::from(file.byte_size());
        let old = self.check_header(file)?;
        let blocks = self.data_blocks(size);
        let needed = (blocks - old) + self.extension_blocks(blocks) - self.extension_blocks(old);
        if needed > 0 && self.with_bitmap(|bitmap, disk| bitmap.free(disk))? < needed {
            return Err(DriverError::NoSpace);
        }
        self.chain = None;
        let ds = self.data_size() as u64;
        let fast = self.layout.dos_type.is_fast();
        let end = offset + data.len() as u64;
        let (mut lo, mut hi) = if data.is_empty() {
            (u64::MAX, 0)
        } else {
            (offset / ds, end.div_ceil(ds))
        };
        if size > old_size {
            // The old last block's tail past the old end is zeroed, and on OFS
            // it is chained to the first new block.
            if old > 0 && (!fast || old_size % ds != 0) {
                lo = lo.min(old - 1);
            }
            lo = lo.min(old);
            hi = hi.max(blocks);
        }
        let mut cursor = Cursor {
            held: None,
            filling: None,
            near: file.block + 1,
            old,
            taken: None,
        };
        if old > 0 {
            // The file grows on from its last block, not from its first.
            cursor.near = self.pointer(file, old - 1, old, &mut cursor.held)? + 1;
        }
        let mut scratch = self.disk.buffer()?;
        let mut run: Option<(u64, u64, u64)> = None;
        for index in lo..hi {
            let block = self.block_at(file, index, &mut cursor)?;
            let start = index * ds;
            if fast && offset <= start && end >= start + ds {
                match &mut run {
                    Some((first, _, count))
                        if *first + *count == block && *count < RUN_BLOCKS as u64 =>
                    {
                        *count += 1;
                    }
                    _ => {
                        self.write_run(run.take(), offset, data)?;
                        run = Some((block, index, 1));
                    }
                }
                continue;
            }
            self.write_run(run.take(), offset, data)?;
            let next = if !fast && index + 1 < blocks {
                self.block_at(file, index + 1, &mut cursor)?
            } else {
                0
            };
            let place = Placed {
                index,
                block,
                next,
                old,
                old_size,
                size,
            };
            self.write_block(file, &place, offset, data, &mut scratch)?;
        }
        self.write_run(run, offset, data)?;
        if let Some(mut filling) = cursor.filling.take() {
            if filling.dirty {
                self.put(filling.ext.block, filling.ext.sealed())?;
            }
        }
        self.chain = None;
        file.set_high_seq(self.table_count(0, blocks));
        file.set_first_data(if blocks > 0 {
            file.table(self.at.table_len() - 1)
        } else {
            0
        });
        file.set_byte_size(low32(size));
        file.set_date(self.now());
        self.write_header(file)
    }

    /// Write a run of whole data blocks — its first block, first index and
    /// length — from `data`, which covers each.
    fn write_run(
        &mut self,
        run: Option<(u64, u64, u64)>,
        offset: u64,
        data: &[u8],
    ) -> Result<(), DriverError> {
        let Some((block, index, count)) = run else {
            return Ok(());
        };
        let ds = self.data_size() as u64;
        let from = usize::try_from(index * ds - offset).map_err(|_| DriverError::OutOfRange)?;
        let len = usize::try_from(count * ds).map_err(|_| DriverError::OutOfRange)?;
        self.put(block, &data[from..from + len])
    }

    /// Data block `index`'s number in a store: an existing block's pointer,
    /// or a block taken now and placed in its table, a new extension block
    /// taken as a table fills. Asked again for a block it took, it answers
    /// the same block.
    fn block_at(
        &mut self,
        file: &mut Header,
        index: u64,
        cursor: &mut Cursor,
    ) -> Result<u64, DriverError> {
        if index < cursor.old {
            return self.pointer(file, index, cursor.old, &mut cursor.held);
        }
        if let Some((taken, block)) = cursor.taken {
            if taken == index {
                return Ok(block);
            }
        }
        // A slot past what a table records holds nothing to trust: the block
        // taken here overwrites it.
        let (ext, slot) = slot_of(index, self.table_len());
        if let Some(number) = ext {
            if cursor
                .filling
                .as_ref()
                .is_none_or(|filling| filling.number != number)
            {
                self.fill_extension(file, number, cursor)?;
            }
        }
        let block = self.allocate(Some(cursor.near))?;
        cursor.near = block + 1;
        match (ext, cursor.filling.as_mut()) {
            (None, _) => {
                file.set_table(slot, low32(block));
                file.set_high_seq(file.high_seq() + 1);
            }
            (Some(_), Some(filling)) => {
                filling.ext.set_table(slot, low32(block));
                let count = filling.ext.high_seq() + 1;
                filling.ext.set_high_seq(count);
                filling.dirty = true;
            }
            (Some(_), None) => return Err(DriverError::BadMagic),
        }
        cursor.taken = Some((index, block));
        Ok(block)
    }

    /// Make extension block `number` the one a store places pointers in: an
    /// existing one read, or a new one taken, written empty, and only then
    /// chained from the one before it or from the header. The one it replaces
    /// is written if it changed.
    fn fill_extension(
        &mut self,
        file: &mut Header,
        number: u64,
        cursor: &mut Cursor,
    ) -> Result<(), DriverError> {
        let previous = cursor.filling.take();
        if number < self.extension_blocks(cursor.old) {
            if let Some(mut done) = previous {
                if done.dirty {
                    self.put(done.ext.block, done.ext.sealed())?;
                }
            }
            let mut ext = self.extension(file, number, cursor.old)?;
            // A growth cut short may have left it holding more than the header
            // commits; the store counts on from what is committed.
            let committed = self.table_count(self.table_len() * (number + 1), cursor.old);
            let dirty = ext.high_seq() != committed;
            ext.set_high_seq(committed);
            cursor.filling = Some(Filling { number, ext, dirty });
            return Ok(());
        }
        let block = self.allocate(Some(cursor.near))?;
        cursor.near = block + 1;
        let buf = self.disk.buffer()?;
        let mut fresh = Extension::new(block, buf, self.at, file.block);
        self.put(block, fresh.sealed())?;
        match previous {
            Some(mut before) if before.number + 1 == number => {
                before.ext.set_next(low32(block));
                self.put(before.ext.block, before.ext.sealed())?;
            }
            Some(mut other) => {
                if other.dirty {
                    self.put(other.ext.block, other.ext.sealed())?;
                }
                self.chain_extension(file, number, block)?;
            }
            None => self.chain_extension(file, number, block)?,
        }
        cursor.filling = Some(Filling {
            number,
            ext: fresh,
            dirty: false,
        });
        Ok(())
    }

    /// Chain the new extension block `block`, numbered `number`, from the
    /// header or from the extension block before it.
    fn chain_extension(
        &mut self,
        file: &mut Header,
        number: u64,
        block: u64,
    ) -> Result<(), DriverError> {
        if number == 0 {
            file.set_extension(low32(block));
            return Ok(());
        }
        let before = self.extension_block(file, number - 1)?;
        let mut before = self.read_extension(file, before)?;
        before.set_next(low32(block));
        self.put(before.block, before.sealed())
    }

    /// Write data block `place` of a store: what it held kept where neither
    /// the growth nor `data`, written at `offset`, replaces it, bytes past the
    /// old end zero; on OFS headed and chained.
    fn write_block(
        &mut self,
        file: &Header,
        place: &Placed,
        offset: u64,
        data: &[u8],
        scratch: &mut [u8],
    ) -> Result<(), DriverError> {
        let fast = self.layout.dos_type.is_fast();
        let ds = self.data_size() as u64;
        let start = place.index * ds;
        let stored = (place.size - start).min(ds);
        let at = if fast { 0 } else { OFS_HEADER };
        if place.index < place.old {
            self.disk.read(place.block, scratch)?;
            if !fast {
                self.ofs_held(scratch, file.block, place.index, place.old_size)?;
            }
            let kept = place.old_size.saturating_sub(start).min(ds);
            scratch[at + usize::try_from(kept).map_err(|_| DriverError::OutOfRange)?..].fill(0);
        } else {
            scratch.fill(0);
        }
        let end = offset + data.len() as u64;
        let lo = offset.max(start);
        let hi = end.min(start + stored);
        if lo < hi {
            let into = at + usize::try_from(lo - start).map_err(|_| DriverError::OutOfRange)?;
            let from = usize::try_from(lo - offset).map_err(|_| DriverError::OutOfRange)?;
            let len = usize::try_from(hi - lo).map_err(|_| DriverError::OutOfRange)?;
            scratch[into..into + len].copy_from_slice(&data[from..from + len]);
        }
        if !fast {
            put_be32(scratch, 0, T_DATA);
            put_be32(scratch, 4, low32(file.block));
            put_be32(scratch, 8, low32(place.index + 1));
            put_be32(scratch, 12, low32(stored));
            put_be32(scratch, 16, low32(place.next));
            put_be32(scratch, CHECKSUM, 0);
            let sum = tairix_fsprobe::affs::checksum(scratch, CHECKSUM);
            put_be32(scratch, CHECKSUM, sum);
        }
        self.put(place.block, scratch)
    }

    /// Cut `file` to `size` bytes, freeing the blocks past it.
    ///
    /// The shorter header is written first, so every state a change cut short
    /// leaves reads as the shorter file: the extension and OFS block it then
    /// trims hold more than it, which a read takes only as far as its size.
    fn shrink(&mut self, file: &mut Header, size: u64) -> Result<(), DriverError> {
        let old = self.check_header(file)?;
        let old_size = u64::from(file.byte_size());
        let blocks = self.data_blocks(size);
        self.chain = None;
        let mut freed: Vec<u64> = Vec::new();
        let table = self.table_len();
        let mut held = None;
        for index in blocks..old {
            let block = self.pointer(file, index, old, &mut held)?;
            if !tairix_util::fallible::push(&mut freed, block) {
                return Err(DriverError::OutOfMemory);
            }
        }
        // The extension blocks the shorter file still needs keep the pointers
        // below the cut; the rest go.
        let keep = self.extension_blocks(blocks);
        let mut last_kept = None;
        for number in 0..self.extension_blocks(old) {
            let block = self.extension_block(file, number)?;
            if number >= keep {
                if !tairix_util::fallible::push(&mut freed, block) {
                    return Err(DriverError::OutOfMemory);
                }
            } else if number + 1 == keep {
                last_kept = Some((number, block));
            }
        }
        // OFS's new last block, read while the old size still describes it.
        let ofs_last = if !self.layout.dos_type.is_fast() && blocks > 0 {
            let last = self.pointer(file, blocks - 1, old, &mut held)?;
            let mut scratch = self.disk.buffer()?;
            self.disk.read(last, &mut scratch)?;
            self.ofs_held(&scratch, file.block, blocks - 1, old_size)?;
            Some((last, scratch))
        } else {
            None
        };
        for index in blocks..old.min(table) {
            let (_, slot) = slot_of(index, table);
            file.set_table(slot, 0);
        }
        if keep == 0 {
            file.set_extension(0);
        }
        file.set_high_seq(self.table_count(0, blocks));
        if blocks == 0 {
            file.set_first_data(0);
        }
        file.set_byte_size(low32(size));
        file.set_date(self.now());
        self.write_header(file)?;
        if let Some((number, block)) = last_kept {
            let mut ext = self.read_extension(file, block)?;
            let first = table * (number + 1);
            for index in blocks.max(first)..(first + table).min(old) {
                let (_, slot) = slot_of(index, table);
                ext.set_table(slot, 0);
            }
            ext.set_high_seq(self.table_count(first, blocks));
            ext.set_next(0);
            self.put(block, ext.sealed())?;
        }
        if let Some((last, mut scratch)) = ofs_last {
            // It holds fewer bytes now and ends the chain.
            let held_now = size - (blocks - 1) * self.data_size() as u64;
            scratch
                [OFS_HEADER + usize::try_from(held_now).map_err(|_| DriverError::OutOfRange)?..]
                .fill(0);
            put_be32(&mut scratch, 12, low32(held_now));
            put_be32(&mut scratch, 16, 0);
            put_be32(&mut scratch, CHECKSUM, 0);
            let sum = tairix_fsprobe::affs::checksum(&scratch, CHECKSUM);
            put_be32(&mut scratch, CHECKSUM, sum);
            self.put(last, &scratch)?;
        }
        for block in freed {
            self.release(block)?;
        }
        Ok(())
    }

    /// Free every data and extension block of `file`.
    pub(crate) fn free_data(&mut self, file: &Header) -> Result<(), DriverError> {
        let blocks = self.check_header(file)?;
        self.chain = None;
        let mut held = None;
        for index in 0..blocks {
            let block = self.pointer(file, index, blocks, &mut held)?;
            self.release(block)?;
        }
        for number in 0..self.extension_blocks(blocks) {
            let block = self.extension_block(file, number)?;
            self.release(block)?;
        }
        self.chain = None;
        Ok(())
    }

    /// Take a free block, the first at or after `near`, or after the last
    /// one taken. A volume with none is refused before anything is written.
    pub(crate) fn allocate(&mut self, near: Option<u64>) -> Result<u64, DriverError> {
        let block = self
            .with_bitmap(|bitmap, disk| bitmap.find(disk, near))?
            .ok_or(DriverError::NoSpace)?;
        self.begin()?;
        self.mutated = true;
        self.with_bitmap(|bitmap, disk| bitmap.take(disk, block))?;
        Ok(block)
    }

    /// Give `block` back.
    pub(crate) fn release(&mut self, block: u64) -> Result<(), DriverError> {
        self.begin()?;
        self.mutated = true;
        self.with_bitmap(|bitmap, disk| bitmap.release(disk, block))
    }
}
