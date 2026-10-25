//! aarch64 early-boot platform discovery.
//!
//! Implements the Arch HAL
//! [`PlatformDiscovery`](tairix_arch_api::PlatformDiscovery) slice over the
//! shared device-tree walk ([`tairix_arch_api::fdtwalk`]), which normalises
//! the flattened device tree the firmware hands the kernel into
//! [`tairix_abi::hwtree`] nodes generically — no per-device list to grow
//! (`PLAN.md` Stage 4.HW).
//!
//! What is genuinely this port's, and so lives here:
//!
//! * **the GIC interrupt specifier.** Both supported boards (QEMU `virt`,
//!   the Pi 4) describe interrupts with the three-cell `<type, number,
//!   flags>` form whose `number` is relative to its type, so the shared
//!   walk is handed the cell width and the type-relative INTID mapping
//!   ([`crate::fdt::gic_intid_from_cells`]);
//! * **the GIC's identity**, so a node wired to a second-level controller
//!   keeps none of its specifiers rather than having them read as the GIC's;
//! * **the board augmentation** the shared walk cannot know: the
//!   `VideoCore` mailbox's DMA property-buffer carve, the GENET MAC's DMA
//!   reach read from its parent bus, the EMMC2 SD host's translated DMA
//!   windows and the firmware-switched rails its supplies name
//!   (`sd_supply`), the BCM2711 PCIe host bridge's
//!   inbound aperture and outbound window, the Broadcom DMA binding's
//!   channel mask, which counts channels across the whole DMA block, the
//!   lines a Raspberry Pi's GPIO block lends its header, the AVS monitor's
//!   window, which the one temperature sensor inside it takes, and the
//!   display engine's fixed wiring — its pixel valves and the HDMI encoders
//!   they feed — which the tree leaves implicit;
//! * **`pcie_bringup`**, the pre-MMU read of those PCIe windows the
//!   in-kernel USB bring-up needs before any hardware tree exists.
//!
//! The PSCI conduit the tree also carries ([`crate::fdt::psci_method`]) is
//! the prerequisite the aarch64 SMP bring-up (Stage W6) consumes; it is a
//! firmware-call property rather than a device node, so it is exposed
//! through the reader, not as a tree node.

use crate::fdt::{gic_intid_from_cells, Fdt};
use tairix_abi::driver::encoder::ENCODER_ENDPOINTS;
use tairix_abi::driver::gpio::HeaderLines;
use tairix_abi::hwlink::{LinkDuty, LinkRequest};
use tairix_abi::{
    DmaCoherence, HwDeviceClass, HwNode, HwProperty, HwResource, HW_NODE_MAX_RESOURCES,
};
use tairix_arch_api::fdtwalk::push_dma_windows;
use tairix_arch_api::fdtwalk::{emitted, FdtPlatform};
use tairix_fdt::{
    bus_level, dma_ranges_aperture, dma_ranges_aperture_of, outbound_mmio_window, read_cells,
    scan_translated, translated_reg, BusLevel, Node, MAX_WALK_DEPTH,
};
// The single source of the mailbox `compatible` match identity lives in the
// device's own client crate (`lib/vcmailbox`) so the discovery key here and
// the `vcmailbox` service driver's `BIND_KEYS` can never diverge. The
// firmware's aperture is that crate's too: the mailbox node's DMA resource
// declares it as the limit the property buffer is carved below.
use tairix_vcmailbox::{
    APERTURE_LIMIT, DISPLAY_ENGINE_COMPATIBLE, ENGINE_OUTPUTS, MAILBOX_COMPATIBLE,
    SD_HOST_COMPATIBLE,
};

/// Length of the DMA-visible property-buffer carve the mailbox node
/// requests: one page — the mapping granularity — which comfortably
/// holds the 128-byte, 16-byte-aligned property message the firmware
/// protocol exchanges.
const MAILBOX_DMA_BUFFER_LEN: u64 = 4096;

/// The window the BCM2711 HDMI binding lists for its interrupt controller's
/// registers, which are that controller's grant.
const HDMI_INTERRUPT_WINDOW: &[u8] = b"intr2";

/// `compatible` string of the BCM2711 `PCIe` root complex — the host
/// bridge the Pi 4's USB-A ports sit behind (the VL805 xHCI controller,
/// `plans/PI.md` P10). Its emission is augmented with the **inbound-DMA
/// aperture** the bridge grants devices behind it, so a matched bus
/// driver knows the CPU-physical window its DMA carves must lie within
/// (a capability-grant request, never an ambient
/// handle); the aperture is read from the node's `dma-ranges`, never a
/// board constant.
///
/// Public because it is the **discovery-contract identity** of the bridge
/// node: the in-kernel driver-candidate catalogue
/// (`kernel/tairix-kernel::driver_catalog`, PLAN Stage 4.HW item 5)
/// reconstructs the discovered node's `compatible` match key from this same
/// constant — the string the device tree advertised and [`pcie_bringup`]
/// verified — so the USB bring-up is gated by a match against the discovered
/// identity, never a fabricated key.
pub const PCIE_COMPATIBLE: &[u8] = b"brcm,bcm2711-pcie";

/// `compatible` string of the BCM2711's GENET v5 Ethernet MAC — the Pi 4's
/// on-board gigabit NIC.
///
/// Its emission is augmented with the **DMA aperture** its parent bus grants
/// (a capability-grant request, never an ambient handle), because only the
/// platform's tree knows that window and the MAC's own node declares none.
/// The autoloaded `drivers/network/genet` driver states the same identity in
/// its bind table; this is the discovery side of that contract.
pub const GENET_COMPATIBLE: &[u8] = b"brcm,bcm2711-genet-v5";

/// The `compatible` vendor prefix of every Raspberry Pi board's root.
const RASPBERRY_PI_VENDOR: &[u8] = b"raspberrypi,";

/// The lines of the chip's GPIO block a Raspberry Pi's 40-pin header lends to
/// system services: GPIO 2–27, every header line but the HAT identification
/// EEPROM's bus on GPIO 0 and 1, which the firmware owns.
const PI_HEADER_FIRST: u8 = 2;
const PI_HEADER_LAST: u8 = 27;

/// The BCM2711 AVS monitor, whose register block holds the temperature
/// sensor's.
const AVS_MONITOR_COMPATIBLE: &[u8] = b"brcm,bcm2711-avs-monitor";

/// The temperature sensor inside the AVS monitor, a node with no window of
/// its own.
const AVS_THERMAL_COMPATIBLE: &[u8] = b"brcm,bcm2711-thermal";

/// The Broadcom DMA binding's channel mask, which numbers channels across the
/// whole DMA block rather than from the node's own first channel.
const BRCM_DMA_CHANNEL_MASK: &str = "brcm,dma-channel-mask";

/// The Broadcom DMA block's channel register stride, and the page its
/// channels share: a node's first channel is its window's offset into that
/// page over the stride.
const BRCM_DMA_CHANNEL_STRIDE: u64 = 0x100;
const BRCM_DMA_PAGE: u64 = 0x1000;

/// This port's half of the shared device-tree walk: the GIC interrupt
/// specifier and the BCM2711 board augmentation.
pub struct Aarch64Fdt {
    /// The GIC's phandle, when the tree gives it one.
    gic: Option<u32>,
    /// Whether the tree describes a Raspberry Pi, whose header the chip's GPIO
    /// block leads.
    raspberry_pi: bool,
    /// The enabled temperature sensor directly inside an enabled AVS
    /// monitor, which takes the monitor's window.
    avs_sensor: Option<AvsSensor>,
    /// Each display engine output the tree describes, in
    /// [`ENGINE_OUTPUTS`]' order.
    outputs: [Option<OutputWiring>; ENGINE_OUTPUTS.len()],
}

/// One display engine output, resolved against the tree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct OutputWiring {
    /// The pixel valve's translated register window.
    window: (u64, u64),
    /// The id the walk gives the encoder's node.
    encoder: u32,
}

/// Each display engine output whose pixel valve the tree describes — a valve
/// is never a node of its own, so its own status does not matter — and whose
/// encoder the walk emits.
fn engine_outputs(fdt: &Fdt<'_>) -> [Option<OutputWiring>; ENGINE_OUTPUTS.len()] {
    let mut windows = [None; ENGINE_OUTPUTS.len()];
    scan_translated(fdt, |node, levels, depth| {
        for (window, output) in windows.iter_mut().zip(&ENGINE_OUTPUTS) {
            if window.is_none() && node.is_compatible(output.valve) {
                *window = translated_reg(node, depth, levels, 0);
            }
        }
        None::<()>
    });
    let mut encoders = [None; ENGINE_OUTPUTS.len()];
    for (id, node, _) in emitted(fdt) {
        for (encoder, output) in encoders.iter_mut().zip(&ENGINE_OUTPUTS) {
            if encoder.is_none() && node.is_compatible(output.encoder) {
                *encoder = Some(id);
            }
        }
    }
    let mut outputs = [None; ENGINE_OUTPUTS.len()];
    for ((output, window), encoder) in outputs.iter_mut().zip(windows).zip(encoders) {
        *output = window
            .zip(encoder)
            .map(|(window, encoder)| OutputWiring { window, encoder });
    }
    outputs
}

/// Whether the tree's root names a Raspberry Pi board.
fn is_raspberry_pi(fdt: &Fdt<'_>) -> bool {
    let Some(Ok(root)) = fdt.nodes().next() else {
        return false;
    };
    root.property("compatible").is_some_and(|names| {
        names
            .iter_strings()
            .any(|name| name.starts_with(RASPBERRY_PI_VENDOR))
    })
}

/// A temperature sensor inside an AVS monitor: its node and the monitor's,
/// each by where it sits in the tree, and the monitor's translated window.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AvsSensor {
    node: usize,
    monitor: usize,
    window: (u64, u64),
}

