//! Arch-neutral virtio-MMIO hardware-discovery observers.
//!
//! These walks probe an enumerated virtio-MMIO bus and emit each populated
//! slot into an [`HwNodeSink`] as a discovered [`tairix_abi::HwNode`] —
//! block disks, and the interrupt-driven input/network devices a
//! user-space driver autoloads against. They are **pure discovery**: they
//! reach the bus only through the frozen [`Bus`] / [`VirtioMmioBus`] ABI
//! seams, name no concrete `drivers/bus/*` type, and never read, mount, or
//! bind a driver.
//!
//! They live here, apart from the root-block *catalogue resolution*
//! ([`crate::root_storage`], which links the in-kernel `driver_catalog` /
//! `drvhost`), so that discovering hardware never drags the driver-signing
//! trust anchor in with it: an architecture whose boot path builds a
//! hardware tree (over its own FDT/ACPI source) reuses these observers
//! without linking the catalogue. Input and network devices are
//! discovered by one shared core (`observe_virtio_mmio_interrupt_devices`);
//! the block probe differs (a different resource shape) and stays separate.

use tairix_abi::driver::bus::{Bus, BusDevice};
use tairix_abi::driver::pci::{PciAddress, CLASS_HD_AUDIO, CLASS_OFFSET, CLASS_USB_XHCI};
use tairix_abi::driver::virtio_mmio::VirtioMmioBus;
use tairix_abi::driver::virtio_pci::{
    virtio_pci_window_resource, VirtioPciBus, VIRTIO_PCI_CFG_COMMON, VIRTIO_PCI_CFG_DEVICE,
    VIRTIO_PCI_CFG_ISR, VIRTIO_PCI_CFG_NOTIFY, VIRTIO_PCI_VENDOR_ID,
};
use tairix_abi::driver::MmioMapper;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{
    DmaCoherence, DriverError, HwDeviceClass, HwMatchKey, HwNode, IommuStreams, HW_NODE_ROOT_ID,
};
use tairix_arch_api::{DiscoveryError, HwNodeSink};
use tairix_drv_audio_virtio_snd::VIRTIO_SND_DEVICE_ID;
use tairix_drv_storage_virtio_blk::VIRTIO_BLK_DEVICE_ID;
use tairix_fdt::{Fdt, Node};
use tairix_kernel_virtio::MAX_SLOTS;
use tairix_log::{Event, EventId, Field, FieldValue, Level, Sink};
use tairix_util::fmt::format_hex_u64;
use tairix_virtio_input::VIRTIO_INPUT_DEVICE_ID;
use tairix_virtio_net::VIRTIO_NET_DEVICE_ID;

use crate::hwtree_node_ids::{
    pci_function_node_id, VIRTIO_AUDIO_PROBE_NODE_BASE_ID, VIRTIO_BLOCK_PROBE_NODE_BASE_ID,
    VIRTIO_INPUT_PROBE_NODE_BASE_ID, VIRTIO_NET_PROBE_NODE_BASE_ID,
};
use crate::iommu_fdt::SlotDma;
use crate::pci_host::HostBus;
use crate::pci_probe::FunctionDma;

/// PCI device-ID base of a **modern** virtio function: the device ID is
/// `0x1040 + virtio_device_type` (virtio 1.1 §4.1.2), so a virtio-net
/// function (type [`VIRTIO_NET_DEVICE_ID`] = 1) reports `0x1041`. The PCI
/// probe translates a function's PCI device ID back to the virtio *type*
/// so it emits the *same* [`HwMatchKey::virtio`]`(type)` node the
/// MMIO probe does — one signed driver bundle binds on either bus.
const VIRTIO_PCI_MODERN_DEVICE_ID_BASE: u32 = 0x1040;

/// The base of the registers of the `virtio,mmio` slot `node` describes, as
/// the MMIO bus enumerates it: the first address of its `reg`. [`None`] for
/// a node that is no slot.
#[must_use]
pub fn virtio_mmio_slot_base(node: &Node<'_>) -> Option<u64> {
    if !node.is_compatible(tairix_virtio::transport_mmio::COMPATIBLE) {
        return None;
    }
    node.property("reg")?.read_be_u64(0).ok()
}

/// The operational `virtio,mmio` slot whose registers are at `base`.
#[must_use]
pub fn virtio_mmio_slot<'a>(fdt: &Fdt<'a>, base: u64) -> Option<Node<'a>> {
    for node in fdt.operational_nodes() {
        let node = node.ok()?;
        if virtio_mmio_slot_base(&node) == Some(base) {
            return Some(node);
        }
    }
    None
}

/// The PCI device id a **modern** virtio function of virtio *type*
/// `virtio_type` reports: `0x1040 + virtio_type` (virtio 1.1 §4.1.2). The
/// one definition the PCI probes and the in-kernel bootstrap-floor bring-up
/// (`crate::x86_64::root_unlock`) share, so the `0x1040 + type` encoding is
/// never respelled.
#[must_use]
pub fn virtio_pci_modern_device_id(virtio_type: u32) -> u32 {
    VIRTIO_PCI_MODERN_DEVICE_ID_BASE + virtio_type
}

/// Enumerate the virtio-MMIO `bus` and emit each populated **block** slot
/// into `sink` as a probed child node.
///
/// The raw `virtio,mmio` firmware node the discovery walk emits carries
/// only its `compatible` string, which no floor block driver binds — the
/// virtio-blk bind key is the device id *read from the transport*, not a
/// string. This is the bootstrap-floor bus enumeration that closes that
/// gap: it reads each slot's `DeviceID` register through the MMIO bus
/// driver and, for a virtio-block device ([`VIRTIO_BLK_DEVICE_ID`]),
/// synthesises the probed child node keyed by [`HwMatchKey::virtio`] — the
/// genuine probed identity (never a fabricated key), exactly the node
/// shape the root-storage gate models. The bring-up
/// ([`crate::unlock_service`]) derives the slot's register window and
/// interrupt from the same device tree, so the probed child carries only its
/// bind identity, its slot's position on the bus as its address
/// ([`virtio_mmio_block_slot`]) and, behind a unit, the streams `dma` says
/// its slot masters DMA as; a slot whose DMA the tree cannot describe yields
/// none.
///
/// The probed child is **emitted into the same [`HwNodeSink`] the platform
/// discovery walk writes to**, so it becomes part of the one buffered
/// hardware tree the boot path both resolves the root binding from
/// ([`crate::root_storage::resolve_root_block_driver`]) and stashes for the
/// unlock kthread's `devmgr` autoload — a discovered node, never a side
/// channel.
///
/// Driver-agnostic: it reaches the bus only through the frozen [`Bus`] ABI
/// seam, so the boot path never names a concrete `drivers/bus/*` type. The
/// enumeration is bounded by [`MAX_SLOTS`]; an over-full bus fails closed
/// rather than under-enumerating.
///
/// # Errors
///
/// Propagates the bus enumeration error verbatim — [`DriverError::BufferTooSmall`]
/// when more than [`MAX_SLOTS`] slots respond, or a malformed-tree
/// [`DriverError::DeviceFault`]. A [`DiscoveryError::SinkFull`] from a full
/// sink is also surfaced as [`DriverError::BufferTooSmall`]. The caller
/// leaves the root unbound on any error (fail closed).
pub fn observe_virtio_mmio_block_devices(
    bus: &dyn Bus,
    dma: &SlotDma,
    sink: &mut dyn HwNodeSink,
) -> Result<(), DriverError> {
    let blank = BusDevice {
        vendor: 0,
        device: 0,
        class: 0,
        reserved0: 0,
        address: 0,
    };
    let mut table = [blank; MAX_SLOTS];
    let count = bus.enumerate(&mut table)?;
    let mut next_id = VIRTIO_BLOCK_PROBE_NODE_BASE_ID;
    for (slot, device) in table.iter().take(count).enumerate() {
        if device.device != VIRTIO_BLK_DEVICE_ID {
            continue;
        }
        let (Ok(streams), Some(address)) =
            (dma.streams(device.address), u32::try_from(slot + 1).ok())
        else {
            continue;
        };
        emit_virtio_block_node(sink, next_id, |node| {
            node.set_address(address);
            push_streams(node, streams)
        })?;
        next_id = next_id.wrapping_add(1);
    }
    Ok(())
}

/// The position in its bus's enumeration of the slot the virtio-MMIO block
/// `node` was discovered at, which it records from 1 as its address.
#[must_use]
pub fn virtio_mmio_block_slot(node: &HwNode) -> Option<usize> {
    usize::try_from(node.address().checked_sub(1)?).ok()
}

/// Put every stream of `streams` on `node`; `false` when it cannot hold
/// them all, as a translated device is never published with part of its
/// identity.
fn push_streams(node: &mut HwNode, mut streams: impl Iterator<Item = IommuStreams>) -> bool {
    streams.all(|range| node.push_resource(HwResource::iommu_stream(range)).is_ok())
}

/// How a translation unit knows the DMA of the PCI function at a
/// configuration address, or [`None`] where no unit translates it.
pub type DmaIdentity<'a> = &'a dyn Fn(u64) -> Option<FunctionDma>;

/// The PCI segment an observer walks: its number, and its position among the
/// segments the kernel owns, which numbers its functions' nodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PciSegment {
    /// The segment's number.
    pub number: u16,
    /// Its position among the segments the kernel owns.
    pub ordinal: u32,
}

impl PciSegment {
    /// The node of the function at configuration `address`, or [`None`] past
    /// the id space.
    #[must_use]
    pub fn node_id(self, address: u64) -> Option<u32> {
        pci_function_node_id(self.ordinal, tairix_abi::driver::pci::requester_id(address))
    }
}

/// One walk of a segment, as every class observer sees it.
#[derive(Copy, Clone)]
pub struct PciWalk<'a> {
    /// The segment walked.
    pub segment: PciSegment,
    /// Its functions, from one enumeration of [`Self::bus`].
    pub functions: &'a [BusDevice],
    /// Its configuration space.
    pub bus: &'a dyn VirtioPciBus,
    /// The kernel's own reach of the registers its functions decode.
    pub registers: &'a dyn MmioMapper,
    /// How a unit knows each function's DMA.
    pub dma: DmaIdentity<'a>,
    /// How its functions' DMA meets the CPU's caches: their host's.
    pub coherence: DmaCoherence,
}

/// One discovered PCI function: where the machine names it and how a unit
/// knows its DMA, where one does.
struct PciFunction {
    address: PciAddress,
    dma: Option<FunctionDma>,
}

