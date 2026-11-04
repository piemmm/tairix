//! AmigaDOS volume recognition: the boot block's `DOS` type and the root
//! block every OFS and FFS volume keeps at its middle.
//!
//! The driver (`drivers/filesystem/affs`) reads its volume through the same
//! constants, checksum and [`Layout`], so a probe and a mounted volume never
//! disagree about where the root lies or what kind of volume it is.
//!
//! The layout is the one the AmigaDOS ROM filesystems write (*The .ADF
//! (Amiga Disk File) format FAQ*, L. Clévy): big-endian longwords, a
//! structure's checksum making all of its longwords sum to zero, and every
//! header block's fields placed from the end of the block, so one description
//! serves every block size a volume may be formatted with.

use tairix_abi::blkio::BlkForm;
use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

use crate::{content_identity, read_present};

/// The boot blocks every volume reserves ahead of its first allocatable
/// block; the bitmap's first bit is the block after them.
pub const BOOT_BLOCKS: u64 = 2;

/// The type field of every header block: the root, a directory, a file, a
/// link.
pub const T_HEADER: u32 = 2;
/// The type field of an OFS data block.
pub const T_DATA: u32 = 8;
/// The type field of a file extension block, holding more data pointers.
pub const T_LIST: u32 = 16;

/// The secondary type of the root block.
pub const ST_ROOT: u32 = 1;
/// The secondary type of a directory.
pub const ST_USERDIR: u32 = 2;
/// The secondary type of a soft link.
pub const ST_SOFTLINK: u32 = 3;
/// The secondary type of a hard link to a directory.
pub const ST_LINKDIR: u32 = 4;
/// The secondary type of a file: −3 as a signed longword.
pub const ST_FILE: u32 = 0xFFFF_FFFD;
/// The secondary type of a hard link to a file: −4 as a signed longword.
pub const ST_LINKFILE: u32 = 0xFFFF_FFFC;

/// The root's bitmap flag on a validated volume, whose bitmap is to be trusted.
pub const BITMAP_VALID: u32 = u32::MAX;

/// The bitmap block pointers the root block holds; a larger volume chains
/// bitmap extension blocks from the root's `bm_ext`.
pub const BITMAP_PAGES: usize = 25;

/// The longest name a classic volume records for an object or itself.
pub const NAME_MAX: usize = 30;

/// The block sizes a volume may be formatted with: a floppy's 512, and the
/// larger ones a hard-disk partition may choose.
pub const BLOCK_SIZES: [u32; 7] = [512, 1024, 2048, 4096, 8192, 16384, 32768];

/// The blocks of a double-density floppy (880 KiB).
pub const DD_FLOPPY_BLOCKS: u64 = 1760;
/// The blocks of a high-density floppy (1760 KiB).
pub const HD_FLOPPY_BLOCKS: u64 = 3520;

/// Byte offset, from a header block's start, of its hash table or data block
/// pointers.
pub const TABLE: usize = 24;

/// Where each header field lies in a block of `block_size` bytes: the fields
/// are placed from the block's end (`BSIZE - n` in the FAQ's terms).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Offsets {
    /// The block size these offsets are for.
    pub block_size: usize,
}

impl Offsets {
    /// The offsets for a block of `block_size` bytes.
    #[must_use]
    pub const fn new(block_size: usize) -> Self {
        Self { block_size }
    }

    /// Longwords in the block.
    #[must_use]
    pub const fn longs(self) -> usize {
        self.block_size / 4
    }

    /// Entries of a directory's hash table, and data block pointers a file
    /// header or extension block holds.
    #[must_use]
    pub const fn table_len(self) -> usize {
        self.longs() - 56
    }

    /// The root's bitmap flag.
    #[must_use]
    pub const fn bm_flag(self) -> usize {
        self.block_size - 200
    }

    /// The root's bitmap block pointers.
    #[must_use]
    pub const fn bm_pages(self) -> usize {
        self.block_size - 196
    }

