//! The endpoint over a scripted engine, with a kernel that answers the grant
//! questions by the kernel's own rule.

extern crate std;

use core::num::NonZeroU32;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::dmaengine::{
    decode_done_reply, decode_open_reply, decode_position_reply, decode_prepare_reply,
    decode_wait_reply, CyclicParams, CyclicTransfer, DmaChannel, DmaChannelEvent, DmaDirection,
    DmaEngine, DmaEngineOp, DmaEngineRequest, Halted, WaitEnd, DMA_CONTROLLER_ENDPOINTS,
    DMA_ENGINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::time::Duration64;
use tairix_abi::{DmaCoherence, DriverError, Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::{Recorder, SupplierHost};

use crate::{split_windows, Buffer, Controller, ControllerHost, Record};

const INSTANCE: ProcId = instance(0xD0);
const PLAYER: ProcId = instance(1);
const OTHER: ProcId = instance(2);

const PERIPHERALS: u64 = 0xFE00_0000;
const FIFO: u64 = PERIPHERALS + 0x20_3004;
/// Where the controller reaches the peripheral window.
const PERIPHERAL_BUS: u64 = 0x7E00_0000;
const PERIOD: u32 = 1024;
const PERIODS: u32 = 4;

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

fn endpoint() -> u64 {
    DMA_CONTROLLER_ENDPOINTS.endpoint(9)
}

fn line(selector: u32) -> LinkRequest {
    LinkRequest::new(endpoint(), 0, &[selector], b"tx").expect("valid")
}

fn params() -> CyclicParams {
    CyclicParams {
        fifo: FIFO,
        direction: DmaDirection::MemoryToDevice,
        period_bytes: PERIOD,
        periods: PERIODS,
    }
}

/// The window the controller reaches peripherals through: its first argument
/// is the window's exclusive top.
fn peripheral_window() -> HwResource {
    HwResource::dma_translated(
        PERIPHERALS + 0x0180_0000,
        0x0180_0000,
        PERIPHERAL_BUS,
        DmaCoherence::Snooped,
    )
}

/// How a scripted channel takes its reset.
#[derive(Copy, Clone, Default, Eq, PartialEq)]
enum Reset {
    #[default]
    Drained,
    Undrained,
    Refused,
}

/// One scripted channel.
#[derive(Default)]
struct ChannelState {
    chain: Option<CyclicTransfer>,
    running: bool,
    position: Option<u32>,
    events: VecDeque<Result<DmaChannelEvent, DriverError>>,
    reset: Reset,
    refuse_chain: bool,
    resets: u32,
}

#[derive(Default)]
struct Script {
    channels: Vec<ChannelState>,
    refuse_line: bool,
}

type Shared = Rc<RefCell<Script>>;

struct FakeChannel {
    index: usize,
    script: Shared,
}

impl FakeChannel {
    fn with<T>(&self, f: impl FnOnce(&mut ChannelState) -> T) -> T {
        f(&mut self.script.borrow_mut().channels[self.index])
    }
}

impl DmaChannel for FakeChannel {
    fn prepare(&mut self, _: &LinkRequest, transfer: &CyclicTransfer) -> Result<(), DriverError> {
        self.with(|state| {
            if state.running {
                return Err(DriverError::Busy);
            }
            if state.refuse_chain {
                return Err(DriverError::OutOfRange);
            }
            state.chain = Some(*transfer);
            state.position = Some(0);
            Ok(())
        })
    }

    fn start(&mut self) -> Result<(), DriverError> {
        self.with(|state| {
            if state.chain.is_none() {
                return Err(DriverError::NotFound);
            }
            state.running = true;
            Ok(())
        })
    }

    fn stop(&mut self) -> Result<Halted, DriverError> {
        self.with(|state| {
            state.resets += 1;
            let halted = match state.reset {
                Reset::Drained => Halted::Drained,
                Reset::Undrained => Halted::Undrained,
                Reset::Refused => return Err(DriverError::DeviceFault),
            };
            state.running = false;
            Ok(halted)
        })
    }

    fn release(&mut self) -> Result<Halted, DriverError> {
        let halted = self.stop()?;
        self.with(|state| state.chain = None);
        Ok(halted)
    }

    fn position(&self) -> Result<u32, DriverError> {
        self.with(|state| {
            if state.chain.is_none() {
                return Err(DriverError::NotFound);
            }
            state.position.ok_or(DriverError::DeviceFault)
        })
    }

    fn take_event(&mut self) -> Result<DmaChannelEvent, DriverError> {
        self.with(|state| {
            state
                .events
                .pop_front()
                .unwrap_or(Ok(DmaChannelEvent::Quiet))
        })
    }
}

struct FakeEngine {
    script: Shared,
    channels: Vec<FakeChannel>,
}

impl DmaEngine for FakeEngine {
    type Channel = FakeChannel;

    fn channel_count(&self) -> u8 {
        u8::try_from(self.channels.len()).expect("fits")
    }

    fn channel(&mut self, index: u8) -> Option<&mut FakeChannel> {
        self.channels.get_mut(usize::from(index))
    }

    fn accept(&self, _: &LinkRequest) -> Result<(), DriverError> {
        if self.script.borrow().refuse_line {
            Err(DriverError::Unsupported)
        } else {
            Ok(())
        }
    }

    fn admit(&self, line: &LinkRequest, params: &CyclicParams) -> Result<u32, DriverError> {
        self.accept(line)?;
        if params.period_bytes == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(4)
    }
}

/// What the kernel holds for the endpoint beyond its callers: the buffers it
/// carved and granted, the clock, and whether it refuses either.
#[derive(Default)]
struct Memory {
    next_region: u64,
    live: Vec<Buffer>,
    granted: Vec<(u64, ProcId)>,
    refuse_grants: bool,
    refuse_carves: bool,
    now: u64,
}

struct Host {
    kernel: FakeKernel,
    memory: Rc<RefCell<Memory>>,
    records: Records<Record>,
}

impl ControllerHost for Host {
    fn carve(&mut self, bytes: u32) -> Result<Buffer, Errno> {
        let mut memory = self.memory.borrow_mut();
        if memory.refuse_carves {
            return Err(Errno::OutOfMemory);
        }
        memory.next_region += 1;
        let region = memory.next_region;
        let buffer = Buffer {
            region,
            base: 0x10_0000_0000 + region * 0x10_0000,
            len: usize::try_from(bytes).expect("fits"),
            bus: 0xC000_0000 + region * 0x10_0000,
        };
        memory.live.push(buffer);
        Ok(buffer)
    }

    fn release(&mut self, buffer: &Buffer) {
        self.memory.borrow_mut().live.retain(|live| live != buffer);
    }

    fn grant(&mut self, buffer: &Buffer, ticket: u64) -> Result<u64, Errno> {
        let caller = self.kernel.caller(ticket)?;
        let mut memory = self.memory.borrow_mut();
        if memory.refuse_grants {
            return Err(Errno::PermissionDenied);
        }
        memory.granted.push((buffer.region, caller));
        Ok(0x6000 + buffer.region)
    }

    fn now(&self) -> Duration64 {
        Duration64::from_nanos(self.memory.borrow().now)
    }

    fn instance(&self) -> ProcId {
        INSTANCE
    }

    fn record(&mut self, record: Record) {
        self.records.record(record);
    }
}

struct Rig {
    script: Shared,
    kernel: FakeKernel,
    memory: Rc<RefCell<Memory>>,
    records: Records<Record>,
    controller: Controller<FakeEngine, FakeKernel, Host>,
}

/// A controller of `count` channels owning `owned` and serving `usable`.
fn rig_with(count: usize, owned: u64, usable: u64, setup: impl FnOnce(&mut Script)) -> Rig {
    try_rig(count, owned, usable, setup).expect("every reset is issued")
}

fn try_rig(count: usize, owned: u64, usable: u64, setup: impl FnOnce(&mut Script)) -> Option<Rig> {
    let script: Shared = Rc::default();
    script
        .borrow_mut()
        .channels
        .resize_with(count, ChannelState::default);
    setup(&mut script.borrow_mut());
    let engine = FakeEngine {
        script: Rc::clone(&script),
        channels: (0..count)
            .map(|index| FakeChannel {
                index,
                script: Rc::clone(&script),
            })
            .collect(),
    };
    let kernel = FakeKernel::new();
    for (holder, grant) in [
        (PLAYER, HwResource::request(&line(10))),
        (PLAYER, HwResource::mmio(FIFO & !0xFFF, 0x1000)),
        (OTHER, HwResource::request(&line(10))),
        (OTHER, HwResource::request(&line(11))),
        (OTHER, HwResource::mmio(FIFO & !0xFFF, 0x1000)),
    ] {
        kernel.grant(holder, grant);
    }
    let memory = Rc::new(RefCell::new(Memory::default()));
    let records = Records::default();
    let host = Host {
        kernel: kernel.clone(),
        memory: Rc::clone(&memory),
        records: records.clone(),
    };
    let controller = Controller::new(
        engine,
        kernel.clone(),
        host,
        endpoint(),
        (owned, usable),
        Some(peripheral_window()),
    )?;
    Some(Rig {
        script,
        kernel,
        memory,
        records,
        controller,
    })
}

fn rig() -> Rig {
    rig_with(4, 0b1111, 0b1111, |_| {})
}

impl Rig {
    /// Serve `request` from `caller`, answering the ticket it travelled on.
    fn post(&mut self, caller: ProcId, request: &DmaEngineRequest) -> u64 {
        let mut frame = [0u8; DMA_ENGINE_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("encodes");
        self.post_frame(caller, &frame[..len])
    }

    fn post_frame(&mut self, caller: ProcId, frame: &[u8]) -> u64 {
        let ticket = self.kernel.call(caller);
        self.controller.serve(ticket, frame);
        ticket
    }

    fn reply(&self, ticket: u64) -> Option<Vec<u8>> {
        self.kernel.answer(ticket)
    }

    fn call(&mut self, caller: ProcId, request: &DmaEngineRequest) -> Vec<u8> {
        let ticket = self.post(caller, request);
        self.reply(ticket).expect("answered at once")
    }

    fn open(&mut self, caller: ProcId, selector: u32) -> Result<u8, Errno> {
        decode_open_reply(&self.call(caller, &DmaEngineRequest::Open(line(selector))))
    }

    fn prepare(&mut self, caller: ProcId, channel: u8) -> Result<u64, Errno> {
        let reply = self.call(
            caller,
            &DmaEngineRequest::Prepare {
                channel,
                params: params(),
            },
        );
        decode_prepare_reply(&reply).map(|grant| {
            assert_eq!(grant.grantor, INSTANCE);
            grant.grant
        })
    }

    fn done(&mut self, caller: ProcId, request: &DmaEngineRequest) -> Result<(), Errno> {
        let op = request.op();
        decode_done_reply(&self.call(caller, request), op)
    }

    fn running(&mut self) -> u8 {
        let channel = self.open(PLAYER, 10).expect("opens");
        self.prepare(PLAYER, channel).expect("prepares");
        self.done(PLAYER, &DmaEngineRequest::Start { channel })
            .expect("starts");
        channel
    }

    fn state<T>(&self, channel: u8, f: impl FnOnce(&mut ChannelState) -> T) -> T {
        f(&mut self.script.borrow_mut().channels[usize::from(channel)])
    }

    /// Place `channel` `bytes` into its buffer and raise `event` on it.
    fn raise(&mut self, channel: u8, bytes: u32, event: Result<DmaChannelEvent, DriverError>) {
        self.state(channel, |state| {
            state.position = Some(bytes % (PERIOD * PERIODS));
            state.events.push_back(event);
        });
        self.controller.interrupt(1 << channel);
    }

    fn records(&self) -> Vec<Record> {
        self.records.all()
    }

    fn live(&self) -> usize {
        self.memory.borrow().live.len()
    }
}

#[test]
fn bring_up_resets_every_owned_channel_and_serves_only_the_usable_ones() {
    let rig = rig_with(4, 0b1011, 0b0011, |_| {});
    let resets: Vec<u32> = (0..4)
        .map(|channel| rig.state(channel, |s| s.resets))
        .collect();
    assert_eq!(resets, [1, 1, 0, 1]);
    assert_eq!(rig.controller.usable(), 0b0011);
}

#[test]
fn a_reset_that_cannot_be_issued_at_bring_up_refuses_the_controller() {
    assert!(try_rig(2, 0b11, 0b11, |script| script.channels[1].reset =
        Reset::Refused)
    .is_none());
}

#[test]
fn a_reset_that_cut_writes_off_at_bring_up_is_recorded() {
    let rig = rig_with(2, 0b11, 0b11, |script| {
        script.channels[0].reset = Reset::Undrained;
    });
    assert_eq!(rig.records(), [Record::Undrained { channel: 0 }]);
}

#[test]
fn a_channel_opens_only_for_a_line_the_kernel_attests_the_caller_holds() {
    let mut rig = rig();
    assert_eq!(rig.open(PLAYER, 11), Err(Errno::PermissionDenied));
    assert!(matches!(
        rig.records().as_slice(),
        [Record::Refused {
            op: Some(DmaEngineOp::Open),
            caller: Some(PLAYER),
            reason: Errno::PermissionDenied,
        }]
    ));
    assert_eq!(rig.open(PLAYER, 10), Ok(0));
}

#[test]
fn a_line_naming_another_controller_or_a_serving_the_engine_refuses_is_turned_away() {
    let mut rig = rig();
    let foreign =
        LinkRequest::new(DMA_CONTROLLER_ENDPOINTS.endpoint(3), 0, &[10], b"tx").expect("valid");
    let reply = rig.call(PLAYER, &DmaEngineRequest::Open(foreign));
    assert_eq!(decode_open_reply(&reply), Err(Errno::OutOfRange));
    rig.script.borrow_mut().refuse_line = true;
    assert_eq!(rig.open(PLAYER, 10), Err(Errno::NotSupported));
}

#[test]
fn the_lowest_free_usable_channel_is_claimed_until_none_is_left() {
    let mut rig = rig_with(4, 0b1111, 0b1010, |_| {});
    assert_eq!(rig.open(PLAYER, 10), Ok(1));
    assert_eq!(rig.open(OTHER, 11), Ok(3));
    rig.kernel.grant(OTHER, HwResource::request(&line(12)));
    assert_eq!(rig.open(OTHER, 12), Err(Errno::Busy));
    assert_eq!(rig.kernel.watched(), [PLAYER, OTHER]);
}

#[test]
fn a_held_line_stays_with_a_live_holder_and_is_reclaimed_from_an_ended_one() {
    let mut rig = rig();
    assert_eq!(rig.open(PLAYER, 10), Ok(0));
    assert_eq!(rig.open(PLAYER, 10), Err(Errno::AlreadyExists));
    assert_eq!(rig.open(OTHER, 10), Err(Errno::Busy));
    rig.kernel.end(PLAYER);
    assert_eq!(rig.open(OTHER, 10), Ok(0));
    assert!(rig.records().contains(&Record::Reclaimed {
        channel: 0,
        from: PLAYER
    }));
}

#[test]
fn every_later_call_must_come_from_the_instance_that_opened_the_channel() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    assert_eq!(rig.prepare(OTHER, channel), Err(Errno::PermissionDenied));
    for request in [
        DmaEngineRequest::Start { channel },
        DmaEngineRequest::Stop { channel },
        DmaEngineRequest::Close { channel },
    ] {
        assert_eq!(rig.done(OTHER, &request), Err(Errno::PermissionDenied));
    }
    let position = rig.call(OTHER, &DmaEngineRequest::Position { channel });
    assert_eq!(
        decode_position_reply(&position),
        Err(Errno::PermissionDenied)
    );
    let wait = rig.call(OTHER, &DmaEngineRequest::Wait { channel, after: 0 });
    assert_eq!(decode_wait_reply(&wait), Err(Errno::PermissionDenied));
}

#[test]
fn prepare_carves_the_buffer_hands_the_engine_bus_addresses_and_grants_the_caller_alone() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    let grant = rig.prepare(PLAYER, channel).expect("prepares");
    let memory = rig.memory.borrow();
    let [buffer] = memory.live.as_slice() else {
        panic!("one buffer carved");
    };
    assert_eq!(buffer.len, (PERIOD * PERIODS) as usize);
    assert_eq!(grant, 0x6000 + buffer.region);
    assert_eq!(memory.granted, [(buffer.region, PLAYER)]);
    let chain = rig
        .state(channel, |state| state.chain)
        .expect("chain built");
    assert_eq!(
        chain,
        CyclicTransfer {
            buffer: buffer.bus,
            fifo: PERIPHERAL_BUS + (FIFO - PERIPHERALS),
            direction: DmaDirection::MemoryToDevice,
            period_bytes: PERIOD,
            periods: PERIODS,
        }
    );
}

#[test]
fn a_fifo_the_caller_does_not_hold_is_refused_before_anything_is_carved() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    rig.kernel
        .revoke(PLAYER, &HwResource::mmio(FIFO & !0xFFF, 0x1000));
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::PermissionDenied));
    assert_eq!(rig.live(), 0);
}

