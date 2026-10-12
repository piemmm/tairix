//! The BSC against a simulated controller and the targets on its bus.
//!
//! The simulation chains a message started while another is active, as the
//! silicon does, and records every condition on the wire, so a transaction
//! split by a STOP is visible. In its eager form the controller moves every
//! byte the moment it can, which is what a driver preempted at the worst
//! instruction faces.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::num::NonZeroU32;

use tairix_abi::driver::i2c::{
    BusFacts, Direction, I2cAddress, I2cFault, I2cMessage, I2cTransaction, RepeatedStart,
    I2C_BUFFER_LEN,
};
use tairix_abi::{
    CapabilityId, DriverError, DriverHost, DriverKind, HwMatchKey, RegisterBlock, HW_COMPATIBLE_MAX,
};
use tairix_i2cbus::{step_budget_ns, I2cBus, STALL_ALLOWANCE_NS};

use super::{
    register, Bsc, BusWait, ParentClock, A, BIND_KEYS, BSC_COMPATIBLE, C, CLKT, C_CLEAR, C_I2CEN,
    C_INTD, C_INTR, C_INTT, C_READ, C_ST, DEL, DIV, DLEN, FIFO, FIFO_DEPTH, REGISTER_BLOCK_LEN,
    REQUIRED_CAPABILITIES, S, S_CLKT, S_DONE, S_ERR, S_RXD, S_RXR, S_TXD, S_TXW,
};

