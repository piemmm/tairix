//! The CRC-16 with the reflected polynomial `0xA001` — `0x8005` read least
//! significant bit first — and no final complement: what ARC, Spark and `ArcFS`
//! check their members with from zero (CRC-16/ARC), what ext4's legacy group
//! descriptors continue from all ones, and what LAME guards its stream tag
//! with. A framing checksum foreign formats specify, not a security
//! primitive.

#![no_std]

/// The reflected generator polynomial.
const POLY: u16 = 0xA001;

/// The remainder of every byte value, generated at build time so no table is
/// transcribed by hand.
const TABLE: [u16; 256] = table();

const fn table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut byte: u16 = 0;
    while byte < 256 {
        let mut crc = byte;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ POLY
            };
            bit += 1;
        }
        table[byte as usize] = crc;
        byte += 1;
    }
    table
}

/// The CRC-16/ARC of `data`: the checksum carried from zero.
#[must_use]
pub fn checksum(data: &[u8]) -> u16 {
    update(0, data)
}

/// `crc` carried on over `data`: the checksum of bytes that arrive in pieces,
/// or of a format that starts it elsewhere.
#[must_use]
pub fn update(crc: u16, data: &[u8]) -> u16 {
    data.iter().fold(crc, |crc, &byte| {
        let index = usize::from(crc.to_le_bytes()[0] ^ byte);
        (crc >> 8) ^ TABLE[index]
    })
}

#[cfg(test)]
mod tests;
