//! PCI configuration-space addressing and descriptor types.
//!
//! The types live in their own module so the enumeration logic in
//! [`crate::enumerate`] and the mechanism-#1 PIO bridge in
//! [`crate::mech_one`] both depend on a single source of truth for
//! the on-wire layout. Nothing here is re-exported outside the
//! crate; the test module exercises every type directly.

/// A `(bus, device, function, register)` quadruple addressing one
/// 32-bit dword of PCI configuration space.
///
/// Encodes to the 32-bit value written to `0xCF8` per PCI Local Bus
/// 3.0 §3.2.2.3.2:
///
/// ```text
///  bit 31    : enable (1)
///  bits 30..24: reserved (0)
///  bits 23..16: bus
///  bits 15..11: device
///  bits 10..8 : function
///  bits 7..2  : register dword index
///  bits 1..0  : 0 (dword-aligned)
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConfigAddress {
    /// PCI bus number (0..=255).
    pub bus: u8,
    /// PCI device number on the bus (0..=31).
    pub device: u8,
    /// Function number within the device (0..=7).
    pub function: u8,
    /// Configuration-space register dword index: the byte offset divided by
    /// 4. The legacy 256-byte space is `0..=63`; a `PCIe` function's extended
    /// space runs on to `1023`, reachable only through a memory-mapped
    /// mechanism.
    pub register: u16,
}

/// Dword index of the first register past the legacy 256-byte configuration
/// space: where a `PCIe` function's extended capabilities begin.
pub const EXTENDED_REGISTER: u16 = 0x100 >> 2;

/// One past the last dword index of a `PCIe` function's 4 KiB configuration
/// space.
const REGISTER_LIMIT: u16 = 0x1000 >> 2;

/// Header dword indices every layout shares (PCI Local Bus 3.0 §6.1): the
/// command and status register, the header type's, and the first BAR.
pub(crate) const COMMAND_STATUS: u8 = 1;
pub(crate) const HEADER_TYPE: u8 = 3;
pub(crate) const FIRST_BAR: u8 = 4;
/// A bridge's bus numbers (PCI-to-PCI Bridge 1.2 §3.2.5.3).
pub(crate) const BUS_NUMBERS: u8 = 6;

/// Header layouts (PCI Local Bus 3.0 §6.1, PCI-to-PCI Bridge 1.2 §3.2).
pub(crate) const HEADER_DEVICE: u8 = 0;
pub(crate) const HEADER_BRIDGE: u8 = 1;
pub(crate) const HEADER_CARDBUS: u8 = 2;
/// The header type's multi-function bit.
pub(crate) const MULTIFUNCTION: u8 = 0x80;

/// The BAR slots each layout carries: a bridge's third would be its bus
/// numbers.
pub(crate) const DEVICE_BAR_SLOTS: u8 = 6;
pub(crate) const BRIDGE_BAR_SLOTS: u8 = 2;

impl ConfigAddress {
    /// Encoded `0xCF8` value, with the high enable bit set.
    ///
    /// Returns `None` if any field exceeds its hardware range; this
    /// is the single defensive gate for the entire driver.
    #[must_use]
    pub fn to_cf8(self) -> Option<u32> {
        if self.device > 31 || self.function > 7 || self.register >= EXTENDED_REGISTER {
            return None;
        }
        let bus = u32::from(self.bus);
        let dev = u32::from(self.device);
        let func = u32::from(self.function);
        // `register` carries a *dword* index, not a byte offset; the
        // `<< 2` produces the canonical byte-aligned form the
        // hardware expects.
        let reg = u32::from(self.register) << 2;
        Some(0x8000_0000 | (bus << 16) | (dev << 11) | (func << 8) | (reg & 0xFC))
    }

