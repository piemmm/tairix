//! The jbd2 journal ext3 and ext4 keep metadata crash-consistent with.
//!
//! A transaction is the set of metadata blocks one commit changes. It is
//! written to the journal — descriptor blocks naming each block's home, the
//! block images, then a commit block — and only once that commit is durable
//! are the images written home. A crash before the commit is durable loses
//! the transaction whole; one after it is finished by replaying the journal.
//!
//! This writer appends each transaction after the one before, circularly,
//! and points the journal superblock at the newest once its commit block is
//! durable; every transaction is written home in full before the next is
//! committed, so a replay is only ever of the newest, whose blocks no later
//! change can have reused — a block freed is reused only after the
//! transaction freeing it commits — and no revoke record is needed. A quiet
//! volume's log is emptied. Recovery reads whatever any writer left,
//! revokes and all. Every structure is big-endian.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use crate::bytes::{crc32c_zeroed, high32, low32, scratch};

/// `h_magic` every journal block header carries.
pub(crate) const MAGIC: u32 = 0xC03B_3998;

const DESCRIPTOR: u32 = 1;
const COMMIT: u32 = 2;
const SUPERBLOCK_V1: u32 = 3;
const SUPERBLOCK_V2: u32 = 4;
const REVOKE: u32 = 5;

const COMPAT_CHECKSUM: u32 = 0x1;
const INCOMPAT_REVOKE: u32 = 0x1;
const INCOMPAT_64BIT: u32 = 0x2;
const INCOMPAT_ASYNC_COMMIT: u32 = 0x4;
const INCOMPAT_CSUM_V2: u32 = 0x8;
const INCOMPAT_CSUM_V3: u32 = 0x10;
const KNOWN_INCOMPAT: u32 =
    INCOMPAT_REVOKE | INCOMPAT_64BIT | INCOMPAT_ASYNC_COMMIT | INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3;

const FLAG_ESCAPE: u32 = 0x1;
const FLAG_SAME_UUID: u32 = 0x2;
const FLAG_LAST_TAG: u32 = 0x8;

/// `s_checksum_type` for crc32c.
const CRC32C: u8 = 4;

/// Bytes of a block header, and of the revoke header with its count.
const HEADER: usize = 12;
const REVOKE_HEADER: usize = 16;
const UUID_LEN: usize = 16;
const TAIL: usize = 4;

/// Journal superblock field offsets.
const SB_BLOCKSIZE: usize = 0x0C;
pub(crate) const SB_MAXLEN: usize = 0x10;
const SB_FIRST: usize = 0x14;
const SB_SEQUENCE: usize = 0x18;
const SB_START: usize = 0x1C;
const SB_ERRNO: usize = 0x20;
const SB_COMPAT: usize = 0x24;
const SB_INCOMPAT: usize = 0x28;
const SB_UUID: usize = 0x30;
const SB_CHECKSUM_TYPE: usize = 0x50;
const SB_CHECKSUM: usize = 0xFC;
/// Bytes the superblock checksum covers.
const SB_LEN: usize = 1024;

/// The longest journal mke2fs makes, in blocks.
const MAX_JOURNAL_BLOCKS: u32 = 10_240_000;

/// Commit block field offsets.
const COMMIT_CHECKSUM: usize = 0x10;
const COMMIT_SEC: usize = 0x30;
const COMMIT_NSEC: usize = 0x38;

