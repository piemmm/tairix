//! Specification-shaped FAT32 disk-image fixture shared by the
//! end-to-end QEMU FAT32-over-virtio_blk vertical.
//!
//! The host harness (`tools/xtask`) plants the bytes returned by
//! [`build_image`] on the test's backing disk before the guest boots.
//! The freestanding guest tail
//! (`tests/integration/virtio_qemu_support`) mounts that very volume
//! through the real FAT32 driver and verifies it, then writes a fresh
//! file and reads it back. Both sides name the same fixed file through
//! the constants below, so the on-disk contract lives in exactly one
//! place.
//!
//! The image is a minimal but genuine FAT32 volume — 1 MiB, two
//! mirrored FATs, one-sector clusters — written through `lib/fat`'s
//! encoders. It has far fewer clusters than a formatted FAT32 volume, which
//! every reader allows: FAT32 is told by its parameter block's shape (no
//! fixed root, no 16-bit FAT size), and the volume probe recognises it by
//! the type its boot sector names. [`build_labelled_image`] labels it, as
//! the twin-volume vertical's USB sticks need. It is `no_std` + `alloc` so
//! it links into both the host build tool and the freestanding guest test.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use tairix_fat::boot::{Bpb, Kind, BOOT_SECTOR};
use tairix_fat::dir::{attr, ShortEntry, ENTRY};
use tairix_fat::format::{self, FIXED_MEDIA};
use tairix_fat::{name, table};

/// On-disk bytes-per-sector (BPB offset 11). The on-disk fields carry
/// their natural width so the boot sector is written without any
/// narrowing cast.
const BYTES_PER_SECTOR: u16 = 512;

/// Total size of the produced image, in 512-byte sectors (1 MiB). Large
/// enough that the FAT covers every data cluster, so the write path can
/// allocate freely without the FAT and the data region overlapping.
const TOTAL_SECTORS_U32: u32 = 2048;

/// Logical block (sector) size of the produced image, in bytes. Matches
/// the 512-byte sector QEMU's virtio-blk reports by default and the unit
/// the backing-disk planter addresses in.
pub const SECTOR_BYTES: usize = BYTES_PER_SECTOR as usize;

/// Total size of the produced image, in 512-byte sectors (1 MiB).
pub const TOTAL_SECTORS: u64 = TOTAL_SECTORS_U32 as u64;

/// 8.3 short-named file planted in the root directory. The guest tail
/// looks it up and verifies [`PLANTED_FILE_CONTENT`].
pub const PLANTED_FILE_NAME: &[u8] = b"HELLO.TXT";

/// Contents of [`PLANTED_FILE_NAME`]. One sector keeps it inside a
/// single cluster.
pub const PLANTED_FILE_CONTENT: &[u8] = b"Hello from a planted FAT32 image on virtio-blk.\n";

/// File the guest tail creates and writes after mounting. The mixed
/// case exercises the driver's VFAT long-name round-trip.
pub const NEW_FILE_NAME: &[u8] = b"Written.txt";

/// Contents the guest tail writes to [`NEW_FILE_NAME`] and reads back.
pub const NEW_FILE_CONTENT: &[u8] = b"TAIRiX wrote this file to FAT32 over virtio-blk.\n";

/// Sectors per cluster (one — the simplest valid geometry).
const SECTORS_PER_CLUSTER: u8 = 1;
/// Reserved sectors before the first FAT (sector 0 is the boot sector).
const RESERVED_SECTORS: u16 = 32;
/// Number of FAT copies; two exercises the driver's FAT mirroring.
const NUM_FATS: u8 = 2;
/// Length of each FAT, in sectors. 16 sectors hold 2048 32-bit entries —
/// more than the data region's cluster count, so every data cluster is
/// addressable.
const FAT_SECTORS: u32 = 16;
/// First cluster of the root directory (FAT32 numbers clusters from 2).
const ROOT_CLUSTER: u32 = 2;
/// Cluster holding [`PLANTED_FILE_CONTENT`].
const HELLO_CLUSTER: u32 = 3;
/// The volume's serial, which every copy of the image shares.
const SERIAL: u32 = 0x7E57_FA32;

