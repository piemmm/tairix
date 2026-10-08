//! Host tests for the GICv3 driver, over a register-level model of a
//! distributor, its redistributors and one CPU's interface.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::vec::Vec;

use super::*;
use crate::gic::{GicController, Gicv3Local};

const GICV3_PIDR2: u64 = 0x3B;
const GICV4_PIDR2: u64 = 0x4B;
const RD0: usize = 0x080A_0000;

/// One recorded register write, in issue order.
#[derive(Clone, Debug, PartialEq)]
enum Write {
    Distributor(usize, u64),
    Redistributor(usize, usize, u64),
}

#[derive(Default)]
struct ModelState {
    distributor: HashMap<usize, u64>,
    redistributors: HashMap<(usize, usize), u64>,
    writes: Vec<Write>,
    /// Redistributors whose `ChildrenAsleep` never clears.
    sleepy: Vec<usize>,
    /// Redistributors whose LPI tables share nothing with the CPU.
    unshared: Vec<usize>,
}

/// A distributor and the redistributors placed in it by [`Model::place`].
struct Model(Mutex<ModelState>);

impl Model {
    fn new(itlines: u32) -> Self {
        let model = Self(Mutex::new(ModelState::default()));
        model.set_distributor(GICD_TYPER, u64::from(itlines));
        model.set_distributor(PIDR2, GICV3_PIDR2);
        model
    }

    fn set_distributor(&self, off: usize, value: u64) {
        self.0.lock().unwrap().distributor.insert(off, value);
    }

    fn distributor(&self, off: usize) -> u64 {
        *self.0.lock().unwrap().distributor.get(&off).unwrap_or(&0)
    }

    fn redistributor(&self, rd: usize, off: usize) -> u64 {
        *self
            .0
            .lock()
            .unwrap()
            .redistributors
            .get(&(rd, off))
            .unwrap_or(&0)
    }

    /// A redistributor at `rd` serving `mpidr`, the last of its region when
    /// `last`, with GICv4's two extra frames when `vlpis`.
    fn place(&self, rd: usize, mpidr: u64, last: bool, vlpis: bool) {
        let a = Affinity::of_mpidr(mpidr).routing();
        let packed = (a & 0x00FF_FFFF) | (((a >> 32) & 0xFF) << 24);
        let mut typer = packed << 32;
        if last {
            typer |= GICR_TYPER_LAST;
        }
        if vlpis {
            typer |= GICR_TYPER_VLPIS;
        }
        let mut state = self.0.lock().unwrap();
        state.redistributors.insert((rd, GICR_TYPER), typer);
        state
            .redistributors
            .insert((rd, PIDR2), if vlpis { GICV4_PIDR2 } else { GICV3_PIDR2 });
        state.redistributors.insert(
            (rd, GICR_WAKER),
            u64::from(GICR_WAKER_PROCESSOR_SLEEP | GICR_WAKER_CHILDREN_ASLEEP),
        );
    }

    fn writes(&self) -> Vec<Write> {
        self.0.lock().unwrap().writes.clone()
    }
}

