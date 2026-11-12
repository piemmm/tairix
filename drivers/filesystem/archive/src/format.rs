//! The formats an archive volume is served in (`plans/ARCHIVES.md` §4): how
//! each reads its index into the member tree, and how each writes the next
//! archive from the tree.
//!
//! A writer copies what it can rather than encoding it again: a member
//! nothing has changed is its old record, copied by the kernel; a member
//! whose bytes are unchanged keeps them, copied likewise beneath a new
//! header; only written bytes pass through the driver. A compressed archive
//! is one stream, so it is compressed again whole, its kept records read
//! decoded.

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{StampField, WriteRefusal, DOS_TIME, EVERY_INSTANT};
use tairix_abi::time::Time64;
use tairix_abi::volume::{ArchiveName, FormatName, VolumeFsType, ARCHIVE_REGION, ARCHIVE_STAGING};
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn;
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Coding, Cursors, Data};
use crate::region::{Regions, Sink};
use crate::riscos;
use crate::stream::{Compression, Packing, Stream, Writer};
use crate::tree::{Kept, Meta, Tree};
use crate::{arcfs, lone, rar, spark, tar, zip};

/// What a member's record keeps of its modification and access times, `None`
/// for a time it keeps nothing of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MemberTimes {
    pub(crate) modified: Option<StampField>,
    pub(crate) accessed: Option<StampField>,
}

impl MemberTimes {
    const NONE: Self = Self {
        modified: None,
        accessed: None,
    };
}

/// What a zip member's extended timestamp holds: whole seconds, negative
/// ones signed and those past 2038 unsigned, which the MS-DOS stamp beside it
/// tells apart.
const ZIP_MODIFIED: StampField = StampField::new(
    Time64::from_secs(-(1 << 31)),
    1_000_000_000,
    0..(1 << 31) + (1 << 32),
);

/// An archive's format and what the format keeps of the archive as a whole.
pub(crate) enum Format {
    /// A zip archive.
    Zip(zip::Zip),
    /// A tar archive, plain or compressed.
    Tar(tar::Tar),
    /// One file, compressed alone.
    Lone(lone::Lone),
    /// A RISC OS Spark archive, or a plain ARC one.
    Spark(spark::Spark),
    /// A RISC OS `ArcFS` archive.
    ArcFs(arcfs::ArcFs),
    /// A RAR archive, read only.
    Rar(rar::Rar),
}

/// An archive read: its members, its format, why it may not be written, and
/// the stream a compressed one's members are read through, or the reader a
/// RAR archive's are.
pub(crate) struct Read {
    pub(crate) tree: Tree,
    pub(crate) format: Format,
    pub(crate) refusal: Option<WriteRefusal>,
    pub(crate) stream: Option<Stream>,
    pub(crate) rar: Option<rar::Reader>,
}

/// Where the next archive put a member: its bytes, its record while
/// nothing about it changes, and its format's notes where they moved.
pub(crate) struct Placed {
    pub(crate) id: u64,
    pub(crate) data: Option<Data>,
    pub(crate) kept: Option<Kept>,
    pub(crate) notes: Option<Vec<u8>>,
}

/// The next archive, written: its length, where each member went, and for a
/// compressed archive its decoded stream's index.
pub(crate) struct Written {
    pub(crate) len: u64,
    pub(crate) placed: Vec<Placed>,
    pub(crate) stream: Option<Stream>,
}