/// The volume's parameter block.
fn bpb() -> Bpb {
    Bpb {
        kind: Kind::Fat32,
        bytes_per_sector: BYTES_PER_SECTOR,
        sectors_per_cluster: SECTORS_PER_CLUSTER,
        reserved_sectors: RESERVED_SECTORS,
        fats: NUM_FATS,
        root_entries: 0,
        total_sectors: TOTAL_SECTORS_U32,
        media: FIXED_MEDIA,
        fat_sectors: FAT_SECTORS,
        sectors_per_track: 0,
        heads: 0,
        hidden_sectors: 0,
        root_cluster: ROOT_CLUSTER,
        fsinfo_sector: 0,
        backup_boot_sector: 0,
        drive: 0x80,
        serial: Some(SERIAL),
        label: None,
    }
}

/// `bytes` as an index into the image.
fn index(bytes: u64) -> usize {
    usize::try_from(bytes).expect("inside the image")
}

/// Byte offset of `cluster`'s data within the image.
fn cluster_byte_offset(cluster: u32) -> usize {
    index(
        bpb()
            .cluster_start(cluster)
            .expect("a cluster of the volume"),
    )
}

/// Write `entry` at byte `offset` of the image.
fn put_entry(img: &mut [u8], offset: usize, entry: &ShortEntry) {
    img[offset..offset + ENTRY].copy_from_slice(&entry.encode());
}

/// Construct the FAT32 image described in the module docs.
#[must_use]
pub fn build_image() -> Vec<u8> {
    build(None)
}

/// [`build_image`]'s volume, labelled `label`, a FAT label field, space
/// padded: its root directory opens with the volume-label entry, where
/// formatters write it and the volume probe reads it, ahead of
/// [`PLANTED_FILE_NAME`]. Every copy carries one identity, as volumes
/// formatted alike do.
#[must_use]
pub fn build_labelled_image(label: &[u8; 11]) -> Vec<u8> {
    build(Some(label))
}

