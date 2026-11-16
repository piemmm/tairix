//! CRT images: a cartridge's hardware type, the levels its EXROM and GAME
//! lines start at, and its ROM chips, each with its bank and the address it
//! answers at.
//!
//! The header is `C64 CARTRIDGE`, its own length, a version, the hardware
//! type, the EXROM and GAME lines (0 low), a subtype and the name; each chip
//! packet follows as `CHIP`, its length, the chip's type, bank, load address
//! and size, then its bytes. Multi-byte fields are high byte first.

use alloc::vec::Vec;

const SIGNATURE: &[u8; 16] = b"C64 CARTRIDGE   ";
const CHIP_SIGNATURE: &[u8; 4] = b"CHIP";
/// The packet header before a chip's bytes.
const CHIP_HEADER: usize = 0x10;
/// The most chips an image holds: 64 banks of ROML and ROMH, and some.
const MAX_CHIPS: usize = 256;
/// A chip's largest size: 16 KiB.
const MAX_CHIP_BYTES: usize = 0x4000;
/// The largest image decoded: an 8 KiB chip in each of sixty-four banks of
/// both ROMs, with its packet, after a header of the usual length — all a
/// cartridge the machine holds can take.
pub const MAX_IMAGE: usize = 0x40 + 2 * 64 * (CHIP_HEADER + 0x2000);

/// Why an image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrtError {
    /// Not a CRT image, or one for another machine.
    BadSignature,
    /// A header or packet outside the image or past the bounds, or no chip.
    Malformed,
}

/// One ROM chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    /// The bank it belongs to.
    pub bank: u16,
    /// The address it answers at: `$8000`, `$A000` or `$E000`.
    pub address: u16,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// A cartridge image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crt {
    /// The hardware type.
    pub hardware: u16,
    /// EXROM and GAME as the cartridge starts them, true while low.
    pub exrom: bool,
    /// See `exrom`.
    pub game: bool,
    /// Its chips.
    pub chips: Vec<Chip>,
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn be32(bytes: &[u8], at: usize) -> Option<usize> {
    let field = bytes.get(at..at + 4)?;
    usize::try_from(u32::from_be_bytes([field[0], field[1], field[2], field[3]])).ok()
}