    /// The root's first bitmap extension block.
    #[must_use]
    pub const fn bm_ext(self) -> usize {
        self.block_size - 96
    }

    /// A header's protection bits.
    #[must_use]
    pub const fn protect(self) -> usize {
        self.block_size - 192
    }

    /// A file header's byte size.
    #[must_use]
    pub const fn byte_size(self) -> usize {
        self.block_size - 188
    }

    /// A header's comment length, its comment following.
    #[must_use]
    pub const fn comment(self) -> usize {
        self.block_size - 184
    }

    /// A header's date stamp: the last change, or the root's last
    /// alteration.
    #[must_use]
    pub const fn date(self) -> usize {
        self.block_size - 92
    }

    /// A header's name length, its name following.
    #[must_use]
    pub const fn name(self) -> usize {
        self.block_size - 80
    }

    /// A hard link's real object.
    #[must_use]
    pub const fn real_entry(self) -> usize {
        self.block_size - 44
    }

    /// The head of the chain of hard links an object has, or a link's next.
    #[must_use]
    pub const fn next_link(self) -> usize {
        self.block_size - 40
    }

    /// The root's last disk alteration date.
    #[must_use]
    pub const fn volume_date(self) -> usize {
        self.block_size - 40
    }

    /// The root's creation date.
    #[must_use]
    pub const fn created(self) -> usize {
        self.block_size - 28
    }

    /// The next header in a hash chain.
    #[must_use]
    pub const fn hash_chain(self) -> usize {
        self.block_size - 16
    }

    /// A header's parent directory.
    #[must_use]
    pub const fn parent(self) -> usize {
        self.block_size - 12
    }

    /// A file's first extension block, or a directory's first cache block.
    #[must_use]
    pub const fn extension(self) -> usize {
        self.block_size - 8
    }

    /// A header's secondary type.
    #[must_use]
    pub const fn sec_type(self) -> usize {
        self.block_size - 4
    }
}

/// What the boot block's fourth byte says the volume is: `DOS\0` to `DOS\7`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DosType(u8);

impl DosType {
    /// The type `flags` names, or `None` past `DOS\7`.
    #[must_use]
    pub const fn new(flags: u8) -> Option<Self> {
        if flags <= 7 {
            Some(Self(flags))
        } else {
            None
        }
    }

    /// The boot block's fourth byte.
    #[must_use]
    pub const fn flags(self) -> u8 {
        self.0
    }

    /// Whether data blocks are bare (FFS) rather than headed (OFS).
    #[must_use]
    pub const fn is_fast(self) -> bool {
        self.0 & 1 != 0
    }

    /// Whether names fold Latin-1 letters as well as ASCII (international
    /// mode, which every type past `DOS\1` has).
    #[must_use]
    pub const fn is_international(self) -> bool {
        self.0 >= 2
    }

    /// Whether directories keep cache blocks (`DOS\4`, `DOS\5`).
    #[must_use]
    pub const fn has_dir_cache(self) -> bool {
        matches!(self.0, 4 | 5)
    }

    /// Whether names may be long (`DOS\6`, `DOS\7`, AmigaOS 3.2).
    #[must_use]
    pub const fn has_long_names(self) -> bool {
        self.0 >= 6
    }
}

/// An AmigaDOS date stamp: days since 1978-01-01, minutes past midnight,
/// ticks (1/50 s) past the minute.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DateStamp {
    /// Days since 1978-01-01.
    pub days: u32,
    /// Minutes past midnight.
    pub mins: u32,
    /// Fiftieths of a second past the minute.
    pub ticks: u32,
}

impl DateStamp {
    /// The stamp at `at` in `block`.
    #[must_use]
    pub fn read(block: &[u8], at: usize) -> Self {
        Self {
            days: be32(block, at),
            mins: be32(block, at + 4),
            ticks: be32(block, at + 8),
        }
    }

