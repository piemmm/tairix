//! The superblock: its field offsets, the feature words read against what
//! this driver implements, and the geometry derived from them.
//!
//! A volume is mounted only when every incompatible feature it carries is one
//! the read path implements; it is written only when every incompatible and
//! read-only-compatible feature is one the write path maintains, and nothing
//! recorded against it says writing is unsafe.

use tairix_abi::DriverError;

use crate::bytes::{le16, le32, low32};

/// The superblock's fixed byte offset, whatever the block size.
pub(crate) const SUPERBLOCK_OFFSET: u64 = 1024;
/// Bytes of the superblock.
pub(crate) const SUPERBLOCK_LEN: usize = 1024;

pub(crate) const SB_INODES_COUNT: usize = 0x00;
pub(crate) const SB_BLOCKS_LO: usize = 0x04;
pub(crate) const SB_R_BLOCKS_LO: usize = 0x08;
pub(crate) const SB_FREE_BLOCKS_LO: usize = 0x0C;
pub(crate) const SB_FREE_INODES: usize = 0x10;
pub(crate) const SB_FIRST_DATA_BLOCK: usize = 0x14;
pub(crate) const SB_LOG_BLOCK_SIZE: usize = 0x18;
const SB_LOG_CLUSTER_SIZE: usize = 0x1C;
const SB_BLOCKS_PER_GROUP: usize = 0x20;
const SB_CLUSTERS_PER_GROUP: usize = 0x24;
const SB_INODES_PER_GROUP: usize = 0x28;
pub(crate) const SB_MTIME: usize = 0x2C;
pub(crate) const SB_WTIME: usize = 0x30;
pub(crate) const SB_MNT_COUNT: usize = 0x34;
const SB_MAGIC: usize = 0x38;
pub(crate) const SB_STATE: usize = 0x3A;
const SB_CREATOR_OS: usize = 0x48;
const SB_REV_LEVEL: usize = 0x4C;
const SB_FIRST_INO: usize = 0x54;
const SB_INODE_SIZE: usize = 0x58;
const SB_FEATURE_COMPAT: usize = 0x5C;
pub(crate) const SB_FEATURE_INCOMPAT: usize = 0x60;
pub(crate) const SB_FEATURE_RO_COMPAT: usize = 0x64;
pub(crate) const SB_UUID: usize = 0x68;
const SB_RESERVED_GDT: usize = 0xCE;
const SB_JOURNAL_INUM: usize = 0xE0;
const SB_JOURNAL_DEV: usize = 0xE4;
pub(crate) const SB_LAST_ORPHAN: usize = 0xE8;
pub(crate) const SB_HASH_SEED: usize = 0xEC;
const SB_DEF_HASH_VERSION: usize = 0xFC;
const SB_DESC_SIZE: usize = 0xFE;
const SB_FIRST_META_BG: usize = 0x104;
const SB_BLOCKS_HI: usize = 0x150;
pub(crate) const SB_R_BLOCKS_HI: usize = 0x154;
const SB_FREE_BLOCKS_HI: usize = 0x158;
pub(crate) const SB_FLAGS: usize = 0x160;
const SB_BACKUP_BGS: usize = 0x24C;
const SB_CHECKSUM_TYPE: usize = 0x175;
const SB_CHECKSUM_SEED: usize = 0x270;
pub(crate) const SB_WTIME_HI: usize = 0x274;
pub(crate) const SB_MTIME_HI: usize = 0x275;
const SB_ORPHAN_FILE_INUM: usize = 0x280;
pub(crate) const SB_CHECKSUM: usize = 0x3FC;

/// `s_magic`; the one definition lives where the volume probe shares it.
pub(crate) const EXT_MAGIC: u16 = tairix_fsprobe::EXT4_SUPERBLOCK_MAGIC;

/// `s_flags`: how an indexed directory's hash reads name bytes.
pub(crate) const FLAG_SIGNED_HASH: u32 = 0x0001;
pub(crate) const FLAG_UNSIGNED_HASH: u32 = 0x0002;

