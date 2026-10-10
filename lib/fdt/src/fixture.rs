//! Device trees for tests, built with the crate's own writer.
//!
//! Exposed behind the `test-fixtures` feature so this crate's own parser
//! tests **and** the architecture ports' discovery tests build their trees
//! one way rather than re-rolling the byte layout in each crate. The trees
//! mirror the ones QEMU's `virt` boards produce closely enough to exercise
//! every branch the boot pipeline relies on.

use alloc::vec::Vec;

use crate::write::FdtWriter;

/// Exclusive top of the DMA window [`raspi_like_arm`]'s `/scb` bus declares
/// its devices may reach, matching the real BCM2711 tree. The GENET node
/// under that bus inherits it as its DMA constraint. It is the bus's single
/// `dma-ranges` size cell, so it is stated at that width.
pub const SCB_DMA_APERTURE_TOP: u32 = 0xfc00_0000;

/// Byte length of the GENET register aperture [`raspi_like_arm`] declares.
pub const GENET_REGS_LEN: u32 = 0x1_0000;

/// The two SPI numbers the GENET node's `interrupts` list names (the real
/// tree's values). They are *type-relative*, so the discovered INTIDs are
/// these plus the GICv2 SPI base.
pub const GENET_SPI_A: u32 = 157;
/// The GENET node's second `interrupts` entry; see [`GENET_SPI_A`].
pub const GENET_SPI_B: u32 = 158;

/// The SPI [`raspi_like_arm`]'s `VideoCore` mailbox raises while its inbox
/// holds a word (the real tree's value), type-relative like [`GENET_SPI_A`].
pub const MAILBOX_SPI: u32 = 33;

/// The board MAC [`raspi_like_arm`] publishes on the GENET node, as the Pi's
/// firmware does through the `local-mac-address` binding.
pub const GENET_BOARD_MAC: [u8; 6] = [0xDC, 0xA6, 0x32, 0x11, 0x22, 0x33];

/// Emit [`raspi_like_arm`]'s `/scb` bus and the GENET Ethernet MAC on it.
///
/// Split out so neither builder is over-long: the BCM2711 hangs its
/// DMA-mastering peripherals off `/scb`, which declares two-cell child
/// addresses, one-cell sizes, the real tree's three `ranges` windows, and the
/// `dma-ranges` giving the reach of every device on it.
fn push_scb_bus(b: &mut FdtWriter) {
    b.begin_node("scb");
    b.prop_str("compatible", "simple-bus");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 1);
    let scb_window = |child: u64, parent: u64, size: u32| {
        let mut entry = Vec::new();
        entry.extend_from_slice(&child.to_be_bytes());
        entry.extend_from_slice(&parent.to_be_bytes());
        entry.extend_from_slice(&size.to_be_bytes());
        entry
    };
    let mut scb_ranges = Vec::new();
    for (child, parent, size) in [
        (0x7c00_0000u64, 0xfc00_0000u64, 0x0380_0000u32),
        (0x4000_0000, 0xff80_0000, 0x0080_0000),
        (0x6_0000_0000, 0x6_0000_0000, 0x4000_0000),
    ] {
        scb_ranges.extend_from_slice(&scb_window(child, parent, size));
    }
    b.prop("ranges", &scb_ranges);
    b.prop("dma-ranges", &scb_window(0, 0, SCB_DMA_APERTURE_TOP));

    // The GENET v5 MAC at its bus address (CPU-physical `0xFD58_0000`
    // through the `0x7C00_0000 -> 0xFC00_0000` window), with the two SPIs it
    // raises, the firmware-published board MAC, and its MDIO child bus.
    b.begin_node("ethernet@7d580000");
    b.prop_str("compatible", "brcm,bcm2711-genet-v5");
    let mut genet_reg = Vec::new();
    genet_reg.extend_from_slice(&0x7d58_0000u64.to_be_bytes());
    genet_reg.extend_from_slice(&GENET_REGS_LEN.to_be_bytes());
    b.prop("reg", &genet_reg);
    let mut genet_interrupts = Vec::new();
    for spi in [GENET_SPI_A, GENET_SPI_B] {
        for cell in [0u32, spi, 0x04] {
            genet_interrupts.extend_from_slice(&cell.to_be_bytes());
        }
    }
    b.prop("interrupts", &genet_interrupts);
    b.prop("local-mac-address", &GENET_BOARD_MAC);
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 1);
    b.begin_node("mdio@e14");
    b.prop_str("compatible", "brcm,genet-mdio-v5");
    let mut mdio_reg = Vec::new();
    mdio_reg.extend_from_slice(&0x0e14u32.to_be_bytes());
    mdio_reg.extend_from_slice(&8u32.to_be_bytes());
    b.prop("reg", &mdio_reg);
    b.end_node(); // mdio@e14
    b.end_node(); // ethernet@7d580000

    b.end_node(); // /scb
}