impl Gicv3Mmio for &Model {
    fn distributor_read(&self, off: usize) -> u32 {
        u32::try_from(self.distributor(off) & 0xFFFF_FFFF).unwrap()
    }
    fn distributor_write(&self, off: usize, value: u32) {
        let mut state = self.0.lock().unwrap();
        state.writes.push(Write::Distributor(off, u64::from(value)));
        let stored = match off {
            // Write-one-to-clear and write-one-to-set banks keep one image.
            o if (GICD_ICENABLER..GICD_ICENABLER + 0x80).contains(&o) => {
                let set = o - (GICD_ICENABLER - crate::gic::GICD_ISENABLER);
                let old = *state.distributor.get(&set).unwrap_or(&0);
                state.distributor.insert(set, old & !u64::from(value));
                return;
            }
            o if (crate::gic::GICD_ISENABLER..crate::gic::GICD_ISENABLER + 0x80).contains(&o) => {
                *state.distributor.get(&o).unwrap_or(&0) | u64::from(value)
            }
            _ => u64::from(value),
        };
        state.distributor.insert(off, stored);
    }
    fn distributor_write_u64(&self, off: usize, value: u64) {
        let mut state = self.0.lock().unwrap();
        state.writes.push(Write::Distributor(off, value));
        state.distributor.insert(off, value);
    }
    fn distributor_write_byte(&self, off: usize, value: u8) {
        let mut state = self.0.lock().unwrap();
        state.writes.push(Write::Distributor(off, u64::from(value)));
        state.distributor.insert(off, u64::from(value));
    }
    fn redistributor_read(&self, rd: usize, off: usize) -> u32 {
        u32::try_from(self.redistributor(rd, off) & 0xFFFF_FFFF).unwrap()
    }
    fn redistributor_read_u64(&self, rd: usize, off: usize) -> u64 {
        self.redistributor(rd, off)
    }
    fn redistributor_write(&self, rd: usize, off: usize, value: u32) {
        let mut state = self.0.lock().unwrap();
        state
            .writes
            .push(Write::Redistributor(rd, off, u64::from(value)));
        let mut stored = u64::from(value);
        if off == GICR_WAKER
            && value & GICR_WAKER_PROCESSOR_SLEEP == 0
            && !state.sleepy.contains(&rd)
        {
            stored &= !u64::from(GICR_WAKER_CHILDREN_ASLEEP);
        }
        let set = SGI_FRAME + crate::gic::GICD_ISENABLER;
        if off == SGI_FRAME + GICD_ICENABLER {
            let old = *state.redistributors.get(&(rd, set)).unwrap_or(&0);
            state.redistributors.insert((rd, set), old & !stored);
            return;
        }
        if off == set {
            stored |= *state.redistributors.get(&(rd, set)).unwrap_or(&0);
        }
        state.redistributors.insert((rd, off), stored);
    }
    fn redistributor_write_byte(&self, rd: usize, off: usize, value: u8) {
        let mut state = self.0.lock().unwrap();
        state
            .writes
            .push(Write::Redistributor(rd, off, u64::from(value)));
        state.redistributors.insert((rd, off), u64::from(value));
    }
    fn redistributor_write_u64(&self, rd: usize, off: usize, value: u64) {
        let mut state = self.0.lock().unwrap();
        state.writes.push(Write::Redistributor(rd, off, value));
        let mut stored = value & !GICR_PENDBASER_PTZ;
        if state.unshared.contains(&rd) {
            stored &= !TABLE_SHAREABILITY;
        }
        state.redistributors.insert((rd, off), stored);
    }
}

#[derive(Default)]
struct CpuState {
    sre: u64,
    sre_locked: bool,
    priority_mask: u64,
    binary_point: u64,
    control: u64,
    group0: u64,
    group1: u64,
    pending1: VecDeque<u32>,
    pending0: VecDeque<u32>,
    ended1: Vec<u32>,
    ended0: Vec<u32>,
    sgis: Vec<u64>,
    mpidr: u64,
}

/// One CPU's interface.
#[derive(Default)]
struct Cpu(Mutex<CpuState>);

impl Cpu {
    fn with_mpidr(mpidr: u64) -> Self {
        let cpu = Self::default();
        cpu.0.lock().unwrap().mpidr = mpidr;
        cpu
    }
}

impl CpuInterface for &Cpu {
    fn sre(&self) -> u64 {
        self.0.lock().unwrap().sre
    }
    fn set_sre(&self, value: u64) {
        let mut state = self.0.lock().unwrap();
        if !state.sre_locked {
            state.sre = value;
        }
    }
    fn set_priority_mask(&self, value: u64) {
        self.0.lock().unwrap().priority_mask = value;
    }
    fn set_binary_point(&self, value: u64) {
        self.0.lock().unwrap().binary_point = value;
    }
    fn control(&self) -> u64 {
        self.0.lock().unwrap().control
    }
    fn set_control(&self, value: u64) {
        self.0.lock().unwrap().control = value;
    }
    fn group0_enable(&self) -> u64 {
        self.0.lock().unwrap().group0
    }
    fn set_group0_enable(&self, value: u64) {
        self.0.lock().unwrap().group0 = value;
    }
    fn set_group1_enable(&self, value: u64) {
        self.0.lock().unwrap().group1 = value;
    }
    fn acknowledge_group1(&self) -> u32 {
        self.0.lock().unwrap().pending1.pop_front().unwrap_or(1023)
    }
    fn end_group1(&self, intid: u32) {
        self.0.lock().unwrap().ended1.push(intid);
    }
    fn acknowledge_group0(&self) -> u32 {
        self.0.lock().unwrap().pending0.pop_front().unwrap_or(1023)
    }
    fn end_group0(&self, intid: u32) {
        self.0.lock().unwrap().ended0.push(intid);
    }
    fn raise_group1_sgi(&self, value: u64) {
        self.0.lock().unwrap().sgis.push(value);
    }
    fn mpidr(&self) -> u64 {
        self.0.lock().unwrap().mpidr
    }
    fn synchronize(&self) {}
}

