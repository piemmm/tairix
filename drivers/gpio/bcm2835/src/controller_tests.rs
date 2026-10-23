//! The controller driven over a register model of the GPIO block, answered
//! through the shared fake kernel.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::gpio::{
    decode_lines, decode_status, HeaderLines, HeaderRequest, Holder, LineState, Polarity,
    HEADER_REQUEST_LEN,
};
use tairix_abi::driver::irqline::{
    decode_reply, IrqLineRequest, INTERRUPT_LINE_ENDPOINTS, IRQLINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{
    CapabilityId, DriverError, DriverHost, DriverKind, Errno, ProcId, RegisterBlock, PROC_ID_LEN,
};
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::{Supplier, SupplierHost};

use crate::block::{Block, Layout, WINDOW_LEN};
use crate::controller::{Controller, HeaderPort, Record, Role, BANK_INTERRUPTS};
use crate::{register, BIND_KEYS, LAYOUTS};

const DEVICE: ProcId = ProcId::from_raw([1; PROC_ID_LEN]);
const FAN: ProcId = ProcId::from_raw([2; PROC_ID_LEN]);
const OTHER: ProcId = ProcId::from_raw([3; PROC_ID_LEN]);

/// The header's lines on a Pi: GPIO 2–27.
fn lent() -> HeaderLines {
    HeaderLines::span(2, 27)
}

fn endpoint() -> u64 {
    INTERRUPT_LINE_ENDPOINTS.endpoint(9)
}

fn link(line: u32, trigger: u32) -> LinkRequest {
    LinkRequest::new(endpoint(), 0, &[line, trigger], b"").expect("valid")
}

const RISING: u32 = 1;
const HIGH: u32 = 4;

const FSEL: usize = 0x00;
const SET: usize = 0x1C;
const CLR: usize = 0x28;
const EDS: usize = 0x40;
const REN: usize = 0x4C;
const FEN: usize = 0x58;
const HEN: usize = 0x64;
const LEN: usize = 0x70;
const AREN: usize = 0x7C;
const AFEN: usize = 0x88;

/// The GPIO block's registers over two banks: function selects, the output
/// latch, the pins, the latched events and the detect enables.
struct Gpio {
    regs: RefCell<[u32; WINDOW_LEN / 4]>,
    pins: Cell<u64>,
    latch: Cell<u64>,
    /// Every write, in order, as (offset, value).
    writes: RefCell<Vec<(usize, u32)>>,
}

impl Gpio {
    fn new() -> Self {
        Self {
            regs: RefCell::new([0; WINDOW_LEN / 4]),
            pins: Cell::new(0),
            latch: Cell::new(0),
            writes: RefCell::new(Vec::new()),
        }
    }

    fn reg(&self, offset: usize) -> u32 {
        self.regs.borrow()[offset / 4]
    }

    fn bank(&self, base: usize, line: u8) -> bool {
        let (bank, bit) = (usize::from(line / 32), line % 32);
        self.reg(base + bank * 4) & (1 << bit) != 0
    }

    fn function_bits(&self, line: u8) -> u32 {
        let shift = u32::from(line % 10) * 3;
        (self.reg(FSEL + usize::from(line / 10) * 4) >> shift) & 0b111
    }

    /// Route `line` to alternate function 0, as firmware sets the console's.
    fn route_alt0(&self, line: u8) {
        let offset = FSEL + usize::from(line / 10) * 4;
        let shift = u32::from(line % 10) * 3;
        let value = (self.reg(offset) & !(0b111 << shift)) | (0b100 << shift);
        self.regs.borrow_mut()[offset / 4] = value;
    }

    fn is_output(&self, line: u8) -> bool {
        self.function_bits(line) == 0b001
    }

    /// The level an output drives.
    fn drives_high(&self, line: u8) -> bool {
        self.latch.get() & (1 << line) != 0
    }

    fn detects(&self, line: u8) -> bool {
        [REN, FEN, HEN, LEN, AREN, AFEN]
            .into_iter()
            .any(|base| self.bank(base, line))
    }

    fn latch_event(&self, line: u8) {
        let (bank, bit) = (usize::from(line / 32), line % 32);
        self.regs.borrow_mut()[(EDS + bank * 4) / 4] |= 1 << bit;
    }

    /// Raise the pin: a rising edge latches where enabled, a high level
    /// latches while it stands.
    fn raise(&self, line: u8) {
        self.pins.set(self.pins.get() | 1 << line);
        if self.bank(REN, line) || self.bank(HEN, line) {
            self.latch_event(line);
        }
    }

    fn lower(&self, line: u8) {
        self.pins.set(self.pins.get() & !(1 << line));
    }

    /// Whether the line's event is latched, which asserts its bank interrupt.
    fn pending(&self, line: u8) -> bool {
        self.bank(EDS, line)
    }

    fn write_index(&self, offset: usize, value: u32) -> Option<usize> {
        self.writes
            .borrow()
            .iter()
            .position(|&write| write == (offset, value))
    }
}

impl RegisterBlock for Gpio {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        if offset >= WINDOW_LEN {
            return Err(DriverError::OutOfRange);
        }
        Ok(self.reg(offset))
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        if offset >= WINDOW_LEN {
            return Err(DriverError::OutOfRange);
        }
        self.writes.borrow_mut().push((offset, value));
        let bank_bits = |base: usize| u64::from(value) << ((offset - base) / 4 * 32);
        match offset {
            o if o == SET || o == SET + 4 => self.latch.set(self.latch.get() | bank_bits(SET)),
            o if o == CLR || o == CLR + 4 => self.latch.set(self.latch.get() & !bank_bits(CLR)),
            o if o == EDS || o == EDS + 4 => {
                let mut regs = self.regs.borrow_mut();
                regs[offset / 4] &= !value;
                drop(regs);
                // A level that still stands latches again at once.
                for line in 0..58u8 {
                    let high = self.pins.get() & (1 << line) != 0;
                    if (self.bank(HEN, line) && high) || (self.bank(LEN, line) && !high) {
                        self.latch_event(line);
                    }
                }
            }
            _ => self.regs.borrow_mut()[offset / 4] = value,
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        WINDOW_LEN
    }
}

/// Calls queued on the header endpoint: each ticket and its frame.
type Queue = Rc<RefCell<VecDeque<(u64, Vec<u8>)>>>;

/// The header endpoint: calls the test queues, answered through the shared
/// fake kernel.
#[derive(Clone)]
struct FakeHeader {
    kernel: FakeKernel,
    queue: Queue,
}

impl HeaderPort for FakeHeader {
    fn receive(&mut self, frame: &mut [u8]) -> Result<Option<(u64, usize)>, Errno> {
        let Some((ticket, bytes)) = self.queue.borrow_mut().pop_front() else {
            return Ok(None);
        };
        frame[..bytes.len()].copy_from_slice(&bytes);
        Ok(Some((ticket, bytes.len())))
    }

    fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
        self.kernel.caller(ticket)
    }

    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
        self.kernel.reply(ticket, frame)
    }
}