fn be32(buf: &[u8], off: usize) -> u32 {
    buf.get(off..off + 4)
        .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn put_be32(buf: &mut [u8], off: usize, v: u32) {
    if let Some(b) = buf.get_mut(off..off + 4) {
        b.copy_from_slice(&v.to_be_bytes());
    }
}

/// Journal-block access: the journal's own blocks by index, and the
/// filesystem's blocks at home.
pub(crate) trait JournalIo {
    /// Read journal block `n` into `buf`.
    fn read_journal(&mut self, n: u32, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Write journal block `n`.
    fn write_journal(&mut self, n: u32, buf: &[u8]) -> Result<(), DriverError>;
    /// Write filesystem block `block` at its home location.
    fn write_home(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError>;
    /// Make every write already accepted durable.
    fn flush(&mut self) -> Result<(), DriverError>;
}

/// Why the journal cannot be used as found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    /// The superblock is not a jbd2 superblock this driver reads.
    Unrecognised,
    /// An incompatible feature this driver does not implement (fast
    /// commits among them), or an asynchronous commit with nothing to prove
    /// its blocks whole.
    Feature(u32),
    /// The superblock's checksum does not match.
    Checksum,
}

/// How a commit failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommitError {
    /// Before its commit block was written: nothing it wrote is a
    /// transaction.
    Unwritten(DriverError),
    /// Writing its commit block, or after: the transaction may be on the
    /// medium, and a replay applies it.
    Uncertain(DriverError),
}

/// A parsed internal journal.
pub(crate) struct Journal {
    block_size: usize,
    maxlen: u32,
    first: u32,
    /// The id the next transaction is written under.
    sequence: u32,
    /// Where the next transaction's first block goes.
    head: u32,
    compat: u32,
    incompat: u32,
    seed: u32,
    /// The superblock block as last written.
    sb: Vec<u8>,
    errno: u32,
}

impl Journal {
    /// Parse journal block 0, `sb`, for a filesystem of `block_size` whose
    /// journal file is `file_blocks` long; the journal keeps the buffer as
    /// its superblock image.
    pub(crate) fn parse(sb: Vec<u8>, block_size: usize, file_blocks: u64) -> Result<Self, Refusal> {
        if sb.len() != block_size || block_size < SB_LEN || be32(&sb, 0) != MAGIC {
            return Err(Refusal::Unrecognised);
        }
        let kind = be32(&sb, 4);
        if kind != SUPERBLOCK_V1 && kind != SUPERBLOCK_V2 {
            return Err(Refusal::Unrecognised);
        }
        let maxlen = be32(&sb, SB_MAXLEN);
        let first = be32(&sb, SB_FIRST);
        if be32(&sb, SB_BLOCKSIZE) as usize != block_size
            || u64::from(maxlen) > file_blocks
            || maxlen > MAX_JOURNAL_BLOCKS
            || first == 0
            || first >= maxlen
        {
            return Err(Refusal::Unrecognised);
        }
        let (compat, incompat) = if kind == SUPERBLOCK_V2 {
            (be32(&sb, SB_COMPAT), be32(&sb, SB_INCOMPAT))
        } else {
            (0, 0)
        };
        if incompat & !KNOWN_INCOMPAT != 0 {
            return Err(Refusal::Feature(incompat & !KNOWN_INCOMPAT));
        }
        let checksummed = incompat & (INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3) != 0;
        if incompat & INCOMPAT_ASYNC_COMMIT != 0 && !checksummed {
            return Err(Refusal::Feature(INCOMPAT_ASYNC_COMMIT));
        }
        if checksummed {
            if sb[SB_CHECKSUM_TYPE] != CRC32C {
                return Err(Refusal::Checksum);
            }
            if be32(&sb, SB_CHECKSUM) != sb_checksum(&sb) {
                return Err(Refusal::Checksum);
            }
        }
        Ok(Self {
            block_size,
            maxlen,
            first,
            sequence: be32(&sb, SB_SEQUENCE),
            head: first,
            compat,
            incompat,
            seed: tairix_crc32c::update(0xFFFF_FFFF, &sb[SB_UUID..SB_UUID + UUID_LEN]),
            errno: be32(&sb, SB_ERRNO),
            sb,
        })
    }

    /// The journal blocks the log uses: no more of its file than this.
    pub(crate) fn maxlen(&self) -> u32 {
        self.maxlen
    }

    /// Whether the journal recorded an error the kernel aborted it over.
    pub(crate) fn errno(&self) -> u32 {
        self.errno
    }

    /// Whether the superblock names a log to replay.
    pub(crate) fn has_log(&self) -> bool {
        be32(&self.sb, SB_START) != 0
    }

    /// Turn on what this writer needs from the format: 64-bit tags on a
    /// 64-bit filesystem, whose block numbers need them, and v3 checksums on a
    /// checksummed one. A log checksummed in v2 moves to
    /// v3, which this writer writes instead. A version-1 superblock has no
    /// feature words to set.
    pub(crate) fn adopt_features(&mut self, wide: bool, checksummed: bool) {
        if be32(&self.sb, 4) != SUPERBLOCK_V2 {
            return;
        }
        if wide {
            self.incompat |= INCOMPAT_64BIT;
        }
        if checksummed || self.checksummed() {
            self.incompat = (self.incompat & !INCOMPAT_CSUM_V2) | INCOMPAT_CSUM_V3;
        }
        // The v1 commit checksum is one this writer never produces, and the
        // format forbids it beside v2/v3.
        self.compat &= !COMPAT_CHECKSUM;
    }

    fn checksummed(&self) -> bool {
        self.incompat & (INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3) != 0
    }

    fn v3(&self) -> bool {
        self.incompat & INCOMPAT_CSUM_V3 != 0
    }

    fn wide(&self) -> bool {
        self.incompat & INCOMPAT_64BIT != 0
    }

    fn tag_bytes(&self) -> usize {
        if self.v3() {
            return 16;
        }
        let tag = if self.wide() { 12 } else { 8 };
        // A v2 tag is two bytes longer than one the log keeps no sum of.
        if self.incompat & INCOMPAT_CSUM_V2 != 0 {
            tag + 2
        } else {
            tag
        }
    }

    fn tail(&self) -> usize {
        if self.checksummed() {
            TAIL
        } else {
            0
        }
    }

    fn next(&self, n: u32) -> u32 {
        if n + 1 >= self.maxlen {
            self.first
        } else {
            n + 1
        }
    }

    /// Metadata blocks one transaction may carry: the log less its
    /// descriptors and commit block, and a quarter of it at most, so no
    /// commit is ever starved of log space.
    pub(crate) fn capacity(&self) -> usize {
        let log = (self.maxlen - self.first) as usize;
        let per = (self.block_size - HEADER - self.tail() - UUID_LEN) / self.tag_bytes();
        let usable = log.saturating_sub(log.div_ceil(per.max(1)) + 1);
        (log / 4).min(usable).max(1)
    }

    /// crc32c of `block` under the journal seed, the four bytes at `at` (the
    /// checksum's own field) counted as zero.
    fn block_checksum(&self, block: &[u8], at: usize) -> u32 {
        crc32c_zeroed::<4>(self.seed, block, at)
    }

    /// A tag's checksum: the block as logged, under the journal seed chained
    /// with the transaction id. An escaped block is logged with its first
    /// word zeroed, and summed that way.
    fn tag_checksum(&self, sequence: u32, data: &[u8], escaped: bool) -> u32 {
        let c = tairix_crc32c::update(self.seed, &sequence.to_be_bytes());
        if escaped {
            crc32c_zeroed::<4>(c, data, 0)
        } else {
            tairix_crc32c::update(c, data)
        }
    }

    /// Replay the log into place and leave the journal empty.
    ///
    /// Three passes, as the format requires: find where the committed log
    /// ends, gather the revoke records, then write home every block a later
    /// transaction did not revoke. A transaction without a sound commit block
    /// is the one the crash interrupted and is discarded.
    pub(crate) fn recover<I: JournalIo>(&mut self, io: &mut I) -> Result<u32, DriverError> {
        if !self.has_log() {
            return Ok(0);
        }
        let start = be32(&self.sb, SB_START);
        if start < self.first || start >= self.maxlen {
            return Err(DriverError::DeviceFault);
        }
        let mut block = scratch(self.block_size)?;
        let mut data = scratch(self.block_size)?;
        let (end, revoked) = self.scan(io, start, &mut block, &mut data)?;
        let mut n = start;
        let mut sequence = self.sequence;
        let mut replayed = 0u32;
        let mut lap = Lap::of(self);
        while sequence != end {
            io.read_journal(n, &mut block)?;
            match be32(&block, 4) {
                DESCRIPTOR => {
                    let mut at = lap.step(self, n)?;
                    let age = sequence.wrapping_sub(self.sequence);
                    for tag in self.tags(&block) {
                        io.read_journal(at, &mut data)?;
                        if self.checksummed() && !self.tag_sound(&tag, sequence, &data) {
                            return Err(DriverError::DeviceFault);
                        }
                        if revoked.by(tag.home).is_none_or(|by| by < age) {
                            if tag.flags & FLAG_ESCAPE != 0 {
                                put_be32(&mut data, 0, MAGIC);
                            }
                            io.write_home(tag.home, &data)?;
                            replayed += 1;
                        }
                        at = lap.step(self, at)?;
                    }
                    n = at;
                }
                COMMIT => {
                    sequence = sequence.wrapping_add(1);
                    n = lap.step(self, n)?;
                }
                _ => n = lap.step(self, n)?,
            }
        }
        io.flush()?;
        // An interrupted transaction may have left blocks under `end`; the
        // next one is written under a later id, so it can never be mistaken
        // for them.
        self.sequence = end.wrapping_add(1);
        self.head = self.first;
        self.reset(io)?;
        io.flush()?;
        Ok(replayed)
    }

    /// Walk the log from `start`: the id the first uncommitted transaction
    /// would carry, and every block a committed transaction revoked.
    fn scan<I: JournalIo>(
        &self,
        io: &mut I,
        start: u32,
        block: &mut [u8],
        data: &mut [u8],
    ) -> Result<(u32, Revoked), DriverError> {
        let mut revoked = Revoked::default();
        // Revokes of the transaction being read, kept only once it commits.
        let mut pending: Vec<u64> = Vec::new();
        let mut n = start;
        let mut sequence = self.sequence;
        let asynchronous = self.incompat & INCOMPAT_ASYNC_COMMIT != 0;
        let mut lap = Lap::of(self);
        'log: loop {
            io.read_journal(n, block)?;
            if be32(block, 0) != MAGIC || be32(block, 8) != sequence {
                break;
            }
            match be32(block, 4) {
                DESCRIPTOR | REVOKE if !self.tail_sound(block) => {
                    // Damage to a transaction its commit vouches for, which
                    // no truncation of the log may quietly drop.
                    if self.committed_after(io, n, sequence, &mut lap, data)? {
                        return Err(DriverError::DeviceFault);
                    }
                    break;
                }
                DESCRIPTOR => {
                    n = lap.step(self, n)?;
                    for tag in self.tags(block) {
                        // An asynchronous commit is written without waiting
                        // for its blocks, so only their checksums prove the
                        // transaction whole.
                        if asynchronous {
                            io.read_journal(n, data)?;
                            if !self.tag_sound(&tag, sequence, data) {
                                break 'log;
                            }
                        }
                        n = lap.step(self, n)?;
                    }
                }
                COMMIT => {
                    if self.checksummed()
                        && be32(block, COMMIT_CHECKSUM)
                            != self.block_checksum(block, COMMIT_CHECKSUM)
                    {
                        break;
                    }
                    revoked.commit(&mut pending, sequence.wrapping_sub(self.sequence))?;
                    sequence = sequence.wrapping_add(1);
                    n = lap.step(self, n)?;
                }
                REVOKE => {
                    for home in self.revokes(block)? {
                        pending.try_reserve(1).map_err(|_| DriverError::NoSpace)?;
                        pending.push(home);
                    }
                    n = lap.step(self, n)?;
                }
                _ => break,
            }
        }
        revoked.settle();
        Ok((sequence, revoked))
    }

    /// Whether descriptor or revoke block `block` carries the checksum its
    /// tail should, where the log keeps one.
    fn tail_sound(&self, block: &[u8]) -> bool {
        !self.checksummed()
            || be32(block, self.block_size - TAIL)
                == self.block_checksum(block, self.block_size - TAIL)
    }

    /// Whether transaction `sequence`'s commit block is the next journal block
    /// after block `n`, sound: what makes a descriptor or revoke block of it
    /// that fails its checksum damage rather than the torn end of the log.
    /// A logged image beginning with the journal magic is escaped, so the
    /// first block carrying the magic is the next journal block.
    fn committed_after<I: JournalIo>(
        &self,
        io: &mut I,
        mut n: u32,
        sequence: u32,
        lap: &mut Lap,
        scratch: &mut [u8],
    ) -> Result<bool, DriverError> {
        loop {
            let Ok(next) = lap.step(self, n) else {
                return Ok(false);
            };
            n = next;
            io.read_journal(n, scratch)?;
            if be32(scratch, 0) == MAGIC {
                return Ok(be32(scratch, 4) == COMMIT
                    && be32(scratch, 8) == sequence
                    && (!self.checksummed()
                        || be32(scratch, COMMIT_CHECKSUM)
                            == self.block_checksum(scratch, COMMIT_CHECKSUM)));
            }
        }
    }

    fn tag_sound(&self, tag: &Tag, sequence: u32, data: &[u8]) -> bool {
        let c = self.tag_checksum(sequence, data, false);
        if self.v3() {
            tag.checksum == c
        } else {
            tag.checksum == c & 0xFFFF
        }
    }

    /// The tags of descriptor block `block`, up to the one marked last.
    fn tags<'b>(&self, block: &'b [u8]) -> Tags<'b> {
        Tags {
            block,
            at: HEADER,
            end: self.block_size - self.tail(),
            size: self.tag_bytes(),
            v3: self.v3(),
            wide: self.wide(),
            done: false,
        }
    }

    /// The block numbers revoke block `block` lists.
    fn revokes<'b>(&self, block: &'b [u8]) -> Result<impl Iterator<Item = u64> + 'b, DriverError> {
        let used = be32(block, HEADER) as usize;
        if used < REVOKE_HEADER || used > self.block_size - self.tail() {
            return Err(DriverError::DeviceFault);
        }
        let width = if self.wide() { 8 } else { 4 };
        Ok((0..(used - REVOKE_HEADER) / width).map(move |i| {
            let at = REVOKE_HEADER + i * width;
            if width == 8 {
                (u64::from(be32(block, at)) << 32) | u64::from(be32(block, at + 4))
            } else {
                u64::from(be32(block, at))
            }
        }))
    }

    /// Write `blocks` — each a home block number and its image — to the log
    /// as one transaction after the last, and make it durable: descriptors
    /// and images, a barrier (which also makes durable the ordered data and
    /// the previous transaction's home writes before them), then the commit
    /// block and the superblock pointing at it, then a second barrier.
    pub(crate) fn commit<'b, I, T>(
        &mut self,
        io: &mut I,
        blocks: T,
        count: usize,
        time: (u64, u32),
    ) -> Result<(), CommitError>
    where
        I: JournalIo,
        T: Iterator<Item = (u64, &'b [u8])>,
    {
        let mut commit = scratch(self.block_size).map_err(CommitError::Unwritten)?;
        let at = self
            .log(io, blocks, count)
            .map_err(CommitError::Unwritten)?;
        self.seal(io, at, &mut commit, time)
            .map_err(CommitError::Uncertain)
    }

    /// Write the descriptors and images of the next transaction from `head`
    /// and make them durable, answering where its commit block goes.
    fn log<'b, I, T>(&mut self, io: &mut I, blocks: T, count: usize) -> Result<u32, DriverError>
    where
        I: JournalIo,
        T: Iterator<Item = (u64, &'b [u8])>,
    {
        if count == 0 || count > self.capacity() {
            return Err(DriverError::NoSpace);
        }
        let size = self.tag_bytes();
        let end = self.block_size - self.tail();
        // The first tag of a descriptor carries the journal's uuid.
        let per = (end - HEADER - UUID_LEN) / size;
        let mut descriptor = scratch(self.block_size)?;
        let mut escaped = scratch(self.block_size)?;
        let mut batch: Vec<(&'b [u8], bool)> = Vec::new();
        batch
            .try_reserve_exact(per.min(count))
            .map_err(|_| DriverError::NoSpace)?;
        let sequence = self.sequence;
        let mut at = self.head;
        let mut blocks = blocks.peekable();
        while blocks.peek().is_some() {
            descriptor.fill(0);
            put_be32(&mut descriptor, 0, MAGIC);
            put_be32(&mut descriptor, 4, DESCRIPTOR);
            put_be32(&mut descriptor, 8, sequence);
            batch.clear();
            let mut pos = HEADER;
            let mut last_tag = HEADER;
            while batch.len() < per {
                let Some((home, image)) = blocks.next() else {
                    break;
                };
                if image.len() != self.block_size {
                    return Err(DriverError::BufferTooSmall);
                }
                let escape = be32(image, 0) == MAGIC;
                let mut flags = if escape { FLAG_ESCAPE } else { 0 };
                if !batch.is_empty() {
                    flags |= FLAG_SAME_UUID;
                }
                let checksum = self.tag_checksum(sequence, image, escape);
                put_be32(&mut descriptor, pos, low32(home));
                if self.v3() {
                    put_be32(&mut descriptor, pos + 4, flags);
                    put_be32(&mut descriptor, pos + 8, high32(home));
                    put_be32(&mut descriptor, pos + 12, checksum);
                } else {
                    let tag_sum = if self.checksummed() {
                        checksum & 0xFFFF
                    } else {
                        0
                    };
                    put_be32(&mut descriptor, pos + 4, (tag_sum << 16) | flags);
                    if self.wide() {
                        put_be32(&mut descriptor, pos + 8, high32(home));
                    } else if home >> 32 != 0 {
                        return Err(DriverError::DeviceFault);
                    }
                }
                last_tag = pos;
                pos += size;
                if batch.is_empty() {
                    descriptor[pos..pos + UUID_LEN]
                        .copy_from_slice(&self.sb[SB_UUID..SB_UUID + UUID_LEN]);
                    pos += UUID_LEN;
                }
                batch.push((image, escape));
            }
            Self::mark_last(&mut descriptor, last_tag);
            if self.checksummed() {
                let sum = self.block_checksum(&descriptor, self.block_size - TAIL);
                put_be32(&mut descriptor, self.block_size - TAIL, sum);
            }
            io.write_journal(at, &descriptor)?;
            at = self.next(at);
            for &(image, escape) in &batch {
                if escape {
                    escaped.copy_from_slice(image);
                    put_be32(&mut escaped, 0, 0);
                    io.write_journal(at, &escaped)?;
                } else {
                    io.write_journal(at, image)?;
                }
                at = self.next(at);
            }
        }
        io.flush()?;
        Ok(at)
    }

    /// Commit the transaction `log` wrote, whose commit block goes at `at`:
    /// the commit block and the superblock naming the transaction, then a
    /// barrier. Its place and id are spent from the first write on, so a
    /// retry never reuses what the medium may hold.
    fn seal<I: JournalIo>(
        &mut self,
        io: &mut I,
        at: u32,
        commit: &mut [u8],
        time: (u64, u32),
    ) -> Result<(), DriverError> {
        let (start, sequence) = (self.head, self.sequence);
        self.head = self.next(at);
        self.sequence = sequence.wrapping_add(1);
        put_be32(commit, 0, MAGIC);
        put_be32(commit, 4, COMMIT);
        put_be32(commit, 8, sequence);
        commit[COMMIT_SEC..COMMIT_SEC + 8].copy_from_slice(&time.0.to_be_bytes());
        put_be32(commit, COMMIT_NSEC, time.1);
        if self.checksummed() {
            let sum = self.block_checksum(commit, COMMIT_CHECKSUM);
            put_be32(commit, COMMIT_CHECKSUM, sum);
        }
        io.write_journal(at, commit)?;
        put_be32(&mut self.sb, SB_START, start);
        put_be32(&mut self.sb, SB_SEQUENCE, sequence);
        self.write_sb(io)?;
        io.flush()
    }

    /// Mark the tag at `at` the last of its descriptor. The flags sit in the
    /// word after the block number in both tag layouts, in its low half.
    fn mark_last(descriptor: &mut [u8], at: usize) {
        let word = be32(descriptor, at + 4) | FLAG_LAST_TAG;
        put_be32(descriptor, at + 4, word);
    }

    /// Every committed transaction is home: once those home writes are
    /// durable, empty the log, durably, so a quiet volume needs no replay.
    pub(crate) fn quiesce<I: JournalIo>(&mut self, io: &mut I) -> Result<(), DriverError> {
        if !self.has_log() {
            return Ok(());
        }
        io.flush()?;
        self.reset(io)?;
        io.flush()
    }

    fn reset<I: JournalIo>(&mut self, io: &mut I) -> Result<(), DriverError> {
        put_be32(&mut self.sb, SB_START, 0);
        put_be32(&mut self.sb, SB_SEQUENCE, self.sequence);
        self.write_sb(io)
    }

    fn write_sb<I: JournalIo>(&mut self, io: &mut I) -> Result<(), DriverError> {
        if be32(&self.sb, 4) == SUPERBLOCK_V2 {
            put_be32(&mut self.sb, SB_COMPAT, self.compat);
            put_be32(&mut self.sb, SB_INCOMPAT, self.incompat);
        }
        if self.checksummed() {
            self.sb[SB_CHECKSUM_TYPE] = CRC32C;
            let sum = sb_checksum(&self.sb);
            put_be32(&mut self.sb, SB_CHECKSUM, sum);
        }
        io.write_journal(0, &self.sb)
    }
}

/// One lap of the log's ring. Every block a walk crosses is counted, so a log
/// naming more blocks than its ring holds — blocks that alias, or a log that
/// laps itself — is refused rather than walked for ever.
struct Lap(u32);

impl Lap {
    fn of(journal: &Journal) -> Self {
        Self(journal.maxlen - journal.first)
    }

    /// The block after `n`, counted against the lap.
    fn step(&mut self, journal: &Journal, n: u32) -> Result<u32, DriverError> {
        self.0 = self.0.checked_sub(1).ok_or(DriverError::DeviceFault)?;
        Ok(journal.next(n))
    }
}

/// One descriptor tag: where a logged block belongs, and how it was logged.
struct Tag {
    home: u64,
    flags: u32,
    checksum: u32,
}

/// The tags of one descriptor block, read in place.
struct Tags<'b> {
    block: &'b [u8],
    at: usize,
    end: usize,
    size: usize,
    v3: bool,
    wide: bool,
    done: bool,
}

impl Iterator for Tags<'_> {
    type Item = Tag;

    fn next(&mut self) -> Option<Tag> {
        if self.done || self.at + self.size > self.end {
            return None;
        }
        let (block, at) = (self.block, self.at);
        let low = be32(block, at);
        let (flags, high, checksum) = if self.v3 {
            (
                be32(block, at + 4),
                be32(block, at + 8),
                be32(block, at + 12),
            )
        } else {
            let word = be32(block, at + 4);
            let high = if self.wide { be32(block, at + 8) } else { 0 };
            (word & 0xFFFF, high, word >> 16)
        };
        self.at += self.size;
        if flags & FLAG_SAME_UUID == 0 {
            self.at += UUID_LEN;
        }
        self.done = flags & FLAG_LAST_TAG != 0;
        Some(Tag {
            home: (u64::from(high) << 32) | u64::from(low),
            flags,
            checksum,
        })
    }
}

