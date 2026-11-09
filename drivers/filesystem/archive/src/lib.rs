//! The archive filesystem driver (`plans/ARCHIVES.md`): an archive file's
//! members served as a volume — zip, tar (plain, gzip'd or bzip2'd), a lone
//! gzip or bzip2 stream, Spark and `ArcFS` read and written, RAR read — each
//! member's RISC OS fields carried as its `acorn.*` attributes.
//!
//! An archive is a stranger's bytes, parsed only here, in a capability-empty
//! process holding nothing but its own attachment: the archive, read by
//! byte, and for a writable one its two work files. Every count, length,
//! offset and name the archive states is checked before it is used, a member
//! that cannot be believed is left out, and an archive refused whole only
//! where nothing in it can be.
//!
//! The archive is never written in place. A change waits in the staging
//! file, and a commit writes the next archive into the other work file — the
//! bytes of a member kept unchanged copied there by the kernel — which the
//! kernel then makes the archive, so a crash at any point leaves the old
//! archive or the new one whole.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

extern crate alloc;

mod arcfs;
mod data;
mod format;
mod fs;
mod lone;
mod name;
mod rar;
mod region;
mod riscos;
mod spark;
mod stream;
mod tar;
mod tree;
mod wire;
mod zip;

#[cfg(test)]
mod test_support;

pub use fs::ArchiveFs;

use tairix_abi::driver::filesystem::WriteRefusal;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsserve::{GrantedVolume, Opened, ServedArchive, VolumeAccess, WritebackBudget};

use crate::format::Format;
use crate::region::Regions;

/// The nodes this driver binds: a volume the kernel attached as each
/// archive format it reads.
pub const BIND_KEYS: &[DriverBindKey] = &[
    tairix_fsserve::bind_key(VolumeFsType::Zip),
    tairix_fsserve::bind_key(VolumeFsType::Tar),
    tairix_fsserve::bind_key(VolumeFsType::Gzip),
    tairix_fsserve::bind_key(VolumeFsType::Bzip2),
    tairix_fsserve::bind_key(VolumeFsType::Spark),
    tairix_fsserve::bind_key(VolumeFsType::ArcFs),
    tairix_fsserve::bind_key(VolumeFsType::Rar),
];

/// Archives, served.
pub struct Served;

impl ServedArchive for Served {
    type Fs<A: VolumeAccess> = ArchiveFs<A>;

    fn open<A: VolumeAccess>(
        mut volume: GrantedVolume<A>,
        read_only: bool,
        _budget: WritebackBudget,
        _log: &'static (dyn tairix_fsserve::Sink + Sync),
    ) -> Result<Opened<ArchiveFs<A>>, DriverError> {
        let info = *volume.info();
        // The archive is recognised again as the kernel recognised it, by the
        // same readers, so the two agree on what it is.
        let fstype = {
            let mut dev = ByteDevice::new(&mut volume)?;
            tairix_fsprobe::archive::recognise(&mut dev, info.byte_len)?
                .ok_or(DriverError::BadMagic)?
                .fstype
        };
        let regions = Regions::new(volume.into_access(), info.byte_len);
        let read = Format::read(fstype, &regions, info.archive_name.as_ref())?;
        // A format written by nothing says so, whatever the attachment allows.
        let refusal = if read_only || !fs::writable_attachment(info.work_files) {
            Some(
                read.refusal
                    .filter(|&why| why == WriteRefusal::FormatReadOnly)
                    .unwrap_or(WriteRefusal::OpenedReadOnly),
            )
        } else {
            read.refusal
        };
        let fs = ArchiveFs::new(
            regions,
            read.format,
            read.tree,
            (read.stream, read.rar),
            refusal,
        );
        let format = fs.format_name();
        let write_refusal = if read_only { None } else { fs.write_refusal() };
        Ok(Opened {
            fs,
            format,
            write_refusal,
            title_apart: false,
        })
    }
}
