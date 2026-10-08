//! Host tests for the GIC facade and its GICv2 driver.

use super::*;

#[test]
fn sgir_packs_target_list_and_intid() {
    // INTID 0 to CPU 0 → target list bit 0.
    assert_eq!(sgir_value(0, 0b0000_0001), 1 << 16);
    // INTID 0 to CPU 2 → target list bit 2.
    assert_eq!(sgir_value(0, 0b0000_0100), 0b0000_0100 << 16);
    // INTID field is the low 4 bits only.
    assert_eq!(sgir_value(0x1F, 1) & 0xF, 0xF);
}

#[test]
fn isenabler_offset_selects_the_right_word() {
    // INTIDs 0..32 live in the first word (offset 0x100).
    assert_eq!(isenabler_offset(0), 0x100);
    assert_eq!(isenabler_offset(30), 0x100);
    // INTID 32 spills into the second word.
    assert_eq!(isenabler_offset(32), 0x104);
}

#[test]
fn isenabler_bit_indexes_within_the_word() {
    assert_eq!(isenabler_bit(0), 1);
    assert_eq!(isenabler_bit(30), 1 << 30);
    assert_eq!(isenabler_bit(32), 1);
}

#[test]
fn iar_mask_and_spurious_match_gicv2_spec() {
    assert_eq!(IAR_INTID_MASK, 0x3FF);
    assert_eq!(SPURIOUS_INTID, 1023);
}

#[test]
fn an_acknowledgement_names_its_line_keeps_the_sgi_source_and_spurious_is_nothing() {
    let gic = Gicv2::new(MockGicMmio::new());
    for line in [30, 27, 77] {
        gic.mmio.gicc_write(GICC_IAR, line);
        assert_eq!(gic.acknowledge().map(|ack| ack.intid), Some(line));
    }
    gic.mmio.gicc_write(GICC_IAR, 0b010 << 10);
    let sgi = gic.acknowledge().unwrap();
    assert_eq!((sgi.intid, sgi.token()), (0, 0b010 << 10));
    gic.mmio.gicc_write(GICC_IAR, SPURIOUS_INTID);
    assert_eq!(gic.acknowledge(), None);
}

#[test]
fn icenabler_offset_parallels_isenabler() {
    assert_eq!(icenabler_offset(0), 0x180);
    assert_eq!(icenabler_offset(30), 0x180);
    assert_eq!(icenabler_offset(32), 0x184);
}

#[test]
fn itargetsr_offset_is_one_byte_per_intid() {
    // SPI 2 on the `virt` board (the PL031 RTC) is INTID 34.
    assert_eq!(itargetsr_offset(34), 0x800 + 34);
    assert_eq!(itargetsr_offset(MIN_SPI_INTID), 0x800 + 32);
}

#[test]
fn route_spi_writes_the_target_byte_for_an_spi() {
    let gic = Gicv2::new(MockGicMmio::new());
    // Route INTID 34 to CPU 0 (target-list bit 0).
    gic.route_spi(34, 0b0000_0001);
    assert_eq!(gic.mmio.gicd_read(itargetsr_offset(34)), 0b0000_0001);
}

#[test]
fn route_spi_skips_sgis_and_ppis() {
    let gic = Gicv2::new(MockGicMmio::new());
    // INTID 30 is the timer PPI: its target byte is read-only and
    // banked, so `route_spi` must not write it.
    gic.route_spi(30, 0b0000_0001);
    assert_eq!(gic.mmio.gicd_read(itargetsr_offset(30)), 0);
}

/// One recorded mock operation, in issue order, so a test can assert
/// the publish-before-signal ordering the SGI hand-off requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MockOp {
    /// A `publish_barrier` (the `dsb ishst` on metal).
    Barrier,
    /// A distributor write to the `GICD_SGIR` register (raising the SGI).
    SgirWrite,
}

/// In-memory GICv2 register file: distributor and CPU-interface
/// windows are independent, so the mock keeps a map per window and
/// serves the last value written to a register on a subsequent read.
/// It also records the barrier / `GICD_SGIR`-write sequence in
/// [`MockGicMmio::ops`] so the publish-before-signal ordering is
/// unit-tested without hardware.
struct MockGicMmio {
    gicd: std::sync::Mutex<std::collections::HashMap<usize, u32>>,
    gicc: std::sync::Mutex<std::collections::HashMap<usize, u32>>,
    ops: std::sync::Mutex<std::vec::Vec<MockOp>>,
    /// Distributor registers that ignore writes, as a Secure line's do
    /// to Non-secure software.
    secure: std::sync::Mutex<std::vec::Vec<usize>>,
}