#[test]
fn an_affinity_keeps_the_four_fields_and_drops_the_mpidr_flags() {
    // MT (bit 24), U (bit 30) and the RES1 bit 31 are not affinity.
    let mpidr = (1 << 31) | (1 << 30) | (1 << 24) | (0x7 << 32) | 0x02_0305;
    assert_eq!(Affinity::of_mpidr(mpidr).routing(), (0x7 << 32) | 0x02_0305);
}

#[test]
fn a_redistributor_s_typer_names_the_same_affinity_as_mpidr() {
    let typer = 0x0702_0305_u64 << 32;
    assert_eq!(
        Affinity::of_redistributor(typer),
        Affinity::of_mpidr((0x7 << 32) | 0x02_0305)
    );
}

#[test]
fn an_sgi_names_every_affinity_level_and_the_one_target() {
    let to = Affinity::of_mpidr((0x7 << 32) | 0x02_0305);
    let value = to.sgi(0);
    assert_eq!(value >> 48 & 0xFF, 0x7, "Aff3");
    assert_eq!(value >> 32 & 0xFF, 0x02, "Aff2");
    assert_eq!(value >> 16 & 0xFF, 0x03, "Aff1");
    assert_eq!(value & 0xFFFF, 1 << 5, "target list");
    assert_eq!(value >> 44 & 0xF, 0, "range selector");
    assert_eq!(to.sgi(9) >> 24 & 0xF, 9, "INTID");
}

#[test]
fn an_aff0_past_fifteen_is_reached_by_range_selector_or_refused() {
    let model = Model::new(1);
    let cpu = Cpu::with_mpidr(0);
    let gic = Gicv3::new(&model, &cpu);
    let to = Affinity::of_mpidr(0x23);
    assert_eq!(gic.send_sgi(0, to), Err(Gicv3Error::Unaddressable));
    assert!(cpu.0.lock().unwrap().sgis.is_empty());

    model.set_distributor(GICD_TYPER, u64::from(GICD_TYPER_RSS) | 1);
    cpu.0.lock().unwrap().control = ICC_CTLR_RSS;
    assert_eq!(gic.send_sgi(0, to), Ok(()));
    let raised = cpu.0.lock().unwrap().sgis[0];
    assert_eq!(raised >> 44 & 0xF, 2);
    assert_eq!(raised & 0xFFFF, 1 << 3);
}

#[test]
fn a_distributor_of_another_revision_is_refused_untouched() {
    let model = Model::new(1);
    model.set_distributor(PIDR2, 0x2B);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    assert_eq!(
        gic.init_distributor(Affinity::of_mpidr(0)),
        Err(Gicv3Error::NotGicv3)
    );
    assert!(model.writes().is_empty());
}

#[test]
fn the_distributor_comes_up_with_every_spi_off_in_group1_and_routed_after_affinity_routing() {
    let model = Model::new(2);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    let boot = Affinity::of_mpidr(0x0102);
    gic.init_distributor(boot).unwrap();

    // Two `ITLinesNumber` words beyond the first: SPIs 32..96.
    for word in [GICD_ICENABLER + 4, GICD_ICENABLER + 8] {
        assert!(model
            .writes()
            .contains(&Write::Distributor(word, u64::from(u32::MAX))));
    }
    assert_eq!(model.distributor(GICD_IGROUPR + 4), u64::from(u32::MAX));
    assert_eq!(model.distributor(GICD_IROUTER + 8 * 95), boot.routing());
    assert_eq!(model.distributor(GICD_IROUTER + 8 * 96), 0);
    assert_eq!(
        model.distributor(GICD_IPRIORITYR + 40),
        u64::from(MID_RANGE_PRIORITY)
    );

    let writes = model.writes();
    let are = writes
        .iter()
        .position(|w| *w == Write::Distributor(GICD_CTLR, u64::from(GICD_CTLR_ARE)))
        .unwrap();
    let first_route = writes
        .iter()
        .position(|w| matches!(w, Write::Distributor(off, _) if *off == GICD_IROUTER + 8 * 32))
        .unwrap();
    assert!(
        are < first_route,
        "IROUTER is RES0 until affinity routing is on"
    );
    assert_eq!(
        model.distributor(GICD_CTLR),
        u64::from(GICD_CTLR_ARE | GICD_CTLR_ENABLE_GRP1)
    );
}

