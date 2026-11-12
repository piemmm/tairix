//! How a file handed to a diskette drive goes in: an ImageDisk archive by its
//! magic, read whole by the machine, or a raw image of a standard diskette's
//! size, its sectors in order — read-only unless it was handed over writable.

use crate::link::{Form, MAX_ARCHIVE};
use crate::storage::floppy::Format;
use crate::storage::imd::MAGIC;

/// Bytes of a file's head [`form_of`] looks at.
pub const HEAD: usize = MAGIC.len();

/// Why a file goes into no diskette drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfit {
    /// An archive larger than any diskette's.
    TooLarge,
    /// Neither an archive nor the size of any diskette.
    NotADiskette,
}

/// How a file of `len` bytes beginning `head` goes in, writable where
/// `writable`.
///
/// # Errors
///
/// [`Unfit`], why it goes in at all.
pub fn form_of(len: u64, head: &[u8], writable: bool) -> Result<Form, Unfit> {
    if head.starts_with(MAGIC) {
        return u32::try_from(len)
            .ok()
            .filter(|&bytes| bytes <= MAX_ARCHIVE)
            .map(|bytes| Form::Archive { bytes })
            .ok_or(Unfit::TooLarge);
    }
    let format = Format::of_size(len).ok_or(Unfit::NotADiskette)?;
    Ok(Form::Sectors {
        count: format.bytes() / crate::storage::SECTOR as u64,
        read_only: !writable,
    })
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
