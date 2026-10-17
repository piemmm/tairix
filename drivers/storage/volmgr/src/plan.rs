//! The probe plan: which attachable volumes a block device carries
//! (`plans/DEVICES.md` D3c).
//!
//! The engine reads only device *heads* — never whole volumes — and is
//! pure policy over the [`Block`] seam, so it is host-tested against
//! in-memory images.
//!
//! At each extent (the whole device, then each partition) a **RAID array
//! member** is recognised first ([`tairix_fsprobe::probe_raid_member`]): a
//! member carries a RAID superblock at its block 0, belongs to an array
//! awaiting assembly, and must never be mounted as a bare filesystem — one copy
//! of a mirror mounted read-write diverges the array, and a member that missed
//! writes serves stale data. Such an extent is counted
//! ([`PlanSummary::raid_members`]) and skipped, never attached. Only when an
//! extent is *not* a member is it probed for a filesystem, in the fixed and
//! documented order:
//!
//! 1. **Whole-device filesystem first.** A supported signature at LBA 0
//!    (`tairix_fsprobe::probe`) means an unpartitioned "superfloppy"
//!    volume: the device is planned whole and the partition parse is
//!    skipped (a real partition table's LBA 0 never carries a valid
//!    filesystem head, and parsing a boot sector's code bytes as an MBR
//!    would fabricate extents).
//! 2. **Else the partition table** (`lib/partition`: GPT, then MBR,
//!    fail-closed), probing each present partition's head. A partition
//!    whose head matches no supported signature is skipped and counted —
//!    never guessed at. The declared partition *type* is a routing hint
//!    the probe deliberately ignores: the content signature decides.
//!
//! Each recognised volume yields a [`VolumePlan`] carrying its extent,
//! filesystem, stable identity, and what its catalog name is made from (its
//! label, read where its format keeps it — for FAT32 the root directory's
//! label entry, which a relabel rewrites — else the device's transport: the
//! [`crate::name`] policy), in stable device order. Collision handling
//! happens at attach time ([`crate::name::candidates`]); the plan itself is
//! one deterministic pass.

use tairix_abi::blkio::BlkTransport;
use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_fsprobe::{
    fat32_root_dir_byte, probe, probe_raid_member, ProbedVolume, FAT32_LABEL_SCAN_LEN,
    PROBE_HEAD_LEN,
};
use tairix_partition::{parse_partition_table, Partition, PartitionError};

use crate::name::NameBase;

/// Largest logical block the read buffer covers (matches the block-size
/// bound the blkio client enforces at connect). It also covers the whole
/// probe head and the FAT32 label scan, so one buffer serves every read.
const HEAD_BUF_LEN: usize = 4096;
const _: () = assert!(HEAD_BUF_LEN >= PROBE_HEAD_LEN && HEAD_BUF_LEN >= FAT32_LABEL_SCAN_LEN);

/// One attachable volume the probe recognised.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumePlan {
    /// First logical block of the volume's extent on the device.
    pub first_lba: u64,
    /// Extent length in logical blocks (never zero).
    pub blocks: u64,
    /// The filesystem the signature identified.
    pub fstype: VolumeFsType,
    /// The volume's stable identity, as the filesystem driver publishes it.
    pub identity: [u8; 16],
    /// What its catalog name is made from; the name itself is chosen at
    /// attach time.
    pub base: NameBase,
}

/// What the device-wide probe pass found.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PlanSummary {
    /// Volumes recognised and handed to the sink.
    pub planned: u32,
    /// Present partitions whose head matched no supported filesystem.
    pub unrecognised: u32,
    /// Extents (the whole device, or a partition) recognised as a **RAID array
    /// member** and deliberately *not* attached: a member belongs to a RAID
    /// array awaiting assembly, and mounting one bare copy read-write would
    /// diverge a mirror's copies or serve stale data. Not an error, and never
    /// counted as blank/unrecognised.
    pub raid_members: u32,
    /// `true` when the device carried no partition table and no
    /// whole-device filesystem (nothing attachable, not an error).
    pub no_scheme: bool,
}

