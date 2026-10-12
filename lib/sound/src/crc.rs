//! The checksums the formats here carry, each starting from the value its
//! format names: FLAC's CRC-8, the CRC-16 FLAC frames and MPEG audio frames
//! share, the reflected CRC-16 LAME guards its stream tag with, and Ogg's
//! CRC-32. Framing checks foreign formats specify, none a security
//! primitive.

/// A CRC of `width` bits over a generator polynomial, a byte at a time.
struct Crc {
    table: [u32; 256],
    width: u32,
}

impl Crc {
    const fn new(poly: u32, width: u32) -> Self {
        let top = 1u32 << (width - 1);
        let mask = u32::MAX >> (32 - width);
        let mut table = [0u32; 256];
        let mut byte: u32 = 0;
        while byte < 256 {
            let mut crc = byte << (width - 8);
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & top == 0 {
                    crc << 1
                } else {
                    (crc << 1) ^ poly
                };
                bit += 1;
            }
            table[byte as usize] = crc & mask;
            byte += 1;
        }
        Self { table, width }
    }

    fn update(&self, mut crc: u32, bytes: &[u8]) -> u32 {
        let shift = self.width - 8;
        let mask = u32::MAX >> (32 - self.width);
        for &byte in bytes {
            let index = ((crc >> shift) ^ u32::from(byte)) & 0xFF;
            crc = ((crc << 8) ^ self.table[index as usize]) & mask;
        }
        crc
    }
}

/// x^8 + x^2 + x + 1.
const CRC8: Crc = Crc::new(0x07, 8);

/// x^16 + x^15 + x^2 + 1.
const CRC16: Crc = Crc::new(0x8005, 16);

/// The IEEE 802.3 polynomial, unreflected.
const CRC32: Crc = Crc::new(0x04C1_1DB7, 32);

/// The CRC-8 of `bytes`, from zero.
pub(crate) fn crc8(bytes: &[u8]) -> u8 {
    (CRC8.update(0, bytes) & 0xFF) as u8
}

/// `crc` carried on over `bytes`.
pub(crate) fn crc16(crc: u16, bytes: &[u8]) -> u16 {
    (CRC16.update(u32::from(crc), bytes) & 0xFFFF) as u16
}

/// `crc` carried on over the first `bits` bits of `bytes`: MPEG audio
/// protects fields that need not end on a byte.
pub(crate) fn crc16_bits(crc: u16, bytes: &[u8], bits: usize) -> u16 {
    let whole = (bits / 8).min(bytes.len());
    let mut crc = crc16(crc, &bytes[..whole]);
    let rest = if whole < bytes.len() { bits % 8 } else { 0 };
    for at in 0..rest {
        let bit = u16::from(bytes[whole] >> (7 - at)) & 1;
        let top = crc >> 15;
        crc <<= 1;
        if top ^ bit == 1 {
            crc ^= 0x8005;
        }
    }
    crc
}

/// x^16 + x^15 + x^2 + 1 reflected, least significant bit first: the CRC
/// LAME guards its stream tag with.
const CRC16_REFLECTED: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut byte: u16 = 0;
    while byte < 256 {
        let mut crc = byte;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xA001
            };
            bit += 1;
        }
        table[byte as usize] = crc;
        byte += 1;
    }
    table
};

/// `crc` carried on over `bytes`, least significant bit first.
pub(crate) fn crc16_reflected(crc: u16, bytes: &[u8]) -> u16 {
    bytes.iter().fold(crc, |crc, &byte| {
        (crc >> 8) ^ CRC16_REFLECTED[usize::from((crc ^ u16::from(byte)) & 0xFF)]
    })
}

/// `crc` carried on over `bytes`.
pub(crate) fn crc32(crc: u32, bytes: &[u8]) -> u32 {
    CRC32.update(crc, bytes)
}

#[cfg(test)]
mod tests {
    use super::{crc16, crc16_bits, crc16_reflected, crc32, crc8};

    const CHECK: &[u8] = b"123456789";

    /// The catalogued check values: CRC-8/SMBUS, CRC-16/UMTS (FLAC's),
    /// CRC-16/CMS (MPEG audio's), CRC-16/ARC (LAME's), and Ogg's.
    #[test]
    fn each_checksum_has_its_catalogued_check_value() {
        assert_eq!(crc8(CHECK), 0xF4);
        assert_eq!(crc16(0, CHECK), 0xFEE8);
        assert_eq!(crc16(0xFFFF, CHECK), 0xAEE7);
        assert_eq!(crc16_reflected(0, CHECK), 0xBB3D);
        assert_eq!(crc32(0, CHECK), 0x89A1_897F);
    }

    #[test]
    fn a_checksum_of_whole_bytes_counted_in_bits_is_the_bytes_own() {
        assert_eq!(
            crc16_bits(0xFFFF, CHECK, CHECK.len() * 8),
            crc16(0xFFFF, CHECK)
        );
        // A bit at a time agrees with the table a byte at a time.
        let bitwise = (0..CHECK.len() * 8).fold(0xFFFFu16, |crc, at| {
            let bit = u16::from(CHECK[at / 8] >> (7 - at % 8)) & 1;
            let top = crc >> 15;
            (crc << 1) ^ if top ^ bit == 1 { 0x8005 } else { 0 }
        });
        assert_eq!(bitwise, 0xAEE7);
        let part = crc16_bits(0xFFFF, CHECK, 12);
        assert_eq!(part, crc16_bits(crc16(0xFFFF, &CHECK[..1]), &CHECK[1..], 4));
    }

    #[test]
    fn carried_over_pieces_a_checksum_is_that_of_the_whole() {
        let (head, tail) = CHECK.split_at(4);
        assert_eq!(crc16(crc16(0, head), tail), crc16(0, CHECK));
        assert_eq!(crc32(crc32(0, head), tail), crc32(0, CHECK));
    }
}
