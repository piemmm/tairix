//! Writing a UDF volume in place, on one physical partition of rewritable or
//! overwritable media: blocks from its space bitmap, entries and directories
//! rewritten where they lie, and the integrity descriptor marked open before
//! a change's first write and closed once the change is published.

use alloc::vec::Vec;

use tairix_abi::blkio::BlkCommandCost;
use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{FilesystemWrite, NodeId, NodeKind, NodeSecurity, VolumeHost};
use tairix_abi::fs::FS_SYMLINK_MAX;
use tairix_abi::time::Time64;
use tairix_abi::{DriverError, DriverHandle};
use tairix_fsmeta::preset::mac::FinderInfo;
use tairix_fsprobe::udf::{Tag, TAG_AED, TAG_EAHD, TAG_EFE, TAG_FE, TAG_FID, TAG_LEN};
use tairix_writeback::CommitScheduler;

use crate::dir::{self, Fid, CHAR_DELETED, CHAR_DIRECTORY, CHAR_PARENT, FID_FIXED};
use crate::entry::{self, AdForm, Extent, Holds, AED_FIXED, EFE_FIXED, FE_FIXED, MAX_EXTENTS};
use crate::fields::{encode_timestamp, Fields};
use crate::layout::{LbAddr, Map};
use crate::space::{Pages, Space};
use crate::{
    addr_of, ea, kind_of, node_of, zeroed, zeroed_len, Mapped, Resolved, Udf, UNSPECIFIED_ID,
};

/// The longest extent a descriptor's 30-bit length holds, in whole blocks
/// of the largest size.
const EXTENT_MAX: u32 = (1 << 30) - 4096;

/// The implementation this driver records itself as.
const IMPLEMENTATION: &[u8] = b"*TAIRiX UDF";

/// The physical partition's reference: the one partition a written volume
/// has.
const PARTITION: u16 = 0;

/// Why a volume is refused writes once a change failed part-way through.
const DAMAGED: &str = "damaged";

/// What a writer keeps between operations.
pub(crate) struct State {
    space: Option<Space>,
    /// The integrity descriptor on the disc is marked open.
    open: bool,
    /// When the open change must be published.
    schedule: CommitScheduler,
    /// Writes of the volume's structures and data, so a failed change can
    /// tell whether it wrote anything.
    writes: u64,
}

impl State {
    /// A writer for a device whose commands cost `cost`.
    pub(crate) const fn new(cost: BlkCommandCost) -> Self {
        Self {
            space: None,
            open: false,
            schedule: CommitScheduler::new(cost),
            writes: 0,
        }
    }

    /// Blocks free, once a write has opened the space bitmap.
    pub(crate) fn free_blocks(&self) -> Option<u64> {
        self.space.as_ref().map(Space::free)
    }
}

/// Where an entry's fields lie: a File Entry's offsets, or an Extended File
/// Entry's.
#[derive(Copy, Clone)]
struct Layout {
    recorded: usize,
    accessed: usize,
    modified: usize,
    changed: usize,
    created: Option<usize>,
    object_size: Option<usize>,
    unique: usize,
    lengths: usize,
    fixed: usize,
}

/// Bytes of a short allocation descriptor.
const SHORT_AD: usize = 8;

/// A file's allocation descriptors laid out to be written: its extents, how
/// many its entry keeps, each continuation block with how many it keeps, and
/// the blocks taken and freed in getting there.
struct Descriptors {
    extents: Vec<Extent>,
    in_entry: usize,
    continuations: Vec<(u32, usize)>,
    /// Continuation blocks newly taken for these descriptors.
    taken: Vec<u32>,
    /// Blocks of the old chain these descriptors no longer need.
    spare: Vec<LbAddr>,
    /// The blocks the file then records: its data's and its continuations'.
    recorded: u64,
}

/// How `count` descriptors divide between an entry with `slots` of room and
/// continuation blocks of `per_block`: the entry's share, then each block's.
/// Every holder but the last gives its final slot to the descriptor that
/// continues into the next.
fn split(count: usize, slots: usize, per_block: usize) -> Result<(usize, Vec<usize>), DriverError> {
    if count <= slots {
        return Ok((count, Vec::new()));
    }
    // An entry without room for the continuing descriptor cannot record them.
    let in_entry = slots.checked_sub(1).ok_or(DriverError::NoSpace)?;
    if per_block < 2 {
        return Err(DriverError::DeviceFault);
    }
    let mut remaining = count - in_entry;
    let mut blocks = Vec::new();
    if !tairix_util::fallible::reserve(&mut blocks, remaining.div_ceil(per_block - 1)) {
        return Err(DriverError::OutOfMemory);
    }
    while remaining > 0 {
        let take = if remaining <= per_block {
            remaining
        } else {
            per_block - 1
        };
        blocks.push(take);
        remaining -= take;
    }
    Ok((in_entry, blocks))
}

impl Layout {
    const fn of(extended: bool) -> Self {
        if extended {
            Self {
                recorded: 72,
                accessed: 80,
                modified: 92,
                changed: 116,
                created: Some(104),
                object_size: Some(64),
                unique: 200,
                lengths: 208,
                fixed: EFE_FIXED,
            }
        } else {
            Self {
                recorded: 64,
                accessed: 72,
                modified: 84,
                changed: 96,
                created: None,
                object_size: None,
                unique: 160,
                lengths: 168,
                fixed: FE_FIXED,
            }
        }
    }
}

impl<B: Block> Pages for Udf<B> {
    fn read_page(&mut self, at: LbAddr, out: &mut [u8]) -> Result<(), DriverError> {
        self.read_block(at, out)
    }

    fn write_page(&mut self, at: LbAddr, bytes: &[u8]) -> Result<(), DriverError> {
        // The bitmap's pages always match the bits it holds, so writing one
        // changes no file's record.
        self.write_sectors(at, bytes)
    }
}

/// The UDF permission word for a POSIX mode. Each UDF class keeps execute,
/// write and read in the bit order POSIX does, then the rights to change
/// attributes and to delete, which follow its write.
fn permissions_of(mode: u32) -> u32 {
    let class = |shift: u32| {
        let rwx = (mode >> shift) & 0b111;
        let write = (rwx >> 1) & 1;
        rwx | write << 3 | write << 4
    };
    class(0) | class(3) << 5 | class(6) << 10
}

/// The ICB flags a POSIX mode sets beyond the descriptor form.
fn mode_flags(mode: u32) -> u16 {
    let mut flags = 0;
    for (bit, flag) in [
        (0o4000, entry::FLAG_SETUID),
        (0o2000, entry::FLAG_SETGID),
        (0o1000, entry::FLAG_STICKY),
    ] {
        if mode & bit != 0 {
            flags |= flag;
        }
    }
    flags
}

impl<B: Block> Udf<B> {
    /// Write `bytes`, whole blocks, from `addr`.
    fn write_block(&mut self, addr: LbAddr, bytes: &[u8]) -> Result<(), DriverError> {
        self.write.writes = self.write.writes.wrapping_add(1);
        self.write_sectors(addr, bytes)
    }

    fn write_sectors(&mut self, addr: LbAddr, bytes: &[u8]) -> Result<(), DriverError> {
        let sector = self.partitions.sector(addr)?;
        let at = sector
            .checked_mul(u64::from(self.block_size))
            .ok_or(DriverError::DeviceFault)?;
        self.dev.write(at, bytes)
    }

    fn now(&self) -> Option<Time64> {
        self.host.and_then(VolumeHost::wall_time)
    }