    /// Write the stamp at `at` in `block`.
    pub fn write(self, block: &mut [u8], at: usize) {
        put_be32(block, at, self.days);
        put_be32(block, at + 4, self.mins);
        put_be32(block, at + 8, self.ticks);
    }
}

/// An AmigaDOS volume, as recognition found it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    /// What the boot block says the volume is.
    pub dos_type: DosType,
    /// The volume's block size.
    pub block_size: u32,
    /// The volume's blocks, the boot blocks included.
    pub blocks: u64,
    /// Where its root block lies.
    pub root: u64,
    /// When it was formatted.
    pub created: DateStamp,
    /// Whether its bitmap was left validated.
    pub bitmap_valid: bool,
    name: [u8; NAME_MAX],
    name_len: u8,
}

impl Layout {
    /// The volume's stable identity: a digest of what formatting fixes and
    /// neither a write nor a retitle changes — the type, the geometry and the
    /// root's creation date. AmigaDOS tells volumes apart by name and creation
    /// date; the name is a retitle's to change, so this keeps to the date,
    /// stamped to the fiftieth of a second.
    #[must_use]
    pub fn identity(&self) -> [u8; 16] {
        let mut created = [0u8; 12];
        self.created.write(&mut created, 0);
        content_identity(
            b"tairix.affs",
            &[
                &[self.dos_type.flags()],
                &self.block_size.to_le_bytes(),
                &self.blocks.to_le_bytes(),
                &self.root.to_le_bytes(),
                &created,
            ],
        )
    }

    /// The volume's name, ISO 8859-1 as recorded.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name[..usize::from(self.name_len)]
    }

    /// The medium it was laid out for: a floppy for a 512-byte volume of a
    /// double- or high-density floppy's blocks, else unstated.
    #[must_use]
    pub const fn form(&self) -> BlkForm {
        if self.block_size == 512
            && (self.blocks == DD_FLOPPY_BLOCKS || self.blocks == HD_FLOPPY_BLOCKS)
        {
            BlkForm::FloppyDisk
        } else {
            BlkForm::Unspecified
        }
    }
}

/// The big-endian longword at `at` in `block`.
#[must_use]
pub fn be32(block: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]])
}

