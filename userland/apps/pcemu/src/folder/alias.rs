//! The 8.3 names a share gives the host's entries. A host name DOS could hold
//! is its own, in upper case; any other takes an alias — the first bytes of
//! its base a name may hold, a tilde and a number, and the first of its last
//! ending's — as Windows gives a long name its short one, so a DOS program
//! reaches every entry by a name it can type.

use alloc::string::String;

use super::DosName;
use crate::dos::name::{is_forbidden, spell, upper};

/// The most aliases tried for one host name: past it the directory has no
/// free alias for it, and the entry is not shown.
pub const TRIES: u32 = 999_999;

/// `ch` as a byte of a DOS name, in upper case, if code page 437 has it and a
/// name may hold it.
fn name_byte(ch: char) -> Option<u8> {
    let byte = upper(tairix_cp437::byte_of(ch)?);
    let allowed = !matches!(byte, b' ' | b'.' | b'*' | b'?') && !is_forbidden(byte);
    allowed.then_some(byte)
}

/// The host name's own 8.3 form, where it has one: a base of one to eight
/// bytes a name may hold and, after one dot, an extension of one to three, in
/// either case — a name DOS spells back as itself.
#[must_use]
pub fn exact(host: &str) -> Option<DosName> {
    let (base, ext) = match host.split_once('.') {
        Some((base, ext)) if !ext.is_empty() && !ext.contains('.') => (base, ext),
        Some(_) => return None,
        None => (host, ""),
    };
    if base.is_empty() || base.chars().count() > 8 || ext.chars().count() > 3 {
        return None;
    }
    let mut field = [b' '; 11];
    for (slot, ch) in field[..8].iter_mut().zip(base.chars()) {
        *slot = name_byte(ch)?;
    }
    for (slot, ch) in field[8..].iter_mut().zip(ext.chars()) {
        *slot = name_byte(ch)?;
    }
    DosName::new(directory_form(field), false)
}

/// The `n`th alias, from 1, of the host name: up to six bytes of its base,
/// fewer as `n` grows, then `~n`, and up to three bytes of its last ending.
/// A space or a dot is left out and any other byte a name may not hold
/// becomes `_`.
#[must_use]
pub fn alias(host: &str, n: u32) -> Option<DosName> {
    if n == 0 || n > TRIES {
        return None;
    }
    let trimmed = host.trim_start_matches('.');
    let (base, ext) = trimmed.rsplit_once('.').unwrap_or((trimmed, ""));
    let shown = |ch: char| match ch {
        ' ' | '.' => None,
        _ => Some(name_byte(ch).unwrap_or(b'_')),
    };
    let mut tail = [0u8; 8];
    let mut digits = 0;
    let mut rest = n;
    while rest > 0 {
        digits += 1;
        tail[8 - digits] = b'0' + u8::try_from(rest % 10).ok()?;
        rest /= 10;
    }
    tail[7 - digits] = b'~';
    let suffix = &tail[7 - digits..];
    let mut field = [b' '; 11];
    let mut used = 0;
    for byte in base.chars().filter_map(shown).take(8 - suffix.len()) {
        field[used] = byte;
        used += 1;
    }
    if used == 0 && suffix.len() < 8 {
        field[0] = b'_';
        used = 1;
    }
    field[used..used + suffix.len()].copy_from_slice(suffix);
    for (slot, byte) in field[8..].iter_mut().zip(ext.chars().filter_map(shown)) {
        *slot = byte;
    }
    DosName::new(directory_form(field), false)
}

/// The label a share of the folder named `folder` carries: the first eight
/// bytes of its name a name may hold, in upper case, or `SHARED` for a name
/// holding none.
#[must_use]
pub fn label(folder: &str) -> DosName {
    let mut field = [b' '; 11];
    for (slot, byte) in field[..8]
        .iter_mut()
        .zip(folder.chars().filter_map(name_byte))
    {
        *slot = byte;
    }
    DosName::new(directory_form(field), false).unwrap_or(UNNAMED)
}

/// What a share of a folder whose name holds nothing DOS could is labelled.
const UNNAMED: DosName = DosName(*b"SHARED     ");

/// The host spelling of the DOS name `name`, for an entry DOS makes: `BASE.EXT`
/// in the characters code page 437 gives its bytes.
#[must_use]
pub fn host_name(name: &DosName) -> String {
    let mut spelled = [0u8; 13];
    let len = spell(name.bytes(), &mut spelled);
    spelled[..len]
        .iter()
        .map(|&byte| tairix_cp437::char_of(byte))
        .collect()
}

/// `field` as a directory entry holds it: a first byte of `E5h`, which marks
/// an entry free, kept as `05h`.
fn directory_form(mut field: [u8; 11]) -> [u8; 11] {
    if field[0] == 0xE5 {
        field[0] = 0x05;
    }
    field
}

#[cfg(test)]
#[path = "alias_tests.rs"]
mod tests;
