//! FileSwitch's own errors: those the filing vectors' defaults do not share
//! with it, whose texts its own message file holds. Each number is genuine
//! RISC OS 5's where it was recorded giving it.

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{Fail, Os};

use super::own;

/// "Bad name".
pub(crate) const BAD_NAME: Error = Error {
    number: 0xCC,
    token: "BadName",
};
/// "Filing system or path %0: not present".
pub(crate) const NO_PREFIX: Error = Error {
    number: 0xF8,
    token: "PrefixNotPresent",
};
/// "End of file".
pub(crate) const END_OF_FILE: Error = Error {
    number: 0xDF,
    token: "EndOfFile",
};
/// "File not open for update".
pub const NOT_FOR_UPDATE: Error = Error {
    number: 0xC1,
    token: "NotOpenForUpdate",
};
/// A read from a stream its filing system opened without read access.
pub(crate) const NOT_FOR_READING: Error = Error {
    number: 0x413,
    token: "NotOpenForReading",
};
/// "Unable to open '%0'": a filing system answered an open with no handle.
pub(crate) const CANT_OPEN: Error = Error {
    number: 0xD6,
    token: "CantOpen",
};
/// "%0 must be given a file name": an empty name to a filing system that
/// takes none.
pub(crate) const NEEDS_NAME: Error = Error {
    number: 0xCC,
    token: "NeedsFileName",
};
/// "Too many open files".
pub(crate) const TOO_MANY_OPEN: Error = Error {
    number: 0xC0,
    token: "TooManyOpenFiles",
};
/// "Outside file".
pub(crate) const OUTSIDE: Error = Error {
    number: 0xB7,
    token: "OutsideFile",
};
/// "'%0' exists, and is not what was asked for": a save or an empty file
/// made over a directory, a directory made over a file.
pub const WRONG_KIND: Error = Error {
    number: 0xAF,
    token: "WrongKindExists",
};
/// "Nothing matches %0": a wipe of a wildcard whose directory lists nothing.
pub const NOTHING_MATCHED: Error = Error {
    number: 0x416,
    token: "NothingMatched",
};
/// A wildcarded leaf naming what `OS_File` is to make or delete, refused
/// before any filing system is asked.
pub const WILDCARDS: Error = Error {
    number: 0xFD,
    token: "Wildcards",
};
/// `OS_FSControl` 3, which RISC OS 5 refuses as reserved.
pub(crate) const RESERVED: Error = Error {
    number: 0x409,
    token: "ReservedReason",
};
/// A special field given to a filing system that takes none.
pub(crate) const NO_SPECIAL_FIELDS: Error = Error {
    number: 0x40F,
    token: "NoSpecialFields",
};
/// A filing system opened a file with a buffer size the protocol does not
/// allow, a condition RISC OS was never seen to report.
pub(crate) const BAD_BUFFER: Error = Error {
    number: 0xC8,
    token: "BadBufferSize",
};

/// `error` from FileSwitch's own file, `%0` onwards `parameters`.
pub(crate) fn fail(os: &mut Os<'_>, error: Error, parameters: &[&[u8]]) -> Fail {
    os.own_error(own, error, parameters)
}

/// "Bad name".
pub(crate) fn bad_name(os: &mut Os<'_>) -> Fail {
    fail(os, BAD_NAME, &[])
}

/// "Filing system or path `prefix`: not present".
pub(crate) fn no_prefix(os: &mut Os<'_>, prefix: &[u8]) -> Fail {
    fail(os, NO_PREFIX, &[prefix])
}

/// An empty name refused by the filing system numbered `number`, named
/// with its colon.
pub(crate) fn needs_name(os: &mut Os<'_>, number: u8) -> Fail {
    let name = super::with(os, |state| {
        let mut named = state.system(number).map(|system| system.name.clone())?;
        tairix_util::fallible::push(&mut named, b':').then_some(named)
    });
    match name {
        Ok(Some(name)) => fail(os, NEEDS_NAME, &[&name]),
        Ok(None) => os.no_memory(),
        Err(fail) => fail,
    }
}
