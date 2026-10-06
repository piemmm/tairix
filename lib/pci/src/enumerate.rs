//! Enumeration core: bus walk, capability-list walk, BAR sizing.
//!
//! All logic is parameterised over [`ConfigSpace`] so the host-side
//! tests can substitute a table-driven mock that reproduces QEMU's
//! `q35` PCI tree byte for byte. The walk is bounded by hardware
//! limits (256 buses × 32 devices × 8 functions, 6 BARs per type-0
//! function, 48 capability entries — the legacy 256-byte
//! configuration space cannot fit more) so it terminates without
//! external timeouts.

use alloc::vec::Vec;

use tairix_abi::driver::bus::BusDevice;
use tairix_abi::driver::pci::{
    Quiesced, BUS_MASTER_ENABLE, COMMAND_OFFSET, INTERRUPT_DISABLE, IO_SPACE_ENABLE,
    MEMORY_SPACE_ENABLE, PCI_DEVICES, PCI_FUNCTIONS,
};
use tairix_abi::driver::virtio_pci::VIRTIO_PCI_CFG_NOTIFY;
use tairix_abi::hwtree::HW_NODE_ROOT;
use tairix_abi::{
    DriverError, HwDeviceClass, HwMatchKey, HwNode, MmioMapError, MmioMapper, MsiMessage,
    RegisterWindow, WindowError,
};

use crate::aperture::Apertures;
use crate::config::{
    BarDescriptor, BarKind, Capability, ConfigAddress, ConfigSpace, BUS_NUMBERS, CAP_ID_VENDOR,
    COMMAND_STATUS, DEVICE_BAR_SLOTS, EXTENDED_REGISTER, FIRST_BAR, HEADER_BRIDGE, HEADER_CARDBUS,
    HEADER_DEVICE, HEADER_TYPE, MULTIFUNCTION,
};
use crate::topology::{
    Acs, Ats, Confinement, Function, Header, Pasid, PciTopology, PortType, Pri, SrIov, Topology,
    BUSES,
};

/// The PCI Express capability's id (PCI Express Base 5.0 §7.5.3).
const CAP_ID_EXPRESS: u8 = 0x10;

/// Byte offsets within the PCI Express capability (PCI Express Base 5.0
/// §7.5.3): the Slot Capabilities register and the Device Control 2 register.
const EXPRESS_SLOT_CAPABILITIES: u8 = 0x14;
const EXPRESS_DEVICE_CONTROL_2: u8 = 0x28;
/// The PCI Express Capabilities register's Slot Implemented bit, in the
/// capability's header dword.
const EXPRESS_SLOT_IMPLEMENTED: u32 = 1 << 24;
const SLOT_HOT_PLUG_CAPABLE: u32 = 1 << 6;
const DEVICE_CONTROL_2_ARI_FORWARDING: u32 = 1 << 5;

/// Extended capability ids (PCI Express Base 5.0 §7.6, §9.3.3, §10.5).
const EXT_CAP_ID_ACS: u16 = 0x000D;
const EXT_CAP_ID_ATS: u16 = 0x000F;
const EXT_CAP_ID_SRIOV: u16 = 0x0010;
const EXT_CAP_ID_PRI: u16 = 0x0013;
const EXT_CAP_ID_PASID: u16 = 0x001B;

/// The enable bits of the translation services, each in the dword one past
/// its capability's header: ATS Control and PASID Control in the high half
/// beside a read-only capability half, PRI Control in the low half beside a
/// status half whose bits clear when written as ones.
const ATS_ENABLE: u32 = 1 << 31;
const PASID_ENABLE: u32 = 1 << 16;
const PASID_CONTROLS: u32 = 0b111 << 16;
const PRI_ENABLE: u32 = 1 << 0;

/// The high half of a control dword whose other half is a status register
/// whose bits a written one clears: written as zero.
const CLEARS_ON_WRITE: u32 = 0xFFFF_0000;

/// SR-IOV registers, as dword offsets from the capability's header: Control
/// beside its status half, then `NumVFs`, then First VF Offset beside VF
/// Stride.
const SRIOV_CONTROL: u16 = 2;
const SRIOV_NUM_VFS: u16 = 4;
const SRIOV_OFFSET_STRIDE: u16 = 5;
const SRIOV_VF_ENABLE: u32 = 1 << 0;
/// VF Enable and VF Memory Space Enable.
const SRIOV_VFS_ON: u32 = SRIOV_VF_ENABLE | (1 << 3);

/// Vendor-ID sentinel returned by the host bridge when no function
/// is present at a given `(bus, device, function)`.
const VENDOR_INVALID: u16 = 0xFFFF;

/// Status-register bit 4 — "Capabilities List".
const STATUS_CAP_LIST: u16 = 1 << 4;

/// Maximum number of capability-list entries the walker will follow.
///
/// The 256-byte legacy configuration space has at most ~48 dword
/// slots available for capabilities; the bound is set above that to
/// catch any walker bug (a circular `next` pointer) without spinning
/// forever.
const CAP_LIST_HARD_LIMIT: usize = 64;

/// MSI-X table entry size in bytes (PCI Local Bus 3.0 §6.8.2.9):
/// message address (8) + message data (4) + vector control (4).
const MSIX_ENTRY_LEN: usize = 16;

/// An MSI-X entry's vector control word with the entry masked.
const MSIX_VECTOR_MASKED: u32 = 1;

/// The writes that reprogram an MSI-X table entry (address low, address
/// high, data, vector control) with `message`, in order: masked first, since
/// changing an unmasked entry's address or data is undefined (PCI Local Bus
/// 3.0 §6.8.2.9), and unmasked last.
pub(crate) const fn msix_entry_writes(message: MsiMessage) -> [(usize, u32); 5] {
    let [b0, b1, b2, b3, b4, b5, b6, b7] = message.address.to_le_bytes();
    [
        (12, MSIX_VECTOR_MASKED),
        (0, u32::from_le_bytes([b0, b1, b2, b3])),
        (4, u32::from_le_bytes([b4, b5, b6, b7])),
        (8, message.data),
        (12, 0),
    ]
}

/// MSI-X Message Control "MSI-X Enable" bit. The Message Control
/// register occupies the high 16 bits of the capability header dword,
/// so its bit 15 lands at bit 31 of the dword.
const MSIX_CTRL_ENABLE: u32 = 1 << 31;

/// MSI-X Message Control "Function Mask" bit (bit 14 of Message
/// Control → bit 30 of the header dword); cleared so unmasked table
/// entries deliver.
const MSIX_CTRL_FUNCTION_MASK: u32 = 1 << 30;

/// The RW1C status half of the command/status dword, written as zero so no
/// latched status bit is cleared.
const COMMAND_BITS: u32 = 0xFFFF;

/// [`COMMAND_OFFSET`] as the byte offset a configuration address takes.
const COMMAND_REGISTER: u8 = {
    let [low, high] = COMMAND_OFFSET.to_le_bytes();
    assert!(high == 0, "the command register lies in the header");
    low
};

/// MSI Message Control "MSI Enable" bit (PCI Local Bus 3.0 §6.8.1.3, MC bit 0).
const MSI_MC_ENABLE: u16 = 1 << 0;

/// Bit offset of the Multiple Message Capable field within Message Control.
const MSI_MC_MMC_SHIFT: u32 = 1;

/// MSI Message Control "Multiple Message Capable" field (MC bits 3:1): the
/// log2 of how many vectors the function requests.
const MSI_MC_MMC_MASK: u16 = 0x7 << MSI_MC_MMC_SHIFT;

/// MSI Message Control "Multiple Message Enable" field (MC bits 6:4): the log2
/// of how many vectors the function may use. Cleared to request exactly one
/// vector — TAIRiX routes a single MSI per function, so a device must not
/// spread interrupts across vectors the kernel did not allocate.
const MSI_MC_MME_MASK: u16 = 0x7 << 4;

/// MSI Message Control "64-bit Address Capable" bit (MC bit 7).
const MSI_MC_ADDR64: u16 = 1 << 7;

