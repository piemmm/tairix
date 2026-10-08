//! Hash-indexed directories: the hash that places a name, and the index
//! blocks that map ranges of it to the leaf blocks holding the names.
//!
//! An independent implementation of the on-disk format, written from the
//! format and from the published algorithms its hashes build on — MD4's round
//! structure (RFC 1320) and the TEA cipher (Wheeler and Needham, *TEA, a Tiny
//! Encryption Algorithm*, 1994) — and checked, as a black box, against the
//! hashes e2fsprogs' `debugfs dx_hash` reports.

use tairix_abi::DriverError;

use super::{le16, le32, put_le16, put_le32, MAX_NAME_LEN};

/// The low bit of an index entry's hash, set when names hashing to its even
/// value continue from the block before.
pub(crate) const CONTINUED: u32 = 1;

/// The largest even hash, which marks the end of a directory and so places
/// no name.
const END: u32 = 0xFFFF_FFFE;

/// Interior levels under the root on a volume without `largedir`, the most
/// this driver reads or writes.
pub(crate) const MAX_LEVELS: u8 = 1;

/// The `hash_version` of `SipHash`, which only casefolded encrypted directories
/// use.
const SIPHASH: u8 = 6;

/// How an indexed directory's names hash: the algorithm its root names, how
/// the volume reads a name's bytes, and the volume's seed.
#[derive(Copy, Clone, Debug)]
pub(crate) struct NameHash {
    algorithm: Algorithm,
    signed: bool,
    seed: [u32; 4],
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Algorithm {
    Legacy,
    HalfMd4,
    Tea,
}

impl NameHash {
    /// The hash a root's `hash_version` byte names, on a volume whose
    /// superblock reads name bytes as `signed` (`None` when it records
    /// neither) and seeds hashes with `seed`.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for `SipHash`, and for a volume that does
    /// not record how it reads bytes; [`DriverError::DeviceFault`] for a
    /// version the format does not define for a root.
    pub(crate) fn new(
        stored: u8,
        signed: Option<bool>,
        seed: [u32; 4],
    ) -> Result<Self, DriverError> {
        let algorithm = match stored {
            0 => Algorithm::Legacy,
            1 => Algorithm::HalfMd4,
            2 => Algorithm::Tea,
            SIPHASH => return Err(DriverError::Unsupported),
            _ => return Err(DriverError::DeviceFault),
        };
        let signed = signed.ok_or(DriverError::Unsupported)?;
        Ok(Self {
            algorithm,
            signed,
            seed,
        })
    }

    /// The hash placing `name`; `None` for a name longer than any the format
    /// holds.
    pub(crate) fn of(&self, name: &[u8]) -> Option<u32> {
        if name.len() > MAX_NAME_LEN {
            return None;
        }
        let raw = match self.algorithm {
            Algorithm::Legacy => legacy(name, self.signed),
            Algorithm::HalfMd4 => self.compressed::<8>(name, half_md4)[1],
            Algorithm::Tea => self.compressed::<4>(name, tea)[0],
        };
        Some(match raw & !CONTINUED {
            END => END - 2,
            even => even,
        })
    }