/// The image, its root directory opening with a volume-label entry recording
/// `label` where there is one.
///
/// # Panics
///
/// Never in practice: the layout constants are sized so every write
/// stays in bounds, [`PLANTED_FILE_NAME`] is an 8.3 name and
/// [`PLANTED_FILE_CONTENT`] fits one cluster. The debug assertion
/// documents the last for a future edit.
fn build(label: Option<&[u8; 11]>) -> Vec<u8> {
    debug_assert!(
        PLANTED_FILE_CONTENT.len() <= SECTOR_BYTES * usize::from(SECTORS_PER_CLUSTER),
        "planted file must fit one cluster"
    );

    let mut img = vec![0u8; TOTAL_SECTORS_U32 as usize * SECTOR_BYTES];
    let bpb = bpb();
    img[..BOOT_SECTOR].copy_from_slice(&format::boot_sector(&bpb, b"TAIRIX  ", &[]));

    // File allocation table, mirrored across both copies.
    let [media, end] = table::head(bpb.kind, bpb.media);
    for fat in 0..u64::from(bpb.fats) {
        let start = index(bpb.fat_start() + fat * bpb.fat_bytes());
        let copy = &mut img[start..start + index(bpb.fat_bytes())];
        for (cluster, value) in [
            (0, media),
            (1, end),
            (ROOT_CLUSTER, end),
            (HELLO_CLUSTER, end),
        ] {
            table::set(bpb.kind, copy, cluster, value);
        }
    }

    // Root directory (cluster 2): the label entry, if any, then HELLO.TXT.
    let mut entry = cluster_byte_offset(ROOT_CLUSTER);
    if let Some(label) = label {
        let label = ShortEntry {
            name: *label,
            attributes: attr::VOLUME_ID,
            ..ShortEntry::default()
        };
        put_entry(&mut img, entry, &label);
        entry += ENTRY;
    }
    let hello = ShortEntry {
        name: name::field(PLANTED_FILE_NAME).expect("an 8.3 name"),
        attributes: attr::ARCHIVE,
        cluster: HELLO_CLUSTER,
        size: u32::try_from(PLANTED_FILE_CONTENT.len()).expect("one cluster"),
        ..ShortEntry::default()
    };
    put_entry(&mut img, entry, &hello);

    // HELLO.TXT contents (cluster 3).
    let hello = cluster_byte_offset(HELLO_CLUSTER);
    img[hello..hello + PLANTED_FILE_CONTENT.len()].copy_from_slice(PLANTED_FILE_CONTENT);

    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::driver::block::{Block, BlockGeometry};
    use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity};
    use tairix_abi::DriverError;
    use tairix_drv_fs_fat32::Fat32;

    /// In-memory [`Block`] device over the built image, used to drive
    /// the real FAT32 driver on the host exactly as the guest drives it
    /// over virtio-blk.
    struct VecBlock {
        store: Vec<u8>,
    }

    impl VecBlock {
        fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
            if len == 0 || !len.is_multiple_of(SECTOR_BYTES) {
                return Err(DriverError::BufferTooSmall);
            }
            let start = usize::try_from(lba)
                .ok()
                .and_then(|l| l.checked_mul(SECTOR_BYTES))
                .ok_or(DriverError::LengthOutOfRange)?;
            let end = start
                .checked_add(len)
                .ok_or(DriverError::LengthOutOfRange)?;
            if end > self.store.len() {
                return Err(DriverError::LengthOutOfRange);
            }
            Ok((start, end))
        }
    }

    impl Block for VecBlock {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            Ok(BlockGeometry {
                block_size: u32::from(BYTES_PER_SECTOR),
                block_count: TOTAL_SECTORS,
            })
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            let (start, end) = self.span(lba, buf.len())?;
            buf.copy_from_slice(&self.store[start..end]);
            Ok(())
        }

        fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
            let (start, end) = self.span(lba, buf.len())?;
            self.store[start..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    fn mount() -> Fat32<VecBlock> {
        let dev = VecBlock {
            store: build_image(),
        };
        Fat32::open(dev).expect("the built image is a valid FAT32 volume")
    }

    #[test]
    fn driver_mounts_the_built_image() {
        let _fs = mount();
    }

    #[test]
    fn planted_file_reads_back_its_known_contents() {
        let mut fs = mount();
        let root = fs.root();
        let node = fs
            .lookup(root, PLANTED_FILE_NAME)
            .expect("HELLO.TXT present");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read HELLO.TXT");
        assert_eq!(&buf[..n], PLANTED_FILE_CONTENT);
    }

    #[test]
    fn a_fresh_file_round_trips_through_create_write_and_read() {
        let mut fs = mount();
        let root = fs.root();
        let file = fs
            .create(
                root,
                NEW_FILE_NAME,
                NodeKind::RegularFile,
                &NodeSecurity::new(0o644, 0, 0),
            )
            .expect("create new file");
        let written = fs
            .write_at(file, 0, NEW_FILE_CONTENT)
            .expect("write new file");
        assert_eq!(written, NEW_FILE_CONTENT.len());

        let node = fs.lookup(root, NEW_FILE_NAME).expect("new file present");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read new file");
        assert_eq!(&buf[..n], NEW_FILE_CONTENT);
    }

    #[test]
    fn the_planted_file_survives_a_second_file_being_written() {
        let mut fs = mount();
        let root = fs.root();
        let file = fs
            .create(
                root,
                NEW_FILE_NAME,
                NodeKind::RegularFile,
                &NodeSecurity::new(0o644, 0, 0),
            )
            .expect("create new file");
        fs.write_at(file, 0, NEW_FILE_CONTENT)
            .expect("write new file");

        let node = fs
            .lookup(root, PLANTED_FILE_NAME)
            .expect("HELLO.TXT present");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read HELLO.TXT");
        assert_eq!(&buf[..n], PLANTED_FILE_CONTENT);
    }

    #[test]
    fn image_is_exactly_the_advertised_size() {
        let expected =
            usize::try_from(TOTAL_SECTORS).expect("sector count fits usize") * SECTOR_BYTES;
        assert_eq!(build_image().len(), expected);
    }

    #[test]
    fn the_boot_sector_names_its_filesystem_type() {
        let at = tairix_fat::boot::at::TYPE_32;
        assert_eq!(&build_image()[at..at + 8], Kind::Fat32.type_name());
    }

    #[test]
    fn a_labelled_image_opens_its_root_with_the_label_and_still_serves_its_file() {
        const LABEL: &[u8; 11] = b"LABELLED   ";
        let image = build_labelled_image(LABEL);
        let entry = cluster_byte_offset(ROOT_CLUSTER);
        let found = ShortEntry::decode(image[entry..entry + ENTRY].try_into().expect("an entry"));
        assert_eq!(&found.name, LABEL);
        assert_eq!(found.attributes, attr::VOLUME_ID);
        let mut fs = Fat32::open(VecBlock { store: image }).expect("a valid FAT32 volume");
        let root = fs.root();
        let node = fs
            .lookup(root, PLANTED_FILE_NAME)
            .expect("HELLO.TXT beside the label");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read HELLO.TXT");
        assert_eq!(&buf[..n], PLANTED_FILE_CONTENT);
    }
}