const FIFO_LEN: usize = 16;
const S_TA: u32 = 1 << 0;
const VPU_HZ: u64 = 500_000_000;
const RTC: u8 = 0x68;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Wire {
    Start { address: u8, read: bool },
    Restart { address: u8, read: bool },
    Byte(u8),
    Nack,
    Stop,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Target {
    Present,
    /// Never acknowledges its address.
    Absent,
    /// Acknowledges this many bytes written to it, then refuses the next.
    RefusesAfter(u32),
    /// Acknowledges a write but refuses its address for a read.
    RefusesReads,
    /// Holds the clock low for good.
    Wedged,
    /// Holds the clock low this long as each message starts.
    Stretches(u64),
    /// Ends a read halfway and reports it done.
    Truncates,
    /// Trips the controller's own stretch timeout.
    TripsTimeout,
}

#[derive(Copy, Clone, Debug)]
struct Transfer {
    address: u8,
    read: bool,
    len: u32,
    moved: u32,
}

/// What byte `at` of a read from `address` carries.
fn pattern(address: u8, at: u32) -> u8 {
    address ^ at.to_le_bytes()[0]
}

#[derive(Default)]
struct SimState {
    control: u32,
    dlen: u32,
    address: u32,
    div: u32,
    del: u32,
    clkt: u32,
    active: Option<Transfer>,
    queued: Option<Transfer>,
    fifo_tx: VecDeque<u8>,
    fifo_rx: VecDeque<u8>,
    /// The status bits that hold until written back.
    latched: u32,
    targets: Vec<(u8, Target)>,
    wire: Vec<Wire>,
    eager: bool,
    stalled_ns: u64,
    now_ns: u64,
    waits: usize,
    /// How far past its deadline every park oversleeps: a preempted driver.
    late_ns: u64,
    controls: Vec<u32>,
}

impl SimState {
    fn target(&self, address: u8) -> Target {
        self.targets
            .iter()
            .find_map(|&(at, target)| (at == address).then_some(target))
            .unwrap_or(Target::Absent)
    }

    fn status(&self) -> u32 {
        let mut status = self.latched;
        if let Some(transfer) = self.active {
            status |= S_TA;
            let wanting = usize::try_from(transfer.len - transfer.moved).expect("short");
            if !transfer.read && wanting > self.fifo_tx.len() && self.fifo_tx.len() < FIFO_LEN / 4 {
                status |= S_TXW;
            }
            if transfer.read && self.fifo_rx.len() >= FIFO_LEN * 3 / 4 {
                status |= S_RXR;
            }
        }
        if !self.fifo_rx.is_empty() {
            status |= S_RXD;
        }
        if self.fifo_tx.len() < FIFO_LEN {
            status |= S_TXD;
        }
        status
    }

    fn write_control(&mut self, value: u32) {
        self.controls.push(value);
        if value & (0b11 << 4) != 0 {
            self.fifo_tx.clear();
            self.fifo_rx.clear();
        }
        if value & C_I2CEN == 0 {
            if self.active.take().is_some() {
                self.wire.push(Wire::Stop);
            }
            self.queued = None;
            self.stalled_ns = 0;
        }
        self.control = value & !(C_ST | 0b11 << 4);
        if value & (C_ST | C_I2CEN) == C_ST | C_I2CEN {
            let transfer = Transfer {
                address: self.address.to_le_bytes()[0] & 0x7f,
                read: value & C_READ != 0,
                len: self.dlen,
                moved: 0,
            };
            if self.active.is_some() {
                self.queued = Some(transfer);
            } else {
                self.begin(transfer, false);
            }
        }
    }

    fn begin(&mut self, transfer: Transfer, restart: bool) {
        let (address, read) = (transfer.address, transfer.read);
        self.wire.push(if restart {
            Wire::Restart { address, read }
        } else {
            Wire::Start { address, read }
        });
        match self.target(address) {
            Target::Absent => self.refuse(),
            Target::RefusesReads if read => self.refuse(),
            Target::TripsTimeout => {
                self.latched |= S_CLKT;
                self.end();
            }
            Target::Stretches(ns) => {
                self.stalled_ns = ns;
                self.active = Some(transfer);
            }
            _ => self.active = Some(transfer),
        }
    }

    fn refuse(&mut self) {
        self.wire.push(Wire::Nack);
        self.latched |= S_ERR;
        self.end();
    }

    /// The controller ends the transaction: a STOP, its chain dropped.
    fn end(&mut self) {
        self.active = None;
        self.queued = None;
        self.wire.push(Wire::Stop);
        self.latched |= S_DONE;
    }

    /// Move every byte the controller can, answering whether any moved.
    fn advance(&mut self) -> bool {
        let mut moved = false;
        while let Some(mut transfer) = self.active {
            let target = self.target(transfer.address);
            if target == Target::Wedged || self.stalled_ns > 0 {
                break;
            }
            if transfer.moved == transfer.len {
                self.active = None;
                if let Some(next) = self.queued.take() {
                    self.begin(next, true);
                } else {
                    self.wire.push(Wire::Stop);
                    self.latched |= S_DONE;
                }
                moved = true;
                continue;
            }
            if transfer.read {
                if target == Target::Truncates && transfer.moved * 2 >= transfer.len {
                    self.end();
                    return true;
                }
                if self.fifo_rx.len() == FIFO_LEN {
                    break;
                }
                let byte = pattern(transfer.address, transfer.moved);
                self.fifo_rx.push_back(byte);
                self.wire.push(Wire::Byte(byte));
            } else {
                let Some(byte) = self.fifo_tx.pop_front() else {
                    break;
                };
                self.wire.push(Wire::Byte(byte));
                if target == Target::RefusesAfter(transfer.moved) {
                    self.refuse();
                    return true;
                }
            }
            transfer.moved += 1;
            self.active = Some(transfer);
            moved = true;
        }
        moved
    }
}

struct Sim(RefCell<SimState>);

impl Sim {
    fn new(targets: &[(u8, Target)]) -> Self {
        Self(RefCell::new(SimState {
            targets: targets.to_vec(),
            ..SimState::default()
        }))
    }

    fn eager(targets: &[(u8, Target)]) -> Self {
        let sim = Self::new(targets);
        sim.0.borrow_mut().eager = true;
        sim
    }

    fn wire(&self) -> Vec<Wire> {
        self.0.borrow().wire.clone()
    }

    fn idle(&self) -> bool {
        let state = self.0.borrow();
        state.active.is_none()
            && state.queued.is_none()
            && state.status() & (S_DONE | S_ERR | S_CLKT) == 0
            && state.control & (C_INTT | C_INTR | C_INTD) == 0
    }
}

impl RegisterBlock for Sim {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        if offset >= REGISTER_BLOCK_LEN {
            return Err(DriverError::DeviceFault);
        }
        let mut state = self.0.borrow_mut();
        Ok(match offset {
            C => state.control,
            S => state.status(),
            DLEN => state
                .active
                .map_or(state.dlen, |transfer| transfer.len - transfer.moved),
            A => state.address,
            FIFO => {
                let byte = state.fifo_rx.pop_front().unwrap_or(0);
                if state.eager {
                    state.advance();
                }
                u32::from(byte)
            }
            DIV => state.div,
            DEL => state.del,
            CLKT => state.clkt,
            _ => 0,
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        if offset >= REGISTER_BLOCK_LEN {
            return Err(DriverError::DeviceFault);
        }
        let mut state = self.0.borrow_mut();
        match offset {
            C => state.write_control(value),
            S => state.latched &= !(value & (S_DONE | S_ERR | S_CLKT)),
            DLEN => state.dlen = value,
            A => state.address = value,
            FIFO => {
                if state.fifo_tx.len() < FIFO_LEN {
                    state.fifo_tx.push_back(value.to_le_bytes()[0]);
                }
            }
            DIV => state.div = value,
            DEL => state.del = value,
            CLKT => state.clkt = value,
            _ => {}
        }
        if state.eager && matches!(offset, C | FIFO) {
            state.advance();
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        REGISTER_BLOCK_LEN
    }
}

impl BusWait for Sim {
    fn now_ns(&self) -> u64 {
        self.0.borrow().now_ns
    }

    fn wait(&self, timeout_ns: u64) {
        let mut state = self.0.borrow_mut();
        state.waits += 1;
        let stall = state.stalled_ns.min(timeout_ns);
        state.stalled_ns -= stall;
        state.now_ns += stall + state.late_ns;
        if !state.advance() && stall == 0 {
            // Nothing could happen: the whole park elapses.
            state.now_ns += timeout_ns;
        }
    }
}

/// A parent clock whose rate a test sets; `None` cannot be read.
#[derive(Clone)]
struct Parent(Rc<Cell<Option<u64>>>);

impl Parent {
    fn at(hz: u64) -> Self {
        Self(Rc::new(Cell::new(Some(hz))))
    }
}

impl ParentClock for Parent {
    fn rate(&mut self) -> Result<u64, DriverError> {
        self.0.get().ok_or(DriverError::DeviceFault)
    }
}

const STANDARD: NonZeroU32 = NonZeroU32::new(100_000).expect("non-zero");

fn controller(sim: &Sim) -> Bsc<'_, Parent> {
    Bsc::new(sim, sim, Parent::at(VPU_HZ), STANDARD).expect("brought up")
}

fn message(address: u8, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address: I2cAddress::new(address).expect("usable"),
        direction,
        offset,
        len,
    }
}

fn transaction(messages: &[I2cMessage]) -> I2cTransaction {
    I2cTransaction::new(messages).expect("valid")
}

/// A register pointer write and the block read back.
fn register_read(address: u8, register: u8, len: u16) -> (I2cTransaction, Vec<u8>) {
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    buffer[0] = register;
    (
        transaction(&[
            message(address, Direction::Write, 0, 1),
            message(address, Direction::Read, 1, len),
        ]),
        buffer,
    )
}

fn read_back(address: u8, len: u16) -> Vec<u8> {
    (0..u32::from(len)).map(|at| pattern(address, at)).collect()
}

fn bytes(of: &[u8]) -> Vec<Wire> {
    of.iter().copied().map(Wire::Byte).collect()
}

#[test]
fn a_register_read_is_one_transaction_joined_by_a_repeated_start() {
    for sim in [
        Sim::new(&[(RTC, Target::Present)]),
        Sim::eager(&[(RTC, Target::Present)]),
    ] {
        let mut bsc = controller(&sim);
        let (read, mut buffer) = register_read(RTC, 0x02, 7);
        assert_eq!(bsc.transfer(&read, &mut buffer), Ok(Ok(())));
        assert_eq!(buffer[1..8], read_back(RTC, 7));
        let mut expected = vec![
            Wire::Start {
                address: RTC,
                read: false,
            },
            Wire::Byte(0x02),
            Wire::Restart {
                address: RTC,
                read: true,
            },
        ];
        expected.extend(bytes(&read_back(RTC, 7)));
        expected.push(Wire::Stop);
        assert_eq!(sim.wire(), expected);
        assert!(sim.idle());
    }
}

#[test]
fn the_last_byte_of_a_write_waits_for_the_message_after_it() {
    // A controller that moves each byte the instant it arrives ends a write
    // the moment its last byte does, so the next message must be started
    // before that byte is handed over.
    let sim = Sim::eager(&[(0x30, Target::Present), (0x50, Target::Present)]);
    let mut bsc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    buffer[..21].copy_from_slice(&[7; 21]);
    let edid = transaction(&[
        message(0x30, Direction::Write, 0, 1),
        message(0x50, Direction::Write, 1, 20),
        message(0x50, Direction::Read, 21, 128),
    ]);
    assert_eq!(bsc.transfer(&edid, &mut buffer), Ok(Ok(())));
    let wire = sim.wire();
    assert_eq!(
        wire.iter().filter(|event| **event == Wire::Stop).count(),
        1,
        "one STOP, at the end: {wire:?}"
    );
    assert_eq!(
        wire.iter()
            .filter(|event| matches!(event, Wire::Restart { .. }))
            .count(),
        2
    );
    assert_eq!(buffer[21..149], read_back(0x50, 128));
}

#[test]
fn a_transfer_longer_than_the_fifo_is_paced_by_the_interrupts() {
    let sim = Sim::new(&[(RTC, Target::Present)]);
    let mut bsc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    let write: Vec<u8> = (0..=255).collect();
    buffer[..256].copy_from_slice(&write);
    let long = transaction(&[
        message(RTC, Direction::Write, 0, 256),
        message(RTC, Direction::Read, 256, 4096),
    ]);
    assert_eq!(bsc.transfer(&long, &mut buffer), Ok(Ok(())));
    assert_eq!(buffer[256..4352], read_back(RTC, 4096));
    let sent: Vec<u8> = sim
        .wire()
        .iter()
        .take_while(|event| !matches!(event, Wire::Restart { .. }))
        .filter_map(|event| match event {
            Wire::Byte(byte) => Some(*byte),
            _ => None,
        })
        .collect();
    assert_eq!(sent, write);
    assert!(
        sim.0.borrow().waits > 4096 / FIFO_LEN,
        "parks between FIFO loads"
    );
}

#[test]
fn a_lone_write_and_a_lone_read_each_run_alone() {
    let sim = Sim::new(&[(RTC, Target::Present)]);
    let mut bsc = controller(&sim);
    let mut buffer = vec![9u8; I2C_BUFFER_LEN];
    let write = transaction(&[message(RTC, Direction::Write, 0, 3)]);
    assert_eq!(bsc.transfer(&write, &mut buffer), Ok(Ok(())));
    let read = transaction(&[message(RTC, Direction::Read, 0, 3)]);
    assert_eq!(bsc.transfer(&read, &mut buffer), Ok(Ok(())));
    assert_eq!(buffer[..3], read_back(RTC, 3));
    let mut expected = vec![Wire::Start {
        address: RTC,
        read: false,
    }];
    expected.extend(bytes(&[9; 3]));
    expected.extend([
        Wire::Stop,
        Wire::Start {
            address: RTC,
            read: true,
        },
    ]);
    expected.extend(bytes(&read_back(RTC, 3)));
    expected.push(Wire::Stop);
    assert_eq!(sim.wire(), expected);
}

#[test]
fn an_empty_write_asks_whether_a_target_answers() {
    let sim = Sim::new(&[(RTC, Target::Present)]);
    let mut bsc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    let probe = |address| transaction(&[message(address, Direction::Write, 0, 0)]);
    assert_eq!(bsc.transfer(&probe(RTC), &mut buffer), Ok(Ok(())));
    assert_eq!(
        bsc.transfer(&probe(0x69), &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 0 }))
    );
    let starts: Vec<u32> = sim
        .0
        .borrow()
        .controls
        .iter()
        .copied()
        .filter(|control| control & C_ST != 0)
        .collect();
    assert!(
        starts.iter().all(|control| control & C_INTT == 0),
        "a message with nothing to send asks for nothing"
    );
}