type Server<'r> = Controller<'r, FakeKernel, FakeHeader, Records<Record>>;

struct Rig {
    kernel: FakeKernel,
    header: FakeHeader,
    records: Records<Record>,
}

impl Rig {
    fn new() -> Self {
        let kernel = FakeKernel::new();
        Self {
            header: FakeHeader {
                kernel: kernel.clone(),
                queue: Rc::default(),
            },
            kernel,
            records: Records::default(),
        }
    }

    fn controller<'r>(&self, gpio: &'r Gpio, banks: usize) -> Server<'r> {
        Controller::new(
            Block::new(gpio, Layout::BCM2711),
            self.kernel.clone(),
            endpoint(),
            banks,
            Some((self.header.clone(), lent())),
            self.records.clone(),
        )
        .expect("the block quiesces")
    }

    /// `caller` asks `request` of the header; answers the reply.
    fn ask(&self, server: &mut Server<'_>, caller: ProcId, request: HeaderRequest) -> Vec<u8> {
        let ticket = self.kernel.call(caller);
        let mut frame = [0u8; HEADER_REQUEST_LEN];
        request.encode(&mut frame).expect("fits");
        self.header
            .queue
            .borrow_mut()
            .push_back((ticket, frame.to_vec()));
        server.woke(BANK_INTERRUPTS).expect("the header serves");
        self.kernel
            .answer(ticket)
            .expect("every header call is answered")
    }

    fn change(
        &self,
        server: &mut Server<'_>,
        caller: ProcId,
        request: HeaderRequest,
    ) -> Result<(), Errno> {
        decode_status(&self.ask(server, caller, request))
    }

    /// `caller` waits on `request`'s line; answers its ticket.
    fn wait(&self, server: &mut Server<'_>, caller: ProcId, request: LinkRequest) -> u64 {
        let ticket = self.kernel.call(caller);
        let mut frame = [0u8; IRQLINE_MAX_REQUEST];
        IrqLineRequest::Wait(request)
            .encode(&mut frame)
            .expect("fits");
        server.serve(ticket, &frame);
        ticket
    }

    fn grant(&self, holder: ProcId, request: &LinkRequest) {
        self.kernel.grant(holder, HwResource::request(request));
    }
}

