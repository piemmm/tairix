//! The probe plan: which attachable volumes a block device carries
//! (`plans/DEVICES.md` D3c).
//!
//! The engine reads only what recognition needs — never whole volumes — and
//! is pure policy over the [`Block`] seam, so it is host-tested against
//! in-memory images.
//!
//! Each extent (the whole device, then each partition) is recognised by
//! [`tairix_fsprobe::recognise`]. A **RAID array member** is counted
//! ([`PlanSummary::raid_members`]) and skipped, never attached: it belongs to
//! an array awaiting assembly, one copy of a mirror mounted read-write
//! diverges the array, and a member that missed writes serves stale data.
//! Volumes are planned in a fixed order:
//!
//! 1. **A whole-device volume first.** A format whose structures open the
//!    device (`ARXFS`, ext4, FAT32, ADFS) leaves no room for a partition
//!    table, so the device is planned whole and the partition parse skipped:
//!    parsing a boot sector's code bytes as an MBR would fabricate extents.
//!    An optical disc — a device that reports a disc layout
//!    ([`Block::disc_layout`]) — is one volume too, whatever its system area
//!    holds.
//! 2. **A disc image on a partitioned device is weighed against its table.**
//!    ISO 9660 and UDF begin 32 KiB in, behind a system area where a hybrid
//!    image keeps a partition table of its own. The image is the device's
//!    volume when that table lays it out (`hybrid_layout`); its partitions
//!    inside it are then windows onto it and are not planned, while those
//!    after it — an appended boot partition — are. A table that contradicts
//!    the image was laid out over a stale one's remains, and wins.
//! 3. **Else the partition table** (`lib/partition`: GPT, then MBR,
//!    fail-closed), recognising each present partition. A partition
//!    recognised as nothing is skipped and counted — never guessed at. The
//!    declared partition *type* is a routing hint the probe deliberately
//!    ignores: the content decides.
//!
//! Each recognised volume yields a [`VolumePlan`] carrying its extent,
//! filesystem, stable identity, and what its catalog name is made from (its
//! label, read where its format keeps it, else the device's transport: the
//! [`crate::name`] policy), in stable device order. Collision handling
//! happens at attach time ([`crate::name::candidates`]); the plan itself is
//! one deterministic pass.

use tairix_abi::blkio::BlkTransport;
use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::{recognise, ProbedVolume, Recognised, SYSTEM_AREA_LEN};
use tairix_partition::{
    parse_partition_table, Partition, PartitionBlock, PartitionError, PartitionTable,
};

use crate::name::NameBase;

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
    /// Present partitions recognised as nothing.
    pub unrecognised: u32,
    /// Extents (the whole device, or a partition) recognised as a **RAID array
    /// member** and deliberately *not* attached: a member belongs to a RAID
    /// array awaiting assembly, and mounting one bare copy read-write would
    /// diverge a mirror's copies or serve stale data. Not an error, and never
    /// counted as blank/unrecognised.
    pub raid_members: u32,
    /// `true` when the device carried no partition table and no
    /// whole-device volume (nothing attachable, not an error).
    pub no_scheme: bool,
}