impl MockGicMmio {
    fn new() -> Self {
        Self {
            gicd: std::sync::Mutex::new(std::collections::HashMap::new()),
            gicc: std::sync::Mutex::new(std::collections::HashMap::new()),
            ops: std::sync::Mutex::new(std::vec::Vec::new()),
            secure: std::sync::Mutex::new(std::vec::Vec::new()),
        }
    }

    /// The barrier / SGIR-write operations recorded so far, in order.
    fn ops(&self) -> std::vec::Vec<MockOp> {
        self.ops.lock().unwrap().clone()
    }
}

impl GicMmio for MockGicMmio {
    fn gicd_read(&self, off: usize) -> u32 {
        *self.gicd.lock().unwrap().get(&off).unwrap_or(&0)
    }
    fn gicd_write(&self, off: usize, val: u32) {
        if off == GICD_SGIR {
            self.ops.lock().unwrap().push(MockOp::SgirWrite);
        }
        if self.secure.lock().unwrap().contains(&off) {
            return;
        }
        self.gicd.lock().unwrap().insert(off, val);
    }
    fn gicd_write_byte(&self, off: usize, val: u8) {
        self.gicd.lock().unwrap().insert(off, u32::from(val));
    }
    fn gicc_read(&self, off: usize) -> u32 {
        *self.gicc.lock().unwrap().get(&off).unwrap_or(&0)
    }
    fn gicc_write(&self, off: usize, val: u32) {
        self.gicc.lock().unwrap().insert(off, val);
    }
    fn publish_barrier(&self) {
        self.ops.lock().unwrap().push(MockOp::Barrier);
    }
}

/// A line whose configuration ignores the write — a Secure one — is
/// refused, not reported configured.
#[test]
fn a_trigger_change_that_does_not_take_is_refused() {
    let controller = GicController::new(Gicv2::new(MockGicMmio::new()), 255);
    controller
        .gic
        .mmio
        .secure
        .lock()
        .unwrap()
        .push(icfgr_offset(50));
    assert_eq!(controller.set_trigger(50, true), Err(TriggerRefused));
    assert!(!controller.gic.is_edge_triggered(50));
}

#[test]
fn a_trigger_change_touches_only_its_own_field_and_only_while_disabled() {
    let controller = GicController::new(Gicv2::new(MockGicMmio::new()), 255);
    let word = icfgr_offset(40);
    controller
        .gic
        .mmio
        .gicd_write(word, 0b10 << (2 * (41 % 16)));
    controller
        .set_trigger(40, true)
        .expect("a disabled SPI changes");
    assert_eq!(
        controller.gic.mmio.gicd_read(word),
        0b10 << (2 * (41 % 16)) | 0b10 << (2 * (40 % 16)),
        "its neighbour's field is kept"
    );
    assert!(controller.gic.is_edge_triggered(40));
    controller
        .set_trigger(40, true)
        .expect("an edge line stays one");
    controller
        .gic
        .mmio
        .gicd_write(isenabler_offset(40), isenabler_bit(40));
    assert_eq!(
        controller.set_trigger(40, true),
        Ok(()),
        "unchanged, so allowed"
    );
    assert_eq!(
        controller.set_trigger(40, false),
        Err(TriggerRefused),
        "enabled"
    );
    assert_eq!(
        controller.set_trigger(27, true),
        Err(TriggerRefused),
        "a PPI's is fixed"
    );
    assert_eq!(
        controller.set_trigger(27, false),
        Ok(()),
        "and already level"
    );
    assert_eq!(
        controller.set_trigger(256, false),
        Err(TriggerRefused),
        "past the controller"
    );
}