impl PciFunction {
    /// The function `device`, or [`None`] for one behind a unit that it
    /// cannot be confined by: one whose isolation group spans units, one below
    /// an external-facing port that does not validate requester ids, or a
    /// virtio function declining `VIRTIO_F_ACCESS_PLATFORM`, which reaches
    /// memory by physical address past the unit. No driver may be handed it.
    /// Decided before anything routes an interrupt to it or makes it a bus
    /// master; the refusal is audited.
    fn admit(walk: &PciWalk<'_>, device: &BusDevice, log: &dyn Sink) -> Option<Self> {
        let address = PciAddress::at(walk.segment.number, device.address);
        let Some(translated) = (walk.dma)(device.address) else {
            return Some(Self { address, dma: None });
        };
        if translated.group.is_none() {
            let reason = if translated.untrusted {
                "untrusted"
            } else {
                "unconfinable"
            };
            log_bypass(log, address, &translated, reason);
            return None;
        }
        // Only virtio defines a way for a device to decline the platform's
        // translation; every other function's requests reach the unit.
        let honoured = device.vendor != u32::from(VIRTIO_PCI_VENDOR_ID)
            || walk
                .bus
                .offered_features(device.address, walk.registers)
                .is_ok_and(|offered| offered & tairix_virtio::VIRTIO_F_ACCESS_PLATFORM != 0);
        if honoured {
            return Some(Self {
                address,
                dma: Some(translated),
            });
        }
        log_bypass(log, address, &translated, "bypasses_unit");
        None
    }

    /// Record the function's segment and requester id on `node`, and how its
    /// unit knows its DMA. `false` when the node cannot hold all of it: a
    /// translated device is never published as an untranslated one, nor with
    /// part of its identity.
    fn describe(self, node: &mut HwNode) -> bool {
        node.set_address(self.address.node_address());
        let Some(dma) = self.dma else {
            return true;
        };
        let Some(group) = dma.group else {
            return false;
        };
        node.push_resource(HwResource::iommu_stream(dma.stream))
            .is_ok()
            && dma
                .aliases
                .as_slice()
                .iter()
                .all(|&alias| node.push_resource(HwResource::iommu_alias(alias)).is_ok())
            && node
                .push_resource(HwResource::iommu_group_member(group))
                .is_ok()
    }
}

fn log_bypass(log: &dyn Sink, address: PciAddress, dma: &FunctionDma, reason: &'static str) {
    let event = tairix_kernel_core::AuditEvent::DmaTranslationBypass;
    tairix_log::log(
        log,
        &Event {
            level: Level::Warn,
            id: event.id(),
            message: event.message(),
            fields: &[
                Field {
                    key: "address",
                    value: FieldValue::UnsignedInt(u64::from(address.node_address())),
                },
                Field {
                    key: "unit",
                    value: FieldValue::UnsignedInt(u64::from(dma.stream.unit())),
                },
                Field {
                    key: "reason",
                    value: FieldValue::Str(reason),
                },
            ],
        },
    );
}

/// Emit one match-key-only virtio-block [`HwDeviceClass::Storage`] node
/// (id `node_id`, parented to the tree root) into `sink`, with what `dma`
/// records of how a unit knows its DMA; one `dma` cannot record wholly is
/// not emitted.
///
/// The bootstrap-floor block bring-up re-derives the device's register
/// window and interrupt line from the platform source at bind time (the
/// firmware device tree on a virtio-MMIO port, PCI configuration space on
/// the virtio-PCI port), so a discovered block node carries only its bind
/// identity — no register-window or DMA grant, unlike a
/// user-space-autoloaded interrupt device whose driver needs those grants.
/// Shared by the MMIO ([`observe_virtio_mmio_block_devices`]) and PCI
/// ([`observe_virtio_pci_block_devices`]) block probes so the discovered
/// block-node shape has exactly one definition.
///
/// The node parents to the tree root id ([`HW_NODE_ROOT_ID`]), not the
/// `HW_NODE_ROOT` *parent sentinel*: a node whose parent is the sentinel is
/// the root itself and is skipped by the autoload walk
/// ([`HwNode::is_root`]), so a top-level discovered device must name the
/// root's id as its parent. One bind key always fits a fresh node; a node
/// that somehow could not hold it is dropped rather than bound on a partial
/// identity. A full sink ([`DiscoveryError::SinkFull`]) is surfaced as the
/// same bounded-capacity [`DriverError::BufferTooSmall`] an over-full bus
/// raises (fail closed).
fn emit_virtio_block_node(
    sink: &mut dyn HwNodeSink,
    node_id: u32,
    dma: impl FnOnce(&mut HwNode) -> bool,
) -> Result<(), DriverError> {
    let mut node = HwNode::new(node_id, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    if node
        .push_match_key(HwMatchKey::virtio(VIRTIO_BLK_DEVICE_ID))
        .is_ok()
        && dma(&mut node)
    {
        sink.emit(node)
            .map_err(|_: DiscoveryError| DriverError::BufferTooSmall)?;
    }
    Ok(())
}

/// Emit each modern virtio-blk function `walk` found as a match-key-only
/// [`HwDeviceClass::Storage`] node: the in-kernel floor bring-up re-resolves
/// its transport from configuration space, so the node carries its bind key,
/// its segment and requester id as its address and, behind a translation
/// unit, how the unit knows its DMA — no grant. Keyed by the virtio type, so
/// one bundle binds on either bus.
///
/// # Errors
///
/// [`DriverError::BufferTooSmall`] for a full sink.
pub fn observe_virtio_pci_block_devices(
    walk: &PciWalk<'_>,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    let want_device_id = virtio_pci_modern_device_id(VIRTIO_BLK_DEVICE_ID);
    for device in walk.functions {
        if device.vendor != u32::from(VIRTIO_PCI_VENDOR_ID) || device.device != want_device_id {
            continue;
        }
        let Some(id) = walk.segment.node_id(device.address) else {
            continue;
        };
        let Some(function) = PciFunction::admit(walk, device, log) else {
            continue;
        };
        emit_virtio_block_node(sink, id, |node| function.describe(node))?;
    }
    Ok(())
}

/// Enumerate the virtio-MMIO `bus` and emit each populated **virtio-input**
/// slot into `sink` as a discovered, user-space-autoloadable device node
/// carrying its register window **and** DMA constraint as capability-grant
/// requests.
///
/// This is the input-device analogue of [`observe_virtio_mmio_block_devices`],
/// and the discovery step the user-space input-driver autoload depends on:
/// a virtio keyboard/pointer is driven entirely from user space, so unlike
/// the in-kernel bootstrap-floor block path (whose bring-up re-derives the
/// slot window from the device tree by base) the input node **must** carry
/// both its MMIO window and a DMA constraint as [`HwResource`]s — a
/// user-space virtio driver maps its registers and drives its split
/// virtqueues out of driver-allocated DMA memory, so a node that requested
/// no DMA would be discovered yet fail its queue bring-up closed. The
/// privileged driver-spawn path mints exactly one device-resource grant per
/// resource the matched node requested ([`crate::driver_spawn_loader`]), so
/// the autoloaded driver is handed a window grant of precisely the slot it
/// owns plus a DMA grant for its virtqueues — and nothing more (no ambient
/// authority).
///
/// Each populated slot whose `DeviceID` register equals
/// [`VIRTIO_INPUT_DEVICE_ID`] (the genuine probed identity read from the
/// transport, never a fabricated key) is emitted as an
/// [`HwDeviceClass::Input`] node keyed by [`HwMatchKey::virtio`], carrying
/// [`HwResource::mmio`] over the slot's discovered base and the extent
/// [`VirtioMmioBus::slot_window`] reports from the device tree (a discovered
/// value, never a literal) plus a coherent [`HwResource::dma`] (the QEMU
/// `virt` virtio interconnect is cache-coherent with no IOMMU, so the device
/// addresses all of RAM — no address limit, never a board constant). The
/// node is parented to the tree root id ([`HW_NODE_ROOT_ID`]), not the
/// `HW_NODE_ROOT` parent sentinel, so the autoload walk treats it as a
/// device rather than skipping it as the root ([`HwNode::is_root`]). It is
/// emitted into the same buffered hardware tree the discovery walk and the
/// block probe write to, so the unlock kthread's `devmgr` autoload sees one
/// faithful tree.
///
/// Driver-agnostic: it reaches the bus only through the frozen
/// [`VirtioMmioBus`] / [`Bus`] ABI seams, so the boot path never names a
/// concrete `drivers/bus/*` type. The Raspberry Pi 4 firmware tree describes
/// no `virtio,mmio` node, so this is a no-op there — it is the QEMU
/// `virt`-board path, additive and metal-neutral.
///
/// A slot whose window extent cannot be resolved (a malformed `reg`), or a
/// fresh node that cannot hold its match key and both resources, is
/// **skipped** rather than emitted on a partial identity — a node the
/// kernel cannot mint a correct, bounded grant for is left undiscovered and
/// thus unbound, never half-described (fail closed).
///
/// # Errors
///
/// Propagates the bus enumeration error verbatim — [`DriverError::BufferTooSmall`]
/// when more than [`MAX_SLOTS`] slots respond, or a malformed-tree
/// [`DriverError::DeviceFault`]. A [`DiscoveryError::SinkFull`] from a full
/// sink is surfaced as [`DriverError::BufferTooSmall`]. The caller leaves
/// the affected node undiscovered on any error (fail closed).
pub fn observe_virtio_mmio_input_devices(
    bus: &dyn VirtioMmioBus,
    slot_irq: &dyn Fn(u64) -> Option<u32>,
    dma: &SlotDma,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    observe_virtio_mmio_interrupt_devices(
        bus,
        slot_irq,
        dma,
        sink,
        log,
        Probe {
            device_id: VIRTIO_INPUT_DEVICE_ID,
            class: HwDeviceClass::Input,
            node_base_id: VIRTIO_INPUT_PROBE_NODE_BASE_ID,
        },
    )
}

/// Discover every populated `virtio,mmio` slot whose `DeviceID` register
/// equals [`VIRTIO_NET_DEVICE_ID`] and emit each as a
/// [`HwDeviceClass::Network`] node keyed by [`HwMatchKey::virtio`],
/// carrying the same register-window + coherent-DMA + interrupt-line
/// grant requests as the input probe — the four things the autoloaded
/// user-space virtio-net driver process needs (`plans/NETWORK.md` N4e).
///
/// The virtio-net driver is interrupt-driven exactly like the input
/// driver — it parks its serve loop on the device interrupt rather than
/// busy-polling — so its discovery is the *same* walk with only the
/// probed device id and the emitted node class differing; both go through
/// the shared `observe_virtio_mmio_interrupt_devices` core.
/// Node ids are drawn from a base disjoint from the block- and
/// input-probe bases so the tree's node origins stay unambiguous.
///
/// # Errors
///
/// As [`observe_virtio_mmio_input_devices`]: propagates the bus
/// enumeration error and surfaces a full sink as
/// [`DriverError::BufferTooSmall`] (fail closed).
pub fn observe_virtio_mmio_network_devices(
    bus: &dyn VirtioMmioBus,
    slot_irq: &dyn Fn(u64) -> Option<u32>,
    dma: &SlotDma,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    observe_virtio_mmio_interrupt_devices(
        bus,
        slot_irq,
        dma,
        sink,
        log,
        Probe {
            device_id: VIRTIO_NET_DEVICE_ID,
            class: HwDeviceClass::Network,
            node_base_id: VIRTIO_NET_PROBE_NODE_BASE_ID,
        },
    )
}

/// Discover every populated `virtio,mmio` slot whose `DeviceID` register
/// equals [`VIRTIO_SND_DEVICE_ID`] and emit each as a
/// [`HwDeviceClass::Audio`] node keyed by [`HwMatchKey::virtio`], carrying
/// the same register-window + coherent-DMA + interrupt-line grant requests
/// as the input and network probes — the three things the autoloaded
/// user-space virtio sound driver process needs (`plans/SOUND.md` SND4).
///
/// A sound card is interrupt-driven exactly like a NIC: the driver parks its
/// serve loop on the device's period interrupt rather than polling, so this
/// is the *same* walk with only the probed device id and the emitted node
/// class differing.
///
/// # Errors
///
/// As [`observe_virtio_mmio_input_devices`]: propagates the bus enumeration
/// error and surfaces a full sink as [`DriverError::BufferTooSmall`] (fail
/// closed).
pub fn observe_virtio_mmio_audio_devices(
    bus: &dyn VirtioMmioBus,
    slot_irq: &dyn Fn(u64) -> Option<u32>,
    dma: &SlotDma,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    observe_virtio_mmio_interrupt_devices(
        bus,
        slot_irq,
        dma,
        sink,
        log,
        Probe {
            device_id: VIRTIO_SND_DEVICE_ID,
            class: HwDeviceClass::Audio,
            node_base_id: VIRTIO_AUDIO_PROBE_NODE_BASE_ID,
        },
    )
}

/// What one interrupt-driven probe looks for, and how it numbers what it
/// finds.
struct Probe {
    device_id: u32,
    class: HwDeviceClass,
    node_base_id: u32,
}

/// The shared core of the interrupt-driven virtio-MMIO class probes
/// ([`observe_virtio_mmio_input_devices`],
/// [`observe_virtio_mmio_network_devices`]): enumerate the bus, and for every
/// populated slot whose `DeviceID` equals the probe's emit a node of its
/// class (numbered from its base) carrying its register window, a coherent
/// DMA constraint, its discovered interrupt line and, behind a unit, the
/// streams `dma` says the slot masters DMA as. Input and network devices are
/// identical here — both are autoloaded into a user-space process that parks on
/// the device interrupt — so the walk is written once; the block probe differs
/// (a different resource shape) and stays separate.
fn observe_virtio_mmio_interrupt_devices(
    bus: &dyn VirtioMmioBus,
    slot_irq: &dyn Fn(u64) -> Option<u32>,
    dma: &SlotDma,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
    Probe {
        device_id,
        class,
        node_base_id,
    }: Probe,
) -> Result<(), DriverError> {
    let blank = BusDevice {
        vendor: 0,
        device: 0,
        class: 0,
        reserved0: 0,
        address: 0,
    };
    let mut table = [blank; MAX_SLOTS];
    let count = bus.enumerate(&mut table)?;
    let mut next_id = node_base_id;
    for device in &table[..count] {
        // Diagnostic audit: every populated virtio-MMIO slot the walk sees,
        // with its probed `DeviceID` and register base — the discovery
        // counterpart of the block probe's bind audit, so a mis-probed or
        // unexpected device is visible in the boot log rather than silent.
        let mut want_buf = [0u8; 16];
        let mut got_buf = [0u8; 16];
        let mut addr_buf = [0u8; 16];
        log.write_event(&Event {
            level: Level::Debug,
            id: EventId(4137),
            message: "virtio-mmio slot probed",
            fields: &[
                Field {
                    key: "want",
                    value: tairix_log::FieldValue::Str(format_hex_u64(
                        u64::from(device_id),
                        &mut want_buf,
                    )),
                },
                Field {
                    key: "got",
                    value: tairix_log::FieldValue::Str(format_hex_u64(
                        u64::from(device.device),
                        &mut got_buf,
                    )),
                },
                Field {
                    key: "base",
                    value: tairix_log::FieldValue::Str(format_hex_u64(
                        device.address,
                        &mut addr_buf,
                    )),
                },
            ],
        });
        if device.device != device_id {
            continue;
        }
        // The window extent the device tree declares for this slot. A
        // malformed `reg` (or a base the bus cannot resolve) means the
        // kernel cannot size a correct grant, so skip the node rather than
        // grant a guessed window (fail closed).
        let Ok(len) = bus.slot_window(device.address) else {
            continue;
        };
        // The interrupt line the platform routes this slot to, resolved by
        // the arch-supplied `slot_irq` (the aarch64 port decodes the FDT
        // `interrupts` specifier through `gic_device_intid`; the line is a
        // *discovered* value, never a board constant). A user-space
        // virtio-input driver is interrupt-driven: it parks on `irq_wait`
        // rather than busy-polling its event queue, so a slot whose IRQ
        // cannot be resolved is left undiscovered rather than emitted
        // without the line its driver needs (fail closed).
        let Some(intid) = slot_irq(device.address) else {
            continue;
        };
        let (Ok(streams), Some(coherence)) =
            (dma.streams(device.address), dma.coherence(device.address))
        else {
            continue;
        };
        // A top-level discovered device parents to the tree root
        // ([`HW_NODE_ROOT_ID`]), never the `HW_NODE_ROOT` parent sentinel
        // (which marks the root itself and is skipped by the autoload
        // walk, `HwNode::is_root`).
        let mut node = HwNode::new(next_id, HW_NODE_ROOT_ID, class);
        next_id = next_id.wrapping_add(1);
        // The driver is granted what the node requests: its registers, DMA
        // for its virtqueues, unbounded and as coherent as its slot, and its
        // line. A node that cannot hold all of it is dropped rather than
        // emitted on a partial identity.
        if node.push_match_key(HwMatchKey::virtio(device_id)).is_ok()
            && node
                .push_resource(HwResource::mmio(device.address, len))
                .is_ok()
            && node.push_resource(HwResource::dma(0, 0, coherence)).is_ok()
            && node
                .push_resource(HwResource::irq(u64::from(intid), 1))
                .is_ok()
            && push_streams(&mut node, streams)
        {
            // A full sink (`DiscoveryError::SinkFull`) is the only emit
            // failure a buffering sink raises; surface it as the same
            // bounded-capacity refusal an over-full bus does (fail closed).
            sink.emit(node)
                .map_err(|_: DiscoveryError| DriverError::BufferTooSmall)?;
        }
    }
    Ok(())
}

/// The interrupt a discovered function is granted: its line, and where it
/// raises that line by a message a translation unit translates, the doorbell
/// its messages are written to, which its domain must map.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceInterrupt {
    /// The line its driver binds.
    pub line: HwResource,
    /// A [`tairix_abi::HwResourceKind::MsiDoorbell`] its node carries.
    pub doorbell: Option<HwResource>,
}

