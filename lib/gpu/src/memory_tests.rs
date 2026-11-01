use alloc::vec;
use alloc::vec::Vec;

use super::{read_bytes, write_bytes};

/// Every start and length within three words, read and written, against a
/// byte array.
#[test]
fn bytes_land_where_asked_at_every_alignment() {
    for start in 0..12usize {
        for len in 0..=12 - start {
            let mut words = vec![0x0302_0100u32, 0x0706_0504, 0x0b0a_0908];
            let mut reference: Vec<u8> = (0..12).collect();
            let mut out = vec![0u8; len];
            read_bytes(&words, start, &mut out).expect("within");
            assert_eq!(out, reference[start..start + len], "read {start}+{len}");
            let data: Vec<u8> = (0..len)
                .map(|i| 0xf0 | u8::try_from(i).unwrap_or(0))
                .collect();
            write_bytes(&mut words, start, &data).expect("within");
            reference[start..start + len].copy_from_slice(&data);
            let stored: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            assert_eq!(stored, reference, "write {start}+{len}");
        }
    }
}

#[test]
fn bytes_past_the_words_are_refused_and_change_nothing() {
    let mut words = vec![0u32; 2];
    assert_eq!(read_bytes(&words, 5, &mut [0u8; 4]), None);
    assert_eq!(write_bytes(&mut words, 7, &[1, 2]), None);
    assert_eq!(write_bytes(&mut words, usize::MAX, &[1]), None);
    assert_eq!(words, [0, 0]);
}