    /// The state after compressing `name`, `4 * WORDS` bytes at a time, from
    /// the volume's seed — or MD4's initial state when that seed is zero.
    fn compressed<const WORDS: usize>(
        &self,
        name: &[u8],
        compress: fn(&mut [u32; 4], &[u32; WORDS]),
    ) -> [u32; 4] {
        let mut state = if self.seed == [0; 4] {
            MD4_INITIAL
        } else {
            self.seed
        };
        for start in (0..name.len()).step_by(4 * WORDS) {
            compress(&mut state, &pack(&name[start..], self.signed));
        }
        state
    }
}

/// A name byte as the hash takes it: sign-extended on a volume reading bytes
/// as signed.
fn byte_value(byte: u8, signed: bool) -> u32 {
    if signed {
        i32::from(i8::from_ne_bytes([byte])).cast_unsigned()
    } else {
        u32::from(byte)
    }
}

/// The message words one chunk packs into, `rest` being the name from the
/// chunk on. A word takes four bytes, the first most significant; a short
/// last group sits above a filler, and words past the name are the filler
/// alone — every byte of it the count of bytes `rest` holds.
fn pack<const WORDS: usize>(rest: &[u8], signed: bool) -> [u32; WORDS] {
    let filler = u32::from_ne_bytes([u8::try_from(rest.len()).unwrap_or(u8::MAX); 4]);
    core::array::from_fn(|word| {
        let group = rest.get(4 * word..).unwrap_or_default();
        group.iter().take(4).fold(filler, |acc, &byte| {
            (acc << 8).wrapping_add(byte_value(byte, signed))
        })
    })
}

/// The format's first hash, which directories indexed before the others
/// existed keep.
fn legacy(name: &[u8], signed: bool) -> u32 {
    let (hash, _) = name.iter().fold(
        (0x12A3_FE2D_u32, 0x37AB_E8F9_u32),
        |(current, previous), &byte| {
            let mixed =
                previous.wrapping_add(current ^ byte_value(byte, signed).wrapping_mul(7_152_373));
            let next = if mixed & 0x8000_0000 == 0 {
                mixed
            } else {
                mixed.wrapping_sub(0x7FFF_FFFF)
            };
            (next, current)
        },
    );
    hash << 1
}

/// MD4's initial state (RFC 1320).
const MD4_INITIAL: [u32; 4] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];

/// One round of the half-MD4 compression: its mixing function, the order it
/// takes the eight message words in, the rotation at each of its four step
/// positions, and its additive constant.
struct Md4Round {
    mix: fn(u32, u32, u32) -> u32,
    words: [usize; 8],
    rotations: [u32; 4],
    constant: u32,
}

/// MD4's three rounds and constants over an eight-word block, in the
/// format's word order.
const HALF_MD4: [Md4Round; 3] = [
    Md4Round {
        mix: |x, y, z| (x & y) | (!x & z),
        words: [0, 1, 2, 3, 4, 5, 6, 7],
        rotations: [3, 7, 11, 19],
        constant: 0,
    },
    Md4Round {
        mix: |x, y, z| (x & y) | (x & z) | (y & z),
        words: [1, 3, 5, 7, 0, 2, 4, 6],
        rotations: [3, 5, 9, 13],
        constant: 0x5A82_7999,
    },
    Md4Round {
        mix: |x, y, z| x ^ y ^ z,
        words: [3, 7, 2, 6, 1, 5, 0, 4],
        rotations: [3, 9, 11, 15],
        constant: 0x6ED9_EBA1,
    },
];

/// Compress `block` into `state` through [`HALF_MD4`], adding each register
/// back into the state.
fn half_md4(state: &mut [u32; 4], block: &[u32; 8]) {
    let mut regs = *state;
    for round in &HALF_MD4 {
        for (step, &word) in round.words.iter().enumerate() {
            // MD4 updates a, d, c and b in turn, each from the other three
            // taken in cyclic order.
            let target = (4 - step % 4) % 4;
            let mixed = (round.mix)(
                regs[(target + 1) % 4],
                regs[(target + 2) % 4],
                regs[(target + 3) % 4],
            );
            regs[target] = regs[target]
                .wrapping_add(mixed)
                .wrapping_add(block[word])
                .wrapping_add(round.constant)
                .rotate_left(round.rotations[step % 4]);
        }
    }
    for (word, reg) in state.iter_mut().zip(regs) {
        *word = word.wrapping_add(reg);
    }
}

/// TEA's round constant, 2^32 over the golden ratio.
const TEA_DELTA: u32 = 0x9E37_79B9;

/// The TEA cycles the format runs, half the cipher's recommended 32.
const TEA_CYCLES: usize = 16;

