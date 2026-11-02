//! The driver's tests' helpers: the metered allocator, an archive opened as
//! the driver opens one over an attachment in memory, and its tree walked.

extern crate std;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{DirVisit, FilesystemRead, NodeId};
use tairix_abi::DriverError;
use tairix_fsserve::{GrantedVolume, ServedArchive, WritebackBudget};
pub(crate) use tairix_fsserve::{MemoryAccess, MemoryAttachment};
use tairix_reclaim::Unpressured;

use crate::{ArchiveFs, Served};

/// Metered, so a test can hold a decoder to what it asks of the allocator
/// and refuse it memory.
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

/// The archive `attachment` holds, opened as the driver opens it.
pub(crate) fn open(attachment: &Rc<MemoryAttachment>) -> ArchiveFs<MemoryAccess> {
    match try_open(attachment) {
        Ok(fs) => fs,
        Err(why) => std::panic!("the archive opens: {why:?}"),
    }
}

/// The archive `attachment` holds, opened as the driver opens it, or the
/// driver's refusal.
pub(crate) fn try_open(
    attachment: &Rc<MemoryAttachment>,
) -> Result<ArchiveFs<MemoryAccess>, DriverError> {
    let granted = GrantedVolume::open(attachment.access()).expect("described");
    let read_only = granted.info().read_only;
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &Unpressured,
    };
    Served::open(granted, read_only, budget).map(|opened| opened.fs)
}

/// The names `dir` lists, in order.
pub(crate) fn names<F: FilesystemRead>(fs: &mut F, dir: NodeId) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    fs.read_dir(dir, 0, &[], &mut |_, name| {
        names.push(name.to_vec());
        DirVisit::Take
    })
    .expect("lists");
    names
}

/// All of `file`'s bytes.
pub(crate) fn contents<F: FilesystemRead>(fs: &mut F, file: NodeId) -> Vec<u8> {
    let size = fs.node_info(file).expect("described").size;
    let mut out = vec![0u8; usize::try_from(size).unwrap()];
    let mut at = 0;
    while at < out.len() {
        let got = fs.read_at(file, at as u64, &mut out[at..]).expect("reads");
        assert_ne!(got, 0, "a file reads to its end");
        at += got;
    }
    out
}

/// The node `path` names from the root.
pub(crate) fn walk<F: FilesystemRead>(fs: &mut F, path: &[&[u8]]) -> NodeId {
    let mut at = fs.root();
    for part in path {
        at = fs.lookup(at, part).expect("found");
    }
    at
}

/// A zip holding nothing: its end record alone.
pub(crate) fn empty_zip() -> Vec<u8> {
    let mut archive = Vec::new();
    archive.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    archive.resize(22, 0);
    archive
}

fn le16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// How a member of a zip built here is coded.
#[derive(Copy, Clone)]
pub(crate) enum Packing<'a> {
    Stored,
    Deflated,
    Bzip2,
    /// Bytes another archiver coded, with its method and the version it
    /// says is needed to extract them.
    Raw {
        method: u16,
        needed: u16,
        coded: &'a [u8],
    },
}

impl Packing<'_> {
    /// The method, the version needed, and `data` coded so.
    fn code(self, data: &[u8]) -> (u16, u16, Vec<u8>) {
        match self {
            Self::Stored => (0, 10, data.to_vec()),
            Self::Deflated => (8, 20, tairix_test_archive_image::deflate(data).unwrap()),
            Self::Bzip2 => (12, 46, tairix_test_archive_image::bzip2(data, 1).unwrap()),
            Self::Raw {
                method,
                needed,
                coded,
            } => (method, needed, coded.to_vec()),
        }
    }
}

