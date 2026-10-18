//! The zone store a process reads once and keeps for its life.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::Errno;
use tairix_rt::File;

use crate::layout::MAX_STORE_LEN;
use crate::store::{StoreError, ZoneInfo};

/// Why the store could not be had.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoadError {
    /// It could not be read.
    Unreadable(Errno),
    /// It was read and refused.
    Refused(StoreError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(errno) => write!(f, "the zone store could not be read ({errno})"),
            Self::Refused(error) => write!(f, "the zone store was refused: {error}"),
        }
    }
}

/// The store `file` holds, read whole and validated, and kept for the rest of
/// the process: every zone and clock built from it borrows it, so a process
/// loads it once.
///
/// `file` is the store at [`STORE_PATH`](crate::STORE_PATH), opened by a
/// process that may, or delegated to one that may not.
///
/// # Errors
///
/// [`LoadError`]; the caller states it and reads UTC.
pub fn load(file: &File) -> Result<&'static ZoneInfo<'static>, LoadError> {
    let bytes = tairix_rt::read_fd_to_end(file.fd(), MAX_STORE_LEN)
        .map_err(|raw| LoadError::Unreadable(Errno::from_syscall(raw)))?;
    // Validated before it is kept, so a refused store costs nothing for the
    // life of the process; the bytes kept are the ones validated, moved.
    let validated = ZoneInfo::parse(&bytes)
        .map_err(LoadError::Refused)?
        .validated();
    let kept: &'static [u8] = Vec::leak(bytes);
    let info =
        ZoneInfo::from_validated(validated, kept).ok_or(LoadError::Refused(StoreError::Length))?;
    Ok(Box::leak(Box::new(info)))
}