impl DeviceInterrupt {
    /// `line`, raised on a wire or by a message no unit translates.
    #[must_use]
    pub const fn line(line: HwResource) -> Self {
        Self {
            line,
            doorbell: None,
        }
    }
}

/// Emit each modern virtio-net function `walk` found as a
/// [`HwDeviceClass::Network`] node carrying its configuration
/// windows, its routed interrupt line and, behind a unit, its DMA identity;
/// one any of them cannot be resolved for is left undiscovered.
///
/// # Errors
///
/// [`DriverError::BufferTooSmall`] for a full sink.
pub fn observe_virtio_pci_network_devices(
    walk: &PciWalk<'_>,
    dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    let kind = VirtioPciKind {
        virtio_type: VIRTIO_NET_DEVICE_ID,
        class: HwDeviceClass::Network,
    };
    observe_virtio_pci_devices(walk, dev_irq, sink, log, kind)
}

/// Emit each modern virtio-input function `walk` found as a
/// [`HwDeviceClass::Input`] node carrying its configuration
/// windows, its routed interrupt line and, behind a unit, its DMA identity;
/// one any of them cannot be resolved for is left undiscovered.
///
/// # Errors
///
/// [`DriverError::BufferTooSmall`] for a full sink.
pub fn observe_virtio_pci_input_devices(
    walk: &PciWalk<'_>,
    dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    let kind = VirtioPciKind {
        virtio_type: VIRTIO_INPUT_DEVICE_ID,
        class: HwDeviceClass::Input,
    };
    observe_virtio_pci_devices(walk, dev_irq, sink, log, kind)
}

/// Emit each modern virtio-sound function `walk` found as a
/// [`HwDeviceClass::Audio`] node carrying its configuration
/// windows, its routed interrupt line and, behind a unit, its DMA identity;
/// one any of them cannot be resolved for is left undiscovered
/// (`plans/SOUND.md` SND4).
///
/// # Errors
///
/// [`DriverError::BufferTooSmall`] for a full sink.
pub fn observe_virtio_pci_audio_devices(
    walk: &PciWalk<'_>,
    dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    let kind = VirtioPciKind {
        virtio_type: VIRTIO_SND_DEVICE_ID,
        class: HwDeviceClass::Audio,
    };
    observe_virtio_pci_devices(walk, dev_irq, sink, log, kind)
}

/// A class of PCI function a driver binds by its class code: the code, the
/// BAR its registers decode at, and the class of node it is published as.
#[derive(Copy, Clone, Debug)]
pub struct PciClass {
    code: u32,
    bar: u8,
    node: HwDeviceClass,
}

/// xHCI USB host controllers, their registers at BAR 0 (xHCI §5.2.1).
pub const XHCI_CONTROLLERS: PciClass = PciClass {
    code: CLASS_USB_XHCI,
    bar: 0,
    node: HwDeviceClass::Bus,
};

/// HD Audio controllers, their registers at BAR 0 (HDA 1.0a §3.3).
pub const HD_AUDIO_CONTROLLERS: PciClass = PciClass {
    code: CLASS_HD_AUDIO,
    bar: 0,
    node: HwDeviceClass::Audio,
};

/// Emit each function of `class` that `walk` found as a node keyed by its PCI
/// identity and class code, carrying the part of its register BAR a driver
/// may hold, an unconstrained DMA reach, the interrupt `dev_irq` routed for it
/// and, behind a unit, its DMA identity.
///
/// The register window stops short of the function's MSI-X table and
/// pending-bit array, which only this kernel programs: a driver able to
/// rewrite them could aim the function's messages at any address. A function
/// whose window, line or node cannot be resolved is left undiscovered.
///
/// # Errors
///
/// [`DriverError::BufferTooSmall`] for a full sink.
pub fn observe_pci_class_functions(
    walk: &PciWalk<'_>,
    bus: &dyn HostBus,
    class: PciClass,
    dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
) -> Result<(), DriverError> {
    for device in walk.functions {
        let bdf = device.address;
        let Ok(code) = bus.read_config(bdf, CLASS_OFFSET).map(|dword| dword >> 8) else {
            continue;
        };
        if code != class.code {
            continue;
        }
        let Ok((base, len)) = bus.driver_window(bdf, class.bar) else {
            continue;
        };
        let (Some(id), Ok(vendor), Ok(product)) = (
            walk.segment.node_id(bdf),
            u16::try_from(device.vendor),
            u16::try_from(device.device),
        ) else {
            continue;
        };
        let Some(function) = PciFunction::admit(walk, device, log) else {
            continue;
        };
        let Some(interrupt) = dev_irq(bdf)
            .filter(|interrupt| interrupt.line.kind() == Some(tairix_abi::HwResourceKind::Irq))
        else {
            continue;
        };
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, class.node);
        if node
            .push_match_key(HwMatchKey::pci(vendor, product, code))
            .is_ok()
            && [
                HwResource::mmio(base, len),
                HwResource::dma(0, 0, walk.coherence),
                interrupt.line,
            ]
            .into_iter()
            .chain(interrupt.doorbell)
            .all(|resource| node.push_resource(resource).is_ok())
            && function.describe(&mut node)
        {
            sink.emit(node)
                .map_err(|_: DiscoveryError| DriverError::BufferTooSmall)?;
        }
    }
    Ok(())
}

