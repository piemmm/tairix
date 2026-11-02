//! Archives laid out for tests, once for every test that needs one: the
//! least archive of each format the archive driver writes, the DEFLATE, gzip
//! and bzip2 streams archives are packed in, and an archive written back by
//! the driver's own writer. The driver's tests pack their members with
//! these, its fuzz harness takes its bases from them, and the disk-image
//! vertical plants archives written from them.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{FilesystemWrite as _, WriteRefusal};
use tairix_abi::DriverError;
use tairix_compress::bzip2::{self, BlockIndex};
use tairix_compress::deflate::{Deflate, Flush};
use tairix_compress::Output;
use tairix_drv_fs_archive::{ArchiveFs, Served};
use tairix_fsserve::{
    GrantedVolume, MemoryAccess, MemoryAttachment, ServedArchive, WritebackBudget,
};
use tairix_reclaim::Unpressured;

/// The member every [`seed`] but the zip's holds, by name and content.
pub const SEED: &[u8] = b"seed";

/// A RAR 5 archive the RAR encoder wrote (rarfile's test archive, under its
/// ISC licence): two members of 2048 bytes holding the same text, one stored
/// and one compressed, each checked by BLAKE2sp.
pub const RAR5_BLAKE: &[u8] = include_bytes!(
    "../../../../drivers/filesystem/archive/tests/fixtures/rar/rarfile/rar5-blake.rar"
);

/// An archive format the driver writes, as [`seed`] lays the least archive
/// of it out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Format {
    /// A zip.
    Zip,
    /// A tar.
    Tar,
    /// A tar in a gzip stream.
    TarGzip,
    /// A tar in a bzip2 stream.
    TarBzip2,
    /// A RISC OS Spark archive.
    Spark,
    /// A RISC OS `ArcFS` archive.
    ArcFs,
}

/// The least archive of `format`: a zip of no member; for every other
/// format, one stored member [`SEED`] holding [`SEED`], as a tar, Spark or
/// `ArcFS` archive is only told from other data by a member.
///
/// # Errors
///
/// The encoder's refusal, for a compressed tar.
pub fn seed(format: Format) -> Result<Vec<u8>, DriverError> {
    Ok(match format {
        Format::Zip => {
            let mut zip = 0x0605_4b50u32.to_le_bytes().to_vec();
            zip.resize(22, 0);
            zip
        }
        Format::Tar => tar(),
        Format::TarGzip => gzip(&tar())?,
        Format::TarBzip2 => bzip2(&tar(), 1)?,
        Format::Spark => spark(),
        Format::ArcFs => arcfs(),
    })
}

/// A ustar archive of [`SEED`].
fn tar() -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..SEED.len()].copy_from_slice(SEED);
    header[100..107].copy_from_slice(b"0000644");
    header[124..135].copy_from_slice(format!("{:011o}", SEED.len()).as_bytes());
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let mut tar = header.to_vec();
    tar.extend_from_slice(SEED);
    tar.resize(512 * 4, 0);
    tar
}

/// A Spark archive of [`SEED`], stored, typed as Data.
fn spark() -> Vec<u8> {
    let mut record = vec![0x1A, 0x82];
    let mut field = [0u8; 13];
    field[..SEED.len()].copy_from_slice(SEED);
    record.extend_from_slice(&field);
    record.extend_from_slice(&4u32.to_le_bytes());
    record.extend_from_slice(&[0x21, 0x58, 0, 0]);
    record.extend_from_slice(&tairix_crc16::checksum(SEED).to_le_bytes());
    record.extend_from_slice(&4u32.to_le_bytes());
    record.extend_from_slice(&0xFFFF_FD00u32.to_le_bytes());
    record.extend_from_slice(&0u32.to_le_bytes());
    record.extend_from_slice(&3u32.to_le_bytes());
    record.extend_from_slice(SEED);
    record.extend_from_slice(&[0x1A, 0x80]);
    record
}

/// An `ArcFS` archive of [`SEED`], stored.
fn arcfs() -> Vec<u8> {
    let mut archive = vec![0u8; 96 + 72];
    archive[..8].copy_from_slice(b"Archive\0");
    archive[8..12].copy_from_slice(&72u32.to_le_bytes());
    archive[12..16].copy_from_slice(&168u32.to_le_bytes());
    archive[16..20].copy_from_slice(&40u32.to_le_bytes());
    archive[20..24].copy_from_slice(&100u32.to_le_bytes());
    let entry = &mut archive[96..132];
    entry[0] = 0x82;
    entry[1..=SEED.len()].copy_from_slice(SEED);
    entry[12..16].copy_from_slice(&4u32.to_le_bytes());
    entry[24..28]
        .copy_from_slice(&(u32::from(tairix_crc16::checksum(SEED)) << 16 | 3).to_le_bytes());
    entry[28..32].copy_from_slice(&4u32.to_le_bytes());
    archive.extend_from_slice(SEED);
    archive
}

/// `data` as a raw DEFLATE stream.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for the encoder's refusal.
pub fn deflate(data: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut encoder = Box::new(Deflate::new());
    let mut stream = vec![0u8; encoder.bound(data.len())];
    let len = encoder
        .deflate(data, &mut stream, Flush::Finish)
        .map_err(|_| DriverError::DeviceFault)?;
    stream.truncate(len);
    Ok(stream)
}

