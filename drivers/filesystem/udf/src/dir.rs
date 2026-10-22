//! File Identifier Descriptors (ECMA-167 4/14.4) and the names they carry.

use alloc::vec::Vec;

use tairix_abi::fs::FS_NAME_MAX;
use tairix_abi::DriverError;
use tairix_fsprobe::udf::{crc16, cs0_chars, LongAd, Tag, TAG_FID, TAG_LEN};

use crate::fields::Fields;
use crate::layout::LbAddr;

/// Bytes of a File Identifier Descriptor before its implementation use.
pub(crate) const FID_FIXED: usize = 38;

/// File characteristic: the entry is a directory.
pub(crate) const CHAR_DIRECTORY: u8 = 1 << 1;
/// File characteristic: the entry was deleted.
pub(crate) const CHAR_DELETED: u8 = 1 << 2;
/// File characteristic: the entry names the parent directory.
pub(crate) const CHAR_PARENT: u8 = 1 << 3;

/// Bytes of the suffix a translated name ends in: `#` and four hex digits.
const TRANSLATION_SUFFIX: usize = 5;

/// One File Identifier Descriptor, its identifier held.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fid {
    pub characteristics: u8,
    pub icb: LongAd,
    /// The OSTA Compressed Unicode identifier, its compression ID first.
    pub identifier: Vec<u8>,
    /// The descriptor's bytes, its padding to four bytes included.
    pub len: usize,
}

impl Fid {
    /// The entry the descriptor names.
    pub(crate) fn node(&self) -> LbAddr {
        LbAddr {
            block: self.icb.block,
            partition: self.icb.partition,
        }
    }

    /// The low half of the named entry's unique id, as UDF 2.00 and later
    /// record it with the name.
    pub(crate) fn unique(&self) -> u64 {
        let id = &self.icb.impl_use;
        u64::from(u32::from_le_bytes([id[2], id[3], id[4], id[5]]))
    }

    /// Bytes of the fixed part and the variable lengths of the descriptor
    /// opening `header`: what a reader needs before the rest.
    pub(crate) fn length(header: &[u8]) -> Result<usize, DriverError> {
        let f = Fields(header);
        let identifier = usize::from(f.u8(19)?);
        let implementation = usize::from(f.u16(36)?);
        Ok((FID_FIXED + implementation + identifier).next_multiple_of(4))
    }

    /// The descriptor `bytes` holds whole, recorded in partition block
    /// `location`: its tag, checksum and location verified, and its CRC
    /// covering every byte up to the end of its identifier.
    pub(crate) fn parse(bytes: &[u8], location: u32) -> Result<Self, DriverError> {
        let tag = Tag::verify(bytes, location).ok_or(DriverError::DeviceFault)?;
        if tag.id != TAG_FID {
            return Err(DriverError::DeviceFault);
        }
        let f = Fields(bytes);
        let identifier_len = usize::from(f.u8(19)?);
        let implementation = usize::from(f.u16(36)?);
        let covered = usize::from(f.u16(10)?) + TAG_LEN;
        if covered < FID_FIXED + implementation + identifier_len {
            return Err(DriverError::DeviceFault);
        }
        let identifier = f.bytes(FID_FIXED + implementation, identifier_len)?;
        Ok(Self {
            characteristics: f.u8(18)?,
            icb: f.long_ad(20)?,
            identifier: tairix_util::fallible::collected(
                identifier.len(),
                identifier.iter().copied(),
            )
            .ok_or(DriverError::OutOfMemory)?,
            len: Self::length(bytes)?,
        })
    }
}

/// A listed name: at most [`FS_NAME_MAX`] bytes.
#[derive(Clone)]
pub(crate) struct Name {
    bytes: [u8; FS_NAME_MAX],
    len: usize,
}

