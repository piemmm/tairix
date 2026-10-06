//! `PCIe` enhanced configuration access mechanism (ECAM / MMCONFIG).
//!
//! Where mechanism #1 ([`crate::mech_one`]) reaches configuration
//! space through the x86 legacy I/O ports, ECAM maps it flat into
//! MMIO: a contiguous physical region in which each
//! `(bus, device, function)` owns a 4 KiB block (PCI Express Base 3.0
//! §7.2.2). Reading or writing a configuration dword is then a single
//! naturally-aligned access at the computed offset
//! ([`ConfigAddress::ecam_offset`]).
//!
//! A segment's ECAM region covers the buses firmware assigned it, from its
//! first; the region is reached through a kernel-mapped [`RegisterWindow`]
//! the caller passes to [`crate::mechanism_ecam`] with that bus range. A bus
//! outside the range, or an offset past the window, reads as the PCI "no
//! device" sentinel and writes nothing, so a walk fails closed rather than
//! reaching past the mapping.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use tairix_abi::RegisterWindow;

use crate::config::{ConfigAddress, ConfigSpace};

/// One configuration region of a segment: the buses it covers, mapped from
/// the first one's block.
pub struct EcamRegion {
    window: RegisterWindow,
    buses: RangeInclusive<u8>,
}

impl EcamRegion {
    /// The region of `buses`, whose first bus's block starts `window`.
    #[must_use]
    pub const fn new(window: RegisterWindow, buses: RangeInclusive<u8>) -> Self {
        Self { window, buses }
    }
}

/// A [`ConfigSpace`] backed by a segment's memory-mapped ECAM regions.
pub struct EcamConfigSpace {
    regions: Vec<EcamRegion>,
}

impl EcamConfigSpace {
    /// The segment `regions` cover; a bus no region covers is no bus of the
    /// segment's.
    #[must_use]
    pub const fn new(regions: Vec<EcamRegion>) -> Self {
        Self { regions }
    }

    /// The region `addr` lies in, and where in its window.
    fn locate(&self, addr: ConfigAddress) -> Option<(&RegisterWindow, usize)> {
        let region = self
            .regions
            .iter()
            .find(|region| region.buses.contains(&addr.bus))?;
        let offset = ConfigAddress {
            bus: addr.bus - region.buses.start(),
            ..addr
        }
        .ecam_offset()?;
        Some((&region.window, offset))
    }
}

impl ConfigSpace for EcamConfigSpace {
    fn read32(&self, addr: ConfigAddress) -> u32 {
        // An address outside every region reads as the PCI Local Bus 3.0 §6.1
        // "no function present" sentinel.
        self.locate(addr)
            .and_then(|(window, offset)| window.read_u32(offset).ok())
            .unwrap_or(0xFFFF_FFFF)
    }