/// The first enabled temperature sensor directly inside an enabled AVS
/// monitor.
fn avs_sensor(fdt: &Fdt<'_>) -> Option<AvsSensor> {
    let mut monitors = [None; MAX_WALK_DEPTH];
    scan_translated(fdt, |node, levels, depth| {
        if node.is_compatible(AVS_THERMAL_COMPATIBLE) && node.is_operational() {
            return depth
                .checked_sub(1)
                .and_then(|parent| monitors.get(parent).copied().flatten())
                .map(|(monitor, window)| AvsSensor {
                    node: node.offset(),
                    monitor,
                    window,
                });
        }
        if let Some(slot) = monitors.get_mut(depth) {
            *slot = if node.is_compatible(AVS_MONITOR_COMPATIBLE) && node.is_operational() {
                translated_reg(node, depth, levels, 0).map(|window| (node.offset(), window))
            } else {
                None
            };
        }
        None
    })
}

/// Whether `node` is one of the encoders the display engine feeds.
fn is_engine_encoder(node: &Node<'_>) -> bool {
    ENGINE_OUTPUTS
        .iter()
        .any(|output| node.is_compatible(output.encoder))
}

/// The id the walk gives the display engine's node, when the tree enables
/// one: the node the boot console moves to, since the engine takes the
/// screen over from the firmware.
#[must_use]
pub fn display_engine(fdt: &Fdt<'_>) -> Option<u32> {
    emitted(fdt)
        .find_map(|(id, node, _)| node.is_compatible(DISPLAY_ENGINE_COMPATIBLE).then_some(id))
}

impl Aarch64Fdt {
    /// The engine's outputs as its node carries them — each pixel valve's
    /// window and a link to the encoder it feeds — and the windows its
    /// scan-out reads memory through.
    fn describe_engine(
        &self,
        depth: usize,
        levels: &[BusLevel<'_>],
        coherence: DmaCoherence,
        hw: &mut HwNode,
    ) {
        for ((output, wiring), index) in ENGINE_OUTPUTS.iter().zip(self.outputs).zip(0u8..) {
            let Some(OutputWiring {
                window: (base, len),
                encoder,
            }) = wiring
            else {
                continue;
            };
            let endpoint = ENCODER_ENDPOINTS.endpoint(encoder);
            let (Some(window), Ok(link)) = (
                HwResource::mmio_named(base, len, output.window),
                LinkRequest::new(endpoint, index, &[], output.link),
            ) else {
                continue;
            };
            // A node with no room left simply describes fewer outputs; the
            // capacity bound is the ABI's.
            if hw.push_resource(window).is_err()
                || hw.push_resource(HwResource::request(&link)).is_err()
            {
                return;
            }
        }
        push_dma_windows(depth, levels, coherence, hw);
    }
}

impl FdtPlatform for Aarch64Fdt {
    /// The three-cell `<type, number, flags>` GIC binding both supported
    /// boards describe interrupts with.
    fn interrupt_cells(&self) -> usize {
        3
    }

    /// The GIC mapping is a pure function of the specifier's own cells; only
    /// the GIC's identity is read, to know which nodes it serves.
    fn from_tree(fdt: &Fdt<'_>) -> Self {
        Self {
            gic: crate::gic::find_gic(fdt).and_then(|gic| gic.phandle),
            raspberry_pi: is_raspberry_pi(fdt),
            avs_sensor: avs_sensor(fdt),
            outputs: engine_outputs(fdt),
        }
    }

    fn root_interrupt_controller(&self) -> Option<u32> {
        self.gic
    }

    fn kernel_driven(&self, node: &Node<'_>) -> bool {
        node.property("compatible").is_some_and(|names| {
            names.iter_strings().any(|name| {
                crate::gic::is_gic_compatible(name) || name == crate::gic::ITS_COMPATIBLE
            })
        })
    }

    /// The SD host the board's tree names `emmc2@…`, outside the generic
    /// stems, is a storage device.
    fn class(&self, node: &Node<'_>) -> Option<HwDeviceClass> {
        node.is_compatible(SD_HOST_COMPATIBLE)
            .then_some(HwDeviceClass::Storage)
    }

    /// The Broadcom binding's mask converted to the generic, node-relative
    /// numbering, masked to the channels the node's window covers; any other
    /// node reads the generic property.
    fn dma_channel_mask(
        &self,
        node: &Node<'_>,
        depth: usize,
        levels: &[BusLevel<'_>],
    ) -> Option<u64> {
        let Some(vendor) = node.property(BRCM_DMA_CHANNEL_MASK) else {
            return tairix_arch_api::fdtwalk::dma_channel_mask(node);
        };
        if vendor.value().len() != 4 {
            return None;
        }
        let absolute = u64::from(vendor.read_be_u32(0).ok()?);
        let (base, len) = translated_reg(node, depth, levels, 0)?;
        let first = (base % BRCM_DMA_PAGE) / BRCM_DMA_CHANNEL_STRIDE;
        let covered = match len / BRCM_DMA_CHANNEL_STRIDE {
            count @ 0..=63 => (1u64 << count) - 1,
            _ => u64::MAX,
        };
        Some((absolute >> first) & covered)
    }

    /// Map a GIC specifier to the global INTID `irq_bind` and the GIC
    /// routing use.
    ///
    /// The `number` cell is relative to its `type`, so emitting it raw would
    /// hand an autoloaded driver a line 32 below the one its device raises.
    /// The mapping is the one shared decoder the boot path's own
    /// `gic_device_intid` reads, and a type this GICv2 port cannot represent
    /// (a GICv3-only extended-SPI binding) maps to nothing rather than a
    /// guessed line.
    fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
        let kind = u32::try_from(read_cells(specifier, 0, 1)?).ok()?;
        let number = u32::try_from(read_cells(specifier, 4, 1)?).ok()?;
        gic_intid_from_cells(kind, number)
    }

    /// The GIC binding's flags cell: a rising or falling edge in its low two
    /// bits, a level in the next two.
    fn edge_triggered(&self, specifier: &[u8]) -> bool {
        read_cells(specifier, 8, 1).is_some_and(|flags| flags & 0b11 != 0)
    }

    // An Arm tree states `dma-coherent` on every master that snoops.
    const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Unsnooped;

    fn grants_window(&self, node: &Node<'_>, name: Option<&[u8]>) -> bool {
        let engine_owned = name == Some(HDMI_INTERRUPT_WINDOW) && is_engine_encoder(node);
        let sensor_owned = self
            .avs_sensor
            .is_some_and(|sensor| sensor.monitor == node.offset());
        !engine_owned && !sensor_owned
    }

