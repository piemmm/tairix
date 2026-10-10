//! Generic PCI/PCIe transport seam (`abi-v1`).
//!
//! [`VirtioPciBus`](super::virtio_pci::VirtioPciBus) provisions the
//! *virtio*-specific register windows a virtio transport needs. A
//! non-virtio PCI device — an xHCI USB host controller, say — needs a
//! different, smaller surface: the physical window of one of its base
//! address registers (BARs), memory decoding on, and bus mastering turned
//! on by whoever owns the function's configuration space when it hands the
//! function over (`plans/IOMMU.md` IOM7).
//!
//! [`PciBus`] is that surface. The PCI configuration-access library
//! (`lib/pci`) implements it; a device-class driver (`drivers/bus/usb`,
//! …) or a composing host reaches the bus through a `&dyn PciBus` rather
//! than naming the concrete bus type (PCI config
//! access is shared `lib/*` logic, and one driver never names another).
//! [`Bus`] is a supertrait so a single trait object can both enumerate
//! the bus (to pick the function) and provision it.
//!
//! Like every other `lib/abi` item the trait is held to the ABI
//! discipline; while `abi-v1` is unfrozen it may still evolve in place, every caller updated in the same change.

use super::bus::Bus;
use super::msix::MsiMessage;
use super::{DriverError, MmioMapper, RegisterWindow};
use crate::HwNode;

/// Byte offset of a function's command/status dword in its configuration
/// header (PCI Local Bus 3.0 §6.2.2).
pub const COMMAND_OFFSET: u16 = 0x04;
/// The command register's I/O Space Enable bit: the function decodes its I/O
/// BARs.
pub const IO_SPACE_ENABLE: u32 = 1 << 0;
/// The command register's Memory Space Enable bit: the function decodes its
/// memory BARs.
pub const MEMORY_SPACE_ENABLE: u32 = 1 << 1;
/// The command register's Bus Master Enable bit: the function may issue
/// upstream memory requests, its DMA and the writes that deliver its MSIs.
pub const BUS_MASTER_ENABLE: u32 = 1 << 2;
/// The command register's Interrupt Disable bit: the function may not assert
/// its INTx pin.
pub const INTERRUPT_DISABLE: u32 = 1 << 10;

/// Byte offset of the dword holding a function's revision id and its 24-bit
/// class code, the class in the upper three bytes (PCI Local Bus 3.0 §6.2.1).
pub const CLASS_OFFSET: u16 = 0x08;

/// The 24-bit class code of an xHCI USB host controller: serial bus `0x0C`,
/// USB `0x03`, programming interface `0x30` (PCI Code and ID Assignment
/// §1.13). The programming interface is what tells it from the OHCI, UHCI and
/// EHCI hosts sharing the sub-class.
pub const CLASS_USB_XHCI: u32 = 0x0C_03_30;

/// The class code of an Intel High Definition Audio controller: multimedia,
/// audio device, programming interface zero.
pub const CLASS_HD_AUDIO: u32 = 0x04_03_00;

/// Devices one PCI bus holds.
pub const PCI_DEVICES: u8 = 32;
/// Functions one PCI device holds.
pub const PCI_FUNCTIONS: u8 = 8;

/// The configuration address of `function` of `device` on `bus`, as a PCI
/// [`BusDevice`](super::bus::BusDevice) address carries it
/// (`bus << 16 | device << 11 | function << 8`), or [`None`] for a device or
/// function past PCI's limits.
#[must_use]
pub const fn function_address(bus: u8, device: u8, function: u8) -> Option<u64> {
    if device >= PCI_DEVICES || function >= PCI_FUNCTIONS {
        return None;
    }
    Some(((bus as u64) << 16) | ((device as u64) << 11) | ((function as u64) << 8))
}

