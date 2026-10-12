//! The endpoint over a scripted bus, answered by the shared fake kernel.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::driver::i2c::{
    decode_close_reply, decode_open_reply, decode_transfer_reply, BusFacts, Direction, I2cAddress,
    I2cChannel, I2cFault, I2cMessage, I2cOp, I2cReach, I2cRequest, I2cTransaction, RepeatedStart,
    I2C_BUFFER_LEN, I2C_CONTROLLER_ENDPOINTS, I2C_MAX_REQUEST,
};
use tairix_abi::hwlink::{LinkRequest, RegionGrant};
use tairix_abi::hwtree::HwResource;
use tairix_abi::reply::decode_status_reply;
use tairix_abi::{DriverError, Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::Supplier;

use crate::{BufferHost, Controller, I2cBus, Record};

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const RTC: ProcId = instance(1);
const CODEC: ProcId = instance(2);
const HDMI: ProcId = instance(3);
const DRIVER: ProcId = instance(9);

const RTC_ADDRESS: u32 = 0x68;
const CODEC_ADDRESS: u32 = 0x4c;
const FACTS: BusFacts = BusFacts {
    speed_hz: 100_000,
    repeated_start: RepeatedStart::BeforeFinalRead,
};

fn endpoint() -> u64 {
    I2C_CONTROLLER_ENDPOINTS.endpoint(7)
}

fn target(address: u32) -> LinkRequest {
    LinkRequest::new(endpoint(), 0, &[address], b"").expect("valid")
}

fn bus_link() -> LinkRequest {
    LinkRequest::new(endpoint(), 0, &[], b"ddc").expect("valid")
}

fn address(raw: u32) -> I2cAddress {
    I2cAddress::from_cell(raw).expect("usable")
}

fn message(raw: u32, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address: address(raw),
        direction,
        offset,
        len,
    }
}

fn register_read(raw: u32) -> I2cTransaction {
    I2cTransaction::new(&[
        message(raw, Direction::Write, 0, 1),
        message(raw, Direction::Read, 1, 7),
    ])
    .expect("valid")
}

/// What the bus ran: each transaction and the first byte of the buffer it ran
/// over, which names the channel that buffer was made for.
#[derive(Default)]
struct BusState {
    ran: Vec<(I2cTransaction, u8)>,
    outcome: Option<Result<Result<(), I2cFault>, DriverError>>,
}

#[derive(Clone, Default)]
struct Bus(Rc<RefCell<BusState>>);

impl I2cBus for Bus {
    fn facts(&self) -> BusFacts {
        FACTS
    }

    fn transfer(
        &mut self,
        transaction: &I2cTransaction,
        buffer: &mut [u8],
    ) -> Result<Result<(), I2cFault>, DriverError> {
        let mut state = self.0.borrow_mut();
        state.ran.push((*transaction, buffer[0]));
        state.outcome.unwrap_or(Ok(Ok(())))
    }
}

/// A buffer whose bytes all carry the order it was made in, and which counts
/// itself out when dropped.
struct Buffer {
    bytes: Vec<u8>,
    live: Rc<Cell<usize>>,
}

impl AsMut<[u8]> for Buffer {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1);
    }
}

#[derive(Default)]
struct HostState {
    made: u8,
    granted: u64,
    refuse_make: Option<Errno>,
    refuse_grant: Option<Errno>,
    short: bool,
}

#[derive(Clone, Default)]
struct Host {
    state: Rc<RefCell<HostState>>,
    live: Rc<Cell<usize>>,
}

impl BufferHost for Host {
    type Buffer = Buffer;

    fn make(&mut self) -> Result<Buffer, Errno> {
        let mut state = self.state.borrow_mut();
        if let Some(reason) = state.refuse_make {
            return Err(reason);
        }
        state.made += 1;
        let len = if state.short {
            I2C_BUFFER_LEN - 1
        } else {
            I2C_BUFFER_LEN
        };
        self.live.set(self.live.get() + 1);
        Ok(Buffer {
            bytes: vec![state.made; len],
            live: self.live.clone(),
        })
    }

    fn grant(&mut self, _buffer: &Buffer, _ticket: u64) -> Result<u64, Errno> {
        let mut state = self.state.borrow_mut();
        if let Some(reason) = state.refuse_grant {
            return Err(reason);
        }
        state.granted += 1;
        Ok(state.granted)
    }

    fn instance(&self) -> ProcId {
        DRIVER
    }
}

struct Rig {
    kernel: FakeKernel,
    records: Records<Record>,
    bus: Bus,
    host: Host,
    controller: Controller<Bus, Host, FakeKernel, Records<Record>>,
}

impl Rig {
    fn new(holders: &[(ProcId, LinkRequest)]) -> Self {
        let kernel = FakeKernel::new();
        for (holder, link) in holders {
            kernel.grant(*holder, HwResource::request(link));
        }
        let records = Records::default();
        let bus = Bus::default();
        let host = Host::default();
        let controller = Controller::new(
            bus.clone(),
            host.clone(),
            kernel.clone(),
            records.clone(),
            endpoint(),
        );
        Self {
            kernel,
            records,
            bus,
            host,
            controller,
        }
    }