#[test]
fn a_distributor_reporting_more_lines_than_the_architecture_stops_at_1019() {
    let model = Model::new(GICD_TYPER_ITLINES_MASK);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    let boot = Affinity::of_mpidr(0x0101);
    gic.init_distributor(boot).unwrap();
    assert!(model
        .writes()
        .iter()
        .all(|w| !matches!(w, Write::Distributor(off, _) if *off == GICD_IROUTER + 8 * 1020)));
    assert_eq!(model.distributor(GICD_IROUTER + 8 * 1019), boot.routing());
}

#[test]
fn a_cpu_finds_its_redistributor_across_regions_and_strides() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    // Region one: two GICv3 redistributors; region two: GICv4 frames.
    model.place(RD0, 0x0, false, false);
    model.place(RD0 + 0x2_0000, 0x1, true, false);
    let second = 0x4000_0000;
    model.place(second, 0x100, false, true);
    model.place(second + 0x4_0000, 0x101, true, true);
    let regions = [
        RedistributorRegion {
            base: RD0 as u64,
            len: 0x4_0000,
        },
        RedistributorRegion {
            base: second as u64,
            len: 0x8_0000,
        },
    ];
    let find = |mpidr| gic.find_redistributor(&regions, None, Affinity::of_mpidr(mpidr));
    assert_eq!(find(0x1), Some(RD0 + 0x2_0000));
    assert_eq!(find(0x101), Some(second + 0x4_0000));
    assert_eq!(find(0x2), None);
}

#[test]
fn a_named_stride_overrides_the_architecture_s_spacing() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    model.place(RD0, 0x0, false, false);
    model.place(RD0 + 0x8_0000, 0x1, true, false);
    let region = [RedistributorRegion {
        base: RD0 as u64,
        len: 0x10_0000,
    }];
    assert_eq!(
        gic.find_redistributor(&region, Some(0x8_0000), Affinity::of_mpidr(1)),
        Some(RD0 + 0x8_0000)
    );
}

#[test]
fn a_walk_stops_at_the_last_frame_the_region_end_or_a_frame_that_is_no_redistributor() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    model.place(RD0, 0x0, true, false);
    // Past the `Last` frame: never read.
    model.place(RD0 + 0x2_0000, 0x1, false, false);
    let region = |len| {
        [RedistributorRegion {
            base: RD0 as u64,
            len,
        }]
    };
    assert_eq!(
        gic.find_redistributor(&region(0x8_0000), None, Affinity::of_mpidr(1)),
        None
    );
    // A region too short for one redistributor's two frames.
    assert_eq!(
        gic.find_redistributor(&region(0x1_0000), None, Affinity::of_mpidr(0)),
        None
    );
    // A frame answering another revision ends the region.
    let model = Model::new(1);
    let gic = Gicv3::new(&model, &cpu);
    model.place(RD0 + 0x2_0000, 0x1, true, false);
    assert_eq!(
        gic.find_redistributor(&region(0x8_0000), None, Affinity::of_mpidr(1)),
        None
    );
}

#[test]
fn a_redistributor_wakes_and_resets_its_private_interrupts() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    model.place(RD0, 0x0, true, false);
    gic.init_redistributor(RD0).unwrap();
    assert_eq!(
        model.redistributor(RD0, GICR_WAKER)
            & u64::from(GICR_WAKER_PROCESSOR_SLEEP | GICR_WAKER_CHILDREN_ASLEEP),
        0
    );
    assert!(model.writes().contains(&Write::Redistributor(
        RD0,
        SGI_FRAME + GICD_ICENABLER,
        u64::from(u32::MAX)
    )));
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + GICD_IGROUPR),
        u64::from(u32::MAX)
    );
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + GICD_IPRIORITYR + 30),
        u64::from(MID_RANGE_PRIORITY)
    );
}