#[test]
fn a_target_that_does_not_answer_is_refused_on_its_first_message() {
    let sim = Sim::eager(&[]);
    let mut bsc = controller(&sim);
    let (read, mut buffer) = register_read(RTC, 0, 2);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 0 }))
    );
    assert!(sim.idle());
}

#[test]
fn a_refusal_is_reported_against_the_earliest_message_not_seen_to_finish() {
    // A refused register pointer is the first message's.
    let sim = Sim::new(&[(RTC, Target::RefusesAfter(0))]);
    let mut bsc = controller(&sim);
    let (read, mut buffer) = register_read(RTC, 0, 2);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 0 }))
    );

    // The controller shows no sign of leaving a write until the read after
    // it delivers a byte, so a refused read address is the write's too.
    let sim = Sim::new(&[(RTC, Target::RefusesReads)]);
    let mut bsc = controller(&sim);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 0 }))
    );

    // A second write asks for its bytes once it runs, so a refusal of them
    // is its own.
    let sim = Sim::new(&[(0x30, Target::Present), (0x50, Target::RefusesAfter(1))]);
    let mut bsc = controller(&sim);
    let edid = transaction(&[
        message(0x30, Direction::Write, 0, 1),
        message(0x50, Direction::Write, 1, 4),
        message(0x50, Direction::Read, 5, 8),
    ]);
    assert_eq!(
        bsc.transfer(&edid, &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 1 }))
    );
    assert!(sim.idle());
}