impl Format {
    /// Read the archive `regions` holds, as the probe recognised it, attached
    /// by `name`.
    ///
    /// # Errors
    ///
    /// The format's refusal: [`DriverError::Unsupported`] for a format or a
    /// variant of one this driver does not read, [`DriverError::BadMagic`]
    /// for an archive damaged where nothing can be believed.
    pub(crate) fn read<A: VolumeAccess>(
        fstype: VolumeFsType,
        regions: &Regions<A>,
        name: Option<&ArchiveName>,
    ) -> Result<Read, DriverError> {
        let compression = match fstype {
            VolumeFsType::Zip => return zip::read(regions),
            VolumeFsType::Tar => return tar::read(regions, None),
            VolumeFsType::Spark => return spark::read(regions),
            VolumeFsType::ArcFs => return arcfs::read(regions),
            VolumeFsType::Rar => return rar::read(regions),
            VolumeFsType::Gzip => Compression::Gzip,
            VolumeFsType::Bzip2 => Compression::Bzip2,
            _ => return Err(DriverError::Unsupported),
        };
        // Whether the stream holds a tar is answered here, by decoding it,
        // never by the kernel's probe.
        let packing = Packing::of(compression, regions)?;
        let mut stream = Stream::new(compression)?;
        if tar::holds_tar(&mut stream, regions, name)? {
            tar::read(regions, Some((packing, stream)))
        } else {
            lone::read(regions, packing, stream, name)
        }
    }

    /// What the mount table calls the archive.
    pub(crate) const fn name(&self) -> FormatName {
        match self {
            Self::Zip(_) => FormatName::Zip,
            Self::Tar(tar) => tar.format_name(),
            Self::Lone(lone) => lone.format_name(),
            Self::Spark(_) => FormatName::Spark,
            Self::ArcFs(_) => FormatName::ArcFs,
            Self::Rar(_) => FormatName::Rar,
        }
    }

    /// Refuse a member name the format cannot spell.
    pub(crate) fn admits_name(&self, name: &[u8]) -> Result<(), DriverError> {
        match self {
            Self::Zip(_) | Self::Tar(_) | Self::Rar(_) => Ok(()),
            Self::Lone(lone) => lone.admits_name(name),
            Self::Spark(_) => spark::admits_name(name),
            Self::ArcFs(_) => arcfs::admits_name(name),
        }
    }

    /// Whether the format keeps symbolic links.
    pub(crate) const fn keeps_links(&self) -> bool {
        matches!(self, Self::Zip(_) | Self::Tar(_) | Self::Rar(_))
    }

    /// The longest member the format can record: four gigabytes less a byte
    /// in an ARC header's sizes, two in the reach of an `ArcFS` data offset.
    pub(crate) const fn largest_member(&self) -> u64 {
        match self {
            Self::Zip(_) | Self::Tar(_) | Self::Lone(_) | Self::Rar(_) => u64::MAX,
            Self::Spark(_) => u32::MAX as u64,
            Self::ArcFs(_) => i32::MAX as u64,
        }
    }

    /// Whether members are made, removed and linked: a lone file's archive
    /// holds that one file and no more.
    pub(crate) const fn holds_tree(&self) -> bool {
        !matches!(self, Self::Lone(_))
    }

    /// What a member with `meta` keeps of its times; nothing of a root the
    /// format records nothing of.
    pub(crate) fn member_times(&self, meta: &Meta, root: bool) -> MemberTimes {
        if root && !self.keeps_root() {
            return MemberTimes::NONE;
        }
        match self {
            Self::Zip(_) => MemberTimes {
                modified: Some(ZIP_MODIFIED),
                accessed: None,
            },
            Self::Tar(_) => MemberTimes {
                modified: Some(EVERY_INSTANT),
                accessed: Some(EVERY_INSTANT),
            },
            Self::Lone(lone) => MemberTimes {
                modified: lone.modified_field(),
                accessed: None,
            },
            Self::Spark(_) => MemberTimes {
                modified: Some(riscos::stamp_field(meta).unwrap_or(DOS_TIME)),
                accessed: None,
            },
            Self::ArcFs(_) => MemberTimes {
                modified: riscos::stamp_field(meta),
                accessed: None,
            },
            Self::Rar(_) => MemberTimes::NONE,
        }
    }

    /// Whether a member's POSIX mode is kept.
    pub(crate) const fn keeps_modes(&self) -> bool {
        matches!(self, Self::Zip(_) | Self::Tar(_) | Self::Rar(_))
    }

    /// Whether a member's owner is kept.
    pub(crate) const fn keeps_owners(&self) -> bool {
        matches!(self, Self::Tar(_))
    }

    /// Whether a member's RISC OS fields are kept.
    pub(crate) const fn keeps_attributes(&self) -> bool {
        self.holds_tree()
    }