#[test]
fn send_sgi_publishes_prior_stores_before_raising_the_interrupt() {
    // The cross-CPU wake hand-off enqueues the woken task and *then*
    // raises the reschedule IPI. On a weakly-ordered PE the enqueue
    // must be published before the target can act on the SGI, or the
    // target dispatches against a stale run queue and strands the task
    // (a lost wake-up that hangs the system). `send_sgi` must issue the
    // publish barrier strictly before the `GICD_SGIR` write.
    let gic = Gicv2::new(MockGicMmio::new());
    gic.send_sgi(0, 0b0000_0010);
    assert_eq!(
        gic.mmio.ops(),
        std::vec![MockOp::Barrier, MockOp::SgirWrite],
        "the publish barrier must precede the SGIR write"
    );
    assert_eq!(gic.mmio.gicd_read(GICD_SGIR), sgir_value(0, 0b0000_0010));
}

#[test]
fn a_cpu_s_interface_mask_is_its_banked_target_byte_and_a_uniprocessor_gic_s_its_one_interface() {
    let gic = Gicv2::new(MockGicMmio::new());
    gic.mmio.gicd_write(GICD_ITARGETSR, 0x0404_0404);
    assert_eq!(gic.local_interface_mask(), 0b100);
    // A uniprocessor GIC reads its target bytes as zero.
    gic.mmio.gicd_write(GICD_ITARGETSR, 0);
    assert_eq!(gic.local_interface_mask(), 1);
}

#[test]
fn enable_intid_sets_priority_and_enable_bit() {
    let gic = Gicv2::new(MockGicMmio::new());
    gic.enable_intid(42);
    assert_eq!(
        gic.mmio.gicd_read(GICD_IPRIORITYR + 42),
        u32::from(MID_RANGE_PRIORITY)
    );
    assert_eq!(
        gic.mmio.gicd_read(isenabler_offset(42)) & isenabler_bit(42),
        isenabler_bit(42)
    );
}

#[test]
fn set_priority_writes_the_priority_byte() {
    // The watchdog self-sample lowers its FIQ below the timer priority
    // through this primitive; it must write exactly the requested byte
    // into the INTID's `GICD_IPRIORITYR` slot, over the enable default.
    let gic = Gicv2::new(MockGicMmio::new());
    gic.enable_intid(27);
    gic.set_priority(27, 0xC0);
    assert_eq!(gic.mmio.gicd_read(GICD_IPRIORITYR + 27), 0xC0);
}

#[test]
fn disable_intid_sets_clear_enable_bit() {
    let gic = Gicv2::new(MockGicMmio::new());
    gic.disable_intid(42);
    assert_eq!(
        gic.mmio.gicd_read(icenabler_offset(42)) & isenabler_bit(42),
        isenabler_bit(42)
    );
}

#[test]
fn igroupr_offset_selects_the_first_word_for_ppis() {
    // The SGIs/PPIs (INTID 0..32) all live in the banked first word
    // `GICD_IGROUPR0` at base 0x080; INTID 32 spills into the next.
    assert_eq!(igroupr_offset(0), 0x080);
    assert_eq!(igroupr_offset(27), 0x080);
    assert_eq!(igroupr_offset(31), 0x080);
    assert_eq!(igroupr_offset(32), 0x084);
}

#[test]
fn gicc_ctlr_group0_fiq_bits_match_the_gicv2_spec() {
    // Single-Security-state view: EnableGrp0 = bit 0, FIQEn = bit 3.
    assert_eq!(GICC_CTLR_ENABLE_GRP0, 0b0001);
    assert_eq!(GICC_CTLR_FIQEN, 0b1000);
}

#[test]
fn set_group0_clears_only_the_target_group_bit() {
    let gic = Gicv2::new(MockGicMmio::new());
    // Start with every bit in IGROUPR0 set to Group 1 (the reset for a
    // Non-secure view), then move only the watchdog PPI (27) to Group 0
    // and confirm no sibling bit changed.
    gic.mmio.gicd_write(igroupr_offset(27), 0xFFFF_FFFF);
    gic.set_group(27, true);
    // Every bit stays set except PPI 27, now Group 0.
    assert_eq!(gic.mmio.gicd_read(igroupr_offset(27)), !(1u32 << 27));
}

#[test]
fn set_group1_restores_only_the_target_group_bit() {
    let gic = Gicv2::new(MockGicMmio::new());
    // From all-Group-0 (0), returning PPI 27 to Group 1 sets exactly
    // its bit — the fail-closed revert of the probe.
    gic.set_group(27, false);
    assert_eq!(gic.mmio.gicd_read(igroupr_offset(27)), 1 << 27);
}

