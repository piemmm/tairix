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

/// The Unicode scalars of IBM code page 437's upper half, `0x80..=0xFF`.
const CP437_HIGH: [u16; 128] = [
    0x00C7, 0x00FC, 0x00E9, 0x00E2, 0x00E4, 0x00E0, 0x00E5, 0x00E7, 0x00EA, 0x00EB, 0x00E8, 0x00EF,
    0x00EE, 0x00EC, 0x00C4, 0x00C5, 0x00C9, 0x00E6, 0x00C6, 0x00F4, 0x00F6, 0x00F2, 0x00FB, 0x00F9,
    0x00FF, 0x00D6, 0x00DC, 0x00A2, 0x00A3, 0x00A5, 0x20A7, 0x0192, 0x00E1, 0x00ED, 0x00F3, 0x00FA,
    0x00F1, 0x00D1, 0x00AA, 0x00BA, 0x00BF, 0x2310, 0x00AC, 0x00BD, 0x00BC, 0x00A1, 0x00AB, 0x00BB,
    0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x2561, 0x2562, 0x2556, 0x2555, 0x2563, 0x2551, 0x2557,
    0x255D, 0x255C, 0x255B, 0x2510, 0x2514, 0x2534, 0x252C, 0x251C, 0x2500, 0x253C, 0x255E, 0x255F,
    0x255A, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256C, 0x2567, 0x2568, 0x2564, 0x2565, 0x2559,
    0x2558, 0x2552, 0x2553, 0x256B, 0x256A, 0x2518, 0x250C, 0x2588, 0x2584, 0x258C, 0x2590, 0x2580,
    0x03B1, 0x00DF, 0x0393, 0x03C0, 0x03A3, 0x03C3, 0x00B5, 0x03C4, 0x03A6, 0x0398, 0x03A9, 0x03B4,
    0x221E, 0x03C6, 0x03B5, 0x2229, 0x2261, 0x00B1, 0x2265, 0x2264, 0x2320, 0x2321, 0x00F7, 0x2248,
    0x00B0, 0x2219, 0x00B7, 0x221A, 0x207F, 0x00B2, 0x25A0, 0x00A0,
];

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
                    let ch = match (self, byte) {
                        (Self::RiscOs, _) => acorn::latin1_char(byte)?,
                        (_, 0..=0x7F) | (Self::Latin1, _) => char::from(byte),
                        _ => char::from_u32(u32::from(CP437_HIGH[usize::from(byte - 0x80)]))?,
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
                    let byte = if let Ok(ascii @ 0..=0x7F) = u8::try_from(u32::from(ch)) {
                        ascii
                    } else {
                        let scalar = u16::try_from(u32::from(ch)).ok()?;
                        let at = CP437_HIGH.iter().position(|&high| high == scalar)?;
                        0x80 + u8::try_from(at).ok()?
                    };
                    out.push(byte);
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