impl Name {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push(&mut self, ch: char) -> bool {
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

/// The name an identifier lists under, `None` for one whose compression no
/// reader knows.
///
/// A name this system cannot hold — one with a `/` or NUL, or `.` or `..`,
/// or longer than [`FS_NAME_MAX`] in UTF-8, or one with a code unit no
/// character is — is translated as OSTA UDF's Unix translation does: each
/// offending character becomes `_`, the name is cut to fit, and `#` and a
/// CRC of the whole identifier make it one name per identifier.
pub(crate) fn decode(identifier: &[u8]) -> Option<Name> {
    let chars = cs0_chars(identifier)?;
    let mut name = Name {
        bytes: [0; FS_NAME_MAX],
        len: 0,
    };
    let mut changed = false;
    let mut cut = 0;
    for ch in chars {
        let ch = match ch {
            Ok('/' | '\0') | Err(_) => {
                changed = true;
                '_'
            }
            Ok(ch) => ch,
        };
        if name.len + ch.len_utf8() > FS_NAME_MAX - TRANSLATION_SUFFIX && cut == 0 {
            cut = name.len;
        }
        if !name.push(ch) {
            changed = true;
            break;
        }
    }
    if matches!(name.as_bytes(), b"" | b"." | b"..") {
        changed = true;
    }
    if changed {
        name.len = if name.len > FS_NAME_MAX - TRANSLATION_SUFFIX {
            cut
        } else {
            name.len
        };
        name.push('#');
        let digest = crc16(identifier);
        for shift in (0..4).rev() {
            let digit = char::from_digit(u32::from(digest >> (shift * 4)) & 0xF, 16).unwrap_or('0');
            name.push(digit.to_ascii_uppercase());
        }
    }
    Some(name)
}

/// `name`, a UTF-8 name, as an OSTA Compressed Unicode identifier: eight
/// bits a character when every character fits them, else UTF-16. `None` for
/// a name longer than an identifier's 255 bytes.
pub(crate) fn encode(name: &str) -> Option<Vec<u8>> {
    let narrow = name.chars().all(|ch| u32::from(ch) < 0x100);
    let mut out = Vec::new();
    if narrow {
        out.push(8);
        out.extend(
            name.chars()
                .map(|ch| u8::try_from(u32::from(ch)).unwrap_or(b'_')),
        );
    } else {
        out.push(16);
        out.extend(name.encode_utf16().flat_map(u16::to_be_bytes));
    }
    (out.len() <= 255).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_decode_from_both_compressions_and_encode_back() {
        for name in ["plain.txt", "Café", "中文名字.doc", "emoji 😀"] {
            let identifier = encode(name).expect("encodes");
            assert_eq!(
                decode(&identifier).expect("decodes").as_bytes(),
                name.as_bytes()
            );
        }
        assert_eq!(encode("Café").expect("narrow")[0], 8);
        assert_eq!(encode("中").expect("wide")[0], 16);
        assert!(decode(&[7, b'x']).is_none(), "an unknown compression");
    }

    #[test]
    fn a_name_this_system_cannot_hold_is_translated_stably() {
        let slash = decode(&[8, b'a', b'/', b'b']).expect("decodes");
        let text = core::str::from_utf8(slash.as_bytes()).expect("UTF-8");
        assert!(text.starts_with("a_b#"), "{text}");
        assert_eq!(text.len(), 3 + TRANSLATION_SUFFIX);
        assert_ne!(
            decode(&[8, b'a', b'\0', b'b']).expect("decodes").as_bytes(),
            slash.as_bytes()
        );
        assert!(
            core::str::from_utf8(decode(&[8, b'.']).expect("decodes").as_bytes())
                .expect("UTF-8")
                .starts_with(".#")
        );
        let long: Vec<u8> = core::iter::once(16)
            .chain(core::iter::repeat_n([0x4E, 0x2D], 120).flatten())
            .collect();
        let cut = decode(&long).expect("decodes");
        assert!(cut.as_bytes().len() <= FS_NAME_MAX);
        assert!(
            core::str::from_utf8(cut.as_bytes()).is_ok(),
            "cut on a character"
        );
        assert_eq!(decode(&long).expect("decodes").as_bytes(), cut.as_bytes());
    }
}
