//! The **permission-edit** model (`plans/NEW-FILEMANAGER.md` FM8b): the pure,
//! host-tested core of committing a new permission mode to the selected node.
//!
//! Changing a node's permissions is modelled here so the one rule that decides
//! whether a requested mode word is acceptable — it may carry only the bits
//! [`fs_set_mode`](tairix_abi::SyscallNumber::FS_SET_MODE) itself accepts,
//! the [`FS_MODE_MASK`] `rwx`/setuid/setgid/sticky
//! bits — runs in `cargo test` with no kernel. The app validates a word with
//! [`validate_mode`] before it asks for the `fs_set_mode` call.
//!
//! Authority is unchanged: the change is an ordinary permission-checked VFS
//! call under the caller's own identity (no new capability), so the engine
//! adds nothing — the trusted picker composes the same [`Browser`](crate::Browser)
//! and simply never calls the write path. Validation is *spelling only*: a mode
//! this module accepts may still be refused by the VFS (the user does not own
//! the node, a read-only mount, a lost race), with the kernel's own `Errno`,
//! which the app words. It never silently masks an out-of-range request into a
//! different mode — an unacceptable word is refused honestly, so the mode
//! applied is always exactly the one asked for.

use tairix_abi::fs::FS_MODE_MASK;

/// A requested mode carrying a bit above [`FS_MODE_MASK`] — a file-type or
/// other bit that is not part of the settable permission word. Decided before
/// any syscall, so nothing is changed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InvalidMode;

impl InvalidMode {
    /// A terse, human-readable reason for the in-UI refusal line. It names no
    /// path and carries no secret.
    #[must_use]
    pub const fn message(self) -> &'static str {
        "Those permission bits are not allowed."
    }
}

/// Validate `mode` as a settable permission word.
///
/// Pure and fail-closed: a word carrying any bit above
/// [`FS_MODE_MASK`] is refused with
/// [`InvalidMode`] rather than masked, so the mode a caller commits is
/// always exactly the one it asked for — the same rule the `fs_set_mode`
/// dispatcher enforces, checked here before the syscall. It performs no I/O
/// and makes no permission decision — that is the VFS's, at commit time.
///
/// # Errors
///
/// [`InvalidMode`] if `mode` carries a bit outside [`FS_MODE_MASK`].
pub const fn validate_mode(mode: u32) -> Result<(), InvalidMode> {
    if mode & !FS_MODE_MASK != 0 {
        return Err(InvalidMode);
    }
    Ok(())
}