    /// The RISC OS attribute bits a member's record holds: a tar's records
    /// spell every one, the others' a `FileSwitch` byte beside the object's
    /// kind.
    pub(crate) const fn acorn_attr_bits(&self) -> u16 {
        match self {
            Self::Tar(_) => acorn::ATTR_BITS,
            _ => acorn::FILESWITCH_ATTR_BITS | acorn::ATTR_DIRECTORY,
        }
    }

    /// Whether the archive can record its root's own mode and owner: a tar
    /// does, as `./`.
    pub(crate) const fn keeps_root(&self) -> bool {
        matches!(self, Self::Tar(_))
    }

    /// Record the root in every next archive, where the format can.
    pub(crate) fn record_root(&mut self) {
        if let Self::Tar(tar) = self {
            tar.record_root();
        }
    }

    /// Write the next archive from `tree`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to write it, or a member this format cannot
    /// spell.
    pub(crate) fn write<A: VolumeAccess>(
        &mut self,
        tree: &Tree,
        regions: &Regions<A>,
        cursors: &mut Cursors,
    ) -> Result<Written, DriverError> {
        let mut sink = Sink::new(regions)?;
        let mut content = Content { regions, cursors };
        let (placed, stream) = match self {
            Self::Zip(zip) => (zip.write(tree, &mut content, &mut sink)?, None),
            Self::Tar(tar) => match tar.packing() {
                None => (tar.write(tree, &mut content, &mut sink)?, None),
                Some(packing) => {
                    let packing = packing.clone();
                    let mut out = Writer::new(&packing, &mut sink)?;
                    let placed = tar.write(tree, &mut content, &mut out)?;
                    (placed, Some(out.finish()?))
                }
            },
            Self::Lone(lone) => {
                let packing = lone.packing(tree)?;
                let mut out = Writer::new(&packing, &mut sink)?;
                let placed = lone::write(tree, &mut content, &mut out)?;
                (placed, Some(out.finish()?))
            }
            Self::Spark(spark) => (spark.write(tree, &mut content, &mut sink)?, None),
            Self::ArcFs(_) => (arcfs::write(tree, &mut content, &mut sink)?, None),
            // The format is never written; its refusal stops every change first.
            Self::Rar(_) => return Err(DriverError::Unsupported),
        };
        Ok(Written {
            len: sink.finish()?,
            placed,
            stream,
        })
    }
}

/// Whether `err`, met putting a member into the tree, leaves just that
/// member out — one no volume can serve: a path climbing out or too deep, a
/// name taken or unspellable, a kind no volume holds — rather than failing
/// the archive.
pub(crate) const fn leaves_out(err: DriverError) -> bool {
    matches!(
        err,
        DriverError::OutOfRange
            | DriverError::LengthOutOfRange
            | DriverError::AlreadyExists
            | DriverError::Unsupported
            | DriverError::NotFound
    )
}

/// Where a writer puts the next archive's bytes: into the next archive as
/// they are, or through the compressor of the one stream it is.
pub(crate) trait Out<A: VolumeAccess> {
    /// Where the next byte lands, in the archive as its format reads it.
    fn position(&self) -> u64;

    /// Append `bytes`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal; [`DriverError::OutOfMemory`].
    fn put(&mut self, bytes: &[u8]) -> Result<(), DriverError>;

    /// Append `len` bytes at `at` of `region`: the archive as its format
    /// reads it, or the staging file.
    ///
    /// # Errors
    ///
    /// The kernel's refusal; a read refusal of the archive.
    fn copy(
        &mut self,
        content: &mut Content<'_, A>,
        region: u8,
        at: u64,
        len: u64,
    ) -> Result<(), DriverError>;
}

impl<A: VolumeAccess> Out<A> for Sink<'_, A> {
    fn position(&self) -> u64 {
        Sink::position(self)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), DriverError> {
        Sink::put(self, bytes)
    }

    fn copy(
        &mut self,
        _content: &mut Content<'_, A>,
        region: u8,
        at: u64,
        len: u64,
    ) -> Result<(), DriverError> {
        Sink::copy(self, region, at, len)
    }
}