/// Probe `dev` and hand each recognised volume to `sink`, in stable
/// device order.
///
/// # Errors
///
/// [`DriverError`] only for a *device* fault (a failed read) or a refused
/// allocation; an unrecognised or absent layout is a normal
/// [`PlanSummary`] outcome, never an error.
pub fn plan_volumes<B: Block>(
    dev: &mut B,
    mut sink: impl FnMut(&VolumePlan),
) -> Result<PlanSummary, DriverError> {
    let geometry = dev.geometry()?;
    let transport = dev.medium().map(|medium| medium.transport);
    let disc = dev.disc_layout().is_some();
    let mut summary = PlanSummary::default();
    let whole = (0, geometry.block_count);
    let mut plan = |volume: &ProbedVolume, extent, summary: &mut PlanSummary| {
        sink(&plan_from(volume, extent, transport));
        summary.planned += 1;
    };

    let image = match recognise_extent(dev, whole)? {
        Some(Recognised::RaidMember(_)) => {
            summary.raid_members = 1;
            return Ok(summary);
        }
        Some(Recognised::Volume(volume)) => match volume.span {
            Some(span) if !disc => Some((volume, span)),
            _ => {
                plan(&volume, whole, &mut summary);
                return Ok(summary);
            }
        },
        None => None,
    };

    let table = match parse_partition_table(dev) {
        Ok(table) => table,
        Err(PartitionError::Device(err)) => return Err(err),
        // No scheme, or a malformed table: a disc image is the device's one
        // volume, and anything else leaves nothing attachable. Only a device
        // with no scheme at all is empty; a malformed table describes
        // something this build cannot read.
        Err(err) => {
            match image {
                Some((volume, _)) => plan(&volume, whole, &mut summary),
                None => summary.no_scheme = err.is_absent(),
            }
            return Ok(summary);
        }
    };
    let mut image_end = None;
    if let Some((volume, span)) = image {
        if hybrid_layout(dev, &geometry, &table, volume.fstype, span)? {
            plan(&volume, whole, &mut summary);
            image_end = Some(span.div_ceil(u64::from(geometry.block_size)));
        }
    }
    for partition in table.partitions() {
        let Some(extent) = bounded_extent(partition, &geometry) else {
            // An extent the geometry cannot hold is a lying table entry;
            // skip it fail-closed rather than read out of range.
            summary.unrecognised += 1;
            continue;
        };
        if image_end.is_some_and(|end| extent.0 < end) {
            continue;
        }
        match recognise_extent(dev, extent)? {
            Some(Recognised::RaidMember(_)) => summary.raid_members += 1,
            Some(Recognised::Volume(volume)) => plan(&volume, extent, &mut summary),
            None => summary.unrecognised += 1,
        }
    }
    Ok(summary)
}