/// A QEMU-`virt`-shaped riscv64 tree: 2/2 root cells, a `/cpus` node with
/// a `timebase-frequency`, and a `/memory@80000000` node.
#[must_use]
pub fn virt_like(base: u64, size: u64, timebase: u32) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("cpus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 0);
    b.prop_u32("timebase-frequency", timebase);
    b.end_node();
    b.begin_node("memory@80000000");
    b.prop("device_type", b"memory\0");
    let mut reg = Vec::new();
    reg.extend_from_slice(&base.to_be_bytes());
    reg.extend_from_slice(&size.to_be_bytes());
    b.prop("reg", &reg);
    b.end_node();
    b.end_node();
    b.build()
}

/// The phandle QEMU's riscv64 `virt` board gives its PLIC, which every device
/// names as its `interrupt-parent`.
pub const VIRT_PLIC_PHANDLE: u32 = 3;

/// A QEMU-`virt`-shaped riscv64 tree with a PLIC and one or more
/// `virtio_mmio` slots, for exercising the riscv64 bootstrap-floor
/// virtio-MMIO discovery + PLIC interrupt-line decode.
///
/// Extends [`virt_like`] with a `plic` node carrying `riscv,ndev` (the
/// PLIC source count) and one `virtio_mmio@<base>` node per entry of
/// `slots`, each `(mmio_base, plic_irq)`: a two-cell `reg` of `<base
/// 0x1000>` and a single-cell `interrupts` of `plic_irq` — the shape the
/// QEMU `virt` board produces (`#interrupt-cells = <1>` on the PLIC, so a
/// device names its PLIC source directly). A `plic_irq` of `0` emits the
/// PLIC "no source" sentinel, so a test can assert a slot with no routable
/// line is rejected fail-closed.
#[must_use]
pub fn virt_like_with_virtio(
    base: u64,
    size: u64,
    timebase: u32,
    ndev: u32,
    slots: &[(u64, u32)],
) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);

    b.begin_node("cpus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 0);
    b.prop_u32("timebase-frequency", timebase);
    b.end_node();

    b.begin_node("memory@80000000");
    b.prop("device_type", b"memory\0");
    let mut reg = Vec::new();
    reg.extend_from_slice(&base.to_be_bytes());
    reg.extend_from_slice(&size.to_be_bytes());
    b.prop("reg", &reg);
    b.end_node();

    // PLIC (`plic@c000000` on the real `virt` board). The interrupt-line
    // decode reads its `riscv,ndev` to bound a device's source.
    b.begin_node("plic@c000000");
    b.prop_str("compatible", "riscv,plic0");
    b.prop("interrupt-controller", &[]);
    b.prop_u32("#interrupt-cells", 1);
    b.prop_u32("riscv,ndev", ndev);
    b.prop_u32("phandle", VIRT_PLIC_PHANDLE);
    // A two-cell `<base size>` reg matching the node's unit address, as the
    // real `virt` board declares it, so the PLIC-base resolver has an address
    // to read.
    let mut plic_reg = Vec::new();
    plic_reg.extend_from_slice(&0x0c00_0000u64.to_be_bytes());
    plic_reg.extend_from_slice(&0x0060_0000u64.to_be_bytes());
    b.prop("reg", &plic_reg);
    b.end_node();

    for (mmio_base, plic_irq) in slots {
        b.begin_node(&alloc::format!("virtio_mmio@{mmio_base:x}"));
        b.prop_str("compatible", "virtio,mmio");
        let mut vreg = Vec::new();
        vreg.extend_from_slice(&mmio_base.to_be_bytes());
        vreg.extend_from_slice(&0x1000u64.to_be_bytes());
        b.prop("reg", &vreg);
        b.prop_u32("interrupts", *plic_irq);
        b.prop_u32("interrupt-parent", VIRT_PLIC_PHANDLE);
        b.end_node();
    }

    b.end_node();
    b.build()
}

