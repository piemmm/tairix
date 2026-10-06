//! Where a host bridge's memory BARs may decode, as the CPU reaches them.
//!
//! A function chooses what its BAR reads back, so every resolution of one is
//! checked: a BAR outside every window the host forwards, or over memory or a
//! platform device's registers, resolves to nothing, and the kernel never
//! maps it or writes through it.

use alloc::vec::Vec;
use core::ops::Range;

/// One window a host bridge forwards: PCI memory addresses `pci`, which the
/// CPU reaches from `cpu` up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Aperture {
    /// The PCI memory addresses it forwards.
    pub pci: Range<u64>,
    /// The CPU physical address `pci.start` is reached at.
    pub cpu: u64,
}

impl Aperture {
    /// A window the CPU reaches at the PCI addresses themselves.
    #[must_use]
    pub const fn identity(pci: Range<u64>) -> Self {
        let cpu = pci.start;
        Self { pci, cpu }
    }
}

/// A host bridge's memory windows, and what no BAR may decode over: memory,
/// and the registers of the devices the platform itself is made of.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Apertures {
    windows: Vec<Aperture>,
    forbidden: Vec<Range<u64>>,
}

impl Apertures {
    /// BARs decoding inside `windows`, over none of `forbidden`.
    #[must_use]
    pub const fn new(windows: Vec<Aperture>, forbidden: Vec<Range<u64>>) -> Self {
        Self { windows, forbidden }
    }

    /// The windows.
    #[must_use]
    pub fn windows(&self) -> &[Aperture] {
        &self.windows
    }

    /// The CPU physical range PCI memory `[base, base + len)` decodes to, or
    /// [`None`] for an empty range, one no single window holds, or one over
    /// anything forbidden.
    #[must_use]
    pub fn cpu_range(&self, base: u64, len: u64) -> Option<Range<u64>> {
        let end = base.checked_add(len)?;
        if len == 0 {
            return None;
        }
        let window = self
            .windows
            .iter()
            .find(|window| window.pci.start <= base && end <= window.pci.end)?;
        let start = window.cpu.checked_add(base - window.pci.start)?;
        let cpu = start..start.checked_add(len)?;
        let forbidden = self
            .forbidden
            .iter()
            .any(|range| range.start < cpu.end && cpu.start < range.end);
        (!forbidden).then_some(cpu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_bar_decodes_only_inside_a_window_and_over_no_memory() {
        let apertures = Apertures::new(
            vec![
                Aperture::identity(0x1000_0000..0x3F00_0000),
                Aperture {
                    pci: 0x0..0x1_0000,
                    cpu: 0x3EFF_0000,
                },
            ],
            vec![0x4000_0000..0x8000_0000, 0x2000_0000..0x2000_1000],
        );
        assert_eq!(
            apertures.cpu_range(0x1000_0000, 0x1000),
            Some(0x1000_0000..0x1000_1000)
        );
        assert_eq!(
            apertures.cpu_range(0x100, 0x100),
            Some(0x3EFF_0100..0x3EFF_0200),
            "translated through its window"
        );
        assert_eq!(
            apertures.cpu_range(0x3EFF_F000, 0x2000),
            None,
            "straddles a window's end"
        );
        assert_eq!(
            apertures.cpu_range(0x4000_0000, 0x1000),
            None,
            "outside every window"
        );
        assert_eq!(
            apertures.cpu_range(0x2000_0000, 0x1000),
            None,
            "over memory"
        );
        assert_eq!(apertures.cpu_range(0x1000_0000, 0), None, "empty");
        assert_eq!(apertures.cpu_range(u64::MAX, 2), None, "wraps");
    }
}