fn claim(line: u8, polarity: Polarity, asserted: bool) -> HeaderRequest {
    HeaderRequest::Claim {
        line,
        polarity,
        asserted,
    }
}

#[test]
fn bring_up_disables_every_detection_and_clears_every_event() {
    let gpio = Gpio::new();
    gpio.regs.borrow_mut()[REN / 4] = u32::MAX;
    gpio.regs.borrow_mut()[EDS / 4 + 1] = 0xF;
    let rig = Rig::new();
    let _server = rig.controller(&gpio, BANK_INTERRUPTS);
    for base in [REN, FEN, HEN, LEN, AREN, AFEN] {
        assert_eq!((gpio.reg(base), gpio.reg(base + 4)), (0, 0), "{base:#x}");
    }
    assert_eq!((gpio.reg(EDS), gpio.reg(EDS + 4)), (0, 0));
}

#[test]
fn the_header_lists_its_lent_lines_and_how_each_stands() {
    let gpio = Gpio::new();
    gpio.route_alt0(14);
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    let interrupt = link(17, RISING);
    rig.grant(DEVICE, &interrupt);
    rig.wait(&mut server, DEVICE, interrupt);
    assert_eq!(
        rig.change(&mut server, FAN, claim(18, Polarity::ActiveHigh, true)),
        Ok(())
    );
    let table = decode_lines(&rig.ask(&mut server, FAN, HeaderRequest::Lines)).unwrap();
    assert!(table.entries().iter().map(|entry| entry.line).eq(2..=27));
    assert_eq!(table.state(14), Some(LineState::Peripheral { function: 0 }));
    assert_eq!(table.state(17), Some(LineState::Interrupt));
    assert_eq!(
        table.state(18),
        Some(LineState::Output {
            holder: Holder::Caller,
            asserted: true
        })
    );
    assert_eq!(table.state(22), Some(LineState::Free));
    let seen = decode_lines(&rig.ask(&mut server, OTHER, HeaderRequest::Lines)).unwrap();
    assert_eq!(
        seen.state(18),
        Some(LineState::Output {
            holder: Holder::Other,
            asserted: true
        })
    );
}

#[test]
fn a_claim_sets_the_level_before_turning_the_line_an_output() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    assert_eq!(
        rig.change(&mut server, FAN, claim(18, Polarity::ActiveHigh, true)),
        Ok(())
    );
    assert!(gpio.is_output(18) && gpio.drives_high(18));
    let level = gpio.write_index(SET, 1 << 18).expect("the level was set");
    let direction = gpio
        .writes
        .borrow()
        .iter()
        .position(|&(offset, _)| offset == FSEL + 4)
        .expect("the direction was set");
    assert!(level < direction, "no glitch as the line turns");
    assert!(rig.kernel.watched().contains(&FAN));
}

#[test]
fn polarity_inverts_what_on_means() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    assert_eq!(
        rig.change(&mut server, FAN, claim(23, Polarity::ActiveLow, true)),
        Ok(())
    );
    assert!(!gpio.drives_high(23), "an active-low load runs low");
    let off = HeaderRequest::Drive {
        line: 23,
        asserted: false,
    };
    assert_eq!(rig.change(&mut server, FAN, off), Ok(()));
    assert!(gpio.drives_high(23));
}

