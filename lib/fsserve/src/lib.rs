//! The server side of `fsserve-v1` (`plans/FSSERVE.md` §2): what a
//! user-space filesystem driver links to serve its volume to the kernel.
//!
//! [`Server`] answers one call at a time: it decodes the kernel's request,
//! runs it against the driver through the filesystem driver traits, and
//! encodes the reply, so every format's driver shares one dispatch and none
//! re-derives the protocol. [`ServedHost`] is the
//! [`VolumeHost`](tairix_abi::driver::filesystem::VolumeHost) the driver
//! reports its commit deadline and its reserve to, and what each reply's
//! [`ServerState`](tairix_abi::fsserve::ServerState) is read from.
//!
//! A format's driver crate supplies only how it opens a volume
//! ([`ServedFormat`]); the volume reaches it as a [`GrantedVolume`] over the
//! kernel's calls on its grant, and the `program` feature's `program::run`
//! is the whole driver process every format shares.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

extern crate alloc;

mod format;
mod host;
mod server;
mod volume;

pub use format::{bind_key, Opened, ServedFormat, WritebackBudget, REQUIRED_CAPABILITIES};
pub use host::{Clock, ServedHost};
pub use server::{ServedFilesystem, Server};
pub use volume::{GrantedVolume, VolumeAccess};

#[cfg(all(freestanding, feature = "program"))]
pub mod program;

#[cfg(test)]
mod testfs;
