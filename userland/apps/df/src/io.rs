//! The seams through which `df` touches the outside world.
//!
//! The namespace itself arrives through the shared
//! [`Transport`](tairix_procinfo::Transport) seam (the `sysinfo-v1`
//! `MOUNT_LIST` and `VIEW_LIST` walks `mount` uses too); only the operand
//! probe and
//! the output streams need seams of their own. Keeping them behind
//! object-safe traits lets the report logic in [`crate::client`] run
//! against in-memory fixtures with no kernel, mirroring the seam
//! discipline of the other userland tools (`ps`'s `Transport`, `du`'s
//! `Walk`).

use tairix_abi::Errno;

/// Verifies that a `file` operand names an existing filesystem node.
///
/// `df <file>` reports the filesystem *containing* `file`, so the operand
/// must exist before its covering mount is chosen; a missing or
/// unreachable operand is diagnosed exactly as GNU `df` does. The
/// implementation performs no authorisation of its own: the secured VFS
/// checks the path per-inode under the caller's attested identity.
pub trait PathProbe {
    /// Confirm `path` resolves to an existing node.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::NotFound`] for
    /// a missing path or [`Errno::PermissionDenied`] when the caller may
    /// not reach it.
    fn probe(&self, path: &str) -> Result<(), Errno>;
}

pub use tairix_util::output::Output;
