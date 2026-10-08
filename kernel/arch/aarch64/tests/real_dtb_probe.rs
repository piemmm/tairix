//! Real-firmware-tree regression probe: run the production boot-path
//! discovery walks — console selection, the BCM2711 GPIO controller, the
//! `VideoCore` mailbox, the `/memory` window, the SD card's rails, and the
//! EMMC2 node's DMA window — over the *pinned* Pi 4
//! firmware DTB, exactly as `boot_aarch64::configure_mmio_from_dtb` does
//! on metal. The synthetic `raspi_like_arm` fixture covers the shapes;
//! this test pins the walks against the real 55 KiB tree (its node
//! order, depth, and property layout), where a parser defect would
//! otherwise only surface as a silent on-metal boot death.
//!
//! The blob is the checksummed firmware input `tools/xtask` fetches into
//! `target/pi-firmware/` for the Pi image build. When that cache has not
//! been populated the test reports a skip and passes: the fixture-based
//! unit tests still cover the logic, and an absent download must not
//! fail an offline `cargo test` (fail closed is for
//! authority, not for missing optional inputs).
//!
//! Note: the on-disk file's `/memory@0` carries a zero `reg` — the
//! firmware patches the real RAM ranges in at boot — so the memory walk
//! is asserted for *shape* (`Some`), not for a size.

use tairix_abi::driver::timing::Delay;
use tairix_abi::{DmaCoherence, HwNode, HwResourceKind};
use tairix_arch_aarch64::{console, firmware, platform, sd_supply, uart_init};
use tairix_arch_api::platform::{DiscoveryError, HwNodeSink, PlatformDiscovery};
use tairix_fdt::Fdt;
use tairix_vcmailbox::mock::MockFirmware;

#[test]
fn real_pi4_dtb_discovery() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/pi-firmware/bcm2711-rpi-4-b.dtb"
    );
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("SKIP: no firmware DTB at {path}: {e}");
            return;
        }
    };
    let fdt = Fdt::new(&blob).expect("real DTB parses");

    let con = console::find_console(&fdt).expect("console discovered");
    assert_eq!(
        con.base, 0xfe20_1000,
        "PL011 UART0 at its bus-translated base"
    );
    assert_eq!(con.model, console::ConsoleModel::Pl011);

    let gpio = uart_init::find_gpio(&fdt).expect("BCM2711 GPIO controller discovered");
    assert_eq!(gpio.base, 0xfe20_0000);

    let mailbox = firmware::find_mailbox(&fdt).expect("VideoCore mailbox discovered");
    assert_eq!(mailbox.base, 0xfe00_b880);

    assert!(
        fdt.first_memory_region().is_some(),
        "a /memory node walks (the firmware patches its reg at boot)"
    );

    // The SD card's rails: power on expander line 6, 1.8 V signalling on
    // line 4, both the firmware's to drive.
    let supplies = sd_supply::find_sd_supplies(&fdt).expect("both SD rails");
    let mut firmware = MockFirmware::healthy();
    let delay = NoDelay;
    let mut rails = sd_supply::FirmwareSdSupply::new(&mut firmware, supplies, &delay);
    rails.set_signalling(1_800_000).expect("1.8 V state");
    rails.set_power(true).expect("power on");
    assert!(firmware.gpio_high(4), "VDD_SD_IO_SEL high selects 1.8 V");
    assert!(firmware.gpio_high(6), "SD_PWR_ON high powers the card");

    // The on-disk tree is the B0 stepping's; the firmware rewrites
    // `/emmc2bus` for later parts before the kernel reads it.
    let mut sink = Collect::default();
    platform::FdtDiscovery::new(fdt)
        .discover(&mut sink)
        .expect("the real tree walks");
    let emmc2 = sink
        .nodes
        .iter()
        .find(|n| {
            n.match_keys()
                .iter()
                .any(|k| k.compatible_bytes() == platform::EMMC2_COMPATIBLE)
        })
        .expect("EMMC2 emitted");
    let dma: Vec<(u64, u64, u64)> = emmc2
        .resources()
        .iter()
        .filter(|r| r.kind() == Some(HwResourceKind::Dma))
        .map(|r| (r.base(), r.length(), r.translated_base()))
        .collect();
    assert_eq!(dma, [(0x4000_0000, 0x4000_0000, 0xc000_0000)]);

    // No BCM2711 master states `dma-coherent`, so by Arm's convention none
    // snoops: every DMA grant the board's masters carry says so.
    for compatible in [
        platform::EMMC2_COMPATIBLE,
        platform::GENET_COMPATIBLE,
        platform::PCIE_COMPATIBLE,
        tairix_vcmailbox::MAILBOX_COMPATIBLE,
    ] {
        let node = sink
            .nodes
            .iter()
            .find(|n| {
                n.match_keys()
                    .iter()
                    .any(|k| k.compatible_bytes() == compatible)
            })
            .expect("master emitted");
        let coherence: Vec<DmaCoherence> = node
            .resources()
            .iter()
            .filter_map(tairix_abi::HwResource::dma_coherence)
            .collect();
        assert!(!coherence.is_empty(), "{compatible:?} states its DMA");
        assert!(
            coherence.iter().all(|&c| c == DmaCoherence::Unsnooped),
            "{compatible:?} does not snoop"
        );
    }
}

/// A delay that returns at once: the probe checks which lines move, not when.
struct NoDelay;

impl Delay for NoDelay {
    fn delay_us(&self, _us: u32) {}

    fn now_us(&self) -> u64 {
        0
    }
}

#[derive(Default)]
struct Collect {
    nodes: Vec<HwNode>,
}

impl HwNodeSink for Collect {
    fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
        self.nodes.push(node);
        Ok(())
    }
}
