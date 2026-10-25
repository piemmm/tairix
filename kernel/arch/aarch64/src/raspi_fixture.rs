//! The Raspberry Pi 4–shaped tree this port's discovery tests read, built
//! with the shared FDT writer.

use std::vec::Vec;

use tairix_fdt::write::FdtWriter;

/// Exclusive top of the DMA window the `/scb` bus declares, which the GENET
/// node under it inherits; the bus's single `dma-ranges` size cell.
pub(crate) const SCB_DMA_APERTURE_TOP: u32 = 0xfc00_0000;

/// Byte length of the GENET register aperture.
pub(crate) const GENET_REGS_LEN: u32 = 0x1_0000;

/// The GENET node's first `interrupts` SPI, type-relative: its INTID is this
/// plus the GICv2 SPI base.
pub(crate) const GENET_SPI_A: u32 = 157;
/// The GENET node's second `interrupts` SPI; see [`GENET_SPI_A`].
pub(crate) const GENET_SPI_B: u32 = 158;

/// The SPI the firmware mailbox raises while its inbox holds a word,
/// type-relative like [`GENET_SPI_A`].
pub(crate) const MAILBOX_SPI: u32 = 33;

/// The board MAC published on the GENET node through `local-mac-address`,
/// as the firmware does.
pub(crate) const GENET_BOARD_MAC: [u8; 6] = [0xDC, 0xA6, 0x32, 0x11, 0x22, 0x33];

/// Emit the `/scb` bus the DMA-mastering peripherals hang off — two-cell
/// child addresses, one-cell sizes, three `ranges` windows and the
/// `dma-ranges` every device on it inherits — and the GENET MAC on it.
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

/// The phandle of the GIC-400, which the root names as every node's
/// `interrupt-parent`.
pub(crate) const RASPI_GIC_PHANDLE: u32 = 1;

/// A Raspberry Pi 4–shaped tree: a PL011 at `pl011_base` (omitted when `0`)
/// and a mini-UART at `miniuart_base`, both bus addresses under a `/soc`
/// whose `ranges` remap the legacy windows into CPU-physical space; the
/// GIC-400, the firmware mailbox, the GPIO controller, `/psci` (`smc`), the
/// `/scb` bus with the GENET MAC, and 1 GiB of memory.
#[must_use]
pub(crate) fn raspi_like_arm(pl011_base: u64, miniuart_base: u64) -> Vec<u8> {
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
        b.begin_node(&std::format!("serial@{pl011_base:x}"));
        b.prop_str("compatible", "arm,pl011");
        b.prop("reg", &soc_reg(pl011_base, 0x200));
        b.end_node();
    }

    b.begin_node(&std::format!("serial@{miniuart_base:x}"));
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
