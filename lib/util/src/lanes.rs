//! Eight bytes at a time: which byte lanes of a word hold a given byte. The
//! portable search for a target whose vector unit is off, where a loop over
//! bytes is not vectorised for it.

/// Every byte's high bit: where [`equal_lanes`] marks a matching lane.
pub const HIGH: u64 = 0x8080_8080_8080_8080;
/// Every byte's low seven bits.
const SEVEN: u64 = 0x7f7f_7f7f_7f7f_7f7f;
/// Every byte's low bit.
const LOW: u64 = 0x0101_0101_0101_0101;

/// The lanes of `word` whose byte equals `byte`, as their high bits.
///
/// Adding `0x7f` to a lane's low seven bits carries into bit 7 unless those
/// bits are zero, and can never carry *out* of the lane — so unlike the
/// shorter `(x - 1) & !x` zero-byte test this one cannot let one lane's
/// borrow forge a match in the next. A caller counting or locating matches
/// needs that exactness.
#[must_use]
pub const fn equal_lanes(word: u64, byte: u8) -> u64 {
    let x = word ^ LOW.wrapping_mul(byte as u64);
    let nonzero = ((x & SEVEN).wrapping_add(SEVEN) | x) & HIGH;
    !nonzero & HIGH
}

/// How many of `bytes` equal `byte`.
#[must_use]
pub fn count(bytes: &[u8], byte: u8) -> usize {
    let (words, rest) = bytes.as_chunks::<8>();
    let lanes: usize = words
        .iter()
        .map(|word| equal_lanes(u64::from_le_bytes(*word), byte).count_ones() as usize)
        .sum();
    lanes + rest.iter().fold(0, |n, &b| n + usize::from(b == byte))
}

/// Where the `nth` (from 1) of `bytes` equal to `byte` lies.
#[must_use]
pub fn nth(bytes: &[u8], byte: u8, nth: usize) -> Option<usize> {
    let (words, rest) = bytes.as_chunks::<8>();
    let mut left = nth.checked_sub(1)?;
    for (index, word) in words.iter().enumerate() {
        let mut lanes = equal_lanes(u64::from_le_bytes(*word), byte);
        let here = lanes.count_ones() as usize;
        if left < here {
            for _ in 0..left {
                lanes &= lanes - 1;
            }
            return Some(index * 8 + (lanes.trailing_zeros() / 8) as usize);
        }
        left -= here;
    }
    rest.iter()
        .enumerate()
        .filter(|&(_, &b)| b == byte)
        .nth(left)
        .map(|(at, _)| words.len() * 8 + at)
}

/// How many of `bytes` equal `byte`, and where the first and the last of
/// them lie, in one pass.
#[must_use]
pub fn span(bytes: &[u8], byte: u8) -> (usize, Option<(usize, usize)>) {
    let (words, rest) = bytes.as_chunks::<8>();
    let mut count = 0;
    let mut ends: Option<(usize, usize)> = None;
    let mut found = |first: usize, last: usize| {
        ends = Some(ends.map_or((first, last), |(seen, _)| (seen, last)));
    };
    for (index, word) in words.iter().enumerate() {
        let lanes = equal_lanes(u64::from_le_bytes(*word), byte);
        if lanes != 0 {
            count += lanes.count_ones() as usize;
            let base = index * 8;
            found(
                base + (lanes.trailing_zeros() / 8) as usize,
                base + (lanes.ilog2() / 8) as usize,
            );
        }
    }
    for (at, _) in rest.iter().enumerate().filter(|&(_, &b)| b == byte) {
        count += 1;
        let at = words.len() * 8 + at;
        found(at, at);
    }
    (count, ends)
}

#[cfg(test)]
mod tests {
    use super::{count, equal_lanes, nth, span, HIGH};

    #[test]
    fn a_lane_matches_exactly_whatever_its_neighbours_hold() {
        for byte in [0u8, 0x0a, 0x7f, 0x80, 0xff] {
            for other in [
                0u8,
                1,
                byte.wrapping_add(1),
                byte.wrapping_sub(1),
                0x80,
                0xff,
            ] {
                if other == byte {
                    continue;
                }
                let mut lanes = [other; 8];
                lanes[3] = byte;
                let found = equal_lanes(u64::from_le_bytes(lanes), byte);
                assert_eq!(found, 0x80 << 24, "{byte:#x} among {other:#x}");
            }
            assert_eq!(equal_lanes(u64::from_le_bytes([byte; 8]), byte), HIGH);
        }
    }

    #[test]
    fn count_and_nth_agree_with_a_byte_by_byte_scan() {
        let bytes: alloc::vec::Vec<u8> = (0..517u32)
            .map(|at| {
                if at % 7 == 0 || at % 11 == 3 {
                    b'\n'
                } else {
                    (at % 251) as u8
                }
            })
            .collect();
        for from in [0usize, 1, 5, 8, 9, 500] {
            let slice = &bytes[from..];
            let want: alloc::vec::Vec<usize> = slice
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(at, _)| at)
                .collect();
            assert_eq!(count(slice, b'\n'), want.len(), "from {from}");
            for (index, &at) in want.iter().enumerate() {
                assert_eq!(nth(slice, b'\n', index + 1), Some(at), "from {from}");
            }
            assert_eq!(nth(slice, b'\n', want.len() + 1), None);
            assert_eq!(nth(slice, b'\n', 0), None, "the first is the 1st");
        }
    }

    #[test]
    fn span_agrees_with_a_byte_by_byte_scan() {
        let mut cases: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec![
            alloc::vec![],
            alloc::vec![b'x'; 23],
            b"\n".to_vec(),
            b"abcdefg\n".to_vec(),
            b"\nbcdefgh".to_vec(),
            b"abcdefgh\nj".to_vec(),
        ];
        for len in [7usize, 8, 9, 16, 17, 64] {
            for at in [0, len / 2, len - 1] {
                let mut bytes = alloc::vec![b'y'; len];
                bytes[at] = b'\n';
                cases.push(bytes.clone());
                bytes[len - 1 - at] = b'\n';
                cases.push(bytes);
            }
        }
        for bytes in cases {
            let at: alloc::vec::Vec<usize> = bytes
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(at, _)| at)
                .collect();
            let want = (at.len(), at.first().copied().zip(at.last().copied()));
            assert_eq!(span(&bytes, b'\n'), want, "{bytes:?}");
        }
    }
}