/// `data` as a gzip stream of one member, named and stamped with nothing.
///
/// # Errors
///
/// As [`deflate`].
pub fn gzip(data: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 255];
    out.extend_from_slice(&deflate(data)?);
    out.extend_from_slice(&tairix_crc32::checksum(data).to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(data.len())
            .map_err(|_| DriverError::LengthOutOfRange)?
            .to_le_bytes(),
    );
    Ok(out)
}

/// `data` as a bzip2 stream at `level`, one to nine.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for the encoder's refusal.
pub fn bzip2(data: &[u8], level: u8) -> Result<Vec<u8>, DriverError> {
    struct Unindexed;
    impl BlockIndex for Unindexed {
        fn block(&mut self, _: u64, _: u64) {}
    }
    struct Gathered(Vec<u8>);
    impl Output for Gathered {
        fn put(&mut self, bytes: &[u8]) -> bool {
            self.0.extend_from_slice(bytes);
            true
        }
    }
    let refused = |_| DriverError::DeviceFault;
    let mut encoder = Box::new(
        bzip2::Encoder::new(
            vec![0u32; bzip2::encoder_work(level)],
            vec![0u8; bzip2::encoder_block(level)],
            level,
        )
        .map_err(refused)?,
    );
    let mut out = Gathered(Vec::new());
    encoder
        .write(data, &mut out, &mut Unindexed)
        .map_err(refused)?;
    encoder.finish(&mut out, &mut Unindexed).map_err(refused)?;
    Ok(out.0)
}

/// The archive `attachment` holds, served by the archive driver as an
/// image attach serves it, and why it refuses to write it, if it does.
fn served(
    attachment: &Rc<MemoryAttachment>,
    writable: bool,
) -> Result<(ArchiveFs<MemoryAccess>, Option<WriteRefusal>), DriverError> {
    let granted = GrantedVolume::open(attachment.access())?;
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &Unpressured,
    };
    let opened = Served::open(granted, !writable, budget)?;
    Ok((opened.fs, opened.write_refusal))
}

/// `archive`, attached by `name` only to be read, as the archive driver
/// serves it.
///
/// # Errors
///
/// The driver's refusal to open it.
pub fn opened(archive: &[u8], name: &[u8]) -> Result<ArchiveFs<MemoryAccess>, DriverError> {
    served(&MemoryAttachment::new(archive, false, name), false).map(|(fs, _)| fs)
}

/// `archive`, attached writable by `name`, changed by `change` and committed
/// by the archive driver: the archive the driver wrote.
///
/// # Errors
///
/// The driver's refusal to open, change or commit it;
/// [`DriverError::ReadOnly`] where it opens the archive only to be read.
pub fn written(
    archive: &[u8],
    name: &[u8],
    change: impl FnOnce(&mut ArchiveFs<MemoryAccess>) -> Result<(), DriverError>,
) -> Result<Vec<u8>, DriverError> {
    let attachment = MemoryAttachment::new(archive, true, name);
    let (mut fs, refusal) = served(&attachment, true)?;
    if refusal.is_some() {
        return Err(DriverError::ReadOnly);
    }
    change(&mut fs)?;
    fs.flush()?;
    Ok(attachment.archive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::driver::filesystem::{
        DirVisit, FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity,
    };

    fn names(archive: &[u8], name: &[u8]) -> Vec<Vec<u8>> {
        let mut fs = opened(archive, name).unwrap();
        let root = fs.root();
        let mut names = Vec::new();
        fs.read_dir(root, 0, &[], &mut |_, name| {
            names.push(name.to_vec());
            DirVisit::Take
        })
        .unwrap();
        names
    }

    /// Every seed opens as its format, holding what it says, and takes a
    /// file the driver writes back.
    #[test]
    fn every_seed_opens_and_is_written_back_with_a_new_member() {
        for (format, name) in [
            (Format::Zip, &b"seed.zip"[..]),
            (Format::Tar, b"seed.tar"),
            (Format::TarGzip, b"seed.tgz"),
            (Format::TarBzip2, b"seed.tbz2"),
            (Format::Spark, b"seed.spk"),
            (Format::ArcFs, b"seed.arc"),
        ] {
            let seed = seed(format).unwrap();
            let held = names(&seed, name);
            assert_eq!(
                held.len(),
                usize::from(format != Format::Zip),
                "{format:?}: {held:?}"
            );
            let archive = written(&seed, name, |fs| {
                let root = fs.root();
                let file = fs.create(
                    root,
                    b"Added",
                    NodeKind::RegularFile,
                    &NodeSecurity::new(0o644, 0, 0),
                )?;
                fs.write_at(file, 0, b"added").map(drop)
            })
            .unwrap();
            assert!(
                names(&archive, name).contains(&b"Added".to_vec()),
                "{format:?}"
            );
        }
    }

    #[test]
    fn streams_decode_to_what_they_packed() {
        let data = b"packed, then unpacked, then compared ".repeat(40);
        let packed = deflate(&data).unwrap();
        let mut out = vec![0u8; data.len()];
        assert_eq!(
            tairix_compress::inflate::inflate_prefix(&packed, &mut out),
            Ok(data.len())
        );
        assert_eq!(out, data);
        assert_eq!(&gzip(&data).unwrap()[..2], &[0x1F, 0x8B]);
        assert_eq!(&bzip2(&data, 3).unwrap()[..4], b"BZh3");
    }
}