    /// Byte offset of this configuration dword within an **enhanced
    /// configuration access mechanism** (ECAM / `PCIe` MMCONFIG) region.
    ///
    /// ECAM maps configuration space flat into MMIO: the byte offset
    /// of a `(bus, device, function, register)` tuple within the region
    /// base is (PCI Express Base 3.0 §7.2.2, "Enhanced Configuration
    /// Access Mechanism"):
    ///
    /// ```text
    ///  bits 27..20: bus      (one 1 MiB block per bus)
    ///  bits 19..15: device   (one 32 KiB block per device)
    ///  bits 14..12: function (one  4 KiB block per function)
    ///  bits 11..0 : register byte offset within the function
    /// ```
    ///
    /// Returns `None` if any field exceeds its hardware range — the
    /// same defensive gate [`to_cf8`](Self::to_cf8) applies to the
    /// mechanism-#1 path, so a malformed address is treated as
    /// "no device" by the caller rather than reaching the window.
    ///
    /// `register` carries a *dword* index into the function's whole 4 KiB
    /// configuration space, extended registers included, so the resulting
    /// offset stays within the function's ECAM block.
    #[must_use]
    pub const fn ecam_offset(self) -> Option<usize> {
        if self.device > 31 || self.function > 7 || self.register >= REGISTER_LIMIT {
            return None;
        }
        let bus = self.bus as usize;
        let dev = self.device as usize;
        let func = self.function as usize;
        // `register` is a dword index; `<< 2` makes it a byte offset.
        let reg = (self.register as usize) << 2;
        Some((bus << 20) | (dev << 15) | (func << 12) | reg)
    }

    /// Pack into the [`tairix_abi::driver::bus::BusDevice::address`]
    /// slot the driver hands back to the host, or [`None`] for a device or
    /// function past PCI's limits.
    #[must_use]
    pub const fn pack_bdf(self) -> Option<u64> {
        tairix_abi::driver::pci::function_address(self.bus, self.device, self.function)
    }
}

/// Read or write 32-bit PCI configuration dwords.
///
/// Implementations:
///
/// * [`crate::mech_one::PortIoConfigSpace`] — real hardware PIO,
///   behind the [`tairix_abi::PortIo`] seam so the unit tests can
///   drive it without touching the actual `in`/`out` instructions
///   (the x86_64 backend lives in the architecture port).
/// * [`crate::mech_ecam::EcamConfigSpace`] — memory-mapped `PCIe`
///   enhanced configuration access over a kernel-mapped
///   [`tairix_abi::RegisterWindow`], the path of every host bridge with a
///   flat configuration region.
/// * [`crate::mech_brcm::BrcmConfigSpace`] — the BCM2711 root complex's
///   index/data window.
/// * `tests::MockConfigSpace` — table-driven fixture for the
///   in-crate enumeration tests.
pub trait ConfigSpace {
    /// Read a 32-bit configuration dword.
    fn read32(&self, addr: ConfigAddress) -> u32;
    /// Write a 32-bit configuration dword.
    fn write32(&self, addr: ConfigAddress, value: u32);
}

/// Kind of a Base Address Register slot.
///
/// Matches PCI Local Bus 3.0 §6.2.5.1 BAR layout bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BarKind {
    /// 32-bit memory-mapped region (BAR bits 2..=1 == 0b00).
    Memory32,
    /// 64-bit memory-mapped region (BAR bits 2..=1 == 0b10); paired
    /// with the next-higher BAR slot carrying the upper 32 bits.
    Memory64,
    /// I/O-port region (BAR bit 0 == 1).
    Io,
}

/// One enumerated BAR slot.
///
/// The `size` field is computed by the standard write-FFFFFFFF /
/// read-back probe sequence and is the *power-of-two* span the
/// driver host must reserve when routing the BAR mapping request
/// through the kernel memory capability.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BarDescriptor {
    /// BAR index within the function's configuration space (0..=5).
    pub index: u8,
    /// BAR layout — memory-mapped (32 / 64) or I/O.
    pub kind: BarKind,
    /// Base address read from the BAR; for [`BarKind::Memory64`]
    /// this already includes the upper-32-bit slot.
    pub base: u64,
    /// Size in bytes (power of two), or `0` if the BAR is unused.
    pub size: u64,
    /// Prefetchable hint from BAR bit 3 (memory BARs only).
    pub prefetchable: bool,
}

