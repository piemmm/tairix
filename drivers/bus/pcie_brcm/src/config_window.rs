//! The root complex's windowed configuration access.
//!
//! The controller maps no flat configuration region. Its own header (bus 0,
//! devfn 0) is read in place at the register offset; a downstream function
//! is selected by writing its `(bus << 20) | (devfn << 12)` block address to
//! [`EXT_CFG_INDEX`] and reached through the 4 KiB [`EXT_CFG_DATA`] window.
//! Enumeration, BAR sizing and the capability walk stay `tairix_pci`'s: this
//! supplies only the access, to `tairix_pci::mechanism_supplied`. A target
//! the link cannot answer, or an access past the mapped window, reads as the
//! PCI "no device" sentinel.

use tairix_abi::RegisterWindow;
use tairix_pci::{ConfigAddress, ConfigSpace};

/// Controller offset of the external configuration index register
/// (`PCIE_EXT_CFG_INDEX`), which selects the function [`EXT_CFG_DATA`]
/// reaches.
pub const EXT_CFG_INDEX: usize = 0x9000;

/// Controller offset of the 4 KiB external configuration data window
/// (`PCIE_EXT_CFG_DATA`).
pub const EXT_CFG_DATA: usize = 0x8000;

/// Configuration access through the controller's register window, the
/// window the link was trained through.
pub struct BrcmConfigSpace {
    window: RegisterWindow,
    secondary_bus: u8,
}

impl BrcmConfigSpace {
    /// Access through `window`, forwarding downstream only to device 0 on
    /// `secondary_bus`, the bus the bring-up programmed into the root port.
    ///
    /// A window that does not reach [`EXT_CFG_INDEX`] leaves every
    /// downstream target "no device".
    #[must_use]
    pub const fn new(window: RegisterWindow, secondary_bus: u8) -> Self {
        Self {
            window,
            secondary_bus,
        }
    }

    /// The controller offset `addr` is reached at, after selecting a
    /// downstream function's block; `None` for a "no device" target.
    fn data_offset(&self, addr: ConfigAddress) -> Option<usize> {
        // The shared range gate, which also keeps the register inside the
        // 4 KiB data window.
        let block = addr.ecam_offset()?;
        let reg = (addr.register as usize) << 2;
        if addr.bus == 0 {
            if addr.device != 0 || addr.function != 0 {
                return None;
            }
            return Some(reg);
        }
        // The root port is a single-device link: an access to any other
        // downstream target goes unanswered and the root complex raises an
        // external abort, so it never reaches the controller.
        if addr.bus != self.secondary_bus || addr.device != 0 {
            return None;
        }
        let index = u32::try_from(block & !0xFFF).ok()?;
        self.window.write_u32(EXT_CFG_INDEX, index).ok()?;
        Some(EXT_CFG_DATA + reg)
    }
}

impl ConfigSpace for BrcmConfigSpace {
    fn read32(&self, addr: ConfigAddress) -> u32 {
        let Some(offset) = self.data_offset(addr) else {
            return 0xFFFF_FFFF;
        };
        self.window.read_u32(offset).unwrap_or(0xFFFF_FFFF)
    }

