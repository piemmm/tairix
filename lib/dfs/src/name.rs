//! A name's bytes. DFS 1.20 stores what it is given; a name it makes holds
//! seven printable bytes at most, none of them those its commands read a
//! meaning into. A catalogue read is believed with a name of any printable
//! bytes, the top bit some protection schemes set in a name's bytes cleared.

use crate::catalogue::NAME_LEN;

/// The directory a name with none given is in.
pub const ROOT_DIR: u8 = b'$';

/// The bytes a name pads to its length with.
pub const PAD: u8 = b' ';

/// Whether `byte` may be in a name DFS makes, or be a directory: printable,
/// not a space, and none of `.` (which ends a directory), `:` (which names
/// a drive), `"` (which quotes) or the wildcards `#` and `*`.
#[must_use]
pub const fn admits(byte: u8) -> bool {
    matches!(byte, 0x21..=0x7E) && !matches!(byte, b'.' | b':' | b'"' | b'#' | b'*')
}

/// Whether a catalogue's `byte`, its top bit cleared, is one a believed name
/// holds: printable, a space among them.
#[must_use]
pub const fn readable(byte: u8) -> bool {
    matches!(byte & 0x7F, 0x20..=0x7E)
}

/// `name` as a catalogue holds it, padded to its length; `None` for one that
/// is empty, too long, or holds a byte DFS does not make a name of.
#[must_use]
pub fn spelled(name: &[u8]) -> Option<[u8; NAME_LEN]> {
    if name.is_empty() || name.len() > NAME_LEN || !name.iter().all(|&byte| admits(byte)) {
        return None;
    }
    let mut spelt = [PAD; NAME_LEN];
    spelt[..name.len()].copy_from_slice(name);
    Some(spelt)
}

/// The length of the name `spelt` holds, its padding off.
#[must_use]
pub fn unpadded_len(spelt: &[u8; NAME_LEN]) -> usize {
    spelt
        .iter()
        .rposition(|&byte| byte & 0x7F != PAD)
        .map_or(0, |last| last + 1)
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