#[test]
fn a_fifo_outside_the_window_the_controller_reaches_peripherals_through_is_refused() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    let outside = 0x1_0000_0000;
    rig.kernel.grant(PLAYER, HwResource::mmio(outside, 0x1000));
    let reply = rig.call(
        PLAYER,
        &DmaEngineRequest::Prepare {
            channel,
            params: CyclicParams {
                fifo: outside,
                ..params()
            },
        },
    );
    assert_eq!(decode_prepare_reply(&reply), Err(Errno::OutOfRange));
    assert_eq!(rig.live(), 0);
}

#[test]
fn a_chain_the_engine_refuses_releases_its_buffer() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    rig.state(channel, |state| state.refuse_chain = true);
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::OutOfRange));
    assert_eq!(rig.live(), 0);
}

#[test]
fn a_failed_grant_resets_the_channel_and_releases_both_buffers() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    rig.prepare(PLAYER, channel).expect("prepares");
    rig.memory.borrow_mut().refuse_grants = true;
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::PermissionDenied));
    assert_eq!(rig.live(), 0);
    assert_eq!(
        rig.done(PLAYER, &DmaEngineRequest::Start { channel }),
        Err(Errno::NotFound)
    );
}

#[test]
fn a_failed_grant_on_a_channel_refusing_its_reset_keeps_both_buffers_and_withdraws_it() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    rig.prepare(PLAYER, channel).expect("prepares");
    rig.memory.borrow_mut().refuse_grants = true;
    rig.state(channel, |state| state.reset = Reset::Refused);
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::PermissionDenied));
    assert_eq!(rig.live(), 2);
    assert!(rig.records().contains(&Record::Unreset { channel }));
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::DeviceFault));
}

