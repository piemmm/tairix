//! The seam through which `mount` reaches the kernel.
//!
//! Listing reuses the [`Transport`](tairix_procinfo::Transport) and
//! [`Output`](tairix_procinfo::Output) seams from `lib/procinfo`; only the
//! mount itself needs one of its own, so [`crate::client`] runs against an
//! in-memory fixture with no kernel.

use alloc::string::String;

use tairix_abi::imageattach::{ImageAttach, ImageAttachReply};
use tairix_abi::Errno;

/// Mounts disk images.
///
/// The kernel decides what may be mounted, for whom, and how; this seam only
/// carries the request there and its answer back.
pub trait Mounter {
    /// Mount the volume the disk image `image` holds as `attach` asks, the
    /// image opened read-write unless `attach` is read-only or the caller may
    /// only read it.
    ///
    /// # Errors
    ///
    /// The refusal to open `image`, or the kernel's to mount it: e.g.
    /// [`Errno::Busy`] while it is locked against the attach — any lock
    /// refuses a writable one — [`Errno::ReadOnly`] for `-w` of an image that
    /// will not write, [`Errno::LimitExceeded`] past the images a user may
    /// have mounted.
    fn attach(&self, image: &str, attach: &ImageAttach<'_>) -> Result<ImageAttachReply, Errno>;

    /// The existing directory `path` names, as an absolute path, a relative
    /// one read from the directory `mount` runs in.
    ///
    /// # Errors
    ///
    /// The refusal to resolve it, [`Errno::NotFound`] for a directory that
    /// does not exist among them.
    fn absolute(&self, path: &str) -> Result<String, Errno>;
}