/// A CRT image's cartridge.
///
/// # Errors
///
/// [`CrtError::BadSignature`] for another signature, and
/// [`CrtError::Malformed`] for an image past [`MAX_IMAGE`], a header or
/// packet outside the image or the bounds, or no chip.
pub fn decode(image: &[u8]) -> Result<Crt, CrtError> {
    if !image.starts_with(SIGNATURE) {
        return Err(CrtError::BadSignature);
    }
    if image.len() > MAX_IMAGE {
        return Err(CrtError::Malformed);
    }
    let header = be32(image, 0x10).ok_or(CrtError::Malformed)?.max(0x40);
    let hardware = be16(image, 0x16).ok_or(CrtError::Malformed)?;
    let exrom = *image.get(0x18).ok_or(CrtError::Malformed)? == 0;
    let game = *image.get(0x19).ok_or(CrtError::Malformed)? == 0;
    let mut chips = Vec::new();
    let mut at = header;
    while at < image.len() {
        let packet = image.get(at..at + CHIP_HEADER).ok_or(CrtError::Malformed)?;
        if &packet[..4] != CHIP_SIGNATURE || chips.len() == MAX_CHIPS {
            return Err(CrtError::Malformed);
        }
        let length = be32(packet, 4).ok_or(CrtError::Malformed)?;
        let bank = be16(packet, 0x0A).ok_or(CrtError::Malformed)?;
        let address = be16(packet, 0x0C).ok_or(CrtError::Malformed)?;
        let size = usize::from(be16(packet, 0x0E).ok_or(CrtError::Malformed)?);
        if size == 0 || size > MAX_CHIP_BYTES || length < CHIP_HEADER + size {
            return Err(CrtError::Malformed);
        }
        let bytes = image
            .get(at + CHIP_HEADER..at + CHIP_HEADER + size)
            .ok_or(CrtError::Malformed)?;
        chips.push(Chip {
            bank,
            address,
            bytes: bytes.to_vec(),
        });
        at = at.checked_add(length).ok_or(CrtError::Malformed)?;
    }
    if chips.is_empty() {
        return Err(CrtError::Malformed);
    }
    Ok(Crt {
        hardware,
        exrom,
        game,
        chips,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A CRT of `hardware` type with `chips`, each (bank, address, bytes).
    pub(crate) fn image(
        hardware: u16,
        exrom: u8,
        game: u8,
        chips: &[(u16, u16, &[u8])],
    ) -> Vec<u8> {
        let mut image = Vec::from(&SIGNATURE[..]);
        image.extend(0x40u32.to_be_bytes());
        image.extend([1, 0]);
        image.extend(hardware.to_be_bytes());
        image.extend([exrom, game, 0, 0, 0, 0, 0, 0]);
        let mut name = Vec::from(&b"TEST"[..]);
        name.resize(32, 0);
        image.extend(name);
        for &(bank, address, bytes) in chips {
            image.extend(CHIP_SIGNATURE);
            image.extend(
                u32::try_from(CHIP_HEADER + bytes.len())
                    .expect("small")
                    .to_be_bytes(),
            );
            image.extend([0, 0]);
            image.extend(bank.to_be_bytes());
            image.extend(address.to_be_bytes());
            image.extend(u16::try_from(bytes.len()).expect("small").to_be_bytes());
            image.extend_from_slice(bytes);
        }
        image
    }

    #[test]
    fn a_cartridge_s_header_and_chips_decode() {
        let crt = decode(&image(
            5,
            0,
            0,
            &[(0, 0x8000, &[1; 0x2000]), (1, 0x8000, &[2; 0x2000])],
        ))
        .expect("a CRT");
        assert_eq!((crt.hardware, crt.exrom, crt.game), (5, true, true));
        assert_eq!(crt.chips.len(), 2);
        assert_eq!(
            (
                crt.chips[1].bank,
                crt.chips[1].address,
                crt.chips[1].bytes[0]
            ),
            (1, 0x8000, 2)
        );
    }

    #[test]
    fn malformed_images_are_refused() {
        assert_eq!(decode(b"C64 CARTRIDGF   "), Err(CrtError::BadSignature));
        let mut short = image(0, 0, 1, &[(0, 0x8000, &[0; 0x2000])]);
        short.truncate(short.len() - 1);
        assert_eq!(decode(&short), Err(CrtError::Malformed));
        let oversized = image(0, 0, 1, &[(0, 0x8000, &alloc::vec![0; 0x4001])]);
        assert_eq!(decode(&oversized), Err(CrtError::Malformed));
        assert_eq!(
            decode(&image(0, 0, 1, &[])),
            Err(CrtError::Malformed),
            "no chip"
        );
        let mut headless = image(0, 0, 1, &[(0, 0x8000, &[0; 0x2000])]);
        headless[0x10..0x14].copy_from_slice(&0x00FF_0000u32.to_be_bytes());
        assert_eq!(
            decode(&headless),
            Err(CrtError::Malformed),
            "a header past the image"
        );
    }

    #[test]
    fn an_image_past_what_a_cartridge_holds_is_refused() {
        let chips: Vec<(u16, u16, &[u8])> = (0..64u16)
            .flat_map(|bank| {
                [
                    (bank, 0x8000, &[0u8; 0x2000][..]),
                    (bank, 0xA000, &[0; 0x2000]),
                ]
            })
            .collect();
        let mut full = image(19, 0, 1, &chips);
        assert_eq!(full.len(), MAX_IMAGE);
        assert_eq!(decode(&full).map(|crt| crt.chips.len()), Ok(128));
        full.push(0);
        assert_eq!(decode(&full), Err(CrtError::Malformed));
    }
}
