//! The tags other software wraps a sound file in: an `ID3v2` tag before it,
//! an `APEv2` or `ID3v1` tag after it. Located and stepped over, never read
//! as sound.

use crate::input::{self, SoundInput};
use crate::DecodeError;

/// Bytes of an `ID3v2` header, and of its footer.
pub(crate) const ID3V2_HEADER_LEN: usize = 10;

const ID3V2_FOOTER_FLAG: u8 = 0x10;

/// Bytes of an `ID3v1` tag.
const ID3V1_LEN: u64 = 128;

/// Bytes of an `APEv2` header, and of its footer.
const APE_FRAME_LEN: u64 = 32;

const APE_HAS_HEADER: u32 = 1 << 31;

/// The bytes of the `ID3v2` tag `bytes` opens with, header and footer
/// included.
pub(crate) fn id3v2_len(bytes: &[u8]) -> Option<u64> {
    let [b'I', b'D', b'3', major, revision, flags, size @ ..] = bytes.get(..ID3V2_HEADER_LEN)?
    else {
        return None;
    };
    if *major == 0xFF || *revision == 0xFF || size.iter().any(|&byte| byte & 0x80 != 0) {
        return None;
    }
    let body = size
        .iter()
        .fold(0u64, |size, &byte| size << 7 | u64::from(byte));
    let footer = if flags & ID3V2_FOOTER_FLAG != 0 {
        ID3V2_HEADER_LEN
    } else {
        0
    };
    let fixed = u64::try_from(ID3V2_HEADER_LEN + footer).ok()?;
    Some(fixed + body)
}

/// The bytes of the `APEv2` and `ID3v1` tags that end the file `input`
/// holds, before `end`.
pub(crate) fn trailing_len(
    input: &mut (impl SoundInput + ?Sized),
    start: u64,
    end: u64,
) -> Result<u64, DecodeError> {
    let mut tags = 0;
    if end - start >= ID3V1_LEN {
        let mut tag = [0u8; 3];
        input::read_exact(input, end - ID3V1_LEN, &mut tag, DecodeError::InputFailed)?;
        if &tag == b"TAG" {
            tags = ID3V1_LEN;
        }
    }
    let before = end - tags;
    if before - start >= APE_FRAME_LEN {
        let mut footer = [0u8; 32];
        input::read_exact(
            input,
            before - APE_FRAME_LEN,
            &mut footer,
            DecodeError::InputFailed,
        )?;
        if footer.starts_with(b"APETAGEX") {
            let field = |at: usize| {
                u32::from_le_bytes([footer[at], footer[at + 1], footer[at + 2], footer[at + 3]])
            };
            let header = if field(20) & APE_HAS_HEADER != 0 {
                APE_FRAME_LEN
            } else {
                0
            };
            let ape = u64::from(field(12)) + header;
            if ape >= APE_FRAME_LEN && ape <= before - start {
                tags += ape;
            }
        }
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{id3v2_len, trailing_len};

    #[test]
    fn an_id3v2_tag_is_measured_by_its_syncsafe_size() {
        let mut header = *b"ID3\x04\x00\x00\x00\x00\x02\x01";
        assert_eq!(id3v2_len(&header), Some(10 + 257));
        header[5] = 0x10;
        assert_eq!(id3v2_len(&header), Some(20 + 257));
        header[9] = 0x80;
        assert_eq!(id3v2_len(&header), None, "a size byte with its top bit set");
        assert_eq!(id3v2_len(b"ID3\x04\x00"), None);
        assert_eq!(id3v2_len(b"fLaC\0\0\0\"\0\0"), None);
    }

    #[test]
    fn trailing_ape_and_id3v1_tags_are_measured() {
        let mut ape = [0u8; 32];
        ape[..8].copy_from_slice(b"APETAGEX");
        ape[12..16].copy_from_slice(&(32u32 + 10).to_le_bytes());
        ape[20..24].copy_from_slice(&(1u32 << 31).to_le_bytes());
        let mut id3v1 = [0u8; 128];
        id3v1[..3].copy_from_slice(b"TAG");
        let sound = [7u8; 300];
        let file: Vec<u8> = [&sound[..], &[0; 32 + 10], &ape, &id3v1].concat();
        let len = file.len() as u64;
        assert_eq!(trailing_len(&mut &file[..], 0, len), Ok(128 + 32 + 10 + 32));
        assert_eq!(trailing_len(&mut &sound[..], 0, 300), Ok(0));
    }
}
