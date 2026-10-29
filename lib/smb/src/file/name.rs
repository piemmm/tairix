//! Names and paths as both TAIRiX and SMB carry them (MS-FSCC §2.1.5).
//!
//! A name crosses, in either direction, only when it is a TAIRiX component
//! and an SMB filename at once: at most 255 bytes of UTF-8, which is never
//! more than SMB's 255 UTF-16 units, no dot name, and none of the characters
//! either side reserves. So a stranger's listing can never smuggle a
//! separator, a stream name or a wildcard into a path, and no TAIRiX name
//! opens a different object on the server than the one it names.

use alloc::string::String;

use crate::wire::{from_utf16le, utf16le, Encoder};
use crate::SmbError;

/// The longest path a request carries, in UTF-16 units.
pub const MAX_PATH_UNITS: usize = 32_760;

/// Whether `c` is one an SMB filename may not hold.
const fn reserved(c: char) -> bool {
    matches!(c, '"' | '\\' | '/' | ':' | '|' | '<' | '>' | '*' | '?') || c < ' '
}

/// Whether `name` is one both TAIRiX and SMB carry.
#[must_use]
pub fn is_name(name: &str) -> bool {
    tairix_abi::fs::is_component(name.as_bytes()) && !name.chars().any(reserved)
}

/// The name in UTF-16LE `bytes`, as a server spelled it, once both sides can
/// carry it.
///
/// # Errors
///
/// [`SmbError::Malformed`] for an odd length; [`SmbError::Name`] for a name
/// that does not round-trip through UTF-16 or that either side refuses;
/// [`SmbError::OutOfMemory`].
pub fn decode_name(bytes: &[u8]) -> Result<String, SmbError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(SmbError::Malformed);
    }
    let name = from_utf16le(bytes).map_err(|err| match err {
        SmbError::Malformed => SmbError::Name,
        other => other,
    })?;
    if is_name(&name) {
        Ok(name)
    } else {
        Err(SmbError::Name)
    }
}

/// A path relative to a share's root, spelled with `\` between its names as
/// SMB spells it. The share's root is the empty path.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct SharePath {
    text: String,
    units: usize,
}

impl SharePath {
    /// The share's root.
    #[must_use]
    pub const fn root() -> Self {
        Self {
            text: String::new(),
            units: 0,
        }
    }

    /// Whether this is the share's root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.text.is_empty()
    }

    /// The path, `\`-separated, as UTF-8.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Its names, outermost first.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.text.split('\\').filter(|name| !name.is_empty())
    }

    /// This path with `name` beneath it.
    ///
    /// # Errors
    ///
    /// [`SmbError::Name`] for a name either side refuses or a path past
    /// [`MAX_PATH_UNITS`]; [`SmbError::OutOfMemory`].
    pub fn join(&self, name: &str) -> Result<Self, SmbError> {
        if !is_name(name) {
            return Err(SmbError::Name);
        }
        let separator = usize::from(!self.is_root());
        let units = self.units + separator + name.encode_utf16().count();
        if units > MAX_PATH_UNITS {
            return Err(SmbError::Name);
        }
        let mut text = String::new();
        text.try_reserve_exact(self.text.len() + separator + name.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        text.push_str(&self.text);
        if separator == 1 {
            text.push('\\');
        }
        text.push_str(name);
        Ok(Self { text, units })
    }

    /// The path `text` spells: names a single `\` apart, no leading or
    /// trailing one, each a name both sides carry; empty for the root.
    ///
    /// # Errors
    ///
    /// [`SmbError::Name`] for an empty name between separators, a name either
    /// side refuses, or a path past [`MAX_PATH_UNITS`];
    /// [`SmbError::OutOfMemory`].
    pub fn parse(text: &str) -> Result<Self, SmbError> {
        if !text.is_empty() && !text.split('\\').all(is_name) {
            return Err(SmbError::Name);
        }
        let units = text.encode_utf16().count();
        if units > MAX_PATH_UNITS {
            return Err(SmbError::Name);
        }
        let mut held = String::new();
        held.try_reserve_exact(text.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        held.push_str(text);
        Ok(Self { text: held, units })
    }

    /// The path a server spelled in UTF-16LE `bytes`: names a single `\`
    /// apart, no leading or trailing one, each a name both sides carry.
    ///
    /// # Errors
    ///
    /// As [`decode_name`], and [`SmbError::Name`] for an empty name between
    /// separators or a path past [`MAX_PATH_UNITS`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SmbError> {
        if !bytes.len().is_multiple_of(2) {
            return Err(SmbError::Malformed);
        }
        let units = bytes.len() / 2;
        if units > MAX_PATH_UNITS {
            return Err(SmbError::Name);
        }
        let text = from_utf16le(bytes).map_err(|err| match err {
            SmbError::Malformed => SmbError::Name,
            other => other,
        })?;
        if !text.is_empty() && !text.split('\\').all(is_name) {
            return Err(SmbError::Name);
        }
        Ok(Self { text, units })
    }

    /// Its length in UTF-16LE bytes.
    #[must_use]
    pub const fn utf16_len(&self) -> usize {
        self.units * 2
    }

    /// Append it as UTF-16LE.
    pub fn encode(&self, enc: &mut Encoder) {
        utf16le(&self.text, enc);
    }
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
