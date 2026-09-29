//! Handing a document to the application chosen to open it: how it is
//! opened, how a fresh launch is told it was handed one, and how far a
//! delegation of it reaches.
//!
//! One rule for every launcher — the file manager, the desktop — so an
//! application is handed the same authority whichever surface the user
//! opened the document from.

#[cfg(any(feature = "rt", test))]
use tairix_abi::fs::OpenFlags;
#[cfg(any(feature = "rt", test))]
use tairix_abi::Errno;
use tairix_abi::{DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, GRANT_EXTENT_INHERIT};

/// The argument that tells a fresh launch it was handed a document on its
/// standard input, and whether that document is open read-write.
#[must_use]
pub const fn role_arg(writable: bool) -> &'static [u8] {
    if writable {
        DOCUMENT_WRITABLE_ROLE_ARG
    } else {
        DOCUMENT_ROLE_ARG
    }
}

/// The `fd_grant` ceiling a document is delegated with: none for a read-only
/// one, and the grantor's own reach for one open read-write.
#[must_use]
pub const fn grant_ceiling(writable: bool) -> u64 {
    if writable {
        GRANT_EXTENT_INHERIT
    } else {
        0
    }
}

/// A document opened for an application: the launcher's own handle on it,
/// and whether it is open read-write.
#[cfg(feature = "rt")]
#[derive(Debug)]
pub struct Opened {
    /// The launcher's handle, closed when this is dropped — once the
    /// document has been handed on.
    pub file: tairix_rt::File,
    /// Whether it is open read-write.
    pub writable: bool,
}

/// Open the document at `path` for an application that `edits` documents or
/// does not: read-write where the user's own authority allows it and the
/// application edits, read-only otherwise.
///
/// # Errors
///
/// The kernel's refusal to open the document even read-only.
#[cfg(feature = "rt")]
pub fn open_for(path: &[u8], edits: bool) -> Result<Opened, tairix_abi::Errno> {
    let (file, writable) = open_with(edits, |flags| {
        tairix_rt::File::open(path, flags).map_err(Errno::from_syscall)
    })?;
    Ok(Opened { file, writable })
}

/// `open_for`'s rule over `open`: the handle, and whether it is writable.
#[cfg(any(feature = "rt", test))]
fn open_with<F>(
    edits: bool,
    mut open: impl FnMut(OpenFlags) -> Result<F, Errno>,
) -> Result<(F, bool), Errno> {
    if edits {
        match open(OpenFlags::READ.union(OpenFlags::WRITE)) {
            Ok(file) => return Ok((file, true)),
            // A document the user may only read is still theirs to read.
            Err(Errno::PermissionDenied) => {}
            Err(err) => return Err(err),
        }
    }
    Ok((open(OpenFlags::READ)?, false))
}

#[cfg(test)]
mod tests {
    use super::{grant_ceiling, open_with, role_arg};
    use alloc::vec::Vec;
    use tairix_abi::fs::OpenFlags;
    use tairix_abi::{Errno, DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, GRANT_EXTENT_INHERIT};

    /// Open under `refusal` for a read-write ask, recording each ask's flags.
    fn opened(edits: bool, refusal: Option<Errno>) -> (Result<bool, Errno>, Vec<OpenFlags>) {
        let mut asked = Vec::new();
        let opened = open_with(edits, |flags| {
            asked.push(flags);
            match refusal {
                Some(err) if flags.contains(OpenFlags::WRITE) => Err(err),
                _ => Ok(()),
            }
        });
        (opened.map(|((), writable)| writable), asked)
    }

    #[test]
    fn an_editor_is_handed_a_document_read_write_where_the_user_may_write_it() {
        let (writable, asked) = opened(true, None);
        assert_eq!(writable, Ok(true));
        assert_eq!(asked, [OpenFlags::READ.union(OpenFlags::WRITE)]);
    }

    #[test]
    fn a_document_the_user_may_only_read_is_handed_over_read_only() {
        let (writable, asked) = opened(true, Some(Errno::PermissionDenied));
        assert_eq!(writable, Ok(false));
        assert_eq!(
            asked,
            [OpenFlags::READ.union(OpenFlags::WRITE), OpenFlags::READ]
        );
    }

    #[test]
    fn any_other_refusal_ends_the_open_and_a_viewer_never_asks_to_write() {
        let (writable, asked) = opened(true, Some(Errno::NotFound));
        assert_eq!(writable, Err(Errno::NotFound));
        assert_eq!(asked.len(), 1, "no read-only retry for a missing document");
        let (writable, asked) = opened(false, Some(Errno::PermissionDenied));
        assert_eq!(writable, Ok(false));
        assert_eq!(asked, [OpenFlags::READ]);
    }

    #[test]
    fn a_read_only_document_is_announced_and_delegated_as_one() {
        assert_eq!(role_arg(false), DOCUMENT_ROLE_ARG);
        assert_eq!(
            grant_ceiling(false),
            0,
            "the kernel refuses a ceiling on a read-only descriptor"
        );
    }

    #[test]
    fn a_writable_document_passes_on_its_grantor_s_reach() {
        assert_eq!(role_arg(true), DOCUMENT_WRITABLE_ROLE_ARG);
        assert_eq!(grant_ceiling(true), GRANT_EXTENT_INHERIT);
    }
}