/// Compress `key` into `state` as a Davies–Meyer step over TEA: the packed
/// name is the key, the state's first two words the block, and the cipher's
/// output is added back in.
fn tea(state: &mut [u32; 4], key: &[u32; 4]) {
    let [k0, k1, k2, k3] = *key;
    let (mut y, mut z, mut sum) = (state[0], state[1], 0u32);
    for _ in 0..TEA_CYCLES {
        sum = sum.wrapping_add(TEA_DELTA);
        y = y.wrapping_add(
            (z << 4).wrapping_add(k0) ^ z.wrapping_add(sum) ^ (z >> 5).wrapping_add(k1),
        );
        z = z.wrapping_add(
            (y << 4).wrapping_add(k2) ^ y.wrapping_add(sum) ^ (y >> 5).wrapping_add(k3),
        );
    }
    state[0] = state[0].wrapping_add(y);
    state[1] = state[1].wrapping_add(z);
}

/// Byte offset of the root's info record, past `.` and `..`.
const ROOT_INFO: usize = 0x18;
/// The info record's length, the only one the format defines.
const ROOT_INFO_LEN: u8 = 8;
/// The info record's flag bits a reader that does not know them must refuse.
const ROOT_INCOMPAT_FLAGS: u8 = 1;
/// Bytes of one index entry: a hash and a logical block.
const ENTRY: usize = 8;
/// Bytes of the checksum tail after an index's entries on a
/// `metadata_csum` volume: a reserved word, then the checksum.
const TAIL: usize = 8;

/// Which index block this is, and so where its entries start.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Level {
    /// The root, block 0: entries past `.`, `..` and the info record.
    Root,
    /// An interior node: entries past one empty record spanning the block.
    Node,
}

impl Level {
    /// Where the count, limit and entries begin.
    const fn offset(self) -> usize {
        match self {
            Self::Root => ROOT_INFO + ROOT_INFO_LEN as usize,
            Self::Node => 8,
        }
    }

    /// The entries this level holds in a `block_size`-byte block.
    pub(crate) const fn limit(self, block_size: usize, csum: bool) -> usize {
        (block_size - self.offset() - if csum { TAIL } else { 0 }) / ENTRY
    }
}

/// An index's entries: a count and a limit, then `(hash, block)` pairs in
/// ascending hash order, the first pair's hash field holding the count and
/// limit, as its range starts wherever its parent's does.
pub(crate) struct Entries<'a> {
    block: &'a [u8],
    at: usize,
}

impl<'a> Entries<'a> {
    /// The index of `level` in `block`.
    pub(crate) fn of(block: &'a [u8], level: Level) -> Self {
        Self {
            block,
            at: level.offset(),
        }
    }

    pub(crate) fn count(&self) -> usize {
        usize::from(le16(self.block, self.at + 2))
    }

    fn limit(&self) -> usize {
        usize::from(le16(self.block, self.at))
    }

    /// Entry `index`'s hash, its continuation bit included; the first
    /// entry's is the bottom of the index's range, so `0` here.
    pub(crate) fn hash(&self, index: usize) -> u32 {
        if index == 0 {
            0
        } else {
            le32(self.block, self.at + index * ENTRY)
        }
    }

    /// The logical block entry `index` points at.
    pub(crate) fn child(&self, index: usize) -> u32 {
        le32(self.block, self.at + index * ENTRY + 4)
    }

    /// The entry whose range holds `hash`: the last whose hash is at most it.
    /// A continued entry's hash is odd, so `hash` (always even) finds the
    /// entry before it, where its run of equal hashes starts.
    pub(crate) fn find(&self, hash: u32) -> usize {
        let (mut low, mut high) = (1, self.count());
        while low < high {
            let mid = low + (high - low) / 2;
            if self.hash(mid) > hash {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        low - 1
    }

    /// Whether the index has no room for another entry.
    pub(crate) fn is_full(&self) -> bool {
        self.count() >= self.limit()
    }
}

/// An index's entries, to change.
pub(crate) struct EntriesMut<'a> {
    block: &'a mut [u8],
    at: usize,
}

impl<'a> EntriesMut<'a> {
    /// The index of `level` in `block`.
    pub(crate) fn of(block: &'a mut [u8], level: Level) -> Self {
        Self {
            block,
            at: level.offset(),
        }
    }

