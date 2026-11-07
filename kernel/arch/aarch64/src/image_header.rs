//! The arm64 `Image` header (the Linux arm64 boot protocol,
//! `Documentation/arch/arm64/booting.rst`): the 64 bytes the kernel image
//! opens with, by which a loader places it.
//!
//! `boot.s` lays the header down from these definitions and the image builder
//! reads it back through [`header`](crate::image_header::header), so the two
//! cannot disagree about the format.

/// Bytes of the header.
pub const HEADER_LEN: usize = 64;

/// `magic` (bytes 56..60, `ARM\x64`), read as a little-endian word.
pub const MAGIC: u32 = u32::from_le_bytes(*b"ARM\x64");

/// `flags` of a little-endian kernel built for 4 KiB pages: bit 0 clear, and
/// `1` in the page-size field, bits 2:1.
pub const FLAGS_LE_4K: u64 = 0b010;

const TEXT_OFFSET_AT: usize = 8;
const IMAGE_SIZE_AT: usize = 16;
const FLAGS_AT: usize = 24;
const MAGIC_AT: usize = 56;

/// What a header states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// How far past a 2 MiB-aligned base of RAM the image asks to be placed.
    pub text_offset: u64,
    /// The image's whole reach, the memory it zeroes past its file included.
    pub image_size: u64,
    /// Byte order, page size and placement.
    pub flags: u64,
}

/// The header `image` opens with, or [`None`] when it is too short to hold
/// one or carries no `ARM\x64` magic.
#[must_use]
pub fn header(image: &[u8]) -> Option<Header> {
    let image = image.get(..HEADER_LEN)?;
    let word = |at: usize| -> Option<u64> {
        let bytes = image.get(at..at + 8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    };
    let magic = image.get(MAGIC_AT..MAGIC_AT + 4)?;
    if u32::from_le_bytes(magic.try_into().ok()?) != MAGIC {
        return None;
    }
    Some(Header {
        text_offset: word(TEXT_OFFSET_AT)?,
        image_size: word(IMAGE_SIZE_AT)?,
        flags: word(FLAGS_AT)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(text_offset: u64, image_size: u64, flags: u64) -> [u8; HEADER_LEN] {
        let mut bytes = [0u8; HEADER_LEN];
        bytes[TEXT_OFFSET_AT..TEXT_OFFSET_AT + 8].copy_from_slice(&text_offset.to_le_bytes());
        bytes[IMAGE_SIZE_AT..IMAGE_SIZE_AT + 8].copy_from_slice(&image_size.to_le_bytes());
        bytes[FLAGS_AT..FLAGS_AT + 8].copy_from_slice(&flags.to_le_bytes());
        bytes[MAGIC_AT..MAGIC_AT + 4].copy_from_slice(&MAGIC.to_le_bytes());
        bytes
    }

    #[test]
    fn the_magic_is_the_protocols() {
        assert_eq!(&MAGIC.to_le_bytes(), b"ARMd");
    }

    #[test]
    fn a_header_reads_back_what_it_states() {
        assert_eq!(
            header(&image(0, 0x1c2_f000, FLAGS_LE_4K)),
            Some(Header {
                text_offset: 0,
                image_size: 0x1c2_f000,
                flags: FLAGS_LE_4K,
            })
        );
    }

    #[test]
    fn no_magic_or_too_few_bytes_is_no_header() {
        let mut bytes = image(0, 0x1000, FLAGS_LE_4K);
        assert!(header(&bytes[..HEADER_LEN - 1]).is_none());
        bytes[MAGIC_AT] = b'X';
        assert!(header(&bytes).is_none());
        assert!(header(&[]).is_none());
    }
}