#[test]
fn a_redistributor_that_never_wakes_is_unresponsive() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    model.place(RD0, 0x0, true, false);
    model.0.lock().unwrap().sleepy.push(RD0);
    assert_eq!(gic.init_redistributor(RD0), Err(Gicv3Error::Unresponsive));
}

#[test]
fn the_cpu_interface_comes_up_open_with_group1_and_combined_end_of_interrupt() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    cpu.0.lock().unwrap().control = ICC_CTLR_EOIMODE;
    let gic = Gicv3::new(&model, &cpu);
    gic.init_cpu_interface().unwrap();
    let state = cpu.0.lock().unwrap();
    assert_eq!(state.sre, ICC_SRE_SRE | ICC_SRE_DFB | ICC_SRE_DIB);
    assert_eq!(state.priority_mask, 0xFF);
    assert_eq!(state.binary_point, 0);
    assert_eq!(state.control & ICC_CTLR_EOIMODE, 0);
    assert_eq!(state.group1, 1);
}

#[test]
fn a_system_register_interface_a_higher_level_holds_off_is_refused() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    cpu.0.lock().unwrap().sre_locked = true;
    let gic = Gicv3::new(&model, &cpu);
    assert_eq!(gic.init_cpu_interface(), Err(Gicv3Error::NoSystemRegisters));
    assert_eq!(cpu.0.lock().unwrap().group1, 0);
}

#[test]
fn a_private_interrupt_lives_in_the_redistributor_and_an_spi_in_the_distributor() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    gic.enable(RD0, 27);
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + crate::gic::GICD_ISENABLER),
        1 << 27
    );
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + GICD_IPRIORITYR + 27),
        u64::from(MID_RANGE_PRIORITY)
    );
    assert_eq!(model.distributor(crate::gic::GICD_ISENABLER), 0);
    assert!(gic.is_enabled(RD0, 27));

    gic.enable(RD0, 40);
    assert_eq!(model.distributor(isenabler_offset(40)), 1 << 8);
    gic.disable(RD0, 40);
    assert!(!gic.is_enabled(RD0, 40));

    gic.set_edge_triggered(RD0, 27, true);
    assert!(gic.is_edge_triggered(RD0, 27));
    assert_ne!(model.redistributor(RD0, SGI_FRAME + icfgr_offset(27)), 0);
    gic.set_group(RD0, 27, true);
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + GICD_IGROUPR) & (1 << 27),
        0
    );
}

#[test]
fn an_spi_route_names_its_cpu_and_a_private_interrupt_has_none() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    let to = Affinity::of_mpidr(0x0201);
    gic.route_spi(33, to);
    gic.route_spi(30, to);
    gic.route_spi(MAX_INTID + 1, to);
    assert_eq!(model.distributor(GICD_IROUTER + 8 * 33), to.routing());
    assert_eq!(
        model
            .writes()
            .iter()
            .filter(|w| matches!(w, Write::Distributor(off, _) if *off >= GICD_IROUTER))
            .count(),
        1
    );
}

#[test]
fn an_acknowledgement_of_a_special_intid_is_nothing_and_an_lpi_is_its_own_id() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    cpu.0
        .lock()
        .unwrap()
        .pending1
        .extend([1020, 1021, 1022, 1023, 8192]);
    let gic = Gicv3::new(&model, &cpu);
    for _ in 0..4 {
        assert_eq!(gic.acknowledge(), None);
    }
    assert_eq!(gic.acknowledge(), Some(8192));
    gic.end_of_interrupt(8192);
    assert_eq!(cpu.0.lock().unwrap().ended1, [8192]);
}

