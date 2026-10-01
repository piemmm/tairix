//! CRC-32 as IEEE 802.3 defines it: reflected, polynomial `0xEDB8_8320`,
//! starting from all ones and complemented at the end — the checksum PNG
//! chunks, GPT headers and entry arrays, and zip and gzip members carry.
//!
//! It is a framing checksum foreign formats specify, not one TAIRiX chooses:
//! the block-integrity checksum of TAIRiX's own formats is CRC-32C
//! (`lib/crc32c`), a different polynomial. Neither is a security primitive.

#![no_std]

/// The reflected generator polynomial.
const POLY: u32 = 0xEDB8_8320;

/// The remainder of every byte value, generated at build time so no table is
/// transcribed by hand.
const TABLE: [u32; 256] = table();

const fn table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut byte: u32 = 0;
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

/// The CRC-32 of `data`.
#[must_use]
pub fn checksum(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

/// A CRC-32 taken over bytes that arrive in pieces: the checksum of every
/// piece fed in, in order, is the checksum of their concatenation.
#[derive(Copy, Clone, Debug)]
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A checksum over no bytes yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: u32::MAX }
    }

    /// Fold `data` in.
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let index = usize::from(self.state.to_le_bytes()[0] ^ byte);
            self.state = (self.state >> 8) ^ TABLE[index];
        }
    }

    /// The checksum of everything folded in.
    #[must_use]
    pub const fn finish(self) -> u32 {
        !self.state
    }
}

#[cfg(test)]
mod tests;
