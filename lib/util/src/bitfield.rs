//! Fields of bits packed along a byte row, the first byte's lowest bit first,
//! as packed pixels lie: at any bit, up to a word wide.

/// The `bits` at bit `bit` of `row`, at most 32; bits past its end read as
/// nought.
#[must_use]
pub fn field_at(row: &[u8], bit: u64, bits: u32) -> u32 {
    let rest = usize::try_from(bit / 8)
        .ok()
        .and_then(|first| row.get(first..))
        .unwrap_or_default();
    let (shift, bits) = ((bit % 8) as u32, bits.min(32));
    if shift + bits <= 8 {
        return rest
            .first()
            .map_or(0, |&byte| (u32::from(byte) >> shift) & ((1 << bits) - 1));
    }
    let window = if let Some(chunk) = rest.first_chunk::<8>() {
        u64::from_le_bytes(*chunk)
    } else {
        let mut window = [0u8; 8];
        for (slot, &byte) in window.iter_mut().zip(rest) {
            *slot = byte;
        }
        u64::from_le_bytes(window)
    };
    u32::try_from((window >> shift) & mask(bits)).unwrap_or(0)
}

/// Sets the `bits` at bit `bit` of `row`, at most 32, to `value`'s low bits;
/// bits past its end are dropped.
pub fn set_field_at(row: &mut [u8], bit: u64, bits: u32, value: u32) {
    let Some(rest) = usize::try_from(bit / 8)
        .ok()
        .and_then(|first| row.get_mut(first..))
    else {
        return;
    };
    let (shift, bits) = ((bit % 8) as u32, bits.min(32));
    if shift == 0 && bits.is_multiple_of(8) {
        let whole = rest.len().min(bits as usize / 8);
        rest[..whole].copy_from_slice(&value.to_le_bytes()[..whole]);
        return;
    }
    // Only the bytes the field spans: a wider store would stall the next
    // field's load on the store it overlaps.
    let spanned = (shift + bits).div_ceil(8) as usize;
    let mask = mask(bits) << shift;
    let placed = (u64::from(value) << shift) & mask;
    let lanes = mask.to_le_bytes().into_iter().zip(placed.to_le_bytes());
    for (byte, (m, v)) in rest.iter_mut().zip(lanes).take(spanned) {
        *byte = (*byte & !m) | v;
    }
}

/// A mask of `bits` low bits, a word's at most.
#[must_use]
pub const fn mask(bits: u32) -> u64 {
    (1 << if bits < 32 { bits } else { 32 }) - 1
}

#[cfg(test)]
mod tests {
    use super::{field_at, set_field_at};

    /// The field as read a bit at a time.
    fn slowly(row: &[u8], bit: u64, bits: u32) -> u32 {
        (0..bits.min(32)).fold(0, |value, i| {
            let at = bit + u64::from(i);
            let byte = usize::try_from(at / 8)
                .ok()
                .and_then(|n| row.get(n))
                .copied()
                .unwrap_or(0);
            value | (u32::from((byte >> (at % 8)) & 1) << i)
        })
    }

    #[test]
    fn a_field_reads_as_its_bits_do_at_every_place_and_width() {
        let row: [u8; 12] = core::array::from_fn(|i| i.to_le_bytes()[0].wrapping_mul(0x9D) ^ 0x5A);
        for bits in [0, 1, 2, 4, 5, 8, 15, 16, 24, 31, 32, 40] {
            for bit in 0..96 + 8 {
                assert_eq!(
                    field_at(&row, bit, bits),
                    slowly(&row, bit, bits),
                    "{bits} bits at bit {bit}"
                );
            }
        }
        assert_eq!(field_at(&row, u64::MAX, 8), 0, "past any row");
        for bit in 0..8 {
            assert_eq!(
                field_at(&row, bit, u32::MAX),
                field_at(&row, bit, 32),
                "a width past a word reads a word at bit {bit}"
            );
        }
    }

    #[test]
    fn a_field_written_reads_back_and_leaves_its_neighbours_alone() {
        for bits in [1, 2, 4, 8, 13, 16, 24, 32] {
            for bit in 0..96 {
                let mut row = [0xA5u8; 12];
                let before = row;
                set_field_at(&mut row, bit, bits, 0x1234_5678);
                let fits = bit + u64::from(bits) <= 96;
                if fits {
                    let wanted = 0x1234_5678 & (u32::MAX >> (32 - bits));
                    assert_eq!(field_at(&row, bit, bits), wanted, "{bits} at {bit}");
                }
                for other in (0..96).filter(|&b| b < bit || b >= bit + u64::from(bits)) {
                    assert_eq!(
                        slowly(&row, other, 1),
                        slowly(&before, other, 1),
                        "bit {other} kept by {bits} at {bit}"
                    );
                }
            }
        }
        let mut row = [0u8; 4];
        set_field_at(&mut row, u64::MAX, 8, 0xFF);
        assert_eq!(row, [0; 4], "past any row dropped");
    }
}