/// The bus, device and function configuration `address` names.
#[must_use]
pub const fn function_of(address: u64) -> (u8, u8, u8) {
    let [_, function_byte, bus, _, _, _, _, _] = address.to_le_bytes();
    (bus, function_byte >> 3, function_byte & (PCI_FUNCTIONS - 1))
}

/// The requester id — bus, device and function packed as the function's
/// transactions carry them — of the function at configuration `address`.
#[must_use]
pub fn requester_id(address: u64) -> u16 {
    u16::try_from((address >> 8) & 0xFFFF).unwrap_or(u16::MAX)
}

/// The configuration address of the function whose requester id is `id`:
/// the inverse of [`requester_id`].
#[must_use]
pub fn config_address(id: u16) -> u64 {
    u64::from(id) << 8
}

/// A PCI function as the machine names it: its segment and its requester id.
/// A requester id alone is unique only within one segment.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PciAddress {
    segment: u16,
    requester: u16,
}

impl PciAddress {
    /// The function with requester id `requester` on segment `segment`.
    #[must_use]
    pub const fn new(segment: u16, requester: u16) -> Self {
        Self { segment, requester }
    }

    /// The function at configuration `address` on segment `segment`.
    #[must_use]
    pub fn at(segment: u16, address: u64) -> Self {
        Self::new(segment, requester_id(address))
    }

    /// The function a hardware-tree node's address names: the segment in the
    /// high half, the requester id in the low.
    #[must_use]
    pub const fn from_node_address(address: u32) -> Self {
        let [requester_low, requester_high, segment_low, segment_high] = address.to_le_bytes();
        Self::new(
            u16::from_le_bytes([segment_low, segment_high]),
            u16::from_le_bytes([requester_low, requester_high]),
        )
    }

    /// The segment.
    #[must_use]
    pub const fn segment(self) -> u16 {
        self.segment
    }

    /// The requester id its own transactions carry.
    #[must_use]
    pub const fn requester_id(self) -> u16 {
        self.requester
    }

    /// Its configuration address within its segment.
    #[must_use]
    pub fn config_address(self) -> u64 {
        config_address(self.requester)
    }

    /// As a hardware-tree node's address carries it
    /// ([`HwNode::set_address`](crate::HwNode::set_address)).
    #[must_use]
    pub const fn node_address(self) -> u32 {
        ((self.segment as u32) << 16) | self.requester as u32
    }
}

/// What telling functions found mastering DMA to stop came to.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Quiesced {
    /// Stopped.
    pub stopped: usize,
    /// Still mastering after the write.
    pub refused: usize,
}

/// A PCI bus that can provision a non-virtio function's resources.
///
/// # Capabilities
///
/// [`map_bar_window`](Self::map_bar_window) routes through the supplied
/// [`MmioMapper`], which enforces
/// [`CapabilityId::MMIO_MAP`](crate::CapabilityId::MMIO_MAP); the
/// implementation synthesises no pointer itself (no
/// ambient authority). [`enable_memory_space`](Self::enable_memory_space)
/// and [`set_bus_master`](Self::set_bus_master) touch only the function's
/// own configuration space, which the bus driver already reaches by holding
/// its [`DriverHandle`](crate::driver::DriverHandle).
pub trait PciBus: Bus {
    /// Resolve the memory BAR at `bar_index` on function `bdf` and ask
    /// `mapper` to map it, returning the resulting [`RegisterWindow`].
    ///
    /// This is the hand-off a memory-mapped device driver consumes:
    /// the bus driver reads the BAR's physical base and probed size
    /// from configuration space and asks the kernel's MMIO-map facility
    /// for a window over exactly that region. The driver never
    /// synthesises a pointer — the kernel allocates and validates the
    /// mapping.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — no BAR with `bar_index` exists, or
    ///   the BAR is unused (probed size zero).
    /// * [`DriverError::Unsupported`] — the BAR is an I/O-port BAR
    ///   (reached through port I/O, not a mapped window), or the
    ///   function is not a type-0 header.
    /// * [`DriverError::LengthOutOfRange`] — the BAR size does not fit
    ///   in `usize` on this target.
    /// * [`DriverError::PermissionDenied`] — the caller does not hold
    ///   [`CapabilityId::MMIO_MAP`](crate::CapabilityId::MMIO_MAP)
    ///   (propagated from the mapper).
    fn map_bar_window(
        &self,
        bdf: u64,
        bar_index: u8,
        mapper: &dyn MmioMapper,
    ) -> Result<RegisterWindow, DriverError>;

