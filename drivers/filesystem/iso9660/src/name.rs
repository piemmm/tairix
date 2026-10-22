//! The names a directory record stands for.

use tairix_abi::fs::FS_NAME_MAX;
use tairix_fsprobe::iso9660::Tree;

/// Bytes of the digest suffix an over-long name's alias ends in: `~` and
/// eight hex digits.
const ALIAS_SUFFIX: usize = 9;

/// A listed name: at most [`FS_NAME_MAX`] bytes.
#[derive(Clone)]
pub(crate) struct Name {
    bytes: [u8; FS_NAME_MAX],
    len: usize,
}

impl Name {
    pub(crate) const fn empty() -> Self {
        Self {
            bytes: [0; FS_NAME_MAX],
            len: 0,
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// `bytes`, unless they exceed [`FS_NAME_MAX`].
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut name = Self::empty();
        name.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        name.len = bytes.len();
        Some(name)
    }

    /// Replace a leading `_` with the `!` RISC OS records it stands for.
    pub(crate) fn restore_bang(&mut self) {
        if self.len > 0 && self.bytes[0] == b'_' {
            self.bytes[0] = b'!';
        }
    }

    /// Append `ch` as UTF-8, answering whether it fitted.
    fn push_char(&mut self, ch: char) -> bool {
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf).as_bytes();
        match self.bytes.get_mut(self.len..self.len + encoded.len()) {
            Some(slot) => {
                slot.copy_from_slice(encoded);
                self.len += encoded.len();
                true
            }
            None => false,
        }
    }
}

/// The longest a decoded identifier can be: Joliet's longest, 103 UCS-2
/// units of up to three UTF-8 bytes each, fits.
const DECODED_MAX: usize = 2 * FS_NAME_MAX;

/// An identifier decoded to UTF-8, each character this system cannot hold in
/// a name — a `/`, a NUL, a unit no character is — already `_`.
struct Decoded {
    bytes: [u8; DECODED_MAX],
    len: usize,
    /// A character was replaced, or the identifier did not fit.
    changed: bool,
}

impl Decoded {
    fn new() -> Self {
        Self {
            bytes: [0; DECODED_MAX],
            len: 0,
            changed: false,
        }
    }

    fn push(&mut self, ch: Option<char>) {
        let ch = match ch {
            Some('/' | '\0') | None => {
                self.changed = true;
                '_'
            }
            Some(ch) => ch,
        };
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf).as_bytes();
        match self.bytes.get_mut(self.len..self.len + encoded.len()) {
            Some(slot) => {
                slot.copy_from_slice(encoded);
                self.len += encoded.len();
            }
            None => self.changed = true,
        }
    }

    /// `bytes` as UTF-8, each sequence that is not one character `_`.
    fn utf8(bytes: &[u8]) -> Self {
        let mut decoded = Self::new();
        for chunk in bytes.utf8_chunks() {
            for ch in chunk.valid().chars() {
                decoded.push(Some(ch));
            }
            if !chunk.invalid().is_empty() {
                decoded.push(None);
            }
        }
        decoded
    }
}

/// The name a record's identifier stands for in the mounted `tree`: Joliet's
/// UCS-2, the other trees' bytes, as UTF-8, a file's `;version` and an
/// extension's empty `.` removed.
pub(crate) fn tree_name(identifier: &[u8], tree: Tree, is_dir: bool) -> Name {
    let decoded = match tree {
        Tree::Joliet => {
            let units = identifier
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&unit| u16::from_be_bytes(unit));
            let mut decoded = Decoded::new();
            for ch in char::decode_utf16(units) {
                decoded.push(ch.ok());
            }
            decoded
        }
        Tree::Primary | Tree::Enhanced => Decoded::utf8(identifier),
    };
    let text = &decoded.bytes[..decoded.len];
    let shown = if is_dir { text } else { without_version(text) };
    listable(shown, decoded.changed, identifier)
}

/// The name a Rock Ridge `NM` record's `bytes` stand for.
pub(crate) fn alternate_name(bytes: &[u8]) -> Name {
    let decoded = Decoded::utf8(bytes);
    listable(&decoded.bytes[..decoded.len], decoded.changed, bytes)
}

/// `shown` as a listed name: as it is, or — where a character was replaced
/// (`changed`), or it is empty, `.`, `..` or longer than [`FS_NAME_MAX`] —
/// by its alias, so no hostile disc names a path's traversal or two of its
/// files alike.
fn listable(shown: &[u8], changed: bool, identifier: &[u8]) -> Name {
    if changed || matches!(shown, b"" | b"." | b"..") {
        return alias(shown, identifier);
    }
    Name::from_bytes(shown).unwrap_or_else(|| alias(shown, identifier))
}

/// The name without its `;version` and, then, a trailing `.`.
fn without_version(name: &[u8]) -> &[u8] {
    let stem = match name.iter().rposition(|&b| b == b';') {
        Some(at) if name[at + 1..].iter().all(u8::is_ascii_digit) => &name[..at],
        _ => name,
    };
    match stem.split_last() {
        Some((b'.', rest)) if !rest.is_empty() => rest,
        _ => stem,
    }
}