#[test]
fn route_selfsample_fiq_isolates_one_line_to_group0_and_keeps_the_rest_irq() {
    // The debug watchdog delivers its cadence PPI as a Group-0 FIQ by
    // setting the *global* GICC_CTLR.FIQEn, which routes every Group-0
    // interrupt to FIQ. To FIQ a single line without storming the
    // (unserviced-as-FIQ) preemption timer, every other interrupt must
    // be Group 1: only `fiq_intid` stays Group 0, the rest are Group 1
    // (IRQ), both groups are enabled, and AckCtl is set so the Group-1
    // IRQs still ACK through GICC_IAR instead of returning the reserved
    // id 1022. The sweep is bounded by GICD_TYPER.ITLinesNumber.
    let gic = Gicv2::new(MockGicMmio::new());
    // Report 3 word-blocks (ITLinesNumber = 2 -> words 0..=2).
    gic.mmio.gicd_write(GICD_TYPER, 2);
    gic.route_selfsample_fiq(27);
    // Word 0 is all Group 1 except the watchdog PPI (27), now Group 0.
    assert_eq!(gic.mmio.gicd_read(GICD_IGROUPR), GROUP1_ALL & !(1u32 << 27));
    // The remaining implemented words are all Group 1.
    assert_eq!(gic.mmio.gicd_read(GICD_IGROUPR + 4), GROUP1_ALL);
    assert_eq!(gic.mmio.gicd_read(GICD_IGROUPR + 8), GROUP1_ALL);
    // A word beyond the implemented count is never touched.
    assert_eq!(gic.mmio.gicd_read(GICD_IGROUPR + 12), 0);
    // The CPU interface enables both groups + AckCtl + FIQEn.
    let cpu = gic.mmio.gicc_read(GICC_CTLR);
    assert_eq!(
        cpu,
        GICC_CTLR_ENABLE_GRP0 | GICC_CTLR_ENABLE_GRP1 | GICC_CTLR_ACKCTL | GICC_CTLR_FIQEN
    );
    // The distributor forwards both groups.
    assert_eq!(
        gic.mmio.gicd_read(GICD_CTLR),
        GICC_CTLR_ENABLE_GRP0 | GICC_CTLR_ENABLE_GRP1
    );
}

#[test]
fn gicc_and_gicd_ctlr_round_trip() {
    let gic = Gicv2::new(MockGicMmio::new());
    let ctlr = GICC_CTLR_ENABLE_GRP0 | (1 << 1) | GICC_CTLR_FIQEN;
    gic.write_gicc_ctlr(ctlr);
    assert_eq!(gic.read_gicc_ctlr(), ctlr);
    gic.write_gicd_ctlr(0b11);
    assert_eq!(gic.read_gicd_ctlr(), 0b11);
}

#[test]
fn acknowledge_returns_the_full_iar_including_the_sgi_source_cpu() {
    let gic = Gicv2::new(MockGicMmio::new());
    // A real IAR carries the source CPU in bits [12:10] for an SGI.
    // `acknowledge` must return the *whole* value: the INTID field is
    // recovered with `IAR_INTID_MASK`, but the source-CPU bits are
    // preserved so the matching EOIR deactivates the SGI.
    let iar = (0b101 << 10) | 0x2A;
    gic.mmio.gicc_write(GICC_IAR, iar);
    let acknowledged = gic.acknowledge().unwrap();
    assert_eq!(acknowledged.token(), iar);
    assert_eq!(acknowledged.intid, 0x2A);
}

#[test]
fn end_of_interrupt_writes_the_source_cpu_field_back_for_an_sgi() {
    // Regression: an SGI (reschedule IPI) sent from a non-zero CPU
    // must be deactivated by writing the *full* IAR back to EOIR,
    // source-CPU field included. Writing only the masked INTID left
    // the SGI active on the target's CPU interface, wedging every
    // further interrupt (timer/watchdog) and hanging that core under
    // IPI-heavy load. Prove the whole value round-trips to EOIR.
    let gic = Gicv2::new(MockGicMmio::new());
    let iar = 0b010 << 10; // SGI 0 (INTID field 0) from source CPU 2.
    gic.mmio.gicc_write(GICC_IAR, iar);
    let claimed = gic.acknowledge().unwrap();
    gic.end_of_interrupt(claimed.token());
    assert_eq!(gic.mmio.gicc_read(GICC_EOIR), iar);
    assert_ne!(
        gic.mmio.gicc_read(GICC_EOIR) & !IAR_INTID_MASK,
        0,
        "the source-CPU field must survive to EOIR"
    );
}