/// Write `value` big-endian at `at` in `block`.
pub fn put_be32(block: &mut [u8], at: usize, value: u32) {
    block[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// The wrapping sum of `block`'s longwords, `skip` excepted.
fn sum_except(block: &[u8], skip: usize) -> u32 {
    block
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|&(at, _)| at * 4 != skip)
        .fold(0u32, |sum, (_, long)| {
            sum.wrapping_add(u32::from_be_bytes(*long))
        })
}

/// The checksum's byte offset in a header, a file extension block and an OFS
/// data block.
pub const CHECKSUM: usize = 20;

/// A bitmap block's checksum: its first longword, its bits following.
pub const BITMAP_CHECKSUM: usize = 0;

/// Seal `block` with the checksum at byte `at` that makes its longwords sum to
/// zero; the longword at `at` is not summed.
pub fn seal(block: &mut [u8], at: usize) {
    let sum = sum_except(block, at).wrapping_neg();
    put_be32(block, at, sum);
}

/// Whether `block`'s longwords sum to zero, its stored checksum included.
#[must_use]
pub fn sums_to_zero(block: &[u8]) -> bool {
    sum_except(block, usize::MAX) == 0
}

/// Where a volume of `blocks` blocks keeps its root: at its middle, counted
/// with the boot blocks as AmigaDOS's formatter counts it.
#[must_use]
pub const fn root_block(blocks: u64) -> u64 {
    (blocks + BOOT_BLOCKS - 1) / 2
}

/// Whether `block` is a root block for a volume of `block_size` bytes a block:
/// every fixed field as the formatter writes it, a name within bounds, and the
/// checksum holding.
#[must_use]
pub fn is_root(block: &[u8], block_size: usize) -> bool {
    let at = Offsets::new(block_size);
    block.len() == block_size
        && be32(block, 0) == T_HEADER
        && be32(block, 4) == 0
        && be32(block, 8) == 0
        && usize::try_from(be32(block, 12)) == Ok(at.table_len())
        && be32(block, 16) == 0
        && be32(block, at.sec_type()) == ST_ROOT
        && usize::from(block[at.name()]) <= NAME_MAX
        && sums_to_zero(block)
}

/// Recognise an AmigaDOS volume on `dev`: a boot block naming `DOS\0` to
/// `DOS\7`, and a root block at the volume's middle for one of the
/// [`BLOCK_SIZES`], or at the block the boot block names, that
/// [`is_root`]. The boot block's own checksum holds only on a bootable disk,
/// so it is not asked for. `None` when nothing holds — never a guess.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub fn identify<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<Layout>, DriverError> {
    let mut boot = [0u8; 12];
    if !read_present(dev, 0, &mut boot)? || &boot[..3] != b"DOS" {
        return Ok(None);
    }
    let Some(dos_type) = DosType::new(boot[3]) else {
        return Ok(None);
    };
    let named_root = u64::from(be32(&boot, 8));
    let mut block = crate::heap_buf::<32_768>()?;
    for size in BLOCK_SIZES {
        let bytes = u64::from(size);
        let blocks = dev.capacity() / bytes;
        if blocks < BOOT_BLOCKS + 2 {
            continue;
        }
        let computed = root_block(blocks);
        let candidates = [computed, named_root];
        for (at, &root) in candidates.iter().enumerate() {
            if root < BOOT_BLOCKS || root >= blocks || (at > 0 && root == computed) {
                continue;
            }
            let block = &mut block[..size as usize];
            if !read_present(dev, root * bytes, block)? || !is_root(block, size as usize) {
                continue;
            }
            let offsets = Offsets::new(size as usize);
            let name_at = offsets.name();
            let name_len = block[name_at];
            let mut name = [0u8; NAME_MAX];
            name[..usize::from(name_len)]
                .copy_from_slice(&block[name_at + 1..name_at + 1 + usize::from(name_len)]);
            return Ok(Some(Layout {
                dos_type,
                block_size: size,
                blocks,
                root,
                created: DateStamp::read(block, offsets.created()),
                bitmap_valid: be32(block, offsets.bm_flag()) == BITMAP_VALID,
                name,
                name_len,
            }));
        }
    }
    Ok(None)
}

/// The blocks an Amiga Rigid Disk Block may lie in, the first it is looked for.
pub const RDB_SEARCH_BLOCKS: usize = 16;

/// The block size the Rigid Disk Block is looked for in.
const RDB_BLOCK: usize = 512;

/// The bytes the Rigid Disk Block is looked for in.
const RDB_WINDOW: usize = RDB_SEARCH_BLOCKS * RDB_BLOCK;

/// Whether `dev` opens with an Amiga Rigid Disk Block: `RDSK` in one of its
/// first [`RDB_SEARCH_BLOCKS`] blocks, its summed longwords within the block
/// and summing to zero. Such a disk holds partitions, not one volume.
///
/// The window is read in one transfer.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub fn rigid_disk_block<B: Block>(dev: &mut ByteDevice<B>) -> Result<bool, DriverError> {
    let (window, len) = crate::read_upto::<_, RDB_WINDOW>(dev, 0)?;
    Ok(window[..len]
        .as_chunks::<RDB_BLOCK>()
        .0
        .iter()
        .any(is_rigid_disk_block))
}

/// Whether `block` is a Rigid Disk Block: `RDSK`, its summed longwords within
/// the block and summing to zero.
fn is_rigid_disk_block(block: &[u8; RDB_BLOCK]) -> bool {
    let summed = usize::try_from(be32(block, 4)).unwrap_or(usize::MAX);
    &block[..4] == b"RDSK"
        && (3..=RDB_BLOCK / 4).contains(&summed)
        && sums_to_zero(&block[..summed * 4])
}