/// Which virtio functions a PCI class probe emits, and as which class of
/// node.
#[derive(Clone, Copy)]
struct VirtioPciKind {
    virtio_type: u32,
    class: HwDeviceClass,
}

/// The shared core of the interrupt-driven virtio-PCI class probes: each
/// modern virtio function of `kind` `walk` found becomes a node keyed by
/// its virtio type, carrying its four configuration windows (the notify
/// window with its multiplier), an unconstrained DMA reach, the interrupt
/// grant `dev_irq` routed for it and, behind a translation unit, how the unit
/// knows its DMA. A user-space driver cannot reach configuration space, so the
/// kernel resolves the windows; a function whose windows or line cannot be
/// resolved, or whose node cannot hold all of it, is left undiscovered.
fn observe_virtio_pci_devices(
    walk: &PciWalk<'_>,
    dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    sink: &mut dyn HwNodeSink,
    log: &dyn Sink,
    kind: VirtioPciKind,
) -> Result<(), DriverError> {
    let VirtioPciKind { virtio_type, class } = kind;
    let bus = walk.bus;
    let want_device_id = virtio_pci_modern_device_id(virtio_type);
    for device in walk.functions {
        if device.vendor != u32::from(VIRTIO_PCI_VENDOR_ID) {
            continue;
        }
        log_virtio_pci_function(log, want_device_id, device);
        if device.device != want_device_id {
            continue;
        }
        let bdf = device.address;
        let (Ok(common), Ok(notify), Ok(isr), Ok(devcfg), Ok(multiplier)) = (
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_COMMON),
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_NOTIFY),
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_ISR),
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_DEVICE),
            bus.notify_off_multiplier(bdf),
        ) else {
            continue;
        };
        let Some(id) = walk.segment.node_id(bdf) else {
            continue;
        };
        let Some(function) = PciFunction::admit(walk, device, log) else {
            continue;
        };
        let Some(interrupt) = dev_irq(bdf)
            .filter(|interrupt| interrupt.line.kind() == Some(tairix_abi::HwResourceKind::Irq))
        else {
            continue;
        };
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, class);
        let window = |role, (base, len): (u64, usize), multiplier| {
            virtio_pci_window_resource(role, base, len as u64, multiplier)
        };
        if node.push_match_key(HwMatchKey::virtio(virtio_type)).is_ok()
            && [
                window(VIRTIO_PCI_CFG_COMMON, common, 0),
                window(VIRTIO_PCI_CFG_NOTIFY, notify, multiplier),
                window(VIRTIO_PCI_CFG_ISR, isr, 0),
                window(VIRTIO_PCI_CFG_DEVICE, devcfg, 0),
                HwResource::dma(0, 0, walk.coherence),
                interrupt.line,
            ]
            .into_iter()
            .chain(interrupt.doorbell)
            .all(|resource| node.push_resource(resource).is_ok())
            && function.describe(&mut node)
        {
            sink.emit(node)
                .map_err(|_: DiscoveryError| DriverError::BufferTooSmall)?;
        }
    }
    Ok(())
}

/// Give each translation unit among `nodes` that is a virtio-iommu function
/// on `segment` its four configuration windows — its registers, which no
/// process may map — and the line `dev_irq` resolves for it, where it raises
/// its faults on a wire. The function the node's address names must answer
/// as a modern virtio-iommu: a node no such function backs is given nothing,
/// and the kernel takes it over as no unit.
pub fn describe_virtio_units(
    segment: u16,
    bus: &dyn HostBus,
    dev_irq: &dyn Fn(u64) -> Option<HwResource>,
    nodes: &mut [HwNode],
) {
    let Ok(key) = HwMatchKey::compatible(tairix_kernel_iommu_virtio::COMPATIBLE) else {
        return;
    };
    let identity = u32::from(VIRTIO_PCI_VENDOR_ID)
        | virtio_pci_modern_device_id(tairix_kernel_iommu_virtio::DEVICE_ID) << 16;
    for node in nodes.iter_mut().filter(|node| {
        node.class() == Some(HwDeviceClass::Iommu) && node.match_keys().contains(&key)
    }) {
        let function = PciAddress::from_node_address(node.address());
        let bdf = function.config_address();
        if function.segment() != segment || bus.read_config(bdf, 0) != Ok(identity) {
            continue;
        }
        let (Ok(common), Ok(notify), Ok(isr), Ok(devcfg), Ok(multiplier)) = (
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_COMMON),
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_NOTIFY),
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_ISR),
            bus.virtio_window_region(bdf, VIRTIO_PCI_CFG_DEVICE),
            bus.notify_off_multiplier(bdf),
        ) else {
            continue;
        };
        let window = |role, (base, len): (u64, usize), multiplier| {
            virtio_pci_window_resource(role, base, len as u64, multiplier)
        };
        describe_whole(
            node,
            [
                window(VIRTIO_PCI_CFG_COMMON, common, 0),
                window(VIRTIO_PCI_CFG_NOTIFY, notify, multiplier),
                window(VIRTIO_PCI_CFG_ISR, isr, 0),
                window(VIRTIO_PCI_CFG_DEVICE, devcfg, 0),
            ]
            .into_iter()
            .chain(dev_irq(bdf)),
        );
    }
}

/// Give `node` every one of `resources`, or none where it cannot hold them
/// all: a unit missing a window, or the line it raises its faults on, is no
/// unit to take over.
fn describe_whole(node: &mut HwNode, resources: impl IntoIterator<Item = HwResource>) {
    let mut described = *node;
    if resources
        .into_iter()
        .all(|resource| described.push_resource(resource).is_ok())
    {
        *node = described;
    }
}