/// Whether `table` is the disc image's own — what a hybrid image carries so
/// firmware that reads partitions can boot it — rather than one laid out over
/// a stale image's remains. It is when it lays the image out from its start
/// (a partition begins in the system area), none of its partitions that begin
/// later straddles the image's end, and no partition covering the image's
/// descriptors holds a filesystem of another kind.
fn hybrid_layout<B: Block>(
    dev: &mut B,
    geometry: &BlockGeometry,
    table: &PartitionTable,
    fstype: VolumeFsType,
    span: u64,
) -> Result<bool, DriverError> {
    let block_bytes = u64::from(geometry.block_size);
    let image_end = span.div_ceil(block_bytes);
    let descriptors = SYSTEM_AREA_LEN / block_bytes;
    let mut lays_out = false;
    for partition in table.partitions() {
        let Some((first, blocks)) = bounded_extent(partition, geometry) else {
            continue;
        };
        let end = first + blocks;
        if first > descriptors {
            if first < image_end && end > image_end {
                return Ok(false);
            }
            continue;
        }
        lays_out = true;
        if end > descriptors {
            match recognise_extent(dev, (first, blocks))? {
                Some(Recognised::Volume(inner)) if inner.fstype == fstype => {}
                Some(_) => return Ok(false),
                None => {}
            }
        }
    }
    Ok(lays_out)
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

/// What the extent (first block, block count) of `dev` holds. A device whose
/// geometry cannot be read through holds nothing recognisable.
fn recognise_extent<B: Block>(
    dev: &mut B,
    (first_lba, blocks): (u64, u64),
) -> Result<Option<Recognised>, DriverError> {
    let window = match PartitionBlock::new(&mut *dev, first_lba, blocks) {
        Ok(window) => window,
        Err(DriverError::LengthOutOfRange) => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut extent = match ByteDevice::new(window) {
        Ok(extent) => extent,
        Err(DriverError::Unsupported) => return Ok(None),
        Err(err) => return Err(err),
    };
    recognise(&mut extent)
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
#[path = "../../../../lib/fsprobe/tests/support/images.rs"]
mod images;

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use tairix_abi::driver::block::DiscLayout;
    use tairix_partition::{mbr, PartitionType};

    use super::*;
    use crate::plan::images;

    const BS: usize = 512;
    const BS_U32: u32 = 512;

    /// An in-memory block device over a byte image.
    struct MemBlock {
        image: Vec<u8>,
        faulty: bool,
        layout: Option<DiscLayout>,
    }

    impl Block for MemBlock {
        fn disc_layout(&self) -> Option<DiscLayout> {
            self.layout
        }

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
            layout: None,
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
            layout: None,
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
            layout: None,
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
            layout: None,
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
            layout: None,
        };
        let (plans, summary) = collect(&mut dev);
        assert!(plans.is_empty());
        assert_eq!(summary.planned, 0);
        assert!(summary.no_scheme);
    }

    #[test]
    fn a_malformed_table_is_not_an_empty_device() {
        // Two overlapping partitions: the table describes something this
        // build cannot read, so the device is never offered as empty.
        let mut image = vec![0u8; 64 * BS];
        for (index, start) in [8u32, 16].into_iter().enumerate() {
            let entry = 446 + index * 16;
            image[entry + 4] = 0x83;
            image[entry + 8..entry + 12].copy_from_slice(&start.to_le_bytes());
            image[entry + 12..entry + 16].copy_from_slice(&16u32.to_le_bytes());
        }
        image[510] = 0x55;
        image[511] = 0xAA;
        let (plans, summary) = collect(&mut device(image, None));
        assert!(plans.is_empty());
        assert!(!summary.no_scheme);
    }

    #[test]
    fn a_device_fault_is_an_error_not_an_empty_plan() {
        let mut dev = MemBlock {
            image: vec![0u8; 64 * BS],
            faulty: true,
            layout: None,
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
            layout: None,
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
            layout: None,
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
            layout: None,
        };
        let (plans, summary) = collect(&mut dev);
        assert!(plans.is_empty());
        assert_eq!(summary.raid_members, 1);
        assert_eq!(summary.planned, 0);
        assert_eq!(summary.unrecognised, 0);
    }

    /// A disc image of 42 sectors (168 blocks): its descriptors at sector 16,
    /// its roots at 40 and 41, and sectors 18..40 free for a partition a
    /// hybrid image keeps inside it.
    const IMAGE_BYTES: usize = 42 * 2048;
    const IMAGE_BLOCKS: u64 = (IMAGE_BYTES / BS) as u64;

    fn disc_image(blocks: usize) -> Vec<u8> {
        let mut image = images::iso::image(&images::iso::Spec {
            volume_id: b"INSTALLER",
            root_sector: 40,
            ..Default::default()
        });
        image.resize(blocks * BS, 0);
        image
    }

    /// `image` with an MBR of `parts`, each of type `0x83`.
    fn with_table(mut image: Vec<u8>, parts: &[Partition]) -> Vec<u8> {
        let mut sector = mbr::encode(parts).expect("encodes");
        for index in 0..parts.len() {
            set_type_byte(&mut sector, index, 0x83);
        }
        image[..512].copy_from_slice(&sector);
        image
    }

    fn part(start_lba: u64, block_count: u64) -> Partition {
        Partition {
            ty: PartitionType::FatBoot,
            start_lba,
            block_count,
        }
    }

    fn device(image: Vec<u8>, layout: Option<DiscLayout>) -> MemBlock {
        MemBlock {
            image,
            faulty: false,
            layout,
        }
    }

    #[test]
    fn a_disc_image_with_no_table_is_planned_whole() {
        let (plans, summary) = collect(&mut device(disc_image(256), None));
        assert_eq!(summary.planned, 1);
        assert_eq!(plans[0].fstype, VolumeFsType::Iso9660);
        assert_eq!((plans[0].first_lba, plans[0].blocks), (0, 256));
        assert_eq!(base_text(&plans[0]), b"installer");
    }

    #[test]
    fn a_hybrid_images_own_table_leaves_it_whole() {
        // Partition 1 begins at the image's descriptors, partition 2 is a
        // boot image inside it, and partition 3 runs to the image's end.
        let mut image = with_table(
            disc_image(256),
            &[part(64, 16), part(80, 40), part(120, IMAGE_BLOCKS - 120)],
        );
        write_fat32(&mut image, 80 * BS, *b"EFIB", b"EFI BOOT   ");
        let (plans, summary) = collect(&mut device(image, None));
        assert_eq!(plans.len(), 1, "the partitions are windows onto the image");
        assert_eq!(plans[0].fstype, VolumeFsType::Iso9660);
        assert_eq!((plans[0].first_lba, plans[0].blocks), (0, 256));
        assert_eq!(summary.unrecognised, 0);
    }

    #[test]
    fn a_classic_hybrid_whose_partition_covers_the_mbr_is_planned_whole() {
        // The table's one partition starts at block 0, which no MBR may.
        let mut image = disc_image(256);
        image[446] = 0x80;
        image[446 + 4] = 0x17;
        let blocks = u32::try_from(IMAGE_BLOCKS).expect("a small image");
        image[446 + 12..446 + 16].copy_from_slice(&blocks.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;
        let (plans, _) = collect(&mut device(image, None));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].fstype, VolumeFsType::Iso9660);
    }

    #[test]
    fn a_partition_appended_to_a_hybrid_image_is_planned_beside_it() {
        // Partition 1 starts at the image's descriptors and ends with it;
        // partition 2 follows the image.
        let mut image = with_table(
            disc_image(256),
            &[part(64, IMAGE_BLOCKS - 64), part(IMAGE_BLOCKS, 64)],
        );
        write_fat32(&mut image, IMAGE_BYTES, *b"APND", b"APPENDED   ");
        let (plans, _) = collect(&mut device(image, None));
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].fstype, VolumeFsType::Iso9660);
        assert_eq!(plans[1].fstype, VolumeFsType::Fat32);
        assert_eq!(plans[1].first_lba, IMAGE_BLOCKS);
    }

    #[test]
    fn a_table_laid_out_over_a_stale_image_wins() {
        // A partition begun after the system area runs past the image's end:
        // the table was laid out without regard to the image.
        let mut image = with_table(disc_image(256), &[part(100, 156)]);
        write_ext4(&mut image, 100 * BS, [7u8; 16], b"data");
        let (plans, _) = collect(&mut device(image, None));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].fstype, VolumeFsType::Ext4);
        assert_eq!(plans[0].first_lba, 100);
    }

    #[test]
    fn a_stale_image_under_an_early_partition_of_another_kind_loses() {
        // A cylinder-aligned partition covers the image's descriptors and
        // holds a filesystem of its own.
        let mut image = with_table(disc_image(256), &[part(63, 193)]);
        write_fat32(&mut image, 63 * BS, *b"OLDP", b"OLD STYLE  ");
        let (plans, _) = collect(&mut device(image, None));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].fstype, VolumeFsType::Fat32);
        assert_eq!(plans[0].first_lba, 63);
    }

    #[test]
    fn a_table_that_never_lays_the_image_out_is_not_its_own() {
        // Every partition begins after the image's descriptors.
        let mut image = with_table(disc_image(256), &[part(IMAGE_BLOCKS, 64)]);
        write_fat32(&mut image, IMAGE_BYTES, *b"LATE", b"LATE       ");
        let (plans, _) = collect(&mut device(image, None));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].fstype, VolumeFsType::Fat32);
    }

    #[test]
    fn an_optical_disc_is_one_volume_whatever_its_system_area_holds() {
        let mut image = with_table(disc_image(256), &[part(100, 156)]);
        write_ext4(&mut image, 100 * BS, [7u8; 16], b"data");
        let layout = DiscLayout {
            last_session_start: 0,
            last_recorded: None,
        };
        let (plans, _) = collect(&mut device(image, Some(layout)));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].fstype, VolumeFsType::Iso9660);
        assert_eq!((plans[0].first_lba, plans[0].blocks), (0, 256));
    }
}
