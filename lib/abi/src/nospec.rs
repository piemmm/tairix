//! Indices a mispredicted bounds check cannot carry past their table.
//!
//! A user-chosen index checked against a table's length and then used to
//! load from it is a Spectre v1 gadget: the core may run the load before the
//! check resolves, with the index the check would have refused. Every such
//! index on a path a caller reaches is passed through [`index_within`]
//! between the check and the load.

/// `index` when it is below `len`, else 0, computed without a branch.
///
/// The high bit of `index | (len - 1 - index)` is set exactly when
/// `index >= len`, so shifting its complement arithmetically spreads that
/// verdict into an all-ones or all-zero mask (Linux's
/// `array_index_mask_nospec`). Two optimisation barriers keep it a plain
/// data dependency: without the first, the compiler folds the mask to all
/// ones from the caller's own bounds check; without the second, it lowers
/// the mask to a `csel`/`cmov`, whose flags an Arm core may predict.
#[must_use]
pub fn index_within(index: usize, len: usize) -> usize {
    let index = core::hint::black_box(index);
    let verdict = index | len.wrapping_sub(1).wrapping_sub(index);
    let mask = ((!verdict).cast_signed() >> (usize::BITS - 1)).cast_unsigned();
    index & core::hint::black_box(mask)
}

#[cfg(test)]
mod tests {
    use super::index_within;

    #[test]
    fn the_branch_free_clamp_keeps_an_index_inside_its_table() {
        for len in [1, 2, 4, 144, 256, 1024, usize::MAX >> 1] {
            assert_eq!(index_within(0, len), 0);
            assert_eq!(index_within(len - 1, len), len - 1);
            assert_eq!(index_within(len, len), 0);
            assert_eq!(index_within(len + 1, len), 0);
            assert_eq!(index_within(usize::MAX >> 1, len.min(1023)), 0);
        }
        for raw in 0..=4096 {
            for len in [1, 4, 145, 256] {
                let clamped = index_within(raw, len);
                assert!(clamped < len);
                assert_eq!(clamped, if raw < len { raw } else { 0 });
            }
        }
    }
}