/// The phandle [`virt_like_aia`] gives the supervisor-level APLIC, which its
/// devices name as their interrupt parent.
pub const VIRT_APLIC_PHANDLE: u32 = 6;

/// The phandle [`virt_like_aia`] gives the supervisor-level IMSIC.
pub const VIRT_IMSIC_PHANDLE: u32 = 4;

/// The supervisor-level IMSIC's first interrupt file in [`virt_like_aia`].
pub const VIRT_IMSIC_S_BASE: u64 = 0x2800_0000;

/// The supervisor-level APLIC's registers in [`virt_like_aia`].
pub const VIRT_APLIC_S_BASE: u64 = 0x0d00_0000;

/// A QEMU `virt,aia=aplic-imsic`-shaped riscv64 tree: `harts` harts, each
/// with its local interrupt controller, a machine- and a supervisor-level
/// IMSIC (255 identities, a file per hart) and APLIC (96 sources, the
/// machine domain delegating every source to the supervisor one), and one
/// `virtio_mmio` slot per `(base, source, sense)` raised through the
/// supervisor APLIC with a two-cell `<source sense>` specifier.
#[must_use]
pub fn virt_like_aia(harts: u32, slots: &[(u64, u32, u32)]) -> Vec<u8> {
    aia_tree(harts, slots, 0)
}

/// [`virt_like_aia`] with no virtio-MMIO slots, its IMSICs giving each hart
/// `2^guest_index_bits - 1` guest files beside its own, as
/// `riscv,guest-index-bits` says.
#[must_use]
pub fn virt_like_aia_with_guest_bits(harts: u32, guest_index_bits: u32) -> Vec<u8> {
    aia_tree(harts, &[], guest_index_bits)
}

