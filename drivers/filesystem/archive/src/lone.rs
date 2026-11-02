//! A lone compressed file (`plans/ARCHIVES.md` AR5): a gzip or bzip2 stream
//! that holds no tar, served as a volume of its one file. The file is named
//! by the gzip header's name, else by the archive's own name less its ending,
//! as gzip and bzip2 name what they decompress. Its bytes may be written,
//! and a gzip one's name changed, the stream then compressed again whole.

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NameMatching, NodeTimes};
use tairix_abi::time::Time64;
use tairix_abi::volume::{ArchiveName, FormatName};
use tairix_abi::DriverError;
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check, Coding, Data, Lead};
use crate::format::{Content, Format, Placed, Read};
use crate::name::{servable, Charset};
use crate::region::Regions;
use crate::stream::{Compression, Packing, Stream, Writer};
use crate::tree::{Body, Meta, Tree, ROOT};
use crate::zip::directory_meta;

/// The endings gzip gives what it compresses, each with what it stands in
/// place of.
const GZIP_ENDINGS: &[(&[u8], &[u8])] = &[
    (b".gz", b""),
    (b".z", b""),
    (b"-gz", b""),
    (b"-z", b""),
    (b"_z", b""),
    (b".tgz", b".tar"),
    (b".taz", b".tar"),
];

/// The endings bzip2 gives what it compresses, each with what it stands in
/// place of.
const BZIP2_ENDINGS: &[(&[u8], &[u8])] = &[
    (b".bz2", b""),
    (b".bz", b""),
    (b".tbz2", b".tar"),
    (b".tbz", b".tar"),
];

/// What bzip2 appends to a name that carries none of its endings.
const NO_ENDING: &[u8] = b".out";

/// The name the file has when nothing names it: an archive attached by no
/// name, which only a test does.
const UNNAMED: &[u8] = b"data";

/// What a lone file's archive keeps: how its stream is written, and the name
/// its file takes from the archive's, which a gzip header then need not
/// record.
pub(crate) struct Lone {
    packing: Packing,
    derived: Vec<u8>,
}

impl Lone {
    /// What the mount table calls the archive.
    pub(crate) const fn format_name(&self) -> FormatName {
        match self.packing.compression() {
            Compression::Gzip => FormatName::Gzip,
            Compression::Bzip2 => FormatName::Bzip2,
        }
    }

    /// Refuse a name the stream cannot record: any for bzip2, whose stream
    /// records none, and for gzip one with a character ISO 8859-1 lacks.
    pub(crate) fn admits_name(&self, name: &[u8]) -> Result<(), DriverError> {
        match self.packing {
            Packing::Gzip { .. } if Charset::Latin1.encode(name).is_some() => Ok(()),
            Packing::Gzip { .. } => Err(DriverError::LengthOutOfRange),
            Packing::Bzip2 { .. } => Err(DriverError::Unsupported),
        }
    }

    /// How the stream is written from `tree`: a gzip header naming the file
    /// where the archive's own name does not, stamped with its modification
    /// time.
    pub(crate) fn packing(&self, tree: &Tree) -> Result<Packing, DriverError> {
        let (_, node) = the_file(tree)?;
        Ok(match &self.packing {
            Packing::Gzip { name: recorded, .. } => {
                let name = if recorded.is_none() && node.name == self.derived {
                    None
                } else {
                    Some(
                        Charset::Latin1
                            .encode(&node.name)
                            .ok_or(DriverError::LengthOutOfRange)?,
                    )
                };
                // A time a header's word cannot hold is recorded as none.
                let mtime = u32::try_from(node.meta.times.modified.secs()).unwrap_or(0);
                Packing::Gzip { name, mtime }
            }
            bzip2 @ Packing::Bzip2 { .. } => bzip2.clone(),
        })
    }
}

/// Write the one file `tree` holds onto the next archive's stream.
pub(crate) fn write<A: VolumeAccess>(
    tree: &Tree,
    content: &mut Content<'_, A>,
    out: &mut Writer<'_, '_, A>,
) -> Result<Vec<Placed>, DriverError> {
    let (id, node) = the_file(tree)?;
    let Body::File(data) = &node.body else {
        return Err(DriverError::DeviceFault);
    };
    content.copy_into((id, node.content_gen), data, out)?;
    let mut placed = Vec::new();
    placed
        .try_reserve_exact(1)
        .map_err(|_| DriverError::OutOfMemory)?;
    placed.push(Placed {
        id,
        data: Some(stream_data(data.len())),
        kept: None,
        notes: None,
    });
    Ok(placed)
}

