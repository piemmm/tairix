//! The one file copy every copying tool runs (`plans/ARXFS-METADATA.md`
//! §6): a file's bytes with its holes ([`DataCopy`]), its extended
//! attributes as far as the destination can keep them
//! ([`copy_attributes`]), and one report of what could not be carried
//! ([`Omissions`]). The engine reaches the filesystem only through the
//! [`Source`], [`Target`] and [`AttributeStore`] seams, so a host test runs
//! it against fixtures and each tool against its own I/O; a tool reaching
//! files by path offers them as [`PathFiles`], each one a [`PathFile`]. The
//! running system's are `tairix_rt::copy`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod attrs;
mod data;
mod files;

pub use attrs::{
    copy_attributes, AttrError, AttributeStore, Attributes, Omissions, Refusal, Report,
};
pub use data::{DataCopy, DataError, Source, Sparse, Step, Target, ZERO_UNIT};
pub use files::{PathFile, PathFiles};

/// Bytes a copy moves through one buffer at a time: as much as one read or
/// write may carry, so a large file costs the fewest calls.
pub const CHUNK_LEN: usize = tairix_abi::fs::FS_IO_MAX;