fn aia_tree(harts: u32, slots: &[(u64, u32, u32)], guest_index_bits: u32) -> Vec<u8> {
    const INTC_PHANDLE: u32 = 0x10;
    const M_IMSIC: u32 = 3;
    const M_APLIC: u32 = 5;
    let cells = |values: &[u32]| {
        values
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect::<Vec<u8>>()
    };
    let reg = |base: u64, len: u64| {
        let mut reg = Vec::new();
        reg.extend_from_slice(&base.to_be_bytes());
        reg.extend_from_slice(&len.to_be_bytes());
        reg
    };
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("cpus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 0);
    b.prop_u32("timebase-frequency", 10_000_000);
    for hart in 0..harts {
        b.begin_node(&alloc::format!("cpu@{hart}"));
        b.prop("device_type", b"cpu\0");
        b.prop_u32("reg", hart);
        b.begin_node("interrupt-controller");
        b.prop_str("compatible", "riscv,cpu-intc");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", INTC_PHANDLE + hart);
        b.end_node();
        b.end_node();
    }
    b.end_node();
    b.begin_node("memory@80000000");
    b.prop("device_type", b"memory\0");
    b.prop("reg", &reg(0x8000_0000, 0x1000_0000));
    b.end_node();
    for (name, base, phandle, cause) in [
        ("imsics@24000000", 0x2400_0000u64, M_IMSIC, 11),
        ("imsics@28000000", VIRT_IMSIC_S_BASE, VIRT_IMSIC_PHANDLE, 9),
    ] {
        b.begin_node(name);
        b.prop_str("compatible", "riscv,imsics");
        b.prop("interrupt-controller", &[]);
        b.prop("msi-controller", &[]);
        b.prop_u32("#interrupt-cells", 0);
        b.prop_u32("riscv,num-ids", 255);
        if guest_index_bits != 0 {
            b.prop_u32("riscv,guest-index-bits", guest_index_bits);
        }
        let per_hart = 0x1000u64
            .checked_shl(guest_index_bits.min(6))
            .unwrap_or(0x1000);
        b.prop("reg", &reg(base, per_hart * u64::from(harts)));
        let extended: Vec<u32> = (0..harts)
            .flat_map(|hart| [INTC_PHANDLE + hart, cause])
            .collect();
        b.prop("interrupts-extended", &cells(&extended));
        b.prop_u32("phandle", phandle);
        b.end_node();
    }
    b.begin_node("aplic@c000000");
    b.prop_str("compatible", "riscv,aplic");
    b.prop("interrupt-controller", &[]);
    b.prop_u32("#interrupt-cells", 2);
    b.prop_u32("riscv,num-sources", 96);
    b.prop("reg", &reg(0x0c00_0000, 0x8000));
    b.prop_u32("msi-parent", M_IMSIC);
    b.prop_u32("riscv,children", VIRT_APLIC_PHANDLE);
    b.prop("riscv,delegation", &cells(&[VIRT_APLIC_PHANDLE, 1, 96]));
    b.prop_u32("phandle", M_APLIC);
    b.end_node();
    b.begin_node("aplic@d000000");
    b.prop_str("compatible", "riscv,aplic");
    b.prop("interrupt-controller", &[]);
    b.prop_u32("#interrupt-cells", 2);
    b.prop_u32("riscv,num-sources", 96);
    b.prop("reg", &reg(VIRT_APLIC_S_BASE, 0x8000));
    b.prop_u32("msi-parent", VIRT_IMSIC_PHANDLE);
    b.prop_u32("phandle", VIRT_APLIC_PHANDLE);
    b.end_node();
    for &(base, source, sense) in slots {
        b.begin_node(&alloc::format!("virtio_mmio@{base:x}"));
        b.prop_str("compatible", "virtio,mmio");
        b.prop("reg", &reg(base, 0x1000));
        b.prop("interrupts", &cells(&[source, sense]));
        b.prop_u32("interrupt-parent", VIRT_APLIC_PHANDLE);
        b.end_node();
    }
    b.end_node();
    b.build()
}

/// An aarch64 tree carrying a `/cpus` node whose `cpu@*` children declare
/// per-core `reg` (the `MPIDR_EL1` affinity) and an optional
/// `capacity-dmips-mhz` rating, plus the usual `/memory` node.
///
/// Each entry of `cpus` is `(mpidr, capacity)`: a `Some` capacity emits a
/// `capacity-dmips-mhz` property (a `big.LITTLE` part), a `None` omits it
/// (a homogeneous part). Used to exercise [`crate::Fdt::each_cpu`] and the
/// aarch64 heterogeneous-core classifier.
#[must_use]
pub fn arm_with_cpus(base: u64, size: u64, cpus: &[(u64, Option<u32>)]) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);

    b.begin_node("cpus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 0);
    for (mpidr, capacity) in cpus {
        let name = alloc::format!("cpu@{mpidr:x}");
        b.begin_node(&name);
        b.prop_str("device_type", "cpu");
        // The fixture writes a single-cell (`#address-cells = 1`) `reg`;
        // `try_from` keeps the cast honest rather than silently truncating
        // a value that does not fit one cell.
        b.prop_u32(
            "reg",
            u32::try_from(*mpidr).expect("fixture MPIDR fits one cell"),
        );
        if let Some(cap) = capacity {
            b.prop_u32("capacity-dmips-mhz", *cap);
        }
        b.end_node();
    }
    b.end_node();

    b.begin_node("memory@40000000");
    b.prop("device_type", b"memory\0");
    let mut reg = Vec::new();
    reg.extend_from_slice(&base.to_be_bytes());
    reg.extend_from_slice(&size.to_be_bytes());
    b.prop("reg", &reg);
    b.end_node();

    b.end_node();
    b.build()
}