    /// The span of function `bdf`'s memory BAR `bar_index` a driver may be
    /// granted, as its physical base and length: the BAR up to the first page
    /// holding the function's MSI-X table or pending-bit array. Only the owner
    /// of the function's configuration space programs those, because a driver
    /// that could write its own table could aim the function's messages at
    /// any address.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — no memory BAR at `bar_index`, or one
    ///   whose first page already holds MSI-X state, leaving a driver
    ///   nothing.
    /// * [`DriverError::Unsupported`] — an I/O-port BAR, a header other than
    ///   type 0, or a bus that resolves no BARs.
    /// * [`DriverError::DeviceFault`] — a capability list that never ends, so
    ///   where the MSI-X state lies cannot be known.
    fn driver_window(&self, _bdf: u64, _bar_index: u8) -> Result<(u64, u64), DriverError> {
        Err(DriverError::Unsupported)
    }

    /// Turn on decoding of function `bdf`'s memory BARs (Memory Space
    /// Enable, PCI Local Bus 3.0 §6.2.2), leaving every other command bit
    /// as it was. A BAR, and an MSI-X table inside one, answers only once
    /// this is on.
    ///
    /// The status half of the command/status register is RW1C, so the
    /// implementation writes it as zero.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the configuration write cannot
    ///   be completed by the bus transport.
    fn enable_memory_space(&self, bdf: u64) -> Result<(), DriverError>;

    /// Let function `bdf` issue upstream memory requests — DMA, and the
    /// writes that deliver its MSIs — or stop it (Bus Master Enable, PCI
    /// Local Bus 3.0 §6.2.2), leaving every other command bit as it was.
    ///
    /// Only the owner of the function's configuration space calls this,
    /// and only when it hands the function over or takes it back: behind a
    /// DMA translation unit a function masters only once its owner's
    /// domain is attached (`plans/IOMMU.md` IOM7). Nothing else turns it on.
    ///
    /// A PCI Express function is let master only once its Enable No Snoop is
    /// clear: a No Snoop request reaches memory past the caches a DMA buffer
    /// is scrubbed and kept coherent through. One whose bit will not clear is
    /// left stopped.
    ///
    /// The status half of the command/status register is RW1C, so the
    /// implementation writes it as zero.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the configuration write cannot
    ///   be completed by the bus transport.
    fn set_bus_master(&self, bdf: u64, master: bool) -> Result<(), DriverError>;

    /// Let function `bdf` assert its INTx pin, or stop it (Interrupt
    /// Disable, PCI Local Bus 3.0 §6.2.2), every other command bit as it
    /// was. Only the owner of its configuration space calls this: a pin is
    /// raised only while an owner is bound to its line.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the configuration write cannot
    ///   be completed by the bus transport.
    fn set_intx(&self, bdf: u64, raise: bool) -> Result<(), DriverError>;