#[test]
fn a_target_holding_the_bus_times_the_transaction_out_and_resets_the_controller() {
    let sim = Sim::new(&[(RTC, Target::Wedged)]);
    let mut bsc = controller(&sim);
    let (read, mut buffer) = register_read(RTC, 0, 2);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::TimedOut { message: 0 }))
    );
    assert!(sim.idle());
    assert!(
        sim.0.borrow().now_ns >= step_budget_ns(FIFO_DEPTH, 100_000),
        "given up only once the step budget is spent"
    );
    assert!(sim.0.borrow().waits <= 2, "parked, never spun");

    let sim = Sim::new(&[(RTC, Target::Stretches(2 * STALL_ALLOWANCE_NS))]);
    let mut bsc = controller(&sim);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::TimedOut { message: 0 }))
    );
}

#[test]
fn a_target_stretching_within_the_allowance_is_waited_for() {
    // Each message's start stalls, and the driver sees no progress between
    // the write's and the read's, so both count against one step.
    let sim = Sim::new(&[(RTC, Target::Stretches(STALL_ALLOWANCE_NS / 4))]);
    let mut bsc = controller(&sim);
    let (read, mut buffer) = register_read(RTC, 0, 2);
    assert_eq!(bsc.transfer(&read, &mut buffer), Ok(Ok(())));
}