/// MSI Message Control "Per-vector Masking Capable" bit (MC bit 8). When set,
/// the capability appends a 32-bit mask register; any bit left set suppresses
/// that vector's MSI write at the device.
const MSI_MC_PVM_CAPABLE: u16 = 1 << 8;

/// Bit offset of the Message Control register within the MSI capability header
/// dword: the low half holds the read-only capability id and next pointer, so
/// enabling MSI is a shifted read-modify-write of the same dword.
const MSI_CTRL_SHIFT: u32 = 16;

/// [`MSI_MC_ENABLE`] in the capability header dword's coordinates.
const MSI_CTRL_ENABLE: u32 = (MSI_MC_ENABLE as u32) << MSI_CTRL_SHIFT;

/// [`MSI_MC_MME_MASK`] in the capability header dword's coordinates.
const MSI_CTRL_MME_MASK: u32 = (MSI_MC_MME_MASK as u32) << MSI_CTRL_SHIFT;

/// The PCI bus driver instance.
///
/// Holds the [`ConfigSpace`] backend; everything else is
/// constructor-injected. The type is `pub(crate)`
/// — outside callers reach the enumeration through `dyn Bus`.
pub struct Pci<C: ConfigSpace> {
    config: C,
    /// Where memory BARs may decode, for a host the kernel owns; [`None`]
    /// for a bus driver's, whose BAR windows the kernel checks against its
    /// grants instead.
    apertures: Option<Apertures>,
}

impl<C: ConfigSpace> Pci<C> {
    /// Construct a new [`Pci`] wired to `config`, its memory BARs resolved
    /// through `apertures` where it has them.
    pub const fn new(config: C, apertures: Option<Apertures>) -> Self {
        Self { config, apertures }
    }

    /// The backend.
    pub(crate) const fn config_space(&self) -> &C {
        &self.config
    }

    /// Enumerate every responding function on every bus into `out`.
    ///
    /// Returns the number of entries written. If `out.len()` is
    /// smaller than the number of devices discovered, the method
    /// fills `out` and returns [`DriverError::BufferTooSmall`] —
    /// matching the [`Bus::enumerate`](tairix_abi::driver::bus::Bus)
    /// contract exactly.
    pub fn enumerate_into(&self, out: &mut [BusDevice]) -> Result<usize, DriverError> {
        let mut count = 0usize;
        self.each_function(|addr, address, id, _| {
            if let Some(slot) = out.get_mut(count) {
                *slot = self.bus_device(addr, address, id);
            }
            count += 1;
        });
        if count > out.len() {
            Err(DriverError::BufferTooSmall)
        } else {
            Ok(count)
        }
    }

    /// Visit every responding function on every bus, in address order, with
    /// its configuration address, its identity dword and whether its device
    /// holds more than one function. Bounded by PCI's own numbering, so it
    /// terminates without a timeout.
    ///
    /// Below a port that forwards every function number to the one device
    /// below it (ARI), each of the bus's 256 function numbers is that
    /// device's, whatever its function 0 says: a scan trusting the
    /// multi-function bit there would miss functions that master DMA.
    pub(crate) fn each_function(&self, mut visit: impl FnMut(ConfigAddress, u64, u32, bool)) {
        let mut ari = [false; BUSES];
        for bus in 0u8..=255 {
            if ari[usize::from(bus)] {
                self.each_ari_function(bus, &mut ari, &mut visit);
                continue;
            }
            for device in 0..PCI_DEVICES {
                let multifunction = self.is_multifunction(bus, device);
                let functions = if multifunction { PCI_FUNCTIONS } else { 1 };
                for function in 0..functions {
                    let addr = ConfigAddress {
                        bus,
                        device,
                        function,
                        register: 0,
                    };
                    self.visit_present(addr, multifunction, &mut ari, &mut visit);
                }
            }
        }
    }

    /// Visit every function of the one device on `bus`, which an ARI port
    /// forwards every function number to.
    fn each_ari_function(
        &self,
        bus: u8,
        ari: &mut [bool; BUSES],
        visit: &mut impl FnMut(ConfigAddress, u64, u32, bool),
    ) {
        let at = |number: u8| ConfigAddress {
            bus,
            device: number >> 3,
            function: number & (PCI_FUNCTIONS - 1),
            register: 0,
        };
        let present = |number: u8| low_u16(self.config.read32(at(number))) != VENDOR_INVALID;
        let multifunction = (0..=u8::MAX)
            .filter(|&number| present(number))
            .nth(1)
            .is_some();
        for number in 0..=u8::MAX {
            self.visit_present(at(number), multifunction, ari, visit);
        }
    }

    /// Visit the function at `addr` if one answers there, noting the bus an
    /// ARI port forwards to.
    fn visit_present(
        &self,
        addr: ConfigAddress,
        multifunction: bool,
        ari: &mut [bool; BUSES],
        visit: &mut impl FnMut(ConfigAddress, u64, u32, bool),
    ) {
        let id = self.config.read32(addr);
        if low_u16(id) == VENDOR_INVALID {
            return;
        }
        if let Some(secondary) = self.ari_secondary(addr) {
            ari[usize::from(secondary)] = true;
        }
        if let Some(address) = addr.pack_bdf() {
            visit(addr, address, id, multifunction);
        }
    }

    /// The secondary bus of the bridge at `addr`, when it forwards every
    /// function number to the device below it.
    fn ari_secondary(&self, addr: ConfigAddress) -> Option<u8> {
        let Header::Bridge {
            secondary,
            ari: true,
            ..
        } = self.header(addr)
        else {
            return None;
        };
        (secondary != 0).then_some(secondary)
    }

    /// What the header at `addr` says the function is.
    fn header(&self, addr: ConfigAddress) -> Header {
        match low_u8(self.config.read32(addr_with_reg(addr, HEADER_TYPE)) >> 16) & 0x7F {
            HEADER_BRIDGE | HEADER_CARDBUS => {
                let [_, secondary, subordinate, _] = self
                    .config
                    .read32(addr_with_reg(addr, BUS_NUMBERS))
                    .to_le_bytes();
                let ari = self.express(addr).is_some_and(|(offset, _)| {
                    let control = addr_with_byte_offset(addr, offset + EXPRESS_DEVICE_CONTROL_2);
                    self.config.read32(control) & DEVICE_CONTROL_2_ARI_FORWARDING != 0
                });
                Header::Bridge {
                    secondary,
                    subordinate,
                    ari,
                }
            }
            _ => Header::Endpoint,
        }
    }

    /// The PCI Express capability of the function at `addr`: its byte offset
    /// and header dword.
    fn express(&self, addr: ConfigAddress) -> Option<(u8, u32)> {
        self.legacy_capabilities(addr)?
            .find(|&(_, header)| low_u8(header) == CAP_ID_EXPRESS)
    }

    /// The [`BusDevice`] record of the function at `addr` (configuration
    /// `address`), whose identity dword is `id`.
    fn bus_device(&self, addr: ConfigAddress, address: u64, id: u32) -> BusDevice {
        BusDevice {
            vendor: u32::from(low_u16(id)),
            device: u32::from(low_u16(id >> 16)),
            class: self.read_class(addr),
            reserved0: 0,
            address,
        }
    }

