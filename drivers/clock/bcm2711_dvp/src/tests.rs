//! The gates over a register-level model of the DVP block, alone and behind
//! the shared endpoint.

use std::cell::{Cell, RefCell};
use std::vec::Vec;

use tairix_abi::driver::clock::{ClockState, CLOCK_CONTROLLER_ENDPOINTS};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId, RegisterBlock, PROC_ID_LEN};
use tairix_clock::testkit::Rig;
use tairix_clock::{ClockHardware, Record};

use crate::{Gates, WINDOW_LEN};

const PARENT_HZ: u64 = 108_000_000;

const MISC_CONFIG: usize = 0x08;
const HDMI0_GATE: u32 = 1 << 3;
const HDMI1_GATE: u32 = 1 << 4;
const GATE_BITS: u32 = HDMI0_GATE | HDMI1_GATE;
/// What the firmware keeps in the register beside the gates.
const OTHER_BITS: u32 = 0xA5A5_0007;
const BOTH_CLOSED: u32 = OTHER_BITS | GATE_BITS;

/// The block's window, holding the driver to the hardware's rules: only the
/// gate register is written, and only its gate bits change.
struct Model {
    regs: RefCell<[u32; 4]>,
    len: usize,
    writes: RefCell<Vec<u32>>,
    stuck: Cell<bool>,
}

impl Model {
    fn new(misc_config: u32) -> Self {
        Self {
            regs: RefCell::new([0x0000_003F, 0, misc_config, 0]),
            len: 0x10,
            writes: RefCell::new(Vec::new()),
            stuck: Cell::new(false),
        }
    }

    fn misc_config(&self) -> u32 {
        self.regs.borrow()[MISC_CONFIG / 4]
    }

    fn writes(&self) -> Vec<u32> {
        self.writes.borrow().clone()
    }
}

impl RegisterBlock for Model {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        assert!(
            offset + 4 <= self.len && offset.is_multiple_of(4),
            "read at {offset:#x}"
        );
        Ok(self.regs.borrow()[offset / 4])
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        assert_eq!(offset, MISC_CONFIG, "only the gates are written");
        let old = self.misc_config();
        assert_eq!((old ^ value) & !GATE_BITS, 0, "only the gate bits change");
        self.writes.borrow_mut().push(value);
        if !self.stuck.get() {
            self.regs.borrow_mut()[MISC_CONFIG / 4] = value;
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        self.len
    }
}

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const HDMI0: ProcId = instance(1);
const HDMI1: ProcId = instance(2);
const CEC: ProcId = instance(3);

fn endpoint() -> u64 {
    CLOCK_CONTROLLER_ENDPOINTS.endpoint(4)
}

fn link(gate: u32, index: u8) -> LinkRequest {
    LinkRequest::new(endpoint(), index, &[gate], b"audio").expect("valid")
}

fn hdmi0() -> LinkRequest {
    link(0, 2)
}

fn hdmi1() -> LinkRequest {
    link(1, 2)
}

/// A second consumer of HDMI0's gate, as a board's tree could name one.
fn cec() -> LinkRequest {
    link(0, 0)
}

fn rig(model: &Model) -> Rig<Gates<'_, Model>> {
    Rig::new(
        Gates::new(model, PARENT_HZ).expect("valid"),
        endpoint(),
        &[(HDMI0, hdmi0()), (HDMI1, hdmi1()), (CEC, cec())],
    )
}

#[test]
fn the_gates_need_their_register_and_a_parent_running_at_a_rate() {
    let mut short = Model::new(BOTH_CLOSED);
    short.len = WINDOW_LEN - 4;
    assert!(matches!(
        Gates::new(&short, PARENT_HZ),
        Err(DriverError::LengthOutOfRange)
    ));
    let model = Model::new(BOTH_CLOSED);
    assert!(matches!(
        Gates::new(&model, 0),
        Err(DriverError::OutOfRange)
    ));
    assert!(Gates::new(&model, PARENT_HZ).is_ok());
    assert!(model.writes().is_empty(), "nothing is touched until asked");
}

#[test]
fn only_the_two_gates_are_served_each_by_its_one_cell() {
    let model = Model::new(BOTH_CLOSED);
    let gates = Gates::new(&model, PARENT_HZ).expect("valid");
    assert_eq!(gates.count(), 2);
    assert_eq!(gates.served(&[0]), Some(0));
    assert_eq!(gates.served(&[1]), Some(1));
    for unserved in [&[2][..], &[0, 0], &[]] {
        assert_eq!(gates.served(unserved), None);
    }
    assert_eq!((gates.id(0), gates.id(1)), (0, 1));
}