/// An aarch64 tree whose `/cpus/cpu@*` children declare the Pi-4
/// stock-firmware spin-table shape: per-core `reg` plus, for each entry
/// with a `Some` release address, `enable-method = "spin-table"` and a
/// two-cell `cpu-release-addr`. A `None` release entry emits neither
/// property (the boot CPU's shape). Used to exercise
/// [`crate::CpuNode::spin_table_release`] and the aarch64 spin-table
/// bring-up discovery.
#[must_use]
pub fn arm_with_spin_table_cpus(base: u64, size: u64, cpus: &[(u64, Option<u64>)]) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);

    b.begin_node("cpus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 0);
    for (mpidr, release) in cpus {
        let name = alloc::format!("cpu@{mpidr:x}");
        b.begin_node(&name);
        b.prop_str("device_type", "cpu");
        b.prop_u32(
            "reg",
            u32::try_from(*mpidr).expect("fixture MPIDR fits one cell"),
        );
        if let Some(addr) = release {
            b.prop("enable-method", b"spin-table\0");
            b.prop("cpu-release-addr", &addr.to_be_bytes());
        }
        b.end_node();
    }
    b.end_node();

    b.begin_node("memory@0");
    b.prop("device_type", b"memory\0");
    let mut reg = Vec::new();
    reg.extend_from_slice(&base.to_be_bytes());
    reg.extend_from_slice(&size.to_be_bytes());
    b.prop("reg", &reg);
    b.end_node();

    b.end_node();
    b.build()
}

/// The phandle the Pi 4 tree gives its GIC-400, which the root names as every
/// node's `interrupt-parent`.
pub const RASPI_GIC_PHANDLE: u32 = 1;