    /// The legacy capability list of the function at `addr`, or [`None`]
    /// when its status register advertises none.
    pub(crate) fn legacy_capabilities(
        &self,
        addr: ConfigAddress,
    ) -> Option<LegacyCapabilities<'_, C>> {
        let status = low_u16(self.config.read32(addr_with_reg(addr, COMMAND_STATUS)) >> 16);
        if status & STATUS_CAP_LIST == 0 {
            return None;
        }
        // Cap pointer at config-space offset 0x34 (register dword 13).
        let first = low_u8(self.config.read32(addr_with_reg(addr, 13)) & 0xFC);
        Some(LegacyCapabilities {
            pci: self,
            addr,
            next: first,
            steps: 0,
        })
    }

    /// The function at `addr` (configuration `address`, identity dword
    /// `id`) as a topology walk records it, after `confinement` is applied.
    /// `external` says whether the platform describes it as an
    /// external-facing port.
    fn read_function(
        &self,
        addr: ConfigAddress,
        address: u64,
        id: u32,
        multifunction: bool,
        confinement: Confinement,
        external: bool,
    ) -> Function {
        let header = self.header(addr);
        let express = self.express(addr);
        let hot_plug = express.is_some_and(|(offset, header)| {
            header & EXPRESS_SLOT_IMPLEMENTED != 0
                && self.config.read32(addr_with_byte_offset(
                    addr,
                    offset + EXPRESS_SLOT_CAPABILITIES,
                )) & SLOT_HOT_PLUG_CAPABLE
                    != 0
        });
        let mut function = Function {
            address,
            vendor: low_u16(id),
            device: low_u16(id >> 16),
            class: self.read_class_24(addr),
            header,
            multifunction,
            express: express.map(|(_, header)| PortType::from_field(low_u8(header >> 20) & 0xF)),
            external_facing: matches!(header, Header::Bridge { .. }) && (hot_plug || external),
            acs: None,
            ats: None,
            pri: None,
            pasid: None,
            sriov: None,
        };
        // Only a PCI Express function has extended space to hold them. A list
        // naming one capability twice has each instance confined, and reports
        // whichever the hardware left on.
        if function.express.is_some() {
            for (header, dword) in self.extended_capabilities(addr) {
                let registers = ConfigAddress {
                    register: header + 1,
                    ..addr
                };
                match low_u16(dword) {
                    EXT_CAP_ID_ACS => function.acs = Some(self.acs(registers, confinement)),
                    EXT_CAP_ID_ATS => {
                        let left = self.turn_off(registers, ATS_ENABLE, 0, confinement);
                        function.ats = Some(Ats {
                            enabled: left & ATS_ENABLE != 0
                                || function.ats.is_some_and(|ats| ats.enabled),
                        });
                    }
                    EXT_CAP_ID_PRI => {
                        let left =
                            self.turn_off(registers, PRI_ENABLE, CLEARS_ON_WRITE, confinement);
                        function.pri = Some(Pri {
                            enabled: left & PRI_ENABLE != 0
                                || function.pri.is_some_and(|pri| pri.enabled),
                        });
                    }
                    EXT_CAP_ID_PASID => {
                        let left = self.turn_off(registers, PASID_CONTROLS, 0, confinement);
                        function.pasid = Some(Pasid {
                            enabled: left & PASID_ENABLE != 0
                                || function.pasid.is_some_and(|pasid| pasid.enabled),
                            width: low_u8(left >> 8) & 0x1F,
                        });
                    }
                    EXT_CAP_ID_SRIOV => {
                        let sriov = self.sriov(addr, header, confinement);
                        function.sriov = match function.sriov {
                            Some(earlier) if earlier.enabled && !sriov.enabled => Some(earlier),
                            _ => Some(sriov),
                        };
                    }
                    _ => {}
                }
            }
        }
        function
    }

    /// The ACS registers at `registers`, after turning on each isolating
    /// control they offer where `confinement` says so. The Capability half of
    /// the dword is read-only, so writing it back unchanged leaves it as it
    /// was.
    fn acs(&self, registers: ConfigAddress, confinement: Confinement) -> Acs {
        let read = || {
            let dword = self.config.read32(registers);
            Acs {
                capable: low_u16(dword),
                enabled: low_u16(dword >> 16),
            }
        };
        let found = read();
        let wanted = found.enabled | (found.capable & Acs::ISOLATING);
        if confinement == Confinement::Leave || wanted == found.enabled {
            return found;
        }
        self.config.write32(
            registers,
            u32::from(wanted) << 16 | u32::from(found.capable),
        );
        read()
    }

    /// Clear the `enable` bits of the control dword at `registers` where
    /// `confinement` says so, writing the bits in `clear_on_write` as zero,
    /// and answer the dword the hardware is left with.
    fn turn_off(
        &self,
        registers: ConfigAddress,
        enable: u32,
        clear_on_write: u32,
        confinement: Confinement,
    ) -> u32 {
        let dword = self.config.read32(registers);
        if dword & enable == 0 || confinement == Confinement::Leave {
            return dword;
        }
        self.config
            .write32(registers, dword & !enable & !clear_on_write);
        self.config.read32(registers)
    }

    /// The SR-IOV capability whose header is at dword `header`, its virtual
    /// functions turned off first where `confinement` says so.
    fn sriov(&self, addr: ConfigAddress, header: u16, confinement: Confinement) -> SrIov {
        let at = |offset: u16| ConfigAddress {
            register: header + offset,
            ..addr
        };
        let control = self.turn_off(
            at(SRIOV_CONTROL),
            SRIOV_VFS_ON,
            CLEARS_ON_WRITE,
            confinement,
        );
        let offset_stride = self.config.read32(at(SRIOV_OFFSET_STRIDE));
        SrIov {
            enabled: control & SRIOV_VF_ENABLE != 0,
            count: low_u16(self.config.read32(at(SRIOV_NUM_VFS))),
            offset: low_u16(offset_stride),
            stride: low_u16(offset_stride >> 16),
        }
    }

    /// The extended capability list of the `PCIe` function at `addr`: empty
    /// where the mechanism reaches no extended space.
    pub(crate) fn extended_capabilities(&self, addr: ConfigAddress) -> ExtendedCapabilities<'_, C> {
        ExtendedCapabilities {
            pci: self,
            addr,
            next: EXTENDED_REGISTER,
            steps: 0,
        }
    }

    /// Walk the function's capability list into `out`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if the function does not advertise
    ///   any capability list (status bit 4 clear).
    /// * [`DriverError::BufferTooSmall`] if `out` cannot hold every
    ///   discovered capability.
    /// * [`DriverError::DeviceFault`] if the cap-list walker exceeds
    ///   [`CAP_LIST_HARD_LIMIT`] — almost certainly a circular
    ///   `next` pointer planted by a malfunctioning device.
    pub fn capabilities(&self, bdf: u64, out: &mut [Capability]) -> Result<usize, DriverError> {
        let addr = unpack_bdf(bdf, 0);
        let mut list = self
            .legacy_capabilities(addr)
            .ok_or(DriverError::NotFound)?;
        let mut count = 0usize;
        for (cap_offset, header) in list.by_ref() {
            let msg_ctrl = low_u16(header >> 16);
            let entry = match low_u8(header) {
                0x05 => decode_msi(self, addr, cap_offset, msg_ctrl),
                0x11 => decode_msix(self, addr, cap_offset, msg_ctrl),
                CAP_ID_VENDOR => decode_virtio(self, addr, cap_offset, msg_ctrl),
                id => Capability::Other {
                    offset: cap_offset,
                    id,
                },
            };
            if let Some(slot) = out.get_mut(count) {
                *slot = entry;
            }
            count += 1;
        }
        if list.malformed() {
            // The budget ran out before a `next == 0` terminator: almost
            // certainly a circular list planted by a malfunctioning device.
            return Err(DriverError::DeviceFault);
        }
        if count > out.len() {
            Err(DriverError::BufferTooSmall)
        } else {
            Ok(count)
        }
    }

    /// Every BAR among the first `slots` of the function at `addr`, sized
    /// with its decoding off.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when they cannot be held.
    pub(crate) fn bars_of(
        &self,
        addr: ConfigAddress,
        slots: u8,
    ) -> Result<Vec<BarDescriptor>, DriverError> {
        let mut bars = Vec::new();
        bars.try_reserve_exact(usize::from(slots))
            .map_err(|_| DriverError::NoSpace)?;
        self.with_decode_off(addr, || {
            let mut slot = 0;
            while slot < slots {
                match self.size_slot(addr, slot, slots) {
                    Some((bar, span)) => {
                        bars.push(bar);
                        slot += span;
                    }
                    None => slot += 1,
                }
            }
        });
        Ok(bars)
    }

    /// Refuse a function whose header is not type 0, which alone carries six
    /// BARs.
    fn type_zero(&self, addr: ConfigAddress) -> Result<(), DriverError> {
        let header_type_byte = low_u8(self.config.read32(addr_with_reg(addr, HEADER_TYPE)) >> 16);
        if header_type_byte & 0x7F != HEADER_DEVICE {
            return Err(DriverError::Unsupported);
        }
        Ok(())
    }

    /// Run `f` with the function at `addr` decoding neither memory nor I/O,
    /// so a BAR written all ones while it is sized claims no address.
    pub(crate) fn with_decode_off<T>(&self, addr: ConfigAddress, f: impl FnOnce() -> T) -> T {
        let command_addr = addr_with_byte_offset(addr, COMMAND_REGISTER);
        let command = self.config.read32(command_addr) & COMMAND_BITS;
        let decoding = command & (MEMORY_SPACE_ENABLE | IO_SPACE_ENABLE);
        if decoding != 0 {
            self.config.write32(command_addr, command & !decoding);
        }
        let result = f();
        if decoding != 0 {
            self.config.write32(command_addr, command);
        }
        result
    }

    /// The BAR at slot `slot` of the function at `addr`, whose header carries
    /// `slots`, and the slots it spans, sized by writing all ones and
    /// restoring what it held; [`None`] for a slot no BAR implements, or a
    /// 64-bit BAR whose upper half would fall past the header's last. The
    /// function must not be decoding.
    pub(crate) fn size_slot(
        &self,
        addr: ConfigAddress,
        slot: u8,
        slots: u8,
    ) -> Option<(BarDescriptor, u8)> {
        let reg = FIRST_BAR + slot;
        let low = self.config.read32(addr_with_reg(addr, reg));
        let probe = |reg: u8, held: u32| {
            self.config.write32(addr_with_reg(addr, reg), u32::MAX);
            let mask = self.config.read32(addr_with_reg(addr, reg));
            self.config.write32(addr_with_reg(addr, reg), held);
            mask
        };
        if low & 0x1 != 0 {
            // An I/O BAR may decode only sixteen bits, its upper ones
            // reading zero: its size is its lowest writable bit.
            let mask = probe(reg, low) & 0xFFFF_FFFC;
            let size = u64::from(mask & mask.wrapping_neg());
            let descriptor = BarDescriptor {
                index: slot,
                kind: BarKind::Io,
                base: u64::from(low & 0xFFFF_FFFC),
                size,
                prefetchable: false,
            };
            return (size != 0).then_some((descriptor, 1));
        }
        let wide = (low >> 1) & 0x3 == 0x2;
        if wide && slot + 1 >= slots {
            return None;
        }
        let high = if wide {
            self.config.read32(addr_with_reg(addr, reg + 1))
        } else {
            0
        };
        let mask_low = probe(reg, low) & 0xFFFF_FFF0;
        // A 32-bit BAR decodes nothing above the 4 GiB line.
        let mask_high = if wide { probe(reg + 1, high) } else { u32::MAX };
        let mask = (u64::from(mask_high) << 32) | u64::from(mask_low);
        let size = mask & mask.wrapping_neg();
        if mask_low == 0 && (!wide || mask_high == 0) {
            return None;
        }
        let descriptor = BarDescriptor {
            index: slot,
            kind: if wide {
                BarKind::Memory64
            } else {
                BarKind::Memory32
            },
            base: (u64::from(high) << 32) | u64::from(low & 0xFFFF_FFF0),
            size,
            prefetchable: low & 0x8 != 0,
        };
        Some((descriptor, if wide { 2 } else { 1 }))
    }

    /// Resolve the memory BAR at `bar_index` on function `bdf` and ask
    /// the kernel `mapper` to map it, returning the resulting
    /// [`RegisterWindow`].
    ///
    /// This is the Stage 4.D Item 3 hand-off: the PCI bus driver
    /// resolves the device's register-block physical base and length
    /// from configuration space and asks the kernel's MMIO-map
    /// facility for a window over it. The driver never synthesises a
    /// pointer — the kernel allocates and validates the mapping. The returned window is what the bus driver
    /// hands to the virtio `PciTransport`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — no BAR with `bar_index` exists,
    ///   or the BAR is unused (`size == 0`).
    /// * [`DriverError::Unsupported`] — the BAR is an I/O-port BAR,
    ///   which is reached through port I/O rather than a mapped
    ///   register window, or the function is not a type-0 header.
    /// * [`DriverError::LengthOutOfRange`] — the BAR size does not fit
    ///   in `usize` on this target.
    /// * [`DriverError::PermissionDenied`] — the caller does not hold
    ///   [`CapabilityId::MMIO_MAP`](tairix_abi::CapabilityId::MMIO_MAP)
    ///   (propagated from the mapper).
    ///
    /// # Capabilities
    ///
    /// The `mapper` enforces
    /// [`CapabilityId::MMIO_MAP`](tairix_abi::CapabilityId::MMIO_MAP).
    pub fn map_bar_window(
        &self,
        bdf: u64,
        bar_index: u8,
        mapper: &dyn MmioMapper,
    ) -> Result<RegisterWindow, DriverError> {
        let bar = self.resolve_bar(bdf, bar_index)?;
        // An I/O-port BAR is reached through port I/O, not a mapped
        // register window; refuse to pretend otherwise.
        if matches!(bar.kind, BarKind::Io) {
            return Err(DriverError::Unsupported);
        }
        if bar.size == 0 {
            return Err(DriverError::NotFound);
        }
        let len = usize::try_from(bar.size).map_err(|_| DriverError::LengthOutOfRange)?;
        mapper
            .map_window(bar.base, len)
            .map_err(MmioMapError::as_driver_error)
    }

    /// Turn on decoding of function `bdf`'s memory BARs, leaving every
    /// other command bit as it was.
    ///
    /// The in-tree [`ConfigSpace`] backends' accesses are infallible,
    /// so this cannot fail; the [`PciBus`](tairix_abi::driver::pci::PciBus)
    /// trait method wraps the result in `Ok` and reserves the error
    /// arm for a future fallible transport.
    pub fn enable_memory_space(&self, bdf: u64) {
        self.update_command(bdf, MEMORY_SPACE_ENABLE, true);
    }

    /// Let function `bdf` master upstream memory requests, or stop it,
    /// leaving every other command bit as it was. Infallible for the same
    /// reason as [`enable_memory_space`](Self::enable_memory_space).
    pub fn set_bus_master(&self, bdf: u64, master: bool) {
        self.update_command(bdf, BUS_MASTER_ENABLE, master);
    }

    /// Let function `bdf` assert its INTx pin, or stop it, leaving every
    /// other command bit as it was. Infallible for the same reason as
    /// [`enable_memory_space`](Self::enable_memory_space).
    pub fn set_intx(&self, bdf: u64, raise: bool) {
        self.update_command(bdf, INTERRUPT_DISABLE, !raise);
    }

    /// A command write a device acts on even when nothing changes — a
    /// virtio function written with Bus Master Enable clear disables
    /// itself — so an unchanged bit is not written.
    fn update_command(&self, bdf: u64, bit: u32, on: bool) {
        let cmd_addr = addr_with_byte_offset(unpack_bdf(bdf, 0), COMMAND_REGISTER);
        let command = self.config.read32(cmd_addr) & COMMAND_BITS;
        let updated = if on { command | bit } else { command & !bit };
        if updated != command {
            self.config.write32(cmd_addr, updated);
        }
    }

    /// Read the configuration-space dword at byte `offset` of function
    /// `bdf`.
    ///
    /// `offset` is a byte offset into the function's configuration space;
    /// it is resolved to the dword it falls in (the low two bits are
    /// ignored) and the little-endian dword is returned exactly as
    /// configuration space holds it. A register the mechanism cannot reach
    /// — extended space through mechanism #1, or past 4 KiB — reads
    /// all-ones, as an absent function does. A read-only diagnostic
    /// accessor: it touches no state.
    #[must_use]
    pub fn read_config(&self, bdf: u64, offset: u16) -> u32 {
        self.config.read32(ConfigAddress {
            register: offset >> 2,
            ..unpack_bdf(bdf, 0)
        })
    }

    /// Assign a memory base to the BAR at `bar_index` on function
    /// `bdf` if it is currently **unassigned**, placing it inside the
    /// PCIe-bus window `[window_base, window_base + window_size)`.
    ///
    /// Firmware normally programs a function's BARs, but when the OS
    /// resets and re-enumerates the root complex (the BCM2711 PCIe
    /// bring-up) a downstream function's BAR address bits read zero: the
    /// BAR is sized and typed but carries no base, so a map of it would
    /// target physical address 0 and be refused. Assigning resources
    /// from the host bridge's outbound window is the PCI core's job,
    /// mirroring Linux's PCI resource assignment. This probes the BAR's
    /// size and type, and:
    ///
    /// * if the BAR already carries a non-zero base, leaves it untouched
    ///   and returns that base (idempotent — firmware's assignment is
    ///   respected);
    /// * otherwise places the BAR at the lowest size-aligned address in
    ///   the window, writes it (both dwords for a 64-bit BAR), and
    ///   returns the assigned base.
    ///
    /// The returned base is a **PCIe-bus** address (what the function's
    /// BAR decodes); the host bridge's [`MmioMapper`] translates it to a
    /// CPU-physical address when [`map_bar_window`](Self::map_bar_window)
    /// maps the window. This only ensures the BAR has a base to map; the
    /// caller maps it afterwards.
    ///
    /// The size probe is transparent — the original BAR value is written
    /// back before this returns, so a no-op (already-assigned) call
    /// leaves configuration space byte-for-byte unchanged.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — `bar_index` is out of range, or no
    ///   memory BAR is implemented at that slot (the size probe reads
    ///   back zero).
    /// * [`DriverError::Unsupported`] — the BAR is an I/O-port BAR
    ///   (reached through port I/O, not a mapped window), or the
    ///   function is not a type-0 header.
    /// * [`DriverError::OutOfRange`] — the BAR's size-aligned placement
    ///   does not fit inside the window, or a 32-bit BAR would land
    ///   above the 4 GiB line (fail closed).
    pub fn assign_bar(
        &self,
        bdf: u64,
        bar_index: u8,
        window_base: u64,
        window_size: u64,
    ) -> Result<u64, DriverError> {
        if bar_index >= DEVICE_BAR_SLOTS {
            return Err(DriverError::NotFound);
        }
        let addr = unpack_bdf(bdf, 0);
        self.type_zero(addr)?;
        let bar = self
            .with_decode_off(addr, || self.size_slot(addr, bar_index, DEVICE_BAR_SLOTS))
            .map(|(bar, _)| bar)
            .ok_or(DriverError::NotFound)?;
        // An I/O-port BAR is reached through port I/O, never a mapped
        // memory window; refuse to assign it a memory base.
        if bar.kind == BarKind::Io {
            return Err(DriverError::Unsupported);
        }
        // A BAR firmware already based is left exactly as found.
        if bar.base != 0 {
            return Ok(bar.base);
        }
        let window_end = window_base
            .checked_add(window_size)
            .ok_or(DriverError::OutOfRange)?;
        let aligned =
            crate::assign::align_up(window_base, bar.size).ok_or(DriverError::OutOfRange)?;
        let end = aligned
            .checked_add(bar.size)
            .ok_or(DriverError::OutOfRange)?;
        if end > window_end || (bar.kind == BarKind::Memory32 && end > 1 << 32) {
            return Err(DriverError::OutOfRange);
        }
        self.write_bar(addr, &bar, aligned);
        Ok(aligned)
    }

    /// Point `bar` of the function at `addr` at `base`, keeping its type
    /// bits.
    pub(crate) fn write_bar(&self, addr: ConfigAddress, bar: &BarDescriptor, base: u64) {
        let reg = FIRST_BAR + bar.index;
        let control = self.config.read32(addr_with_reg(addr, reg)) & 0xF;
        let mask = if bar.kind == BarKind::Io { 0x3 } else { 0xF };
        self.config.write32(
            addr_with_reg(addr, reg),
            (low_dword(base) & !mask) | (control & mask),
        );
        if bar.kind == BarKind::Memory64 {
            self.config
                .write32(addr_with_reg(addr, reg + 1), high_dword(base));
        }
    }

    /// Resolve the virtio-1.x configuration structure of kind `cfg_type`
    /// on function `bdf` to its CPU-physical `(base, len)` window,
    /// **without mapping it**.
    ///
    /// The bus driver walks the function's capability list, locates the
    /// vendor-specific virtio capability of the requested `cfg_type` (one
    /// of the [`VIRTIO_PCI_CFG_*`](tairix_abi::driver::virtio_pci)
    /// discriminants), and resolves the `(bar, bar_offset, length)` triple to
    /// a CPU-physical base. This is the resolve primitive the two-process
    /// driver contract grants to a user-space driver, which maps the window in
    /// its own address space through its capability-gated MMIO facility;
    /// [`map_virtio_window`] is the in-kernel resolve-and-map sibling.
    ///
    /// [`map_virtio_window`]: tairix_abi::driver::virtio_pci::VirtioPciBus::map_virtio_window
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — the function advertises no virtio
    ///   capability of `cfg_type`, or the underlying BAR is unused.
    /// * [`DriverError::Unsupported`] — the structure lives in an
    ///   I/O-port BAR, which is reached through port I/O rather than a
    ///   mapped register window, or the function is not a type-0 header.
    /// * [`DriverError::OutOfRange`] — the structure's
    ///   `bar_offset + length` exceeds the resolved BAR size.
    /// * [`DriverError::LengthOutOfRange`] — the region length does not
    ///   fit in `usize` on this target.
    pub fn virtio_window_region(
        &self,
        bdf: u64,
        cfg_type: u8,
    ) -> Result<(u64, usize), DriverError> {
        let (bar_index, bar_offset, length) = self.find_virtio_region(bdf, cfg_type)?;
        let bar = self.resolve_bar(bdf, bar_index)?;
        if matches!(bar.kind, BarKind::Io) {
            return Err(DriverError::Unsupported);
        }
        let end = u64::from(bar_offset)
            .checked_add(u64::from(length))
            .ok_or(DriverError::OutOfRange)?;
        if length == 0 || end > bar.size {
            return Err(DriverError::OutOfRange);
        }
        // `bar.base + bar_offset` stays within the BAR's reserved span
        // (checked above), so the addition cannot overflow the address.
        let phys_base = bar
            .base
            .checked_add(u64::from(bar_offset))
            .ok_or(DriverError::OutOfRange)?;
        let len = usize::try_from(length).map_err(|_| DriverError::LengthOutOfRange)?;
        Ok((phys_base, len))
    }

    /// Read the `notify_off_multiplier` from the function's virtio
    /// notification capability.
    ///
    /// Returned alongside the four windows from [`map_virtio_window`] to
    /// populate `PciTransport`'s notification scale (virtio 1.x §4.1.4.4).
    ///
    /// [`map_virtio_window`]: tairix_abi::driver::virtio_pci::VirtioPciBus::map_virtio_window
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — the function advertises no virtio
    ///   notification capability, or no capability list at all.
    /// * [`DriverError::BufferTooSmall`] / [`DriverError::DeviceFault`]
    ///   — propagated from the capability-list walk.
    pub fn virtio_notify_off_multiplier(&self, bdf: u64) -> Result<u32, DriverError> {
        let mut caps = [Capability::Other { offset: 0, id: 0 }; CAP_LIST_HARD_LIMIT];
        let n = self.capabilities(bdf, &mut caps)?;
        caps[..n]
            .iter()
            .find_map(|c| match *c {
                Capability::VirtioNotify {
                    notify_off_multiplier,
                    ..
                } => Some(notify_off_multiplier),
                _ => None,
            })
            .ok_or(DriverError::NotFound)
    }

    /// Program MSI-X table `entry` of function `bdf` with `message`,
    /// unmask the entry, and enable MSI-X on the function.
    ///
    /// This is the interrupt-routing hand-off a virtio (or any
    /// MSI-X-capable) driver needs: the kernel's interrupt controller
    /// mints an [`MsiMessage`] for a chosen vector/destination, and the
    /// bus driver writes it into the device's table and flips the
    /// enable bit. The driver never synthesises a pointer — the table
    /// write goes through a kernel-mapped [`RegisterWindow`] obtained
    /// from `mapper`. Memory decoding is turned on, since the table lives
    /// in a BAR; bus mastering is left as it was, so the message is
    /// delivered only once the function's owner makes it a bus master.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — the function advertises no MSI-X
    ///   capability, or no capability list at all.
    /// * [`DriverError::OutOfRange`] — `entry` is beyond the function's
    ///   MSI-X table, or the addressed entry overruns its BAR.
    /// * [`DriverError::Unsupported`] — the table lives in an I/O-port
    ///   BAR, which is not memory-mappable, or the function is not a
    ///   type-0 header.
    /// * [`DriverError::LengthOutOfRange`] — the region length does not
    ///   fit in `usize` on this target (propagated from the mapper).
    /// * [`DriverError::PermissionDenied`] — the caller does not hold
    ///   [`CapabilityId::MMIO_MAP`](tairix_abi::CapabilityId::MMIO_MAP)
    ///   (propagated from the mapper).
    /// * [`DriverError::BufferTooSmall`] / [`DriverError::DeviceFault`]
    ///   — propagated from the capability-list walk.
    ///
    /// # Capabilities
    ///
    /// The `mapper` enforces
    /// [`CapabilityId::MMIO_MAP`](tairix_abi::CapabilityId::MMIO_MAP).
    pub fn route_msix(
        &self,
        bdf: u64,
        entry: u16,
        message: MsiMessage,
        mapper: &dyn MmioMapper,
    ) -> Result<(), DriverError> {
        let (cap_offset, table_size, table_bar, table_offset) = self.find_msix(bdf)?;
        if entry >= table_size {
            return Err(DriverError::OutOfRange);
        }
        let bar = self.resolve_bar(bdf, table_bar)?;
        if matches!(bar.kind, BarKind::Io) {
            return Err(DriverError::Unsupported);
        }
        // Byte offset of this entry within the table BAR, bounds-checked
        // against the BAR's reserved span before any access.
        let entry_off = u64::from(table_offset)
            .checked_add(u64::from(entry).wrapping_mul(MSIX_ENTRY_LEN as u64))
            .ok_or(DriverError::OutOfRange)?;
        let end = entry_off
            .checked_add(MSIX_ENTRY_LEN as u64)
            .ok_or(DriverError::OutOfRange)?;
        if end > bar.size {
            return Err(DriverError::OutOfRange);
        }
        let phys = bar
            .base
            .checked_add(entry_off)
            .ok_or(DriverError::OutOfRange)?;
        self.enable_memory_space(bdf);
        let window = mapper
            .map_window(phys, MSIX_ENTRY_LEN)
            .map_err(MmioMapError::as_driver_error)?;
        for (offset, value) in msix_entry_writes(message) {
            window
                .write_u32(offset, value)
                .map_err(WindowError::as_driver_error)?;
        }
        // Enable MSI-X function-wide and clear the function mask so the
        // freshly-unmasked entry can deliver. The Message Control
        // register lives in the high 16 bits of the capability header
        // dword; cap_id / next-pointer in the low 16 bits are
        // read-only and ignore writes.
        let header_addr = addr_with_byte_offset(unpack_bdf(bdf, 0), cap_offset);
        let header = self.config.read32(header_addr);
        let updated = (header | MSIX_CTRL_ENABLE) & !MSIX_CTRL_FUNCTION_MASK;
        self.config.write32(header_addr, updated);
        Ok(())
    }

    /// Program function `bdf`'s **MSI** (not MSI-X) capability with
    /// `message`, force a single vector, and enable it.
    ///
    /// The MSI interrupt-routing hand-off for a function that advertises
    /// the legacy MSI capability rather than MSI-X (the Pi 4's VL805 xHCI
    /// host): the kernel's interrupt controller mints the
    /// [`MsiMessage`] (the doorbell address + the data word that selects
    /// the vector — on the BCM2711 PCIe RC, the internal MSI controller's
    /// doorbell), and this writes it into the capability's Message Address
    /// and Message Data registers, then sets MSI Enable with Multiple
    /// Message Enable cleared (exactly one vector).
    ///
    /// Unlike [`route_msix`](Self::route_msix) this needs no `MmioMapper`:
    /// the MSI capability lives entirely in configuration space, reached
    /// through the same [`ConfigSpace`] backend, so there is no BAR table
    /// to map. Bus mastering is left as it was: an MSI is an upstream memory
    /// write, so it is delivered only once the function's owner makes it a
    /// bus master.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — the function advertises no MSI
    ///   capability (or no capability list at all).
    /// * [`DriverError::OutOfRange`] — `message.address` needs 64-bit
    ///   addressing but the capability is 32-bit only (writing the low
    ///   half alone would deliver to the wrong address — fail closed).
    pub fn route_msi(&self, bdf: u64, message: MsiMessage) -> Result<(), DriverError> {
        let (cap_offset, addr64, per_vector_masking) = self.find_msi(bdf)?;
        let base = unpack_bdf(bdf, 0);
        if addr64 || message.address >> 32 == 0 {
            // Off while the message changes: a function raising it between
            // the address and the data writes would send half of each.
            let header_addr = addr_with_byte_offset(base, cap_offset);
            let header = self.config.read32(header_addr);
            self.config.write32(header_addr, header & !MSI_CTRL_ENABLE);
        }
        // Message Address (low). Bits 1:0 are reserved and must be written
        // zero (the doorbell is at least dword-aligned, §6.8.1.1).
        self.config.write32(
            addr_with_byte_offset(base, cap_offset + 4),
            (message.address & 0xFFFF_FFFC) as u32,
        );
        if addr64 {
            // 64-bit capable: upper address dword at +0x08, Message Data
            // at +0x0C (§6.8.1). The data is a 16-bit field; the upper
            // half is reserved for a function without per-vector masking,
            // so writing it zero is correct.
            self.config.write32(
                addr_with_byte_offset(base, cap_offset + 8),
                (message.address >> 32) as u32,
            );
            self.config.write32(
                addr_with_byte_offset(base, cap_offset + 0x0C),
                message.data & 0xFFFF,
            );
            if per_vector_masking {
                self.config
                    .write32(addr_with_byte_offset(base, cap_offset + 0x10), 0);
            }
        } else {
            // 32-bit capable only: a doorbell above 4 GiB cannot be
            // expressed, so fail closed rather than truncate it.
            if message.address >> 32 != 0 {
                return Err(DriverError::OutOfRange);
            }
            self.config.write32(
                addr_with_byte_offset(base, cap_offset + 8),
                message.data & 0xFFFF,
            );
            if per_vector_masking {
                self.config
                    .write32(addr_with_byte_offset(base, cap_offset + 0x0C), 0);
            }
        }
        // Enable MSI and force Multiple Message Enable to 0 (one vector),
        // so the function delivers only the single doorbell the kernel
        // allocated. The Message Control register is the high 16 bits of
        // the capability header dword; cap_id / next-pointer in the low
        // 16 bits are read-only and ignore writes.
        let header_addr = addr_with_byte_offset(base, cap_offset);
        let header = self.config.read32(header_addr);
        let updated = (header & !MSI_CTRL_MME_MASK) | MSI_CTRL_ENABLE;
        self.config.write32(header_addr, updated);
        Ok(())
    }

    /// Locate the function's MSI capability, returning its
    /// `(cap_offset, addressing_64bit, per_vector_masking)`.
    fn find_msi(&self, bdf: u64) -> Result<(u8, bool, bool), DriverError> {
        let mut caps = [Capability::Other { offset: 0, id: 0 }; CAP_LIST_HARD_LIMIT];
        let n = self.capabilities(bdf, &mut caps)?;
        caps[..n]
            .iter()
            .find_map(|c| match *c {
                Capability::Msi {
                    offset,
                    addressing_64bit,
                    per_vector_masking,
                    ..
                } => Some((offset, addressing_64bit, per_vector_masking)),
                _ => None,
            })
            .ok_or(DriverError::NotFound)
    }

    /// Locate the function's MSI-X capability, returning its
    /// `(cap_offset, table_size, table_bar, table_offset)`.
    fn find_msix(&self, bdf: u64) -> Result<(u8, u16, u8, u32), DriverError> {
        let mut caps = [Capability::Other { offset: 0, id: 0 }; CAP_LIST_HARD_LIMIT];
        let n = self.capabilities(bdf, &mut caps)?;
        caps[..n]
            .iter()
            .find_map(|c| match *c {
                Capability::MsiX {
                    offset,
                    table_size,
                    table_bar,
                    table_offset,
                    ..
                } => Some((offset, table_size, table_bar, table_offset)),
                _ => None,
            })
            .ok_or(DriverError::NotFound)
    }

    /// Locate the virtio config region of `cfg_type`, returning its
    /// `(bar_index, bar_offset, length)`.
    fn find_virtio_region(&self, bdf: u64, cfg_type: u8) -> Result<(u8, u32, u32), DriverError> {
        let mut caps = [Capability::Other { offset: 0, id: 0 }; CAP_LIST_HARD_LIMIT];
        let n = self.capabilities(bdf, &mut caps)?;
        caps[..n]
            .iter()
            .find_map(|c| match *c {
                Capability::Virtio {
                    cfg_type: ct,
                    bar,
                    bar_offset,
                    length,
                    ..
                } if ct == cfg_type => Some((bar, bar_offset, length)),
                Capability::VirtioNotify {
                    bar,
                    bar_offset,
                    length,
                    ..
                } if cfg_type == VIRTIO_PCI_CFG_NOTIFY => Some((bar, bar_offset, length)),
                _ => None,
            })
            .ok_or(DriverError::NotFound)
    }

    /// Resolve a single BAR descriptor by index.
    /// The BAR at slot `bar_index` of `bdf`, a memory BAR's base the CPU
    /// physical address it decodes at where the host has apertures.
    ///
    /// A function chooses what its BAR reads back, so the apertures are
    /// checked on every resolution: a memory BAR outside them, or over
    /// memory, is refused.
    fn resolve_bar(&self, bdf: u64, bar_index: u8) -> Result<BarDescriptor, DriverError> {
        let addr = unpack_bdf(bdf, 0);
        self.type_zero(addr)?;
        // A slot holding a 64-bit BAR's upper half begins no BAR.
        let mut slot = 0u8;
        while slot < bar_index {
            let low = self.config.read32(addr_with_reg(addr, FIRST_BAR + slot));
            slot += if low & 0x1 == 0 && (low >> 1) & 0x3 == 0x2 {
                2
            } else {
                1
            };
        }
        if slot != bar_index || slot >= DEVICE_BAR_SLOTS {
            return Err(DriverError::NotFound);
        }
        let (mut bar, _) = self
            .with_decode_off(addr, || self.size_slot(addr, slot, DEVICE_BAR_SLOTS))
            .ok_or(DriverError::NotFound)?;
        if bar.kind == BarKind::Io {
            return Ok(bar);
        }
        if let Some(apertures) = &self.apertures {
            bar.base = apertures
                .cpu_range(bar.base, bar.size)
                .ok_or(DriverError::PermissionDenied)?
                .start;
        }
        Ok(bar)
    }

    fn is_multifunction(&self, bus: u8, device: u8) -> bool {
        let addr = ConfigAddress {
            bus,
            device,
            function: 0,
            register: 3,
        };
        // Reading function 0 first: if vendor is invalid the slot
        // is empty and we needn't probe higher functions.
        let id_addr = ConfigAddress {
            register: 0,
            ..addr
        };
        let id = self.config.read32(id_addr);
        if low_u16(id) == VENDOR_INVALID {
            return false;
        }
        let header_type = low_u8(self.config.read32(addr) >> 16);
        header_type & MULTIFUNCTION != 0
    }

    fn read_class(&self, base_addr: ConfigAddress) -> u16 {
        // Class is the upper 16 bits of dword 2: class code (high
        // byte) plus subclass code (low byte). Programming interface
        // and revision ID live in the lower 16 bits and are not part
        // of the [`BusDevice::class`] field for `abi-v1`.
        let dword = self.config.read32(addr_with_reg(base_addr, 2));
        low_u16(dword >> 16)
    }

    /// Read function `base_addr`'s full **24-bit** class code
    /// `(base_class << 16) | (sub_class << 8) | prog_if`.
    ///
    /// Unlike [`read_class`](Self::read_class) — which yields only the
    /// base + sub-class for the 16-bit [`BusDevice::class`] field — this
    /// keeps the programming interface (config dword 2, byte 1), so an
    /// xHCI USB host (`0x0C_03_30`) is distinguished from the older
    /// OHCI/UHCI/EHCI host classes that share `0x0C_03`. The low byte
    /// (revision id) is masked off.
    fn read_class_24(&self, base_addr: ConfigAddress) -> u32 {
        let dword = self.config.read32(addr_with_reg(base_addr, 2));
        (dword >> 8) & 0x00FF_FFFF
    }

    /// Describe the function at `bdf` as a discovered child [`HwNode`].
    ///
    /// The node carries one [`HwMatchKey::pci`] of the function's
    /// `vendor:device` and its full 24-bit class
    /// ([`read_class_24`](Self::read_class_24)), so `devmgr` resolves a
    /// driver's signed bind table against it. The
    /// node's [`HwDeviceClass`] is derived from the PCI base class. Its
    /// identity (id/parent) is left unassigned: the `hw_emit_node` publish
    /// path assigns a fresh, collision-free id and the emitter's own node
    /// as parent.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if no function responds at `bdf` (the
    ///   vendor id reads the all-ones sentinel) — fail closed, never a
    ///   fabricated node.
    /// * [`DriverError::DeviceFault`] if the match key cannot be pushed.
    pub fn describe_function(&self, bdf: u64) -> Result<HwNode, DriverError> {
        let addr = unpack_bdf(bdf, 0);
        let id = self.config.read32(addr);
        let vendor = low_u16(id);
        if vendor == VENDOR_INVALID {
            return Err(DriverError::NotFound);
        }
        let device = low_u16(id >> 16);
        let class24 = self.read_class_24(addr);
        // The base class is byte 3 of config dword 2 (bits 16..24 of the
        // 24-bit code); `low_u8` masks to 8 bits, so the cast is lossless.
        let base_class = low_u8(class24 >> 16);
        // Identity is unassigned: the `hw_emit_node` publish path assigns a
        // fresh, collision-free id and the emitter's own node as parent. Build with placeholder id/parent it
        // overwrites.
        let mut node = HwNode::new(0, HW_NODE_ROOT, device_class_from_base(base_class));
        node.push_match_key(HwMatchKey::pci(vendor, device, class24))
            .map_err(|_| DriverError::DeviceFault)?;
        Ok(node)
    }
}

