//! Image builders and an in-memory device the recognition tests and the fuzz
//! harness share: structurally valid heads of every format the probe
//! recognises, to recognise as they are and to mutate.

// Test images are a few hundred KiB, so every offset and length fits the
// narrower on-disk field it is written to; and each test crate that includes
// this uses the builders its own tests need.
#![allow(clippy::cast_possible_truncation, dead_code)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry, DiscLayout};
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fat::boot::{Bpb, Kind};
use tairix_fat::dir::{ShortEntry, ENTRY};
use tairix_fat::format;
use tairix_fat::stamp::Stamps;
use tairix_fsprobe::{adfs, affs, ARXFS_HEADER_MAGIC, EXT4_SUPERBLOCK_MAGIC};

/// An in-memory device over a byte image, counting the transfers it serves.
pub struct Image {
    pub block_size: u32,
    pub bytes: Vec<u8>,
    pub layout: Option<DiscLayout>,
    pub reads: usize,
}

impl Block for Image {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: (self.bytes.len() / self.block_size as usize) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.reads += 1;
        let at = lba as usize * self.block_size as usize;
        let src = self
            .bytes
            .get(at..at + buf.len())
            .ok_or(DriverError::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn disc_layout(&self) -> Option<DiscLayout> {
        self.layout
    }
}

/// `bytes`, padded to whole blocks of `block_size`, as a device.
pub fn device(
    mut bytes: Vec<u8>,
    block_size: u32,
    layout: Option<DiscLayout>,
) -> ByteDevice<Image> {
    let bs = block_size as usize;
    bytes.resize(bytes.len().div_ceil(bs).max(1) * bs, 0);
    ByteDevice::new(Image {
        block_size,
        bytes,
        layout,
        reads: 0,
    })
    .expect("valid geometry")
}

/// Bytes of the head the probe reads.
pub const HEAD: usize = 2048;

/// The sectors `fat32_boot`'s volume says it has: room past its FATs for
/// the clusters the tests move its root to, and for a FAT twice the size.
pub const FAT32_SECTORS: u32 = 32 + 2 * 2048 + 8 * 64;

/// A FAT32 boot sector with the given label bytes (11, space-padded) and
/// serial, written by the format's own encoder: eight-sector clusters, 32
/// reserved sectors and two 1024-sector FATs, and no `FSInfo`.
pub fn fat32_boot(serial: [u8; 4], label11: &[u8; 11]) -> Vec<u8> {
    let bpb = Bpb {
        kind: Kind::Fat32,
        bytes_per_sector: 512,
        sectors_per_cluster: 8,
        reserved_sectors: 32,
        fats: 2,
        root_entries: 0,
        total_sectors: FAT32_SECTORS,
        media: 0xF8,
        fat_sectors: 1024,
        sectors_per_track: 0,
        heads: 0,
        hidden_sectors: 0,
        root_cluster: 2,
        fsinfo_sector: 0,
        backup_boot_sector: 0,
        drive: 0x80,
        serial: Some(u32::from_le_bytes(serial)),
        label: Some(*label11),
    };
    let mut head = vec![0u8; HEAD];
    head[..512].copy_from_slice(&format::boot_sector(&bpb, b"MSWIN4.1", &[]));
    head
}

/// Where `fat32_boot`'s root directory lies: 32 reserved sectors and two
/// 1024-sector FATs of 512 bytes.
pub const FAT32_ROOT: usize = (32 + 2 * 1024) * 512;

/// One 32-byte FAT directory entry named `name` with `attributes`.
pub fn dir_entry(name: &[u8; 11], attributes: u8) -> [u8; ENTRY] {
    ShortEntry {
        name: *name,
        attributes,
        case: 0,
        stamps: Stamps::NONE,
        cluster: 0,
        size: 0,
    }
    .encode()
}

/// `fat32_boot` with `entries` at the head of the directory at `root`.
pub fn fat32_with_root(label11: &[u8; 11], root: usize, entries: &[[u8; ENTRY]]) -> Vec<u8> {
    let mut image = fat32_boot(*b"SRLN", label11);
    image.resize(root + 4096, 0);
    for (slot, entry) in image[root..]
        .as_chunks_mut::<ENTRY>()
        .0
        .iter_mut()
        .zip(entries)
    {
        slot.copy_from_slice(entry);
    }
    image
}

/// A structurally valid ext4 superblock head with the given UUID and label.
pub fn ext4_head(uuid: [u8; 16], label: &[u8]) -> Vec<u8> {
    let mut head = vec![0u8; HEAD];
    let sb = 1024;
    head[sb..sb + 4].copy_from_slice(&8192u32.to_le_bytes()); // inodes
    head[sb + 4..sb + 8].copy_from_slice(&32768u32.to_le_bytes()); // blocks
    head[sb + 0x18..sb + 0x1C].copy_from_slice(&2u32.to_le_bytes()); // 4 KiB
    head[sb + 0x38..sb + 0x3A].copy_from_slice(&EXT4_SUPERBLOCK_MAGIC.to_le_bytes());
    head[sb + 0x68..sb + 0x78].copy_from_slice(&uuid);
    head[sb + 0x78..sb + 0x78 + label.len()].copy_from_slice(label);
    head
}

/// A `ARXFS` superblock-slot head with the given volume UUID.
pub fn arxfs_head(uuid: [u8; 16]) -> Vec<u8> {
    let mut head = vec![0u8; HEAD];
    head[..8].copy_from_slice(&ARXFS_HEADER_MAGIC.to_le_bytes());
    head[8..12].copy_from_slice(&1u32.to_le_bytes());
    head[16..32].copy_from_slice(&uuid);
    head
}

/// A RAID array-member head: a valid mirror superblock, member 0 of 2.
pub fn raid_member_head(uuid: [u8; 16]) -> Vec<u8> {
    let superblock = tairix_raidmeta::ArraySuperblock {
        array_uuid: uuid,
        raid_level: tairix_abi::raid::RaidLevel::Mirror,
        member_count: 2,
        member_slot: 0,
        geometry: BlockGeometry {
            block_size: 512,
            block_count: 4096,
        },
        generation: 7,
        updated_at: tairix_abi::time::Time64::from_secs(1_700_000_000),
        chunk_blocks: 0,
    };
    let mut head = vec![0u8; HEAD];
    let encoded = superblock.encode();
    head[..encoded.len()].copy_from_slice(&encoded);
    head
}

/// An old-map ADFS (L format) image named `name`.
pub fn adfs_old_map(name: &[u8; 10]) -> Vec<u8> {
    const NAME_AT: [usize; 10] = [
        0x0F7, 0x1F6, 0x0F8, 0x1F7, 0x0F9, 0x1F8, 0x0FA, 0x1F9, 0x0FB, 0x1FA,
    ];
    let mut image = vec![0u8; 64 * 1024];
    let sectors = (image.len() / 256) as u32;
    image[0xFC..0xFF].copy_from_slice(&sectors.to_le_bytes()[..3]);
    for (&at, &byte) in NAME_AT.iter().zip(name) {
        image[at] = byte;
    }
    image[255] = adfs::old_map_checksum(&image[..256]);
    image[511] = adfs::old_map_checksum(&image[256..512]);
    image[512 + 1..512 + 5].copy_from_slice(b"Hugo");
    image
}

/// The F format's disc record: 1600 KiB of 1024-byte sectors in four zones.
pub fn adfs_f_record() -> adfs::DiscRecord {
    let mut record = adfs::DiscRecord::parse(&[0; adfs::DISC_RECORD_SIZE]);
    record.log2secsize = 10;
    record.secspertrack = 10;
    record.heads = 2;
    record.density = 4;
    record.idlen = 15;
    record.log2bpmb = 6;
    record.nzones = 4;
    record.zone_spare = 0x640;
    record.root = 0x200;
    record.disc_size = 1600 * 1024;
    record.disc_name = *b"FloppyF   ";
    record
}

/// An image whose sealed boot block carries `record`, and nothing else.
pub fn adfs_boot_block(record: &adfs::DiscRecord) -> Vec<u8> {
    let at = adfs::BOOT_BLOCK_OFFSET as usize;
    let mut image = vec![0u8; at + adfs::BOOT_BLOCK_SIZE];
    let mut block = [0u8; adfs::BOOT_BLOCK_SIZE];
    adfs::seal_boot_block(&mut block, record);
    image[at..].copy_from_slice(&block);
    image
}

/// Seal again the boot block `image` carries, where it reaches that far.
pub fn seal_adfs_boot_block(image: &mut [u8]) {
    let at = adfs::BOOT_BLOCK_OFFSET as usize;
    if let Some(block) = image
        .get_mut(at..at + adfs::BOOT_BLOCK_SIZE)
        .and_then(|block| <&mut [u8; adfs::BOOT_BLOCK_SIZE]>::try_from(block).ok())
    {
        block[adfs::BOOT_BLOCK_SIZE - 1] = adfs::boot_block_checksum(block);
    }
}

/// An AmigaDOS volume of `blocks` blocks of `block_size` bytes: a `DOS`
/// boot block of type `flags` and a root block named `name` at the volume's
/// middle.
pub fn affs_volume(flags: u8, block_size: usize, blocks: u64, name: &[u8]) -> Vec<u8> {
    let mut image = vec![0u8; blocks as usize * block_size];
    image[..3].copy_from_slice(b"DOS");
    image[3] = flags;
    let root = affs::root_block(blocks);
    image[8..12].copy_from_slice(&(root as u32).to_be_bytes());
    let at = root as usize * block_size;
    write_affs_root(&mut image[at..at + block_size], name);
    image
}

/// Lay a root block named `name` out in `block`, its checksum sealed.
pub fn write_affs_root(block: &mut [u8], name: &[u8]) {
    let offsets = affs::Offsets::new(block.len());
    block.fill(0);
    affs::put_be32(block, 0, affs::T_HEADER);
    affs::put_be32(block, 12, offsets.table_len() as u32);
    affs::put_be32(block, offsets.bm_flag(), affs::BITMAP_VALID);
    block[offsets.name()] = name.len() as u8;
    block[offsets.name() + 1..offsets.name() + 1 + name.len()].copy_from_slice(name);
    affs::DateStamp {
        days: 9000,
        mins: 600,
        ticks: 25,
    }
    .write(block, offsets.created());
    affs::put_be32(block, offsets.sec_type(), affs::ST_ROOT);
    affs::seal(block, affs::CHECKSUM);
}

pub mod iso {
    use super::{vec, Vec};
    use tairix_test_iso9660_image::{
        archimedes, rr, terminator, typed_load, ucs2, Descriptor, Rec, Standard, BLOCK, DIRECTORY,
    };