/// A Raspberry-Pi-shaped aarch64 tree carrying the two console UARTs the
/// Pi exposes — a PrimeCell PL011 (`arm,pl011`) and a BCM2835 AUX
/// mini-UART (`brcm,bcm2835-aux-uart`) — a GIC-400 interrupt controller
/// (`arm,gic-400`) at the BCM2711 bases, the `VideoCore` firmware mailbox
/// (`brcm,bcm2835-mbox`) at the BCM2711 ARM-physical base `0xFE00_B880`
/// with a `0x40`-byte doorbell window and SPI [`MAILBOX_SPI`], plus a
/// `/psci` (`smc`) node and a
/// 1 GiB `/memory@0` node.
///
/// The tree mirrors the real `bcm2711-rpi-4-b.dtb` shape: the root
/// declares `#address-cells = 2` / `#size-cells = 1`, and every
/// peripheral sits under a `/soc` `simple-bus` whose `#address-cells` /
/// `#size-cells` are both `1` and whose three-entry `ranges` remap the
/// legacy bus windows into CPU-physical space (`0x7E00_0000 →
/// 0xFE00_0000`, `0x7C00_0000 → 0xFC00_0000`, `0x4000_0000 →
/// 0xFF80_0000`). `pl011_base` and `miniuart_base` are therefore the
/// *bus* addresses the nodes' `reg` cells carry (e.g. `0x7E20_1000` /
/// `0x7E21_5040`); readers must translate them through the `/soc`
/// `ranges` exactly as on the real board. A `pl011_base` of `0` omits
/// the PL011 node, leaving the mini-UART as the only console — used to
/// exercise the aarch64 port's console-model fallback. The PL011 window
/// is `0x200` bytes; the mini-UART window is `0x40` bytes (the
/// `AUX_MU_*` register block); the GIC-400 carries the real tree's four
/// one-cell regions (GICD/GICC/GICH/GICV).
#[must_use]
pub fn raspi_like_arm(pl011_base: u64, miniuart_base: u64) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 1);
    b.prop_u32("interrupt-parent", RASPI_GIC_PHANDLE);

    b.begin_node("psci");
    b.prop_str("compatible", "arm,psci-1.0");
    b.prop_str("method", "smc");
    b.end_node();

    // One `reg` entry under `/soc`: a one-cell bus address plus a
    // one-cell length, exactly as the real BCM2711 tree encodes them.
    let soc_reg = |base: u64, size: u32| {
        let mut reg = Vec::new();
        reg.extend_from_slice(
            &u32::try_from(base)
                .expect("bus address fits one cell")
                .to_be_bytes(),
        );
        reg.extend_from_slice(&size.to_be_bytes());
        reg
    };

    b.begin_node("soc");
    b.prop_str("compatible", "simple-bus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 1);
    // The real tree's three windows: one-cell child address, two-cell
    // parent address, one-cell size per entry.
    let mut ranges = Vec::new();
    for (child, parent, size) in [
        (0x7e00_0000u32, 0xfe00_0000u64, 0x0180_0000u32),
        (0x7c00_0000, 0xfc00_0000, 0x0200_0000),
        (0x4000_0000, 0xff80_0000, 0x0080_0000),
    ] {
        ranges.extend_from_slice(&child.to_be_bytes());
        ranges.extend_from_slice(&parent.to_be_bytes());
        ranges.extend_from_slice(&size.to_be_bytes());
    }
    b.prop("ranges", &ranges);

    // GIC-400 (a GICv2) at the real tree's bus addresses (CPU-physical
    // GICD `0xFF84_1000`, GICC `0xFF84_2000` through the `0x4000_0000 →
    // 0xFF80_0000` range): four `reg` regions — GICD, GICC, GICH, GICV.
    b.begin_node("interrupt-controller@40041000");
    b.prop_str("compatible", "arm,gic-400");
    b.prop("interrupt-controller", &[]);
    b.prop_u32("#interrupt-cells", 3);
    b.prop_u32("phandle", RASPI_GIC_PHANDLE);
    let mut gic_reg = soc_reg(0x4004_1000, 0x1000);
    gic_reg.extend_from_slice(&soc_reg(0x4004_2000, 0x2000));
    gic_reg.extend_from_slice(&soc_reg(0x4004_4000, 0x2000));
    gic_reg.extend_from_slice(&soc_reg(0x4004_6000, 0x2000));
    b.prop("reg", &gic_reg);
    b.end_node();

    // The VideoCore firmware mailbox doorbell block at its bus address
    // (CPU-physical `0xFE00_B880`), the node the HVS framebuffer
    // discovery binds, with the level-high SPI its inbox raises.
    b.begin_node("mailbox@7e00b880");
    b.prop_str("compatible", "brcm,bcm2835-mbox");
    b.prop("reg", &soc_reg(0x7e00_b880, 0x40));
    let mut mailbox_interrupts = Vec::new();
    for cell in [0u32, MAILBOX_SPI, 0x04] {
        mailbox_interrupts.extend_from_slice(&cell.to_be_bytes());
    }
    b.prop("interrupts", &mailbox_interrupts);
    b.end_node();

    // The BCM2711 GPIO controller at its bus address (CPU-physical
    // `0xFE20_0000`), the node the aarch64 port's UART pin-mux
    // discovery binds. The `0xB4` length is the real tree's value (it
    // predates the BCM2711 pull registers; the consumer sizes the
    // register window from the datasheet, not this length).
    b.begin_node("gpio@7e200000");
    b.prop_str("compatible", "brcm,bcm2711-gpio");
    b.prop("reg", &soc_reg(0x7e20_0000, 0xb4));
    b.end_node();

    if pl011_base != 0 {
        b.begin_node(&alloc::format!("serial@{pl011_base:x}"));
        b.prop_str("compatible", "arm,pl011");
        b.prop("reg", &soc_reg(pl011_base, 0x200));
        b.end_node();
    }

    b.begin_node(&alloc::format!("serial@{miniuart_base:x}"));
    b.prop_str("compatible", "brcm,bcm2835-aux-uart");
    b.prop("reg", &soc_reg(miniuart_base, 0x40));
    b.end_node();

    b.end_node(); // /soc

    push_scb_bus(&mut b);

    b.begin_node("memory@0");
    b.prop("device_type", b"memory\0");
    // Root cells: two-cell address, one-cell size (the real tree's
    // shape; the firmware patches the size in at boot).
    let mut mem_reg = Vec::new();
    mem_reg.extend_from_slice(&0u64.to_be_bytes());
    mem_reg.extend_from_slice(&0x4000_0000u32.to_be_bytes());
    b.prop("reg", &mem_reg);
    b.end_node();

    b.end_node();
    b.build()
}

/// The phandle QEMU's aarch64 `virt` board gives its GIC, which the root names
/// as every node's `interrupt-parent`.
pub const VIRT_GIC_PHANDLE: u32 = 0x8002;

