//! Where the RAM went: the parts a memory composition is drawn as, from the
//! bytes the kernel charges to each memory class.
//!
//! The kernel charges every frame to exactly one class at allocation and
//! discharges it from the same one at free, so the classes partition the RAM
//! in use, and the free remainder closes the whole: the parts can never
//! account for more than there is. The Switchboard's memory pane and the
//! System Monitor screensaver both draw this one composition.

use alloc::vec::Vec;

use tairix_abi::{MemoryClass, MEMORY_CLASS_COUNT};

/// One part of a memory composition: a class the kernel charges frames to,
/// or — `class` being `None` — the free remainder closing the whole.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MemoryPart {
    /// The class, or `None` for the free remainder.
    pub class: Option<MemoryClass>,
    /// The bytes the part stands for: the class's charge, or the bytes free.
    pub bytes: u64,
    /// The part's share of the whole, in permille.
    pub share: u16,
}

impl MemoryPart {
    /// How the part reads to someone looking at their own machine, rather
    /// than in the kernel's charging vocabulary.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self.class {
            Some(MemoryClass::UserAnon) => "Processes",
            Some(MemoryClass::UserFile) => "File cache",
            Some(MemoryClass::PageTable) => "Page tables",
            Some(MemoryClass::Kernel) => "Kernel",
            Some(MemoryClass::Dma) => "Device buffers",
            Some(MemoryClass::Compressed) => "Compressed",
            None => "Free",
        }
    }

    /// Whether this is the free remainder, which is always last.
    #[must_use]
    pub const fn is_remainder(&self) -> bool {
        self.class.is_none()
    }
}

/// The composition of `total_bytes`: one part per class holding anything, in
/// [`MemoryClass::ALL`] order, then the free remainder; `None` for a zero
/// total, which nothing can be a share of.
///
/// A class holding nothing is left out rather than drawn as a part of no
/// width. Each class's share is rounded down and the remainder takes what that
/// leaves, so the shares sum to exactly a thousand.
#[must_use]
pub fn memory_composition(
    class_bytes: &[u64; MEMORY_CLASS_COUNT],
    free_bytes: u64,
    total_bytes: u64,
) -> Option<Vec<MemoryPart>> {
    if total_bytes == 0 {
        return None;
    }
    let mut parts: Vec<MemoryPart> = MemoryClass::ALL
        .iter()
        .map(|&class| (class, class_bytes[class.index()]))
        .filter(|&(_, bytes)| bytes > 0)
        .map(|(class, bytes)| MemoryPart {
            class: Some(class),
            bytes,
            share: share_of(bytes, total_bytes),
        })
        .collect();
    let named: u32 = parts.iter().map(|part| u32::from(part.share)).sum();
    parts.push(MemoryPart {
        class: None,
        bytes: free_bytes,
        share: u16::try_from(1_000u32.saturating_sub(named)).unwrap_or(0),
    });
    Some(parts)
}

/// `bytes` as a permille of `total`, which is not zero, saturating at full.
fn share_of(bytes: u64, total: u64) -> u16 {
    let share = u128::from(bytes) * 1_000 / u128::from(total);
    u16::try_from(share.min(1_000)).unwrap_or(1_000)
}

#[cfg(test)]
mod tests {
    use tairix_abi::{MemoryClass, MEMORY_CLASS_COUNT};

    use super::memory_composition;

    #[test]
    fn the_shares_close_the_whole_exactly() {
        let mut classes = [0u64; MEMORY_CLASS_COUNT];
        classes[MemoryClass::UserAnon.index()] = 333;
        classes[MemoryClass::Kernel.index()] = 333;
        let parts = memory_composition(&classes, 334, 1_000).expect("a whole");
        let total: u32 = parts.iter().map(|part| u32::from(part.share)).sum();
        assert_eq!(total, 1_000);
        assert_eq!(parts.len(), 3, "two classes and the remainder");
    }

    #[test]
    fn a_class_holding_nothing_is_left_out_and_the_remainder_is_last() {
        let mut classes = [0u64; MEMORY_CLASS_COUNT];
        classes[MemoryClass::UserFile.index()] = 500;
        let parts = memory_composition(&classes, 500, 1_000).expect("a whole");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].label(), "File cache");
        assert_eq!(parts[0].share, 500);
        assert!(parts[1].is_remainder());
        assert_eq!(parts[1].label(), "Free");
        assert_eq!(parts[1].bytes, 500);
    }

    #[test]
    fn rounding_and_unaccounted_bytes_fall_to_the_remainder() {
        let mut classes = [0u64; MEMORY_CLASS_COUNT];
        classes[MemoryClass::UserAnon.index()] = 1;
        let parts = memory_composition(&classes, 0, 3).expect("a whole");
        assert_eq!(parts[0].share, 333);
        assert_eq!(parts[1].share, 667);
    }

    #[test]
    fn a_zero_whole_has_no_composition() {
        assert!(memory_composition(&[0; MEMORY_CLASS_COUNT], 0, 0).is_none());
    }

    #[test]
    fn every_class_has_a_reader_facing_name_of_its_own() {
        let mut labels: alloc::vec::Vec<&str> = MemoryClass::ALL
            .iter()
            .map(|&class| {
                super::MemoryPart {
                    class: Some(class),
                    bytes: 1,
                    share: 1,
                }
                .label()
            })
            .collect();
        labels.push("Free");
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "two parts would read alike");
    }
}