#[test]
fn a_driver_woken_late_is_never_timed_out_while_the_bus_progresses() {
    let sim = Sim::new(&[(RTC, Target::Present)]);
    sim.0.borrow_mut().late_ns = 10 * step_budget_ns(FIFO_DEPTH, 100_000);
    let mut bsc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    let long = transaction(&[
        message(RTC, Direction::Write, 0, 200),
        message(RTC, Direction::Read, 200, 600),
    ]);
    assert_eq!(bsc.transfer(&long, &mut buffer), Ok(Ok(())));
    assert_eq!(buffer[200..800], read_back(RTC, 600));
}

#[test]
fn a_controller_that_ends_a_read_early_or_trips_its_own_timeout_faults_the_bus() {
    let sim = Sim::new(&[(RTC, Target::Truncates)]);
    let mut bsc = controller(&sim);
    let (read, mut buffer) = register_read(RTC, 0, 40);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::BusFault { message: 1 }))
    );
    assert!(sim.idle());

    let sim = Sim::new(&[(RTC, Target::TripsTimeout)]);
    let mut bsc = controller(&sim);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::BusFault { message: 0 }))
    );
}

#[test]
fn the_divider_follows_the_parent_clock_into_every_transaction() {
    let sim = Sim::new(&[(RTC, Target::Present)]);
    let parent = Parent::at(VPU_HZ);
    let mut bsc = Bsc::new(&sim, &sim, parent.clone(), STANDARD).expect("brought up");
    let (read, mut buffer) = register_read(RTC, 0, 1);
    bsc.transfer(&read, &mut buffer)
        .expect("ran")
        .expect("answered");
    let registers = |sim: &Sim| {
        let state = sim.0.borrow();
        (state.div, state.del, state.clkt)
    };
    assert_eq!(
        registers(&sim),
        (5000, 0x0138_04E2, 0),
        "a sixteenth and a quarter of the divider as edge delays: 312 and 1250"
    );

    parent.0.set(Some(300_000_001));
    bsc.transfer(&read, &mut buffer)
        .expect("ran")
        .expect("answered");
    assert_eq!(registers(&sim).0, 3002, "rounded up, then up to even");
}

