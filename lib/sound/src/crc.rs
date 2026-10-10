//! The checksums the formats here carry, every one most significant bit
//! first and starting from the value its format names: FLAC's CRC-8, the
//! CRC-16 FLAC frames and MPEG audio frames share, and Ogg's CRC-32. Framing
//! checks foreign formats specify, none a security primitive.

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

/// `crc` carried on over `bytes`.
pub(crate) fn crc32(crc: u32, bytes: &[u8]) -> u32 {
    CRC32.update(crc, bytes)
}

#[cfg(test)]
mod tests {
    use super::{crc16, crc32, crc8};

    const CHECK: &[u8] = b"123456789";

    /// The catalogued check values: CRC-8/SMBUS, CRC-16/UMTS (FLAC's),
    /// CRC-16/CMS (MPEG audio's), and Ogg's.
    #[test]
    fn each_checksum_has_its_catalogued_check_value() {
        assert_eq!(crc8(CHECK), 0xF4);
        assert_eq!(crc16(0, CHECK), 0xFEE8);
        assert_eq!(crc16(0xFFFF, CHECK), 0xAEE7);
        assert_eq!(crc32(0, CHECK), 0x89A1_897F);
    }

    #[test]
    fn carried_over_pieces_a_checksum_is_that_of_the_whole() {
        let (head, tail) = CHECK.split_at(4);
        assert_eq!(crc16(crc16(0, head), tail), crc16(0, CHECK));
        assert_eq!(crc32(crc32(0, head), tail), crc32(0, CHECK));
    }
}