    fn serve(&mut self, caller: ProcId, frame: &[u8]) -> Vec<u8> {
        let ticket = self.kernel.call(caller);
        self.controller.serve(ticket, frame);
        self.kernel
            .answer(ticket)
            .expect("every call is answered at once")
    }

    fn call(&mut self, caller: ProcId, request: &I2cRequest) -> Vec<u8> {
        let mut frame = [0u8; I2C_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("fits");
        self.serve(caller, &frame[..len])
    }

    fn open(&mut self, caller: ProcId, link: LinkRequest) -> Result<I2cChannel, Errno> {
        decode_open_reply(&self.call(caller, &I2cRequest::Open(link)))
    }

    fn transfer(
        &mut self,
        caller: ProcId,
        link: LinkRequest,
        transaction: I2cTransaction,
    ) -> Result<Result<(), I2cFault>, Errno> {
        decode_transfer_reply(&self.call(caller, &I2cRequest::Transfer { link, transaction }))
    }

    fn close(&mut self, caller: ProcId, link: LinkRequest) -> Result<(), Errno> {
        decode_close_reply(&self.call(caller, &I2cRequest::Close(link)))
    }

    fn ran(&self) -> Vec<(I2cTransaction, u8)> {
        self.bus.0.borrow().ran.clone()
    }

    fn live(&self) -> usize {
        self.host.live.get()
    }

    fn refusals(&self) -> Vec<(Option<I2cOp>, Errno)> {
        self.records
            .all()
            .into_iter()
            .filter_map(|record| match record {
                Record::Refused { op, reason, .. } => Some((op, reason)),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_holder_opens_its_channel_and_is_granted_a_buffer_made_for_it() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    let channel = rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    assert_eq!(
        channel,
        I2cChannel {
            facts: FACTS,
            buffer: RegionGrant {
                grant: 1,
                grantor: DRIVER,
            },
        }
    );
    assert_eq!(rig.live(), 1);
    assert_eq!(rig.kernel.watched(), [RTC]);
    assert_eq!(
        rig.records.all(),
        [Record::Opened {
            holder: RTC,
            reach: I2cReach::Target(address(RTC_ADDRESS)),
        }]
    );
}

#[test]
fn a_link_the_caller_does_not_hold_opens_nothing() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    assert_eq!(
        rig.open(CODEC, target(RTC_ADDRESS)),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        rig.open(RTC, target(CODEC_ADDRESS)),
        Err(Errno::PermissionDenied),
        "a holder of one target's link holds no other's"
    );
    let elsewhere = LinkRequest::new(I2C_CONTROLLER_ENDPOINTS.endpoint(8), 0, &[RTC_ADDRESS], b"")
        .expect("valid");
    assert_eq!(rig.open(RTC, elsewhere), Err(Errno::OutOfRange));
    assert_eq!(rig.live(), 0);
    assert!(rig.kernel.watched().is_empty());
}

#[test]
fn a_channel_is_opened_once() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    assert_eq!(
        rig.open(RTC, target(RTC_ADDRESS)),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(rig.live(), 1);
}

#[test]
fn a_target_channel_reaches_its_address_alone() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), register_read(CODEC_ADDRESS)),
        Err(Errno::PermissionDenied)
    );
    let mixed = I2cTransaction::new(&[
        message(RTC_ADDRESS, Direction::Write, 0, 1),
        message(CODEC_ADDRESS, Direction::Read, 1, 1),
    ])
    .expect("valid");
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), mixed),
        Err(Errno::PermissionDenied),
        "one message aimed elsewhere refuses the whole transaction"
    );
    assert!(rig.ran().is_empty(), "the bus is never touched");
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS)),
        Ok(Ok(()))
    );
    assert_eq!(rig.ran(), [(register_read(RTC_ADDRESS), 1)]);
}

#[test]
fn a_bus_channel_reaches_every_address() {
    let mut rig = Rig::new(&[(HDMI, bus_link())]);
    let channel = rig.open(HDMI, bus_link()).expect("opened");
    assert_eq!(channel.facts, FACTS);
    let edid = I2cTransaction::new(&[
        message(0x30, Direction::Write, 0, 1),
        message(0x50, Direction::Write, 1, 1),
        message(0x50, Direction::Read, 2, 128),
    ])
    .expect("valid");
    assert_eq!(rig.transfer(HDMI, bus_link(), edid), Ok(Ok(())));
    assert_eq!(rig.ran(), [(edid, 1)]);
}

#[test]
fn a_transaction_the_bus_cannot_issue_is_refused_before_the_bus_is_touched() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    let read_then_write = I2cTransaction::new(&[
        message(RTC_ADDRESS, Direction::Read, 0, 1),
        message(RTC_ADDRESS, Direction::Write, 1, 1),
    ])
    .expect("valid");
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), read_then_write),
        Err(Errno::NotSupported)
    );
    assert!(rig.ran().is_empty());
}