#[test]
fn a_prepared_stopped_channel_takes_a_new_buffer_in_place_of_its_old_one() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    let first = rig.prepare(PLAYER, channel).expect("prepares");
    let second = rig.prepare(PLAYER, channel).expect("prepares again");
    assert_ne!(first, second);
    assert_eq!(rig.live(), 1);
}

#[test]
fn start_needs_a_chain_and_refuses_a_running_channel() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    let start = DmaEngineRequest::Start { channel };
    assert_eq!(rig.done(PLAYER, &start), Err(Errno::NotFound));
    rig.prepare(PLAYER, channel).expect("prepares");
    assert_eq!(rig.done(PLAYER, &start), Ok(()));
    assert_eq!(rig.done(PLAYER, &start), Err(Errno::Busy));
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::Busy));
}

#[test]
fn position_reports_the_engines_offset_once_a_chain_exists() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    let position = DmaEngineRequest::Position { channel };
    assert_eq!(
        decode_position_reply(&rig.call(PLAYER, &position)),
        Err(Errno::NotFound)
    );
    rig.prepare(PLAYER, channel).expect("prepares");
    rig.state(channel, |state| state.position = Some(1500));
    assert_eq!(
        decode_position_reply(&rig.call(PLAYER, &position)),
        Ok(1500)
    );
}