/// `s_state`: cleanly unmounted / errors detected.
pub(crate) const STATE_VALID: u16 = 0x0001;
pub(crate) const STATE_ERROR: u16 = 0x0002;

pub(crate) const COMPAT_HAS_JOURNAL: u32 = 0x0004;
pub(crate) const COMPAT_RESIZE_INODE: u32 = 0x0010;
pub(crate) const COMPAT_DIR_INDEX: u32 = 0x0020;
const COMPAT_EXCLUDE_BITMAP: u32 = 0x0100;
pub(crate) const COMPAT_SPARSE_SUPER2: u32 = 0x0200;
pub(crate) const COMPAT_ORPHAN_FILE: u32 = 0x1000;

const INCOMPAT_COMPRESSION: u32 = 0x0001;
pub(crate) const INCOMPAT_FILETYPE: u32 = 0x0002;
pub(crate) const INCOMPAT_RECOVER: u32 = 0x0004;
const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
pub(crate) const INCOMPAT_META_BG: u32 = 0x0010;
pub(crate) const INCOMPAT_EXTENTS: u32 = 0x0040;
pub(crate) const INCOMPAT_64BIT: u32 = 0x0080;
/// Multi-mount protection: another host may be writing the volume, which
/// only that protocol's own check could rule out.
pub(crate) const INCOMPAT_MMP: u32 = 0x0100;
pub(crate) const INCOMPAT_FLEX_BG: u32 = 0x0200;
const INCOMPAT_EA_INODE: u32 = 0x0400;
pub(crate) const INCOMPAT_CSUM_SEED: u32 = 0x2000;
pub(crate) const INCOMPAT_LARGEDIR: u32 = 0x4000;
pub(crate) const INCOMPAT_INLINE_DATA: u32 = 0x8000;
const INCOMPAT_ENCRYPT: u32 = 0x1_0000;
const INCOMPAT_CASEFOLD: u32 = 0x2_0000;

/// Incompatible features the read path implements.
const READ_INCOMPAT: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_RECOVER
    | INCOMPAT_META_BG
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_MMP
    | INCOMPAT_FLEX_BG
    | INCOMPAT_EA_INODE
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR
    | INCOMPAT_INLINE_DATA
    | INCOMPAT_ENCRYPT
    | INCOMPAT_CASEFOLD;
/// Incompatible features the write path maintains. `recover` is replayed
/// away before the first write.
const WRITE_INCOMPAT: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_RECOVER
    | INCOMPAT_META_BG
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_FLEX_BG
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR;

pub(crate) const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
pub(crate) const RO_COMPAT_LARGE_FILE: u32 = 0x0002;
pub(crate) const RO_COMPAT_HUGE_FILE: u32 = 0x0008;
pub(crate) const RO_COMPAT_GDT_CSUM: u32 = 0x0010;
/// Block bitmaps count clusters of several blocks.
const RO_COMPAT_BIGALLOC: u32 = 0x0200;
pub(crate) const RO_COMPAT_DIR_NLINK: u32 = 0x0020;
pub(crate) const RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
pub(crate) const RO_COMPAT_METADATA_CSUM: u32 = 0x0400;
pub(crate) const RO_COMPAT_ORPHAN_PRESENT: u32 = 0x1_0000;
/// Read-only-compatible features the write path maintains; the orphan file's
/// entries are processed before the first write.
const WRITE_RO_COMPAT: u32 = RO_COMPAT_SPARSE_SUPER
    | RO_COMPAT_LARGE_FILE
    | RO_COMPAT_HUGE_FILE
    | RO_COMPAT_GDT_CSUM
    | RO_COMPAT_DIR_NLINK
    | RO_COMPAT_EXTRA_ISIZE
    | RO_COMPAT_METADATA_CSUM
    | RO_COMPAT_ORPHAN_PRESENT;