/// As much of `decoded` as leaves room on a character boundary for `~` and
/// the CRC-32 of `identifier`, which keeps one name per identifier.
fn alias(decoded: &[u8], identifier: &[u8]) -> Name {
    let mut keep = decoded.len().min(FS_NAME_MAX - ALIAS_SUFFIX);
    while keep > 0 && decoded.get(keep).is_some_and(|&b| b & 0xC0 == 0x80) {
        keep -= 1;
    }
    let mut name = Name::empty();
    name.bytes[..keep].copy_from_slice(&decoded[..keep]);
    name.len = keep;
    let digest = tairix_crc32::checksum(identifier);
    name.push_char('~');
    for shift in (0..8).rev() {
        let digit = char::from_digit((digest >> (shift * 4)) & 0xF, 16).unwrap_or('0');
        name.push_char(digit);
    }
    name
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn versions_and_empty_extensions_are_removed_from_files_only() {
        let file = tree_name(b"README.TXT;1", Tree::Primary, false);
        assert_eq!(file.as_bytes(), b"README.TXT");
        assert_eq!(
            tree_name(b"MAKEFILE.;1", Tree::Primary, false).as_bytes(),
            b"MAKEFILE"
        );
        assert_eq!(
            tree_name(b"NOVERSION", Tree::Primary, false).as_bytes(),
            b"NOVERSION"
        );
        assert_eq!(
            tree_name(b"A;B", Tree::Primary, false).as_bytes(),
            b"A;B",
            "not a version"
        );
        assert_eq!(
            tree_name(b"DIR.D", Tree::Primary, true).as_bytes(),
            b"DIR.D"
        );
    }

    #[test]
    fn joliet_names_are_utf8() {
        let units: Vec<u8> = "Café;1".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(
            tree_name(&units, Tree::Joliet, false).as_bytes(),
            "Café".as_bytes()
        );
        let lone = [0xD8, 0x00, 0x00, b'A'];
        let other = [0xDC, 0x00, 0x00, b'A'];
        let text = String::from_utf8(tree_name(&lone, Tree::Joliet, true).as_bytes().to_vec())
            .expect("UTF-8");
        assert!(text.starts_with("_A~"), "{text}");
        assert_ne!(
            tree_name(&other, Tree::Joliet, true).as_bytes(),
            text.as_bytes(),
            "one name per identifier"
        );
    }

    #[test]
    fn a_name_no_path_could_hold_is_listed_by_its_alias() {
        let alias_of = |identifier: &[u8], tree| {
            String::from_utf8(tree_name(identifier, tree, true).as_bytes().to_vec()).expect("UTF-8")
        };
        let joliet =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_be_bytes).collect() };
        for (identifier, tree, stem) in [
            (&b".."[..].to_vec(), Tree::Enhanced, "..~"),
            (&b"."[..].to_vec(), Tree::Enhanced, ".~"),
            (&b"a/b"[..].to_vec(), Tree::Enhanced, "a_b~"),
            (&b"nul\0"[..].to_vec(), Tree::Primary, "nul_~"),
            (&joliet("../up"), Tree::Joliet, ".._up~"),
            (&vec![b'L', 0xE9, b'a'], Tree::Enhanced, "L_a~"),
        ] {
            let name = alias_of(identifier, tree);
            assert!(name.starts_with(stem), "{name}");
            assert!(!name.contains('/') && !name.contains('\0') && name != "." && name != "..");
        }
        let file = tree_name(b";1", Tree::Primary, false);
        assert!(
            file.as_bytes().starts_with(b"~"),
            "an empty name has an alias"
        );
        let rock_ridge = alternate_name(b"../../u/.profile");
        assert!(rock_ridge.as_bytes().starts_with(b".._.._u_.profile~"));
        assert_eq!(alternate_name(b"plain").as_bytes(), b"plain");
    }

    #[test]
    fn an_overlong_joliet_name_is_listed_by_a_stable_alias() {
        let wide: Vec<u8> = core::iter::repeat_n('中', 100)
            .collect::<String>()
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        let name = tree_name(&wide, Tree::Joliet, true);
        assert!(name.as_bytes().len() <= FS_NAME_MAX);
        let text = core::str::from_utf8(name.as_bytes()).expect("cut on a character boundary");
        assert!(text.starts_with('中'));
        let (_, digest) = text.rsplit_once('~').expect("the alias suffix");
        assert_eq!(digest.len(), 8);
        assert_eq!(
            tree_name(&wide, Tree::Joliet, true).as_bytes(),
            name.as_bytes()
        );
    }

    #[test]
    fn a_risc_os_bang_is_restored() {
        let mut name = tree_name(b"_BOOT", Tree::Primary, true);
        name.restore_bang();
        assert_eq!(name.as_bytes(), b"!BOOT");
    }
}