    fn write32(&self, addr: ConfigAddress, value: u32) {
        let Some(offset) = self.data_offset(addr) else {
            return;
        };
        let _ = self.window.write_u32(offset, value);
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ptr::NonNull;

    /// A [`RegisterWindow`] over a zeroed `bytes`-byte register block, and
    /// the `Vec` that owns it, which must outlive the window.
    fn ctrl_window(bytes: usize) -> (Vec<u32>, RegisterWindow) {
        let mut backing = vec![0u32; bytes / 4];
        let base = NonNull::new(backing.as_mut_ptr().cast::<u8>()).expect("non-null heap buffer");
        let len = backing.len() * 4;
        // SAFETY: `base` is 4-byte aligned (the `Vec<u32>` allocation
        // guarantee) and covers exactly `len` bytes; the backing `Vec`
        // is returned to the caller so it outlives the window, and no
        // other reference aliases it while the window is live.
        let window = unsafe { RegisterWindow::from_mapping(0xFD50_0000, base, len) };
        (backing, window)
    }

    #[test]
    fn root_bus_reads_header_directly() {
        let (mut backing, window) = ctrl_window(0x9400);
        backing[0] = 0x2711_14E4;
        let cs = BrcmConfigSpace::new(window, 1);
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 0,
                device: 0,
                function: 0,
                register: 0,
            }),
            0x2711_14E4,
        );
    }

    #[test]
    fn root_bus_other_functions_are_no_device() {
        let (_backing, window) = ctrl_window(0x9400);
        let cs = BrcmConfigSpace::new(window, 1);
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 0,
                device: 1,
                function: 0,
                register: 0,
            }),
            0xFFFF_FFFF,
        );
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 0,
                device: 0,
                function: 1,
                register: 0,
            }),
            0xFFFF_FFFF,
        );
    }

    #[test]
    fn downstream_access_writes_index_then_reads_data_window() {
        let (mut backing, window) = ctrl_window(0x9400);
        backing[EXT_CFG_DATA / 4] = 0x3483_1106;
        let cs = BrcmConfigSpace::new(window, 1);
        let function = ConfigAddress {
            bus: 1,
            device: 0,
            function: 0,
            register: 0,
        };
        assert_eq!(cs.read32(function), 0x3483_1106);
        // Bus 1, devfn 0, the register stripped.
        assert_eq!(backing[EXT_CFG_INDEX / 4], 0x10_0000);
    }

    #[test]
    fn downstream_register_offset_indexes_the_data_window() {
        let (mut backing, window) = ctrl_window(0x9400);
        // BAR0 of function 1 of the single downstream device.
        backing[(EXT_CFG_DATA + 0x10) / 4] = 0xDEAD_BEEF;
        let cs = BrcmConfigSpace::new(window, 1);
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 1,
                device: 0,
                function: 1,
                register: 4,
            }),
            0xDEAD_BEEF,
        );
        assert_eq!(backing[EXT_CFG_INDEX / 4], (1 << 20) | (1 << 12));
    }

    #[test]
    fn phantom_downstream_targets_are_no_device_without_an_index_write() {
        // An unanswered downstream access would abort the CPU, so a phantom
        // target must not even select a block.
        let (backing, window) = ctrl_window(0x9400);
        let cs = BrcmConfigSpace::new(window, 1);
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 1,
                device: 1,
                function: 0,
                register: 0,
            }),
            0xFFFF_FFFF,
        );
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 2,
                device: 0,
                function: 0,
                register: 0,
            }),
            0xFFFF_FFFF,
        );
        assert_eq!(backing[EXT_CFG_INDEX / 4], 0);
    }

    #[test]
    fn downstream_write_round_trips_through_the_window() {
        let (_backing, window) = ctrl_window(0x9400);
        let cs = BrcmConfigSpace::new(window, 1);
        let addr = ConfigAddress {
            bus: 1,
            device: 0,
            function: 0,
            register: 1,
        };
        cs.write32(addr, 0x0000_0146);
        assert_eq!(cs.read32(addr), 0x0000_0146);
    }

    #[test]
    fn out_of_range_address_is_no_device() {
        let (_backing, window) = ctrl_window(0x9400);
        let cs = BrcmConfigSpace::new(window, 1);
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 0,
                device: 99,
                function: 0,
                register: 0,
            }),
            0xFFFF_FFFF,
        );
    }

    #[test]
    fn access_beyond_window_reads_no_device_sentinel() {
        // A window short of the index register fails every downstream
        // access closed.
        let (_backing, window) = ctrl_window(0x100);
        let cs = BrcmConfigSpace::new(window, 1);
        assert_eq!(
            cs.read32(ConfigAddress {
                bus: 1,
                device: 0,
                function: 0,
                register: 0,
            }),
            0xFFFF_FFFF,
        );
    }
}