    /// Run `body` as one change to the volume, then publish the volume — at
    /// once, unless the host's write-back timer will when its window closes.
    ///
    /// Every path undoes a change that runs out of space, so one that failed
    /// after writing for any other reason may be part-made: the volume is
    /// then damaged, refused further writes and left marked open.
    pub(crate) fn change<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        self.begin()?;
        let writes = self.write.writes;
        let outcome = body(self);
        if matches!(outcome, Err(err) if err != DriverError::NoSpace) && self.write.writes != writes
        {
            self.refusal = Some(DAMAGED);
        }
        let now = self.refusal.is_some()
            || !self.write.schedule.defers()
            || self.write.schedule.expired();
        let published = if now {
            self.publish()
        } else {
            if outcome.is_ok() {
                self.write.schedule.joined();
            }
            Ok(())
        };
        let value = outcome?;
        published?;
        Ok(value)
    }

    /// Ready the volume for a change: refuse one it may not take, open the
    /// space bitmap, and mark the integrity descriptor open on the disc
    /// before anything else is written.
    fn begin(&mut self) -> Result<(), DriverError> {
        if self.refusal.is_some() {
            return Err(DriverError::ReadOnly);
        }
        self.file = None;
        if self.write.space.is_none() {
            let Some(Map::Physical(physical)) = self.partitions.maps.first().cloned() else {
                return Err(DriverError::ReadOnly);
            };
            let start = LbAddr {
                block: physical.bitmap.position,
                partition: PARTITION,
            };
            let recorded = self
                .integrity
                .free_table
                .first()
                .copied()
                .filter(|&free| free != u32::MAX);
            let space = Space::open(self, start, self.block_size, physical.length, recorded)?;
            self.write.space = Some(space);
        }
        if !self.write.open {
            self.write_integrity(true)?;
            self.dev.flush()?;
            self.write.open = true;
            self.write.schedule.opened();
        }
        Ok(())
    }

    /// Publish the open change: the space bitmap written back and the
    /// integrity descriptor closed — but a damaged volume's left open, so it
    /// is mounted read-only until it is checked.
    fn publish(&mut self) -> Result<(), DriverError> {
        if !self.write.open {
            return Ok(());
        }
        self.with_space(Space::write_back)?;
        self.dev.flush()?;
        if self.refusal.is_none() {
            self.write_integrity(false)?;
            self.dev.flush()?;
        }
        self.write.open = false;
        self.write.schedule.closed();
        Ok(())
    }

    fn write_integrity(&mut self, open: bool) -> Result<(), DriverError> {
        if let Some(space) = &self.write.space {
            let free = u32::try_from(space.free()).unwrap_or(u32::MAX);
            if let Some(slot) = self.integrity.free_table.first_mut() {
                *slot = free;
            }
            self.integrity.free_blocks = u64::from(free);
        }
        let (sector, raw) = self
            .integrity
            .encoded(open, self.now(), crate::WRITE_REVISION)?;
        let at = sector
            .checked_mul(u64::from(self.block_size))
            .ok_or(DriverError::DeviceFault)?;
        self.dev.write(at, &raw)?;
        self.integrity.closed = !open;
        Ok(())
    }

    /// Run `f` with the space bitmap, the volume as its pages.
    fn with_space<R>(
        &mut self,
        f: impl FnOnce(&mut Space, &mut Self) -> Result<R, DriverError>,
    ) -> Result<R, DriverError> {
        let mut space = self.write.space.take().ok_or(DriverError::ReadOnly)?;
        let result = f(&mut space, self);
        self.write.space = Some(space);
        result
    }

    /// Take up to `want` blocks in one run near `goal`.
    fn allocate(&mut self, want: u32, goal: Option<u32>) -> Result<(u32, u32), DriverError> {
        self.with_space(|space, fs| space.allocate(fs, want, goal))
    }

    fn release(&mut self, first: u32, count: u32) -> Result<(), DriverError> {
        if count == 0 {
            return Ok(());
        }
        self.with_space(|space, fs| space.release(fs, first, count))
    }

    /// Free runs of blocks, each its first block and count: all of them, or
    /// none.
    fn release_runs(&mut self, runs: &[(u32, u32)]) -> Result<(), DriverError> {
        for (done, &(first, count)) in runs.iter().enumerate() {
            if let Err(err) = self.release(first, count) {
                self.retake_runs(&runs[..done])?;
                return Err(err);
            }
        }
        Ok(())
    }

    /// Take back runs [`Self::release_runs`] freed.
    fn retake_runs(&mut self, runs: &[(u32, u32)]) -> Result<(), DriverError> {
        for &(first, count) in runs {
            self.with_space(|space, fs| space.retake(fs, first, count))?;
        }
        Ok(())
    }

    /// Record `extents` as the entry of a file that no longer names the
    /// `freed` runs, those runs free to hold its continuations: freed first,
    /// and taken back should the descriptors still not fit, so a volume
    /// without the space is left as it was.
    fn store_shrunk(
        &mut self,
        node: &mut Resolved,
        extents: &[Extent],
        chain: Vec<LbAddr>,
        freed: &[(u32, u32)],
    ) -> Result<(), DriverError> {
        self.release_runs(freed)?;
        match self.prepare_descriptors(node, extents, chain) {
            Ok(descriptors) => self.commit_descriptors(node, descriptors),
            Err(err) => {
                self.retake_runs(freed)?;
                Err(err)
            }
        }
    }

    /// The next unique id, as UDF 2.60 §3.2.1.1 issues them: its low 32 bits
    /// never 0 to 15. Refused as no space once every id has been issued.
    fn next_unique_id(&mut self) -> Result<u64, DriverError> {
        let id = lift_unique(self.integrity.next_unique_id);
        let next = id
            .checked_add(1)
            .map(lift_unique)
            .ok_or(DriverError::NoSpace)?;
        self.integrity.next_unique_id = next;
        Ok(id)
    }

    /// The tag a new descriptor of kind `id` at `location` is sealed with:
    /// the volume's descriptor version and serial.
    fn tag(&self, id: u16, location: u32) -> Tag {
        Tag {
            id,
            version: u16::from(self.volume.nsr),
            serial: 0,
            location,
        }
    }

    /// A new entry of `file_type` at `addr` with unique id `unique`: an
    /// Extended File Entry on a volume of UDF 2.00 or later, a File Entry
    /// before, its descriptors short and none yet.
    fn new_entry(
        &self,
        addr: LbAddr,
        file_type: u8,
        mode: u32,
        unique: u64,
    ) -> Result<Vec<u8>, DriverError> {
        let extended = self.volume.logical().revision >= 0x0200;
        let layout = Layout::of(extended);
        let mut block = zeroed(self.block_size)?;
        // ICB tag: strategy 4, one entry, the type, short descriptors.
        block[20..22].copy_from_slice(&entry::STRATEGY_4.to_le_bytes());
        block[24..26].copy_from_slice(&1u16.to_le_bytes());
        block[27] = file_type;
        block[34..36].copy_from_slice(&mode_flags(mode).to_le_bytes());
        block[36..40].copy_from_slice(&UNSPECIFIED_ID.to_le_bytes());
        block[40..44].copy_from_slice(&UNSPECIFIED_ID.to_le_bytes());
        block[44..48].copy_from_slice(&permissions_of(mode).to_le_bytes());
        block[48..50].copy_from_slice(&1u16.to_le_bytes());
        let stamp = self.now().and_then(encode_timestamp).unwrap_or([0; 12]);
        for at in [
            Some(layout.accessed),
            Some(layout.modified),
            Some(layout.changed),
            layout.created,
        ]
        .into_iter()
        .flatten()
        {
            block[at..at + 12].copy_from_slice(&stamp);
        }
        let checkpoint = if extended { 128 } else { 108 };
        block[checkpoint..checkpoint + 4].copy_from_slice(&1u32.to_le_bytes());
        let implementation = if extended { 168 } else { 128 };
        block[implementation + 1..implementation + 1 + IMPLEMENTATION.len()]
            .copy_from_slice(IMPLEMENTATION);
        block[layout.unique..layout.unique + 8].copy_from_slice(&unique.to_le_bytes());
        let id = if extended { TAG_EFE } else { TAG_FE };
        self.tag(id, addr.block)
            .seal(&mut block, layout.fixed - TAG_LEN)?;
        Ok(block)
    }

    /// Re-seal and write a resolved entry whose block was changed.
    fn store_entry(&mut self, node: &mut Resolved) -> Result<(), DriverError> {
        let layout = Layout::of(node.entry.extended);
        let f = Fields(&node.block);
        let ea = usize::try_from(f.u32(layout.lengths)?).map_err(|_| DriverError::DeviceFault)?;
        let ad =
            usize::try_from(f.u32(layout.lengths + 4)?).map_err(|_| DriverError::DeviceFault)?;
        let tag = Tag::checked(&node.block).unwrap_or_else(|| {
            self.tag(
                if node.entry.extended { TAG_EFE } else { TAG_FE },
                node.addr.block,
            )
        });
        tag.seal(&mut node.block, layout.fixed + ea + ad - TAG_LEN)?;
        let block = core::mem::take(&mut node.block);
        let written = self.write_block(node.addr, &block);
        node.block = block;
        written?;
        node.entry = entry::Entry::parse(&node.block, node.addr.block)?;
        Ok(())
    }

    /// Stamp a resolved entry's modification and attribute times.
    fn touch(&self, node: &mut Resolved, content: bool) {
        let Some(stamp) = self.now().and_then(encode_timestamp) else {
            return;
        };
        let layout = Layout::of(node.entry.extended);
        node.block[layout.changed..layout.changed + 12].copy_from_slice(&stamp);
        if content {
            node.block[layout.modified..layout.modified + 12].copy_from_slice(&stamp);
        }
    }

    fn set_size(node: &mut Resolved, size: u64) {
        node.block[56..64].copy_from_slice(&size.to_le_bytes());
        if let Some(at) = Layout::of(node.entry.extended).object_size {
            node.block[at..at + 8].copy_from_slice(&size.to_le_bytes());
        }
    }

    fn set_links(node: &mut Resolved, links: u16) {
        node.block[48..50].copy_from_slice(&links.to_le_bytes());
    }

    /// The node `node` names, resolved for a write as a node of `kind` — one
    /// of another kind refused before anything changes — an embedded node's
    /// bytes moved into blocks of their own first, so every write works on
    /// extents.
    fn writable(
        &mut self,
        node: NodeId,
        kind: NodeKind,
    ) -> Result<(Resolved, Vec<Extent>, Vec<LbAddr>), DriverError> {
        let mut resolved = self.rewritable(node)?;
        if kind_of(resolved.entry.file_type) != Some(kind) {
            return Err(DriverError::Unsupported);
        }
        if resolved.entry.form == AdForm::Embedded {
            self.unembed(&mut resolved)?;
        }
        let (extents, chain) = self.allocation_of(&resolved)?;
        Ok((resolved, extents, chain))
    }

    /// The entry `node` names, where it may be rewritten in place: an entry
    /// kept as a chain is write-once media's.
    fn rewritable(&mut self, node: NodeId) -> Result<Resolved, DriverError> {
        let addr = addr_of(node).ok_or(DriverError::NotFound)?;
        let resolved = self.resolve(addr)?;
        if resolved.addr != addr || entry::Entry::strategy(&resolved.block)? != entry::STRATEGY_4 {
            return Err(DriverError::ReadOnly);
        }
        Ok(resolved)
    }

    /// The directory `dir`, resolved to have its descriptors rewritten.
    fn rewritable_dir(&mut self, dir: NodeId) -> Result<Resolved, DriverError> {
        let resolved = self.rewritable(dir)?;
        if kind_of(resolved.entry.file_type) != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        Ok(resolved)
    }

    /// Move an embedded node's bytes into blocks of their own, its content
    /// unchanged.
    fn unembed(&mut self, resolved: &mut Resolved) -> Result<(), DriverError> {
        let held = usize::try_from(resolved.entry.size)
            .ok()
            .and_then(|size| {
                resolved
                    .block
                    .get(resolved.entry.ad.start..resolved.entry.ad.start.checked_add(size)?)
            })
            .ok_or(DriverError::DeviceFault)?;
        let mut bytes = zeroed_len(held.len())?;
        bytes.copy_from_slice(held);
        let flags = (resolved.entry.flags & !entry::FLAG_AD_MASK).to_le_bytes();
        resolved.block[34..36].copy_from_slice(&flags);
        let layout = Layout::of(resolved.entry.extended);
        resolved.block[layout.lengths + 4..layout.lengths + 8].copy_from_slice(&0u32.to_le_bytes());
        let ad = resolved.entry.ad.clone();
        resolved.block[ad].fill(0);
        if bytes.is_empty() {
            return self.store_extents(resolved, &[], Vec::new());
        }
        self.write_content(resolved, &[], Vec::new(), 0, &bytes)
    }

    /// A node's extents and continuation blocks, each extent rounded to the
    /// whole blocks it holds: only a file's last extent may stop short of its
    /// last block, and [`Self::store_extents`] cuts it to the file's length
    /// again. Every block they name lies in the volume's one partition, so no
    /// change made from them reaches past it.
    fn allocation_of(
        &mut self,
        resolved: &Resolved,
    ) -> Result<(Vec<Extent>, Vec<LbAddr>), DriverError> {
        let block_size = self.block_size;
        let (mut extents, chain) = entry::allocation(
            &resolved.block,
            &resolved.entry,
            resolved.addr.partition,
            |addr, out| {
                if out.len() != block_size as usize {
                    *out = zeroed(block_size)?;
                }
                self.read_block(addr, out)
            },
        )?;
        let length = self
            .partitions
            .physical(PARTITION)
            .ok_or(DriverError::ReadOnly)?
            .length;
        let inside = |start: LbAddr, blocks: u32| {
            start.partition == PARTITION
                && start
                    .block
                    .checked_add(blocks)
                    .is_some_and(|end| end <= length)
        };
        for extent in &mut extents {
            extent.bytes = extent
                .bytes
                .checked_next_multiple_of(block_size)
                .filter(|&bytes| bytes < 1 << 30)
                .ok_or(DriverError::DeviceFault)?;
            if extent.holds != Holds::Hole && !inside(extent.start, extent.bytes / block_size) {
                return Err(DriverError::DeviceFault);
            }
        }
        if chain.iter().any(|&block| !inside(block, 1)) {
            return Err(DriverError::DeviceFault);
        }
        Ok((extents, chain))
    }

    /// Record `extents` as the entry's allocation descriptors and write it,
    /// as [`Self::prepare_descriptors`] and [`Self::commit_descriptors`] do.
    fn store_extents(
        &mut self,
        node: &mut Resolved,
        extents: &[Extent],
        old_chain: Vec<LbAddr>,
    ) -> Result<(), DriverError> {
        let descriptors = self.prepare_descriptors(node, extents, old_chain)?;
        self.commit_descriptors(node, descriptors)
    }

    /// Lay `extents` out as the entry's allocation descriptors — in the entry
    /// while they fit, the rest in allocation extent descriptors — taking
    /// every continuation block they need, the old chain's first, before
    /// anything is written. A volume without the space is left as it was.
    fn prepare_descriptors(
        &mut self,
        node: &Resolved,
        extents: &[Extent],
        old_chain: Vec<LbAddr>,
    ) -> Result<Descriptors, DriverError> {
        let layout = Layout::of(node.entry.extended);
        let bs = self.block_size as usize;
        let room = bs
            .checked_sub(layout.fixed + node.entry.ea.len())
            .ok_or(DriverError::DeviceFault)?;
        let size = Fields(&node.block).u64(56)?;
        let extents = fit(extents, size)?;
        // A length past 30 bits would spill into the descriptor's type.
        if extents.iter().any(|extent| extent.bytes >= 1 << 30) {
            return Err(DriverError::DeviceFault);
        }
        let (in_entry, counts) =
            split(extents.len(), room / SHORT_AD, (bs - AED_FIXED) / SHORT_AD)?;
        let mut continuations = Vec::new();
        if !tairix_util::fallible::reserve(&mut continuations, counts.len()) {
            return Err(DriverError::OutOfMemory);
        }
        let mut reused = old_chain.into_iter();
        let mut taken = Vec::new();
        let mut goal = node.addr.block;
        for count in counts {
            let block = match reused.next() {
                Some(old) => old.block,
                None => match self.allocate(1, Some(goal)) {
                    Ok((block, _)) if tairix_util::fallible::push(&mut taken, block) => block,
                    outcome => {
                        if let Ok((block, _)) = outcome {
                            self.release(block, 1)?;
                        }
                        for block in taken {
                            self.release(block, 1)?;
                        }
                        return Err(outcome.err().unwrap_or(DriverError::OutOfMemory));
                    }
                },
            };
            goal = block;
            continuations.push((block, count));
        }
        let data_blocks: u64 = extents
            .iter()
            .filter(|extent| extent.holds != Holds::Hole)
            .map(|extent| u64::from(extent.bytes).div_ceil(u64::from(self.block_size)))
            .sum();
        Ok(Descriptors {
            recorded: data_blocks + continuations.len() as u64,
            extents,
            in_entry,
            continuations,
            taken,
            spare: reused.collect(),
        })
    }

    /// Give back the continuation blocks prepared descriptors took, the write
    /// they were for abandoned.
    fn abandon(&mut self, descriptors: Descriptors) -> Result<(), DriverError> {
        for block in descriptors.taken {
            self.release(block, 1)?;
        }
        Ok(())
    }

    /// Write prepared descriptors and the entry recording them, then free the
    /// old chain's blocks they no longer need.
    fn commit_descriptors(
        &mut self,
        node: &mut Resolved,
        descriptors: Descriptors,
    ) -> Result<(), DriverError> {
        let Descriptors {
            extents,
            in_entry,
            continuations,
            spare,
            recorded,
            ..
        } = descriptors;
        let layout = Layout::of(node.entry.extended);
        let block_size = self.block_size;
        let link = |at: usize| {
            continuations
                .get(at)
                .map(|&(block, _)| continuation_ad(block, block_size))
        };
        // Every buffer is had before the first write, so only the device can
        // fail the commit part-way.
        let mut area = Vec::new();
        if !tairix_util::fallible::reserve(&mut area, (in_entry + 1) * SHORT_AD) {
            return Err(DriverError::OutOfMemory);
        }
        let mut aed = if continuations.is_empty() {
            Vec::new()
        } else {
            zeroed(block_size)?
        };
        for extent in &extents[..in_entry] {
            area.extend_from_slice(&short_ad(extent));
        }
        area.extend(link(0).into_iter().flatten());
        let ad_start = layout.fixed + node.entry.ea.len();
        let ad_end = ad_start
            .checked_add(area.len())
            .filter(|&end| end <= node.block.len())
            .ok_or(DriverError::DeviceFault)?;
        node.block[ad_start..].fill(0);
        node.block[ad_start..ad_end].copy_from_slice(&area);
        let area_len = u32::try_from(area.len()).map_err(|_| DriverError::DeviceFault)?;
        node.block[layout.lengths + 4..layout.lengths + 8].copy_from_slice(&area_len.to_le_bytes());
        let flags = (node.entry.flags & !entry::FLAG_AD_MASK).to_le_bytes();
        node.block[34..36].copy_from_slice(&flags);
        let mut rest = &extents[in_entry..];
        let mut previous = node.addr.block;
        for (index, &(at, take)) in continuations.iter().enumerate() {
            aed.fill(0);
            aed[16..20].copy_from_slice(&previous.to_le_bytes());
            let mut offset = AED_FIXED;
            for extent in &rest[..take] {
                aed[offset..offset + SHORT_AD].copy_from_slice(&short_ad(extent));
                offset += SHORT_AD;
            }
            rest = &rest[take..];
            if let Some(next) = link(index + 1) {
                aed[offset..offset + SHORT_AD].copy_from_slice(&next);
                offset += SHORT_AD;
            }
            let len = u32::try_from(offset - AED_FIXED).map_err(|_| DriverError::DeviceFault)?;
            aed[20..24].copy_from_slice(&len.to_le_bytes());
            self.tag(TAG_AED, at).seal(&mut aed, offset - TAG_LEN)?;
            self.write_block(
                LbAddr {
                    block: at,
                    partition: node.addr.partition,
                },
                &aed,
            )?;
            previous = at;
        }
        node.block[layout.recorded..layout.recorded + 8].copy_from_slice(&recorded.to_le_bytes());
        self.store_entry(node)?;
        // The entry names only the new descriptors now: an old block that
        // cannot be given back is space lost, not a change undone.
        for block in spare {
            if self.release(block.block, 1).is_err() {
                self.refusal = Some(DAMAGED);
            }
        }
        Ok(())
    }

    /// Write `data` at `offset` of the file `node` mapped as `extents`,
    /// giving any hole it covers blocks of its own, and record the file.
    fn write_range(
        &mut self,
        node: &mut Resolved,
        extents: &[Extent],
        chain: Vec<LbAddr>,
        offset: u64,
        data: &[u8],
    ) -> Result<(), DriverError> {
        if data.is_empty() {
            return Ok(());
        }
        self.touch(node, true);
        self.write_content(node, extents, chain, offset, data)
    }

    /// Write `data` at `offset` of the file `node` mapped as `extents` and
    /// record its new map, every block it needs — for data and descriptors
    /// alike — taken before any is written, so a volume without them is left
    /// as it was.
    fn write_content(
        &mut self,
        node: &mut Resolved,
        extents: &[Extent],
        chain: Vec<LbAddr>,
        offset: u64,
        data: &[u8],
    ) -> Result<(), DriverError> {
        if data.is_empty() {
            return Ok(());
        }
        let (first, last) = self.block_span(offset, data.len())?;
        let (placed, taken) = self.blocks(node.addr, extents, first, last)?;
        self.write_placed(node, &placed, &taken, chain, offset, data)
    }

    /// The first and last block of `len` bytes at `offset`, `len` not zero.
    fn block_span(&self, offset: u64, len: usize) -> Result<(u64, u64), DriverError> {
        let bs = u64::from(self.block_size);
        let end = offset
            .checked_add(len as u64)
            .ok_or(DriverError::OutOfRange)?;
        Ok((offset / bs, (end - 1) / bs))
    }

    /// Write `data` at `offset` through `placed` — the file's map, the blocks
    /// the write needs already taken, those newly so in `taken` — and record
    /// the map. The continuation blocks are taken before anything is
    /// written; failing that, `taken` goes back and the volume is as it was.
    fn write_placed(
        &mut self,
        node: &mut Resolved,
        placed: &[Extent],
        taken: &[Extent],
        chain: Vec<LbAddr>,
        offset: u64,
        data: &[u8],
    ) -> Result<(), DriverError> {
        let (first, last) = self.block_span(offset, data.len())?;
        let end = offset + data.len() as u64;
        let recorded = written_blocks(placed, first, last, self.block_size);
        let size = node.entry.size;
        Self::set_size(node, size.max(end));
        let descriptors =
            match recorded.and_then(|recorded| self.prepare_descriptors(node, &recorded, chain)) {
                Ok(descriptors) => descriptors,
                Err(err) => {
                    Self::set_size(node, size);
                    self.give_back(taken)?;
                    return Err(err);
                }
            };
        if let Err(err) = self.write_data(placed, offset, data) {
            self.abandon(descriptors)?;
            self.give_back(taken)?;
            return Err(err);
        }
        self.commit_descriptors(node, descriptors)
    }

    /// Write `data` at `offset` through the blocks `placed` maps: whole
    /// blocks in runs as long as the device lays them end to end, a partial
    /// block merged with what it holds.
    fn write_data(
        &mut self,
        placed: &[Extent],
        offset: u64,
        data: &[u8],
    ) -> Result<(), DriverError> {
        let bs = u64::from(self.block_size);
        let block_len = self.block_size as usize;
        let mapped = Mapped::of(copied(placed)?)?;
        let mut block_buf = zeroed(self.block_size)?;
        let mut done = 0usize;
        while done < data.len() {
            let at = offset + done as u64;
            let (index, within) = mapped.find(at).ok_or(DriverError::DeviceFault)?;
            let extent = mapped.extents[index];
            let addr = block_at(&extent, within / bs)?;
            let skip = usize::try_from(within % bs).map_err(|_| DriverError::DeviceFault)?;
            let left = data.len() - done;
            if skip == 0 && left >= block_len {
                let in_extent = (u64::from(extent.bytes) - within) / bs;
                let wanted = u32::try_from((left / block_len) as u64)
                    .unwrap_or(u32::MAX)
                    .min(u32::try_from(in_extent).unwrap_or(u32::MAX))
                    .max(1);
                let run = self.partitions.contiguous(addr, wanted)? as usize * block_len;
                self.write_block(addr, &data[done..done + run])?;
                done += run;
                continue;
            }
            let take = (block_len - skip).min(left);
            if extent.holds == Holds::Data {
                self.read_block(addr, &mut block_buf)?;
            } else {
                block_buf.fill(0);
            }
            block_buf[skip..skip + take].copy_from_slice(&data[done..done + take]);
            self.write_block(addr, &block_buf)?;
            done += take;
        }
        Ok(())
    }

    /// Return newly taken runs to the free pool.
    fn give_back(&mut self, taken: &[Extent]) -> Result<(), DriverError> {
        for run in taken {
            self.release(run.start.block, run.bytes / self.block_size)?;
        }
        Ok(())
    }

    /// `extents` with every block from `first` to `last` allocated: a hole,
    /// or the space past the file's last extent, given blocks of its own,
    /// near the blocks before it — and the runs newly taken. A request that
    /// cannot be met in full takes nothing.
    fn blocks(
        &mut self,
        entry: LbAddr,
        extents: &[Extent],
        first: u64,
        last: u64,
    ) -> Result<(Vec<Extent>, Vec<Extent>), DriverError> {
        let mut taken = Vec::new();
        match self.place(entry, extents, first, last, &mut taken) {
            Ok(placed) => Ok((placed, taken)),
            Err(err) => {
                self.give_back(&taken)?;
                Err(err)
            }
        }
    }

    fn place(
        &mut self,
        entry: LbAddr,
        extents: &[Extent],
        first: u64,
        last: u64,
        taken: &mut Vec<Extent>,
    ) -> Result<Vec<Extent>, DriverError> {
        let bs = u64::from(self.block_size);
        let mut out = Vec::new();
        let mut at = 0u64;
        let mut goal = Some(entry.block.saturating_add(1));
        for extent in extents {
            let blocks = u64::from(extent.bytes).div_ceil(bs);
            let span = at..at + blocks;
            if extent.holds != Holds::Hole || span.end <= first || span.start > last {
                if extent.holds != Holds::Hole {
                    goal = u32::try_from(blocks)
                        .ok()
                        .and_then(|blocks| extent.start.block.checked_add(blocks));
                }
                push(&mut out, *extent)?;
                at = span.end;
                continue;
            }
            // Split the hole around the blocks the write covers.
            let from = first.max(span.start);
            let to = (last + 1).min(span.end);
            push_hole(&mut out, from - span.start, self.block_size)?;
            for run in self.runs(to - from, &mut goal, taken)? {
                push(&mut out, run)?;
            }
            push_hole(&mut out, span.end - to, self.block_size)?;
            at = span.end;
        }
        if last >= at {
            push_hole(&mut out, first.saturating_sub(at), self.block_size)?;
            at = at.max(first);
            for run in self.runs(last + 1 - at, &mut goal, taken)? {
                push(&mut out, run)?;
            }
        }
        Ok(merged(out, self.block_size))
    }

    /// `count` newly allocated blocks, as unwritten extents, the first near
    /// `goal`; each run is also recorded in `taken`, for a caller that must
    /// give them back.
    fn runs(
        &mut self,
        mut count: u64,
        goal: &mut Option<u32>,
        taken: &mut Vec<Extent>,
    ) -> Result<Vec<Extent>, DriverError> {
        let mut out = Vec::new();
        while count > 0 {
            let want =
                u32::try_from(count.min(u64::from(EXTENT_MAX / self.block_size))).unwrap_or(1);
            let (start, len) = self.allocate(want, *goal)?;
            let run = Extent {
                holds: Holds::Unwritten,
                bytes: len * self.block_size,
                start: LbAddr {
                    block: start,
                    partition: PARTITION,
                },
            };
            if !tairix_util::fallible::push(taken, run) {
                self.release(start, len)?;
                return Err(DriverError::OutOfMemory);
            }
            push(&mut out, run)?;
            *goal = start.checked_add(len);
            count -= u64::from(len);
        }
        Ok(out)
    }

    /// Free every block `extents` allocated, and the continuation blocks.
    fn free_extents(&mut self, extents: &[Extent], chain: &[LbAddr]) -> Result<(), DriverError> {
        for extent in extents {
            if extent.holds != Holds::Hole {
                self.release(extent.start.block, extent.bytes.div_ceil(self.block_size))?;
            }
        }
        for block in chain {
            self.release(block.block, 1)?;
        }
        Ok(())
    }

    /// The live descriptor naming `name` in the directory `dir`: its offset
    /// in the directory's stream and the descriptor.
    fn find(&mut self, dir: &Resolved, name: &[u8]) -> Result<Option<(u64, Fid)>, DriverError> {
        let mut found = None;
        self.for_each_fid(dir, &mut |at, fid| {
            if fid.characteristics & (CHAR_DELETED | CHAR_PARENT) != 0 {
                return Ok(true);
            }
            if dir::decode(&fid.identifier).is_some_and(|listed| listed.as_bytes() == name) {
                found = Some((at, fid.clone()));
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(found)
    }

    /// Hand `visit` every descriptor of `dir` with its offset, live or not,
    /// until it answers `false`.
    fn for_each_fid(
        &mut self,
        dir: &Resolved,
        visit: &mut dyn FnMut(u64, &Fid) -> Result<bool, DriverError>,
    ) -> Result<(), DriverError> {
        let mut reader = self.dir_reader(dir)?;
        let mut at = 0;
        while let Some((fid, next)) = self.next_fid(dir, &mut reader, at)? {
            if !visit(at, &fid)? {
                return Ok(());
            }
            at = next;
        }
        Ok(())
    }

    /// Append a descriptor naming `icb` by `identifier` to the directory
    /// `dir`.
    fn add_fid(
        &mut self,
        dir: NodeId,
        identifier: &[u8],
        icb: LbAddr,
        unique: u64,
        characteristics: u8,
    ) -> Result<(), DriverError> {
        let (mut node, extents, chain) = self.writable(dir, NodeKind::Directory)?;
        self.append_fid(
            &mut node,
            &extents,
            chain,
            identifier,
            icb,
            unique,
            characteristics,
        )
    }

    /// Append a descriptor to the directory `node`, its blocks allocated
    /// first so its tag can name the block it starts in.
    #[allow(clippy::too_many_arguments)] // The directory, and the fields of one descriptor.
    fn append_fid(
        &mut self,
        node: &mut Resolved,
        extents: &[Extent],
        chain: Vec<LbAddr>,
        identifier: &[u8],
        icb: LbAddr,
        unique: u64,
        characteristics: u8,
    ) -> Result<(), DriverError> {
        let at = node.entry.size;
        let len = (FID_FIXED + identifier.len()).next_multiple_of(4);
        let (first, last) = self.block_span(at, len)?;
        let (placed, taken) = self.blocks(node.addr, extents, first, last)?;
        let fid = self.new_fid(&placed, at, len, identifier, icb, unique, characteristics);
        let bytes = match fid {
            Ok(bytes) => bytes,
            Err(err) => {
                self.give_back(&taken)?;
                return Err(err);
            }
        };
        self.touch(node, true);
        self.write_placed(node, &placed, &taken, chain, at, &bytes)
    }

    /// A descriptor of `len` bytes at `at` of a directory stream `placed`
    /// maps, its tag naming the block it starts in.
    #[allow(clippy::too_many_arguments)] // Where it lies, and the fields of one descriptor.
    fn new_fid(
        &self,
        placed: &[Extent],
        at: u64,
        len: usize,
        identifier: &[u8],
        icb: LbAddr,
        unique: u64,
        characteristics: u8,
    ) -> Result<Vec<u8>, DriverError> {
        let bs = u64::from(self.block_size);
        let mapped = Mapped::of(copied(placed)?)?;
        let (index, within) = mapped.find(at).ok_or(DriverError::DeviceFault)?;
        let location = block_at(&mapped.extents[index], within / bs)?.block;
        let mut bytes = zeroed_len(len)?;
        bytes[16..18].copy_from_slice(&1u16.to_le_bytes());
        bytes[19] = u8::try_from(identifier.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        bytes[FID_FIXED..FID_FIXED + identifier.len()].copy_from_slice(identifier);
        self.name_node(&mut bytes, icb, unique, characteristics);
        self.tag(TAG_FID, location)
            .seal(&mut bytes, len - TAG_LEN)?;
        Ok(bytes)
    }

    /// Point the descriptor `bytes` at the node `icb`: its characteristics,
    /// its ICB, and the low half of its unique id, as UDF 2.00 records it.
    fn name_node(&self, bytes: &mut [u8], icb: LbAddr, unique: u64, characteristics: u8) {
        bytes[18] = characteristics;
        bytes[20..24].copy_from_slice(&self.block_size.to_le_bytes());
        bytes[24..28].copy_from_slice(&icb.block.to_le_bytes());
        bytes[28..30].copy_from_slice(&icb.partition.to_le_bytes());
        bytes[30..32].fill(0);
        bytes[32..36].copy_from_slice(
            &u32::try_from(unique & 0xFFFF_FFFF)
                .unwrap_or(0)
                .to_le_bytes(),
        );
    }

    /// The descriptor at byte `offset` of the directory `node`'s stream,
    /// whole, its tag checked.
    fn fid_bytes(&mut self, node: &Resolved, offset: u64) -> Result<(Vec<u8>, Tag), DriverError> {
        let mapped = Mapped::of(self.extents_of(node)?)?;
        let mut header = [0u8; FID_FIXED];
        self.read_content(node, &mapped, offset, &mut header)?;
        let len = Fid::length(&header)?;
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > node.entry.size)
        {
            return Err(DriverError::DeviceFault);
        }
        let mut bytes = zeroed_len(len)?;
        self.read_content(node, &mapped, offset, &mut bytes)?;
        let tag = Tag::checked(&bytes).ok_or(DriverError::DeviceFault)?;
        Ok((bytes, tag))
    }

    /// Write `bytes` over the stream of `node` at `offset`, where it already
    /// holds as many: embedded bytes in its entry, so a directory takes no
    /// space to change a descriptor it holds.
    fn overwrite(
        &mut self,
        node: &mut Resolved,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), DriverError> {
        let len = bytes.len() as u64;
        if offset
            .checked_add(len)
            .is_none_or(|end| end > node.entry.size)
        {
            return Err(DriverError::DeviceFault);
        }
        if node.entry.form == AdForm::Embedded {
            // The entry's size lies within its area, which lies within its block.
            let start = node.entry.ad.start
                + usize::try_from(offset).map_err(|_| DriverError::DeviceFault)?;
            node.block[start..start + bytes.len()].copy_from_slice(bytes);
            self.touch(node, true);
            return self.store_entry(node);
        }
        let (extents, chain) = self.allocation_of(node)?;
        self.write_range(node, &extents, chain, offset, bytes)
    }

    /// Mark the descriptor at `offset` of `dir`'s stream deleted.
    fn delete_fid(&mut self, dir: NodeId, offset: u64) -> Result<(), DriverError> {
        let mut node = self.rewritable_dir(dir)?;
        let (mut bytes, tag) = self.fid_bytes(&node, offset)?;
        bytes[18] |= CHAR_DELETED;
        let len = bytes.len();
        tag.seal(&mut bytes, len - TAG_LEN)?;
        self.overwrite(&mut node, offset, &bytes)
    }

    /// Point the descriptor at `offset` of `dir`'s stream at the node `icb`,
    /// its name kept where it lies.
    fn repoint_fid(
        &mut self,
        dir: NodeId,
        offset: u64,
        icb: LbAddr,
        unique: u64,
        characteristics: u8,
    ) -> Result<(), DriverError> {
        let mut node = self.rewritable_dir(dir)?;
        let (mut bytes, tag) = self.fid_bytes(&node, offset)?;
        self.name_node(&mut bytes, icb, unique, characteristics);
        let len = bytes.len();
        tag.seal(&mut bytes, len - TAG_LEN)?;
        self.overwrite(&mut node, offset, &bytes)
    }

    /// Drop one name from the resolved node `node`: its count falls, and at
    /// none the node and everything it holds are freed.
    fn unlink_node(&mut self, mut node: Resolved) -> Result<(), DriverError> {
        let links = node.entry.links.saturating_sub(1);
        let is_dir = node.entry.file_type == entry::TYPE_DIRECTORY;
        if links > 0 && !is_dir {
            Self::set_links(&mut node, links);
            self.touch(&mut node, false);
            return self.store_entry(&mut node);
        }
        self.free_entry(&node, 0)?;
        if is_dir {
            self.integrity.directories = self.integrity.directories.saturating_sub(1);
        } else {
            self.integrity.files = self.integrity.files.saturating_sub(1);
        }
        Ok(())
    }

    /// Free a resolved entry and what only it holds: its blocks, its extended
    /// attribute file, and for a node itself (`depth` 0) its stream
    /// directory with the streams that names. Nothing nests deeper than a
    /// stream's attribute file.
    fn free_entry(&mut self, node: &Resolved, depth: u8) -> Result<(), DriverError> {
        let streams = node.entry.streams.filter(|_| depth == 0);
        if let Some(addr) = streams {
            let directory = self.resolve(addr)?;
            if directory.entry.file_type != entry::TYPE_STREAMS {
                return Err(DriverError::DeviceFault);
            }
            let mut named = Vec::new();
            let mut reader = self.descriptor_reader(&directory)?;
            let mut at = 0;
            while let Some((fid, next)) = self.next_fid(&directory, &mut reader, at)? {
                if fid.characteristics & (CHAR_DELETED | CHAR_PARENT) == 0 {
                    push(&mut named, fid.node())?;
                }
                at = next;
            }
            for addr in named {
                let stream = self.resolve(addr)?;
                self.free_entry(&stream, 1)?;
            }
            self.free_entry(&directory, 1)?;
        }
        if let Some(addr) = node.entry.ea_file.filter(|_| depth < 2) {
            let file = self.resolve(addr)?;
            if file.entry.file_type != entry::TYPE_ATTRIBUTES {
                return Err(DriverError::DeviceFault);
            }
            self.free_entry(&file, 2)?;
        }
        let (extents, chain) = self.allocation_of(node)?;
        self.free_extents(&extents, &chain)?;
        self.release(node.addr.block, 1)
    }

    /// Change the link count of the directory `dir` by `delta`.
    fn relink_dir(&mut self, dir: NodeId, delta: i32) -> Result<(), DriverError> {
        let mut node = self.rewritable_dir(dir)?;
        let links = i32::from(node.entry.links) + delta;
        Self::set_links(
            &mut node,
            u16::try_from(links.max(1)).map_err(|_| DriverError::TooManyLinks)?,
        );
        self.store_entry(&mut node)
    }

    /// Whether the directory `node` holds any live entry.
    fn has_entries(&mut self, node: &Resolved) -> Result<bool, DriverError> {
        let mut any = false;
        self.for_each_fid(node, &mut |_, fid| {
            any = fid.characteristics & (CHAR_DELETED | CHAR_PARENT) == 0;
            Ok(!any)
        })?;
        Ok(any)
    }

    /// Set a node's owner, group and mode.
    pub(crate) fn set_owner_mode(
        &mut self,
        node: NodeId,
        security: NodeSecurity,
    ) -> Result<(), DriverError> {
        let mut resolved = self.rewritable(node)?;
        resolved.block[36..40].copy_from_slice(&security.uid.to_le_bytes());
        resolved.block[40..44].copy_from_slice(&security.gid.to_le_bytes());
        resolved.block[44..48].copy_from_slice(&permissions_of(security.mode).to_le_bytes());
        let flags = (resolved.entry.flags
            & !(entry::FLAG_SETUID | entry::FLAG_SETGID | entry::FLAG_STICKY))
            | mode_flags(security.mode);
        resolved.block[34..36].copy_from_slice(&flags.to_le_bytes());
        self.touch(&mut resolved, false);
        self.store_entry(&mut resolved)
    }

    /// Set a node's Finder information to what `change` makes of what it
    /// records, told whether the node is a directory: where it records it,
    /// in its entry or its extended attribute file, else in its entry.
    pub(crate) fn update_finder(
        &mut self,
        node: NodeId,
        change: impl FnOnce(Option<FinderInfo>, bool) -> Result<FinderInfo, DriverError>,
    ) -> Result<(), DriverError> {
        let mut resolved = self.rewritable(node)?;
        let is_dir =
            kind_of(resolved.entry.file_type).ok_or(DriverError::NotFound)? == NodeKind::Directory;
        let held = resolved
            .block
            .get(resolved.entry.ea.clone())
            .ok_or(DriverError::DeviceFault)?;
        let mut space = zeroed_len(held.len())?;
        space.copy_from_slice(held);
        let tag = self.tag(TAG_EAHD, resolved.addr.block);
        let revision = self.volume.logical().revision;
        if let Some(at) = ea::find_finder(&space, tag.location)? {
            let finder = change(Some(ea::finder_at(&space, at, is_dir)?), is_dir)?;
            let rewritten = ea::with_finder(&space, tag, is_dir, finder, revision)?;
            let range = resolved.entry.ea.clone();
            resolved.block[range].copy_from_slice(&rewritten);
            self.touch(&mut resolved, false);
            return self.store_entry(&mut resolved);
        }
        if let Some((mut file, content, location)) = self.attribute_file(&resolved)? {
            if let Some(at) = ea::find_finder(&content, location)? {
                let finder = change(Some(ea::finder_at(&content, at, is_dir)?), is_dir)?;
                let rewritten =
                    ea::with_finder(&content, Tag { location, ..tag }, is_dir, finder, revision)?;
                self.rewrite_attribute_file(&mut file, &content, &rewritten)?;
                self.touch(&mut resolved, false);
                return self.store_entry(&mut resolved);
            }
        }
        let finder = change(None, is_dir)?;
        let rewritten = ea::with_finder(&space, tag, is_dir, finder, revision)?;
        self.store_attributes(&mut resolved, &rewritten)
    }

    /// Write the bytes of an extended attribute file that `rewritten` changes
    /// from `content`, its length unchanged.
    fn rewrite_attribute_file(
        &mut self,
        file: &mut Resolved,
        content: &[u8],
        rewritten: &[u8],
    ) -> Result<(), DriverError> {
        let first = content
            .iter()
            .zip(rewritten)
            .position(|(old, new)| old != new);
        let last = content
            .iter()
            .zip(rewritten)
            .rposition(|(old, new)| old != new);
        let (Some(first), Some(last)) = (first, last) else {
            return Ok(());
        };
        self.overwrite(file, first as u64, &rewritten[first..=last])
    }

    /// Record `space` as a resolved entry's extended attribute space — its
    /// embedded bytes, or its descriptors, moved to follow it, into blocks of
    /// their own when they no longer fit — and write it.
    fn store_attributes(&mut self, node: &mut Resolved, space: &[u8]) -> Result<(), DriverError> {
        let layout = Layout::of(node.entry.extended);
        let room = (self.block_size as usize)
            .checked_sub(layout.fixed + space.len())
            .ok_or(DriverError::NoSpace)?;
        let len = u32::try_from(space.len()).map_err(|_| DriverError::NoSpace)?;
        if node.entry.form == AdForm::Embedded && node.entry.ad.len() > room {
            self.unembed(node)?;
        }
        if node.entry.form == AdForm::Embedded {
            let held = &node.block[node.entry.ad.clone()];
            let mut content = zeroed_len(held.len())?;
            content.copy_from_slice(held);
            let after = layout.fixed + space.len();
            node.block[layout.fixed..].fill(0);
            node.block[layout.fixed..after].copy_from_slice(space);
            node.block[after..after + content.len()].copy_from_slice(&content);
            node.block[layout.lengths..layout.lengths + 4].copy_from_slice(&len.to_le_bytes());
            self.touch(node, false);
            return self.store_entry(node);
        }
        let (extents, chain) = self.allocation_of(node)?;
        node.block[layout.fixed..layout.fixed + space.len()].copy_from_slice(space);
        node.block[layout.lengths..layout.lengths + 4].copy_from_slice(&len.to_le_bytes());
        node.entry.ea = layout.fixed..layout.fixed + space.len();
        self.touch(node, false);
        self.store_extents(node, &extents, chain)
    }

    /// Create a node of `file_type` called `name` — `identifier` encoded —
    /// in `dir`: its entry written, and `fill` handed it to record what it
    /// holds, before the name is added, so a node whose content could not be
    /// recorded is never named.
    fn make(
        &mut self,
        dir: NodeId,
        name: &[u8],
        identifier: &[u8],
        file_type: u8,
        mode: u32,
        fill: impl FnOnce(&mut Self, &mut Resolved) -> Result<(), DriverError>,
    ) -> Result<LbAddr, DriverError> {
        let parent = self.node(dir)?;
        if kind_of(parent.entry.file_type) != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        if self.find(&parent, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        let unique = self.next_unique_id()?;
        let (block, _) = self.allocate(1, Some(parent.addr.block))?;
        let addr = LbAddr {
            block,
            partition: PARTITION,
        };
        let written = self
            .new_entry(addr, file_type, mode, unique)
            .and_then(|entry| self.write_block(addr, &entry));
        if let Err(err) = written {
            self.release(block, 1)?;
            return Err(err);
        }
        let characteristics = if file_type == entry::TYPE_DIRECTORY {
            CHAR_DIRECTORY
        } else {
            0
        };
        let named = self.resolve(addr).and_then(|mut node| {
            fill(self, &mut node)?;
            self.add_fid(dir, identifier, addr, unique, characteristics)
        });
        if let Err(err) = named {
            self.discard(addr)?;
            return Err(err);
        }
        Ok(addr)
    }

    /// Free the node at `addr`, which no directory names, with all it holds.
    fn discard(&mut self, addr: LbAddr) -> Result<(), DriverError> {
        let node = self.resolve(addr)?;
        self.free_entry(&node, 0)
    }

    fn create_node(
        &mut self,
        dir: NodeId,
        name: &[u8],
        identifier: &[u8],
        kind: NodeKind,
    ) -> Result<NodeId, DriverError> {
        let addr = match kind {
            NodeKind::RegularFile => {
                let addr =
                    self.make(
                        dir,
                        name,
                        identifier,
                        entry::TYPE_FILE,
                        0o644,
                        |_, _| Ok(()),
                    )?;
                self.integrity.files = self.integrity.files.saturating_add(1);
                addr
            }
            NodeKind::Directory => {
                let parent_addr = addr_of(dir).ok_or(DriverError::NotFound)?;
                let parent = self.rewritable_dir(dir)?;
                // The child's parent descriptor counts as one of the
                // parent's names, so the room for it is checked first.
                if parent.entry.links == u16::MAX {
                    return Err(DriverError::TooManyLinks);
                }
                let parent_unique = parent.entry.unique_id;
                let addr = self.make(
                    dir,
                    name,
                    identifier,
                    entry::TYPE_DIRECTORY,
                    0o755,
                    |fs, node| {
                        fs.append_fid(
                            node,
                            &[],
                            Vec::new(),
                            &[],
                            parent_addr,
                            parent_unique,
                            CHAR_DIRECTORY | CHAR_PARENT,
                        )
                    },
                )?;
                self.relink_dir(dir, 1)?;
                self.integrity.directories = self.integrity.directories.saturating_add(1);
                addr
            }
            NodeKind::Symlink => return Err(DriverError::Unsupported),
        };
        Ok(node_of(addr))
    }

    fn link_node(
        &mut self,
        dir: NodeId,
        name: &[u8],
        identifier: &[u8],
        node: NodeId,
    ) -> Result<(), DriverError> {
        let addr = addr_of(node).ok_or(DriverError::NotFound)?;
        let mut target = self.rewritable(node)?;
        if target.entry.file_type == entry::TYPE_DIRECTORY {
            return Err(DriverError::Unsupported);
        }
        let parent = self.node(dir)?;
        if kind_of(parent.entry.file_type) != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        if self.find(&parent, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        let links = target.entry.links;
        let raised = links.checked_add(1).ok_or(DriverError::TooManyLinks)?;
        // Counted before it is named: a name the count missed would have
        // the node freed from under it.
        Self::set_links(&mut target, raised);
        self.touch(&mut target, false);
        self.store_entry(&mut target)?;
        if let Err(err) = self.add_fid(dir, identifier, addr, target.entry.unique_id, 0) {
            Self::set_links(&mut target, links);
            self.store_entry(&mut target)?;
            return Err(err);
        }
        Ok(())
    }

    /// The file `name` in `dir`, resolved to be written, with its extents
    /// and continuation blocks.
    fn writable_file(
        &mut self,
        dir: NodeId,
        name: &[u8],
    ) -> Result<(Resolved, Vec<Extent>, Vec<LbAddr>), DriverError> {
        let parent = self.node(dir)?;
        let (_, fid) = self.find(&parent, name)?.ok_or(DriverError::NotFound)?;
        self.writable(node_of(fid.node()), NodeKind::RegularFile)
    }

    fn truncate_file(&mut self, dir: NodeId, name: &[u8], size: u64) -> Result<(), DriverError> {
        let (mut node, extents, chain) = self.writable_file(dir, name)?;
        let bs = u64::from(self.block_size);
        let keep = size.div_ceil(bs);
        let mut kept = Vec::new();
        let mut freed = Vec::new();
        let mut at = 0u64;
        for extent in extents {
            let blocks = u64::from(extent.bytes).div_ceil(bs);
            if at >= keep {
                if extent.holds != Holds::Hole {
                    push(&mut freed, (extent.start.block, u32_of(blocks)?))?;
                }
            } else if at + blocks > keep {
                let stay = keep - at;
                if extent.holds != Holds::Hole {
                    push(
                        &mut freed,
                        (block_at(&extent, stay)?.block, u32_of(blocks - stay)?),
                    )?;
                }
                push(
                    &mut kept,
                    Extent {
                        bytes: u32_of(stay * bs)?,
                        ..extent
                    },
                )?;
            } else {
                push(&mut kept, extent)?;
            }
            at += blocks;
        }
        // Whatever lies past the file's data in the block it ends in must
        // read as zeros: after a shrink, or once the file grows past it.
        let boundary = size.min(node.entry.size);
        if boundary % bs != 0 {
            if let Some((index, within)) =
                Mapped::of(copied(&kept)?)?.find(boundary - boundary % bs)
            {
                let extent = kept[index];
                if extent.holds == Holds::Data {
                    let addr = block_at(&extent, within / bs)?;
                    let mut buf = zeroed(self.block_size)?;
                    self.read_block(addr, &mut buf)?;
                    buf[usize::try_from(boundary % bs).map_err(|_| DriverError::DeviceFault)?..]
                        .fill(0);
                    self.write_block(addr, &buf)?;
                }
            }
        }
        push_hole(&mut kept, keep.saturating_sub(at), self.block_size)?;
        Self::set_size(&mut node, size);
        self.touch(&mut node, true);
        let kept = merged(kept, self.block_size);
        self.store_shrunk(&mut node, &kept, chain, &freed)
    }

    fn punch_file(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        len: u64,
    ) -> Result<(), DriverError> {
        let (mut node, extents, chain) = self.writable_file(dir, name)?;
        let end = offset
            .checked_add(len)
            .ok_or(DriverError::OutOfRange)?
            .min(node.entry.size);
        if end <= offset {
            return Ok(());
        }
        let bs = u64::from(self.block_size);
        // Whole blocks go back to the free pool. The partial ends keep their
        // blocks, zeroed in place where they record data: a hole or an
        // unwritten extent reads as zeros already, and nothing is allocated.
        let first = offset.div_ceil(bs);
        let last = end / bs;
        let ends = if first > last {
            [(offset, end), (end, end)]
        } else {
            [
                (offset, (first * bs).min(end)),
                ((last * bs).max(offset), end),
            ]
        };
        let mapped = Mapped::of(copied(&extents)?)?;
        let mut zero = [None; 2];
        for (slot, (from, to)) in zero.iter_mut().zip(ends) {
            if to <= from {
                continue;
            }
            if let Some((index, within)) = mapped.find(from) {
                let extent = mapped.extents[index];
                if extent.holds == Holds::Data {
                    let skip =
                        usize::try_from(within % bs).map_err(|_| DriverError::DeviceFault)?;
                    let count = usize::try_from(to - from).map_err(|_| DriverError::DeviceFault)?;
                    *slot = Some((block_at(&extent, within / bs)?, skip, count));
                }
            }
        }
        let mut buf = zeroed(self.block_size)?;
        let mut punched = Vec::new();
        let mut freed = Vec::new();
        let mut at = 0u64;
        for extent in extents {
            let blocks = u64::from(extent.bytes).div_ceil(bs);
            let span = at..at + blocks;
            at = span.end;
            if extent.holds == Holds::Hole || span.end <= first || span.start >= last {
                push(&mut punched, extent)?;
                continue;
            }
            let from = first.max(span.start);
            let to = last.min(span.end);
            if from > span.start {
                push(
                    &mut punched,
                    piece(&extent, extent.holds, 0, from - span.start, self.block_size)?,
                )?;
            }
            push(
                &mut freed,
                (
                    block_at(&extent, from - span.start)?.block,
                    u32_of(to - from)?,
                ),
            )?;
            push_hole(&mut punched, to - from, self.block_size)?;
            if span.end > to {
                push(
                    &mut punched,
                    piece(
                        &extent,
                        extent.holds,
                        to - span.start,
                        span.end - to,
                        self.block_size,
                    )?,
                )?;
            }
        }
        self.touch(&mut node, true);
        let punched = merged(punched, self.block_size);
        self.store_shrunk(&mut node, &punched, chain, &freed)?;
        for (addr, skip, count) in zero.into_iter().flatten() {
            self.read_block(addr, &mut buf)?;
            buf[skip..skip + count].fill(0);
            self.write_block(addr, &buf)?;
        }
        Ok(())
    }

    fn remove_name(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let parent = self.node(dir)?;
        let (offset, fid) = self.find(&parent, name)?.ok_or(DriverError::NotFound)?;
        // Resolved to be rewritten before the name goes, so a node whose
        // count cannot be changed keeps its name.
        let target = self.rewritable(node_of(fid.node()))?;
        let is_dir = target.entry.file_type == entry::TYPE_DIRECTORY;
        if is_dir && self.has_entries(&target)? {
            return Err(DriverError::DirectoryNotEmpty);
        }
        self.delete_fid(dir, offset)?;
        self.unlink_node(target)?;
        if is_dir {
            self.relink_dir(dir, -1)?;
        }
        Ok(())
    }

    /// Move the name `src_name` of `src_dir` to `dst_name` — `identifier`
    /// encoded — in `dst_dir`, replacing what that names. Everything that
    /// can be refused is refused before the first write, and every write
    /// that follows rewrites a descriptor or an entry where it lies, but the
    /// new name's when it replaces none.
    fn move_name(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
        identifier: &[u8],
    ) -> Result<(), DriverError> {
        let source = self.node(src_dir)?;
        let (offset, fid) = self.find(&source, src_name)?.ok_or(DriverError::NotFound)?;
        let icb = fid.node();
        if src_dir == dst_dir && src_name == dst_name {
            return Ok(());
        }
        let moving = self.resolve(icb)?;
        let is_dir = moving.entry.file_type == entry::TYPE_DIRECTORY;
        if is_dir && self.is_within(dst_dir, icb)? {
            return Err(DriverError::DirectoryCycle);
        }
        self.rewritable_dir(src_dir)?;
        let destination = self.rewritable_dir(dst_dir)?;
        let replaced = match self.find(&destination, dst_name)? {
            Some((_, existing)) if existing.node() == icb => {
                // Two names of one node: renaming one onto the other changes
                // nothing.
                return Ok(());
            }
            Some((at, existing)) => {
                let replaced = self.rewritable(node_of(existing.node()))?;
                let replaced_dir = replaced.entry.file_type == entry::TYPE_DIRECTORY;
                if replaced_dir != is_dir {
                    return Err(if replaced_dir {
                        DriverError::Unsupported
                    } else {
                        DriverError::AlreadyExists
                    });
                }
                if replaced_dir && self.has_entries(&replaced)? {
                    return Err(DriverError::DirectoryNotEmpty);
                }
                Some((at, replaced))
            }
            None => None,
        };
        let across = is_dir && src_dir != dst_dir;
        let replaces_dir = replaced
            .as_ref()
            .is_some_and(|(_, node)| node.entry.file_type == entry::TYPE_DIRECTORY);
        if across && !replaces_dir && destination.entry.links == u16::MAX {
            return Err(DriverError::TooManyLinks);
        }
        let characteristics = fid.characteristics & !(CHAR_DELETED | CHAR_PARENT);
        let unique = fid.unique();
        if let Some((at, replaced)) = replaced {
            // The destination's name is pointed at the node where it lies,
            // so the destination needs no room it may lack.
            self.repoint_fid(dst_dir, at, icb, unique, characteristics)?;
            self.delete_fid(src_dir, offset)?;
            self.unlink_node(replaced)?;
            if replaces_dir {
                self.relink_dir(dst_dir, -1)?;
            }
        } else {
            self.add_fid(dst_dir, identifier, icb, unique, characteristics)?;
            // The source's offset is unchanged: the name was appended to
            // another directory, or after it.
            self.delete_fid(src_dir, offset)?;
        }
        if across {
            self.reparent(icb, dst_dir)?;
            self.relink_dir(src_dir, -1)?;
            self.relink_dir(dst_dir, 1)?;
        }
        Ok(())
    }

    /// Whether the directory `dir` is `ancestor` or lies beneath it, read up
    /// its chain of parents.
    fn is_within(&mut self, dir: NodeId, ancestor: LbAddr) -> Result<bool, DriverError> {
        let mut at = addr_of(dir).ok_or(DriverError::NotFound)?;
        for _ in 0..4096 {
            if at == ancestor {
                return Ok(true);
            }
            if at == self.root {
                return Ok(false);
            }
            let node = self.resolve(at)?;
            let mut parent = None;
            self.for_each_fid(&node, &mut |_, fid| {
                if fid.characteristics & CHAR_PARENT != 0 {
                    parent = Some(fid.node());
                    return Ok(false);
                }
                Ok(true)
            })?;
            at = parent.ok_or(DriverError::DeviceFault)?;
        }
        Err(DriverError::DeviceFault)
    }

    /// Point the parent descriptor of the directory at `dir` to `parent`.
    fn reparent(&mut self, dir: LbAddr, parent: NodeId) -> Result<(), DriverError> {
        let parent_addr = addr_of(parent).ok_or(DriverError::NotFound)?;
        let parent_unique = self.node(parent)?.entry.unique_id;
        let mut node = self.rewritable_dir(node_of(dir))?;
        let mut found = None;
        self.for_each_fid(&node, &mut |at, fid| {
            if fid.characteristics & CHAR_PARENT != 0 {
                found = Some((at, fid.characteristics));
                return Ok(false);
            }
            Ok(true)
        })?;
        let (at, characteristics) = found.ok_or(DriverError::DeviceFault)?;
        let (mut bytes, tag) = self.fid_bytes(&node, at)?;
        self.name_node(&mut bytes, parent_addr, parent_unique, characteristics);
        let len = bytes.len();
        tag.seal(&mut bytes, len - TAG_LEN)?;
        self.overwrite(&mut node, at, &bytes)
    }
}

/// `id`, its low 32 bits lifted past the 0 to 15 UDF reserves.
fn lift_unique(id: u64) -> u64 {
    if id & 0xFFFF_FFFF < 16 {
        (id & !0xFFFF_FFFF) | 16
    } else {
        id
    }
}

/// `name` as the identifier a directory records: one path component of
/// valid UTF-8, encoded as OSTA Compressed Unicode.
fn encoded(name: &[u8]) -> Result<Vec<u8>, DriverError> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0)
    {
        return Err(DriverError::OutOfRange);
    }
    let text = core::str::from_utf8(name).map_err(|_| DriverError::OutOfRange)?;
    dir::encode(text).ok_or(DriverError::LengthOutOfRange)
}

/// A short descriptor of `extent`, whose length fits its 30 bits.
fn short_ad(extent: &Extent) -> [u8; 8] {
    let kind = match extent.holds {
        Holds::Data => 0,
        Holds::Unwritten => 1,
        Holds::Hole => 2,
    };
    let mut ad = [0u8; 8];
    ad[..4].copy_from_slice(&((kind << 30) | extent.bytes).to_le_bytes());
    let start = if extent.holds == Holds::Hole {
        0
    } else {
        extent.start.block
    };
    ad[4..].copy_from_slice(&start.to_le_bytes());
    ad
}

/// A short descriptor continuing the list in the block `at`.
fn continuation_ad(at: u32, block_size: u32) -> [u8; 8] {
    let mut ad = [0u8; 8];
    ad[..4].copy_from_slice(&((3u32 << 30) | block_size).to_le_bytes());
    ad[4..].copy_from_slice(&at.to_le_bytes());
    ad
}

/// Append `item` to `out`, a refused allocation answered as such.
fn push<T>(out: &mut Vec<T>, item: T) -> Result<(), DriverError> {
    if tairix_util::fallible::push(out, item) {
        Ok(())
    } else {
        Err(DriverError::OutOfMemory)
    }
}

/// `extents`, copied fallibly.
fn copied(extents: &[Extent]) -> Result<Vec<Extent>, DriverError> {
    tairix_util::fallible::collected(extents.len(), extents.iter().copied())
        .ok_or(DriverError::OutOfMemory)
}

/// Append a hole of `blocks` blocks to `out`, in descriptors no longer than
/// one records — refused as too long where it needs more than a file may
/// have.
fn push_hole(out: &mut Vec<Extent>, blocks: u64, block_size: u32) -> Result<(), DriverError> {
    let per = u64::from(EXTENT_MAX / block_size);
    if blocks.div_ceil(per) > MAX_EXTENTS.saturating_sub(out.len()) as u64 {
        return Err(DriverError::LengthOutOfRange);
    }
    let mut left = blocks;
    while left > 0 {
        let take = left.min(per);
        let hole = Extent {
            holds: Holds::Hole,
            bytes: u32_of(take * u64::from(block_size))?,
            start: LbAddr {
                block: 0,
                partition: 0,
            },
        };
        push(out, hole)?;
        left -= take;
    }
    Ok(())
}

/// The address of block `offset` of `extent`.
fn block_at(extent: &Extent, offset: u64) -> Result<LbAddr, DriverError> {
    let block = u32::try_from(offset)
        .ok()
        .and_then(|offset| extent.start.block.checked_add(offset))
        .ok_or(DriverError::DeviceFault)?;
    Ok(LbAddr {
        block,
        partition: extent.start.partition,
    })
}

/// The `count` blocks of `extent` from its block `offset`, as holding
/// `holds`.
fn piece(
    extent: &Extent,
    holds: Holds,
    offset: u64,
    count: u64,
    block_size: u32,
) -> Result<Extent, DriverError> {
    Ok(Extent {
        holds,
        bytes: u32_of(count * u64::from(block_size))?,
        start: block_at(extent, offset)?,
    })
}

/// `value` narrowed, overflow refused as damage.
fn u32_of(value: u64) -> Result<u32, DriverError> {
    u32::try_from(value).map_err(|_| DriverError::DeviceFault)
}

/// `extents` with neighbours that continue one another joined, up to the
/// longest a descriptor holds.
fn merged(mut extents: Vec<Extent>, block_size: u32) -> Vec<Extent> {
    let cap = EXTENT_MAX - EXTENT_MAX % block_size;
    extents.dedup_by(|next, last| {
        let follows = last.holds == next.holds
            && last.bytes % block_size == 0
            && last
                .bytes
                .checked_add(next.bytes)
                .is_some_and(|sum| sum <= cap)
            && (next.holds == Holds::Hole
                || (last.start.partition == next.start.partition
                    && last.start.block.checked_add(last.bytes / block_size)
                        == Some(next.start.block)));
        if follows {
            last.bytes += next.bytes;
        }
        follows
    });
    extents
}

/// `extents` cut to describe exactly `size` bytes: the extent the file ends
/// in stops at its last byte, as UDF requires of a file's last descriptor.
fn fit(extents: &[Extent], size: u64) -> Result<Vec<Extent>, DriverError> {
    let mut out = Vec::new();
    let mut at = 0u64;
    for extent in extents {
        if at >= size {
            break;
        }
        let bytes = u64::from(extent.bytes).min(size - at);
        push(
            &mut out,
            Extent {
                bytes: u32_of(bytes)?,
                ..*extent
            },
        )?;
        at += bytes;
    }
    Ok(out)
}

/// `extents` with the blocks `first..=last` marked as holding data.
fn written_blocks(
    extents: &[Extent],
    first: u64,
    last: u64,
    block_size: u32,
) -> Result<Vec<Extent>, DriverError> {
    let bs = u64::from(block_size);
    let mut out = Vec::new();
    let mut at = 0u64;
    for extent in extents {
        let blocks = u64::from(extent.bytes).div_ceil(bs);
        let span = at..at + blocks;
        at = span.end;
        if extent.holds != Holds::Unwritten || span.end <= first || span.start > last {
            push(&mut out, *extent)?;
            continue;
        }
        let from = first.max(span.start);
        let to = (last + 1).min(span.end);
        if from > span.start {
            push(
                &mut out,
                piece(extent, Holds::Unwritten, 0, from - span.start, block_size)?,
            )?;
        }
        push(
            &mut out,
            piece(
                extent,
                Holds::Data,
                from - span.start,
                to - from,
                block_size,
            )?,
        )?;
        if span.end > to {
            push(
                &mut out,
                piece(
                    extent,
                    Holds::Unwritten,
                    to - span.start,
                    span.end - to,
                    block_size,
                )?,
            )?;
        }
    }
    Ok(merged(out, block_size))
}

impl<B: Block> FilesystemWrite for Udf<B> {
    fn create(&mut self, dir: NodeId, name: &[u8], kind: NodeKind) -> Result<NodeId, DriverError> {
        let identifier = encoded(name)?;
        self.change(|fs| fs.create_node(dir, name, &identifier, kind))
    }

    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
    ) -> Result<NodeId, DriverError> {
        let identifier = encoded(name)?;
        if target.is_empty() || target.len() > FS_SYMLINK_MAX {
            return Err(DriverError::LengthOutOfRange);
        }
        let content = path_components(target)?;
        self.change(|fs| {
            let addr = fs.make(
                dir,
                name,
                &identifier,
                entry::TYPE_SYMLINK,
                0o777,
                |fs, node| fs.write_range(node, &[], Vec::new(), 0, &content),
            )?;
            fs.integrity.files = fs.integrity.files.saturating_add(1);
            Ok(node_of(addr))
        })
    }

    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        let identifier = encoded(name)?;
        self.change(|fs| fs.link_node(dir, name, &identifier, node))
    }

    fn write_at(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        self.change(|fs| {
            let (mut node, extents, chain) = fs.writable_file(dir, name)?;
            fs.write_range(&mut node, &extents, chain, offset, data)?;
            Ok(data.len())
        })
    }

    fn truncate(&mut self, dir: NodeId, name: &[u8], size: u64) -> Result<(), DriverError> {
        self.change(|fs| fs.truncate_file(dir, name, size))
    }

    fn punch(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        len: u64,
    ) -> Result<(), DriverError> {
        self.change(|fs| fs.punch_file(dir, name, offset, len))
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.change(|fs| fs.remove_name(dir, name))
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let identifier = encoded(dst_name)?;
        self.change(|fs| fs.move_name(src_dir, src_name, dst_dir, dst_name, &identifier))
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.publish()
    }

    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.host = Some(host);
        self.write.schedule.set_host(volume, host);
    }
}

/// A symbolic link's target as ECMA-167 4/14.16 path components: a root
/// for a leading `/`, then `..`, `.` and named components.
fn path_components(target: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut out = Vec::new();
    let path = core::str::from_utf8(target).map_err(|_| DriverError::OutOfRange)?;
    let mut push = |kind: u8, identifier: &[u8]| -> Result<(), DriverError> {
        if !tairix_util::fallible::reserve(&mut out, 4 + identifier.len()) {
            return Err(DriverError::OutOfMemory);
        }
        out.extend_from_slice(&[
            kind,
            u8::try_from(identifier.len()).map_err(|_| DriverError::LengthOutOfRange)?,
            0,
            0,
        ]);
        out.extend_from_slice(identifier);
        Ok(())
    };
    if path.starts_with('/') {
        push(2, &[])?;
    }
    for part in path.split('/').filter(|part| !part.is_empty()) {
        match part {
            "." => push(4, &[])?,
            ".." => push(3, &[])?,
            name => push(5, &dir::encode(name).ok_or(DriverError::LengthOutOfRange)?)?,
        }
    }
    Ok(out)
}