#[test]
fn each_transaction_runs_over_its_own_channel_s_buffer() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS)), (CODEC, target(CODEC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    rig.open(CODEC, target(CODEC_ADDRESS)).expect("opened");
    rig.transfer(CODEC, target(CODEC_ADDRESS), register_read(CODEC_ADDRESS))
        .expect("answered")
        .expect("ran");
    rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS))
        .expect("answered")
        .expect("ran");
    assert_eq!(
        rig.ran(),
        [
            (register_read(CODEC_ADDRESS), 2),
            (register_read(RTC_ADDRESS), 1)
        ]
    );
}

#[test]
fn a_transfer_needs_an_open_channel_and_a_link_still_held() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS)),
        Err(Errno::NotFound)
    );
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    rig.kernel
        .revoke(RTC, &HwResource::request(&target(RTC_ADDRESS)));
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS)),
        Err(Errno::PermissionDenied)
    );
    assert!(rig.ran().is_empty());
}

#[test]
fn a_fault_on_the_bus_is_answered_and_recorded() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    let fault = I2cFault::NoAck { message: 0 };
    rig.bus.0.borrow_mut().outcome = Some(Ok(Err(fault)));
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS)),
        Ok(Err(fault))
    );
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::Faulted { holder: RTC, fault })
    );
}

#[test]
fn a_controller_that_cannot_run_a_transaction_refuses_it() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    for (refused, reason) in [
        (DriverError::DeviceFault, Errno::DeviceFault),
        (DriverError::OutOfRange, Errno::OutOfRange),
        (DriverError::Unsupported, Errno::NotSupported),
    ] {
        rig.bus.0.borrow_mut().outcome = Some(Err(refused));
        assert_eq!(
            rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS)),
            Err(reason)
        );
    }
    assert_eq!(
        rig.refusals().last(),
        Some(&(Some(I2cOp::Transfer), Errno::NotSupported))
    );
}

#[test]
fn closing_drops_the_channel_and_its_buffer_and_stops_watching_its_holder() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    assert_eq!(rig.close(RTC, target(RTC_ADDRESS)), Ok(()));
    assert_eq!(rig.live(), 0);
    assert!(rig.kernel.watched().is_empty());
    assert_eq!(rig.close(RTC, target(RTC_ADDRESS)), Err(Errno::NotFound));
    assert_eq!(
        rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS)),
        Err(Errno::NotFound)
    );
    rig.open(RTC, target(RTC_ADDRESS))
        .expect("a closed channel opens again");
}

#[test]
fn a_holder_s_end_drops_every_channel_it_had() {
    let mut rig = Rig::new(&[
        (CODEC, target(CODEC_ADDRESS)),
        (CODEC, target(0x4d)),
        (RTC, target(RTC_ADDRESS)),
    ]);
    rig.open(CODEC, target(CODEC_ADDRESS)).expect("opened");
    rig.open(CODEC, target(0x4d)).expect("opened");
    rig.open(RTC, target(RTC_ADDRESS)).expect("opened");
    assert_eq!(rig.kernel.watched(), [CODEC, RTC], "watched once each");
    rig.kernel.end(CODEC);
    rig.controller.peer_exited(CODEC);
    assert_eq!(rig.live(), 1);
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::Abandoned { holder: CODEC })
    );
    rig.transfer(RTC, target(RTC_ADDRESS), register_read(RTC_ADDRESS))
        .expect("answered")
        .expect("the survivor's channel stands");
}

#[test]
fn an_open_that_fails_leaves_nothing_behind() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    rig.host.state.borrow_mut().refuse_grant = Some(Errno::PermissionDenied);
    assert_eq!(
        rig.open(RTC, target(RTC_ADDRESS)),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(rig.live(), 0, "the ungranted buffer is dropped");
    assert!(rig.kernel.watched().is_empty());
    rig.host.state.borrow_mut().refuse_grant = None;

    rig.host.state.borrow_mut().refuse_make = Some(Errno::OutOfMemory);
    assert_eq!(rig.open(RTC, target(RTC_ADDRESS)), Err(Errno::OutOfMemory));
    rig.host.state.borrow_mut().refuse_make = None;

    rig.host.state.borrow_mut().short = true;
    assert_eq!(rig.open(RTC, target(RTC_ADDRESS)), Err(Errno::OutOfMemory));
    assert_eq!(rig.live(), 0);
    rig.host.state.borrow_mut().short = false;

    rig.kernel.end(RTC);
    assert_eq!(
        rig.open(RTC, target(RTC_ADDRESS)),
        Err(Errno::NotFound),
        "a holder that has ended cannot be watched"
    );
    assert_eq!(rig.live(), 0);
}

#[test]
fn a_frame_or_caller_the_endpoint_cannot_believe_is_refused() {
    let mut rig = Rig::new(&[(RTC, target(RTC_ADDRESS))]);
    let reply = rig.serve(RTC, &[0u8; 4]);
    assert!(decode_status_reply(&reply).is_err());
    rig.controller.serve(FakeKernel::OUT_OF_SERVICE, &[0u8; 4]);
    assert_eq!(
        rig.kernel.answered(),
        1,
        "an unnamed caller has nobody to tell"
    );
    assert_eq!(rig.refusals().len(), 2);
    assert_eq!(rig.refusals()[1], (None, Errno::NotFound));
}