impl<C: ConfigSpace> PciTopology for Pci<C> {
    fn topology(
        &self,
        confinement: Confinement,
        external: &dyn Fn(u64) -> bool,
    ) -> Result<Topology, DriverError> {
        let mut functions = Vec::new();
        let mut exhausted = false;
        self.each_function(|addr, address, id, multifunction| {
            if functions.try_reserve(1).is_ok() {
                functions.push(self.read_function(
                    addr,
                    address,
                    id,
                    multifunction,
                    confinement,
                    external(address),
                ));
            } else {
                exhausted = true;
            }
        });
        if exhausted {
            return Err(DriverError::NoSpace);
        }
        Ok(Topology::new(functions)?)
    }

    fn quiesce(&self, stopped: &dyn Fn(&Function) -> bool) -> Quiesced {
        let masters =
            |address: u64| self.read_config(address, COMMAND_OFFSET) & BUS_MASTER_ENABLE != 0;
        let mut quiesced = Quiesced::default();
        self.each_function(|addr, address, id, multifunction| {
            let function =
                self.read_function(addr, address, id, multifunction, Confinement::Leave, false);
            if !stopped(&function) || !masters(address) {
                return;
            }
            self.set_bus_master(address, false);
            if masters(address) {
                quiesced.refused += 1;
            } else {
                quiesced.stopped += 1;
            }
        });
        quiesced
    }
}

