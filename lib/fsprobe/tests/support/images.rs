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
use tairix_fsprobe::{adfs, ARXFS_HEADER_MAGIC, EXT4_SUPERBLOCK_MAGIC, FAT_DIR_ENTRY_LEN};

/// An in-memory device over a byte image.
pub struct Image {
    pub block_size: u32,
    pub bytes: Vec<u8>,
    pub layout: Option<DiscLayout>,
}

impl Block for Image {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: (self.bytes.len() / self.block_size as usize) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
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
    })
    .expect("valid geometry")
}

/// Bytes of the head the probe reads.
pub const HEAD: usize = 2048;

/// A structurally valid FAT32 boot sector with the given label bytes (11,
/// space-padded) and serial.
pub fn fat32_boot(serial: [u8; 4], label11: &[u8; 11]) -> Vec<u8> {
    let mut head = vec![0u8; HEAD];
    head[11..13].copy_from_slice(&512u16.to_le_bytes());
    head[13] = 8; // sectors per cluster
    head[14..16].copy_from_slice(&32u16.to_le_bytes());
    head[16] = 2; // FAT count
    head[36..40].copy_from_slice(&1024u32.to_le_bytes());
    head[44..48].copy_from_slice(&2u32.to_le_bytes());
    head[66] = 0x29;
    head[67..71].copy_from_slice(&serial);
    head[71..82].copy_from_slice(label11);
    head[82..90].copy_from_slice(b"FAT32   ");
    head[510] = 0x55;
    head[511] = 0xAA;
    head
}

/// Where `fat32_boot`'s root directory lies: 32 reserved sectors and two
/// 1024-sector FATs of 512 bytes.
pub const FAT32_ROOT: usize = (32 + 2 * 1024) * 512;

/// One 32-byte FAT directory entry named `name` with `attributes`.
pub fn dir_entry(name: &[u8; 11], attributes: u8) -> [u8; FAT_DIR_ENTRY_LEN] {
    let mut entry = [0u8; FAT_DIR_ENTRY_LEN];
    entry[..11].copy_from_slice(name);
    entry[11] = attributes;
    entry
}

/// `fat32_boot` with `entries` at the head of the directory at `root`.
pub fn fat32_with_root(
    label11: &[u8; 11],
    root: usize,
    entries: &[[u8; FAT_DIR_ENTRY_LEN]],
) -> Vec<u8> {
    let mut image = fat32_boot(*b"SRLN", label11);
    image.resize(root + 4096, 0);
    for (slot, entry) in image[root..]
        .as_chunks_mut::<FAT_DIR_ENTRY_LEN>()
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