/// What a member's bytes are, for a writer: the spans the kernel can copy
/// where they are stored as they are, and a read of them otherwise.
pub(crate) struct Content<'w, A> {
    pub(crate) regions: &'w Regions<A>,
    pub(crate) cursors: &'w mut Cursors,
}

/// One span of a member's bytes, as a writer copies it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Span {
    /// `len` bytes at `at` of `region`, the archive as its format reads it
    /// or the staging file.
    In { region: u8, at: u64, len: u64 },
    /// `len` zeros.
    Zeros(u64),
}

impl<A: VolumeAccess> Content<'_, A> {
    /// The bytes of `data` as spans a writer copies, or `None` where they
    /// are compressed apart from the archive and must be decoded.
    pub(crate) fn spans(data: &Data) -> Option<Vec<Span>> {
        let mut spans = Vec::new();
        match data {
            Data::Archived(Archived {
                at,
                len,
                coding: Coding::Stored | Coding::Stream,
                ..
            }) => spans.push(Span::In {
                region: ARCHIVE_REGION,
                at: *at,
                len: *len,
            }),
            Data::Archived(_) => return None,
            Data::Staged(staged) => {
                let mut covered = 0;
                for (&start, run) in &staged.runs {
                    if start > covered {
                        spans.push(Span::Zeros(start - covered));
                    }
                    spans.push(Span::In {
                        region: ARCHIVE_STAGING,
                        at: run.at,
                        len: run.len,
                    });
                    covered = start + run.len;
                }
                if staged.len > covered {
                    spans.push(Span::Zeros(staged.len - covered));
                }
            }
        }
        Some(spans)
    }

    /// Copy `data`'s bytes into the next archive, through the kernel where
    /// they are stored as they are.
    pub(crate) fn copy_into<O: Out<A>>(
        &mut self,
        (id, content_gen): (u64, u64),
        data: &Data,
        out: &mut O,
    ) -> Result<(), DriverError> {
        if let Some(spans) = Self::spans(data) {
            for span in spans {
                match span {
                    Span::In { region, at, len } => out.copy(self, region, at, len)?,
                    Span::Zeros(len) => put_zeros(out, len)?,
                }
            }
            return Ok(());
        }
        let mut buf =
            tairix_util::fallible::filled(64 * 1024, 0u8).ok_or(DriverError::OutOfMemory)?;
        let mut offset = 0;
        while offset < data.len() {
            let got = self.read((id, content_gen), data, offset, &mut buf)?;
            if got == 0 {
                return Err(DriverError::MediumError);
            }
            out.put(&buf[..got])?;
            offset += got as u64;
        }
        Ok(())
    }

    /// Read up to `buf.len()` bytes at `offset` of `data`.
    pub(crate) fn read(
        &mut self,
        node: (u64, u64),
        data: &Data,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        match data {
            Data::Archived(archived) => {
                self.cursors.read(self.regions, node, archived, offset, buf)
            }
            Data::Staged(staged) => staged.read(self.regions, offset, buf),
        }
    }

    /// Read up to `buf.len()` bytes at `at` of `region`: the archive as its
    /// format reads it — decoded, where it is one stream — or the staging
    /// file.
    pub(crate) fn read_region(
        &mut self,
        region: u8,
        at: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        match (region, self.cursors.stream_mut()) {
            (ARCHIVE_REGION, Some(stream)) => stream.read(self.regions, at, buf),
            _ => self.regions.read(region, at, buf),
        }
    }
}

/// Append `len` zeros to `out`.
pub(crate) fn put_zeros<A: VolumeAccess, O: Out<A>>(
    out: &mut O,
    mut len: u64,
) -> Result<(), DriverError> {
    const ZEROS: [u8; 4096] = [0; 4096];
    while len > 0 {
        let step = usize::try_from(len).map_or(ZEROS.len(), |len| len.min(ZEROS.len()));
        out.put(&ZEROS[..step])?;
        len -= step as u64;
    }
    Ok(())
}
