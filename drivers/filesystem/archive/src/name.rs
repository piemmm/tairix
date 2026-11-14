//! Member names (`plans/ARCHIVES.md` §1): an archive's spelling of a path cut
//! into the names a volume serves, and a served name spelled back.
//!
//! A served name is UTF-8 and one directory entry's. An archive spells its
//! names in its own character set — zip in IBM code page 437 unless a member
//! says UTF-8, RISC OS in ISO 8859-1 — and converted names are read, written
//! back and compared in UTF-8; a member kept unchanged keeps the bytes it
//! had.

use alloc::vec::Vec;

use tairix_abi::fs::is_component;
use tairix_fsmeta::preset::acorn;

use crate::tree::MAX_DEPTH;

/// A character set an archive spells names in. RISC OS's names cross through
/// [`from_riscos`] and [`to_riscos`] instead, since a RISC OS leaf is more
/// than its characters.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Charset {
    /// UTF-8, as written.
    Utf8,
    /// IBM code page 437, a zip member's unless it says otherwise.
    Cp437,
    /// ISO 8859-1.
    Latin1,
}

impl Charset {
    /// `bytes` in UTF-8, or `None` for bytes that are not text in this
    /// character set.
    pub(crate) fn decode(self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        out.try_reserve(bytes.len().saturating_mul(2)).ok()?;
        match self {
            Self::Utf8 => {
                core::str::from_utf8(bytes).ok()?;
                out.extend_from_slice(bytes);
            }
            Self::Cp437 | Self::Latin1 => {
                for &byte in bytes {
                    let ch = match self {
                        Self::Latin1 => char::from(byte),
                        _ => tairix_cp437::char_of(byte),
                    };
                    let mut utf8 = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                }
            }
        }
        Some(out)
    }

    /// The UTF-8 `name` spelled in this character set, or `None` for a name
    /// holding a character it has no byte for.
    pub(crate) fn encode(self, name: &[u8]) -> Option<Vec<u8>> {
        let text = core::str::from_utf8(name).ok()?;
        let mut out = Vec::new();
        out.try_reserve(name.len()).ok()?;
        match self {
            Self::Utf8 => out.extend_from_slice(name),
            Self::Latin1 => {
                for ch in text.chars() {
                    out.push(u8::try_from(u32::from(ch)).ok()?);
                }
            }
            Self::Cp437 => {
                for ch in text.chars() {
                    out.push(tairix_cp437::byte_of(ch)?);
                }
            }
        }
        Some(out)
    }
}

/// Whether `name` is one a volume serves: one directory entry's, in UTF-8.
pub(crate) fn servable(name: &[u8]) -> bool {
    is_component(name) && core::str::from_utf8(name).is_ok()
}

/// What a member path names, cut at `separator`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Spelled {
    /// The components of a path within the volume: a leading separator, an
    /// empty component and `.` name nothing and are passed over.
    Within(Vec<Vec<u8>>),
    /// A path naming the archive's own root.
    Root,
    /// A path that climbs out of the volume or holds a component no volume
    /// serves; its member is left out.
    Refused,
}

/// The UTF-8 member path `path`, cut at `separator`; refused past the depth
/// the tree nests to, before a component past it is made.
pub(crate) fn spelled(path: &[u8], separator: u8) -> Spelled {
    let mut parts = Vec::new();
    for part in path.split(|&byte| byte == separator) {
        match part {
            b"" | b"." => continue,
            b".." => return Spelled::Refused,
            _ if parts.len() > MAX_DEPTH => return Spelled::Refused,
            _ => {}
        }
        let mut component = Vec::new();
        if component.try_reserve_exact(part.len()).is_err() {
            return Spelled::Refused;
        }
        component.extend_from_slice(part);
        if !servable(&component) || parts.try_reserve(1).is_err() {
            return Spelled::Refused;
        }
        parts.push(component);
    }
    if parts.is_empty() {
        Spelled::Root
    } else {
        Spelled::Within(parts)
    }
}

/// A RISC OS leaf name as an archive spells it, served, or `None` for one no
/// RISC OS name can be ([`acorn::leaf_from_riscos`]).
pub(crate) fn from_riscos(spelt: &[u8]) -> Option<Vec<u8>> {
    owned(acorn::leaf_from_riscos(spelt).ok()?.as_bytes())
}

/// The served `name` as RISC OS spells it in an archive, in at most `max`
/// bytes, or `None` where it cannot be ([`acorn::leaf_to_riscos`]).
pub(crate) fn to_riscos(name: &[u8], max: usize) -> Option<Vec<u8>> {
    let leaf = acorn::leaf_to_riscos(name).ok()?;
    let spelt = leaf.as_bytes();
    if spelt.len() > max {
        return None;
    }
    owned(spelt)
}

/// `bytes`, held, or `None` where no room could be had for them.
fn owned(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len()).ok()?;
    out.extend_from_slice(bytes);
    Some(out)
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
