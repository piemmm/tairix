//! Why a PDF, or a part of one, could not be read.

use alloc::vec::Vec;
use core::fmt;

/// Why a PDF, or a part of one, could not be read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PdfError {
    /// It is not a PDF, or its structure is broken past repair.
    Malformed,
    /// It holds something this reader does not read.
    Unsupported,
    /// It is encrypted, and the empty password does not open it.
    NeedsPassword,
    /// The password given does not open it.
    WrongPassword,
    /// It exceeds a containment bound.
    TooComplex,
    /// The memory to read it could not be had.
    OutOfMemory,
    /// The page asked for is not one it has.
    NoSuchPage,
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "it is not a PDF, or it is damaged past repair",
            Self::Unsupported => "it holds something this reader does not read",
            Self::NeedsPassword => "it is protected by a password",
            Self::WrongPassword => "the password does not open it",
            Self::TooComplex => "it is larger or deeper than a PDF may be here",
            Self::OutOfMemory => "there is not enough memory to read it",
            Self::NoSuchPage => "it has no such page",
        })
    }
}

/// A file a document was not opened from, handed back with why, so it may be
/// tried again — with another password — at no copy.
pub struct Refused<E = PdfError> {
    /// Why it was refused.
    pub error: E,
    /// The file, as it was handed over.
    pub file: Vec<u8>,
}

impl<E> Refused<E> {
    /// The same file refused for `error`'s reason in another vocabulary.
    pub fn map<F>(self, error: impl FnOnce(E) -> F) -> Refused<F> {
        Refused {
            error: error(self.error),
            file: self.file,
        }
    }
}

impl From<Refused> for PdfError {
    /// Why, the file let go.
    fn from(refused: Refused) -> Self {
        refused.error
    }
}

impl<E: fmt::Debug> fmt::Debug for Refused<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Refused")
            .field("error", &self.error)
            .field("file", &format_args!("{} bytes", self.file.len()))
            .finish()
    }
}

/// `read`'s value, or `None` where what was read was damaged: only the heap's
/// refusal and a containment bound are errors.
pub(crate) fn tolerated<T>(read: Result<T, PdfError>) -> Result<Option<T>, PdfError> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(error @ (PdfError::OutOfMemory | PdfError::TooComplex)) => Err(error),
        Err(_) => Ok(None),
    }
}

impl From<alloc::collections::TryReserveError> for PdfError {
    fn from(_: alloc::collections::TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

impl From<tairix_collections::TryReserveError> for PdfError {
    fn from(error: tairix_collections::TryReserveError) -> Self {
        match error {
            tairix_collections::TryReserveError::AllocFailed => Self::OutOfMemory,
            tairix_collections::TryReserveError::CapacityOverflow => Self::TooComplex,
        }
    }
}