#[test]
fn a_fiq_route_moves_one_private_interrupt_to_group0_and_puts_back_what_it_found() {
    let model = Model::new(1);
    model.set_distributor(
        GICD_CTLR,
        u64::from(GICD_CTLR_ARE | GICD_CTLR_ENABLE_GRP1 | GICD_CTLR_DS),
    );
    let cpu = Cpu::default();
    cpu.0.lock().unwrap().pending0.push_back(27);
    let gic = Gicv3::new(&model, &cpu);
    assert!(gic.single_security_state());
    model.place(RD0, 0x0, true, false);
    gic.init_redistributor(RD0).unwrap();

    let saved = gic.fiq_route();
    gic.route_fiq(RD0, 27).unwrap();
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + GICD_IGROUPR),
        u64::from(!(1u32 << 27))
    );
    assert_ne!(
        model.distributor(GICD_CTLR) & u64::from(GICD_CTLR_ENABLE_GRP0),
        0
    );
    assert_eq!(cpu.0.lock().unwrap().group0, 1);
    assert_eq!(gic.acknowledge_fiq(), Some(27));
    gic.end_of_fiq(27);
    assert_eq!(cpu.0.lock().unwrap().ended0, [27]);

    gic.restore_fiq(RD0, 27, saved).unwrap();
    assert_eq!(
        model.redistributor(RD0, SGI_FRAME + GICD_IGROUPR),
        u64::from(u32::MAX)
    );
    assert_eq!(
        model.distributor(GICD_CTLR) & u64::from(GICD_CTLR_ENABLE_GRP0),
        0
    );
    assert_eq!(cpu.0.lock().unwrap().group0, 0);
}

#[test]
fn a_stuck_spi_is_read_from_the_distributor_banks() {
    let model = Model::new(1);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    model.set_distributor(crate::gic::GICD_ISACTIVER + 4, 1 << 6);
    assert_eq!(
        gic.stuck_spi(MAX_INTID),
        Some(StuckInterrupt {
            intid: 38,
            active: true
        })
    );
}

#[test]
fn the_controller_over_a_gicv3_passes_the_hal_conformance_and_keeps_the_trigger_rules() {
    let model = Model::new(GICD_TYPER_ITLINES_MASK);
    let cpu = Cpu::default();
    cpu.0.lock().unwrap().pending1.extend([30, 42, 8200]);
    let gic = Gicv3::new(&model, &cpu);
    let controller = GicController::new(Gicv3Local::new(&gic, RD0), MAX_INTID);
    tairix_arch_api::irq::conformance::run_controller(&controller, 42, 2000);
    tairix_arch_api::irq::conformance::run_entry(&controller);
    assert_eq!(cpu.0.lock().unwrap().ended1, [30, 42, 8200]);

    assert_eq!(controller.set_trigger(50, true), Ok(()));
    assert!(gic.is_edge_triggered(RD0, 50));
    gic.enable(RD0, 51);
    assert_eq!(
        controller.set_trigger(51, true),
        Err(crate::gic::TriggerRefused)
    );
    assert_eq!(
        controller.set_trigger(27, true),
        Err(crate::gic::TriggerRefused)
    );
}

/// A redistributor implementing physical LPIs, at `RD0`, processor number
/// 5.
fn lpi_redistributor() -> Model {
    let model = Model::new(2);
    model.place(RD0, 0, true, false);
    let typer =
        model.redistributor(RD0, GICR_TYPER) | GICR_TYPER_PLPIS | (5 << GICR_TYPER_PROCESSOR_SHIFT);
    model
        .0
        .lock()
        .unwrap()
        .redistributors
        .insert((RD0, GICR_TYPER), typer);
    model
}

const TABLES: LpiTables = LpiTables {
    properties: 0x4010_0000,
    pending: 0x4020_0000,
    id_bits: 14,
};

#[test]
fn lpis_are_turned_on_over_cacheable_tables_the_redistributor_shares() {
    let model = lpi_redistributor();
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    assert!(gic.lpis_available(RD0));
    assert_eq!(gic.enable_lpis(RD0, TABLES), Ok(true));
    assert_eq!(
        model.redistributor(RD0, GICR_PROPBASER),
        TABLES.properties
            | u64::from(TABLES.id_bits - 1)
            | TABLE_INNER_SHAREABLE
            | TABLE_INNER_WRITE_BACK
    );
    assert_eq!(
        model.redistributor(RD0, GICR_PENDBASER),
        0x4020_0000 | TABLE_INNER_SHAREABLE | TABLE_INNER_WRITE_BACK
    );
    assert!(
        model.writes().contains(&Write::Redistributor(
            RD0,
            GICR_PENDBASER,
            0x4020_0000 | GICR_PENDBASER_PTZ | TABLE_INNER_SHAREABLE | TABLE_INNER_WRITE_BACK
        )),
        "the pending table is said to be zero"
    );
    assert_ne!(
        model.redistributor(RD0, GICR_CTLR) & u64::from(GICR_CTLR_ENABLE_LPIS),
        0
    );
    assert!(
        !gic.lpis_available(RD0),
        "LPIs on are no longer free to take"
    );
}