/// The one file `tree` holds.
fn the_file(tree: &Tree) -> Result<(u64, &crate::tree::Node), DriverError> {
    let mut named = tree.nodes().filter(|&(id, node)| id != ROOT && node.named);
    match (named.next(), named.next()) {
        (Some((id, node)), None) if matches!(node.body, Body::File(_)) => Ok((id, node)),
        _ => Err(DriverError::DeviceFault),
    }
}

/// A file's `len` bytes, the whole of the decoded stream.
const fn stream_data(len: u64) -> Data {
    Data::Archived(Archived {
        at: 0,
        lead: Lead::None,
        len,
        coding: Coding::Stream,
        check: Check::None,
    })
}

/// Read the lone file the stream `regions` holds is, written as `packing`
/// says, of an archive attached by `archive`.
///
/// # Errors
///
/// The stream's refusal; [`DriverError::OutOfMemory`].
pub(crate) fn read<A: VolumeAccess>(
    regions: &Regions<A>,
    packing: Packing,
    mut stream: Stream,
    archive: Option<&ArchiveName>,
) -> Result<Read, DriverError> {
    let len = stream.measure(regions)?;
    let derived = match archive {
        Some(archive) => decompressed_name(packing.compression(), archive.as_bytes())?,
        None => joined(UNNAMED, b"")?,
    };
    let (name, mtime) = match &packing {
        Packing::Gzip { name, mtime } => (name.as_deref().and_then(recorded_name), *mtime),
        Packing::Bzip2 { .. } => (None, 0),
    };
    let modified = if mtime == 0 {
        Time64::UNIX_EPOCH
    } else {
        Time64::from_secs(i64::from(mtime))
    };
    let meta = Meta {
        mode: 0o644,
        owner: None,
        times: NodeTimes {
            created: Time64::UNIX_EPOCH,
            modified,
            accessed: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
        },
        acorn: None,
    };
    let mut tree = Tree::new(NameMatching::Exact, directory_meta());
    let name = match name {
        Some(name) => name,
        None => joined(&derived, b"")?,
    };
    tree.add(ROOT, &name, Body::File(stream_data(len)), meta, None)?;
    Ok(Read {
        tree,
        format: Format::Lone(Lone { packing, derived }),
        refusal: None,
        stream: Some(stream),
        rar: None,
    })
}

/// The name a gzip header records, as a served name: its last component,
/// as gzip itself takes it, read as ISO 8859-1, or `None` for one that
/// names nothing.
fn recorded_name(field: &[u8]) -> Option<Vec<u8>> {
    let leaf = field.rsplit(|&byte| byte == b'/').next()?;
    let name = Charset::Latin1.decode(leaf)?;
    servable(&name).then_some(name)
}

/// The name of what an archive named `archive` decompresses to: the name
/// with its compressor's ending replaced, else with [`NO_ENDING`] added,
/// else the name itself.
fn decompressed_name(compression: Compression, archive: &[u8]) -> Result<Vec<u8>, DriverError> {
    let endings = match compression {
        Compression::Gzip => GZIP_ENDINGS,
        Compression::Bzip2 => BZIP2_ENDINGS,
    };
    for &(ending, stands_for) in endings {
        if let Some(stem) = strip_ending(archive, ending) {
            let name = joined(stem, stands_for)?;
            if servable(&name) {
                return Ok(name);
            }
        }
    }
    let name = joined(archive, NO_ENDING)?;
    if servable(&name) {
        return Ok(name);
    }
    joined(archive, b"")
}

/// Whether an archive named `name` is a tar by its ending, compressed or
/// not.
pub(crate) fn names_tar(name: &[u8]) -> bool {
    [GZIP_ENDINGS, BZIP2_ENDINGS]
        .iter()
        .flat_map(|endings| endings.iter())
        .any(|&(ending, stands_for)| match strip_ending(name, ending) {
            Some(stem) => stands_for == b".tar" || strip_ending(stem, b".tar").is_some(),
            None => false,
        })
}

/// `name` less `ending`, matched without regard to ASCII case, where it ends
/// so and holds more.
fn strip_ending<'n>(name: &'n [u8], ending: &[u8]) -> Option<&'n [u8]> {
    let cut = name
        .len()
        .checked_sub(ending.len())
        .filter(|&cut| cut > 0)?;
    name[cut..]
        .eq_ignore_ascii_case(ending)
        .then(|| &name[..cut])
}

/// `head` then `tail`, owned.
fn joined(head: &[u8], tail: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut out = Vec::new();
    out.try_reserve_exact(head.len() + tail.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    out.extend_from_slice(head);
    out.extend_from_slice(tail);
    Ok(out)
}

#[cfg(test)]
#[path = "lone_tests.rs"]
mod tests;