/// `s_creator_os` for GNU Hurd, whose inode layout differs in its osd fields.
const OS_HURD: u32 = 1;
/// `s_checksum_type` for crc32c.
const CHECKSUM_CRC32C: u8 = 1;

/// The largest block size this driver stages.
pub(crate) const MAX_FS_BLOCK: u32 = 4096;

/// The widest cluster a `bigalloc` volume may declare, as a power of two
/// of blocks: mke2fs documents two to 32768 blocks a cluster.
const MAX_LOG_BLOCKS_PER_CLUSTER: u32 = 15;

/// The narrowest group descriptor `64bit` allows: the high words of its
/// block numbers live in its second half.
const MIN_WIDE_DESC: u32 = 64;

/// The widest group descriptor the format allows: one smallest block.
const MAX_DESC_SIZE: u32 = 1024;

/// Why a volume is not written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum NoWrite {
    /// It carries a feature the write path does not maintain.
    Feature,
    /// A checker or kernel recorded errors against it.
    Errors,
    /// Its inode layout is the Hurd's.
    Hurd,
    /// Its journal is one this driver cannot write, or recorded an abort.
    Journal,
    /// A crash left it needing a checker: unjournaled and not marked
    /// clean, or with orphans that could not be finished.
    Unclean,
    /// It was opened read-only.
    ReadOnly,
    /// A commit failed with operations already reported done, so the handle
    /// stopped rather than serve writes it can no longer honour.
    Halted,
}

/// Validated geometry and the feature facts every path consults.
//
// The flags are independent on-disk feature toggles, not a state machine,
// so a flat record reads more clearly than an enum.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Layout {
    pub(crate) block_size: u32,
    /// The allocation cluster: the block size unless `bigalloc`.
    pub(crate) cluster_size: u32,
    pub(crate) blocks_count: u64,
    pub(crate) inodes_count: u32,
    pub(crate) inodes_per_group: u32,
    pub(crate) blocks_per_group: u32,
    pub(crate) inode_size: u32,
    pub(crate) desc_size: u32,
    pub(crate) group_count: u64,
    pub(crate) first_data_block: u64,
    pub(crate) first_ino: u32,
    /// The block the superblock lies in, and its offset within it.
    pub(crate) sb_block: u64,
    pub(crate) sb_offset: usize,
    /// The first block of the traditional descriptor table.
    pub(crate) gdt_block: u64,
    /// Blocks of descriptors a full table would take.
    pub(crate) gdt_blocks: u64,
    pub(crate) reserved_gdt: u64,
    /// Meta-groups below this keep the traditional table; `None` without
    /// `meta_bg`.
    pub(crate) first_meta_bg: Option<u64>,
    pub(crate) sparse_super: bool,
    /// The two groups `sparse_super2` keeps backups in.
    pub(crate) backup_bgs: Option<[u64; 2]>,
    pub(crate) filetype: bool,
    pub(crate) extents: bool,
    pub(crate) huge_file: bool,
    pub(crate) metadata_csum: bool,
    pub(crate) gdt_csum: bool,
    /// The crc32c seed every `metadata_csum` checksum starts from.
    pub(crate) csum_seed: u32,
    pub(crate) uuid: [u8; 16],
    pub(crate) dir_index: bool,
    pub(crate) hash_seed: [u32; 4],
    /// The hash a new index places names by.
    pub(crate) def_hash_version: u8,
    /// Whether indexed directories hash names as signed bytes; `None` when
    /// the volume records neither.
    pub(crate) hash_signed: Option<bool>,
    pub(crate) largedir: bool,
    /// Directories stop counting links past the format's limit.
    pub(crate) dir_nlink: bool,
    /// Small files may keep their contents in the inode.
    pub(crate) inline_data: bool,
    pub(crate) journal_inum: Option<u32>,
    pub(crate) orphan_file_inum: Option<u32>,
    /// Why writing is refused, when it is.
    pub(crate) no_write: Option<NoWrite>,
}