    fn view(&self) -> Entries<'_> {
        Entries {
            block: self.block,
            at: self.at,
        }
    }

    fn set_count(&mut self, count: usize) -> Result<(), DriverError> {
        put_le16(
            self.block,
            self.at + 2,
            u16::try_from(count).map_err(|_| DriverError::DeviceFault)?,
        );
        Ok(())
    }

    /// Add `(hash, child)` just after entry `after`, which must exist, in an
    /// index with room.
    pub(crate) fn insert(
        &mut self,
        after: usize,
        hash: u32,
        child: u32,
    ) -> Result<(), DriverError> {
        let count = self.view().count();
        if self.view().is_full() || after >= count {
            return Err(DriverError::DeviceFault);
        }
        let from = self.at + (after + 1) * ENTRY;
        self.block
            .copy_within(from..self.at + count * ENTRY, from + ENTRY);
        put_le32(self.block, from, hash);
        put_le32(self.block, from + 4, child);
        self.set_count(count + 1)
    }

    /// Move entries `from..` into `into`, an empty node, leaving the others;
    /// answers the first moved entry's hash, which starts `into`'s range.
    pub(crate) fn split_off(
        &mut self,
        from: usize,
        into: &mut EntriesMut<'_>,
    ) -> Result<u32, DriverError> {
        let count = self.view().count();
        if from == 0 || from >= count || count - from > into.view().limit() {
            return Err(DriverError::DeviceFault);
        }
        let lowest = self.view().hash(from);
        into.append_from(&self.view(), from..count)?;
        self.set_count(from)?;
        Ok(lowest)
    }

    /// Move every entry into `into`, an empty node, and leave this index one
    /// entry pointing at `node`.
    pub(crate) fn push_down(
        &mut self,
        into: &mut EntriesMut<'_>,
        node: u32,
    ) -> Result<(), DriverError> {
        let count = self.view().count();
        into.append_from(&self.view(), 0..count)?;
        put_le32(self.block, self.at + 4, node);
        self.set_count(1)
    }

    /// Fill this empty index with `source`'s entries `range`; the first's
    /// hash is dropped, as an index's range starts wherever its parent's
    /// entry for it does.
    fn append_from(
        &mut self,
        source: &Entries<'_>,
        range: core::ops::Range<usize>,
    ) -> Result<(), DriverError> {
        if self.view().count() != 0 || range.len() > self.view().limit() {
            return Err(DriverError::DeviceFault);
        }
        for (slot, index) in range.clone().enumerate() {
            let at = self.at + slot * ENTRY;
            if slot > 0 {
                put_le32(self.block, at, source.hash(index));
            }
            put_le32(self.block, at + 4, source.child(index));
        }
        self.set_count(range.len())
    }
}

/// What a checked root says of its index: the hash its names are placed by,
/// and how many interior levels lie under it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct RootInfo {
    pub(crate) hash_version: u8,
    pub(crate) levels: u8,
}