#[test]
fn a_wait_on_a_stopped_channel_is_answered_at_once() {
    let mut rig = rig();
    let channel = rig.open(PLAYER, 10).expect("opens");
    rig.prepare(PLAYER, channel).expect("prepares");
    rig.memory.borrow_mut().now = 77;
    let report =
        decode_wait_reply(&rig.call(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 }))
            .expect("answered");
    assert_eq!(report.end, WaitEnd::Stopped);
    assert_eq!(report.position, 0);
    assert_eq!(report.serviced, Duration64::from_nanos(77));
}

#[test]
fn a_posted_wait_is_answered_at_the_first_boundary_past_it_with_its_service_time() {
    let mut rig = rig();
    let channel = rig.running();
    let ticket = rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    assert_eq!(rig.reply(ticket), None);
    rig.memory.borrow_mut().now = 500;
    rig.raise(channel, PERIOD + 8, Ok(DmaChannelEvent::Boundary));
    let report = decode_wait_reply(&rig.reply(ticket).expect("answered")).expect("report");
    assert_eq!(report.end, WaitEnd::Boundary);
    assert_eq!(report.position, u64::from(PERIOD));
    assert_eq!(report.serviced, Duration64::from_nanos(500));
}

#[test]
fn coalesced_interrupts_count_every_boundary_the_channel_passed() {
    let mut rig = rig();
    let channel = rig.running();
    rig.raise(channel, 3 * PERIOD + 4, Ok(DmaChannelEvent::Boundary));
    let report =
        decode_wait_reply(&rig.call(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 }))
            .expect("answered");
    assert_eq!(report.position, 3 * u64::from(PERIOD));
    // A further lap wraps the period count and keeps counting.
    rig.raise(
        channel,
        PERIOD * PERIODS + PERIOD,
        Ok(DmaChannelEvent::Boundary),
    );
    let report = decode_wait_reply(&rig.call(
        PLAYER,
        &DmaEngineRequest::Wait {
            channel,
            after: 3 * u64::from(PERIOD),
        },
    ))
    .expect("answered");
    assert_eq!(report.position, 5 * u64::from(PERIOD));
}

