//! Commodore's group code recording: each four bits written as five, chosen
//! so that no more than two zeros ever run together and the drive's read
//! clock never loses step, and ten ones in a row — never a code — mark a
//! SYNC.

use alloc::vec::Vec;

/// Each nybble's five-bit code.
pub const CODES: [u8; 16] = [
    0x0A, 0x0B, 0x12, 0x13, 0x0E, 0x0F, 0x16, 0x17, 0x09, 0x19, 0x1A, 0x1B, 0x0D, 0x1D, 0x1E, 0x15,
];

/// Each five-bit value's nybble, where it is a code.
const NYBBLES: [Option<u8>; 32] = {
    let mut table = [None; 32];
    let mut nybble: u8 = 0;
    while nybble < 16 {
        table[CODES[nybble as usize] as usize] = Some(nybble);
        nybble += 1;
    }
    table
};

/// Four bytes as the five their codes fill, high nybble first.
#[must_use]
pub fn encode_group(bytes: [u8; 4]) -> [u8; 5] {
    let mut bits = 0u64;
    for byte in bytes {
        bits = bits << 10
            | u64::from(CODES[usize::from(byte >> 4)]) << 5
            | u64::from(CODES[usize::from(byte & 0x0F)]);
    }
    let mut gcr = [0; 5];
    gcr.copy_from_slice(&bits.to_be_bytes()[3..]);
    gcr
}

/// Five bytes of codes as the four they record, if every code is one.
#[must_use]
pub fn decode_group(gcr: [u8; 5]) -> Option<[u8; 4]> {
    let mut padded = [0; 8];
    padded[3..].copy_from_slice(&gcr);
    let bits = u64::from_be_bytes(padded);
    let mut bytes = [0; 4];
    for (i, byte) in bytes.iter_mut().enumerate() {
        let shift = 30 - 10 * i;
        let high = NYBBLES[usize::try_from((bits >> (shift + 5)) & 0x1F).ok()?]?;
        let low = NYBBLES[usize::try_from((bits >> shift) & 0x1F).ok()?]?;
        *byte = high << 4 | low;
    }
    Some(bytes)
}

/// `bytes`, a whole number of groups of four, encoded onto `out`.
pub fn encode(bytes: &[u8], out: &mut Vec<u8>) {
    for group in bytes.as_chunks::<4>().0 {
        out.extend(encode_group(*group));
    }
}

/// `gcr`, a whole number of groups of five, decoded, if every code is one.
#[must_use]
pub fn decode(gcr: &[u8]) -> Option<Vec<u8>> {
    let (groups, rest) = gcr.as_chunks::<5>();
    if !rest.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(groups.len() * 4);
    for group in groups {
        bytes.extend(decode_group(*group)?);
    }
    Some(bytes)
}

/// The first `count` bytes `gcr` records, if each of their codes is one;
/// codes past them are not looked at.
#[must_use]
pub fn decode_prefix(gcr: &[u8], count: usize) -> Option<Vec<u8>> {
    let code = |index: usize| -> Option<u8> {
        let first = index * 5;
        let window = [
            *gcr.get(first / 8)?,
            gcr.get(first / 8 + 1).copied().unwrap_or(0),
        ];
        let bits = u16::from_be_bytes(window) >> (11 - first % 8);
        NYBBLES[usize::from(bits & 0x1F)]
    };
    (0..count)
        .map(|byte| Some(code(2 * byte)? << 4 | code(2 * byte + 1)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_decodes_to_what_was_encoded() {
        for first in 0..=255u8 {
            let group = [first, !first, first.rotate_left(3), 0x5A];
            assert_eq!(decode_group(encode_group(group)), Some(group));
        }
    }

    #[test]
    fn no_code_runs_more_than_two_zeros_or_eight_ones() {
        for &code in &CODES {
            assert!(
                code & 0x1C != 0 && code & 0x0E != 0 && code & 0x07 != 0,
                "{code:05b}"
            );
        }
        let mut gcr = Vec::new();
        encode(&[0xFF; 4], &mut gcr);
        assert_eq!(gcr, [0xAD, 0x6B, 0x5A, 0xD6, 0xB5], "%10101 eight times");
    }

    #[test]
    fn a_prefix_decodes_without_the_codes_after_it() {
        let mut gcr = Vec::new();
        encode(&[0x07, 0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x00], &mut gcr);
        gcr[9] = 0xFF;
        assert_eq!(decode(&gcr), None);
        assert_eq!(
            decode_prefix(&gcr, 6),
            Some(alloc::vec![0x07, 0x12, 0x34, 0x56, 0x78, 0x00])
        );
        assert_eq!(decode_prefix(&gcr, 8), None);
        assert_eq!(decode_prefix(&gcr[..3], 4), None, "too short");
    }

    #[test]
    fn a_non_code_does_not_decode() {
        assert_eq!(decode_group([0xFF; 5]), None);
        assert_eq!(decode(&[0x52, 0x54, 0xB5, 0x29]), None, "not whole groups");
    }
}