/// Check the shape of `block` as an indexed directory's root: `.` and `..`
/// laid out as the format places them, the info record, and the index after
/// it, sized for a checksum tail when `csum`. [`verify`] checks the tail.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a block that is not a valid root.
pub(crate) fn check_root(block: &[u8], csum: bool) -> Result<RootInfo, DriverError> {
    let dot_dot_len = usize::from(le16(block, 12 + 4));
    if usize::from(le16(block, 4)) != 12
        || dot_dot_len != block.len().saturating_sub(12)
        || le32(block, ROOT_INFO) != 0
        || block.get(ROOT_INFO + 5) != Some(&ROOT_INFO_LEN)
        || block
            .get(ROOT_INFO + 7)
            .is_none_or(|flags| flags & ROOT_INCOMPAT_FLAGS != 0)
    {
        return Err(DriverError::DeviceFault);
    }
    check_index(block, Level::Root, csum)?;
    Ok(RootInfo {
        hash_version: block[ROOT_INFO + 4],
        levels: block[ROOT_INFO + 6],
    })
}

/// Record `levels` interior levels under the root in `block`.
pub(crate) fn set_levels(block: &mut [u8], levels: u8) {
    if let Some(byte) = block.get_mut(ROOT_INFO + 6) {
        *byte = levels;
    }
}

/// Check the shape of `block` as an interior node: one empty record spanning
/// the block, then the index, sized for a checksum tail when `csum`.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a block that is not a valid node.
pub(crate) fn check_node(block: &[u8], csum: bool) -> Result<(), DriverError> {
    if le32(block, 0) != 0 || usize::from(le16(block, 4)) != block.len() {
        return Err(DriverError::DeviceFault);
    }
    check_index(block, Level::Node, csum)
}

/// Lay `block` out as an empty interior node.
pub(crate) fn init_node(block: &mut [u8], csum: bool) -> Result<(), DriverError> {
    block.fill(0);
    put_le16(
        block,
        4,
        u16::try_from(block.len()).map_err(|_| DriverError::DeviceFault)?,
    );
    let limit = Level::Node.limit(block.len(), csum);
    put_le16(
        block,
        Level::Node.offset(),
        u16::try_from(limit).map_err(|_| DriverError::DeviceFault)?,
    );
    Ok(())
}

/// Check the index of `level` in `block`: the limit its block size and any
/// checksum tail give, a count within it, and hashes in order.
fn check_index(block: &[u8], level: Level, csum: bool) -> Result<(), DriverError> {
    let entries = Entries::of(block, level);
    let count = entries.count();
    let ordered = (2..count).all(|index| entries.hash(index - 1) <= entries.hash(index));
    if entries.limit() != level.limit(block.len(), csum)
        || count == 0
        || count > entries.limit()
        || !ordered
    {
        return Err(DriverError::DeviceFault);
    }
    Ok(())
}

/// Whether the checksum tail of `level`'s index in `block`, a shape
/// [`check_root`] or [`check_node`] passed, holds its checksum under `seed`.
pub(crate) fn verify(block: &[u8], level: Level, seed: u32) -> bool {
    le32(block, tail_at(block, level) + 4) == tail_checksum(block, level, seed)
}

/// Where the checksum tail of `level`'s index sits in `block`: just past the
/// last entry its limit allows.
fn tail_at(block: &[u8], level: Level) -> usize {
    level.offset() + Entries::of(block, level).limit() * ENTRY
}

/// The checksum of `level`'s index in `block`: over the block up to its last
/// entry in use, the tail's reserved word, and zero in place of the checksum.
fn tail_checksum(block: &[u8], level: Level, seed: u32) -> u32 {
    let used = level.offset() + Entries::of(block, level).count() * ENTRY;
    let tail = tail_at(block, level);
    let csum = tairix_crc32c::update(seed, block.get(..used).unwrap_or_default());
    let csum = tairix_crc32c::update(csum, block.get(tail..tail + 4).unwrap_or_default());
    tairix_crc32c::update(csum, &[0; 4])
}

/// Stamp the checksum of `level`'s index in `block` under `seed`.
pub(crate) fn seal(block: &mut [u8], level: Level, seed: u32) {
    let csum = tail_checksum(block, level, seed);
    put_le32(block, tail_at(block, level) + 4, csum);
}

#[cfg(test)]
#[path = "htree_tests.rs"]
mod tests;