    /// What goes into an image.
    #[derive(Default)]
    pub struct Spec<'a> {
        pub volume_id: &'a [u8],
        pub joliet_id: Option<&'a str>,
        pub rock_ridge: bool,
        pub archimedes: bool,
        pub high_sierra: bool,
        /// Logical sectors before the descriptor set's session.
        pub session: usize,
        /// Where the primary root goes, from the session's start, the Joliet
        /// root after it: sector 19 when zero.
        pub root_sector: usize,
    }

    /// The image `spec` describes: the descriptor set at sector 16 of its
    /// session, then the primary root and the Joliet root, each holding one
    /// file.
    pub fn image(spec: &Spec<'_>) -> Vec<u8> {
        let standard = if spec.high_sierra {
            Standard::HighSierra
        } else {
            Standard::Iso9660
        };
        let base = spec.session;
        let root = if spec.root_sector == 0 {
            19
        } else {
            spec.root_sector
        };
        let sectors = base + root + 2;
        let mut image = vec![0u8; sectors * BLOCK];
        let at = |sector: usize| (base + sector) * BLOCK;
        let primary = Descriptor {
            standard,
            kind: 1,
            location: (base + 16) as u32,
            volume_id: spec.volume_id,
            volume_blocks: sectors as u32,
            root: ((base + root) as u32, BLOCK as u32),
            escapes: None,
        };
        image[at(16)..at(17)].copy_from_slice(&primary.bytes());
        let mut next = 17;
        if let Some(joliet) = spec.joliet_id.filter(|_| !spec.high_sierra) {
            let id = ucs2(joliet);
            let supplementary = Descriptor {
                kind: 2,
                location: (base + next) as u32,
                volume_id: &id,
                root: ((base + root + 1) as u32, BLOCK as u32),
                escapes: Some(b"%/E"),
                ..primary
            };
            image[at(next)..at(next + 1)].copy_from_slice(&supplementary.bytes());
            next += 1;
        }
        image[at(next)..at(next + 1)].copy_from_slice(&terminator(standard, (base + next) as u32));
        let mut dot_use = Vec::new();
        if spec.rock_ridge {
            dot_use = rr::root(0);
            dot_use.extend(rr::px(0o040_755, 2, 0, 0));
        }
        let file_use = if spec.archimedes {
            archimedes(typed_load(0xFFF), 0, 0x13)
        } else {
            Vec::new()
        };
        for sector in [root, root + 1] {
            let extent = (base + sector) as u32;
            let records = [
                Rec {
                    system_use: &dot_use,
                    standard,
                    ..Rec::new(extent, BLOCK as u32, DIRECTORY, &[0])
                },
                Rec {
                    standard,
                    ..Rec::new(extent, BLOCK as u32, DIRECTORY, &[1])
                },
                Rec {
                    system_use: &file_use,
                    standard,
                    ..Rec::new(0, 0, 0, b"README.TXT;1")
                },
            ];
            let dir: Vec<u8> = records.iter().flat_map(Rec::bytes).collect();
            image[at(sector)..at(sector) + dir.len()].copy_from_slice(&dir);
        }
        image
    }
}