/// Blocks committed transactions revoked, each with the latest transaction
/// revoking it, counted from the log's first.
#[derive(Default)]
struct Revoked {
    entries: Vec<(u64, u32)>,
}

impl Revoked {
    /// Keep the revokes of a transaction that turned out committed, `age`
    /// transactions into the log.
    fn commit(&mut self, pending: &mut Vec<u64>, age: u32) -> Result<(), DriverError> {
        self.entries
            .try_reserve(pending.len())
            .map_err(|_| DriverError::NoSpace)?;
        self.entries
            .extend(pending.drain(..).map(|home| (home, age)));
        Ok(())
    }

    /// Sort for lookup, keeping each block's latest revoke.
    fn settle(&mut self) {
        self.entries
            .sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        self.entries.dedup_by_key(|entry| entry.0);
    }

    /// The age of the latest transaction that revoked `home`.
    fn by(&self, home: u64) -> Option<u32> {
        self.entries
            .binary_search_by_key(&home, |entry| entry.0)
            .ok()
            .map(|i| self.entries[i].1)
    }
}

/// The superblock checksum: crc32c over its first 1024 bytes with the
/// checksum field zeroed.
fn sb_checksum(sb: &[u8]) -> u32 {
    crc32c_zeroed::<4>(0xFFFF_FFFF, sb.get(..SB_LEN).unwrap_or(sb), SB_CHECKSUM)
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
