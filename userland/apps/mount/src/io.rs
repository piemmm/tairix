//! The seam through which `mount` reaches the kernel.
//!
//! Listing reuses the [`Transport`](tairix_procinfo::Transport) and
//! [`Output`](tairix_procinfo::Output) seams from `lib/procinfo`; the mount
//! and the answers for held writes need one of their own, so
//! [`crate::client`] runs against an in-memory fixture with no kernel.

use tairix_abi::imageattach::{ImageAttach, ImageAttachReply};
use tairix_abi::volume::VolumeReplayRequest;
use tairix_abi::Errno;
use tairix_procinfo::Caller;

/// Mounts disk images, and answers for the writes a volume holds.
///
/// The kernel decides what may be mounted and who may answer, for whom, and
/// how; this seam only carries each request there and its answer back. As a
/// [`Caller`] it says who runs the tool and where a path it names leads: a
/// mount's target, and the volume an answer names.
pub trait Mounter: Caller {
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

    /// Carry `request` to the kernel: the writes the volume it names holds
    /// written to its medium, or given up.
    ///
    /// # Errors
    ///
    /// The kernel's refusal: e.g. [`Errno::PermissionDenied`] for a caller
    /// whose authority does not reach the volume, [`Errno::Stale`] for one no
    /// longer awaiting an answer, [`Errno::ReadOnly`] for writes kept because
    /// the medium no longer proves itself unmutated.
    fn replay(&self, request: VolumeReplayRequest) -> Result<(), Errno>;
}