impl Layout {
    /// Descriptors one block holds.
    pub(crate) fn descs_per_block(&self) -> u64 {
        u64::from(self.block_size / self.desc_size)
    }

    /// Whether descriptors are wide enough to carry their high halves.
    pub(crate) fn wide_desc(&self) -> bool {
        self.desc_size >= 64
    }

    /// The first block of group `group`.
    pub(crate) fn group_first(&self, group: u64) -> u64 {
        self.first_data_block + group * u64::from(self.blocks_per_group)
    }

    /// Blocks group `group` covers: the last one may be short.
    pub(crate) fn blocks_in_group(&self, group: u64) -> u64 {
        let first = self.group_first(group);
        (self.blocks_count - first).min(u64::from(self.blocks_per_group))
    }

    /// The group block `block` belongs to.
    pub(crate) fn group_of(&self, block: u64) -> u64 {
        block.saturating_sub(self.first_data_block) / u64::from(self.blocks_per_group)
    }

    /// Whether group `group` holds a backup superblock.
    pub(crate) fn has_super(&self, group: u64) -> bool {
        if group == 0 {
            return true;
        }
        if let Some(backups) = self.backup_bgs {
            return backups.contains(&group);
        }
        !self.sparse_super || group == 1 || [3u64, 5, 7].iter().any(|&b| is_power_of(group, b))
    }

    /// The block holding group `group`'s descriptor, and the descriptor's
    /// byte offset within it.
    pub(crate) fn desc_location(&self, group: u64) -> (u64, usize) {
        let per = self.descs_per_block();
        let meta_group = group / per;
        let offset = low32(group % per) as usize * self.desc_size as usize;
        match self.first_meta_bg {
            Some(first) if meta_group >= first => {
                let leader = meta_group * per;
                let block = self.group_first(leader) + u64::from(self.has_super(leader));
                (block, offset)
            }
            _ => (self.gdt_block + meta_group, offset),
        }
    }

    /// Blocks at the start of group `group` that its superblock backup and
    /// descriptor tables occupy.
    pub(crate) fn group_overhead(&self, group: u64) -> u64 {
        let per = self.descs_per_block();
        let mut blocks = 0;
        if self.has_super(group) {
            blocks += 1;
            blocks += match self.first_meta_bg {
                Some(first) => first.min(self.gdt_blocks),
                None => self.gdt_blocks + self.reserved_gdt,
            };
        }
        if let Some(first) = self.first_meta_bg {
            let meta_group = group / per;
            // A meta-group's leader, the group after it and its last place
            // hold its descriptor block; a meta-group the volume ends inside
            // has no last place, and no third copy.
            if meta_group >= first {
                let at = group % per;
                if at == 0 || at == 1 || at == per - 1 {
                    blocks += 1;
                }
            }
        }
        blocks
    }
}

fn is_power_of(mut n: u64, base: u64) -> bool {
    while n > 1 && n.is_multiple_of(base) {
        n /= base;
    }
    n == 1
}

