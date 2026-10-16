//! The sparse fixture format the large e2fsprogs-made images are stored in:
//! `TXSPARSE`, the block size (le32), the block count (le64), the stored
//! block count (le32), then each stored block as its index (le64) and bytes.
//! Every block not stored reads as zero.

extern crate alloc;

use alloc::vec::Vec;

/// The image `fixture` stores.
///
/// # Panics
///
/// On a fixture that is not whole: it is test data built in-tree.
#[must_use]
pub fn sparse(fixture: &[u8]) -> Vec<u8> {
    assert_eq!(&fixture[..8], b"TXSPARSE", "a sparse fixture");
    let word = |at: usize| u32::from_le_bytes(fixture[at..at + 4].try_into().expect("four bytes"));
    let wide = |at: usize| u64::from_le_bytes(fixture[at..at + 8].try_into().expect("eight bytes"));
    let bs = usize::try_from(word(8)).expect("fits");
    let blocks = usize::try_from(wide(12)).expect("fits");
    let mut image = alloc::vec![0u8; bs * blocks];
    let mut at = 24;
    for _ in 0..word(20) {
        let index = usize::try_from(wide(at)).expect("fits");
        image[index * bs..(index + 1) * bs].copy_from_slice(&fixture[at + 8..at + 8 + bs]);
        at += 8 + bs;
    }
    assert_eq!(at, fixture.len(), "the fixture is whole");
    image
}