/// Emit the per-function virtio-PCI discovery diagnostic: the wanted PCI
/// device ID, the one the function reports, and its bus address — the PCI
/// counterpart of the MMIO probe's slot audit, so a mis-probed or
/// unexpected function is visible in the boot log rather than silent.
fn log_virtio_pci_function(log: &dyn Sink, want_device_id: u32, device: &BusDevice) {
    let mut want_buf = [0u8; 16];
    let mut got_buf = [0u8; 16];
    let mut addr_buf = [0u8; 16];
    log.write_event(&Event {
        level: Level::Debug,
        id: EventId(4138),
        message: "virtio-pci function probed",
        fields: &[
            Field {
                key: "want",
                value: tairix_log::FieldValue::Str(format_hex_u64(
                    u64::from(want_device_id),
                    &mut want_buf,
                )),
            },
            Field {
                key: "got",
                value: tairix_log::FieldValue::Str(format_hex_u64(
                    u64::from(device.device),
                    &mut got_buf,
                )),
            },
            Field {
                key: "bdf",
                value: tairix_log::FieldValue::Str(format_hex_u64(device.address, &mut addr_buf)),
            },
        ],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::driver::virtio_pci::common;
    use tairix_abi::{HwDeviceClass, HwMatchKey, HwNode, HwResource};

    use crate::discovery_test_bus::FakeBus;
    use crate::test_support::NullSink;

    /// A deterministic interrupt line the interrupt-probe tests hand the
    /// `slot_irq` closure for every slot, so the emitted node carries a
    /// predictable [`HwResource::irq`] the assertions check. An arbitrary
    /// in-range GICv2 SPI; the value is the test's own and never a board
    /// constant the production path uses.
    const TEST_INPUT_INTID: u32 = 34;

    /// Records the id of every event it is handed.
    #[derive(Default)]
    struct IdLog(core::cell::RefCell<alloc::vec::Vec<EventId>>);

    impl Sink for IdLog {
        fn write_event(&self, event: &Event<'_>) {
            self.0.borrow_mut().push(event.id);
        }
    }

    /// Collects every node a discovery probe emits, so the tests can assert
    /// the emitted node's class, bind key, and resource directly. Unbounded,
    /// so emit never fails.
    #[derive(Default)]
    struct CollectingSink {
        nodes: alloc::vec::Vec<HwNode>,
    }

    impl HwNodeSink for CollectingSink {
        fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
            self.nodes.push(node);
            Ok(())
        }
    }

    /// A unit is described whole or not at all.
    #[test]
    fn a_unit_without_room_for_its_whole_description_is_given_none_of_it() {
        let irq = HwResource::irq(u64::from(TEST_INPUT_INTID), 1);
        let mut room = HwNode::new(7, 0, HwDeviceClass::Iommu);
        describe_whole(&mut room, [irq, irq]);
        assert_eq!(room.resources(), &[irq, irq]);
        let mut full = HwNode::new(8, 0, HwDeviceClass::Iommu);
        for _ in 1..tairix_abi::HW_NODE_MAX_RESOURCES {
            full.push_resource(irq).unwrap();
        }
        let before = full;
        describe_whole(&mut full, [irq, irq]);
        assert_eq!(full, before, "nothing of the description was kept");
    }

    #[test]
    fn a_probed_virtio_input_slot_is_discovered_with_its_mmio_window() {
        // A populated virtio-input slot (DeviceID 18) is emitted as a
        // user-space-autoloadable `Input` node keyed by its probed virtio
        // device id and carrying its register window as a grant request —
        // the discovery the input-driver autoload binds against.
        let bus = FakeBus::with(&[VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Input));
        assert_eq!(node.id(), VIRTIO_INPUT_PROBE_NODE_BASE_ID);
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::virtio(VIRTIO_INPUT_DEVICE_ID)]
        );
        // The grant requests are exactly the slot's discovered register
        // window, a coherent DMA constraint, and the discovered interrupt
        // line — the window of precisely the region it owns, the DMA region
        // its virtqueues need, and the IRQ it parks on.
        assert_eq!(
            node.resources(),
            &[
                HwResource::mmio(0x0A00_0000, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1)
            ]
        );
    }

    #[test]
    fn a_slot_s_device_masters_dma_as_coherently_as_its_slot() {
        let bus = FakeBus::with(&[VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::every(DmaCoherence::Unsnooped),
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        let dma: alloc::vec::Vec<_> = sink.nodes[0]
            .resources()
            .iter()
            .filter_map(HwResource::dma_coherence)
            .collect();
        assert_eq!(dma, [DmaCoherence::Unsnooped]);
    }

    #[test]
    fn a_non_input_virtio_slot_emits_no_input_node() {
        // A virtio-blk slot (2) and a virtio-net slot (1) are not input
        // devices, so the input probe emits nothing.
        let bus = FakeBus::with(&[VIRTIO_BLK_DEVICE_ID, 1]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    #[test]
    fn an_input_slot_beside_a_block_slot_emits_only_the_input_node() {
        // On a mixed bus the input probe emits exactly the input device and
        // ignores the block disk; its node id comes from the disjoint input
        // base, so it can never collide with a block probe child.
        let bus = FakeBus::with(&[VIRTIO_BLK_DEVICE_ID, VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Input));
        assert_eq!(node.id(), VIRTIO_INPUT_PROBE_NODE_BASE_ID);
        // The input device sits in slot 1 (base = 0x0A00_0000 + 0x200),
        // and carries its coherent DMA grant and IRQ alongside the window.
        assert_eq!(
            node.resources(),
            &[
                HwResource::mmio(0x0A00_0200, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1)
            ]
        );
    }

    #[test]
    fn two_input_slots_get_distinct_ids_and_windows() {
        // Two virtio-input devices each become a distinct node with its own
        // discovered window, so a keyboard and a pointer are never merged.
        let bus = FakeBus::with(&[VIRTIO_INPUT_DEVICE_ID, VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 2);
        assert_eq!(sink.nodes[0].id(), VIRTIO_INPUT_PROBE_NODE_BASE_ID);
        assert_eq!(sink.nodes[1].id(), VIRTIO_INPUT_PROBE_NODE_BASE_ID + 1);
        assert_eq!(
            sink.nodes[0].resources(),
            &[
                HwResource::mmio(0x0A00_0000, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1)
            ]
        );
        assert_eq!(
            sink.nodes[1].resources(),
            &[
                HwResource::mmio(0x0A00_0200, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1)
            ]
        );
    }

    #[test]
    fn probed_device_nodes_parent_to_the_root_id_not_the_sentinel() {
        // Regression: a probed device node must name the
        // tree root's id (`HW_NODE_ROOT_ID`) as its parent, never the
        // `HW_NODE_ROOT` *parent sentinel*. A node parented to the sentinel
        // satisfies `HwNode::is_root`, and the devmgr autoload walk skips
        // every root node — so a probed device parented to the sentinel
        // would be discovered yet never bind its driver. Guards both the
        // block and the input probe.
        let blk_bus = FakeBus::with(&[VIRTIO_BLK_DEVICE_ID]);
        let mut blk = CollectingSink::default();
        observe_virtio_mmio_block_devices(&blk_bus, &SlotDma::UNTRANSLATED, &mut blk)
            .expect("enumerate");
        assert_eq!(blk.nodes.len(), 1);
        assert_eq!(blk.nodes[0].parent(), HW_NODE_ROOT_ID);
        assert!(
            !blk.nodes[0].is_root(),
            "a probed block node is a device, not the tree root"
        );

        let kbd_bus = FakeBus::with(&[VIRTIO_INPUT_DEVICE_ID]);
        let mut kbd = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &kbd_bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut kbd,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(kbd.nodes.len(), 1);
        assert_eq!(kbd.nodes[0].parent(), HW_NODE_ROOT_ID);
        assert!(
            !kbd.nodes[0].is_root(),
            "a probed input node is a device, not the tree root"
        );
    }

    #[test]
    fn a_probed_virtio_net_slot_is_discovered_with_its_grants() {
        // A populated virtio-net slot (DeviceID 1) is emitted as a
        // user-space-autoloadable `Network` node keyed by its probed virtio
        // device id, carrying the same register-window + coherent-DMA + IRQ
        // grant requests as an input node (both are interrupt-driven
        // autoloaded drivers). Its node id comes from the network base,
        // disjoint from the block / input / boot-display bases.
        let bus = FakeBus::with(&[tairix_virtio_net::VIRTIO_NET_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_network_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Network));
        assert_eq!(node.id(), VIRTIO_NET_PROBE_NODE_BASE_ID);
        assert_ne!(
            node.id(),
            crate::boot_display::BOOT_DISPLAY_NODE_ID,
            "the network probe base must not collide with the boot-display node id"
        );
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::virtio(tairix_virtio_net::VIRTIO_NET_DEVICE_ID)]
        );
        assert_eq!(
            node.resources(),
            &[
                HwResource::mmio(0x0A00_0000, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1)
            ]
        );
    }

    #[test]
    fn a_probed_virtio_sound_slot_is_discovered_with_its_grants() {
        // Without this probe a sound card is never a hardware-tree node at
        // all, so the signed driver bundle sits in the store as a candidate
        // that nothing can match and the machine has no audio device.
        let bus = FakeBus::with(&[VIRTIO_SND_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_audio_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Audio));
        assert_eq!(node.id(), VIRTIO_AUDIO_PROBE_NODE_BASE_ID);
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::virtio(VIRTIO_SND_DEVICE_ID)],
            "the emitted key must be the one the driver's own bind table carries"
        );
        assert_eq!(
            node.resources(),
            &[
                HwResource::mmio(0x0A00_0000, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1)
            ]
        );
    }

    #[test]
    fn a_non_audio_virtio_slot_emits_no_audio_node() {
        let bus = FakeBus::with(&[
            VIRTIO_BLK_DEVICE_ID,
            VIRTIO_INPUT_DEVICE_ID,
            tairix_virtio_net::VIRTIO_NET_DEVICE_ID,
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_audio_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty(), "only a sound card is a sound card");
    }

    #[test]
    fn a_non_network_virtio_slot_emits_no_network_node() {
        // A virtio-blk slot (2) and a virtio-input slot (18) are not network
        // devices, so the network probe emits nothing — the exact
        // network-free case a display world without a NIC presents.
        let bus = FakeBus::with(&[VIRTIO_BLK_DEVICE_ID, VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_network_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    #[test]
    fn an_overfull_bus_fails_closed_for_input() {
        // More than `MAX_SLOTS` responding slots cannot be enumerated whole,
        // so the input probe surfaces the error and the caller leaves the
        // affected nodes undiscovered.
        let devices = alloc::vec![VIRTIO_INPUT_DEVICE_ID; MAX_SLOTS + 1];
        let bus = FakeBus::with(&devices);
        let mut sink = CollectingSink::default();
        assert_eq!(
            observe_virtio_mmio_input_devices(
                &bus,
                &|_| Some(TEST_INPUT_INTID),
                &SlotDma::UNTRANSLATED,
                &mut sink,
                &NullSink
            ),
            Err(DriverError::BufferTooSmall)
        );
    }

    #[test]
    fn an_input_slot_without_a_resolvable_irq_is_skipped_fail_closed() {
        // A user-space virtio-input driver is interrupt-driven, so a slot
        // whose interrupt line the arch `slot_irq` cannot resolve is left
        // undiscovered rather than emitted without the IRQ its driver parks
        // on.
        let bus = FakeBus::with(&[VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| None,
            &SlotDma::UNTRANSLATED,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    /// A device probed in a slot behind a unit masters DMA as the slot's
    /// streams; one whose slot the tree cannot describe is not published at
    /// all, never as a device reaching memory around every unit.
    #[test]
    fn a_probed_device_masters_dma_as_its_slot_is_described() {
        let streams = tairix_abi::IommuStreams::new(0x8000_0004, 0x20, 2).unwrap();
        let dma = SlotDma::of(
            alloc::vec![(0x0A00_0000, streams)],
            alloc::vec![0x0A00_0200],
        );
        let bus = FakeBus::with(&[VIRTIO_INPUT_DEVICE_ID, VIRTIO_INPUT_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_input_devices(
            &bus,
            &|_| Some(TEST_INPUT_INTID),
            &dma,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(
            sink.nodes.len(),
            1,
            "the undescribed slot's device is withheld"
        );
        assert_eq!(
            sink.nodes[0].resources(),
            &[
                HwResource::mmio(0x0A00_0000, 0x200),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::irq(u64::from(TEST_INPUT_INTID), 1),
                HwResource::iommu_stream(streams),
            ]
        );
        let bus = FakeBus::with(&[VIRTIO_BLK_DEVICE_ID, VIRTIO_BLK_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_block_devices(&bus, &dma, &mut sink).expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        assert_eq!(
            sink.nodes[0].resources(),
            &[HwResource::iommu_stream(streams)]
        );
        assert_eq!(virtio_mmio_block_slot(&sink.nodes[0]), Some(0));
    }

    /// A block node names the slot it was found at, so the floor drives that
    /// slot, never the first on the bus when the first was withheld.
    #[test]
    fn a_block_node_names_the_slot_it_was_found_at() {
        let dma = SlotDma::of(alloc::vec![], alloc::vec![0x0A00_0000]);
        let bus = FakeBus::with(&[VIRTIO_BLK_DEVICE_ID, VIRTIO_BLK_DEVICE_ID]);
        let mut sink = CollectingSink::default();
        observe_virtio_mmio_block_devices(&bus, &dma, &mut sink).expect("enumerate");
        assert_eq!(sink.nodes.len(), 1, "the first slot is withheld");
        assert_eq!(virtio_mmio_block_slot(&sink.nodes[0]), Some(1));
        let unplaced = HwNode::new(1, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
        assert_eq!(virtio_mmio_block_slot(&unplaced), None);
    }

    /// A slot is found by its registers' base among the operational
    /// `virtio,mmio` nodes alone.
    #[test]
    fn a_slot_is_found_by_its_base_while_it_is_operational() {
        let mut b = tairix_fdt::write::FdtWriter::new();
        b.begin_node("");
        for (base, compatible, status) in [
            (0x0A00_0000u64, "virtio,mmio", "okay"),
            (0x0A00_0200, "virtio,mmio", "disabled"),
            (0x0A00_0400, "arm,pl011", "okay"),
        ] {
            b.begin_node(&alloc::format!("dev@{base:x}"));
            b.prop_str("compatible", compatible);
            b.prop_str("status", status);
            let mut reg = alloc::vec::Vec::new();
            reg.extend_from_slice(&base.to_be_bytes());
            reg.extend_from_slice(&0x200u64.to_be_bytes());
            b.prop("reg", &reg);
            b.end_node();
        }
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).unwrap();
        let found = virtio_mmio_slot(&fdt, 0x0A00_0000).unwrap();
        assert_eq!(virtio_mmio_slot_base(&found), Some(0x0A00_0000));
        assert!(virtio_mmio_slot(&fdt, 0x0A00_0200).is_none(), "disabled");
        assert!(virtio_mmio_slot(&fdt, 0x0A00_0400).is_none(), "no slot");
        assert!(
            virtio_mmio_slot(&fdt, 0x0B00_0000).is_none(),
            "nothing there"
        );
    }

    // --- virtio-PCI probe ------------------------------------------------
    //
    // The `VirtioPciBus` trait, the `VIRTIO_PCI_CFG_*` roles, the vendor
    // id, and `virtio_pci_window_resource` are all in scope via
    // `super::*` (the module imports them for the probe itself).

    /// Deterministic interrupt line the PCI-probe tests hand `dev_irq` for
    /// every function; the value is the test's own, never a production
    /// constant.
    const TEST_PCI_INTID: u32 = 40;

    /// Notification multiplier the fake PCI bus advertises, so a test can
    /// assert it flows onto the notify window's grant.
    const TEST_NOTIFY_MULTIPLIER: u32 = 4;

    /// A fake virtio-PCI bus enumerating a fixed function list and
    /// resolving each virtio config window to a synthetic `(base, len)`
    /// keyed by `cfg_type`, so a test can assert the exact windows the
    /// probe grants without any real config-space access.
    struct FakePciBus {
        functions: alloc::vec::Vec<BusDevice>,
        /// The features every function offers.
        offered: u64,
    }

    impl FakePciBus {
        /// A bus carrying one function per `(device_id, bdf)`, all
        /// reporting the virtio vendor id and offering the transport
        /// features.
        fn with(functions: &[(u16, u64)]) -> Self {
            Self {
                functions: functions
                    .iter()
                    .map(|&(device, address)| BusDevice {
                        vendor: u32::from(VIRTIO_PCI_VENDOR_ID),
                        device: u32::from(device),
                        class: 0x0200,
                        reserved0: 0,
                        address,
                    })
                    .collect(),
                offered: tairix_virtio::TRANSPORT_FEATURES,
            }
        }
    }

    impl Bus for FakePciBus {
        fn enumerate(&self, out: &mut [BusDevice]) -> Result<usize, DriverError> {
            if out.len() < self.functions.len() {
                return Err(DriverError::BufferTooSmall);
            }
            out[..self.functions.len()].copy_from_slice(&self.functions);
            Ok(self.functions.len())
        }
    }

    impl VirtioPciBus for FakePciBus {
        fn virtio_window_region(
            &self,
            bdf: u64,
            cfg_type: u8,
        ) -> Result<(u64, usize), DriverError> {
            // Base encodes the function (bdf) and structure (cfg_type) so
            // distinct windows are distinguishable in assertions.
            let len = match cfg_type {
                VIRTIO_PCI_CFG_COMMON => 0x38,
                VIRTIO_PCI_CFG_NOTIFY => 0x10,
                VIRTIO_PCI_CFG_ISR => 0x4,
                VIRTIO_PCI_CFG_DEVICE => 0x8,
                _ => return Err(DriverError::NotFound),
            };
            let base = 0xC000_0000 + (bdf << 16) + (u64::from(cfg_type) << 8);
            Ok((base, len))
        }

        fn notify_off_multiplier(&self, _bdf: u64) -> Result<u32, DriverError> {
            Ok(TEST_NOTIFY_MULTIPLIER)
        }

        fn offered_features(
            &self,
            _bdf: u64,
            _registers: &dyn MmioMapper,
        ) -> Result<u64, DriverError> {
            Ok(self.offered)
        }
    }

    /// Reaches no registers: the fake answers its features itself.
    struct NoRegisters;

    impl MmioMapper for NoRegisters {
        fn map_window(
            &self,
            _phys_base: u64,
            _len: usize,
        ) -> Result<tairix_abi::RegisterWindow, tairix_abi::MmioMapError> {
            Err(tairix_abi::MmioMapError::InvalidRegion)
        }
    }

    /// One walk of `bus` on `segment`, its units knowing its functions'
    /// DMA as `dma` says.
    fn walk_of<'a>(bus: &'a FakePciBus, segment: PciSegment, dma: DmaIdentity<'a>) -> PciWalk<'a> {
        PciWalk {
            segment,
            functions: &bus.functions,
            bus,
            registers: &NoRegisters,
            dma,
            coherence: DmaCoherence::Snooped,
        }
    }

    /// How a unit at node `0x800A_0000` knows the function at `address`: by
    /// its requester id, in a group of its own.
    fn translated_dma(address: u64) -> FunctionDma {
        let id = u32::from(tairix_abi::driver::pci::requester_id(address));
        FunctionDma {
            stream: tairix_abi::IommuStreams::new(0x800A_0000, id, 1).unwrap(),
            aliases: crate::pci_probe::Aliases::new(),
            group: Some(tairix_abi::IommuGroup::new(0x800A_0000, id)),
            untrusted: false,
            interrupts: tairix_kernel_core::iommu::InterruptSource::Requester(
                tairix_abi::driver::pci::requester_id(address),
            ),
        }
    }

    /// The modern virtio-net PCI device id (`0x1040 + 1`).
    const VIRTIO_NET_PCI_DEVICE_ID: u16 = 0x1041;

    /// The first segment the kernel owns.
    const SEGMENT: PciSegment = PciSegment {
        number: 0,
        ordinal: 0,
    };

    /// The node a function at configuration `address` on [`SEGMENT`] gets.
    fn node_of(address: u64) -> u32 {
        SEGMENT.node_id(address).unwrap()
    }

    /// An xHCI function at [`Self::XHCI`] and an EHCI one at [`Self::EHCI`],
    /// on a host resolving the xHCI's register window as `driver_window`
    /// does; `window` is what that resolution answers.
    struct UsbHosts {
        functions: [BusDevice; 3],
        window: Result<(u64, u64), DriverError>,
    }

    impl UsbHosts {
        const XHCI: u64 = 0x0000_2000;
        const EHCI: u64 = 0x0000_2800;
        const HDA: u64 = 0x0000_1800;

        fn with(window: Result<(u64, u64), DriverError>) -> Self {
            let function = |address, device, class| BusDevice {
                vendor: 0x1B36,
                device,
                class,
                reserved0: 0,
                address,
            };
            Self {
                functions: [
                    function(Self::XHCI, 0x000D, 0x0C03),
                    function(Self::EHCI, 0x000D, 0x0C03),
                    function(Self::HDA, 0x2668, 0x0403),
                ],
                window,
            }
        }
    }

    impl Bus for UsbHosts {
        fn enumerate(&self, out: &mut [BusDevice]) -> Result<usize, DriverError> {
            let slots = out.get_mut(..3).ok_or(DriverError::BufferTooSmall)?;
            slots.copy_from_slice(&self.functions);
            Ok(3)
        }
    }

    impl VirtioPciBus for UsbHosts {
        fn virtio_window_region(&self, _: u64, _: u8) -> Result<(u64, usize), DriverError> {
            Err(DriverError::NotFound)
        }

        fn notify_off_multiplier(&self, _: u64) -> Result<u32, DriverError> {
            Err(DriverError::NotFound)
        }
    }

    impl tairix_abi::driver::msix::MsixBus for UsbHosts {
        fn route_msix(
            &self,
            _: u64,
            _: u16,
            _: tairix_abi::driver::msix::MsiMessage,
            _: &dyn MmioMapper,
        ) -> Result<(), DriverError> {
            Ok(())
        }

        fn msix_entries(&self, _: u64) -> Result<u16, DriverError> {
            Ok(16)
        }

        fn mask_msix(&self, _: u64, _: bool) -> Result<(), DriverError> {
            Ok(())
        }
    }

    impl tairix_abi::driver::pci::PciBus for UsbHosts {
        fn map_bar_window(
            &self,
            _: u64,
            _: u8,
            _: &dyn MmioMapper,
        ) -> Result<tairix_abi::RegisterWindow, DriverError> {
            Err(DriverError::Unsupported)
        }

        fn driver_window(&self, bdf: u64, bar_index: u8) -> Result<(u64, u64), DriverError> {
            if (bdf == Self::XHCI || bdf == Self::HDA) && bar_index == 0 {
                self.window
            } else {
                Err(DriverError::NotFound)
            }
        }

        fn enable_memory_space(&self, _: u64) -> Result<(), DriverError> {
            Ok(())
        }

        fn set_bus_master(&self, _: u64, _: bool) -> Result<(), DriverError> {
            Ok(())
        }

        fn set_intx(&self, _: u64, _: bool) -> Result<(), DriverError> {
            Ok(())
        }

        fn assign_bar(&self, _: u64, _: u8, _: u64, _: u64) -> Result<u64, DriverError> {
            Err(DriverError::Unsupported)
        }

        fn read_config(&self, bdf: u64, offset: u16) -> Result<u32, DriverError> {
            Ok(match (bdf, offset) {
                (Self::XHCI, tairix_abi::driver::pci::CLASS_OFFSET) => 0x0C03_3001,
                (Self::EHCI, tairix_abi::driver::pci::CLASS_OFFSET) => 0x0C03_2001,
                (Self::HDA, tairix_abi::driver::pci::CLASS_OFFSET) => 0x0403_0001,
                _ => 0,
            })
        }

        fn capability_header(&self, _: u64, _: u8) -> Result<u32, DriverError> {
            Err(DriverError::NotFound)
        }

        fn describe_function(&self, _: u64) -> Result<HwNode, DriverError> {
            Err(DriverError::Unsupported)
        }
    }

    fn observe_usb_hosts(
        hosts: &UsbHosts,
        dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    ) -> alloc::vec::Vec<HwNode> {
        observe_hosts(hosts, XHCI_CONTROLLERS, dev_irq)
    }

    fn observe_hosts(
        hosts: &UsbHosts,
        class: PciClass,
        dev_irq: &dyn Fn(u64) -> Option<DeviceInterrupt>,
    ) -> alloc::vec::Vec<HwNode> {
        let walk = PciWalk {
            segment: SEGMENT,
            functions: &hosts.functions,
            bus: hosts,
            registers: &NoRegisters,
            dma: &|_| None,
            coherence: DmaCoherence::Snooped,
        };
        let mut sink = CollectingSink::default();
        observe_pci_class_functions(&walk, hosts, class, dev_irq, &mut sink, &NullSink)
            .expect("enumerate");
        sink.nodes
    }

    #[test]
    fn an_xhci_function_is_published_with_its_window_below_its_msix_state() {
        let hosts = UsbHosts::with(Ok((0xFE00_0000, 0x3000)));
        let line = HwResource::message_irq(u64::from(TEST_PCI_INTID), 0);
        let nodes = observe_usb_hosts(&hosts, &|_| Some(DeviceInterrupt::line(line)));
        assert_eq!(nodes.len(), 1, "the EHCI function is no xHCI");
        let node = &nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Bus));
        assert_eq!(node.id(), node_of(UsbHosts::XHCI));
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::pci(0x1B36, 0x000D, CLASS_USB_XHCI)]
        );
        assert_eq!(
            node.resources(),
            &[
                HwResource::mmio(0xFE00_0000, 0x3000),
                HwResource::dma(0, 0, DmaCoherence::Snooped),
                line,
            ]
        );
    }

    #[test]
    fn an_hd_audio_controller_is_published_as_an_audio_node() {
        let hosts = UsbHosts::with(Ok((0xFE10_0000, 0x4000)));
        let line = HwResource::message_irq(u64::from(TEST_PCI_INTID), 0);
        let nodes = observe_hosts(&hosts, HD_AUDIO_CONTROLLERS, &|_| {
            Some(DeviceInterrupt::line(line))
        });
        assert_eq!(
            nodes.len(),
            1,
            "only the audio function is an HD Audio controller"
        );
        let node = &nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Audio));
        assert_eq!(node.id(), node_of(UsbHosts::HDA));
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::pci(0x1B36, 0x2668, CLASS_HD_AUDIO)]
        );
        assert_eq!(
            node.resources(),
            &[
                HwResource::mmio(0xFE10_0000, 0x4000),
                HwResource::dma(0, 0, DmaCoherence::Snooped),
                line,
            ]
        );
    }

    #[test]
    fn an_xhci_function_with_no_window_or_no_line_is_left_undiscovered() {
        let line = HwResource::message_irq(u64::from(TEST_PCI_INTID), 0);
        let windowless = UsbHosts::with(Err(DriverError::NotFound));
        assert!(observe_usb_hosts(&windowless, &|_| Some(DeviceInterrupt::line(line))).is_empty());
        let unrouted = UsbHosts::with(Ok((0xFE00_0000, 0x3000)));
        assert!(observe_usb_hosts(&unrouted, &|_| None).is_empty());
        let mmio_line = HwResource::mmio(0x1000, 0x1000);
        assert!(
            observe_usb_hosts(&unrouted, &|_| Some(DeviceInterrupt::line(mmio_line))).is_empty(),
            "only an interrupt is a line"
        );
    }

    #[test]
    fn a_virtio_net_pci_function_is_discovered_with_role_tagged_windows() {
        // A modern virtio-net PCI function is emitted as a `Network` node
        // keyed by the shared virtio *type* (1), carrying its four
        // role-tagged config windows (the notify window alone carrying the
        // multiplier), a coherent DMA constraint, and its routed interrupt
        // line — the exact grant set the autoloaded user-space driver's
        // `virtio_pci_windows` resolver consumes.
        let bus = FakePciBus::with(&[(VIRTIO_NET_PCI_DEVICE_ID, 0x0000_0800)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Network));
        assert_eq!(node.id(), node_of(0x0000_0800));
        // The bind key is the virtio *type*, identical to the MMIO probe's,
        // so one signed bundle binds on both buses.
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::virtio(tairix_virtio_net::VIRTIO_NET_DEVICE_ID)]
        );
        let base = 0xC000_0000 + (0x0000_0800u64 << 16);
        assert_eq!(
            node.resources(),
            &[
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_COMMON,
                    base + (u64::from(VIRTIO_PCI_CFG_COMMON) << 8),
                    0x38,
                    0,
                ),
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_NOTIFY,
                    base + (u64::from(VIRTIO_PCI_CFG_NOTIFY) << 8),
                    0x10,
                    TEST_NOTIFY_MULTIPLIER,
                ),
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_ISR,
                    base + (u64::from(VIRTIO_PCI_CFG_ISR) << 8),
                    0x4,
                    0,
                ),
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_DEVICE,
                    base + (u64::from(VIRTIO_PCI_CFG_DEVICE) << 8),
                    0x8,
                    0,
                ),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::message_irq(u64::from(TEST_PCI_INTID), 0),
            ]
        );
        // The emitted windows round-trip through the driver-side resolver.
        let windows =
            tairix_abi::driver::virtio_pci::virtio_pci_windows(node.resources()).expect("resolve");
        assert_eq!(windows.notify_off_multiplier, TEST_NOTIFY_MULTIPLIER);
        assert_eq!(windows.msix_entry, Some(0));
        assert_eq!(
            windows.common,
            (base + (u64::from(VIRTIO_PCI_CFG_COMMON) << 8), 0x38)
        );
    }

    /// A function whose messages a unit translates carries the doorbell they
    /// are written to beside its line, so its domain maps it; one raising
    /// them on a wire carries none.
    #[test]
    fn a_function_raising_messages_through_a_unit_carries_their_doorbell() {
        let bus = FakePciBus::with(&[(VIRTIO_NET_PCI_DEVICE_ID, 0x0000_0800)]);
        let doorbell = HwResource::msi_doorbell(0x0809_0000, 0x1000).expect("a page");
        let mut sink = CollectingSink::default();
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt {
                    line: HwResource::message_irq(u64::from(TEST_PCI_INTID), 0),
                    doorbell: Some(doorbell),
                })
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        let resources = sink.nodes[0].resources();
        assert_eq!(
            resources[resources.len() - 2..],
            [
                HwResource::message_irq(u64::from(TEST_PCI_INTID), 0),
                doorbell
            ]
        );
    }

    #[test]
    fn a_non_net_virtio_pci_function_emits_no_network_node() {
        // A virtio-blk PCI function (0x1042) and a non-virtio device id are
        // not virtio-net, so the network probe emits nothing.
        let bus = FakePciBus::with(&[(0x1042, 0x0000_0800), (0x1050, 0x0000_0900)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    #[test]
    fn two_virtio_net_pci_functions_get_distinct_ids() {
        // Two virtio-net functions each become a distinct node, so a machine
        // with two NICs never merges them.
        let bus = FakePciBus::with(&[
            (VIRTIO_NET_PCI_DEVICE_ID, 0x0000_0800),
            (VIRTIO_NET_PCI_DEVICE_ID, 0x0000_1000),
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 2);
        assert_eq!(sink.nodes[0].id(), node_of(0x0000_0800));
        assert_eq!(sink.nodes[1].id(), node_of(0x0000_1000));
    }

    #[test]
    fn a_virtio_net_pci_function_without_an_irq_is_skipped_fail_closed() {
        // The driver parks on its interrupt, so a function whose line the
        // arch resolver cannot route is left undiscovered rather than
        // emitted without it.
        let bus = FakePciBus::with(&[(VIRTIO_NET_PCI_DEVICE_ID, 0x0000_0800)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| None,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    // --- virtio-PCI block probe ------------------------------------------

    /// The modern virtio-blk PCI device id (`0x1040 + 2`).
    const VIRTIO_BLK_PCI_DEVICE_ID: u16 = 0x1042;

    #[test]
    fn a_virtio_blk_pci_function_is_discovered_match_key_only() {
        // A modern virtio-blk PCI function is emitted as a `Storage` node
        // keyed by the shared virtio *type* (the same key the MMIO block
        // probe uses, so one signed virtio-blk bundle binds on either bus),
        // and — being an in-kernel bootstrap floor whose bring-up
        // re-resolves the transport from config space — carries **no**
        // resource grants, unlike the user-space PCI network node.
        let bus = FakePciBus::with(&[(VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_0800)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(&walk_of(&bus, SEGMENT, &|_| None), &mut sink, &NullSink)
            .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Storage));
        assert_eq!(node.id(), node_of(0x0000_0800));
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::virtio(VIRTIO_BLK_DEVICE_ID)]
        );
        assert!(
            node.resources().is_empty(),
            "a bootstrap-floor block node carries only its bind key, no grants"
        );
        // A probed device node is parented to the tree root id, not the
        // parent sentinel, so the autoload walk treats it as a device.
        assert_eq!(node.parent(), HW_NODE_ROOT_ID);
        assert!(!node.is_root());
        assert_eq!(
            node.address(),
            0x0008,
            "the function's segment and requester id"
        );
    }

    #[test]
    fn a_translated_pci_function_carries_the_stream_its_unit_knows_it_by() {
        // The unit knows every function by its requester id.
        let translated = |address: u64| Some(translated_dma(address));
        let bus = FakePciBus::with(&[
            (VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_1800),
            (VIRTIO_NET_PCI_DEVICE_ID, 0x0002_0900),
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(
            &walk_of(&bus, SEGMENT, &translated),
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &translated),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 2);
        for (node, id) in [(&sink.nodes[0], 0x0018), (&sink.nodes[1], 0x0209)] {
            assert_eq!(node.address(), id);
            let streams: alloc::vec::Vec<_> = node
                .resources()
                .iter()
                .filter_map(|r| r.iommu_streams().ok())
                .collect();
            assert_eq!(
                streams,
                [tairix_abi::IommuStreams::new(0x800A_0000, id, 1).unwrap()]
            );
        }
    }

    /// A device that declines `VIRTIO_F_ACCESS_PLATFORM` reaches memory by
    /// physical address, past its unit, so it is never published, and never
    /// routed an interrupt or made a bus master on the way; the refusal is
    /// audited. Without a unit the same device is published untranslated.
    #[test]
    fn a_function_behind_a_unit_that_would_not_use_it_is_refused() {
        let translated = |address: u64| Some(translated_dma(address));
        let mut bus = FakePciBus::with(&[
            (VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_1800),
            (VIRTIO_NET_PCI_DEVICE_ID, 0x0002_0900),
        ]);
        bus.offered = tairix_virtio::VIRTIO_F_VERSION_1;
        let log = IdLog::default();
        let routed = core::cell::Cell::new(0);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(&walk_of(&bus, SEGMENT, &translated), &mut sink, &log)
            .expect("enumerate");
        observe_virtio_pci_network_devices(
            &walk_of(&bus, SEGMENT, &translated),
            &|_| {
                routed.set(routed.get() + 1);
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &log,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
        assert_eq!(
            routed.get(),
            0,
            "no interrupt is routed to a refused function"
        );
        let bypass = tairix_kernel_core::AuditEvent::DmaTranslationBypass.id();
        assert_eq!(log.0.borrow().iter().filter(|&&id| id == bypass).count(), 2);

        let mut untranslated = CollectingSink::default();
        observe_virtio_pci_block_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &mut untranslated,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(untranslated.nodes.len(), 1);
    }

    /// Only virtio lets a device decline the platform's translation, so a
    /// translated function of any other vendor is admitted with its stream
    /// whatever its "features" read as.
    #[test]
    fn a_non_virtio_function_behind_a_unit_is_admitted_with_its_stream() {
        let mut bus = FakePciBus::with(&[]);
        bus.offered = 0;
        let device = |vendor| BusDevice {
            vendor,
            device: 0x10D3,
            class: 0x0200,
            reserved0: 0,
            address: 0x0000_1800,
        };
        let translated = |address: u64| Some(translated_dma(address));
        let walk = walk_of(&bus, SEGMENT, &translated);
        let admitted = PciFunction::admit(&walk, &device(0x8086), &NullSink)
            .expect("a translated non-virtio function");
        assert_eq!(admitted.dma, Some(translated_dma(0x0000_1800)));
        assert!(
            PciFunction::admit(&walk, &device(u32::from(VIRTIO_PCI_VENDOR_ID)), &NullSink)
                .is_none(),
            "a virtio function that declines the unit is still refused"
        );
    }

    /// A virtio function whose common configuration is a register file the
    /// test holds: the trait's own read reaches it through the walk's
    /// registers, as the kernel's does.
    struct CommonConfig {
        function: BusDevice,
        registers: core::cell::UnsafeCell<[u32; 16]>,
    }

    impl Bus for CommonConfig {
        fn enumerate(&self, out: &mut [BusDevice]) -> Result<usize, DriverError> {
            let slot = out.first_mut().ok_or(DriverError::BufferTooSmall)?;
            *slot = self.function;
            Ok(1)
        }
    }

    impl VirtioPciBus for CommonConfig {
        fn virtio_window_region(&self, _bdf: u64, cfg: u8) -> Result<(u64, usize), DriverError> {
            if cfg == VIRTIO_PCI_CFG_COMMON {
                Ok((0xC000_0000, 0x38))
            } else {
                Err(DriverError::NotFound)
            }
        }

        fn notify_off_multiplier(&self, _bdf: u64) -> Result<u32, DriverError> {
            Ok(TEST_NOTIFY_MULTIPLIER)
        }
    }

    impl MmioMapper for CommonConfig {
        fn map_window(
            &self,
            phys_base: u64,
            len: usize,
        ) -> Result<tairix_abi::RegisterWindow, tairix_abi::MmioMapError> {
            let base = core::ptr::NonNull::new(self.registers.get().cast::<u8>())
                .ok_or(tairix_abi::MmioMapError::InvalidRegion)?;
            // SAFETY: the register file outlives every window a test mints
            // from it, the window touches at most its 64 bytes, and nothing
            // else references it while one is live.
            Ok(unsafe { tairix_abi::RegisterWindow::from_mapping(phys_base, base, len.min(64)) })
        }
    }

    /// A translated virtio function is judged on the features its common
    /// configuration shows through the kernel's own register reach: admitted
    /// when they honour the unit, refused when they do not, and refused when
    /// the read answers all ones, as a function not decoding does.
    #[test]
    fn a_translated_virtio_function_is_judged_through_the_kernel_s_registers() {
        let translated = |address: u64| Some(translated_dma(address));
        let function = BusDevice {
            vendor: u32::from(VIRTIO_PCI_VENDOR_ID),
            device: 0x1052,
            class: 0x0980,
            reserved0: 0,
            address: 0x0000_1800,
        };
        // The register file answers both halves alike, so a word with bit 1
        // set offers bit 33, `VIRTIO_F_ACCESS_PLATFORM`.
        for (word, admitted) in [(0x2, true), (0x1, false), (u32::MAX, false)] {
            let bus = CommonConfig {
                function,
                registers: core::cell::UnsafeCell::new([0; 16]),
            };
            // SAFETY: no window over the register file is live yet.
            unsafe { (*bus.registers.get())[common::DEVICE_FEATURE / 4] = word };
            let walk = PciWalk {
                segment: SEGMENT,
                functions: core::slice::from_ref(&function),
                bus: &bus,
                registers: &bus,
                dma: &translated,
                coherence: DmaCoherence::Snooped,
            };
            assert_eq!(
                PciFunction::admit(&walk, &function, &NullSink).is_some(),
                admitted,
                "{word:#x}"
            );
        }
    }

    /// Records the `reason` field of every event that carries one.
    #[derive(Default)]
    struct ReasonLog(core::cell::RefCell<alloc::vec::Vec<&'static str>>);

    impl Sink for ReasonLog {
        fn write_event(&self, event: &Event<'_>) {
            for field in event.fields {
                if let (true, FieldValue::Str(reason)) = (field.key == "reason", &field.value) {
                    let known = ["untrusted", "unconfinable", "bypasses_unit"];
                    if let Some(reason) = known.iter().find(|known| *known == reason) {
                        self.0.borrow_mut().push(reason);
                    }
                }
            }
        }
    }

    /// A function behind an external-facing port that does not validate
    /// requester ids is refused, and the record says why.
    #[test]
    fn an_untrusted_function_its_port_cannot_confine_is_refused_as_untrusted() {
        let bus = FakePciBus::with(&[(VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_0800)]);
        let unconfinable = |untrusted| {
            move |address: u64| {
                Some(FunctionDma {
                    group: None,
                    untrusted,
                    ..translated_dma(address)
                })
            }
        };
        let log = ReasonLog::default();
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(
            &walk_of(&bus, SEGMENT, &unconfinable(true)),
            &mut sink,
            &log,
        )
        .expect("enumerate");
        observe_virtio_pci_block_devices(
            &walk_of(&bus, SEGMENT, &unconfinable(false)),
            &mut sink,
            &log,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
        assert_eq!(*log.0.borrow(), ["untrusted", "unconfinable"]);
    }

    #[test]
    fn a_function_on_another_segment_names_it_and_takes_that_segment_s_node() {
        let segment = PciSegment {
            number: 3,
            ordinal: 1,
        };
        let bus = FakePciBus::with(&[(VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_0800)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(&walk_of(&bus, segment, &|_| None), &mut sink, &NullSink)
            .expect("enumerate");
        let node = &sink.nodes[0];
        assert_eq!(node.address(), 0x0003_0008);
        assert_eq!(
            PciAddress::from_node_address(node.address()),
            PciAddress::new(3, 0x0008)
        );
        assert_eq!(node.id(), segment.node_id(0x0000_0800).unwrap());
        assert_ne!(
            node.id(),
            node_of(0x0000_0800),
            "one requester id on two segments is two nodes"
        );
    }

    #[test]
    fn a_non_block_virtio_pci_function_emits_no_block_node() {
        // A virtio-net PCI function (0x1041) and a non-block virtio device
        // id are not virtio-blk, so the block probe emits nothing.
        let bus = FakePciBus::with(&[
            (VIRTIO_NET_PCI_DEVICE_ID, 0x0000_0800),
            (0x1050, 0x0000_0900),
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(&walk_of(&bus, SEGMENT, &|_| None), &mut sink, &NullSink)
            .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    #[test]
    fn a_non_virtio_vendor_function_with_the_blk_device_id_emits_no_block_node() {
        // The vendor guard is load-bearing: a function that happens to
        // report the virtio-blk *device id* but a foreign vendor is not a
        // virtio device and must not be bound as the root disk (fail
        // closed). `FakePciBus::with` pins the virtio vendor, so build the
        // function directly with a non-virtio vendor.
        let bus = FakePciBus {
            functions: alloc::vec![BusDevice {
                vendor: 0x1234,
                device: u32::from(VIRTIO_BLK_PCI_DEVICE_ID),
                class: 0x0100,
                reserved0: 0,
                address: 0x0000_0800,
            }],
            offered: tairix_virtio::TRANSPORT_FEATURES,
        };
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(&walk_of(&bus, SEGMENT, &|_| None), &mut sink, &NullSink)
            .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    #[test]
    fn two_virtio_blk_pci_functions_get_distinct_ids() {
        // Two virtio-blk functions each become a distinct node, so a machine
        // with two disks never merges them (the root selection then fails
        // closed on the ambiguity, but discovery must still surface both).
        let bus = FakePciBus::with(&[
            (VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_0800),
            (VIRTIO_BLK_PCI_DEVICE_ID, 0x0000_1000),
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_block_devices(&walk_of(&bus, SEGMENT, &|_| None), &mut sink, &NullSink)
            .expect("enumerate");
        assert_eq!(sink.nodes.len(), 2);
        assert_eq!(sink.nodes[0].id(), node_of(0x0000_0800));
        assert_eq!(sink.nodes[1].id(), node_of(0x0000_1000));
    }

    // --- virtio-PCI input probe ------------------------------------------

    /// The modern virtio-input PCI device id (`0x1040 + 18`).
    const VIRTIO_INPUT_PCI_DEVICE_ID: u16 = 0x1052;

    #[test]
    fn a_virtio_input_pci_function_is_discovered_with_role_tagged_windows() {
        // A modern virtio-input PCI function (a `-device virtio-keyboard-pci`
        // / `virtio-mouse-pci`) is emitted as an `Input` node keyed by the
        // shared virtio *type* (18) — the same key the MMIO input probe
        // uses, so one signed input bundle binds on either bus — carrying
        // its four role-tagged config windows (the notify window alone
        // carrying the multiplier), a coherent DMA constraint, and its
        // routed interrupt line: the exact grant set the autoloaded
        // `virtio_kbd` driver's `virtio_pci_windows` resolver consumes.
        let bus = FakePciBus::with(&[(VIRTIO_INPUT_PCI_DEVICE_ID, 0x0000_0800)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_input_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.class(), Some(HwDeviceClass::Input));
        assert_eq!(node.id(), node_of(0x0000_0800));
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::virtio(VIRTIO_INPUT_DEVICE_ID)]
        );
        let base = 0xC000_0000 + (0x0000_0800u64 << 16);
        assert_eq!(
            node.resources(),
            &[
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_COMMON,
                    base + (u64::from(VIRTIO_PCI_CFG_COMMON) << 8),
                    0x38,
                    0,
                ),
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_NOTIFY,
                    base + (u64::from(VIRTIO_PCI_CFG_NOTIFY) << 8),
                    0x10,
                    TEST_NOTIFY_MULTIPLIER,
                ),
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_ISR,
                    base + (u64::from(VIRTIO_PCI_CFG_ISR) << 8),
                    0x4,
                    0,
                ),
                virtio_pci_window_resource(
                    VIRTIO_PCI_CFG_DEVICE,
                    base + (u64::from(VIRTIO_PCI_CFG_DEVICE) << 8),
                    0x8,
                    0,
                ),
                HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped),
                HwResource::message_irq(u64::from(TEST_PCI_INTID), 0),
            ]
        );
        // The emitted windows round-trip through the driver-side resolver,
        // exactly as the `virtio_kbd` PCI path consumes them.
        let windows =
            tairix_abi::driver::virtio_pci::virtio_pci_windows(node.resources()).expect("resolve");
        assert_eq!(windows.notify_off_multiplier, TEST_NOTIFY_MULTIPLIER);
        assert_eq!(windows.msix_entry, Some(0));
        assert_eq!(
            windows.common,
            (base + (u64::from(VIRTIO_PCI_CFG_COMMON) << 8), 0x38)
        );
    }

    #[test]
    fn a_non_input_virtio_pci_function_emits_no_input_node() {
        // A virtio-net PCI function (0x1041) and a non-input virtio device
        // id are not virtio-input, so the input probe emits nothing.
        let bus = FakePciBus::with(&[
            (VIRTIO_NET_PCI_DEVICE_ID, 0x0000_0800),
            (0x1050, 0x0000_0900),
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_input_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }

    #[test]
    fn two_virtio_input_pci_functions_get_distinct_ids() {
        // A keyboard and a mouse function each become a distinct node, so
        // the two are never merged (one driver instance is spawned per node).
        let bus = FakePciBus::with(&[
            (VIRTIO_INPUT_PCI_DEVICE_ID, 0x0000_0800),
            (VIRTIO_INPUT_PCI_DEVICE_ID, 0x0000_1000),
        ]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_input_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| {
                Some(DeviceInterrupt::line(HwResource::message_irq(
                    u64::from(TEST_PCI_INTID),
                    0,
                )))
            },
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert_eq!(sink.nodes.len(), 2);
        assert_eq!(sink.nodes[0].id(), node_of(0x0000_0800));
        assert_eq!(sink.nodes[1].id(), node_of(0x0000_1000));
    }

    #[test]
    fn a_virtio_input_pci_function_without_an_irq_is_skipped_fail_closed() {
        // The driver parks on its interrupt, so a function whose line the
        // arch resolver cannot route is left undiscovered rather than
        // emitted without it.
        let bus = FakePciBus::with(&[(VIRTIO_INPUT_PCI_DEVICE_ID, 0x0000_0800)]);
        let mut sink = CollectingSink::default();
        observe_virtio_pci_input_devices(
            &walk_of(&bus, SEGMENT, &|_| None),
            &|_| None,
            &mut sink,
            &NullSink,
        )
        .expect("enumerate");
        assert!(sink.nodes.is_empty());
    }
}
