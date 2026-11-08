//! Why a PDF, or a part of one, could not be read.

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