#[test]
fn claim_returns_the_full_iar_cookie_and_maps_spurious_to_none() {
    use tairix_arch_api::InterruptEntry;
    let c = GicController::new(Gicv2::new(MockGicMmio::new()), 1019);
    // A real SGI from source CPU 3: claim yields the full cookie, and
    // completing it writes that cookie (source CPU included) to EOIR.
    let iar = 0b011 << 10; // SGI 0 (INTID field 0) from source CPU 3.
    c.gic.mmio.gicc_write(GICC_IAR, iar);
    assert_eq!(c.claim(), Some(iar));
    c.complete(iar);
    assert_eq!(c.gic.mmio.gicc_read(GICC_EOIR), iar);
    // A spurious acknowledge (INTID field == SPURIOUS) claims nothing.
    c.gic.mmio.gicc_write(GICC_IAR, SPURIOUS_INTID);
    assert_eq!(c.claim(), None);
}

#[test]
fn controller_clamps_max_intid_to_the_spec_ceiling() {
    let c = GicController::new(Gicv2::new(MockGicMmio::new()), u32::MAX);
    assert_eq!(c.max_intid(), MAX_INTID);
}

#[test]
fn stuck_spi_is_none_when_nothing_is_active_or_pending() {
    let gic = Gicv2::new(MockGicMmio::new());
    assert_eq!(gic.stuck_spi(MAX_INTID), None);
}

#[test]
fn stuck_spi_names_the_lowest_active_line() {
    let gic = Gicv2::new(MockGicMmio::new());
    // SPI 37 active: word covering 32..64 with bit (37-32)=5 set — a
    // handler in flight, a genuine hard-lockup suspect.
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISACTIVER, 37), 1 << 5);
    assert_eq!(
        gic.stuck_spi(MAX_INTID),
        Some(StuckInterrupt {
            intid: 37,
            active: true,
        })
    );
}

#[test]
fn stuck_spi_reports_an_enabled_pending_line_when_none_is_active() {
    let gic = Gicv2::new(MockGicMmio::new());
    // SPI 50 pending (bit 50-32=18 in the 32..64 pending word) and
    // still enabled: asserted, deliverable, and so a real suspect.
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISPENDR, 50), 1 << 18);
    gic.mmio.gicd_write(isenabler_offset(50), isenabler_bit(50));
    assert_eq!(
        gic.stuck_spi(MAX_INTID),
        Some(StuckInterrupt {
            intid: 50,
            active: false,
        })
    );
}

#[test]
fn stuck_spi_skips_a_masked_pending_line() {
    let gic = Gicv2::new(MockGicMmio::new());
    // SPI 50 pending but with no enable bit set: masked, so it cannot
    // reach a CPU and can never be the wedge. It must not be reported
    // (the recurring spurious `stuck_irq=111` this fix closes).
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISPENDR, 50), 1 << 18);
    assert_eq!(gic.stuck_spi(MAX_INTID), None);
    // A higher pending line that *is* enabled is still found, skipping
    // the lower masked one rather than stopping at it.
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISPENDR, 55), 1 << 23);
    gic.mmio.gicd_write(isenabler_offset(55), isenabler_bit(55));
    assert_eq!(
        gic.stuck_spi(MAX_INTID),
        Some(StuckInterrupt {
            intid: 55,
            active: false,
        })
    );
}

#[test]
fn stuck_spi_prefers_an_active_line_over_a_lower_pending_one() {
    let gic = Gicv2::new(MockGicMmio::new());
    // A higher line stuck *active* is the stronger hard-lockup signal
    // than a lower one merely pending, so active wins outright.
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISACTIVER, 96), 1 << 0);
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISPENDR, 40), 1 << 8);
    gic.mmio.gicd_write(isenabler_offset(40), isenabler_bit(40));
    assert_eq!(
        gic.stuck_spi(MAX_INTID),
        Some(StuckInterrupt {
            intid: 96,
            active: true,
        })
    );
}