#[inline]
fn low_u8(v: u32) -> u8 {
    // Masking to 8 bits then casting is lossless by construction.
    (v & 0xFF) as u8
}

#[inline]
fn low_u16(v: u32) -> u16 {
    // Masking to 16 bits then casting is lossless by construction.
    (v & 0xFFFF) as u16
}

/// Low 32 bits of a 64-bit BAR base (the value written to the BAR's
/// own dword).
#[inline]
const fn low_dword(value: u64) -> u32 {
    let bytes = value.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// High 32 bits of a 64-bit BAR base (the value written to the BAR's
/// upper dword).
#[inline]
const fn high_dword(value: u64) -> u32 {
    let bytes = value.to_le_bytes();
    u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
}

/// Map a PCI base class code (PCI Local Bus 3.0 Appendix D) to the
/// architecture-neutral [`HwDeviceClass`] a discovered node carries.
///
/// The class is informational on the node — driver binding is decided
/// by the [`HwMatchKey`], not the class — so an
/// unrecognised base class is reported as [`HwDeviceClass::Other`]
/// rather than guessed.
fn device_class_from_base(base_class: u8) -> HwDeviceClass {
    match base_class {
        // Mass-storage controller.
        0x01 => HwDeviceClass::Storage,
        // Network controller.
        0x02 => HwDeviceClass::Network,
        // Display controller.
        0x03 => HwDeviceClass::Display,
        // Multimedia controller, which on every modern board is an HDA
        // controller or a virtio sound device.
        0x04 => HwDeviceClass::Audio,
        // Bridge (0x06) and serial-bus controller (0x0C, incl. USB
        // host controllers) are buses to further devices.
        0x06 | 0x0C => HwDeviceClass::Bus,
        // Processing accelerator: a device that computes rather than moves.
        0x12 => HwDeviceClass::Accelerator,
        _ => HwDeviceClass::Other,
    }
}

#[inline]
fn addr_with_reg(addr: ConfigAddress, register: u8) -> ConfigAddress {
    ConfigAddress {
        register: u16::from(register),
        ..addr
    }
}

#[inline]
fn addr_with_byte_offset(addr: ConfigAddress, byte_offset: u8) -> ConfigAddress {
    addr_with_reg(addr, byte_offset >> 2)
}

/// A function's legacy capability list: each entry's offset and header dword.
/// A circular list stops at [`CAP_LIST_HARD_LIMIT`] entries, and says so.
pub(crate) struct LegacyCapabilities<'p, C: ConfigSpace> {
    pci: &'p Pci<C>,
    addr: ConfigAddress,
    next: u8,
    steps: usize,
}