/// A zip another archiver wrote: each member coded as its packing says,
/// with no extra fields, named in code page 437, counted in the end record
/// modulo 65536 as a writer knowing no ZIP64 counts.
pub(crate) fn foreign_zip(members: &[(&[u8], &[u8], Packing<'_>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for &(name, data, packing) in members {
        let crc = tairix_crc32::checksum(data);
        let (method, needed, packed) = packing.code(data);
        let local = u32::try_from(out.len()).unwrap();
        le32(&mut out, 0x0403_4b50);
        for value in [needed, 0, method, 0x6000, 0x5a21] {
            le16(&mut out, value);
        }
        le32(&mut out, crc);
        le32(&mut out, u32::try_from(packed.len()).unwrap());
        le32(&mut out, u32::try_from(data.len()).unwrap());
        le16(&mut out, u16::try_from(name.len()).unwrap());
        le16(&mut out, 0);
        out.extend_from_slice(name);
        out.extend_from_slice(&packed);
        le32(&mut central, 0x0201_4b50);
        for value in [20u16, needed, 0, method, 0x6000, 0x5a21] {
            le16(&mut central, value);
        }
        le32(&mut central, crc);
        le32(&mut central, u32::try_from(packed.len()).unwrap());
        le32(&mut central, u32::try_from(data.len()).unwrap());
        le16(&mut central, u16::try_from(name.len()).unwrap());
        for value in [0u16, 0, 0, 0] {
            le16(&mut central, value);
        }
        le32(&mut central, 0);
        le32(&mut central, local);
        central.extend_from_slice(name);
    }
    let at = u32::try_from(out.len()).unwrap();
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    let count = u16::try_from(members.len() & 0xFFFF).unwrap();
    le16(&mut out, count);
    le16(&mut out, count);
    le32(&mut out, u32::try_from(central.len()).unwrap());
    le32(&mut out, at);
    le16(&mut out, 0);
    out
}

/// `block`'s ustar checksum, written again after a change to it: the sum of
/// its bytes, the field counted as spaces.
pub(crate) fn seal_tar_header(block: &mut [u8; 512]) {
    block[148..156].fill(b' ');
    let sum: u32 = block.iter().map(|&byte| u32::from(byte)).sum();
    let field = std::format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(field.as_bytes());
}

/// A ustar header naming `name` of `kind`, `size` bytes long, owned by user
/// and group 1000.
pub(crate) fn tar_header(name: &[u8], kind: u8, size: usize) -> [u8; 512] {
    let mut block = [0u8; 512];
    block[..name.len()].copy_from_slice(name);
    block[100..107].copy_from_slice(b"0000644");
    block[108..115].copy_from_slice(b"0001750");
    block[116..123].copy_from_slice(b"0001750");
    let size = std::format!("{size:011o}");
    block[124..135].copy_from_slice(size.as_bytes());
    block[136..147].copy_from_slice(b"14727463270");
    block[156] = kind;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    seal_tar_header(&mut block);
    block
}

/// The records `records` as a pax extended header's data.
pub(crate) fn pax_records(records: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(key, value) in records {
        let body = key.len() + value.len() + 3;
        let mut len = body + 1;
        while len != body + std::format!("{len}").len() {
            len = body + std::format!("{len}").len();
        }
        out.extend_from_slice(std::format!("{len} ").as_bytes());
        out.extend_from_slice(key);
        out.push(b'=');
        out.extend_from_slice(value);
        out.push(b'\n');
    }
    out
}

/// The blocks of a tar holding `entries` — each a header and the data after
/// it, padded to whole blocks — then its end marker.
pub(crate) fn tar_of(entries: &[([u8; 512], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (header, data) in entries {
        out.extend_from_slice(header);
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
    }
    out.extend_from_slice(&[0; 1024]);
    out
}

/// A tar of `members`, each a file, or a directory where its name ends in
/// `/`.
pub(crate) fn tar(members: &[(&[u8], &[u8])]) -> Vec<u8> {
    let entries: Vec<([u8; 512], &[u8])> = members
        .iter()
        .map(|&(name, data)| {
            let kind = if name.ends_with(b"/") { b'5' } else { b'0' };
            (tar_header(name, kind, data.len()), data)
        })
        .collect();
    tar_of(&entries)
}