#[test]
fn stuck_spi_ignores_sgi_ppi_status_and_out_of_range_bits() {
    let gic = Gicv2::new(MockGicMmio::new());
    // Bits in the first word (SGIs/PPIs, id 0..32) are banked per CPU
    // and must not be reported. Only the SPI range is scanned.
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISACTIVER, 0), 0xF);
    assert_eq!(gic.stuck_spi(MAX_INTID), None);
    // A line above the controller's max is out of range and ignored.
    gic.mmio
        .gicd_write(gicd_bit_word_offset(GICD_ISACTIVER, 40), 1 << 8);
    assert_eq!(gic.stuck_spi(39), None);
    assert_eq!(
        gic.stuck_spi(48),
        Some(StuckInterrupt {
            intid: 40,
            active: true,
        })
    );
}

/// / W3: the GIC controller passes the shared Arch HAL
/// interrupt-controller + interrupt-entry conformance verticals over
/// its real handle (`plans/WIRING.md` Stage W3). INTID 42 is an
/// addressable SPI; 2000 is above [`MAX_INTID`]. The mock's `GICC_IAR`
/// is seeded with [`SPURIOUS_INTID`] so the [`InterruptEntry`] drain
/// terminates ("nothing pending").
#[test]
fn gic_controller_passes_arch_hal_irq_conformance() {
    use tairix_arch_api::{InterruptEntry, IrqController};

    let c = GicController::new(Gicv2::new(MockGicMmio::new()), 1019);
    c.gic.mmio.gicc_write(GICC_IAR, SPURIOUS_INTID);

    tairix_arch_api::irq::conformance::run_controller(&c, 42, 2000);
    tairix_arch_api::irq::conformance::run_entry(&c);

    // Object-safe behind `&dyn`, the way the kernel reaches it.
    let dyn_ctrl: &dyn IrqController = &c;
    assert_eq!(dyn_ctrl.mask(42), Ok(()));
    let dyn_entry: &dyn InterruptEntry = &c;
    assert_eq!(dyn_entry.claim(), None);
}

#[test]
fn gic_compatible_matches_both_versions_and_nothing_else() {
    assert!(is_gic_compatible(b"arm,cortex-a15-gic"));
    assert!(is_gic_compatible(b"arm,gic-400"));
    assert!(is_gic_compatible(b"arm,gic-v3"));
    assert!(!is_gic_compatible(b"arm,gic-v3-its"));
    assert!(!is_gic_compatible(b""));
}

fn cells(values: &[u64]) -> std::vec::Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// QEMU `virt`'s GICv3 shape: the distributor, `regions` redistributor
/// regions, an ITS beneath, and a UART after the GIC's subtree.
fn gicv3_tree(
    declared: Option<u32>,
    regions: &[(u64, u64)],
    stride: Option<u64>,
) -> std::vec::Vec<u8> {
    let mut b = tairix_fdt::fixture::DtbBuilder::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("intc@8000000");
    b.prop_str("compatible", "arm,gic-v3");
    b.prop_u32("phandle", 0x8002);
    b.prop("interrupt-controller", &[]);
    b.prop_u32("#interrupt-cells", 3);
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.prop("ranges", &[]);
    if let Some(declared) = declared {
        b.prop_u32("#redistributor-regions", declared);
    }
    if let Some(stride) = stride {
        b.prop("redistributor-stride", &stride.to_be_bytes());
    }
    let mut reg = std::vec![0x0800_0000, 0x1_0000];
    for &(base, len) in regions {
        reg.extend([base, len]);
    }
    b.prop("reg", &cells(&reg));
    b.begin_node("its@8080000");
    b.prop_str("compatible", "arm,gic-v3-its");
    b.prop_u32("phandle", 0x8003);
    b.prop("msi-controller", &[]);
    b.prop("reg", &cells(&[0x0808_0000, 0x2_0000]));
    b.end_node();
    b.end_node();
    b.begin_node("serial@9000000");
    b.prop_str("compatible", "arm,pl011");
    b.prop("reg", &cells(&[0x0900_0000, 0x1000]));
    b.end_node();
    b.end_node();
    b.build()
}