/// Parse and validate superblock `sb` for a device of `device_bytes`.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a structurally invalid superblock,
/// [`DriverError::Unsupported`] for a feature the read path does not
/// implement, [`DriverError::DeviceFault`] for a checksum that does not match.
pub(crate) fn parse(sb: &[u8], device_bytes: u64) -> Result<Layout, DriverError> {
    if sb.len() < SUPERBLOCK_LEN || le16(sb, SB_MAGIC) != EXT_MAGIC {
        return Err(DriverError::BadMagic);
    }
    let features = Features::read(sb)?;
    let geometry = Geometry::read(sb, device_bytes, &features)?;
    let Features {
        compat,
        incompat,
        ro_compat,
    } = features;
    let metadata_csum = ro_compat & RO_COMPAT_METADATA_CSUM != 0;
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&sb[SB_UUID..SB_UUID + 16]);
    let csum_seed = if incompat & INCOMPAT_CSUM_SEED != 0 {
        le32(sb, SB_CHECKSUM_SEED)
    } else {
        tairix_crc32c::update(0xFFFF_FFFF, &uuid)
    };
    let sb_flags = le32(sb, SB_FLAGS);
    let hash_signed = if sb_flags & FLAG_UNSIGNED_HASH != 0 {
        Some(false)
    } else if sb_flags & FLAG_SIGNED_HASH != 0 {
        Some(true)
    } else {
        None
    };
    let journal_inum = (compat & COMPAT_HAS_JOURNAL != 0).then(|| le32(sb, SB_JOURNAL_INUM));
    if journal_inum.is_some() && le32(sb, SB_JOURNAL_DEV) != 0 {
        // An external journal device: nothing here can replay or write it.
        return Err(DriverError::Unsupported);
    }
    let orphan_file_inum = (compat & COMPAT_ORPHAN_FILE != 0)
        .then(|| le32(sb, SB_ORPHAN_FILE_INUM))
        .filter(|&ino| ino != 0);
    Ok(Layout {
        no_write: features.write_refusal(sb, &geometry),
        block_size: geometry.block_size,
        cluster_size: geometry.cluster_size,
        blocks_count: geometry.blocks_count,
        inodes_count: geometry.inodes_count,
        inodes_per_group: geometry.inodes_per_group,
        blocks_per_group: geometry.blocks_per_group,
        inode_size: geometry.inode_size,
        desc_size: geometry.desc_size,
        group_count: geometry.group_count,
        first_data_block: geometry.first_data_block,
        first_ino: geometry.first_ino,
        sb_block: geometry.sb_block,
        sb_offset: geometry.sb_offset,
        gdt_block: geometry.sb_block + 1,
        gdt_blocks: geometry
            .group_count
            .div_ceil(u64::from(geometry.block_size / geometry.desc_size)),
        reserved_gdt: if compat & COMPAT_RESIZE_INODE != 0 {
            u64::from(le16(sb, SB_RESERVED_GDT))
        } else {
            0
        },
        first_meta_bg: (incompat & INCOMPAT_META_BG != 0)
            .then(|| u64::from(le32(sb, SB_FIRST_META_BG))),
        sparse_super: ro_compat & RO_COMPAT_SPARSE_SUPER != 0,
        backup_bgs: (compat & COMPAT_SPARSE_SUPER2 != 0).then(|| {
            [
                u64::from(le32(sb, SB_BACKUP_BGS)),
                u64::from(le32(sb, SB_BACKUP_BGS + 4)),
            ]
        }),
        filetype: incompat & INCOMPAT_FILETYPE != 0,
        extents: incompat & INCOMPAT_EXTENTS != 0,
        huge_file: ro_compat & RO_COMPAT_HUGE_FILE != 0,
        metadata_csum,
        gdt_csum: !metadata_csum && ro_compat & RO_COMPAT_GDT_CSUM != 0,
        csum_seed,
        uuid,
        dir_index: compat & COMPAT_DIR_INDEX != 0,
        hash_seed: core::array::from_fn(|word| le32(sb, SB_HASH_SEED + 4 * word)),
        def_hash_version: sb[SB_DEF_HASH_VERSION],
        hash_signed,
        largedir: incompat & INCOMPAT_LARGEDIR != 0,
        dir_nlink: ro_compat & RO_COMPAT_DIR_NLINK != 0,
        inline_data: incompat & INCOMPAT_INLINE_DATA != 0,
        journal_inum,
        orphan_file_inum,
    })
}

/// The three feature words, checked against what the read path implements.
#[derive(Copy, Clone)]
struct Features {
    compat: u32,
    incompat: u32,
    ro_compat: u32,
}