#[test]
fn a_gate_runs_at_its_parents_rate_only_while_open() {
    let model = Model::new(OTHER_BITS | HDMI1_GATE);
    let mut rig = rig(&model);
    assert_eq!(
        rig.describe(HDMI0, hdmi0()),
        Ok(ClockState {
            hz: PARENT_HZ,
            max_hz: PARENT_HZ,
            held_elsewhere: false
        })
    );
    assert_eq!(rig.describe(HDMI1, hdmi1()).map(|state| state.hz), Ok(0));
}

#[test]
fn a_floor_opens_its_gate_alone_and_the_last_release_closes_it() {
    let model = Model::new(BOTH_CLOSED);
    let mut rig = rig(&model);
    assert_eq!(rig.floor(HDMI0, hdmi0(), 0), Ok(PARENT_HZ));
    assert_eq!(model.misc_config(), OTHER_BITS | HDMI1_GATE);
    assert_eq!(rig.floor(HDMI1, hdmi1(), PARENT_HZ), Ok(PARENT_HZ));
    assert_eq!(model.misc_config(), OTHER_BITS);
    assert_eq!(rig.release(HDMI0, hdmi0()), Ok(()));
    assert_eq!(model.misc_config(), OTHER_BITS | HDMI0_GATE);
    assert_eq!(
        rig.records(),
        [
            Record::Ran {
                clock: 0,
                hz: PARENT_HZ,
                holder: HDMI0
            },
            Record::Ran {
                clock: 1,
                hz: PARENT_HZ,
                holder: HDMI1
            },
            Record::Stopped { clock: 0 },
        ]
    );
    assert_eq!(rig.kernel.watched(), [HDMI1]);
}

#[test]
fn a_shared_gate_stays_open_until_its_last_holder_leaves() {
    let model = Model::new(BOTH_CLOSED);
    let mut rig = rig(&model);
    rig.floor(HDMI0, hdmi0(), 48_000_000).expect("opens");
    let opened = model.writes().len();
    assert_eq!(rig.floor(CEC, cec(), 1_000_000), Ok(PARENT_HZ));
    assert_eq!(rig.release(HDMI0, hdmi0()), Ok(()));
    assert_eq!(model.writes().len(), opened, "still held");
    assert_eq!(model.misc_config() & HDMI0_GATE, 0);
    rig.end(CEC);
    assert_eq!(model.misc_config() & HDMI0_GATE, HDMI0_GATE);
    assert!(rig.records().contains(&Record::Abandoned {
        clock: 0,
        holder: CEC
    }));
}

#[test]
fn a_floor_above_the_parents_rate_is_refused_rather_than_given_less() {
    let model = Model::new(BOTH_CLOSED);
    let mut rig = rig(&model);
    assert_eq!(
        rig.floor(HDMI0, hdmi0(), PARENT_HZ + 1),
        Err(Errno::OutOfRange)
    );
    assert_eq!(rig.run(HDMI0, hdmi0(), PARENT_HZ), Err(Errno::NotSupported));
    let elsewhere = link(2, 2);
    rig.kernel.grant(HDMI0, HwResource::request(&elsewhere));
    assert_eq!(rig.floor(HDMI0, elsewhere, 0), Err(Errno::NotSupported));
    assert!(model.writes().is_empty());
    assert!(rig.kernel.watched().is_empty());
}

#[test]
fn a_gate_that_will_not_move_is_refused_as_a_fault_and_left_unheld() {
    let model = Model::new(BOTH_CLOSED);
    model.stuck.set(true);
    let mut rig = rig(&model);
    assert_eq!(rig.floor(HDMI0, hdmi0(), 0), Err(Errno::DeviceFault));
    assert!(rig.records().contains(&Record::Stuck { clock: 0 }));
    assert!(rig.kernel.watched().is_empty());
    assert_eq!(rig.release(HDMI0, hdmi0()), Err(Errno::NotFound));
}

#[test]
fn a_gate_the_firmware_left_open_is_never_closed_unasked() {
    let model = Model::new(OTHER_BITS);
    let mut rig = rig(&model);
    rig.describe(HDMI0, hdmi0()).expect("described");
    rig.end(HDMI0);
    assert!(model.writes().is_empty());
}