/// One enumerated PCI capability-list entry.
///
/// MSI and MSI-X are recognised explicitly because the bus driver
/// must surface their addressing for the virtio-blk / virtio-net
/// drivers in Stage 4.D. Other capability IDs are surfaced
/// opaquely so the upper-layer driver host can audit them without
/// re-walking config space.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Capability {
    /// MSI capability (`cap_id = 0x05`).
    Msi {
        /// Byte offset of the capability header in configuration space.
        offset: u8,
        /// Number of distinct message vectors the device can request
        /// (decoded from the Message Control register's MMC field).
        message_count: u8,
        /// `true` if the capability advertises 64-bit addressing.
        addressing_64bit: bool,
        /// `true` if the capability carries per-vector mask and pending
        /// registers. A masked MSI vector never emits its message write, so
        /// the routing path must explicitly clear the mask register.
        per_vector_masking: bool,
    },
    /// MSI-X capability (`cap_id = 0x11`).
    MsiX {
        /// Byte offset of the capability header in configuration space.
        offset: u8,
        /// Number of entries in the MSI-X table (decoded from
        /// `table_size = (msg_ctrl & 0x7FF) + 1`).
        table_size: u16,
        /// BAR index containing the MSI-X table.
        table_bar: u8,
        /// Offset of the table within `table_bar`.
        table_offset: u32,
        /// BAR index containing the Pending Bit Array.
        pba_bar: u8,
        /// Offset of the PBA within `pba_bar`.
        pba_offset: u32,
    },
    /// A virtio-1.x vendor-specific capability (`cap_id = 0x09`) other
    /// than the notification structure.
    ///
    /// virtio reuses the generic PCI vendor-specific capability to
    /// publish the byte location of each device-configuration
    /// structure (common / ISR / device / PCI-window) as a
    /// `(bar, bar_offset, length)` triple (virtio 1.x §4.1.4). The
    /// notification structure carries an extra field and is surfaced
    /// as [`Capability::VirtioNotify`] instead.
    Virtio {
        /// Byte offset of the capability header in configuration space.
        offset: u8,
        /// Structure kind (`cfg_type`); one of the
        /// [`VIRTIO_PCI_CFG_*`](tairix_abi::driver::virtio_pci) discriminants,
        /// or a future value surfaced verbatim.
        cfg_type: u8,
        /// Index of the BAR holding the structure.
        bar: u8,
        /// Offset of the structure within `bar`.
        bar_offset: u32,
        /// Length of the structure in bytes.
        length: u32,
    },
    /// The virtio-1.x notification capability
    /// (`cap_id = 0x09`, `cfg_type = `[`VIRTIO_PCI_CFG_NOTIFY`]).
    ///
    /// Identical to [`Capability::Virtio`] but additionally carries
    /// `notify_off_multiplier`, the scale applied to a queue's
    /// `queue_notify_off` to derive its notification address
    /// (virtio 1.x §4.1.4.4).
    ///
    /// [`VIRTIO_PCI_CFG_NOTIFY`]: tairix_abi::driver::virtio_pci::VIRTIO_PCI_CFG_NOTIFY
    VirtioNotify {
        /// Byte offset of the capability header in configuration space.
        offset: u8,
        /// Index of the BAR holding the notification structure.
        bar: u8,
        /// Offset of the structure within `bar`.
        bar_offset: u32,
        /// Length of the structure in bytes.
        length: u32,
        /// Multiplier applied to `queue_notify_off` (virtio 1.x §4.1.4.4).
        notify_off_multiplier: u32,
    },
    /// Any other capability ID. Surfaced opaquely; the host audits.
    Other {
        /// Byte offset of the capability header in configuration space.
        offset: u8,
        /// Raw `cap_id` byte.
        id: u8,
    },
}