#[test]
fn a_redistributor_sharing_nothing_reads_its_tables_uncached() {
    let model = lpi_redistributor();
    model.0.lock().unwrap().unshared.push(RD0);
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    assert_eq!(gic.enable_lpis(RD0, TABLES), Ok(false));
    assert_eq!(
        model.redistributor(RD0, GICR_PROPBASER),
        TABLES.properties | u64::from(TABLES.id_bits - 1) | TABLE_INNER_NON_CACHEABLE
    );
    assert_eq!(
        model.redistributor(RD0, GICR_PENDBASER),
        0x4020_0000 | TABLE_INNER_NON_CACHEABLE
    );
}

#[test]
fn a_redistributor_without_lpis_or_whose_lpis_firmware_left_on_is_refused() {
    let none = Model::new(2);
    none.place(RD0, 0, true, false);
    let cpu = Cpu::default();
    assert!(!Gicv3::new(&none, &cpu).lpis_available(RD0));
    assert_eq!(
        Gicv3::new(&none, &cpu).enable_lpis(RD0, TABLES),
        Err(Gicv3Error::NoLpis)
    );
    let on = lpi_redistributor();
    on.0.lock()
        .unwrap()
        .redistributors
        .insert((RD0, GICR_CTLR), u64::from(GICR_CTLR_ENABLE_LPIS));
    assert!(!Gicv3::new(&on, &cpu).lpis_available(RD0));
    assert_eq!(
        Gicv3::new(&on, &cpu).enable_lpis(RD0, TABLES),
        Err(Gicv3Error::LpisInUse)
    );
    assert!(
        !on.writes()
            .iter()
            .any(|write| matches!(write, Write::Redistributor(_, GICR_PROPBASER, _))),
        "tables firmware's LPIs read are never repointed"
    );
}

#[test]
fn a_collection_names_its_redistributor_by_address_or_processor_number() {
    let model = lpi_redistributor();
    let cpu = Cpu::default();
    let gic = Gicv3::new(&model, &cpu);
    assert_eq!(gic.collection_target(RD0, true), RD0 as u64);
    assert_eq!(gic.collection_target(RD0, false), 5 << 16);
}

#[test]
fn a_distributor_says_whether_it_has_lpis_and_how_many_intid_bits() {
    let model = Model::new(2);
    let cpu = Cpu::default();
    assert_eq!(Gicv3::new(&model, &cpu).lpi_id_bits(), None);
    model.set_distributor(
        GICD_TYPER,
        u64::from(GICD_TYPER_LPIS | (15 << GICD_TYPER_IDBITS_SHIFT) | 2),
    );
    assert_eq!(Gicv3::new(&model, &cpu).lpi_id_bits(), Some(16));
}

#[test]
fn lpi_tables_enable_the_lpis_routed_and_give_a_zeroed_aligned_pending_table() {
    let frames = crate::test_frames::HostBlocks::default();
    let tables = LpiTables::allocate(&frames, 14, 3).expect("allocated");
    assert_eq!(tables.id_bits, 14);
    let word = frames.read(tables.properties);
    assert_eq!(
        word & 0xFF_FFFF,
        0x83_8383,
        "the first three enabled at mid priority"
    );
    assert_eq!(word >> 24, 0, "the rest left off");
    assert_eq!(
        tables.pending % 0x1_0000,
        0,
        "a pending table is 64 KiB aligned"
    );
    assert_eq!(frames.read(tables.pending), 0);
    assert_eq!(frames.live(), 2);
}

#[test]
fn lpi_tables_naming_no_lpi_or_more_than_they_hold_are_refused() {
    let frames = crate::test_frames::HostBlocks::default();
    assert!(LpiTables::allocate(&frames, 13, 0).is_none());
    assert!(LpiTables::allocate(&frames, 14, 8193).is_none());
    assert!(LpiTables::allocate(&frames, 33, 1).is_none());
    assert_eq!(frames.live(), 0, "nothing is left drawn");
}