    fn write32(&self, addr: ConfigAddress, value: u32) {
        if let Some((window, offset)) = self.locate(addr) {
            let _ = window.write_u32(offset, value);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ptr::NonNull;

    /// Build a [`RegisterWindow`] over a freshly-allocated, zeroed
    /// ECAM region of `words` dwords. The returned `Vec` owns the
    /// backing and must outlive the window.
    fn ecam_region(words: usize) -> (Vec<u32>, RegisterWindow) {
        let mut backing = vec![0u32; words];
        let base = NonNull::new(backing.as_mut_ptr().cast::<u8>()).expect("non-null heap buffer");
        let len = backing.len() * 4;
        // SAFETY: `base` is 4-byte aligned (the `Vec<u32>` allocation
        // guarantee) and covers exactly `len` bytes; the backing `Vec`
        // is returned to the caller so it outlives the window, and no
        // other reference aliases it while the window is live.
        let window = unsafe { RegisterWindow::from_mapping(0xF800_0000, base, len) };
        (backing, window)
    }

    #[test]
    fn read_resolves_ecam_offset() {
        // One full bus block (1 MiB) is plenty for bus 0.
        let (mut backing, window) = ecam_region(0x10_0000 / 4);
        // Plant a vendor/device dword at 00:1f.3 register 0.
        let addr = ConfigAddress {
            bus: 0,
            device: 0x1F,
            function: 3,
            register: 0,
        };
        let off = addr.ecam_offset().expect("in range");
        backing[off / 4] = 0x2930_8086;
        let cs = EcamConfigSpace::new(vec![EcamRegion::new(window, 0..=0)]);
        assert_eq!(cs.read32(addr), 0x2930_8086);
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_backing, window) = ecam_region(0x10_0000 / 4);
        let cs = EcamConfigSpace::new(vec![EcamRegion::new(window, 0..=0)]);
        let addr = ConfigAddress {
            bus: 0,
            device: 5,
            function: 0,
            register: 4,
        };
        cs.write32(addr, 0xCAFE_F00D);
        assert_eq!(cs.read32(addr), 0xCAFE_F00D);
    }

    #[test]
    fn access_beyond_window_reads_no_device_sentinel() {
        // A region covering only bus 0; bus 1 lies past its end.
        let (_backing, window) = ecam_region(0x10_0000 / 4);
        let cs = EcamConfigSpace::new(vec![EcamRegion::new(window, 0..=0)]);
        let bus1 = ConfigAddress {
            bus: 1,
            device: 0,
            function: 0,
            register: 0,
        };
        assert_eq!(cs.read32(bus1), 0xFFFF_FFFF);
        // The out-of-bounds write is dropped (no panic, no growth).
        cs.write32(bus1, 0x1234_5678);
        assert_eq!(cs.read32(bus1), 0xFFFF_FFFF);
    }

    /// A segment whose buses start above 0 is mapped from its first bus, so
    /// that bus's block opens the window and a bus below it is not one of
    /// the region's.
    #[test]
    fn a_region_starting_above_bus_zero_is_reached_from_its_first_bus() {
        let (mut backing, window) = ecam_region(2 * 0x10_0000 / 4);
        backing[0] = 0x1111_8086;
        backing[0x10_0000 / 4] = 0x2222_8086;
        let cs = EcamConfigSpace::new(vec![EcamRegion::new(window, 0x80..=0x81)]);
        let at = |bus| ConfigAddress {
            bus,
            device: 0,
            function: 0,
            register: 0,
        };
        assert_eq!(cs.read32(at(0x80)), 0x1111_8086);
        assert_eq!(cs.read32(at(0x81)), 0x2222_8086);
        assert_eq!(
            cs.read32(at(0x00)),
            0xFFFF_FFFF,
            "bus 0 is not this region's"
        );
        assert_eq!(cs.read32(at(0x7F)), 0xFFFF_FFFF);
        assert_eq!(cs.read32(at(0x82)), 0xFFFF_FFFF);
        cs.write32(at(0x00), 0);
        assert_eq!(
            backing[0], 0x1111_8086,
            "a write to another bus lands nowhere"
        );
    }

    /// A segment split across two regions reaches each bus through its own.
    #[test]
    fn a_segment_split_across_regions_reaches_each_bus_through_its_own() {
        let (mut low, low_window) = ecam_region(0x10_0000 / 4);
        let (mut high, high_window) = ecam_region(0x10_0000 / 4);
        low[0] = 0x1111_8086;
        high[0] = 0x2222_8086;
        let cs = EcamConfigSpace::new(vec![
            EcamRegion::new(low_window, 0x00..=0x00),
            EcamRegion::new(high_window, 0x40..=0x40),
        ]);
        let at = |bus| ConfigAddress {
            bus,
            device: 0,
            function: 0,
            register: 0,
        };
        assert_eq!(cs.read32(at(0x00)), 0x1111_8086);
        assert_eq!(cs.read32(at(0x40)), 0x2222_8086);
        assert_eq!(cs.read32(at(0x20)), 0xFFFF_FFFF, "between the regions");
    }

    #[test]
    fn out_of_range_address_reads_no_device_sentinel() {
        let (_backing, window) = ecam_region(0x10_0000 / 4);
        let cs = EcamConfigSpace::new(vec![EcamRegion::new(window, 0..=0)]);
        let bad = ConfigAddress {
            bus: 0,
            device: 99,
            function: 0,
            register: 0,
        };
        assert_eq!(cs.read32(bad), 0xFFFF_FFFF);
    }
}