/// UDF test images.
pub mod udf_image {
    use super::{vec, Vec};
    use tairix_fsprobe::udf::{
        Tag, DOMAIN_ID, TAG_AVDP, TAG_LVD, TAG_LVID, TAG_PD, TAG_PVD, TAG_TD,
    };

    pub struct Spec<'a> {
        pub block_size: usize,
        pub blocks: usize,
        pub lvid: &'a str,
        pub volume_set: &'a [u8],
        pub domain: &'a [u8],
        /// Where the ISO 9660 descriptors of a bridge disc go, before the
        /// UDF recognition sequence.
        pub iso_first: bool,
    }

    impl Default for Spec<'_> {
        fn default() -> Self {
            Self {
                block_size: 2048,
                blocks: 600,
                lvid: "Archive",
                volume_set: b"0123456789ABCDEFvolume",
                domain: DOMAIN_ID,
                iso_first: false,
            }
        }
    }

    /// Where the main sequence, its reserve and the integrity sequence lie
    /// in `block_size`-byte sectors: past the recognition sequence, before
    /// the anchor at 256.
    pub fn places(block_size: usize) -> (u32, u32, u32) {
        let main = (48 * 1024 / block_size) as u32;
        (main, main + 16, main + 32)
    }

    pub const PARTITION_START: u32 = 257;

    fn dstring(field: &mut [u8], text: &str) {
        let last = field.len() - 1;
        field[0] = 8;
        let bytes = text.as_bytes();
        field[1..=bytes.len()].copy_from_slice(bytes);
        field[last] = (bytes.len() + 1) as u8;
    }

    fn seal(sector: &mut [u8], id: u16, location: u32, crc_len: usize) {
        Tag {
            id,
            version: 3,
            serial: 1,
            location,
        }
        .seal(sector, crc_len)
        .expect("fits");
    }

    /// Write a descriptor sequence at `start`, its LVD of sequence number
    /// `lvd_vdsn` naming the logical volume `identifier`.
    pub fn sequence(
        image: &mut [u8],
        spec: &Spec<'_>,
        start: u32,
        lvd_vdsn: u32,
        identifier: &str,
    ) {
        let bs = spec.block_size;
        let at = |sector: u32| sector as usize * bs;
        let mut pvd = vec![0u8; bs];
        pvd[16..20].copy_from_slice(&0u32.to_le_bytes());
        dstring(&mut pvd[24..56], "VOLUME");
        pvd[72..72 + spec.volume_set.len()].copy_from_slice(spec.volume_set);
        pvd[376..388].copy_from_slice(&[0x10, 0x10, 0xE5, 0x07, 1, 2, 3, 4, 5, 0, 0, 0]);
        seal(&mut pvd, TAG_PVD, start, 496);
        image[at(start)..at(start) + bs].copy_from_slice(&pvd);

        let mut pd = vec![0u8; bs];
        pd[16..20].copy_from_slice(&1u32.to_le_bytes());
        pd[20..22].copy_from_slice(&1u16.to_le_bytes());
        pd[22..24].copy_from_slice(&0u16.to_le_bytes());
        pd[25..31].copy_from_slice(b"+NSR03");
        pd[184..188].copy_from_slice(&4u32.to_le_bytes());
        pd[188..192].copy_from_slice(&PARTITION_START.to_le_bytes());
        let length = (spec.blocks as u32) - PARTITION_START - 1;
        pd[192..196].copy_from_slice(&length.to_le_bytes());
        seal(&mut pd, TAG_PD, start + 1, 496);
        image[at(start + 1)..at(start + 1) + bs].copy_from_slice(&pd);

        let mut lvd = vec![0u8; bs];
        lvd[16..20].copy_from_slice(&lvd_vdsn.to_le_bytes());
        dstring(&mut lvd[84..212], identifier);
        lvd[212..216].copy_from_slice(&(bs as u32).to_le_bytes());
        lvd[217..217 + spec.domain.len()].copy_from_slice(spec.domain);
        lvd[240..242].copy_from_slice(&0x0260u16.to_le_bytes());
        lvd[248..252].copy_from_slice(&(bs as u32).to_le_bytes());
        lvd[264..268].copy_from_slice(&6u32.to_le_bytes());
        lvd[268..272].copy_from_slice(&1u32.to_le_bytes());
        lvd[432..436].copy_from_slice(&(bs as u32 * 2).to_le_bytes());
        lvd[436..440].copy_from_slice(&places(bs).2.to_le_bytes());
        lvd[440..446].copy_from_slice(&[1, 6, 1, 0, 0, 0]);
        seal(&mut lvd, TAG_LVD, start + 2, 446 - 16);
        image[at(start + 2)..at(start + 2) + bs].copy_from_slice(&lvd);

        let mut td = vec![0u8; bs];
        seal(&mut td, TAG_TD, start + 3, 496);
        image[at(start + 3)..at(start + 3) + bs].copy_from_slice(&td);
    }

    pub fn image(spec: &Spec<'_>) -> Vec<u8> {
        let bs = spec.block_size;
        let mut image = vec![0u8; spec.blocks * bs];
        let stride = bs.max(2048);
        let mut vrs = 32768;
        let mut vsd = |ident: &[u8; 5], kind: u8| {
            image[vrs] = kind;
            image[vrs + 1..vrs + 6].copy_from_slice(ident);
            image[vrs + 6] = 1;
            vrs += stride;
        };
        if spec.iso_first {
            vsd(b"CD001", 1);
            vsd(b"CD001", 255);
        }
        vsd(b"BEA01", 0);
        vsd(b"NSR03", 0);
        vsd(b"TEA01", 0);
        let at = |sector: usize| sector * bs;
        let (main, reserve, integrity) = places(bs);
        for anchor in [256, spec.blocks - 1] {
            let mut avdp = vec![0u8; bs];
            avdp[16..20].copy_from_slice(&(16 * bs as u32).to_le_bytes());
            avdp[20..24].copy_from_slice(&main.to_le_bytes());
            avdp[24..28].copy_from_slice(&(16 * bs as u32).to_le_bytes());
            avdp[28..32].copy_from_slice(&reserve.to_le_bytes());
            seal(&mut avdp, TAG_AVDP, anchor as u32, 496);
            image[at(anchor)..at(anchor) + bs].copy_from_slice(&avdp);
        }
        sequence(&mut image, spec, main, 1, spec.lvid);
        sequence(&mut image, spec, reserve, 1, spec.lvid);
        let mut integrity_descriptor = vec![0u8; bs];
        seal(&mut integrity_descriptor, TAG_LVID, integrity, 496);
        image[at(integrity as usize)..at(integrity as usize) + bs]
            .copy_from_slice(&integrity_descriptor);
        image
    }
}
