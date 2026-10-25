//! The one file copy every copying tool runs (`plans/ARXFS-METADATA.md`
//! §6): a file's bytes with its holes ([`DataCopy`]), its extended
//! attributes as far as the destination can keep them
//! ([`copy_attributes`]), one report of what could not be carried
//! ([`Omissions`]), the modes what a copy makes is given ([`file_mode`],
//! [`DirModes`]) and the mode and owner a move carries ([`carry_owner`]),
//! and the one resumable copy, move and removal of a tree built from them
//! ([`op`]). It reaches the filesystem only through held nodes: the
//! shared tree seam ([`tree`]) a tool walks a tree through, and
//! the [`Source`] and [`Target`] data seams a held file is read and written
//! through, so a host test runs it against the `fixture` tree and each tool
//! against its own I/O. The running system's are `tairix_rt::copy`. A tool
//! copying a tree asks [`landing`] whether it would write inside the source
//! before it writes, and walks under a [`Descent`], so no copy reproduces
//! itself.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod attrs;
mod created;
mod data;
mod mode;
pub mod op;
pub mod tree;
mod within;

#[cfg(feature = "test-support")]
pub mod fixture;

pub use attrs::{copy_attributes, AttrError, Attributes, Omissions, Refusal, Report};
pub use created::Created;
pub use data::{DataCopy, DataError, Source, Sparse, Step, Target, ZERO_UNIT};
pub use mode::{carry_owner, file_mode, DirModes, Ownership, Settle, OWNER_RW, OWNER_RWX};
pub use within::{landing, lands_inside, Descent, Landing, Unresolved};

/// Bytes a copy moves through one buffer at a time: as much as one read or
/// write may carry, so a large file costs the fewest calls.
pub const CHUNK_LEN: usize = tairix_abi::fs::FS_IO_MAX;
