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

/// A character set an archive spells names in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Charset {
    /// UTF-8, as written.
    Utf8,
    /// IBM code page 437, a zip member's unless it says otherwise.
    Cp437,
    /// ISO 8859-1.
    Latin1,
    /// RISC OS's Latin-1: ISO 8859-1 with characters of its own in
    /// `0x80..=0x9F`.
    RiscOs,
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
            Self::Cp437 | Self::Latin1 | Self::RiscOs => {
                for &byte in bytes {
                    let ch = match self {
                        Self::RiscOs => acorn::latin1_char(byte)?,
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
            Self::RiscOs => {
                for ch in text.chars() {
                    out.push(acorn::latin1_byte(ch)?);
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

/// The UTF-8 member path `path`, cut at `separator`, each component crossed
/// through `cross` (RISC OS's exchange of `.` and `/`, or none); refused past
/// the depth the tree nests to, before a component past it is made.
pub(crate) fn spelled(path: &[u8], separator: u8, cross: fn(u8) -> u8) -> Spelled {
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
        component.extend(part.iter().map(|&byte| cross(byte)));
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

/// A byte that crosses as itself.
pub(crate) const fn as_is(byte: u8) -> u8 {
    byte
}

/// A RISC OS leaf name as an archive spells it — RISC OS's Latin-1, a dot
/// written as `/` — served, or `None` for one no volume serves.
pub(crate) fn from_riscos(spelt: &[u8]) -> Option<Vec<u8>> {
    let mut crossed = Vec::new();
    crossed.try_reserve_exact(spelt.len()).ok()?;
    crossed.extend(spelt.iter().map(|&byte| acorn::crossed_name_byte(byte)));
    let name = Charset::RiscOs.decode(&crossed)?;
    servable(&name).then_some(name)
}

/// The served `name` as RISC OS spells it in an archive, in at most `max`
/// bytes, or `None` where it cannot be: a character RISC OS's Latin-1 lacks,
/// a space, or one of the characters RISC OS keeps out of a name.
pub(crate) fn to_riscos(name: &[u8], max: usize) -> Option<Vec<u8>> {
    let mut spelt = Charset::RiscOs.encode(name)?;
    for byte in &mut spelt {
        *byte = acorn::crossed_name_byte(*byte);
    }
    let keeps = |byte: &u8| *byte != b' ' && !acorn::RESERVED_NAME_BYTES.contains(byte);
    (!spelt.is_empty() && spelt.len() <= max && spelt.iter().all(keeps)).then_some(spelt)
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