    /// Assign a memory base to the BAR at `bar_index` on function
    /// `bdf` if it is currently **unassigned**, placing it inside the
    /// PCIe-bus window `[window_base, window_base + window_size)` and
    /// returning the resolved PCIe-bus base.
    ///
    /// Firmware normally programs a function's BARs, but when the OS
    /// resets and re-enumerates the host bridge (the BCM2711 PCIe
    /// bring-up) a downstream function's BAR address bits read zero: the
    /// BAR is sized and typed but carries no base, so mapping it would
    /// target physical address 0 and be refused. Assigning resources
    /// from the bridge's outbound window is the PCI core's job. A BAR
    /// that already carries a non-zero base is left untouched and its
    /// base returned (firmware's assignment is respected); the call is
    /// then a no-op that leaves configuration space unchanged. A
    /// DMA-driving driver calls this once before
    /// [`map_bar_window`](Self::map_bar_window).
    ///
    /// The returned base is a **PCIe-bus** address; the host bridge's
    /// [`MmioMapper`] translates it to CPU-physical at map time.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — `bar_index` is out of range, or no
    ///   memory BAR is implemented at that slot.
    /// * [`DriverError::Unsupported`] — the BAR is an I/O-port BAR, or
    ///   the function is not a type-0 header.
    /// * [`DriverError::OutOfRange`] — the BAR's size-aligned placement
    ///   does not fit inside the window, or a 32-bit BAR would land
    ///   above the 4 GiB line (fail closed).
    fn assign_bar(
        &self,
        bdf: u64,
        bar_index: u8,
        window_base: u64,
        window_size: u64,
    ) -> Result<u64, DriverError>;

    /// Read the configuration-space dword at byte `offset` of function
    /// `bdf`.
    ///
    /// `offset` is a **byte** offset into the function's 256-byte
    /// configuration header and is taken modulo-4 (the dword the byte
    /// falls in); the returned value is the little-endian dword exactly
    /// as configuration space holds it. This is a read-only window onto
    /// a function's own configuration the bus driver already reaches by
    /// holding its [`DriverHandle`](crate::driver::DriverHandle), used to
    /// confirm a write took effect (a just-assigned BAR, an enabled
    /// command register, a programmed bridge window) — a diagnostic
    /// read, not a side-effecting one.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the configuration read cannot
    ///   be completed by the bus transport.
    fn read_config(&self, bdf: u64, offset: u16) -> Result<u32, DriverError>;

    /// The first dword of function `bdf`'s capability `id`: its id, next
    /// pointer and the capability's own leading bits, as its structure
    /// defines them.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if the function lists no such capability
    ///   (or no capability list at all).
    /// * [`DriverError::DeviceFault`] if the list never ends.
    fn capability_header(&self, bdf: u64, id: u8) -> Result<u32, DriverError>;

    /// Describe the function at `bdf` as a discovered child
    /// [`HwNode`] to attach beneath the bus's own
    /// hardware-tree node.
    ///
    /// A bus that enumerates downstream devices is responsible for
    /// growing the hardware tree at runtime: each device it finds
    /// becomes a child node carrying the match keys a driver's signed
    /// bind table is resolved against, so a device
    /// behind the bus autoloads its driver as match **data** rather than
    /// by hand-wired composition. For a PCI
    /// function the emitted node carries a single
    /// [`HwMatchKey::pci`](crate::HwMatchKey::pci) of the function's
    /// `vendor:device` and its **full 24-bit class code**
    /// `(base_class << 16) | (sub_class << 8) | prog_if` — the prog-if
    /// is part of the class so an xHCI host (`0x0C_03_30`) is
    /// distinguished from the older USB host classes, exactly as the
    /// generic xHCI driver's bind key requires.
    ///
    /// The returned node carries **no** identity: its id and parent are
    /// unassigned placeholders ([`HwNode::set_identity`] is the kernel's
    /// to call). A bus driver does not name the child's id or its own
    /// node id — when the node is published through the `hw_emit_node`
    /// syscall the kernel assigns a fresh, collision-free id and sets the
    /// parent to the emitting driver's own matched node, so a driver can
    /// neither forge its tree position nor collide with an existing id
    /// (identity is kernel-provided, never
    /// caller-supplied;). No resource capabilities are
    /// attached here either; those are minted at the load gate.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if no function responds at `bdf` (an
    ///   absent function reads the all-ones vendor sentinel) — a
    ///   fail-closed refusal, never a fabricated node.
    /// * [`DriverError::DeviceFault`] if the configuration read cannot be
    ///   completed by the bus transport, or the node cannot be assembled.
    fn describe_function(&self, bdf: u64) -> Result<HwNode, DriverError>;