impl<C: ConfigSpace> LegacyCapabilities<'_, C> {
    /// Whether the walk gave up on a list that never ended.
    pub(crate) fn malformed(&self) -> bool {
        self.next != 0 && self.steps >= CAP_LIST_HARD_LIMIT
    }
}

impl<C: ConfigSpace> Iterator for LegacyCapabilities<'_, C> {
    type Item = (u8, u32);

    fn next(&mut self) -> Option<(u8, u32)> {
        if self.next == 0 || self.steps >= CAP_LIST_HARD_LIMIT {
            return None;
        }
        self.steps += 1;
        let offset = self.next;
        let header = self
            .pci
            .config
            .read32(addr_with_byte_offset(self.addr, offset));
        self.next = low_u8((header >> 8) & 0xFC);
        Some((offset, header))
    }
}

/// Entries a `PCIe` function's extended capability list can hold: each takes
/// at least its header dword, in the 960 dwords past the legacy space.
const EXTENDED_LIST_LIMIT: usize = 960;

/// A `PCIe` function's extended capability list (PCI Express Base 5.0
/// §7.6): each entry's dword index and header. It ends at an empty or
/// absent header, a `next` of zero, or a `next` pointing back into the
/// legacy space, which no well-formed list does.
pub(crate) struct ExtendedCapabilities<'p, C: ConfigSpace> {
    pci: &'p Pci<C>,
    addr: ConfigAddress,
    next: u16,
    steps: usize,
}