#[test]
fn only_a_lent_free_line_can_be_claimed() {
    let gpio = Gpio::new();
    gpio.route_alt0(14);
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    let on = |line| claim(line, Polarity::ActiveHigh, true);
    assert_eq!(
        rig.change(&mut server, FAN, on(1)),
        Err(Errno::NotFound),
        "the HAT EEPROM's"
    );
    assert_eq!(
        rig.change(&mut server, FAN, on(40)),
        Err(Errno::NotFound),
        "not on the header"
    );
    assert_eq!(
        rig.change(&mut server, FAN, on(14)),
        Err(Errno::Busy),
        "routed to the console"
    );
    assert_eq!(rig.change(&mut server, FAN, on(18)), Ok(()));
    assert_eq!(rig.change(&mut server, OTHER, on(18)), Err(Errno::Busy));
    assert_eq!(rig.change(&mut server, FAN, on(18)), Err(Errno::Busy));
    assert!(!gpio.is_output(14));
}

#[test]
fn only_the_holder_drives_or_releases_its_line() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    assert_eq!(
        rig.change(&mut server, FAN, claim(18, Polarity::ActiveHigh, false)),
        Ok(())
    );
    let on = HeaderRequest::Drive {
        line: 18,
        asserted: true,
    };
    assert_eq!(
        rig.change(&mut server, OTHER, on),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        rig.change(&mut server, OTHER, HeaderRequest::Release { line: 18 }),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        rig.change(
            &mut server,
            FAN,
            HeaderRequest::Drive {
                line: 19,
                asserted: true
            }
        ),
        Err(Errno::NotFound)
    );
    assert!(!gpio.drives_high(18));
    assert_eq!(rig.change(&mut server, FAN, on), Ok(()));
    assert!(gpio.drives_high(18));
    assert_eq!(
        rig.change(&mut server, FAN, HeaderRequest::Release { line: 18 }),
        Ok(())
    );
    assert!(!gpio.is_output(18), "released to an input");
    assert!(
        !rig.kernel.watched().contains(&FAN),
        "nothing left to watch"
    );
}

#[test]
fn a_holder_that_ends_has_its_lines_returned_to_inputs() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    for line in [18, 24] {
        assert_eq!(
            rig.change(&mut server, FAN, claim(line, Polarity::ActiveHigh, true)),
            Ok(())
        );
    }
    server.peer_exited(FAN);
    assert!(!gpio.is_output(18) && !gpio.is_output(24));
    assert_eq!(
        rig.change(&mut server, OTHER, claim(18, Polarity::ActiveHigh, true)),
        Ok(())
    );
    let released = rig
        .records
        .all()
        .iter()
        .filter(|record| matches!(record, Record::Released { .. }))
        .count();
    assert_eq!(released, 2);
}

#[test]
fn an_edge_is_answered_when_it_fires_and_one_that_fires_unwaited_is_latched() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    let interrupt = link(17, RISING);
    rig.grant(DEVICE, &interrupt);
    let first = rig.wait(&mut server, DEVICE, interrupt);
    assert!(gpio.detects(17) && !gpio.is_output(17));
    assert_eq!(rig.kernel.answer(first), None, "nothing fired yet");
    gpio.raise(17);
    server.woke(0).unwrap();
    assert_eq!(decode_reply(&rig.kernel.answer(first).unwrap()), Ok(()));
    assert!(!gpio.pending(17), "acknowledged");
    gpio.lower(17);
    gpio.raise(17);
    server.woke(0).unwrap();
    let second = rig.wait(&mut server, DEVICE, interrupt);
    assert_eq!(
        decode_reply(&rig.kernel.answer(second).unwrap()),
        Ok(()),
        "the latched edge answers at once"
    );
}

#[test]
fn a_level_is_masked_as_it_fires_and_cannot_storm() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    let interrupt = link(30, HIGH);
    rig.grant(DEVICE, &interrupt);
    gpio.raise(30);
    assert!(!gpio.pending(30), "not armed until waited on");
    let ticket = rig.wait(&mut server, DEVICE, interrupt);
    assert!(gpio.bank(HEN, 30));
    gpio.raise(30);
    server.woke(1).unwrap();
    assert_eq!(decode_reply(&rig.kernel.answer(ticket).unwrap()), Ok(()));
    assert!(!gpio.detects(30), "masked until the next wait");
    assert!(!gpio.pending(30), "the standing level does not re-latch");
}

