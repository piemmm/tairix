//! The length and distance slots RAR 2's LZ coding defines, which RAR 3's
//! keeps and extends with twelve wider distance slots.

/// Length slots, which a repeated distance's length is coded in too.
pub(super) const LEN_SLOTS: usize = 28;

/// A length slot's base.
pub(super) const LEN_BASE: [u32; LEN_SLOTS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224,
];

/// A length slot's extra bits.
pub(super) const LEN_BITS: [u32; LEN_SLOTS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
];

/// A short distance's base.
pub(super) const SHORT_BASE: [u64; 8] = [0, 4, 8, 16, 32, 64, 128, 192];

/// A short distance's extra bits.
pub(super) const SHORT_BITS: [u32; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

/// Distance slots: RAR 2 codes the first 48, RAR 3 all of them.
pub(super) const DIST_SLOTS: usize = 60;

/// A distance slot's base and extra bits: bits in pairs from 0 to 16,
/// twelve of 16, then twelve of 18; each base the slots below it, each its
/// extra bits' worth.
pub(super) const DIST: [(u64, u32); DIST_SLOTS] = {
    let mut table = [(0, 0); DIST_SLOTS];
    let mut base = 0;
    let mut slot = 0u32;
    while (slot as usize) < DIST_SLOTS {
        let bits = if slot < 4 {
            0
        } else if slot < 36 {
            (slot - 2) / 2
        } else if slot < 48 {
            16
        } else {
            18
        };
        table[slot as usize] = (base, bits);
        base += 1 << bits;
        slot += 1;
    }
    table
};
