//! A set of gigapages: one bit per 1 GiB slot of a 512-entry table level,
//! the unit a port plans its identity window and its direct map in.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::frames::PAGE_TABLE_ENTRIES;

/// Words of a [`GigapageMask`].
pub const MASK_WORDS: usize = PAGE_TABLE_ENTRIES / 64;

/// One bit per gigapage below [`PAGE_TABLE_ENTRIES`] GiB.
pub type GigapageMask = [u64; MASK_WORDS];

/// Every gigapage an extent `(base, len)` of `extents` overlaps, below
/// [`PAGE_TABLE_ENTRIES`] GiB; an empty extent overlaps none.
#[must_use]
pub fn from_extents(extents: &[(u64, u64)]) -> GigapageMask {
    let mut mask = [0u64; MASK_WORDS];
    for &(base, len) in extents {
        let Some(last) = len.checked_sub(1) else {
            continue;
        };
        let first = base >> 30;
        let last = base.saturating_add(last) >> 30;
        for index in first..=last.min(PAGE_TABLE_ENTRIES as u64 - 1) {
            // Below `PAGE_TABLE_ENTRIES`, by the bound above.
            #[allow(clippy::cast_possible_truncation)]
            let index = index as usize;
            mask[index / 64] |= 1 << (index % 64);
        }
    }
    mask
}

/// Whether `word`, the mask word covering gigapage `index`, holds it.
#[must_use]
pub const fn word_holds(word: u64, index: usize) -> bool {
    word & (1 << (index % 64)) != 0
}

/// Whether `mask` holds gigapage `index`.
#[must_use]
pub const fn holds(mask: &GigapageMask, index: usize) -> bool {
    index < PAGE_TABLE_ENTRIES && word_holds(mask[index / 64], index)
}

/// Whether `mask` holds every gigapage `[phys, phys + len)` touches, all of
/// them below `limit`. Fails closed on an empty or wrapping range.
#[must_use]
pub fn covers(mask: &[AtomicU64; MASK_WORDS], limit: usize, phys: u64, len: u64) -> bool {
    let Some(last) = len.checked_sub(1).and_then(|off| phys.checked_add(off)) else {
        return false;
    };
    let (Ok(first), Ok(last)) = (usize::try_from(phys >> 30), usize::try_from(last >> 30)) else {
        return false;
    };
    last < limit.min(PAGE_TABLE_ENTRIES)
        && (first..=last).all(|index| word_holds(mask[index / 64].load(Ordering::Acquire), index))
}

/// Gigapages of device registers the kernel reaches through its direct map:
/// those the boot path names before the map is published, and those the live
/// map carries once it is.
pub struct KernelDevices {
    named: [AtomicU64; MASK_WORDS],
    mapped: [AtomicU64; MASK_WORDS],
    /// Gigapages the map can carry.
    limit: usize,
}

impl KernelDevices {
    /// None named or carried, by a map carrying at most `limit` gigapages.
    #[must_use]
    pub const fn new(limit: usize) -> Self {
        Self {
            named: [const { AtomicU64::new(0) }; MASK_WORDS],
            mapped: [const { AtomicU64::new(0) }; MASK_WORDS],
            limit,
        }
    }

    /// Name `mask`'s gigapages, before the map is published.
    pub fn name(&self, mask: GigapageMask) {
        for (slot, word) in self.named.iter().zip(mask) {
            slot.store(word, Ordering::Release);
        }
    }

    /// Whether gigapage `index` was named.
    #[must_use]
    pub fn named(&self, index: usize) -> bool {
        index < PAGE_TABLE_ENTRIES
            && word_holds(self.named[index / 64].load(Ordering::Acquire), index)
    }

    /// Record `mask` as what the live map carries.
    pub fn publish(&self, mask: GigapageMask) {
        for (slot, word) in self.mapped.iter().zip(mask) {
            slot.store(word, Ordering::Release);
        }
    }

    /// Whether the live map carries every byte of `[phys, phys + len)`.
    /// Fails closed on an empty, wrapping or over-wide range.
    #[must_use]
    pub fn covers(&self, phys: u64, len: u64) -> bool {
        covers(&self.mapped, self.limit, phys, len)
    }

    /// One past the highest physical address the map can carry them at.
    #[must_use]
    pub const fn reach(&self) -> u64 {
        (self.limit as u64) << 30
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_devices_cover_only_what_the_map_published() {
        let devices = KernelDevices::new(64);
        devices.name(from_extents(&[(16 * GIB, GIB)]));
        assert!(devices.named(16));
        assert!(!devices.named(17));
        assert!(!devices.named(PAGE_TABLE_ENTRIES));
        assert!(!devices.covers(16 * GIB, 1), "named is not yet carried");
        devices.publish(from_extents(&[(16 * GIB, GIB)]));
        assert!(devices.covers(16 * GIB, GIB));
        assert!(!devices.covers(16 * GIB, GIB + 1));
        assert_eq!(devices.reach(), 64 * GIB);
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn every_overlapped_gigapage_is_held() {
        let mask = from_extents(&[
            (0x8_0000, 0x10_0000),
            // Straddling the gigapage 3 / 4 boundary marks both.
            (4 * GIB - 0x10, 0x20),
            (0x40 * GIB, 0),
        ]);
        assert_eq!(mask[0], 0b1_1001);
        assert_eq!(mask[1..], [0; MASK_WORDS - 1]);
    }

    #[test]
    fn an_extent_past_the_level_is_clamped_to_its_last_gigapage() {
        assert_eq!(from_extents(&[(511 * GIB, 4 * GIB)])[7], 1 << 63);
        assert_eq!(from_extents(&[(512 * GIB, GIB)]), [0; MASK_WORDS]);
        assert_eq!(
            from_extents(&[(511 * GIB, u64::MAX)])[7],
            1 << 63,
            "a wrapping length saturates"
        );
        assert_eq!(
            from_extents(&[(u64::MAX, 2)]),
            [0; MASK_WORDS],
            "starts past it"
        );
    }

    #[test]
    fn holds_reads_one_bit_and_nothing_past_the_level() {
        let mask = from_extents(&[(17 * GIB, 1)]);
        assert!(holds(&mask, 17));
        assert!(!holds(&mask, 16));
        assert!(!holds(&mask, PAGE_TABLE_ENTRIES));
    }

    #[test]
    fn a_range_is_covered_only_where_every_gigapage_it_touches_is_held_below_the_limit() {
        let mask = from_extents(&[(16 * GIB, 2 * GIB)]).map(AtomicU64::new);
        assert!(covers(&mask, 64, 16 * GIB, 2 * GIB));
        assert!(covers(&mask, 64, 17 * GIB + 5, 10));
        assert!(
            !covers(&mask, 64, 17 * GIB, 2 * GIB),
            "runs into gigapage 18"
        );
        assert!(!covers(&mask, 17, 16 * GIB, 2 * GIB), "past the limit");
        assert!(!covers(&mask, 64, 16 * GIB, 0), "an empty range");
        assert!(!covers(&mask, 64, u64::MAX, 2), "a wrapping one");
    }
}