impl Features {
    /// Read the feature words, refusing a volume the read path cannot serve
    /// and one whose superblock fails its checksum.
    fn read(sb: &[u8]) -> Result<Self, DriverError> {
        let features = Self {
            compat: le32(sb, SB_FEATURE_COMPAT),
            incompat: le32(sb, SB_FEATURE_INCOMPAT),
            ro_compat: le32(sb, SB_FEATURE_RO_COMPAT),
        };
        if features.incompat & !READ_INCOMPAT != 0
            || features.incompat & (INCOMPAT_COMPRESSION | INCOMPAT_JOURNAL_DEV) != 0
        {
            return Err(DriverError::Unsupported);
        }
        if features.ro_compat & RO_COMPAT_METADATA_CSUM != 0 {
            if sb[SB_CHECKSUM_TYPE] != CHECKSUM_CRC32C {
                return Err(DriverError::Unsupported);
            }
            if le32(sb, SB_CHECKSUM) != checksum(sb) {
                return Err(DriverError::DeviceFault);
            }
        }
        Ok(features)
    }

    /// Why the volume is not written, if it is not.
    fn write_refusal(self, sb: &[u8], geometry: &Geometry) -> Option<NoWrite> {
        if self.incompat & !WRITE_INCOMPAT != 0
            || self.ro_compat & !WRITE_RO_COMPAT != 0
            || self.compat & COMPAT_EXCLUDE_BITMAP != 0
            || geometry.cluster_size != geometry.block_size
        {
            Some(NoWrite::Feature)
        } else if le16(sb, SB_STATE) & STATE_ERROR != 0 {
            Some(NoWrite::Errors)
        } else if le32(sb, SB_CREATOR_OS) == OS_HURD {
            Some(NoWrite::Hurd)
        } else {
            None
        }
    }
}

/// The geometry a superblock declares, checked against itself and the
/// device.
struct Geometry {
    block_size: u32,
    cluster_size: u32,
    blocks_count: u64,
    inodes_count: u32,
    inodes_per_group: u32,
    blocks_per_group: u32,
    inode_size: u32,
    desc_size: u32,
    group_count: u64,
    first_data_block: u64,
    first_ino: u32,
    sb_block: u64,
    sb_offset: usize,
}