    /// Program function `bdf`'s legacy **MSI** capability with `message`,
    /// force a single vector, and enable it.
    ///
    /// The non-virtio counterpart of
    /// [`MsixBus::route_msix`](super::msix::MsixBus::route_msix) for a
    /// function that advertises the legacy MSI capability rather than MSI-X
    /// (the Pi 4's VL805 xHCI host): the kernel's interrupt controller mints
    /// the opaque [`MsiMessage`] (doorbell address + data), and this writes
    /// it into the capability's Message-Address/Message-Data registers, then
    /// sets MSI Enable with Multiple-Message-Enable cleared (exactly one
    /// vector). Unlike `route_msix` it needs no [`MmioMapper`] — the MSI
    /// capability lives entirely in configuration space. Bus mastering is
    /// left as it was: the message is delivered only once the function is
    /// a bus master ([`set_bus_master`](Self::set_bus_master)).
    ///
    /// The default implementation returns [`DriverError::Unsupported`], the
    /// correct shape for a bus seam that does not provision MSI (a test
    /// double, or a transport with no configuration-space MSI capability).
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the seam provisions no MSI (the
    ///   default).
    /// * [`DriverError::NotFound`] if the function advertises no MSI
    ///   capability.
    /// * [`DriverError::OutOfRange`] if `message.address` needs 64-bit
    ///   addressing but the capability is 32-bit only (fail closed).
    fn route_msi(&self, bdf: u64, message: MsiMessage) -> Result<(), DriverError> {
        let _ = (bdf, message);
        Err(DriverError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::bus::BusDevice;
    use crate::driver::mmio::MmioMapError;
    use crate::{HwDeviceClass, HwMatchKey};
    use core::cell::Cell;
    use core::ptr::NonNull;

    #[test]
    fn a_requester_id_is_the_bus_device_and_function_of_a_config_address() {
        let address = (0x12 << 16) | (0x1F << 11) | (0x7 << 8);
        assert_eq!(requester_id(address), 0x12FF);
        assert_eq!(config_address(0x12FF), address);
        assert_eq!(
            requester_id(address | 0xFC),
            0x12FF,
            "the register bits are not the function's"
        );
        assert_eq!(requester_id(0), 0);
    }

    #[test]
    fn a_pci_address_names_its_segment_beside_its_requester_id() {
        let address = PciAddress::at(0x0003, (0x12 << 16) | (0x1F << 11) | (0x7 << 8));
        assert_eq!(address.segment(), 3);
        assert_eq!(address.requester_id(), 0x12FF);
        assert_eq!(address.node_address(), 0x0003_12FF);
        assert_eq!(PciAddress::from_node_address(0x0003_12FF), address);
        assert_eq!(address.config_address(), config_address(0x12FF));
        assert_ne!(
            PciAddress::new(0, 0x12FF),
            PciAddress::new(1, 0x12FF),
            "one requester id on two segments is two functions"
        );
        for raw in [0, 0xFFFF, 0x0001_0000, u32::MAX] {
            assert_eq!(PciAddress::from_node_address(raw).node_address(), raw);
        }
    }

    #[test]
    fn a_function_address_packs_and_unpacks_within_pci_s_limits() {
        assert_eq!(
            function_address(0x12, 0x1F, 7),
            Some((0x12 << 16) | (0x1F << 11) | (0x7 << 8))
        );
        assert_eq!(
            function_of((0x12 << 16) | (0x1F << 11) | (0x7 << 8)),
            (0x12, 0x1F, 7)
        );
        assert_eq!(function_address(0, PCI_DEVICES, 0), None);
        assert_eq!(function_address(0, 0, PCI_FUNCTIONS), None);
        for bus in [0, 0x80, 0xFF] {
            for device in 0..PCI_DEVICES {
                for function in 0..PCI_FUNCTIONS {
                    let address = function_address(bus, device, function).unwrap();
                    assert_eq!(function_of(address), (bus, device, function));
                }
            }
        }
    }

    /// 4-byte-aligned backing so a window base satisfies
    /// `RegisterWindow::from_mapping`'s alignment contract.
    static mut BACKING: [u32; 16] = [0u32; 16];

    struct FakeMapper {
        grant: bool,
        last: Cell<Option<(u64, usize)>>,
    }

    impl MmioMapper for FakeMapper {
        fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
            if !self.grant {
                return Err(MmioMapError::CapabilityMissing);
            }
            self.last.set(Some((phys_base, len)));
            let base = NonNull::new(core::ptr::addr_of_mut!(BACKING).cast::<u8>())
                .expect("static is non-null");
            // SAFETY: single-threaded test; the static outlives the
            // window and the window only touches `len <= 64` bytes.
            Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len.min(64)) })
        }
    }

    struct FakeBus {
        bar_base: u64,
        bar_size: u64,
        decoding: Cell<bool>,
        mastering: Cell<bool>,
    }

    impl Bus for FakeBus {
        fn enumerate(&self, out: &mut [BusDevice]) -> Result<usize, DriverError> {
            if out.is_empty() {
                return Err(DriverError::BufferTooSmall);
            }
            out[0] = BusDevice {
                vendor: 0x1106,
                device: 0x3483,
                class: 0x0C03,
                reserved0: 0,
                address: 0x0001_0000,
            };
            Ok(1)
        }
    }

    impl PciBus for FakeBus {
        fn map_bar_window(
            &self,
            _bdf: u64,
            bar_index: u8,
            mapper: &dyn MmioMapper,
        ) -> Result<RegisterWindow, DriverError> {
            if bar_index != 0 {
                return Err(DriverError::NotFound);
            }
            if self.bar_size == 0 {
                return Err(DriverError::NotFound);
            }
            let len = usize::try_from(self.bar_size).map_err(|_| DriverError::LengthOutOfRange)?;
            mapper
                .map_window(self.bar_base, len)
                .map_err(MmioMapError::as_driver_error)
        }

        fn enable_memory_space(&self, _bdf: u64) -> Result<(), DriverError> {
            self.decoding.set(true);
            Ok(())
        }

        fn set_bus_master(&self, _bdf: u64, master: bool) -> Result<(), DriverError> {
            self.mastering.set(master);
            Ok(())
        }

        fn set_intx(&self, _bdf: u64, _raise: bool) -> Result<(), DriverError> {
            Ok(())
        }

        fn assign_bar(
            &self,
            _bdf: u64,
            bar_index: u8,
            window_base: u64,
            window_size: u64,
        ) -> Result<u64, DriverError> {
            if bar_index != 0 || self.bar_size == 0 {
                return Err(DriverError::NotFound);
            }
            // Already-based BAR: respected unchanged.
            if self.bar_base != 0 {
                return Ok(self.bar_base);
            }
            if self.bar_size > window_size {
                return Err(DriverError::OutOfRange);
            }
            Ok(window_base)
        }

        fn read_config(&self, _bdf: u64, offset: u16) -> Result<u32, DriverError> {
            // BAR0 at byte offset 0x10 reads back the assigned base;
            // every other offset reads zero (enough for the trait test).
            match offset & !0x3 {
                0x10 => Ok((self.bar_base & 0xFFFF_FFFF) as u32),
                _ => Ok(0),
            }
        }

        fn capability_header(&self, _bdf: u64, _id: u8) -> Result<u32, DriverError> {
            Err(DriverError::NotFound)
        }

        fn describe_function(&self, _bdf: u64) -> Result<HwNode, DriverError> {
            // Identity is unassigned: the kernel sets it on publish. Build
            // the node with placeholder id/parent the publish path
            // overwrites.
            let mut node = HwNode::new(0, crate::hwtree::HW_NODE_ROOT, HwDeviceClass::Bus);
            node.push_match_key(HwMatchKey::pci(0x1106, 0x3483, 0x0C_03_30))
                .map_err(|_| DriverError::DeviceFault)?;
            Ok(node)
        }
    }

    fn bus() -> FakeBus {
        FakeBus {
            bar_base: 0x6000_0000,
            bar_size: 0x40,
            decoding: Cell::new(false),
            mastering: Cell::new(false),
        }
    }

    #[test]
    fn trait_object_maps_the_bar_and_turns_decoding_and_mastering_on_and_off() {
        let bus = bus();
        let dyn_bus: &dyn PciBus = &bus;
        let mapper = FakeMapper {
            grant: true,
            last: Cell::new(None),
        };
        dyn_bus
            .enable_memory_space(0x0001_0000)
            .expect("memory decoding");
        let window = dyn_bus
            .map_bar_window(0x0001_0000, 0, &mapper)
            .expect("bar window");
        assert_eq!(window.len(), 0x40);
        assert_eq!(mapper.last.get(), Some((0x6000_0000, 0x40)));
        assert!(bus.decoding.get());
        assert!(!bus.mastering.get(), "decoding makes no bus master");
        dyn_bus.set_bus_master(0x0001_0000, true).expect("master");
        assert!(bus.mastering.get());
        dyn_bus.set_bus_master(0x0001_0000, false).expect("stop");
        assert!(!bus.mastering.get());
    }

    #[test]
    fn read_config_returns_the_dword_at_the_byte_offset() {
        let bus = bus();
        let dyn_bus: &dyn PciBus = &bus;
        // BAR0 byte offset 0x10 reads back the (low 32 bits of the)
        // assigned base; the byte offset is taken to its dword.
        assert_eq!(dyn_bus.read_config(0x0001_0000, 0x10), Ok(0x6000_0000));
        assert_eq!(dyn_bus.read_config(0x0001_0000, 0x12), Ok(0x6000_0000));
        assert_eq!(dyn_bus.read_config(0x0001_0000, 0x04), Ok(0));
    }

    #[test]
    fn missing_bar_is_not_found() {
        let bus = bus();
        let dyn_bus: &dyn PciBus = &bus;
        let mapper = FakeMapper {
            grant: true,
            last: Cell::new(None),
        };
        assert!(matches!(
            dyn_bus.map_bar_window(0x0001_0000, 2, &mapper),
            Err(DriverError::NotFound)
        ));
    }

    #[test]
    fn missing_capability_propagates_as_permission_denied() {
        let bus = bus();
        let dyn_bus: &dyn PciBus = &bus;
        let mapper = FakeMapper {
            grant: false,
            last: Cell::new(None),
        };
        assert!(matches!(
            dyn_bus.map_bar_window(0x0001_0000, 0, &mapper),
            Err(DriverError::PermissionDenied)
        ));
    }

    #[test]
    fn describe_function_emits_a_child_node_with_the_pci_match_key() {
        let bus = bus();
        let dyn_bus: &dyn PciBus = &bus;
        let node = dyn_bus
            .describe_function(0x0001_0000)
            .expect("describes the function");
        // The lone key is the function's vendor:device:24-bit class, so a
        // generic xHCI bind key (class `0x0C_03_30`, vendor/device
        // wildcard) resolves against it.
        assert_eq!(node.match_keys().len(), 1);
        let bind = HwMatchKey::pci(0, 0, 0x0C_03_30);
        assert!(bind.matches(&node.match_keys()[0]));
        // A bind key naming a different class does not.
        assert!(!HwMatchKey::pci(0, 0, 0x0C_03_20).matches(&node.match_keys()[0]));
    }
}