const QEMU_REDISTRIBUTORS: (u64, u64) = (0x080A_0000, 0xF6_0000);

#[test]
fn finds_a_gicv3_distributor_with_no_memory_mapped_cpu_interface() {
    let blob = gicv3_tree(None, &[QEMU_REDISTRIBUTORS], None);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let gic = find_gic(&fdt).expect("a GIC is present");
    assert_eq!(gic.version, GicVersion::V3);
    assert_eq!(gic.gicd_base, 0x0800_0000);
    assert_eq!(gic.gicc_base, None);
    assert_eq!(gic.phandle, Some(0x8002));
}

#[test]
fn a_gicv3_s_windows_are_its_distributor_redistributors_and_its_and_nothing_past_it() {
    let blob = gicv3_tree(None, &[QEMU_REDISTRIBUTORS], None);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let mut windows = std::vec::Vec::new();
    for_each_window(&fdt, |base, len| windows.push((base, len)));
    assert_eq!(
        windows,
        [
            (0x0800_0000, 0x1_0000),
            QEMU_REDISTRIBUTORS,
            (0x0808_0000, 0x2_0000)
        ]
    );
}

#[test]
fn each_translation_service_beneath_a_gicv3_is_found_with_its_phandle() {
    let blob = gicv3_tree(None, &[QEMU_REDISTRIBUTORS], None);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let mut services = std::vec::Vec::new();
    for_each_its(&fdt, |phandle, base, len| {
        services.push((phandle, base, len));
    });
    assert_eq!(services, [(Some(0x8003), 0x0808_0000, 0x2_0000)]);
    let blob = tairix_fdt::fixture::virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30);
    let mut none = 0;
    for_each_its(&Fdt::new(&blob).expect("valid fdt"), |_, _, _| none += 1);
    assert_eq!(none, 0, "a GICv2 has no translation service");
}

#[test]
fn a_gicv2_s_windows_are_its_distributor_and_cpu_interface() {
    let blob = tairix_fdt::fixture::virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let mut windows = std::vec::Vec::new();
    for_each_window(&fdt, |base, _| windows.push(base));
    assert_eq!(
        windows,
        [DEFAULT_GICD_BASE as u64, DEFAULT_GICC_BASE as u64]
    );
}

#[test]
fn redistributor_regions_follow_the_count_the_tree_names_and_its_stride() {
    let two = [QEMU_REDISTRIBUTORS, (0x40_0000_0000, 0x400_0000)];
    let blob = gicv3_tree(Some(2), &two, Some(0x4_0000));
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let mut regions = std::vec::Vec::new();
    let stride = redistributor_regions(&fdt, |r| regions.push((r.base, r.len)));
    assert_eq!(stride, Some(Some(0x4_0000)));
    assert_eq!(regions, two);
}

#[test]
fn a_redistributor_count_past_the_regions_reg_holds_is_capped() {
    let blob = gicv3_tree(Some(u32::MAX), &[QEMU_REDISTRIBUTORS], None);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let mut regions = 0;
    assert_eq!(redistributor_regions(&fdt, |_| regions += 1), Some(None));
    assert_eq!(regions, 1);
    let mut windows = 0;
    for_each_window(&fdt, |_, _| windows += 1);
    assert_eq!(windows, 3);
}

#[test]
fn a_gicv3_with_no_redistributor_region_or_a_gicv2_names_none() {
    let blob = gicv3_tree(Some(0), &[QEMU_REDISTRIBUTORS], None);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    assert_eq!(redistributor_regions(&fdt, |_| {}), None);
    let blob = tairix_fdt::fixture::virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    assert_eq!(redistributor_regions(&fdt, |_| {}), None);
}

#[test]
fn finds_gic_400_bases_in_a_raspi_tree() {
    // The Pi-shaped fixture carries a GIC-400 under `/soc` with bus
    // `reg` values; discovery translates them through the `ranges`
    // to the BCM2711 CPU-physical bases.
    let blob = tairix_fdt::fixture::raspi_like_arm(0x7e20_1000, 0x7e21_5040);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let gic = find_gic(&fdt).expect("a GIC is present");
    assert_eq!(gic.gicd_base, 0xff84_1000);
    assert_eq!(gic.gicc_base, Some(0xff84_2000));
    assert_eq!(gic.version, GicVersion::V2);
}