impl Geometry {
    fn read(sb: &[u8], device_bytes: u64, features: &Features) -> Result<Self, DriverError> {
        let log_block = le32(sb, SB_LOG_BLOCK_SIZE);
        if log_block > 2 {
            return Err(DriverError::Unsupported);
        }
        let block_size = 1024u32 << log_block;
        // Without `bigalloc` a cluster is a block, whatever the field says.
        let cluster_size = if features.ro_compat & RO_COMPAT_BIGALLOC == 0 {
            block_size
        } else {
            let log_cluster = le32(sb, SB_LOG_CLUSTER_SIZE);
            if log_cluster < log_block || log_cluster - log_block > MAX_LOG_BLOCKS_PER_CLUSTER {
                return Err(DriverError::BadMagic);
            }
            1024u32 << log_cluster
        };
        let inodes_per_group = le32(sb, SB_INODES_PER_GROUP);
        let blocks_per_group = le32(sb, SB_BLOCKS_PER_GROUP);
        // A block bitmap counts clusters, which `bigalloc` makes larger than
        // a block; without it the two counts are one.
        let clusters_per_group = le32(sb, SB_CLUSTERS_PER_GROUP);
        let per_bitmap = 8 * block_size;
        if inodes_per_group == 0
            || clusters_per_group == 0
            || blocks_per_group == 0
            || inodes_per_group > per_bitmap
            || clusters_per_group > per_bitmap
            || clusters_per_group.checked_mul(cluster_size / block_size) != Some(blocks_per_group)
        {
            return Err(DriverError::BadMagic);
        }
        let (inode_size, first_ino) = if le32(sb, SB_REV_LEVEL) == 0 {
            (128, 11)
        } else {
            (u32::from(le16(sb, SB_INODE_SIZE)), le32(sb, SB_FIRST_INO))
        };
        if inode_size < 128 || !inode_size.is_power_of_two() || inode_size > block_size {
            return Err(DriverError::BadMagic);
        }
        let wide = features.incompat & INCOMPAT_64BIT != 0;
        let desc_size = if wide {
            u32::from(le16(sb, SB_DESC_SIZE))
        } else {
            32
        };
        let wide_fits = (MIN_WIDE_DESC..=MAX_DESC_SIZE).contains(&desc_size);
        if (wide && !wide_fits) || desc_size > block_size || !desc_size.is_power_of_two() {
            return Err(DriverError::BadMagic);
        }
        let hi = |off: usize| {
            if wide {
                u64::from(le32(sb, off)) << 32
            } else {
                0
            }
        };
        let blocks_count = u64::from(le32(sb, SB_BLOCKS_LO)) | hi(SB_BLOCKS_HI);
        let first_data_block = u64::from(le32(sb, SB_FIRST_DATA_BLOCK));
        let fits = blocks_count
            .checked_mul(u64::from(block_size))
            .is_some_and(|bytes| bytes <= device_bytes);
        if blocks_count <= first_data_block || !fits {
            return Err(DriverError::BadMagic);
        }
        let group_count = (blocks_count - first_data_block).div_ceil(u64::from(blocks_per_group));
        let inodes_count = le32(sb, SB_INODES_COUNT);
        if group_count.checked_mul(u64::from(inodes_per_group)) != Some(u64::from(inodes_count))
            || first_ino <= 2
            || first_ino > inodes_count
        {
            return Err(DriverError::BadMagic);
        }
        Ok(Self {
            block_size,
            cluster_size,
            blocks_count,
            inodes_count,
            inodes_per_group,
            blocks_per_group,
            inode_size,
            desc_size,
            group_count,
            first_data_block,
            first_ino,
            sb_block: SUPERBLOCK_OFFSET / u64::from(block_size),
            sb_offset: low32(SUPERBLOCK_OFFSET % u64::from(block_size)) as usize,
        })
    }
}

/// The superblock's free-block count, both halves on a 64-bit volume.
pub(crate) fn free_blocks(sb: &[u8], wide: bool) -> u64 {
    let hi = if wide {
        u64::from(le32(sb, SB_FREE_BLOCKS_HI)) << 32
    } else {
        0
    };
    u64::from(le32(sb, SB_FREE_BLOCKS_LO)) | hi
}

/// The superblock's reserved-block count.
pub(crate) fn reserved_blocks(sb: &[u8], wide: bool) -> u64 {
    let hi = if wide {
        u64::from(le32(sb, SB_R_BLOCKS_HI)) << 32
    } else {
        0
    };
    u64::from(le32(sb, SB_R_BLOCKS_LO)) | hi
}

/// Store `free` as the superblock's free-block count.
pub(crate) fn set_free_blocks(sb: &mut [u8], wide: bool, free: u64) -> Result<(), DriverError> {
    if !wide && free >> 32 != 0 {
        return Err(DriverError::DeviceFault);
    }
    crate::bytes::put_le32(sb, SB_FREE_BLOCKS_LO, low32(free));
    if wide {
        crate::bytes::put_le32(sb, SB_FREE_BLOCKS_HI, crate::bytes::high32(free));
    }
    Ok(())
}

/// Recompute `s_checksum` over a `metadata_csum` superblock.
pub(crate) fn stamp(sb: &mut [u8]) {
    let sum = checksum(sb);
    crate::bytes::put_le32(sb, SB_CHECKSUM, sum);
}

/// The `metadata_csum` checksum superblock `sb` should carry.
fn checksum(sb: &[u8]) -> u32 {
    tairix_crc32c::update(0xFFFF_FFFF, &sb[..SB_CHECKSUM])
}

#[cfg(test)]
#[path = "superblock_tests.rs"]
mod tests;