/// Probe `dev` and hand each recognised volume to `sink`, in stable
/// device order.
///
/// # Errors
///
/// [`DriverError`] only for a *device* fault (a failed read); an
/// unrecognised or absent layout is a normal [`PlanSummary`] outcome,
/// never an error.
pub fn plan_volumes<B: Block>(
    dev: &mut B,
    mut sink: impl FnMut(&VolumePlan),
) -> Result<PlanSummary, DriverError> {
    let geometry = dev.geometry()?;
    let transport = dev.medium().map(|medium| medium.transport);
    let mut summary = PlanSummary::default();

    // Whole-device: a RAID array member first, then a whole-device
    // filesystem (see the module docs for the order and why).
    let whole = (0, geometry.block_count);
    if let Some(read) = read_extent(dev, &geometry, whole, 0, PROBE_HEAD_LEN)? {
        let head = read.bytes();
        if probe_raid_member(head).is_some() {
            // A member occupies the whole device: it belongs to a RAID array
            // awaiting assembly, so it is not attachable as a standalone
            // volume and is emphatically not blank. Fail closed rather than
            // ever mounting one bare copy.
            summary.raid_members = 1;
            return Ok(summary);
        }
        if let Some(probed) = probe(head) {
            let probed = with_directory_label(dev, &geometry, whole, head, probed)?;
            sink(&plan_from(&probed, whole, transport));
            summary.planned = 1;
            return Ok(summary);
        }
    }

    let table = match parse_partition_table(dev) {
        Ok(table) => table,
        Err(PartitionError::Device(err)) => return Err(err),
        // No scheme, or a malformed table: nothing attachable. The device
        // may be blank or foreign; refusing whole is the honest outcome.
        Err(_) => {
            summary.no_scheme = true;
            return Ok(summary);
        }
    };
    for partition in table.partitions() {
        let Some(extent) = bounded_extent(partition, &geometry) else {
            // An extent the geometry cannot hold is a lying table entry;
            // skip it fail-closed rather than read out of range.
            summary.unrecognised += 1;
            continue;
        };
        match read_extent(dev, &geometry, extent, 0, PROBE_HEAD_LEN)? {
            Some(read) => {
                let head = read.bytes();
                if probe_raid_member(head).is_some() {
                    // A partition that is a RAID array member is skipped for
                    // the same reason as a whole-device member above.
                    summary.raid_members += 1;
                } else if let Some(probed) = probe(head) {
                    let probed = with_directory_label(dev, &geometry, extent, head, probed)?;
                    sink(&plan_from(&probed, extent, transport));
                    summary.planned += 1;
                } else {
                    summary.unrecognised += 1;
                }
            }
            None => summary.unrecognised += 1,
        }
    }
    Ok(summary)
}

/// Clamp a partition's declared extent against the live geometry, or
/// `None` when it lies outside the device.
fn bounded_extent(partition: &Partition, geometry: &BlockGeometry) -> Option<(u64, u64)> {
    if partition.block_count == 0 {
        return None;
    }
    let end = partition.start_lba.checked_add(partition.block_count)?;
    if end > geometry.block_count {
        return None;
    }
    Some((partition.start_lba, partition.block_count))
}

/// Bytes read from an extent: whole blocks, of which [`Fetched::bytes`] are
/// the ones asked for.
struct Fetched {
    buf: [u8; HEAD_BUF_LEN],
    start: usize,
    end: usize,
}

impl Fetched {
    fn bytes(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }
}

/// Read up to `len` bytes of `extent` (first block, block count) from byte
/// `offset` of it, or `None` when the geometry cannot be used (a zero or
/// oversized block size, an empty extent) or `offset` lies outside the
/// extent.
///
/// One read serves both classifiers at a head: the caller passes the bytes
/// to [`probe_raid_member`] (is this a RAID array member?) and, failing that,
/// to [`probe`] (is this a supported filesystem?), so a head is read once.
fn read_extent<B: Block>(
    dev: &mut B,
    geometry: &BlockGeometry,
    (first_lba, blocks): (u64, u64),
    offset: u64,
    len: usize,
) -> Result<Option<Fetched>, DriverError> {
    let Ok(block_size) = usize::try_from(geometry.block_size) else {
        return Ok(None);
    };
    if block_size == 0 || block_size > HEAD_BUF_LEN || blocks == 0 {
        return Ok(None);
    }
    let block_bytes = u64::from(geometry.block_size);
    let skipped = offset / block_bytes;
    if skipped >= blocks {
        return Ok(None);
    }
    // Less than one block, so it fits `usize`.
    let skip = usize::try_from(offset % block_bytes).unwrap_or(0);
    let wanted = (skip + len)
        .div_ceil(block_size)
        .min(HEAD_BUF_LEN / block_size);
    let read_blocks = usize::try_from(blocks - skipped).map_or(wanted, |left| wanted.min(left));
    let read_bytes = read_blocks * block_size;
    let mut buf = [0u8; HEAD_BUF_LEN];
    dev.read_blocks(first_lba + skipped, &mut buf[..read_bytes])?;
    Ok(Some(Fetched {
        buf,
        start: skip,
        end: (skip + len).min(read_bytes),
    }))
}