impl<C: ConfigSpace> Iterator for ExtendedCapabilities<'_, C> {
    type Item = (u16, u32);

    fn next(&mut self) -> Option<(u16, u32)> {
        if self.next < EXTENDED_REGISTER || self.steps >= EXTENDED_LIST_LIMIT {
            return None;
        }
        self.steps += 1;
        let register = self.next;
        let header = self.pci.config.read32(ConfigAddress {
            register,
            ..self.addr
        });
        if header == 0 || header == 0xFFFF_FFFF {
            return None;
        }
        self.next = u16::try_from(header >> 22).unwrap_or(0);
        Some((register, header))
    }
}

fn unpack_bdf(bdf: u64, register: u8) -> ConfigAddress {
    let (bus, device, function) = tairix_abi::driver::pci::function_of(bdf);
    ConfigAddress {
        bus,
        device,
        function,
        register: u16::from(register),
    }
}

fn decode_msi<C: ConfigSpace>(
    _this: &Pci<C>,
    _base: ConfigAddress,
    offset: u8,
    msg_ctrl: u16,
) -> Capability {
    // `mmc` is a 3-bit field; the mask + cast is lossless.
    let mmc = ((msg_ctrl & MSI_MC_MMC_MASK) >> MSI_MC_MMC_SHIFT) as u8;
    Capability::Msi {
        offset,
        message_count: 1 << mmc,
        addressing_64bit: msg_ctrl & MSI_MC_ADDR64 != 0,
        per_vector_masking: msg_ctrl & MSI_MC_PVM_CAPABLE != 0,
    }
}