/// PCI capability ID for a vendor-specific capability (PCI Local Bus
/// 3.0 §H); virtio 1.x reuses it for its configuration structures.
pub const CAP_ID_VENDOR: u8 = 0x09;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf8_encodes_canonical_example() {
        // Bus 0, device 0x1F, function 0 (LPC bridge on q35), register 0.
        let addr = ConfigAddress {
            bus: 0,
            device: 0x1F,
            function: 0,
            register: 0,
        };
        // 0x8000_0000 | (0 << 16) | (0x1F << 11) | (0 << 8) | 0 == 0x8000_F800
        assert_eq!(addr.to_cf8(), Some(0x8000_F800));
    }

    #[test]
    fn cf8_rejects_out_of_range() {
        assert_eq!(
            ConfigAddress {
                bus: 0,
                device: 32,
                function: 0,
                register: 0
            }
            .to_cf8(),
            None,
        );
        assert_eq!(
            ConfigAddress {
                bus: 0,
                device: 0,
                function: 8,
                register: 0
            }
            .to_cf8(),
            None,
        );
        assert_eq!(
            ConfigAddress {
                bus: 0,
                device: 0,
                function: 0,
                register: 64
            }
            .to_cf8(),
            None,
        );
    }

    #[test]
    fn ecam_offset_encodes_block_layout() {
        // Bus 1, device 0, function 0, register 0 — the VL805 xHCI on
        // the BCM2711 root complex sits one 1 MiB bus block in.
        assert_eq!(
            ConfigAddress {
                bus: 1,
                device: 0,
                function: 0,
                register: 0,
            }
            .ecam_offset(),
            Some(0x10_0000),
        );
        // Bus 0, device 0x1F, function 3, register 13 (cap pointer
        // dword at byte 0x34): (0x1F << 15) | (3 << 12) | (13 << 2).
        assert_eq!(
            ConfigAddress {
                bus: 0,
                device: 0x1F,
                function: 3,
                register: 13,
            }
            .ecam_offset(),
            Some((0x1F << 15) | (3 << 12) | (13 << 2)),
        );
    }

    #[test]
    fn ecam_offset_rejects_out_of_range() {
        assert_eq!(
            ConfigAddress {
                bus: 0,
                device: 32,
                function: 0,
                register: 0
            }
            .ecam_offset(),
            None,
        );
        assert_eq!(
            ConfigAddress {
                bus: 0,
                device: 0,
                function: 8,
                register: 0
            }
            .ecam_offset(),
            None,
        );
        let extended = |register| {
            ConfigAddress {
                bus: 0,
                device: 0,
                function: 0,
                register,
            }
            .ecam_offset()
        };
        assert_eq!(extended(EXTENDED_REGISTER), Some(0x100), "extended space");
        assert_eq!(extended(REGISTER_LIMIT - 1), Some(0xFFC), "its last dword");
        assert_eq!(extended(REGISTER_LIMIT), None, "past the function's 4 KiB");
    }

    #[test]
    fn mechanism_one_reaches_only_the_legacy_space() {
        let at = |register| {
            ConfigAddress {
                bus: 0,
                device: 0,
                function: 0,
                register,
            }
            .to_cf8()
        };
        assert_eq!(at(EXTENDED_REGISTER - 1), Some(0x8000_00FC));
        assert_eq!(at(EXTENDED_REGISTER), None);
    }

    #[test]
    fn pack_bdf_matches_bit_layout() {
        let addr = ConfigAddress {
            bus: 0x12,
            device: 0x0A,
            function: 0x3,
            register: 0,
        };
        assert_eq!(
            addr.pack_bdf(),
            Some((0x12 << 16) | (0x0A << 11) | (0x3 << 8))
        );
    }
}