/// `probed` labelled from its root directory when it is a FAT32 volume,
/// whose label entry there outranks the boot sector's copy. A root the
/// parameter block places outside the extent leaves the boot label standing.
fn with_directory_label<B: Block>(
    dev: &mut B,
    geometry: &BlockGeometry,
    extent: (u64, u64),
    head: &[u8],
    probed: ProbedVolume,
) -> Result<ProbedVolume, DriverError> {
    if probed.fstype != VolumeFsType::Fat32 {
        return Ok(probed);
    }
    let Some(root) = fat32_root_dir_byte(head) else {
        return Ok(probed);
    };
    Ok(
        match read_extent(dev, geometry, extent, root, FAT32_LABEL_SCAN_LEN)? {
            Some(entries) => probed.with_fat32_directory_label(entries.bytes()),
            None => probed,
        },
    )
}

/// Build the plan record for one recognised volume on a device reached over
/// `transport`.
fn plan_from(
    probed: &ProbedVolume,
    (first_lba, blocks): (u64, u64),
    transport: Option<BlkTransport>,
) -> VolumePlan {
    VolumePlan {
        first_lba,
        blocks,
        fstype: probed.fstype,
        identity: probed.identity,
        base: NameBase::of(probed.label(), transport),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use tairix_partition::{mbr, PartitionType};

    use super::*;

    const BS: usize = 512;
    const BS_U32: u32 = 512;

    /// An in-memory block device over a byte image.
    struct MemBlock {
        image: Vec<u8>,
        faulty: bool,
    }

    impl Block for MemBlock {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            Ok(BlockGeometry {
                block_size: BS_U32,
                block_count: (self.image.len() / BS) as u64,
            })
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            if self.faulty {
                return Err(DriverError::DeviceFault);
            }
            let start = usize::try_from(lba).map_err(|_| DriverError::OutOfRange)? * BS;
            let end = start
                .checked_add(buf.len())
                .ok_or(DriverError::OutOfRange)?;
            let src = self.image.get(start..end).ok_or(DriverError::OutOfRange)?;
            buf.copy_from_slice(src);
            Ok(())
        }

        fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
            Err(DriverError::Unsupported)
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    /// A structurally valid FAT32 head written at byte `offset` of `image`.
    fn write_fat32(image: &mut [u8], offset: usize, serial: [u8; 4], label11: &[u8; 11]) {
        let boot = &mut image[offset..offset + 512];
        boot[11..13].copy_from_slice(&512u16.to_le_bytes());
        boot[13] = 8;
        boot[14..16].copy_from_slice(&32u16.to_le_bytes());
        boot[16] = 2;
        boot[36..40].copy_from_slice(&1024u32.to_le_bytes());
        boot[44..48].copy_from_slice(&2u32.to_le_bytes());
        boot[66] = 0x29;
        boot[67..71].copy_from_slice(&serial);
        boot[71..82].copy_from_slice(label11);
        boot[82..90].copy_from_slice(b"FAT32   ");
        boot[510] = 0x55;
        boot[511] = 0xAA;
    }

    /// A structurally valid ext4 superblock written at partition byte
    /// `offset` of `image` (the superblock sits 1024 bytes in).
    fn write_ext4(image: &mut [u8], offset: usize, uuid: [u8; 16], label: &[u8]) {
        let sb = &mut image[offset + 1024..offset + 2048];
        sb[0..4].copy_from_slice(&8192u32.to_le_bytes());
        sb[4..8].copy_from_slice(&32768u32.to_le_bytes());
        sb[0x18..0x1C].copy_from_slice(&2u32.to_le_bytes());
        sb[0x38..0x3A].copy_from_slice(&tairix_fsprobe::EXT4_SUPERBLOCK_MAGIC.to_le_bytes());
        sb[0x68..0x78].copy_from_slice(&uuid);
        sb[0x78..0x78 + label.len()].copy_from_slice(label);
    }

    /// Rewrite entry `index`'s MBR type byte, so a test can author the
    /// foreign (e.g. Linux `0x83`) partitions `mbr::encode` deliberately
    /// refuses to spell.
    fn set_type_byte(sector: &mut [u8; 512], index: usize, byte: u8) {
        sector[446 + index * 16 + 4] = byte;
    }

    fn collect(dev: &mut MemBlock) -> (Vec<VolumePlan>, PlanSummary) {
        let mut plans = Vec::new();
        let summary = plan_volumes(dev, |plan| plans.push(*plan)).expect("plans");
        (plans, summary)
    }

    #[test]
    fn a_superfloppy_is_planned_whole() {
        let mut image = vec![0u8; 64 * BS];
        write_fat32(&mut image, 0, *b"SRLN", b"HOLIDAY PIX");
        let mut dev = MemBlock {
            image,
            faulty: false,
        };
        let (plans, summary) = collect(&mut dev);
        assert_eq!(summary.planned, 1);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].first_lba, 0);
        assert_eq!(plans[0].blocks, 64);
        assert_eq!(plans[0].fstype, VolumeFsType::Fat32);
        assert_eq!(base_text(&plans[0]), b"holiday-pix");
    }

    /// The text a plan's name is made from: its label, or the transport its
    /// unlabelled volume is named after.
    fn base_text(plan: &VolumePlan) -> &[u8] {
        match &plan.base {
            NameBase::Label(label) => label.as_bytes(),
            NameBase::Unlabelled(_) => b"<unlabelled>",
        }
    }

    #[test]
    fn a_fat32_label_is_read_from_its_root_directory() {
        let mut image = vec![0u8; 64 * BS];
        write_fat32(&mut image, 0, *b"SRLN", b"NO NAME    ");
        // Two reserved sectors and two one-sector FATs: the root directory,
        // cluster 2 of one sector, is sector 4.
        image[13] = 1;
        image[14..16].copy_from_slice(&2u16.to_le_bytes());
        image[36..40].copy_from_slice(&1u32.to_le_bytes());
        let root = 4 * BS;
        image[root..root + 11].copy_from_slice(b"CAMERA CARD");
        image[root + 11] = tairix_fsprobe::FAT_ATTR_VOLUME_ID;
        let mut dev = MemBlock {
            image,
            faulty: false,
        };
        let (plans, _) = collect(&mut dev);
        assert_eq!(base_text(&plans[0]), b"camera-card");
    }

    #[test]
    fn partitions_are_probed_by_content_with_fallback_names() {
        // MBR: partition 1 = FAT32 with a blank label (fallback name),
        // partition 2 = ext4 with a label, partition 3 = unrecognised.
        let mut image = vec![0u8; 256 * BS];
        let parts = [
            Partition {
                ty: PartitionType::FatBoot,
                start_lba: 8,
                block_count: 64,
            },
            Partition {
                ty: PartitionType::FatBoot,
                start_lba: 80,
                block_count: 64,
            },
            Partition {
                ty: PartitionType::FatBoot,
                start_lba: 160,
                block_count: 32,
            },
        ];
        let mut sector = mbr::encode(&parts).expect("encodes");
        // Partitions 2 and 3 are foreign (Linux) — the declared type is a
        // hint the probe ignores; the content decides.
        set_type_byte(&mut sector, 1, 0x83);
        set_type_byte(&mut sector, 2, 0x83);
        image[..512].copy_from_slice(&sector);
        write_fat32(&mut image, 8 * BS, [1, 2, 3, 4], b"           ");
        write_ext4(&mut image, 80 * BS, [7u8; 16], b"backup");
        let mut dev = MemBlock {
            image,
            faulty: false,
        };

        let (plans, summary) = collect(&mut dev);
        assert_eq!(summary.planned, 2);
        assert_eq!(summary.unrecognised, 1);
        assert!(!summary.no_scheme);

        assert_eq!(plans[0].first_lba, 8);
        assert_eq!(plans[0].fstype, VolumeFsType::Fat32);
        assert_eq!(
            plans[0].base,
            NameBase::Unlabelled(None),
            "a blank label names the volume by its device, never its format"
        );
        assert_eq!(plans[1].first_lba, 80);
        assert_eq!(plans[1].fstype, VolumeFsType::Ext4);
        assert_eq!(base_text(&plans[1]), b"backup");
        assert_eq!(plans[1].identity, [7u8; 16]);
    }

    #[test]
    fn a_lying_extent_is_skipped_without_reading_past_the_device() {
        // The table declares a partition reaching past the device end.
        let mut image = vec![0u8; 64 * BS];
        let parts = [Partition {
            ty: PartitionType::FatBoot,
            start_lba: 32,
            block_count: 1024,
        }];
        let mut sector = mbr::encode(&parts).expect("encodes");
        set_type_byte(&mut sector, 0, 0x83);
        image[..512].copy_from_slice(&sector);
        let mut dev = MemBlock {
            image,
            faulty: false,
        };
        let (plans, summary) = collect(&mut dev);
        assert!(plans.is_empty());
        assert_eq!(summary.unrecognised, 1);
    }

    #[test]
    fn a_blank_device_plans_nothing() {
        let mut dev = MemBlock {
            image: vec![0u8; 64 * BS],
            faulty: false,
        };
        let (plans, summary) = collect(&mut dev);
        assert!(plans.is_empty());
        assert_eq!(summary.planned, 0);
        assert!(summary.no_scheme);
    }

    #[test]
    fn a_device_fault_is_an_error_not_an_empty_plan() {
        let mut dev = MemBlock {
            image: vec![0u8; 64 * BS],
            faulty: true,
        };
        assert_eq!(
            plan_volumes(&mut dev, |_| {}),
            Err(DriverError::DeviceFault)
        );
    }

    #[test]
    fn two_unlabelled_volumes_of_one_type_are_named_apart_by_identity() {
        let mut image = vec![0u8; 256 * BS];
        let parts = [
            Partition {
                ty: PartitionType::FatBoot,
                start_lba: 8,
                block_count: 64,
            },
            Partition {
                ty: PartitionType::FatBoot,
                start_lba: 80,
                block_count: 64,
            },
        ];
        let mut sector = mbr::encode(&parts).expect("encodes");
        set_type_byte(&mut sector, 0, 0x83);
        set_type_byte(&mut sector, 1, 0x83);
        image[..512].copy_from_slice(&sector);
        write_fat32(&mut image, 8 * BS, [1, 1, 1, 1], b"           ");
        write_fat32(&mut image, 80 * BS, [2, 2, 2, 2], b"           ");
        let mut dev = MemBlock {
            image,
            faulty: false,
        };
        let (plans, _) = collect(&mut dev);
        let first = crate::name::candidates(plans[0].base, plans[0].identity).next();
        let second = crate::name::candidates(plans[1].base, plans[1].identity).next();
        assert_ne!(first, second);
    }

    /// Write a valid RAID array-member superblock (a RAID1 mirror, member 0 of
    /// 2) into the first bytes of the extent at byte `offset`, exactly as a
    /// member carries it at its block 0.
    fn write_raid_member(image: &mut [u8], offset: usize, uuid: [u8; 16]) {
        let superblock = tairix_raidmeta::ArraySuperblock {
            array_uuid: uuid,
            raid_level: tairix_abi::raid::RaidLevel::Mirror,
            member_count: 2,
            member_slot: 0,
            geometry: BlockGeometry {
                block_size: BS_U32,
                block_count: 64,
            },
            generation: 5,
            updated_at: tairix_abi::time::Time64::from_secs(1_700_000_000),
            chunk_blocks: 0,
        };
        let encoded = superblock.encode();
        image[offset..offset + encoded.len()].copy_from_slice(&encoded);
    }

    #[test]
    fn a_whole_device_raid_member_is_recognised_and_never_attached() {
        // The device's block 0 is a RAID member superblock, not a partition
        // table or a filesystem. It must be recognised and skipped — mounting
        // one bare copy of an array would diverge the array or serve stale
        // data — never mistaken for a blank device.
        let mut image = vec![0u8; 64 * BS];
        write_raid_member(&mut image, 0, [0x5A; 16]);
        let mut dev = MemBlock {
            image,
            faulty: false,
        };
        let (plans, summary) = collect(&mut dev);
        assert!(plans.is_empty(), "a bare RAID member is never attached");
        assert_eq!(summary.raid_members, 1);
        assert_eq!(summary.planned, 0);
        assert_eq!(summary.unrecognised, 0);
        assert!(!summary.no_scheme, "a member is not a blank device");
    }

    #[test]
    fn a_partition_that_is_a_raid_member_is_recognised_and_never_attached() {
        // A partitioned disk whose one partition is a RAID member: the table
        // parses, but the member partition is skipped rather than attached.
        let mut image = vec![0u8; 64 * BS];
        let parts = [Partition {
            ty: PartitionType::FatBoot,
            start_lba: 8,
            block_count: 32,
        }];
        let mut sector = mbr::encode(&parts).expect("encodes");
        set_type_byte(&mut sector, 0, 0x83);
        image[..512].copy_from_slice(&sector);
        write_raid_member(&mut image, 8 * BS, [0xC3; 16]);
        let mut dev = MemBlock {
            image,
            faulty: false,
        };
        let (plans, summary) = collect(&mut dev);
        assert!(plans.is_empty());
        assert_eq!(summary.raid_members, 1);
        assert_eq!(summary.planned, 0);
        assert_eq!(summary.unrecognised, 0);
    }
}