#[test]
fn a_bus_the_parent_cannot_clock_is_refused_before_it_is_touched() {
    let sim = Sim::new(&[(RTC, Target::Present)]);
    let parent = Parent::at(10_000_000_000);
    let mut bsc = Bsc::new(&sim, &sim, parent.clone(), STANDARD).expect("brought up");
    let (read, mut buffer) = register_read(RTC, 0, 1);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Err(DriverError::OutOfRange),
        "too slow a bus for the divider to reach"
    );
    parent.0.set(Some(0));
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Err(DriverError::OutOfRange)
    );
    parent.0.set(None);
    assert_eq!(
        bsc.transfer(&read, &mut buffer),
        Err(DriverError::DeviceFault)
    );
    assert!(sim.wire().is_empty());
    assert!(sim.idle());
}

#[test]
fn the_bus_states_its_declared_speed_and_chained_repeated_starts() {
    let sim = Sim::new(&[]);
    let fast = NonZeroU32::new(400_000).expect("non-zero");
    let bsc = Bsc::new(&sim, &sim, Parent::at(VPU_HZ), fast).expect("brought up");
    assert_eq!(
        bsc.facts(),
        BusFacts {
            speed_hz: 400_000,
            repeated_start: RepeatedStart::BeforeFinalRead,
        }
    );
}

/// A window a word shorter than the registers.
struct Short;

impl RegisterBlock for Short {
    fn read32(&self, _: usize) -> Result<u32, DriverError> {
        Ok(0)
    }

    fn write32(&self, _: usize, _: u32) -> Result<(), DriverError> {
        Ok(())
    }

    fn block_len(&self) -> usize {
        REGISTER_BLOCK_LEN - 4
    }
}

#[test]
fn bring_up_leaves_the_controller_idle_and_refuses_a_short_window() {
    let sim = Sim::new(&[]);
    sim.0.borrow_mut().latched = S_DONE | S_ERR;
    let _ = controller(&sim);
    assert!(sim.idle());
    assert_eq!(sim.0.borrow().controls.last(), Some(&(C_I2CEN | C_CLEAR)));

    assert!(matches!(
        Bsc::new(&Short, &sim, Parent::at(VPU_HZ), STANDARD),
        Err(DriverError::LengthOutOfRange)
    ));
}

struct Host {
    caps: &'static [CapabilityId],
}

impl DriverHost for Host {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        self.caps.contains(&cap)
    }

    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

#[test]
fn register_requires_the_load_capability() {
    assert!(register(&Host {
        caps: &[CapabilityId::DRV_LOAD]
    })
    .is_ok());
    assert_eq!(
        register(&Host { caps: &[] }).err(),
        Some(DriverError::PermissionDenied)
    );
}

#[test]
fn the_bind_table_names_the_controller_and_fits_the_abi_bound() {
    assert_eq!(BIND_KEYS.len(), 1);
    assert!(BSC_COMPATIBLE.len() <= HW_COMPATIBLE_MAX);
    let expected = HwMatchKey::compatible(BSC_COMPATIBLE).expect("fits");
    assert_eq!(BIND_KEYS[0].key, expected);
}

/// Each channel's buffer is a shared region granted to its client, and the
/// parent clock is reached over its link.
#[test]
fn the_manifest_requests_the_buffers_and_the_clock_link_it_needs() {
    for cap in [
        CapabilityId::SHM,
        CapabilityId::IPC_ENDPOINT,
        CapabilityId::LOG_EMIT,
    ] {
        assert!(REQUIRED_CAPABILITIES.contains(&cap));
    }
}