#[test]
fn a_boundary_raised_within_the_same_period_counts_nothing() {
    let mut rig = rig();
    let channel = rig.running();
    let ticket = rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    rig.raise(channel, 12, Ok(DmaChannelEvent::Boundary));
    assert_eq!(rig.reply(ticket), None);
}

#[test]
fn a_second_wait_beside_a_posted_one_is_busy() {
    let mut rig = rig();
    let channel = rig.running();
    rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    let second = rig.call(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    assert_eq!(decode_wait_reply(&second), Err(Errno::Busy));
}

#[test]
fn stop_answers_the_posted_wait_as_stopped() {
    let mut rig = rig();
    let channel = rig.running();
    let ticket = rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    assert_eq!(
        rig.done(PLAYER, &DmaEngineRequest::Stop { channel }),
        Ok(())
    );
    let report = decode_wait_reply(&rig.reply(ticket).expect("answered")).expect("report");
    assert_eq!(report.end, WaitEnd::Stopped);
    assert!(!rig.state(channel, |state| state.running));
}

#[test]
fn a_fault_stops_the_channel_and_reports_the_controllers_bits() {
    let mut rig = rig();
    let channel = rig.running();
    let ticket = rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    let bits = NonZeroU32::new(0x104).expect("nonzero");
    rig.raise(channel, 4, Ok(DmaChannelEvent::Faulted(bits)));
    let report = decode_wait_reply(&rig.reply(ticket).expect("answered")).expect("report");
    assert_eq!(report.end, WaitEnd::Faulted(bits));
    assert!(rig.records().contains(&Record::Faulted { channel, bits }));
    assert!(!rig.state(channel, |state| state.running));
}

#[test]
fn a_position_that_cannot_be_read_stops_the_channel_as_lost() {
    let mut rig = rig();
    let channel = rig.running();
    let ticket = rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    rig.state(channel, |state| {
        state.events.push_back(Ok(DmaChannelEvent::Boundary));
        state.position = None;
    });
    rig.controller.interrupt(1 << channel);
    let report = decode_wait_reply(&rig.reply(ticket).expect("answered")).expect("report");
    assert_eq!(report.end, WaitEnd::Stopped);
    assert!(rig.records().contains(&Record::Lost { channel }));
}

#[test]
fn a_latched_fault_on_an_idle_channel_is_cleared_by_a_reset() {
    let mut rig = rig();
    let before = rig.state(2, |state| state.resets);
    let bits = NonZeroU32::new(1).expect("nonzero");
    rig.raise(2, 0, Ok(DmaChannelEvent::Faulted(bits)));
    assert_eq!(rig.state(2, |state| state.resets), before + 1);
}

#[test]
fn a_wait_that_cannot_be_answered_stops_the_channel() {
    let mut rig = rig();
    let channel = rig.running();
    let ticket = rig.post(PLAYER, &DmaEngineRequest::Wait { channel, after: 0 });
    rig.kernel.retire(ticket);
    rig.raise(channel, PERIOD, Ok(DmaChannelEvent::Boundary));
    assert!(!rig.state(channel, |state| state.running));
}

#[test]
fn a_channel_that_refuses_its_reset_takes_only_stop_and_close_and_keeps_its_buffer() {
    let mut rig = rig();
    let channel = rig.running();
    rig.state(channel, |state| state.reset = Reset::Refused);
    assert_eq!(
        rig.done(PLAYER, &DmaEngineRequest::Stop { channel }),
        Err(Errno::DeviceFault)
    );
    assert_eq!(rig.controller.usable() & 1 << channel, 0);
    assert_eq!(rig.prepare(PLAYER, channel), Err(Errno::DeviceFault));
    assert_eq!(
        rig.done(PLAYER, &DmaEngineRequest::Start { channel }),
        Err(Errno::DeviceFault)
    );
    assert_eq!(
        rig.done(PLAYER, &DmaEngineRequest::Close { channel }),
        Ok(())
    );
    assert_eq!(rig.live(), 1);
    // A withdrawn channel's latched events are only quietened.
    rig.raise(channel, PERIOD, Ok(DmaChannelEvent::Boundary));
    assert!(rig.state(channel, |state| state.events.is_empty()));
}

#[test]
fn close_releases_the_buffer_and_forgets_the_owner() {
    let mut rig = rig();
    let channel = rig.running();
    assert_eq!(
        rig.done(PLAYER, &DmaEngineRequest::Close { channel }),
        Ok(())
    );
    assert_eq!(rig.live(), 0);
    assert!(rig.kernel.watched().is_empty());
    assert!(rig.state(channel, |state| state.chain.is_none()));
    assert_eq!(rig.open(OTHER, 10), Ok(channel));
}

#[test]
fn an_ended_owner_releases_every_channel_it_held() {
    let mut rig = rig();
    let first = rig.running();
    rig.kernel.grant(PLAYER, HwResource::request(&line(12)));
    let second = rig.open(PLAYER, 12).expect("opens");
    rig.controller.peer_exited(PLAYER);
    assert!(rig
        .records()
        .contains(&Record::Abandoned { channel: first }));
    assert!(rig
        .records()
        .contains(&Record::Abandoned { channel: second }));
    assert_eq!(rig.live(), 0);
    assert!(rig.kernel.watched().is_empty());
    assert!(!rig.state(first, |state| state.running));
}

#[test]
fn a_malformed_frame_or_a_call_no_longer_in_service_is_refused() {
    let mut rig = rig();
    let ticket = rig.post_frame(PLAYER, b"junk");
    assert!(rig.reply(ticket).is_some());
    assert!(matches!(
        rig.records().last(),
        Some(Record::Refused {
            op: None,
            caller: Some(PLAYER),
            ..
        })
    ));
    rig.controller.serve(999, b"junk");
    assert!(matches!(
        rig.records().last(),
        Some(Record::Refused {
            op: None,
            caller: None,
            reason: Errno::NotFound,
        })
    ));
}

#[test]
fn the_window_covering_the_registers_reaches_peripherals_and_another_memory() {
    let memory =
        HwResource::dma_translated(0x4000_0000, 0x4000_0000, 0xC000_0000, DmaCoherence::Snooped);
    let registers = PERIPHERALS + 0x7000;
    for order in [[peripheral_window(), memory], [memory, peripheral_window()]] {
        assert_eq!(
            split_windows(|| order.iter(), registers, 0x1000),
            (Some(peripheral_window()), Some(memory))
        );
    }
    let (peripheral_alone, memory_alone) = ([peripheral_window()], [memory]);
    assert_eq!(
        split_windows(|| peripheral_alone.iter(), registers, 0x1000),
        (Some(peripheral_window()), None)
    );
    assert_eq!(
        split_windows(|| memory_alone.iter(), registers, 0x1000),
        (None, Some(memory))
    );
}

/// Beside a peripheral aperture, a bus may map all memory one to one, a window
/// that spans the registers too: the aperture, the narrower, is how the
/// engines reach a FIFO, and the wide window still reaches memory. The shape
/// is a firmware tree's on-chip bus whose wide-address engines name
/// peripherals at `0x4_7c000000`.
#[test]
fn a_memory_window_spanning_the_registers_beside_an_aperture_still_reaches_memory() {
    let aperture = HwResource::dma_translated(
        PERIPHERALS - 0x0200_0000 + 0x0380_0000,
        0x0380_0000,
        0x4_7C00_0000,
        DmaCoherence::Snooped,
    );
    let one_to_one =
        HwResource::dma_translated(0x4_0000_0000, 0x4_0000_0000, 0, DmaCoherence::Snooped);
    let registers = PERIPHERALS + 0x7B00;
    for order in [[aperture, one_to_one], [one_to_one, aperture]] {
        assert_eq!(
            split_windows(|| order.iter(), registers, 0x400),
            (Some(aperture), Some(one_to_one))
        );
    }
    assert_eq!(
        aperture.dma_bus_address(FIFO, 4),
        Some(0x4_7C00_0000 + (FIFO - (PERIPHERALS - 0x0200_0000))),
        "a FIFO is reached through the aperture"
    );
    let alone = [one_to_one];
    assert_eq!(
        split_windows(|| alone.iter(), registers, 0x400),
        (Some(one_to_one), None),
        "a window alone names no separate way to memory, so the engines are refused"
    );
}