#[test]
fn finds_gicv2_bases_in_a_virt_tree() {
    // The `virt`-shaped fixture carries the GICv2 at the default bases.
    let blob = tairix_fdt::fixture::virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 30);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let gic = find_gic(&fdt).expect("a GIC is present");
    assert_eq!(usize::try_from(gic.gicd_base).unwrap(), DEFAULT_GICD_BASE);
    assert_eq!(gic.gicc_base, Some(DEFAULT_GICC_BASE as u64));
}

#[test]
fn no_gic_in_a_gicless_tree_is_none() {
    // A tree with only the two console UARTs (no `intc` node) yields
    // no GIC — the boot path then keeps the fail-safe default.
    let mut b = tairix_fdt::fixture::DtbBuilder::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("serial@9000000");
    b.prop_str("compatible", "arm,pl011");
    let mut reg = std::vec::Vec::new();
    reg.extend_from_slice(&0x0900_0000u64.to_be_bytes());
    reg.extend_from_slice(&0x1000u64.to_be_bytes());
    b.prop("reg", &reg);
    b.end_node();
    b.end_node();
    let blob = b.build();
    let fdt = Fdt::new(&blob).expect("valid fdt");
    assert_eq!(find_gic(&fdt), None);
}

#[test]
fn configure_from_fdt_applies_the_discovered_bases() {
    // Drive the global config through a Pi-shaped FDT and read it
    // back. This test owns the global GIC base slot for its duration;
    // the other tests here either exercise pure helpers (`find_gic`)
    // or the mock MMIO, so there is no cross-test interference.
    let blob = tairix_fdt::fixture::raspi_like_arm(0x7e20_1000, 0x7e21_5040);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    let applied = configure_from_fdt(&fdt).expect("GIC discovered");
    assert_eq!(applied.gicd_base, 0xff84_1000);
    assert_eq!(current(), (0xff84_1000, 0xff84_2000));
    assert_eq!(version(), GicVersion::V2);

    let blob = gicv3_tree(None, &[QEMU_REDISTRIBUTORS], None);
    let fdt = Fdt::new(&blob).expect("valid fdt");
    configure_from_fdt(&fdt).expect("GIC discovered");
    assert_eq!(version(), GicVersion::V3);
    assert_eq!(current().0, 0x0800_0000);

    // Nothing else on the host reads the process-wide configuration.
    configure(DEFAULT_GICD_BASE, DEFAULT_GICC_BASE);
}

#[test]
fn a_cpu_is_addressable_only_once_it_records_itself() {
    static CPUS: [GicCpu; 2] = [GicCpu::new(), GicCpu::new()];
    let topology = GicTopology::new(&CPUS);
    assert_eq!(topology.cpu(0).unwrap().target(), None);
    assert_eq!(topology.cpu(0).unwrap().redistributor(), None);
    assert!(matches!(topology.cpu(2), Err(GicError::UnknownCpu)));

    topology.cpu(1).unwrap().record(0x0102, 0x080C_0000);
    assert_eq!(topology.cpu(1).unwrap().target(), Some(0x0102));
    assert_eq!(topology.cpu(1).unwrap().redistributor(), Some(0x080C_0000));
    // A GICv2 interface mask records alike, mask zero included.
    topology.cpu(0).unwrap().record(0, 0);
    assert_eq!(topology.cpu(0).unwrap().target(), Some(0));
}

#[test]
fn a_gicv3_topology_carries_its_redistributor_regions_and_stride() {
    static CPUS: [GicCpu; 1] = [GicCpu::new()];
    static REGIONS: [RedistributorRegion; 1] = [RedistributorRegion {
        base: 0x080A_0000,
        len: 0xF6_0000,
    }];
    let topology = GicTopology::new(&CPUS).with_redistributors(&REGIONS, Some(0x4_0000));
    assert_eq!(topology.redistributors, &REGIONS);
    assert_eq!(topology.redistributor_stride, Some(0x4_0000));
    assert_eq!(topology.cpus.len(), 1);
}
