//! A program's own compiled table, read from its system bundle's
//! `Resources/` through the kernel-authorised `fs_*` wrappers.
//!
//! The bundle is found through the one shared bundle-resolution policy with an
//! empty environment, as the program's own help is, so a user-writable
//! directory cannot answer for a system program's table. Reading adds no
//! authority: every check happens kernel-side under the caller's identity.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::fs::OpenFlags;
use tairix_abi::{BundleEntry, Errno};
use tairix_cmdres::{bundle_candidates, CommandEnv};

use crate::{DbKind, MAX_TABLE_BYTES, TABLE_SUFFIX};

/// Why a program's bundled table could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadError {
    /// No system bundle of the program's word holds the table.
    Missing,
    /// The table is there but could not be opened or read.
    Unreadable {
        /// The table's path.
        path: String,
        /// What the kernel answered.
        errno: Errno,
    },
    /// The file is larger than any table the generator emits.
    TooLarge {
        /// The table's path.
        path: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("the bundle carries no table"),
            Self::Unreadable { path, errno } => write!(f, "cannot read {path}: {errno}"),
            Self::TooLarge { path } => write!(f, "{path} exceeds the table size bound"),
        }
    }
}

/// The compiled `kind` table shipped in the system bundle of the program
/// named `word`, read whole.
///
/// # Errors
///
/// [`LoadError`]: the caller states it and goes on without names.
pub fn load(kind: DbKind, word: &str) -> Result<Vec<u8>, LoadError> {
    for bundle in bundle_candidates(word, CommandEnv::default()) {
        let path = format!(
            "{bundle}/{}/{}{TABLE_SUFFIX}",
            BundleEntry::Resources.as_str(),
            kind.name()
        );
        let unreadable = |path: String, ret: i64| LoadError::Unreadable {
            path,
            errno: Errno::from_syscall(ret),
        };
        let file = match tairix_rt::File::open(path.as_bytes(), OpenFlags::READ) {
            Ok(file) => file,
            Err(ret) if Errno::from_syscall(ret) == Errno::NotFound => continue,
            Err(ret) => return Err(unreadable(path, ret)),
        };
        let bytes = match tairix_rt::read_fd_to_end(file.fd(), MAX_TABLE_BYTES) {
            Ok(bytes) => bytes,
            Err(ret) => return Err(unreadable(path, ret)),
        };
        if bytes.len() > MAX_TABLE_BYTES {
            return Err(LoadError::TooLarge { path });
        }
        return Ok(bytes);
    }
    Err(LoadError::Missing)
}