#[test]
fn a_line_is_held_by_one_kind_of_holder_at_a_time() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    assert_eq!(
        rig.change(&mut server, FAN, claim(18, Polarity::ActiveHigh, true)),
        Ok(())
    );
    let interrupt = link(18, RISING);
    rig.grant(DEVICE, &interrupt);
    let ticket = rig.wait(&mut server, DEVICE, interrupt);
    assert_eq!(
        decode_reply(&rig.kernel.answer(ticket).unwrap()),
        Err(Errno::Busy)
    );
    let held = link(17, RISING);
    rig.grant(DEVICE, &held);
    rig.wait(&mut server, DEVICE, held);
    assert_eq!(
        rig.change(&mut server, FAN, claim(17, Polarity::ActiveHigh, true)),
        Err(Errno::Busy)
    );
}

#[test]
fn a_wait_needs_the_link_a_bound_bank_and_a_trigger_the_block_detects() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, 1);
    let ungranted = link(4, RISING);
    let ticket = rig.wait(&mut server, DEVICE, ungranted);
    assert_eq!(
        decode_reply(&rig.kernel.answer(ticket).unwrap()),
        Err(Errno::PermissionDenied)
    );
    let unbanked = link(30, RISING);
    rig.grant(DEVICE, &unbanked);
    let ticket = rig.wait(&mut server, DEVICE, unbanked);
    assert_eq!(
        decode_reply(&rig.kernel.answer(ticket).unwrap()),
        Err(Errno::NotSupported),
        "the bank's interrupt is not bound"
    );
    let odd = link(5, 6);
    rig.grant(DEVICE, &odd);
    let ticket = rig.wait(&mut server, DEVICE, odd);
    assert_eq!(
        decode_reply(&rig.kernel.answer(ticket).unwrap()),
        Err(Errno::NotSupported)
    );
}

#[test]
fn an_ended_consumer_s_line_is_taken_by_its_link_s_next_holder() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    let interrupt = link(17, RISING);
    rig.grant(DEVICE, &interrupt);
    rig.grant(OTHER, &interrupt);
    rig.wait(&mut server, DEVICE, interrupt);
    let refused = rig.wait(&mut server, OTHER, interrupt);
    assert_eq!(
        decode_reply(&rig.kernel.answer(refused).unwrap()),
        Err(Errno::Busy)
    );
    rig.kernel.end(DEVICE);
    rig.wait(&mut server, OTHER, interrupt);
    assert!(rig.records.all().contains(&Record::Reclaimed {
        line: 17,
        from: DEVICE
    }));
    server.peer_exited(OTHER);
    assert!(!gpio.detects(17), "released with its consumer");
}

#[test]
fn a_header_call_the_kernel_cannot_attribute_is_refused() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    let ticket = rig.kernel.call(FAN);
    rig.header
        .queue
        .borrow_mut()
        .push_back((ticket, [0u8; HEADER_REQUEST_LEN].to_vec()));
    server.woke(BANK_INTERRUPTS).unwrap();
    assert_eq!(
        decode_status(&rig.kernel.answer(ticket).unwrap()),
        Err(Errno::BadMagic)
    );
    assert!(rig.records.all().contains(&Record::Refused {
        caller: Some(FAN),
        reason: Errno::BadMagic
    }));
}

#[test]
fn every_claim_is_recorded_with_its_role() {
    let gpio = Gpio::new();
    let rig = Rig::new();
    let mut server = rig.controller(&gpio, BANK_INTERRUPTS);
    assert_eq!(
        rig.change(&mut server, FAN, claim(18, Polarity::ActiveHigh, true)),
        Ok(())
    );
    assert!(rig.records.all().contains(&Record::Claimed {
        line: 18,
        holder: FAN,
        role: Role::Output
    }));
}

/// A host that grants the load gate, or does not.
struct Host(bool);

impl DriverHost for Host {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        self.0 && cap == CapabilityId::DRV_LOAD
    }
    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

#[test]
fn the_bind_table_and_layouts_agree_and_loading_needs_its_capability() {
    assert_eq!(BIND_KEYS.len(), LAYOUTS.len());
    assert_eq!(LAYOUTS, [Layout::BCM2711, Layout::BCM2835]);
    assert!(register(&Host(true)).is_ok());
    assert_eq!(register(&Host(false)), Err(DriverError::PermissionDenied));
}