    fn augment(
        &self,
        fdt: &Fdt<'_>,
        node: &Node<'_>,
        depth: usize,
        levels: &[BusLevel<'_>],
        coherence: DmaCoherence,
        hw: &mut HwNode,
    ) {
        let Some(compatible) = node.property("compatible") else {
            return;
        };
        let names_compatible = |wanted: &[u8]| compatible.iter_strings().any(|s| s == wanted);

        // The VideoCore firmware mailbox additionally *requests* a DMA
        // property-buffer carve inside the 30-bit aperture (`plans/PI.md`
        // P7) — a capability-grant request the driver host satisfies, declared here because only the platform
        // knows the firmware's aperture.
        if names_compatible(MAILBOX_COMPATIBLE) {
            let dma = HwResource::dma(APERTURE_LIMIT, MAILBOX_DMA_BUFFER_LEN, coherence);
            // A node with no room left simply carries no carve request; the
            // capacity bound is the ABI's, never a panic.
            let _ = hw.push_resource(dma);
        }

        // A BCM2711 GENET MAC masters DMA to ordinary system memory but its own
        // node declares no reach: Devicetree Spec v0.4 §2.3.9 puts `dma-ranges`
        // on the *bus*, so the constraint is read from the parent bus level (the
        // Pi 4's `/scb`) — a discovered value, never a board constant. Declared
        // here because only the platform's tree knows the aperture.
        if names_compatible(GENET_COMPATIBLE) {
            if let Some((aperture_top, aperture_len)) = parent_dma_aperture(depth, levels) {
                // No room left simply carries no carve request; the capacity
                // bound is the ABI's, never a panic.
                let _ = hw.push_resource(HwResource::dma(aperture_top, aperture_len, coherence));
            }
        }
        // The SD host masters DMA through its bus's `dma-ranges`, rewritten per
        // `SoC` stepping, and its card's supplies are lines the firmware switches.
        if names_compatible(SD_HOST_COMPATIBLE) {
            push_dma_windows(depth, levels, coherence, hw);
            push_sd_rails(fdt, node, hw);
        }
        if self.raspberry_pi && names_compatible(crate::uart_init::GPIO_COMPATIBLE) {
            let lines = HeaderLines::span(PI_HEADER_FIRST, PI_HEADER_LAST);
            let _ = hw.push_resource(HwResource::property(
                HwProperty::GpioHeaderLines,
                lines.mask(),
            ));
        }
        if let Some(AvsSensor {
            window: (base, len),
            ..
        }) = self
            .avs_sensor
            .filter(|sensor| sensor.node == node.offset())
        {
            let _ = hw.push_resource(HwResource::mmio(base, len));
        }
        if names_compatible(DISPLAY_ENGINE_COMPATIBLE) {
            self.describe_engine(depth, levels, coherence, hw);
        }
        if is_engine_encoder(node) {
            if let Ok(duty) = LinkDuty::new(ENCODER_ENDPOINTS.endpoint(hw.id()), None) {
                let _ = hw.push_resource(HwResource::duty(&duty));
            }
        }
        // The BCM2711 PCIe host bridge additionally *requests* the
        // inbound-DMA aperture it grants devices behind it (`plans/PI.md`
        // P10): the CPU-physical window read from its `dma-ranges`, with the
        // node's own `#address`/`#size`-cells decoding the child PCI triple
        // and its parent bus's `#address-cells` the CPU base. Declared here
        // because only the platform's tree knows the aperture; the VL805 wiring carves its xHCI DMA region below the top.
        if names_compatible(PCIE_COMPATIBLE) {
            let level = bus_level(node);
            let parent_addr_cells = depth
                .checked_sub(1)
                .and_then(|i| levels.get(i))
                .map_or(2, |l| l.addr_cells);
            if let Some((aperture_top, aperture_len, inbound_pcie_base)) =
                dma_ranges_aperture(node, level.addr_cells, parent_addr_cells, level.size_cells)
            {
                // No room left simply carries no aperture request; the
                // capacity bound is the ABI's, never a panic. An
                // unreadable `dma-ranges` likewise omits it rather than
                // inventing a window. The far-side PCIe base the inbound
                // viewport starts at rides the resource's translation field
                // so the VL805 wiring can program the inbound BAR from the
                // tree, never a board constant.
                let _ = hw.push_resource(HwResource::dma_translated(
                    aperture_top,
                    aperture_len,
                    inbound_pcie_base,
                    coherence,
                ));
            }
            // …and the outbound memory window from its `ranges`: the
            // CPU-physical aperture the bridge forwards to PCIe memory space
            // and the PCIe-space base it maps to, so the VL805 wiring can
            // both program the root complex's outbound window and translate
            // the enumerated BAR back to a CPU-physical address. Carried as a
            // single `BusWindow` (CPU base, size, far-side PCIe base) rather
            // than conflated with the controller's own `reg` MMIO windows. An unreadable `ranges` omits it rather
            // than inventing a window.
            if let Some((cpu_base, pcie_base, size)) =
                outbound_mmio_window(node, level.addr_cells, parent_addr_cells, level.size_cells)
            {
                let _ = hw.push_resource(HwResource::bus_window(cpu_base, size, pcie_base));
            }
        }
    }
}

/// State the rails the SD host `node`'s supplies name, both or neither.
fn push_sd_rails(fdt: &Fdt<'_>, node: &Node<'_>, hw: &mut HwNode) {
    let Some((io, power)) = crate::sd_supply::sd_rails(fdt, node) else {
        return;
    };
    // A power rail whose delays its record cannot carry is no rail, and a
    // node without room for the pair carries neither.
    let Some(power) = HwResource::power_rail(power) else {
        return;
    };
    if hw.resources().len() + 2 <= HW_NODE_MAX_RESOURCES {
        let _ = hw.push_resource(HwResource::io_rail(io));
        let _ = hw.push_resource(power);
    }
}

/// The [`PlatformDiscovery`](tairix_arch_api::PlatformDiscovery)
/// implementation the boot path constructs: the shared walk over this
/// port's [`Aarch64Fdt`].
pub type FdtDiscovery<'a> = tairix_arch_api::fdtwalk::FdtDiscovery<'a, Aarch64Fdt>;

/// The BCM2711 PCIe root-complex address windows the boot path types Device
/// and points the root complex's MSI controller at (`plans/PI.md` P10), read
/// from the `brcm,bcm2711-pcie` node — never compiled-in.
///
/// The same three windows the [`FdtDiscovery`] walk emits on the bridge's
/// hardware-tree node (the controller `reg`, the inbound `dma-ranges`
/// aperture, the outbound `ranges` window), but resolved by a single
/// early-returning [`scan_translated`] walk so the boot path can read them
/// **before the MMU is enabled** — early enough to fold the controller and
/// outbound-window gigapages into the identity map's Device mask
/// (`plans/PI.md` P4/P5 MMU-off watch-out; a whole-tree scan faults there).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PcieDiscovery {
    /// CPU-physical base of the PCIe controller register block (the
    /// translated `reg[0]`).
    pub regs_phys: u64,
    /// Byte length of the controller register block.
    pub regs_len: u64,
    /// Exclusive upper bound of the CPU-physical window devices behind the
    /// bridge may DMA through (the inbound aperture's top): a DMA carve
    /// must lie wholly below it.
    pub dma_aperture_top: u64,
    /// Byte length of the inbound aperture.
    pub inbound_size: u64,
    /// PCIe-space base the inbound viewport is programmed at.
    pub inbound_pcie_base: u64,
    /// CPU-physical base of the outbound MMIO window (the bridge forwards
    /// it to PCIe memory space — the enumerated BARs live here).
    pub outbound_cpu_base: u64,
    /// PCIe-space base the outbound window maps to.
    pub outbound_pcie_base: u64,
    /// Byte length of the outbound MMIO window.
    pub outbound_size: u64,
}

/// The GIC INTID the [`PCIE_COMPATIBLE`] root complex's internal MSI
/// controller raises: the node's second `interrupts` specifier (the first is
/// the root complex's own). [`None`] without such a node, a second
/// specifier, or a GIC line this port routes.
#[must_use]
pub fn pcie_msi_intid(fdt: &Fdt<'_>) -> Option<u32> {
    let node = fdt.find_compatible(PCIE_COMPATIBLE)?;
    let interrupts = node.property("interrupts")?;
    let kind = interrupts.read_be_u32(12).ok()?;
    let number = interrupts.read_be_u32(16).ok()?;
    gic_intid_from_cells(kind, number)
}

/// Discover the BCM2711 PCIe root complex's three address windows from the
/// firmware device tree at `fdt` (`plans/PI.md` P10).
///
/// Uses the early-returning [`scan_translated`] walk, reading only the
/// matched `brcm,bcm2711-pcie` node's own `reg`/`ranges`/`dma-ranges`
/// against its ancestors' cell counts, so it is safe to call **with the
/// MMU still off** (`plans/PI.md` P4/P5 watch-out — a whole-tree scan
/// widens the byte loads and faults there).
///
/// Returns `None` when the tree describes no `brcm,bcm2711-pcie` node, or
/// when any of the three windows is absent or undecodable — fail closed, never a board constant. The QEMU `virt`
/// tree carries no such node, so it simply yields `None` and the keyboard
/// bring-up is skipped.
#[must_use]
pub fn pcie_bringup(fdt: &Fdt<'_>) -> Option<PcieDiscovery> {
    scan_translated(fdt, |node, levels, depth| {
        let compatible = node.property("compatible")?;
        if !compatible.iter_strings().any(|s| s == PCIE_COMPATIBLE) {
            // Not the PCIe bridge: keep walking.
            return None;
        }
        let level = bus_level(node);
        let parent_addr_cells = depth
            .checked_sub(1)
            .and_then(|i| levels.get(i))
            .map_or(2, |l| l.addr_cells);
        // The controller register block (first `reg` entry), translated
        // through the ancestor buses' `ranges` (untranslatable → `None`,
        // never read raw — the real Pi tree is bus-nested under `/scb`).
        let (regs_phys, regs_len) = translated_reg(node, depth, levels, 0)?;
        // The inbound-DMA aperture from `dma-ranges`.
        let (dma_aperture_top, inbound_size, inbound_pcie_base) =
            dma_ranges_aperture(node, level.addr_cells, parent_addr_cells, level.size_cells)?;
        // The outbound MMIO window from `ranges`.
        let (outbound_cpu_base, outbound_pcie_base, outbound_size) =
            outbound_mmio_window(node, level.addr_cells, parent_addr_cells, level.size_cells)?;
        Some(PcieDiscovery {
            regs_phys,
            regs_len,
            dma_aperture_top,
            inbound_size,
            inbound_pcie_base,
            outbound_cpu_base,
            outbound_pcie_base,
            outbound_size,
        })
    })
}

/// The DMA aperture a node's **parent bus** grants the devices on it, as
/// `(exclusive top, extent)`, or [`None`] when no ancestor level declares
/// one.
///
/// A mastering device's reach is a property of the bus it sits on
/// (Devicetree Spec v0.4 §2.3.9), so the window is decoded from the parent
/// [`BusLevel`]'s `dma-ranges` against that bus's own cell counts and its
/// parent's address width — exactly as [`dma_ranges_aperture`] decodes a
/// bridge's own property.
fn parent_dma_aperture(depth: usize, levels: &[BusLevel<'_>]) -> Option<(u64, u64)> {
    let parent = levels.get(depth.checked_sub(1)?)?;
    let grandparent_addr_cells = depth
        .checked_sub(2)
        .and_then(|i| levels.get(i))
        .map_or(2, |l| l.addr_cells);
    let (top, len, _) = dma_ranges_aperture_of(
        parent.dma_ranges?,
        parent.addr_cells,
        grandparent_addr_cells,
        parent.size_cells,
    )?;
    Some((top, len))
}

#[cfg(test)]
mod tests {
    use super::{display_engine, pcie_bringup, pcie_msi_intid, FdtDiscovery, PCIE_COMPATIBLE};
    use crate::fdt::{gic_intid_from_cells, Fdt, GIC_PPI_INTID_BASE};
    use crate::gic::MIN_SPI_INTID;
    use crate::raspi_fixture as fixture;
    use crate::raspi_fixture::raspi_like_arm;
    use std::vec::Vec;
    use tairix_abi::driver::supply::{IoRail, PowerRail};
    use tairix_abi::{HwDeviceClass, HwNode, HwResource, HwResourceKind, HW_NODE_MAX_RESOURCES};
    use tairix_arch_api::platform::{conformance, DiscoveryError, HwNodeSink, PlatformDiscovery};
    use tairix_fdt::fixture::{arm_with_cpus, virt_like_arm};
    use tairix_fdt::write::FdtWriter;
    use tairix_vcmailbox::SD_HOST_COMPATIBLE;

    #[test]
    fn passes_platform_discovery_conformance() {
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let disco = FdtDiscovery::new(fdt);
        conformance::run(&disco);
    }

    #[derive(Default)]
    struct CollectingSink {
        nodes: Vec<HwNode>,
    }

    impl HwNodeSink for CollectingSink {
        fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
            self.nodes.push(node);
            Ok(())
        }
    }

    fn discover_all(blob: &[u8]) -> Vec<HwNode> {
        let fdt = Fdt::new(blob).expect("valid fdt");
        let mut sink = CollectingSink::default();
        FdtDiscovery::new(fdt)
            .discover(&mut sink)
            .expect("discovery succeeds");
        sink.nodes
    }

    /// The node carrying `compatible` among its match keys.
    fn by_key<'a>(nodes: &'a [HwNode], compatible: &[u8]) -> &'a HwNode {
        nodes
            .iter()
            .find(|n| {
                n.match_keys()
                    .iter()
                    .any(|k| k.compatible_bytes() == compatible)
            })
            .expect("node with the key is emitted")
    }

    fn mmio_windows(node: &HwNode) -> Vec<(u64, u64)> {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Mmio))
            .map(|r| (r.base(), r.length()))
            .collect()
    }

    /// A Pi-shaped tree: the board named at the root, the GPIO block, and the
    /// AVS monitor holding the temperature sensor its thermal zone reads;
    /// with `stray`, a second node naming the sensor sits beside the monitor.
    fn pi_cooling_tree(root_compatible: &[u8], stray: bool) -> Vec<u8> {
        let cells =
            |values: &[i32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop("compatible", root_compatible);
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop("ranges", &cells(&[0x7c00_0000, -0x0400_0000, 0x0400_0000]));
        b.begin_node("gpio@7e200000");
        b.prop_str("compatible", "brcm,bcm2711-gpio");
        b.prop("reg", &cells(&[0x7e20_0000, 0xb4]));
        b.end_node();
        if stray {
            b.begin_node("thermal");
            b.prop_str("compatible", "brcm,bcm2711-thermal");
            b.end_node();
        }
        b.begin_node("avs-monitor@7d5d2000");
        b.prop(
            "compatible",
            b"brcm,bcm2711-avs-monitor\0syscon\0simple-mfd\0",
        );
        b.prop("reg", &cells(&[0x7d5d_2000, 0xf00]));
        b.begin_node("thermal");
        b.prop_str("compatible", "brcm,bcm2711-thermal");
        b.prop_u32("#thermal-sensor-cells", 0);
        b.prop_u32("phandle", 9);
        b.end_node();
        b.end_node();
        b.end_node();
        b.begin_node("thermal-zones");
        b.begin_node("cpu-thermal");
        b.prop("thermal-sensors", &cells(&[9]));
        b.prop("coefficients", &cells(&[-487, 410_040]));
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn a_pi_s_gpio_block_lends_its_header_lines_and_another_board_s_lends_none() {
        use tairix_abi::driver::gpio::HeaderLines;
        use tairix_abi::HwProperty;

        let pi = discover_all(&pi_cooling_tree(
            b"raspberrypi,4-model-b\0brcm,bcm2711\0",
            false,
        ));
        let lines = by_key(&pi, b"brcm,bcm2711-gpio").property(HwProperty::GpioHeaderLines);
        assert_eq!(lines, Some(HeaderLines::span(2, 27).mask()));
        let other = discover_all(&pi_cooling_tree(b"acme,board\0brcm,bcm2711\0", false));
        assert_eq!(
            by_key(&other, b"brcm,bcm2711-gpio").property(HwProperty::GpioHeaderLines),
            None
        );
    }

    #[test]
    fn the_avs_sensor_takes_its_monitor_s_window_and_its_zone_s_calibration() {
        use tairix_abi::driver::thermal::ThermalCalibration;

        let nodes = discover_all(&pi_cooling_tree(b"raspberrypi,4-model-b\0", false));
        let sensor = by_key(&nodes, b"brcm,bcm2711-thermal");
        assert_eq!(mmio_windows(sensor), [(0xfd5d_2000, 0xf00)]);
        assert_eq!(
            sensor.thermal_calibration(),
            Some(ThermalCalibration {
                slope: -487,
                offset: 410_040
            })
        );
        let monitor = by_key(&nodes, b"brcm,bcm2711-avs-monitor");
        assert!(
            mmio_windows(monitor).is_empty(),
            "one node holds the registers"
        );
        assert_eq!(
            sensor.property(tairix_abi::HwProperty::ThermalZone),
            Some(tairix_abi::driver::thermal::ThermalZone::Processor.as_u64()),
            "the processor's zone reads it"
        );
    }

    #[test]
    fn only_the_sensor_inside_the_monitor_takes_its_window() {
        let nodes = discover_all(&pi_cooling_tree(b"raspberrypi,4-model-b\0", true));
        let sensors: Vec<&HwNode> = nodes
            .iter()
            .filter(|n| {
                n.match_keys()
                    .iter()
                    .any(|k| k.compatible_bytes() == b"brcm,bcm2711-thermal")
            })
            .collect();
        assert_eq!(sensors.len(), 2);
        assert!(
            mmio_windows(sensors[0]).is_empty(),
            "a node beside the monitor takes nothing"
        );
        assert_eq!(mmio_windows(sensors[1]), [(0xfd5d_2000, 0xf00)]);
    }

    #[test]
    fn emits_every_described_device_from_a_virt_tree() {
        let nodes = discover_all(&virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30));
        // root + psci + intc + timer + memory — every `compatible`-
        // carrying node plus the memory node, nothing invented.
        assert_eq!(nodes.len(), 5);
        assert!(nodes[0].is_root());

        // `/psci` is firmware, not hardware the port special-cases: the
        // generic walk still surfaces it (unbound is fine).
        let psci = by_key(&nodes, b"arm,psci-1.0");
        assert_eq!(psci.class(), Some(HwDeviceClass::Other));
        assert!(psci.resources().is_empty());

        // The GICv2 (`intc@…` — the `virt` board's name) carries both
        // register windows with their *tree-declared* lengths.
        let gic = by_key(&nodes, b"arm,cortex-a15-gic");
        assert_eq!(gic.class(), Some(HwDeviceClass::InterruptController));
        assert_eq!(
            mmio_windows(gic),
            [(0x0800_0000, 0x1_0000), (0x0801_0000, 0x1_0000)]
        );
        // The kernel drives the GIC, so no driver may be loaded for it.
        assert!(gic.is_kernel_driven());
        assert!(nodes.iter().filter(|n| n.is_kernel_driven()).eq([gic]));

        // The generic timer carries its PPI from `interrupts` and its
        // own `compatible` as the bind key.
        let timer = by_key(&nodes, b"arm,armv8-timer");
        assert_eq!(timer.class(), Some(HwDeviceClass::Timer));
        let irq = timer.resources().first().expect("timer irq resource");
        assert_eq!(irq.kind(), Some(HwResourceKind::Irq));
        assert_eq!(
            irq.base(),
            u64::from(GIC_PPI_INTID_BASE + 30),
            "the timer's PPI is emitted as its global GICv2 INTID"
        );

        // `/memory` has no `compatible`; `device_type` classifies it.
        let memory = nodes
            .iter()
            .find(|n| n.class() == Some(HwDeviceClass::Memory))
            .expect("memory node emitted");
        assert!(memory.match_keys().is_empty());
        assert_eq!(mmio_windows(memory), [(0x4000_0000, 0x2000_0000)]);

        // Every device sits directly under the root.
        assert!(nodes[1..].iter().all(|n| n.parent() == 0));
    }

    #[test]
    fn a_gicv3_and_its_translation_service_are_the_kernel_s_and_a_gpio_controller_is_not() {
        let cells =
            |values: &[u64]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = tairix_fdt::write::FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("intc@8000000");
        b.prop_str("compatible", "arm,gic-v3");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 3);
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.prop("ranges", &[]);
        b.prop(
            "reg",
            &cells(&[0x0800_0000, 0x1_0000, 0x080A_0000, 0xF6_0000]),
        );
        b.begin_node("its@8080000");
        b.prop_str("compatible", "arm,gic-v3-its");
        b.prop("msi-controller", &[]);
        b.prop("reg", &cells(&[0x0808_0000, 0x2_0000]));
        b.end_node();
        b.end_node();
        b.begin_node("gpio@fe200000");
        b.prop_str("compatible", "brcm,bcm2711-gpio");
        b.prop("interrupt-controller", &[]);
        b.prop("reg", &cells(&[0xFE20_0000, 0x1000]));
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());
        assert!(by_key(&nodes, b"arm,gic-v3").is_kernel_driven());
        assert!(by_key(&nodes, b"arm,gic-v3-its").is_kernel_driven());
        let gpio = by_key(&nodes, b"brcm,bcm2711-gpio");
        assert_eq!(gpio.class(), Some(HwDeviceClass::InterruptController));
        assert!(!gpio.is_kernel_driven());
    }

    /// The generic ECAM host is the kernel's, which owns every function's
    /// configuration space below it, so its window is never a driver's.
    #[test]
    fn a_generic_ecam_host_is_the_kernel_s() {
        let cells =
            |values: &[u64]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = tairix_fdt::write::FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("pcie@4010000000");
        b.prop_str("compatible", "pci-host-ecam-generic");
        b.prop_str("device_type", "pci");
        b.prop("reg", &cells(&[0x40_1000_0000, 0x1000_0000]));
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());
        assert!(by_key(&nodes, b"pci-host-ecam-generic").is_kernel_driven());
    }

    #[test]
    fn emits_every_described_device_from_a_raspi_tree() {
        let nodes = discover_all(&raspi_like_arm(0x7e20_1000, 0x7e21_5040));
        // root + psci + soc + gic + mailbox + gpio + pl011 + mini-uart +
        // scb + ethernet + mdio + memory: the generic walk emits *both*
        // UARTs — preferring one is console policy (`crate::console`), not
        // tree shape — and each simple-bus is itself a bindable bus node.
        assert_eq!(nodes.len(), 12);

        // Every `/soc` child names the emitted bus node as its parent. Both
        // `/soc` and `/scb` are `simple-bus`; the walk emits `/soc` first.
        let soc = by_key(&nodes, b"simple-bus");
        assert_eq!(soc.class(), Some(HwDeviceClass::Bus));

        // The Pi 4's GIC-400 `reg` carries the real tree's four one-cell
        // bus regions (GICD/GICC/GICH/GICV), each translated through the
        // `/soc` `ranges` to its BCM2711 CPU-physical window.
        let gic = by_key(&nodes, b"arm,gic-400");
        assert_eq!(gic.class(), Some(HwDeviceClass::InterruptController));
        assert_eq!(gic.parent(), soc.id());
        assert_eq!(
            mmio_windows(gic),
            [
                (0xff84_1000, 0x1000),
                (0xff84_2000, 0x2000),
                (0xff84_4000, 0x2000),
                (0xff84_6000, 0x2000)
            ]
        );

        // The VideoCore mailbox carries the discovered (translated)
        // doorbell window, the inbox interrupt its service parks on as a
        // global INTID, and the DMA property-buffer carve request bounded by
        // the 30-bit aperture (`plans/PI.md` P7).
        let mailbox = by_key(&nodes, b"brcm,bcm2835-mbox");
        assert_eq!(mailbox.class(), Some(HwDeviceClass::Other));
        assert_eq!(mmio_windows(mailbox), [(0xfe00_b880, 0x40)]);
        assert_eq!(
            irq_lines(mailbox),
            [u64::from(MIN_SPI_INTID + fixture::MAILBOX_SPI)]
        );
        let dma = mailbox.resources().get(2).expect("dma carve request");
        assert_eq!(dma.kind(), Some(HwResourceKind::Dma));
        assert_eq!(dma.base(), 0x4000_0000, "VC aperture limit");
        assert_eq!(dma.length(), 4096, "one-page property carve");

        // The BCM2711 GPIO controller (the pin-mux block
        // `uart_init::find_gpio` binds) carries its translated window
        // with the tree-declared length.
        let gpio = by_key(&nodes, b"brcm,bcm2711-gpio");
        assert_eq!(gpio.parent(), soc.id());
        assert_eq!(mmio_windows(gpio), [(0xfe20_0000, 0xb4)]);

        // Both UARTs are serial-class nodes with their bus `reg` values
        // translated to the CPU-physical bases.
        let pl011 = by_key(&nodes, b"arm,pl011");
        assert_eq!(pl011.class(), Some(HwDeviceClass::Serial));
        assert_eq!(mmio_windows(pl011), [(0xfe20_1000, 0x200)]);
        let mini = by_key(&nodes, b"brcm,bcm2835-aux-uart");
        assert_eq!(mini.class(), Some(HwDeviceClass::Serial));
        assert_eq!(mmio_windows(mini), [(0xfe21_5040, 0x40)]);

        let memory = nodes
            .iter()
            .find(|n| n.class() == Some(HwDeviceClass::Memory))
            .expect("memory node emitted");
        assert_eq!(mmio_windows(memory), [(0, 0x4000_0000)]);
    }

    #[test]
    fn the_genet_node_carries_its_window_intids_dma_reach_and_board_mac() {
        let nodes = discover_all(&raspi_like_arm(0x7e20_1000, 0x7e21_5040));
        let genet = by_key(&nodes, b"brcm,bcm2711-genet-v5");
        // A network device, at the CPU-physical base its `/scb` bus `ranges`
        // translates its `reg` to.
        assert_eq!(genet.class(), Some(HwDeviceClass::Network));
        assert_eq!(
            mmio_windows(genet),
            [(0xfd58_0000, u64::from(fixture::GENET_REGS_LEN))]
        );

        // Both `interrupts` entries are emitted as the *global* GICv2 INTIDs
        // a driver binds, not the type-relative device-tree cells.
        let irqs: Vec<u64> = genet
            .resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .map(HwResource::base)
            .collect();
        assert_eq!(
            irqs,
            [
                u64::from(MIN_SPI_INTID + fixture::GENET_SPI_A),
                u64::from(MIN_SPI_INTID + fixture::GENET_SPI_B)
            ]
        );

        // The MAC masters DMA but declares no reach of its own: the window
        // comes from its `/scb` bus's `dma-ranges`, a discovered value.
        let dma = genet
            .resources()
            .iter()
            .find(|r| r.kind() == Some(HwResourceKind::Dma))
            .expect("dma constraint request");
        assert_eq!(dma.base(), u64::from(fixture::SCB_DMA_APERTURE_TOP));
        assert_eq!(dma.length(), u64::from(fixture::SCB_DMA_APERTURE_TOP));
        // Untranslated: a GENET buffer is reached at its CPU-physical base.
        assert_eq!(dma.translated_base(), 0);

        // The firmware-published board MAC rides on the node as a fact that
        // confers no authority, so the driver programs the address printed
        // on the board rather than inventing one.
        let mac = genet
            .resources()
            .iter()
            .find_map(HwResource::link_address_octets)
            .expect("link address");
        assert_eq!(mac, fixture::GENET_BOARD_MAC);

        // Its MDIO child is emitted too, and left unbound: the GENET driver
        // reaches the PHY through its own register window.
        let mdio = by_key(&nodes, b"brcm,genet-mdio-v5");
        assert_eq!(mdio.parent(), genet.id());
    }

    #[test]
    fn a_node_on_a_bus_without_dma_ranges_requests_no_dma() {
        // The QEMU `virt` tree declares no `dma-ranges` anywhere, so nothing
        // discovered from it carries a DMA constraint: a driver receives no
        // reach it was not granted.
        let nodes = discover_all(&virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30));
        assert!(nodes
            .iter()
            .flat_map(HwNode::resources)
            .all(|r| r.kind() != Some(HwResourceKind::Dma)));
    }

    #[test]
    fn an_unrepresentable_interrupt_specifier_is_dropped_not_guessed() {
        // A GICv3-only extended-SPI binding (type 2) is not a line this
        // GICv2 port can raise: the node is still emitted, carrying no IRQ
        // resource, rather than a line 32 below something real.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("widget");
        b.prop_str("compatible", "vendor,widget");
        let mut interrupts = Vec::new();
        for cell in [2u32, 7, 0x04] {
            interrupts.extend_from_slice(&cell.to_be_bytes());
        }
        b.prop("interrupts", &interrupts);
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());
        let widget = by_key(&nodes, b"vendor,widget");
        assert!(widget
            .resources()
            .iter()
            .all(|r| r.kind() != Some(HwResourceKind::Irq)));
    }

    #[test]
    fn a_spi_the_tree_says_is_edge_triggered_is_granted_as_one() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.prop_u32("interrupt-parent", 1);
        b.begin_node("intc@8000000");
        b.prop_str("compatible", "arm,cortex-a15-gic");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 3);
        let mut reg = Vec::new();
        for cell in [0u32, 0x800_0000, 0, 0x1_0000, 0, 0x801_0000, 0, 0x1_0000] {
            reg.extend_from_slice(&cell.to_be_bytes());
        }
        b.prop("reg", &reg);
        b.prop_u32("phandle", 1);
        b.end_node();
        b.begin_node("smmuv3@9050000");
        b.prop_str("compatible", "arm,smmu-v3");
        let mut interrupts = Vec::new();
        for cell in [0u32, 74, 1, 0, 75, 4] {
            interrupts.extend_from_slice(&cell.to_be_bytes());
        }
        b.prop("interrupts", &interrupts);
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());
        let lines: Vec<(u64, bool)> = by_key(&nodes, b"arm,smmu-v3")
            .resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .map(|r| (r.base(), r.is_edge_triggered()))
            .collect();
        assert_eq!(lines, [(74 + 32, true), (75 + 32, false)]);
    }

    /// A Pi-4-shaped nested tree: devices under a `simple-bus` `/soc`
    /// whose `ranges` maps the legacy bus addresses (`0x7e……`) to the
    /// BCM2711 ARM-physical window (`0xfe……`) — the real Pi DTB shape.
    fn nested_soc_tree(with_ranges: bool) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        if with_ranges {
            // <child=0x7e000000 parent=0x0_fe000000 size=0x01800000>
            let mut ranges = Vec::new();
            ranges.extend_from_slice(&0x7e00_0000u32.to_be_bytes());
            ranges.extend_from_slice(&0xfe00_0000u64.to_be_bytes());
            ranges.extend_from_slice(&0x0180_0000u32.to_be_bytes());
            b.prop("ranges", &ranges);
        }
        b.begin_node("emmc2@7e340000");
        b.prop_str("compatible", "brcm,bcm2711-emmc2");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x7e34_0000u32.to_be_bytes());
        reg.extend_from_slice(&0x100u32.to_be_bytes());
        b.prop("reg", &reg);
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn translates_nested_reg_through_the_bus_ranges() {
        let nodes = discover_all(&nested_soc_tree(true));
        assert_eq!(nodes.len(), 3, "root + soc + emmc2");

        let soc = by_key(&nodes, b"simple-bus");
        assert_eq!(soc.class(), Some(HwDeviceClass::Bus));
        assert_eq!(soc.parent(), 0);

        // The EMMC2 node — the P8 SD host — is emitted with no per-device
        // code, parented under the bus, its window translated into the
        // ARM-physical space.
        let emmc2 = by_key(&nodes, b"brcm,bcm2711-emmc2");
        assert_eq!(emmc2.class(), Some(HwDeviceClass::Storage));
        assert_eq!(emmc2.parent(), soc.id());
        assert_eq!(mmio_windows(emmc2), [(0xfe34_0000, 0x100)]);
    }

    /// A Pi-4-shaped tree with a `PCIe` host bridge under `/scb`, carrying
    /// the real BCM2711 `reg`, `ranges`, and `dma-ranges` shapes.
    fn scb_pcie_tree() -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("scb");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        // <child=0x7c000000 parent=0xfc000000 size=0x03800000>: so
        // 0x7d500000 translates to 0xfd500000.
        let mut ranges = Vec::new();
        ranges.extend_from_slice(&0x7c00_0000u64.to_be_bytes());
        ranges.extend_from_slice(&0xfc00_0000u64.to_be_bytes());
        ranges.extend_from_slice(&0x0380_0000u64.to_be_bytes());
        b.prop("ranges", &ranges);
        b.begin_node("pcie@7d500000");
        b.prop_str("compatible", "brcm,bcm2711-pcie");
        b.prop_str("device_type", "pci");
        b.prop_u32("#address-cells", 3);
        b.prop_u32("#size-cells", 2);
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x7d50_0000u64.to_be_bytes());
        reg.extend_from_slice(&0x9310u64.to_be_bytes());
        b.prop("reg", &reg);
        // Outbound `ranges`: <pci.hi=0x02000000 pci.mid=0 pci.lo=0xc0000000
        // cpu=0x6_00000000 size=0x40000000> — the Pi 4's high MMIO
        // aperture, CPU 0x6_0000_0000 mapped to PCIe 0xc000_0000, 1 GiB.
        let mut ranges = Vec::new();
        ranges.extend_from_slice(&0x0200_0000u32.to_be_bytes());
        ranges.extend_from_slice(&0u32.to_be_bytes());
        ranges.extend_from_slice(&0xc000_0000u32.to_be_bytes());
        ranges.extend_from_slice(&0x6_0000_0000u64.to_be_bytes());
        ranges.extend_from_slice(&0x4000_0000u64.to_be_bytes());
        b.prop("ranges", &ranges);
        // <pci.hi=0x02000000 pci.mid=0 pci.lo=0  cpu=0x0  size=0xc0000000>
        let mut dma_ranges = Vec::new();
        dma_ranges.extend_from_slice(&0x0200_0000u32.to_be_bytes());
        dma_ranges.extend_from_slice(&0u32.to_be_bytes());
        dma_ranges.extend_from_slice(&0u32.to_be_bytes());
        dma_ranges.extend_from_slice(&0u64.to_be_bytes());
        dma_ranges.extend_from_slice(&0xc000_0000u64.to_be_bytes());
        b.prop("dma-ranges", &dma_ranges);
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    fn dma_resources(node: &HwNode) -> Vec<(u64, u64, u64)> {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Dma))
            .map(|r| (r.base(), r.length(), r.translated_base()))
            .collect()
    }

    fn bus_windows(node: &HwNode) -> Vec<(u64, u64, u64)> {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::BusWindow))
            .map(|r| (r.base(), r.length(), r.translated_base()))
            .collect()
    }

    #[test]
    fn pcie_bringup_reads_all_three_windows_off_the_bridge() {
        // The pre-MMU bring-up discovery resolves the same three windows
        // the full `discover` walk emits on the bridge node, by a single
        // early-returning `scan_translated` pass (`plans/PI.md` P10).
        let blob = scb_pcie_tree();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let pcie = pcie_bringup(&fdt).expect("the pcie bridge is discovered");
        assert_eq!(pcie.regs_phys, 0xfd50_0000);
        assert_eq!(pcie.regs_len, 0x9310);
        assert_eq!(pcie.dma_aperture_top, 0xc000_0000);
        assert_eq!(pcie.inbound_size, 0xc000_0000);
        assert_eq!(pcie.inbound_pcie_base, 0);
        assert_eq!(pcie.outbound_cpu_base, 0x6_0000_0000);
        assert_eq!(pcie.outbound_pcie_base, 0xc000_0000);
        assert_eq!(pcie.outbound_size, 0x4000_0000);
    }

    #[test]
    fn pcie_bringup_is_none_when_no_bridge_is_present() {
        // The QEMU `virt`-shaped tree has no `brcm,bcm2711-pcie` node.
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(pcie_bringup(&fdt), None);
    }

    #[test]
    fn the_msi_controller_s_interrupt_is_the_root_complex_s_second() {
        let tree = |interrupts: &[u32]| {
            let mut b = FdtWriter::new();
            b.begin_node("");
            b.begin_node("pcie@7d500000");
            b.prop("compatible", &[PCIE_COMPATIBLE, b"\0"].concat());
            let cells: Vec<u8> = interrupts.iter().flat_map(|v| v.to_be_bytes()).collect();
            b.prop("interrupts", &cells);
            b.end_node();
            b.end_node();
            b.build()
        };
        let both = tree(&[0, 147, 4, 0, 148, 4]);
        assert_eq!(
            pcie_msi_intid(&Fdt::new(&both).expect("valid fdt")),
            gic_intid_from_cells(0, 148)
        );
        let one = tree(&[0, 147, 4]);
        assert_eq!(pcie_msi_intid(&Fdt::new(&one).expect("valid fdt")), None);
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30);
        assert_eq!(pcie_msi_intid(&Fdt::new(&blob).expect("valid fdt")), None);
    }

    #[test]
    fn emits_the_pcie_bridge_with_its_outbound_window() {
        let nodes = discover_all(&scb_pcie_tree());
        let pcie = by_key(&nodes, b"brcm,bcm2711-pcie");

        // The outbound `ranges` memory window: CPU 0x6_0000_0000 -> PCIe
        // 0xc000_0000, 1 GiB, carried as one `BusWindow` (CPU base, size,
        // far-side PCIe base) distinct from the controller `reg` MMIO.
        assert_eq!(
            bus_windows(pcie),
            [(0x6_0000_0000, 0x4000_0000, 0xc000_0000)]
        );
        let win = pcie
            .resources()
            .iter()
            .find(|r| r.kind() == Some(HwResourceKind::BusWindow))
            .expect("outbound window request");
        assert_eq!(
            win.required_capability(),
            Ok(Some(tairix_abi::CapabilityId::MMIO_MAP))
        );
    }

    #[test]
    fn pcie_bridge_without_ranges_carries_no_outbound_window() {
        // The without-`dma-ranges` fixture also carries no `ranges`: no
        // outbound window is invented.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("pcie@7d500000");
        b.prop_str("compatible", "brcm,bcm2711-pcie");
        b.prop_u32("#address-cells", 3);
        b.prop_u32("#size-cells", 2);
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());
        let pcie = by_key(&nodes, b"brcm,bcm2711-pcie");
        assert!(bus_windows(pcie).is_empty());
    }

    #[test]
    fn emits_the_pcie_bridge_with_its_inbound_dma_aperture() {
        let nodes = discover_all(&scb_pcie_tree());
        let scb = by_key(&nodes, b"simple-bus");

        // The PCIe root complex is a bus node, parented under `/scb`, its
        // controller `reg` translated through the bus `ranges` (the
        // ECAM/config-access window the VL805 wiring maps under
        // `CAP_MMIO_MAP`) — no per-device code on this path.
        let pcie = by_key(&nodes, b"brcm,bcm2711-pcie");
        assert_eq!(pcie.class(), Some(HwDeviceClass::Bus));
        assert_eq!(pcie.parent(), scb.id());
        assert_eq!(mmio_windows(pcie), [(0xfd50_0000, 0x9310)]);

        // The augmentation: the inbound-DMA aperture from `dma-ranges` —
        // the low 3 GiB of SDRAM devices behind the bridge may reach.
        // base is the exclusive top, and the far-side PCIe base (0 on the
        // Pi: memory is viewed at PCIe address 0) rides the translation
        // field.
        assert_eq!(dma_resources(pcie), [(0xc000_0000, 0xc000_0000, 0)]);
        let dma = pcie
            .resources()
            .iter()
            .find(|r| r.kind() == Some(HwResourceKind::Dma))
            .expect("aperture request");
        assert_eq!(
            dma.required_capability(),
            Ok(Some(tairix_abi::CapabilityId::MEM_DMA))
        );
    }

    #[test]
    fn pcie_bridge_without_dma_ranges_carries_no_aperture() {
        // Strip the `dma-ranges`: the bridge is still emitted with its
        // translated window, but no aperture is invented.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("pcie@7d500000");
        b.prop_str("compatible", "brcm,bcm2711-pcie");
        b.prop_u32("#address-cells", 3);
        b.prop_u32("#size-cells", 2);
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());
        let pcie = by_key(&nodes, b"brcm,bcm2711-pcie");
        assert!(dma_resources(pcie).is_empty());
    }

    #[test]
    fn drops_a_window_an_ancestor_cannot_translate() {
        // Without `ranges` the bus address cannot reach the CPU's space;
        // the node is still emitted (bindable) but carries no invented
        // window.
        let nodes = discover_all(&nested_soc_tree(false));
        let emmc2 = by_key(&nodes, b"brcm,bcm2711-emmc2");
        assert!(emmc2.resources().is_empty());
    }

    #[test]
    fn skips_nodes_no_driver_could_ever_bind() {
        // `/cpus` and its `cpu@*` children carry no `compatible` in this
        // fixture, and are not memory nodes: nothing can bind them, so
        // they are not emitted. Only root + memory remain.
        let blob = arm_with_cpus(0x4000_0000, 0x2000_0000, &[(0, Some(1024)), (1, None)]);
        let nodes = discover_all(&blob);
        assert_eq!(nodes.len(), 2, "root + memory");
        assert_eq!(nodes[1].class(), Some(HwDeviceClass::Memory));
    }

    #[test]
    fn skips_an_overlong_compatible_and_keeps_list_order() {
        let overlong = "x".repeat(65);
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        // A node whose only compatible cannot be represented is skipped
        // outright (no driver bind key could carry it either).
        b.begin_node("bogus");
        b.prop_str("compatible", &overlong);
        b.end_node();
        // A multi-string list is emitted in devicetree order (most
        // specific first), the unrepresentable entry dropped.
        b.begin_node("sdhci@0");
        let mut list = Vec::new();
        for s in ["brcm,bcm2711-emmc2", overlong.as_str(), "arasan,sdhci-5.1"] {
            list.extend_from_slice(s.as_bytes());
            list.push(0);
        }
        b.prop("compatible", &list);
        b.end_node();
        b.end_node();
        let nodes = discover_all(&b.build());

        assert_eq!(nodes.len(), 2, "root + the sdhci node");
        let sdhci = &nodes[1];
        assert_eq!(sdhci.class(), Some(HwDeviceClass::Storage));
        let keys: Vec<&[u8]> = sdhci
            .match_keys()
            .iter()
            .map(tairix_abi::HwMatchKey::compatible_bytes)
            .collect();
        assert_eq!(
            keys,
            [b"brcm,bcm2711-emmc2".as_slice(), b"arasan,sdhci-5.1"]
        );
    }

    #[test]
    fn refuses_a_tree_nested_beyond_the_walk_bound() {
        // 16 nested nodes exceed `MAX_WALK_DEPTH`; the walk fails closed
        // rather than silently under-enumerating.
        let mut b = FdtWriter::new();
        b.begin_node("");
        for _ in 0..16 {
            b.begin_node("nest");
        }
        for _ in 0..16 {
            b.end_node();
        }
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut sink = CollectingSink::default();
        assert_eq!(
            FdtDiscovery::new(fdt).discover(&mut sink),
            Err(DiscoveryError::MalformedSource)
        );
    }

    fn be_cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// The pinned Pi 4 tree's interrupt wiring: a GIC-400 the root names, and
    /// the `aon_intr` second-level controller both HDMI blocks name instead.
    fn nested_intc_tree() -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.prop_u32("interrupt-parent", 1);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop(
            "ranges",
            &be_cells(&[0x4000_0000, 0, 0xff80_0000, 0x0080_0000]),
        );
        b.begin_node("interrupt-controller@40041000");
        b.prop_str("compatible", "arm,gic-400");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 3);
        b.prop(
            "reg",
            &be_cells(&[0x4004_1000, 0x1000, 0x4004_2000, 0x2000]),
        );
        b.prop_u32("phandle", 1);
        b.end_node();
        b.begin_node("interrupt-controller@7ef00100");
        b.prop_str("compatible", "brcm,bcm2711-l2-intc");
        b.prop("interrupts", &be_cells(&[0, 0x60, 1]));
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", 0x2c);
        b.end_node();
        b.begin_node("hdmi@7ef00700");
        b.prop_str("compatible", "brcm,bcm2711-hdmi0");
        b.prop_u32("interrupt-parent", 0x2c);
        b.prop("interrupts", &be_cells(&[0, 1, 2, 3, 4, 5]));
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    fn irq_lines(node: &HwNode) -> Vec<u64> {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .map(HwResource::base)
            .collect()
    }

    /// The Pi 4's `/emmc2bus` holding the EMMC2 host, its `dma-ranges`
    /// mapping `size` bytes at bus address `bus` onto CPU address `cpu`.
    fn emmc2_tree(bus: u32, cpu: u32, size: u32) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.begin_node("emmc2bus");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.prop(
            "ranges",
            &be_cells(&[0, 0x7e00_0000, 0, 0xfe00_0000, 0x0180_0000]),
        );
        b.prop("dma-ranges", &be_cells(&[0, bus, 0, cpu, size]));
        b.begin_node("mmc@7e340000");
        b.prop_str("compatible", "brcm,bcm2711-emmc2");
        b.prop("reg", &be_cells(&[0, 0x7e34_0000, 0x100]));
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn the_emmc2_node_carries_the_dma_window_its_bus_translates_through() {
        // The tree the firmware leaves on a B0-stepping part: the host reaches
        // only the first GiB, at bus addresses 0xC000_0000 up.
        let nodes = discover_all(&emmc2_tree(0xc000_0000, 0, 0x4000_0000));
        let emmc2 = by_key(&nodes, SD_HOST_COMPATIBLE);
        assert_eq!(emmc2.class(), Some(HwDeviceClass::Storage));
        assert_eq!(mmio_windows(emmc2), [(0xfe34_0000, 0x100)]);
        assert_eq!(
            dma_resources(emmc2),
            [(0x4000_0000, 0x4000_0000, 0xc000_0000)],
            "CPU window [0, 1 GiB) seen by the host from bus 0xC000_0000"
        );
    }

    #[test]
    fn the_emmc2_window_follows_the_firmwares_rewrite_for_a_later_stepping() {
        // The firmware widens the window on a C0 part to the whole low bus,
        // untranslated; the node follows the tree rather than the stepping.
        let nodes = discover_all(&emmc2_tree(0, 0, 0xfc00_0000));
        let emmc2 = by_key(&nodes, SD_HOST_COMPATIBLE);
        assert_eq!(dma_resources(emmc2), [(0xfc00_0000, 0xfc00_0000, 0)]);
    }

    /// The firmware's GPIO expander's phandle in [`sd_supply_tree`].
    const EXPANDER: u32 = 0xb;

    /// A Pi-4-shaped SD supply wiring: the GPIO expander, as `controller`
    /// names it, and one SD host per `(io line, power line)` in `hosts`, each
    /// naming a `regulator-gpio` I/O rail and a `regulator-fixed` power rail
    /// of its own on those lines, the power rail carrying `card_extra`.
    fn sd_supply_tree(
        controller: &str,
        hosts: &[(u32, u32)],
        card_extra: Option<(&str, u32)>,
    ) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("gpio");
        b.prop_str("compatible", controller);
        b.prop_u32("#gpio-cells", 2);
        b.prop_u32("phandle", EXPANDER);
        b.end_node();
        for (index, &(io_line, power_line)) in (0u32..).zip(hosts) {
            let (io, power) = (0x40 + 2 * index, 0x41 + 2 * index);
            b.begin_node(&std::format!("regulator-sd-io@{index}"));
            b.prop_str("compatible", "regulator-gpio");
            b.prop("gpios", &be_cells(&[EXPANDER, io_line, 0]));
            b.prop("states", &be_cells(&[1_800_000, 1, 3_300_000, 0]));
            b.prop_u32("regulator-settling-time-us", 5000);
            b.prop_u32("phandle", io);
            b.end_node();
            b.begin_node(&std::format!("regulator-sd-vcc@{index}"));
            b.prop_str("compatible", "regulator-fixed");
            b.prop("enable-active-high", &[]);
            b.prop("gpio", &be_cells(&[EXPANDER, power_line, 0]));
            if let Some((name, value)) = card_extra {
                b.prop_u32(name, value);
            }
            b.prop_u32("phandle", power);
            b.end_node();
            b.begin_node(&std::format!("mmc@{index}"));
            b.prop_str("compatible", "brcm,bcm2711-emmc2");
            b.prop_u32("vqmmc-supply", io);
            b.prop_u32("vmmc-supply", power);
            b.end_node();
        }
        b.end_node();
        b.build()
    }

    /// Every SD host the walk emits, in tree order.
    fn sd_hosts(nodes: &[HwNode]) -> Vec<&HwNode> {
        nodes
            .iter()
            .filter(|n| {
                n.match_keys()
                    .iter()
                    .any(|k| k.compatible_bytes() == SD_HOST_COMPATIBLE)
            })
            .collect()
    }

    #[test]
    fn each_sd_host_carries_the_rails_its_own_supplies_name() {
        let nodes = discover_all(&sd_supply_tree(
            "raspberrypi,firmware-gpio",
            &[(4, 6), (2, 3)],
            Some(("off-on-delay-us", 3000)),
        ));
        let rails: Vec<_> = sd_hosts(&nodes)
            .iter()
            .map(|host| (host.io_rail(), host.power_rail()))
            .collect();
        let io = |line| IoRail {
            line,
            level_1v8: Some(true),
            level_3v3: Some(false),
            settle_us: 5000,
        };
        let power = |line| PowerRail {
            line,
            active_low: false,
            startup_us: 0,
            off_on_us: 3000,
        };
        assert_eq!(
            rails,
            [(Some(io(4)), Some(power(6))), (Some(io(2)), Some(power(3)))]
        );
    }

    #[test]
    fn a_rail_on_another_controller_states_none() {
        let nodes = discover_all(&sd_supply_tree("brcm,bcm2711-gpio", &[(4, 6)], None));
        let host = sd_hosts(&nodes)[0];
        assert_eq!((host.io_rail(), host.power_rail()), (None, None));
    }

    #[test]
    fn a_node_without_room_for_both_rails_states_neither() {
        let tree = sd_supply_tree("raspberrypi,firmware-gpio", &[(4, 6)], None);
        let fdt = Fdt::new(&tree).expect("valid fdt");
        let host = fdt.find_compatible(SD_HOST_COMPATIBLE).expect("an SD host");
        let mut roomy = HwNode::new(1, 0, HwDeviceClass::Storage);
        super::push_sd_rails(&fdt, &host, &mut roomy);
        assert!(roomy.io_rail().is_some() && roomy.power_rail().is_some());
        let mut cramped = HwNode::new(2, 0, HwDeviceClass::Storage);
        while cramped.resources().len() + 1 < HW_NODE_MAX_RESOURCES {
            cramped
                .push_resource(HwResource::mmio(0x1000, 0x100))
                .expect("room");
        }
        super::push_sd_rails(&fdt, &host, &mut cramped);
        assert_eq!((cramped.io_rail(), cramped.power_rail()), (None, None));
    }

    #[test]
    fn a_power_rail_kept_on_or_too_slow_for_its_record_states_none() {
        for extra in [
            ("regulator-always-on", 0),
            ("off-on-delay-us", PowerRail::MAX_DELAY_US + 1),
        ] {
            let nodes = discover_all(&sd_supply_tree(
                "raspberrypi,firmware-gpio",
                &[(4, 6)],
                Some(extra),
            ));
            let host = sd_hosts(&nodes)[0];
            assert_eq!(
                (host.io_rail(), host.power_rail()),
                (None, None),
                "{extra:?}"
            );
        }
    }

    #[test]
    fn a_node_wired_to_a_second_level_controller_gets_no_gic_line() {
        let nodes = discover_all(&nested_intc_tree());
        // Its one-cell specifiers read as GIC triples would have granted
        // SPI 1 — INTID 33, a line another device raises.
        assert!(irq_lines(by_key(&nodes, b"brcm,bcm2711-hdmi0")).is_empty());
        // The second-level controller's own line is the GIC's.
        assert_eq!(
            irq_lines(by_key(&nodes, b"brcm,bcm2711-l2-intc")),
            [u64::from(MIN_SPI_INTID + 0x60)]
        );
    }

    /// Both Pi 4 DMA controller nodes: the legacy block's first node at the
    /// start of the DMA page, and DMA4's four channels starting at channel
    /// eleven of the same page, each stating the Broadcom absolute mask.
    fn bcm_dma_tree(legacy_mask: &[u8]) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop(
            "ranges",
            &be_cells(&[0x7e00_0000, 0, 0xfe00_0000, 0x0180_0000]),
        );
        b.begin_node("dma-controller@7e007000");
        b.prop_str("compatible", "brcm,bcm2835-dma");
        b.prop("reg", &be_cells(&[0x7e00_7000, 0xb00]));
        b.prop_u32("#dma-cells", 1);
        b.prop("brcm,dma-channel-mask", legacy_mask);
        b.end_node();
        b.end_node();
        b.begin_node("scb");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.prop(
            "ranges",
            &be_cells(&[0, 0x7c00_0000, 0, 0xfc00_0000, 0, 0x0380_0000]),
        );
        b.begin_node("dma@7e007b00");
        b.prop_str("compatible", "brcm,bcm2711-dma");
        b.prop("reg", &be_cells(&[0, 0x7e00_7b00, 0, 0x400]));
        b.prop_u32("#dma-cells", 1);
        b.prop_u32("brcm,dma-channel-mask", 0x7000);
        b.end_node();
        b.begin_node("generic-dma");
        b.prop_str("compatible", "test,generic-dma");
        b.prop("reg", &be_cells(&[0, 0x7e01_0000, 0, 0x100]));
        b.prop_u32("#dma-cells", 1);
        b.prop_u32("dma-channel-mask", 0x3);
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    fn channel_mask(nodes: &[HwNode], compatible: &[u8]) -> Option<u64> {
        by_key(nodes, compatible)
            .resources()
            .iter()
            .find_map(|r| r.link_duty().ok())
            .expect("a controller duty")
            .channels()
    }

    #[test]
    fn the_broadcom_channel_mask_is_numbered_from_each_nodes_first_channel() {
        let nodes = discover_all(&bcm_dma_tree(&0x7f5u32.to_be_bytes()));
        // The legacy node starts at channel 0 and covers eleven channels.
        assert_eq!(channel_mask(&nodes, b"brcm,bcm2835-dma"), Some(0x7f5));
        // DMA4's node starts at channel 11: channels 12–14 are its 1–3.
        assert_eq!(channel_mask(&nodes, b"brcm,bcm2711-dma"), Some(0b1110));
        // A controller of any other binding reads the generic property.
        assert_eq!(channel_mask(&nodes, b"test,generic-dma"), Some(0x3));
    }

    #[test]
    fn a_broadcom_mask_outside_the_nodes_window_or_malformed_is_not_stated() {
        // Channel 15 lies past the legacy node's eleven.
        let nodes = discover_all(&bcm_dma_tree(&0x8000u32.to_be_bytes()));
        assert_eq!(channel_mask(&nodes, b"brcm,bcm2835-dma"), Some(0));
        let nodes = discover_all(&bcm_dma_tree(&[0, 0, 7]));
        assert_eq!(channel_mask(&nodes, b"brcm,bcm2835-dma"), None);
    }

    /// The display engine's corner of a Pi 4 tree, as the image's overlay
    /// leaves it: the HVS and both HDMI blocks enabled, their pixel valves
    /// still disabled, each block naming its interrupt controller's
    /// registers among its own.
    fn display_engine_tree(engine: &str) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.prop_u32("interrupt-parent", 1);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop(
            "ranges",
            &be_cells(&[0x7e00_0000, 0, 0xfe00_0000, 0x0180_0000]),
        );
        b.prop("dma-ranges", &be_cells(&[0xc000_0000, 0, 0, 0x4000_0000]));
        b.begin_node("interrupt-controller@40041000");
        b.prop_str("compatible", "arm,gic-400");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 3);
        b.prop(
            "reg",
            &be_cells(&[0x7e04_1000, 0x1000, 0x7e04_2000, 0x2000]),
        );
        b.prop_u32("phandle", 1);
        b.end_node();
        b.begin_node("interrupt-controller@7ef00100");
        b.prop_str("compatible", "brcm,bcm2711-l2-intc");
        b.prop("reg", &be_cells(&[0x7ef0_0100, 0x30]));
        b.prop("interrupts", &be_cells(&[0, 0x60, 1]));
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", 0x2c);
        b.end_node();
        b.begin_node("hvs@7e400000");
        b.prop_str("compatible", "brcm,bcm2711-hvs");
        b.prop("reg", &be_cells(&[0x7e40_0000, 0x8000]));
        b.prop_str("reg-names", "hvs");
        b.prop_str("status", engine);
        b.end_node();
        for (address, valve) in [
            (0x7e20_a000u32, "brcm,bcm2711-pixelvalve2"),
            (0x7e21_6000, "brcm,bcm2711-pixelvalve4"),
        ] {
            b.begin_node(&std::format!("pixelvalve@{address:x}"));
            b.prop_str("compatible", valve);
            b.prop("reg", &be_cells(&[address, 0x100]));
            b.prop_str("status", "disabled");
            b.end_node();
        }
        for (address, encoder) in [
            (0x7ef0_0700u32, "brcm,bcm2711-hdmi0"),
            (0x7ef0_5700, "brcm,bcm2711-hdmi1"),
        ] {
            b.begin_node(&std::format!("hdmi@{address:x}"));
            b.prop_str("compatible", encoder);
            b.prop("reg", &be_cells(&[address, 0x300, 0x7ef0_0100, 0x30]));
            b.prop_strs("reg-names", &["hdmi", "intr2"]);
            b.prop_u32("interrupt-parent", 0x2c);
            b.prop("interrupts", &be_cells(&[0, 1, 2, 3, 4, 5]));
            b.end_node();
        }
        b.end_node();
        b.end_node();
        b.build()
    }

    fn link_requests(node: &HwNode) -> Vec<tairix_abi::hwlink::LinkRequest> {
        node.resources()
            .iter()
            .filter_map(|r| r.link_request().ok())
            .collect()
    }

    #[test]
    fn the_engine_s_node_states_each_output_s_valve_and_the_encoder_it_feeds() {
        use tairix_abi::driver::encoder::ENCODER_ENDPOINTS;
        use tairix_abi::hwlink::LinkRole;
        use tairix_vcmailbox::ENGINE_OUTPUTS;
        let blob = display_engine_tree("okay");
        let nodes = discover_all(&blob);
        let engine = by_key(&nodes, b"brcm,bcm2711-hvs");
        for (output, base) in ENGINE_OUTPUTS.iter().zip([0xfe20_a000u64, 0xfe21_6000]) {
            let window = engine
                .resources()
                .iter()
                .find(|r| r.is_window_named(output.window))
                .expect("the valve's window");
            assert_eq!(
                (window.base(), window.length()),
                (base, 0x100),
                "translated"
            );
            let encoder = by_key(&nodes, output.encoder);
            let link = link_requests(engine)
                .into_iter()
                .find(|link| link.name() == output.link)
                .expect("the encoder's link");
            assert_eq!(link.role(), LinkRole::Encoder);
            assert_eq!(link.endpoint(), ENCODER_ENDPOINTS.endpoint(encoder.id()));
            let duty = encoder
                .resources()
                .iter()
                .find_map(|r| r.link_duty().ok())
                .expect("the encoder serves its links");
            assert_eq!(duty.endpoint(), ENCODER_ENDPOINTS.endpoint(encoder.id()));
            assert!(
                !encoder
                    .resources()
                    .iter()
                    .any(|r| r.is_window_named(b"intr2")),
                "the interrupt controller's registers are its own"
            );
            assert!(encoder
                .resources()
                .iter()
                .any(|r| r.is_window_named(b"hdmi")));
        }
        assert!(
            engine
                .resources()
                .iter()
                .any(|r| r.dma_bus_address(0x1000, 0x1000) == Some(0xc000_1000)),
            "its scan-out reaches memory through the bus's DMA window"
        );
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(
            display_engine(&fdt),
            Some(engine.id()),
            "the boot console moves to the engine"
        );
    }

    #[test]
    fn an_engine_the_tree_leaves_disabled_is_described_nowhere() {
        let blob = display_engine_tree("disabled");
        let nodes = discover_all(&blob);
        assert!(!nodes.iter().any(|node| node
            .match_keys()
            .iter()
            .any(|key| key.compatible_bytes() == b"brcm,bcm2711-hvs")));
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(display_engine(&fdt), None);
        let encoder = by_key(&nodes, b"brcm,bcm2711-hdmi0");
        assert!(!encoder
            .resources()
            .iter()
            .any(|r| r.is_window_named(b"intr2")));
    }
}
