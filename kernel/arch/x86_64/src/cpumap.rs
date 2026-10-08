//! The dense CPU an APIC id names: the reverse of the arch handle's forward
//! map, sorted by APIC id once the CPUs are known, so a lookup is a binary
//! search over the machine's own CPUs whatever width its ids have.

use core::sync::atomic::{AtomicU64, Ordering};

/// A slot holding no CPU. It sorts past every slot that does, and names the
/// x2APIC broadcast id, which no CPU has.
pub(crate) const VACANT: u64 = u64::MAX;

/// The x2APIC broadcast id, which no CPU has: what a forward-map slot naming
/// no CPU holds.
pub(crate) const NO_LAPIC: u32 = u32::MAX;

/// Why a forward map has no reverse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unmappable {
    /// Two CPUs claim one APIC id.
    Duplicate,
    /// A CPU claims the broadcast id, or the slots cannot hold every CPU.
    OutOfRange,
}

/// APIC id → dense CPU id over one slot per CPU of the caller's storage:
/// `apic << 32 | cpu`, ascending, every slot past the last all ones.
#[derive(Clone, Copy, Debug)]
pub struct ApicMap<'s> {
    slots: &'s [AtomicU64],
}

#[cfg(any(test, feature = "sched-arch"))]
impl<'s> ApicMap<'s> {
    /// The map of `cpus`, each a dense CPU id and its APIC id, written into
    /// `slots` once, before any lookup.
    ///
    /// # Errors
    ///
    /// [`Unmappable`] for a map with no reverse.
    pub(crate) fn build(
        slots: &'s [AtomicU64],
        cpus: impl IntoIterator<Item = (u32, u32)>,
    ) -> Result<Self, Unmappable> {
        let mut len = 0;
        for (cpu, apic) in cpus {
            if apic == NO_LAPIC || len == slots.len() {
                return Err(Unmappable::OutOfRange);
            }
            let at = Self::first_at_or_above(&slots[..len], apic);
            if at < len && Self::apic_of(slots[at].load(Ordering::Relaxed)) == apic {
                return Err(Unmappable::Duplicate);
            }
            for index in (at..len).rev() {
                slots[index + 1].store(slots[index].load(Ordering::Relaxed), Ordering::Relaxed);
            }
            slots[at].store(u64::from(apic) << 32 | u64::from(cpu), Ordering::Relaxed);
            len += 1;
        }
        for slot in &slots[len..] {
            slot.store(VACANT, Ordering::Relaxed);
        }
        Ok(Self { slots })
    }
}

impl ApicMap<'_> {
    /// Whether the map lives in `slots`.
    #[must_use]
    pub fn is_backed_by(&self, slots: &[AtomicU64]) -> bool {
        core::ptr::eq(self.slots.as_ptr(), slots.as_ptr())
    }

    /// Whether `cpus`, each a dense CPU id and its APIC id, are every CPU
    /// the map holds and no other.
    #[must_use]
    pub fn holds_exactly(&self, cpus: impl IntoIterator<Item = (u32, u32)>) -> bool {
        let mut named = 0;
        for (cpu, apic) in cpus {
            if self.cpu_of(apic) != Some(cpu) {
                return false;
            }
            named += 1;
        }
        let held = self
            .slots
            .iter()
            .take_while(|slot| slot.load(Ordering::Relaxed) != VACANT)
            .count();
        named == held
    }

    /// The dense id of the CPU whose APIC id is `apic`, or [`None`] for one
    /// no CPU has.
    #[must_use]
    pub fn cpu_of(&self, apic: u32) -> Option<u32> {
        if apic == NO_LAPIC {
            return None;
        }
        let at = Self::first_at_or_above(self.slots, apic);
        let slot = self.slots.get(at)?.load(Ordering::Relaxed);
        // The low half is the CPU id, by construction.
        #[allow(clippy::cast_possible_truncation)]
        (Self::apic_of(slot) == apic).then_some(slot as u32)
    }

    /// The first of `sorted` whose APIC id is `apic` or above.
    fn first_at_or_above(sorted: &[AtomicU64], apic: u32) -> usize {
        sorted.partition_point(|slot| Self::apic_of(slot.load(Ordering::Relaxed)) < apic)
    }

    const fn apic_of(slot: u64) -> u32 {
        // The high half, by construction.
        #[allow(clippy::cast_possible_truncation)]
        let apic = (slot >> 32) as u32;
        apic
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
mod live {
    use tairix_sync::once::OnceCell;

    use super::ApicMap;

    /// The boot's map, published by the arch handle before any other CPU
    /// starts or any interrupt is taken.
    static PUBLISHED: OnceCell<ApicMap<'static>> = OnceCell::new();

    /// Publish `map` for [`published`]: once per boot, a second ignored.
    #[cfg(feature = "sched-arch")]
    pub(crate) fn publish(map: ApicMap<'static>) {
        let _ = PUBLISHED.set(map);
    }

    /// The boot's map, or [`None`] before it is published.
    #[must_use]
    pub fn published() -> Option<ApicMap<'static>> {
        PUBLISHED.get().ok().flatten().copied()
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none", feature = "sched-arch"))]
pub(crate) use live::publish;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub use live::published;

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    fn slots<const N: usize>() -> [AtomicU64; N] {
        [const { AtomicU64::new(0) }; N]
    }

    #[test]
    fn every_cpu_is_found_by_its_apic_id_whatever_its_width() {
        let ids = [
            (0, 0x1_0002),
            (1, 0),
            (2, 0x100),
            (3, 0xFE),
            (4, 0xFFFF_FFFE),
        ];
        let slots = slots::<6>();
        let map = ApicMap::build(&slots, ids).expect("a reverse map");
        for (cpu, apic) in ids {
            assert_eq!(map.cpu_of(apic), Some(cpu));
        }
        for absent in [1, 0xFF, 0x101, 0x1_0001, NO_LAPIC] {
            assert_eq!(map.cpu_of(absent), None, "{absent:#x} names no CPU");
        }
    }

    #[test]
    fn the_slots_are_sorted_by_apic_id_with_the_vacant_last() {
        let slots = slots::<4>();
        ApicMap::build(&slots, [(0, 9), (1, 3), (2, 7)]).expect("a reverse map");
        let read: Vec<u64> = slots
            .iter()
            .map(|slot| slot.load(Ordering::Relaxed))
            .collect();
        assert_eq!(read, [3 << 32 | 1, 7 << 32 | 2, 9 << 32, VACANT]);
    }

    #[test]
    fn a_map_with_no_reverse_is_refused() {
        let (four, one) = (slots::<4>(), slots::<1>());
        assert_eq!(
            ApicMap::build(&four, [(0, 5), (1, 5)]).map(|_| ()),
            Err(Unmappable::Duplicate)
        );
        assert_eq!(
            ApicMap::build(&four, [(0, NO_LAPIC)]).map(|_| ()),
            Err(Unmappable::OutOfRange)
        );
        assert_eq!(
            ApicMap::build(&one, [(0, 1), (1, 2)]).map(|_| ()),
            Err(Unmappable::OutOfRange)
        );
    }

    #[test]
    fn an_empty_map_finds_nothing() {
        let empty = slots::<0>();
        let map = ApicMap::build(&empty, []).expect("a reverse map");
        assert_eq!(map.cpu_of(0), None);
    }
}