fn decode_msix<C: ConfigSpace>(
    this: &Pci<C>,
    base: ConfigAddress,
    offset: u8,
    msg_ctrl: u16,
) -> Capability {
    let table_size = (msg_ctrl & 0x7FF) + 1;
    // Table offset/BIR lives at cap_offset + 4 (dword 1 of cap).
    let table_dword = this.config.read32(addr_with_byte_offset(base, offset + 4));
    let pba_dword = this.config.read32(addr_with_byte_offset(base, offset + 8));
    Capability::MsiX {
        offset,
        table_size,
        // Mask + cast: `table_dword & 0x7` is a 3-bit field, lossless.
        table_bar: (table_dword & 0x7) as u8,
        table_offset: table_dword & 0xFFFF_FFF8,
        pba_bar: (pba_dword & 0x7) as u8,
        pba_offset: pba_dword & 0xFFFF_FFF8,
    }
}

/// A function's virtio configuration-access capability: its `bar`, `offset`
/// and `length` fields aim a four-byte data window at a BAR region, which a
/// configuration access then reads or writes (virtio 1.2 §4.1.4.9).
fn decode_virtio<C: ConfigSpace>(
    this: &Pci<C>,
    base: ConfigAddress,
    offset: u8,
    msg_ctrl: u16,
) -> Capability {
    // The virtio cap header reuses the vendor-specific layout: the
    // upper half of the header dword (`msg_ctrl`) carries `cap_len`
    // in its low byte and `cfg_type` in its high byte (virtio 1.x
    // §4.1.4). Mask + cast of an 8-bit field is lossless.
    let cfg_type = (msg_ctrl >> 8) as u8;
    // `bar` is byte 4 of the capability (dword 1, low byte).
    let bar = (this.config.read32(addr_with_byte_offset(base, offset + 4)) & 0x7) as u8;
    // `offset`/`length` are dwords 2 and 3 of the capability.
    let bar_offset = this.config.read32(addr_with_byte_offset(base, offset + 8));
    let length = this.config.read32(addr_with_byte_offset(base, offset + 12));
    if cfg_type == VIRTIO_PCI_CFG_NOTIFY {
        // The notification structure appends `notify_off_multiplier`
        // as dword 4 of the capability (virtio 1.x §4.1.4.4).
        let notify_off_multiplier = this.config.read32(addr_with_byte_offset(base, offset + 16));
        Capability::VirtioNotify {
            offset,
            bar,
            bar_offset,
            length,
            notify_off_multiplier,
        }
    } else {
        Capability::Virtio {
            offset,
            cfg_type,
            bar,
            bar_offset,
            length,
        }
    }
}