/// A QEMU-`virt`-shaped aarch64 tree: 2/2 root cells, a `/memory` node, a
/// `/psci` node with a `method` (`hvc`/`smc`), and a `/timer` node with an
/// `interrupts` cell list (the per-CPU PPI the generic timer raises).
#[must_use]
pub fn virt_like_arm(base: u64, size: u64, psci_method: &str, timer_ppi: u32) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.prop_u32("interrupt-parent", VIRT_GIC_PHANDLE);

    b.begin_node("psci");
    b.prop_str("compatible", "arm,psci-1.0");
    b.prop_str("method", psci_method);
    b.end_node();

    // GICv2 interrupt controller (`intc@8000000` on the real `virt`
    // board): distributor `0x0800_0000`, CPU interface `0x0801_0000`,
    // each a 0x10000 window. Two `reg` regions, the layout the aarch64
    // GIC discovery reads.
    b.begin_node("intc@8000000");
    b.prop_str("compatible", "arm,cortex-a15-gic");
    b.prop("interrupt-controller", &[]);
    b.prop_u32("#interrupt-cells", 3);
    b.prop_u32("phandle", VIRT_GIC_PHANDLE);
    let mut gic_reg = Vec::new();
    for cell in [0x0800_0000u64, 0x1_0000, 0x0801_0000, 0x1_0000] {
        gic_reg.extend_from_slice(&cell.to_be_bytes());
    }
    b.prop("reg", &gic_reg);
    b.end_node();

    b.begin_node("timer");
    b.prop_str("compatible", "arm,armv8-timer");
    // GIC interrupt specifier triple: <type, number, flags>. The fourth
    // (EL1 physical timer) entry is the one the kernel arms; the fixture
    // carries that PPI number for the discovery reader to surface.
    let mut interrupts = Vec::new();
    for cell in [1u32, timer_ppi, 0x08] {
        interrupts.extend_from_slice(&cell.to_be_bytes());
    }
    b.prop("interrupts", &interrupts);
    b.end_node();

    b.begin_node("memory@40000000");
    b.prop("device_type", b"memory\0");
    let mut reg = Vec::new();
    reg.extend_from_slice(&base.to_be_bytes());
    reg.extend_from_slice(&size.to_be_bytes());
    b.prop("reg", &reg);
    b.end_node();

    b.end_node();
    b.build()
}

/// QEMU aarch64 `virt`'s generic PCI host: high ECAM in segment 2, its I/O,
/// 32-bit and 64-bit windows, a GIC parent taking two address and three
/// interrupt cells, and the swizzled INTx map; with a root port at `00:01.0`
/// marked external-facing where `external`.
#[must_use]
pub fn ecam_host_arm(external: bool) -> Vec<u8> {
    let cells =
        |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("intc@8000000");
    b.prop_u32("phandle", 0x8002);
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#interrupt-cells", 3);
    b.end_node();
    b.begin_node("pcie@10000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_str("device_type", "pci");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop_u32("#interrupt-cells", 1);
    b.prop("reg", &cells(&[0x40, 0x1000_0000, 0, 0x1000_0000]));
    b.prop("bus-range", &cells(&[0, 0xFF]));
    b.prop_u32("linux,pci-domain", 2);
    b.prop(
        "ranges",
        &cells(&[
            0x0100_0000,
            0,
            0,
            0,
            0x3EFF_0000,
            0,
            0x1_0000,
            0x0200_0000,
            0,
            0x1000_0000,
            0,
            0x1000_0000,
            0,
            0x2EFF_0000,
            0x0300_0000,
            0x80,
            0,
            0x80,
            0,
            0x80,
            0,
        ]),
    );
    b.prop("interrupt-map-mask", &cells(&[0x1800, 0, 0, 7]));
    let mut map = Vec::new();
    for slot in 0..4u32 {
        for pin in 0..4u32 {
            map.extend_from_slice(&[
                slot << 11,
                0,
                0,
                pin + 1,
                0x8002,
                0,
                0,
                0,
                3 + (slot + pin) % 4,
                4,
            ]);
        }
    }
    b.prop("interrupt-map", &cells(&map));
    if external {
        b.begin_node("pcie@1,0");
        b.prop("reg", &cells(&[0x0800, 0, 0, 0, 0]));
        b.prop("external-facing", &[]);
        b.end_node();
    }
    b.end_node();
    b.end_node();
    b.build()
}
