//! Discs the crate's tests lay out and read: one in memory, and one that
//! keeps only what is written, for discs far larger than memory.

extern crate std;

use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use crate::Disc;

/// What a test disc refuses: an access past its end.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct PastEnd;

/// A disc held whole in memory.
pub(crate) struct MemDisc(pub Vec<u8>);

impl MemDisc {
    pub(crate) fn new(bytes: usize) -> Self {
        Self(vec![0; bytes])
    }
}

fn span(at: u64, length: usize, capacity: usize) -> Result<core::ops::Range<usize>, PastEnd> {
    let start = usize::try_from(at).map_err(|_| PastEnd)?;
    let end = start.checked_add(length).ok_or(PastEnd)?;
    if end > capacity {
        return Err(PastEnd);
    }
    Ok(start..end)
}

impl Disc for MemDisc {
    type Error = PastEnd;

    fn capacity(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), PastEnd> {
        let range = span(at, out.len(), self.0.len())?;
        out.copy_from_slice(&self.0[range]);
        Ok(())
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), PastEnd> {
        let range = span(at, bytes.len(), self.0.len())?;
        self.0[range].copy_from_slice(bytes);
        Ok(())
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), PastEnd> {
        let length = usize::try_from(length).map_err(|_| PastEnd)?;
        let range = span(at, length, self.0.len())?;
        self.0[range].fill(0);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), PastEnd> {
        Ok(())
    }
}

/// A disc of any size that holds only the blocks written to it, every other
/// byte reading zero.
pub(crate) struct SparseDisc {
    capacity: u64,
    blocks: BTreeMap<u64, [u8; Self::BLOCK]>,
}

impl SparseDisc {
    const BLOCK: usize = 512;

    /// The block holding the byte at `at`, and the byte's place in it.
    fn place(at: u64) -> (u64, usize) {
        let block = Self::BLOCK as u64;
        (
            at / block,
            usize::try_from(at % block).expect("within a block"),
        )
    }

    pub(crate) fn new(capacity: u64) -> Self {
        Self {
            capacity,
            blocks: BTreeMap::new(),
        }
    }

    fn check(&self, at: u64, length: usize) -> Result<(), PastEnd> {
        at.checked_add(length as u64)
            .filter(|&end| end <= self.capacity)
            .map(drop)
            .ok_or(PastEnd)
    }
}

impl Disc for SparseDisc {
    type Error = PastEnd;

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), PastEnd> {
        self.check(at, out.len())?;
        let mut done = 0;
        while done < out.len() {
            let (block, offset) = Self::place(at + done as u64);
            let run = (Self::BLOCK - offset).min(out.len() - done);
            let into = &mut out[done..done + run];
            match self.blocks.get(&block) {
                Some(bytes) => into.copy_from_slice(&bytes[offset..offset + run]),
                None => into.fill(0),
            }
            done += run;
        }
        Ok(())
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), PastEnd> {
        self.check(at, bytes.len())?;
        let mut done = 0;
        while done < bytes.len() {
            let (block, offset) = Self::place(at + done as u64);
            let run = (Self::BLOCK - offset).min(bytes.len() - done);
            self.blocks.entry(block).or_insert([0; Self::BLOCK])[offset..offset + run]
                .copy_from_slice(&bytes[done..done + run]);
            done += run;
        }
        Ok(())
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), PastEnd> {
        let length = usize::try_from(length).map_err(|_| PastEnd)?;
        self.write(at, &vec![0; length])
    }

    fn flush(&mut self) -> Result<(), PastEnd> {
        Ok(())
    }
}

/// The non-zero bytes of the one map zone RISC OS 5 laid down for a 256 KiB
/// RAM disc, by offset: its header, its disc record, the map's and the root's
/// fragments, the free space's stop bit, and the fragment beyond the disc.
const RAM_DISC_ZONE: [(usize, u8); 38] = [
    (0, 0xBC),
    (1, 0x18),
    (2, 0x82),
    (3, 0xFF),
    (4, 0x0A),
    (5, 0x80),
    (6, 0x01),
    (7, 0x01),
    (8, 0x0D),
    (9, 0x07),
    (13, 0x01),
    (14, 0x20),
    (16, 0x01),
    (17, 0x03),
    (22, 0x04),
    (24, 0xF9),
    (25, 0x21),
    (26, b'R'),
    (27, b'a'),
    (28, b'm'),
    (29, b'D'),
    (30, b'i'),
    (31, b's'),
    (32, b'c'),
    (33, b'0'),
    (36, 0x18),
    (37, 0x8C),
    (38, 0x14),
    (39, 0x20),
    (48, 0x01),
    (53, 0x08),
    (64, 0x02),
    (65, 0x80),
    (66, 0x03),
    (67, 0x80),
    (319, 0x80),
    (320, 0x01),
    (1023, 0x80),
];

/// That zone, whole.
pub(crate) fn ram_disc_zone() -> [u8; 1024] {
    let mut zone = [0u8; 1024];
    for (at, byte) in RAM_DISC_ZONE {
        zone[at] = byte;
    }
    zone
}
