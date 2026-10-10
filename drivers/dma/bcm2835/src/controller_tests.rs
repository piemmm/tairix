//! The endpoint driven over the real engine and the register-level model,
//! with a kernel that answers the grant questions by the kernel's own rule.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::dmaengine::{
    decode_done_reply, decode_open_reply, decode_position_reply, decode_prepare_reply,
    decode_wait_reply, CyclicParams, DmaBufferGrant, DmaChannel, DmaDirection, DmaEngine,
    DmaEngineOp, DmaEngineRequest, WaitEnd, WaitReport, DMA_CONTROLLER_ENDPOINTS,
    DMA_ENGINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::time::Duration64;
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::SupplierHost;

use crate::controller::{split_windows, Buffer, Controller, ControllerHost, Record};
use crate::engine::Bcm2835Dma;
use crate::model::{
    Model, ModelStore, Timeline, Trace, CONBLK_AD, CS, CS_ERROR, CS_RESET, DEBUG_READ_ERROR,
    LITE_MAX_BLOCK,
};

/// The legacy Pi 4 node's mask: every channel but the firmware's 1 and 3.
const MASK: u64 = 0x7F5;
const PCM: u64 = 0xFE20_3000;
const FIFO: u64 = PCM + 4;
const PERIOD: u32 = 1920;
const PERIODS: u32 = 4;
const INSTANCE: ProcId = ProcId::from_raw([0xD0; PROC_ID_LEN]);

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const PLAYER: ProcId = instance(1);
const RECORDER: ProcId = instance(2);

/// The window covering the controller's registers, the only one it reaches
/// peripherals through.
fn peripheral_window() -> HwResource {
    HwResource::dma_translated(
        0xFF80_0000,
        0x0380_0000,
        0x7C00_0000,
        tairix_abi::DmaCoherence::Snooped,
    )
}

fn endpoint_id() -> u64 {
    DMA_CONTROLLER_ENDPOINTS.endpoint(12)
}

fn line(dreq: u32, index: u8) -> LinkRequest {
    LinkRequest::new(endpoint_id(), index, &[dreq], b"tx").expect("valid")
}

fn params() -> CyclicParams {
    CyclicParams {
        fifo: FIFO,
        direction: DmaDirection::MemoryToDevice,
        period_bytes: PERIOD,
        periods: PERIODS,
    }
}

/// The bus address the kernel carves region `region` at.
fn carved_bus(region: u64) -> u64 {
    0xC000_0000 + region * 0x0010_0000
}

#[derive(Default)]
struct Kernel {
    callers: BTreeMap<u64, ProcId>,
    next_ticket: u64,
    holdings: Vec<(ProcId, HwResource)>,
    ended: Vec<ProcId>,
    watched: Vec<ProcId>,
    next_region: u64,
    live: Vec<Buffer>,
    granted: Vec<(u64, u64)>,
    refuse_grants: bool,
    refuse_carves: bool,
    replies: Vec<(u64, Vec<u8>)>,
    unanswerable: Vec<u64>,
    now: u64,
    records: Vec<Record>,
}

struct Host {
    kernel: Rc<RefCell<Kernel>>,
    timeline: Timeline,
}

impl SupplierHost for Host {
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
        self.kernel
            .borrow()
            .callers
            .get(&ticket)
            .copied()
            .ok_or(Errno::NotFound)
    }
    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno> {
        let kernel = self.kernel.borrow();
        let caller = kernel.callers.get(&ticket).ok_or(Errno::NotFound)?;
        Ok(kernel
            .holdings
            .iter()
            .any(|(holder, grant)| holder == caller && grant.covers(record)))
    }
    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
        let mut kernel = self.kernel.borrow_mut();
        if kernel.unanswerable.contains(&ticket) {
            return Err(Errno::NotFound);
        }
        kernel.replies.push((ticket, frame.to_vec()));
        Ok(())
    }
    fn watch(&mut self, peer: ProcId) -> Result<(), Errno> {
        let mut kernel = self.kernel.borrow_mut();
        if kernel.ended.contains(&peer) {
            return Err(Errno::NotFound);
        }
        if !kernel.watched.contains(&peer) {
            kernel.watched.push(peer);
        }
        Ok(())
    }
    fn unwatch(&mut self, peer: ProcId) {
        self.kernel
            .borrow_mut()
            .watched
            .retain(|watched| *watched != peer);
    }
}

impl ControllerHost for Host {
    fn carve(&mut self, bytes: u32) -> Result<Buffer, Errno> {
        let mut kernel = self.kernel.borrow_mut();
        if kernel.refuse_carves {
            return Err(Errno::OutOfMemory);
        }
        kernel.next_region += 1;
        let region = kernel.next_region;
        let buffer = Buffer {
            region,
            base: 0x10_0000_0000 + region * 0x10_0000,
            len: usize::try_from(bytes).expect("fits"),
            bus: carved_bus(region),
        };
        kernel.live.push(buffer);
        Ok(buffer)
    }

    fn release(&mut self, buffer: &Buffer) {
        self.kernel.borrow_mut().live.retain(|live| live != buffer);
        self.timeline
            .borrow_mut()
            .push(Trace::Released { bus: buffer.bus });
    }

    fn grant(&mut self, buffer: &Buffer, ticket: u64) -> Result<u64, Errno> {
        let mut kernel = self.kernel.borrow_mut();
        if kernel.refuse_grants {
            return Err(Errno::PermissionDenied);
        }
        kernel.granted.push((buffer.region, ticket));
        Ok(0x1000 + buffer.region)
    }

    fn now(&self) -> Duration64 {
        Duration64::from_nanos(self.kernel.borrow().now)
    }

    fn instance(&self) -> ProcId {
        INSTANCE
    }

    fn record(&mut self, record: Record) {
        self.kernel.borrow_mut().records.push(record);
    }
}

type Endpoint<'a> = Controller<Bcm2835Dma<'a, ModelStore>, Host>;

struct Rig {
    model: Model,
    store: ModelStore,
    kernel: Rc<RefCell<Kernel>>,
}

impl Rig {
    fn new() -> Self {
        let model = Model::pi4();
        let store = model.store();
        let kernel = Rc::new(RefCell::new(Kernel::default()));
        {
            let mut kernel = kernel.borrow_mut();
            for (holder, dreq) in [(PLAYER, 2), (RECORDER, 3)] {
                kernel
                    .holdings
                    .push((holder, HwResource::request(&line(dreq, 0))));
                kernel.holdings.push((holder, HwResource::mmio(PCM, 0x24)));
            }
        }
        Self {
            model,
            store,
            kernel,
        }
    }

    fn endpoint(&self, usable: u64) -> Endpoint<'_> {
        self.owning(usable, usable).expect("every channel resets")
    }

    /// The endpoint resetting the channels in `owned` and serving those in
    /// `usable`; `None` when a reset could not be issued.
    fn owning(&self, owned: u64, usable: u64) -> Option<Endpoint<'_>> {
        let engine = Bcm2835Dma::new(&self.model, &self.store).expect("whole channels");
        Controller::new(
            engine,
            self.host(),
            endpoint_id(),
            (owned, usable),
            Some(peripheral_window()),
        )
    }

    fn host(&self) -> Host {
        Host {
            kernel: Rc::clone(&self.kernel),
            timeline: self.model.timeline(),
        }
    }

    /// Post `request` from `caller`, answering its ticket and the reply it
    /// was sent, if it was answered yet.
    fn call(
        &self,
        endpoint: &mut Endpoint<'_>,
        caller: ProcId,
        request: &DmaEngineRequest,
    ) -> (u64, Option<Vec<u8>>) {
        let mut frame = [0u8; DMA_ENGINE_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("encodes");
        self.send(endpoint, caller, &frame[..len])
    }

    fn send(
        &self,
        endpoint: &mut Endpoint<'_>,
        caller: ProcId,
        frame: &[u8],
    ) -> (u64, Option<Vec<u8>>) {
        let ticket = {
            let mut kernel = self.kernel.borrow_mut();
            kernel.next_ticket += 1;
            let ticket = kernel.next_ticket;
            kernel.callers.insert(ticket, caller);
            ticket
        };
        endpoint.serve(ticket, frame);
        (ticket, self.reply(ticket))
    }

    fn reply(&self, ticket: u64) -> Option<Vec<u8>> {
        self.kernel
            .borrow()
            .replies
            .iter()
            .rev()
            .find(|(answered, _)| *answered == ticket)
            .map(|(_, frame)| frame.clone())
    }

    fn open(&self, endpoint: &mut Endpoint<'_>, caller: ProcId, dreq: u32) -> Result<u8, Errno> {
        let (_, reply) = self.call(endpoint, caller, &DmaEngineRequest::Open(line(dreq, 0)));
        decode_open_reply(&reply.expect("answered"))
    }

    fn prepare(
        &self,
        endpoint: &mut Endpoint<'_>,
        caller: ProcId,
        channel: u8,
        params: CyclicParams,
    ) -> Result<DmaBufferGrant, Errno> {
        let (_, reply) = self.call(
            endpoint,
            caller,
            &DmaEngineRequest::Prepare { channel, params },
        );
        decode_prepare_reply(&reply.expect("answered"))
    }

    fn done(
        &self,
        endpoint: &mut Endpoint<'_>,
        caller: ProcId,
        request: &DmaEngineRequest,
    ) -> Result<(), Errno> {
        let (_, reply) = self.call(endpoint, caller, request);
        decode_done_reply(&reply.expect("answered"), request.op())
    }

    /// Open, prepare and start a channel for the player, with the model
    /// holding it to the buffer the kernel carved.
    fn running(&self, endpoint: &mut Endpoint<'_>) -> u8 {
        let channel = self.open(endpoint, PLAYER, 2).expect("opens");
        self.prepare(endpoint, PLAYER, channel, params())
            .expect("prepares");
        let bus = self.kernel.borrow().live.last().expect("carved").bus;
        self.model
            .own(usize::from(channel), bus, u64::from(PERIOD * PERIODS));
        self.done(endpoint, PLAYER, &DmaEngineRequest::Start { channel })
            .expect("starts");
        channel
    }

    fn records(&self) -> Vec<Record> {
        self.kernel.borrow().records.clone()
    }

    fn tick(&self, nanos: u64) {
        self.kernel.borrow_mut().now += nanos;
    }
}

#[test]
fn bring_up_resets_every_usable_channel_and_touches_no_other() {
    let rig = Rig::new();
    let endpoint = rig.endpoint(MASK);
    assert_eq!(endpoint.usable(), MASK);
    for channel in 0..11 {
        if MASK & 1 << channel == 0 {
            assert!(
                !rig.model.touched(channel),
                "channel {channel} is the firmware's"
            );
        } else {
            assert_eq!(rig.model.writes(channel, CS).last(), Some(&CS_RESET));
        }
    }
    // A mask naming channels the window does not hold serves only the ones
    // it does.
    assert_eq!(rig.endpoint(u64::MAX).usable(), (1 << 11) - 1);
}

#[test]
fn a_chain_an_earlier_instance_left_running_is_stopped_at_bring_up() {
    let rig = Rig::new();
    {
        let mut earlier = Bcm2835Dma::new(&rig.model, &rig.store).expect("whole channels");
        let channel = earlier.channel(0).expect("channel 0");
        let transfer = tairix_abi::driver::dmaengine::CyclicTransfer {
            buffer: 0xC100_0000,
            fifo: 0x7E20_3004,
            direction: DmaDirection::MemoryToDevice,
            period_bytes: PERIOD,
            periods: PERIODS,
        };
        channel.prepare(&line(2, 0), &transfer).expect("prepares");
        channel.start().expect("starts");
        // The instance ends without stopping it; its chain must outlive it.
        core::mem::forget(earlier);
    }
    rig.model.advance(0, 700);
    assert!(rig.model.active(0));
    rig.model.drain_reads(0, u32::MAX);
    let _endpoint = rig.endpoint(MASK);
    assert!(!rig.model.active(0));
    assert_eq!(rig.model.writes(0, CS).last(), Some(&CS_RESET));
    assert_eq!(rig.records(), [Record::Undrained { channel: 0 }]);
}

/// A channel the node owns whose line did not bind this time is reset all
/// the same, though not served: a chain an earlier instance left on it
/// would otherwise run on into memory the quarantine then gives back.
#[test]
fn an_owned_channel_is_reset_at_bring_up_though_it_is_not_served() {
    let rig = Rig::new();
    {
        let mut earlier = Bcm2835Dma::new(&rig.model, &rig.store).expect("whole channels");
        let channel = earlier.channel(4).expect("channel 4");
        let transfer = tairix_abi::driver::dmaengine::CyclicTransfer {
            buffer: 0xC100_0000,
            fifo: 0x7E20_3004,
            direction: DmaDirection::MemoryToDevice,
            period_bytes: PERIOD,
            periods: PERIODS,
        };
        channel.prepare(&line(2, 0), &transfer).expect("prepares");
        channel.start().expect("starts");
        core::mem::forget(earlier);
    }
    rig.model.advance(4, 700);
    assert!(rig.model.active(4));
    let unbound = MASK & !(1 << 4);
    let endpoint = rig.owning(MASK, unbound).expect("every channel resets");
    assert!(!rig.model.active(4), "stopped before the quarantine lifts");
    assert_eq!(rig.model.writes(4, CS).last(), Some(&CS_RESET));
    assert_eq!(endpoint.usable(), unbound, "and still not served");
}

/// A reset that cannot be issued leaves the node's memory quarantined: the
/// endpoint is refused, rather than serving beside a channel still running.
#[test]
fn a_channel_that_will_not_reset_keeps_the_memory_quarantined() {
    let rig = Rig::new();
    let refusing = crate::model::Unresettable {
        model: &rig.model,
        armed: core::cell::Cell::new(true),
        ignored: false,
    };
    let engine = Bcm2835Dma::new(&refusing, &rig.store).expect("whole channels");
    assert!(Controller::new(
        engine,
        rig.host(),
        endpoint_id(),
        (MASK, MASK),
        Some(peripheral_window())
    )
    .is_none());
    assert!(matches!(
        rig.records().first(),
        Some(Record::Unreset { .. })
    ));
}

#[test]
fn a_channel_opens_only_for_a_line_the_caller_holds() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    // The recorder holds DREQ 3, not the player's DREQ 2.
    assert_eq!(
        rig.open(&mut endpoint, RECORDER, 2),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        rig.records(),
        [Record::Refused {
            op: Some(DmaEngineOp::Open),
            caller: Some(RECORDER),
            reason: Errno::PermissionDenied,
        }]
    );
    assert_eq!(rig.open(&mut endpoint, PLAYER, 2), Ok(0));
    assert_eq!(rig.open(&mut endpoint, RECORDER, 3), Ok(2));
    assert!(rig.kernel.borrow().watched.contains(&PLAYER));
}

#[test]
fn a_line_naming_another_controller_or_an_undefined_serving_is_refused() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let elsewhere =
        LinkRequest::new(DMA_CONTROLLER_ENDPOINTS.endpoint(13), 0, &[2], b"tx").expect("valid");
    rig.kernel
        .borrow_mut()
        .holdings
        .push((PLAYER, HwResource::request(&elsewhere)));
    let (_, reply) = rig.call(&mut endpoint, PLAYER, &DmaEngineRequest::Open(elsewhere));
    assert_eq!(
        decode_open_reply(&reply.expect("answered")),
        Err(Errno::OutOfRange)
    );

    let undefined = line(2 | 1 << 26, 1);
    rig.kernel
        .borrow_mut()
        .holdings
        .push((PLAYER, HwResource::request(&undefined)));
    let (_, reply) = rig.call(&mut endpoint, PLAYER, &DmaEngineRequest::Open(undefined));
    assert_eq!(
        decode_open_reply(&reply.expect("answered")),
        Err(Errno::NotSupported)
    );
}

#[test]
fn the_lowest_free_usable_channel_is_claimed_until_none_is_left() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(0b101);
    assert_eq!(rig.open(&mut endpoint, PLAYER, 2), Ok(0));
    assert_eq!(rig.open(&mut endpoint, RECORDER, 3), Ok(2));
    let third = line(4, 0);
    rig.kernel
        .borrow_mut()
        .holdings
        .push((PLAYER, HwResource::request(&third)));
    let (_, reply) = rig.call(&mut endpoint, PLAYER, &DmaEngineRequest::Open(third));
    assert_eq!(
        decode_open_reply(&reply.expect("answered")),
        Err(Errno::Busy)
    );
    // Its holder opening a line a second time is refused rather than doubled.
    assert_eq!(
        rig.open(&mut endpoint, PLAYER, 2),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn a_line_stays_with_a_holder_that_lives_and_is_reclaimed_from_one_that_has_ended() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    let successor = instance(9);
    rig.kernel
        .borrow_mut()
        .holdings
        .push((successor, HwResource::request(&line(2, 0))));
    // Two nodes may carry one line; while its holder lives it stays theirs.
    assert_eq!(rig.open(&mut endpoint, successor, 2), Err(Errno::Busy));
    assert!(rig.model.active(usize::from(channel)));

    rig.kernel.borrow_mut().ended.push(PLAYER);
    assert_eq!(rig.open(&mut endpoint, successor, 2), Ok(channel));
    assert!(!rig.model.active(usize::from(channel)));
    assert!(
        rig.kernel.borrow().live.is_empty(),
        "the old buffer was released"
    );
    assert!(rig.records().contains(&Record::Reclaimed {
        channel,
        from: PLAYER
    }));
}

#[test]
fn every_later_call_must_come_from_the_instance_that_opened_the_channel() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    for request in [
        DmaEngineRequest::Prepare {
            channel,
            params: params(),
        },
        DmaEngineRequest::Start { channel },
        DmaEngineRequest::Stop { channel },
        DmaEngineRequest::Position { channel },
        DmaEngineRequest::Close { channel },
        DmaEngineRequest::Wait { channel, after: 0 },
    ] {
        let (_, reply) = rig.call(&mut endpoint, RECORDER, &request);
        assert_eq!(
            decode_position_reply(&reply.expect("answered")),
            Err(Errno::PermissionDenied),
            "{request:?}"
        );
    }
    // A channel nobody opened is nobody's.
    assert_eq!(
        rig.done(
            &mut endpoint,
            PLAYER,
            &DmaEngineRequest::Start { channel: 5 }
        ),
        Err(Errno::PermissionDenied)
    );
    assert!(rig.model.active(usize::from(channel)));
}

#[test]
fn prepare_carves_the_buffer_and_grants_it_to_the_caller_alone() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    let (ticket, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Prepare {
            channel,
            params: params(),
        },
    );
    let buffer = decode_prepare_reply(&reply.expect("answered")).expect("prepared");
    assert_eq!(buffer.grantor, INSTANCE);
    assert_eq!(buffer.grant, 0x1001);
    assert_eq!(rig.kernel.borrow().granted, [(1, ticket)]);
    assert_eq!(
        rig.kernel.borrow().live.first().map(|live| live.len),
        Some(usize::try_from(PERIOD * PERIODS).expect("fits"))
    );
    rig.model.own(
        usize::from(channel),
        carved_bus(1),
        u64::from(PERIOD * PERIODS),
    );
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel })
        .expect("starts");
    rig.model.advance(usize::from(channel), PERIOD);
    // The chain reaches the carved buffer through the memory window and the
    // FIFO through the one covering the controller's registers.
    let block = rig.model.loaded(usize::from(channel))[0];
    assert_eq!(u64::from(block.source), carved_bus(1));
    assert_eq!(block.dest, 0x7E20_3004);
}

#[test]
fn a_fifo_outside_the_callers_windows_is_refused_before_anything_is_carved() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    for fifo in [PCM + 0x24, 0xFE20_4004, PCM + 0x22] {
        let refused = rig.prepare(
            &mut endpoint,
            PLAYER,
            channel,
            CyclicParams { fifo, ..params() },
        );
        assert!(
            matches!(refused, Err(Errno::PermissionDenied | Errno::OutOfRange)),
            "{fifo:#x}: {refused:?}"
        );
    }
    assert_eq!(rig.kernel.borrow().next_region, 0);
    assert_eq!(rig.model.carved(), 0);
}

#[test]
fn a_fifo_the_controller_cannot_reach_is_refused() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    // The player holds a window the controller's windows do not reach.
    let unreachable = 0x6_0000_0000;
    rig.kernel
        .borrow_mut()
        .holdings
        .push((PLAYER, HwResource::mmio(unreachable, 0x1000)));
    assert_eq!(
        rig.prepare(
            &mut endpoint,
            PLAYER,
            channel,
            CyclicParams {
                fifo: unreachable,
                ..params()
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(rig.kernel.borrow().next_region, 0);
}

/// A region the caller holds inside the node's memory window, the first GiB,
/// is memory rather than a FIFO: translating it would point the engine at RAM.
#[test]
fn a_fifo_in_memory_is_refused() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    let in_memory = 0x3000_0000;
    rig.kernel
        .borrow_mut()
        .holdings
        .push((PLAYER, HwResource::mmio(in_memory, 0x1000)));
    assert_eq!(
        rig.prepare(
            &mut endpoint,
            PLAYER,
            channel,
            CyclicParams {
                fifo: in_memory,
                ..params()
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(rig.model.carved(), 0);
}

#[test]
fn a_failed_grant_leaves_neither_a_buffer_nor_a_chain() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    rig.prepare(&mut endpoint, PLAYER, channel, params())
        .expect("prepares");
    rig.kernel.borrow_mut().refuse_grants = true;
    assert_eq!(
        rig.prepare(&mut endpoint, PLAYER, channel, params()),
        Err(Errno::PermissionDenied)
    );
    assert!(rig.kernel.borrow().live.is_empty());
    assert_eq!(rig.model.live_tables(), 0);
    assert_eq!(
        rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel }),
        Err(Errno::NotFound)
    );
    rig.kernel.borrow_mut().refuse_carves = true;
    assert_eq!(
        rig.prepare(&mut endpoint, PLAYER, channel, params()),
        Err(Errno::OutOfMemory)
    );
}

/// The endpoint over `regs`, serving every channel of the mask.
fn endpoint_over<'a>(rig: &'a Rig, regs: &'a dyn tairix_abi::RegisterBlock) -> Endpoint<'a> {
    let engine = Bcm2835Dma::new(regs, &rig.store).expect("whole channels");
    Controller::new(
        engine,
        rig.host(),
        endpoint_id(),
        (MASK, MASK),
        Some(peripheral_window()),
    )
    .expect("every channel resets")
}

/// A channel that refuses its reset may still run a chain nothing can stop:
/// it is withdrawn, takes no further work, and what it reached stays held.
#[test]
fn a_channel_that_refuses_its_reset_takes_only_stop_and_close() {
    let rig = Rig::new();
    let refusing = crate::model::Unresettable {
        model: &rig.model,
        armed: core::cell::Cell::new(false),
        ignored: false,
    };
    let mut endpoint = endpoint_over(&rig, &refusing);
    let channel = rig.running(&mut endpoint);
    refusing.armed.set(true);
    assert_eq!(
        rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Stop { channel }),
        Err(Errno::DeviceFault),
        "a channel that would not stop is never reported stopped"
    );
    assert!(rig.records().contains(&Record::Unreset { channel }));
    assert_eq!(endpoint.usable() & (1 << channel), 0, "withdrawn");
    // Nothing stopped the chain: it runs on to its next boundary.
    rig.model.set_active(usize::from(channel), true);
    rig.model.advance(usize::from(channel), u32::MAX);
    assert!(rig.model.pending(usize::from(channel)), "its chain ran on");
    endpoint.interrupt(1 << channel);
    assert!(
        !rig.model.pending(usize::from(channel)),
        "quietened, so its line is not held up"
    );
    assert_eq!(
        rig.prepare(&mut endpoint, PLAYER, channel, params()),
        Err(Errno::DeviceFault)
    );
    assert_eq!(
        rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel }),
        Err(Errno::DeviceFault)
    );
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Position { channel },
    );
    assert_eq!(
        decode_position_reply(&reply.expect("answered")),
        Err(Errno::DeviceFault)
    );
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert_eq!(
        decode_wait_reply(&reply.expect("answered")).map(|report| report.end),
        Err(Errno::DeviceFault)
    );
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Close { channel })
        .expect("closes");
    assert_eq!(rig.kernel.borrow().live.len(), 1, "its buffer stays held");
    assert_eq!(rig.model.live_tables(), 1, "and so does its chain");
}

/// A channel that takes its reset without effect is withdrawn as one that
/// refuses it is, is not reset again as it closes, and keeps all it reached.
#[test]
fn a_channel_that_ignores_its_reset_is_withdrawn_and_keeps_what_it_reached() {
    let rig = Rig::new();
    let ignoring = crate::model::Unresettable {
        model: &rig.model,
        armed: core::cell::Cell::new(false),
        ignored: true,
    };
    let mut endpoint = endpoint_over(&rig, &ignoring);
    let channel = rig.running(&mut endpoint);
    ignoring.armed.set(true);
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Close { channel })
        .expect("closes");
    let unreset = rig
        .records()
        .iter()
        .filter(|record| **record == Record::Unreset { channel })
        .count();
    assert_eq!(unreset, 1, "one reset tried, not a second on release");
    assert_eq!(endpoint.usable() & (1 << channel), 0, "withdrawn");
    assert_eq!(rig.kernel.borrow().live.len(), 1, "its buffer stays held");
    assert_eq!(rig.model.live_tables(), 1, "and so does its chain");
}

#[test]
fn the_window_covering_the_registers_is_the_peripherals_and_another_the_memory() {
    let memory = HwResource::dma_translated(
        0x0000_0000,
        0x4000_0000,
        0xC000_0000,
        tairix_abi::DmaCoherence::Snooped,
    );
    let registers = 0xFE00_7000;
    for order in [[peripheral_window(), memory], [memory, peripheral_window()]] {
        assert_eq!(
            split_windows(&order, registers, 0x1000),
            (Some(peripheral_window()), Some(memory))
        );
    }
    assert_eq!(
        split_windows(&[peripheral_window()], registers, 0x1000),
        (Some(peripheral_window()), None)
    );
    assert_eq!(
        split_windows(&[memory], registers, 0x1000),
        (None, Some(memory))
    );
}

/// A grant refused on a channel that then refuses its reset frees neither
/// buffer, and the shape of the chain it replaced is forgotten.
#[test]
fn a_failed_grant_on_a_channel_that_refuses_its_reset_keeps_both_buffers() {
    let rig = Rig::new();
    let refusing = crate::model::Unresettable {
        model: &rig.model,
        armed: core::cell::Cell::new(false),
        ignored: false,
    };
    let mut endpoint = endpoint_over(&rig, &refusing);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    rig.prepare(&mut endpoint, PLAYER, channel, params())
        .expect("prepares");
    rig.kernel.borrow_mut().refuse_grants = true;
    refusing.armed.set(true);
    assert_eq!(
        rig.prepare(&mut endpoint, PLAYER, channel, params()),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(rig.kernel.borrow().live.len(), 2, "neither buffer is freed");
    assert_eq!(rig.model.live_tables(), 1);
    assert_eq!(
        rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel }),
        Err(Errno::DeviceFault)
    );
}

#[test]
fn a_posted_wait_is_answered_at_the_boundary_with_its_position_and_service_time() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    let (ticket, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert!(reply.is_none(), "held until a boundary passes");
    rig.model.advance(usize::from(channel), PERIOD - 1);
    endpoint.interrupt(1 << channel);
    assert!(rig.reply(ticket).is_none(), "no boundary yet");
    rig.tick(5_000_000);
    rig.model.advance(usize::from(channel), 1);
    endpoint.interrupt(1 << channel);
    assert_eq!(
        decode_wait_reply(&rig.reply(ticket).expect("answered")),
        Ok(WaitReport {
            end: WaitEnd::Boundary,
            position: u64::from(PERIOD),
            serviced: Duration64::from_nanos(5_000_000),
        })
    );
}

#[test]
fn a_boundary_passed_while_no_wait_was_posted_is_answered_at_once() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    rig.tick(7);
    rig.model.advance(usize::from(channel), PERIOD);
    endpoint.interrupt(1 << channel);
    rig.tick(1_000);
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert_eq!(
        decode_wait_reply(&reply.expect("answered at once")),
        Ok(WaitReport {
            end: WaitEnd::Boundary,
            position: u64::from(PERIOD),
            serviced: Duration64::from_nanos(7),
        })
    );
    // Waiting past what has been seen holds until the next one.
    let (ticket, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait {
            channel,
            after: u64::from(PERIOD),
        },
    );
    assert!(reply.is_none());
    let (_, second) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert_eq!(
        decode_wait_reply(&second.expect("answered")),
        Err(Errno::Busy)
    );
    rig.model.advance(usize::from(channel), PERIOD);
    endpoint.interrupt(1 << channel);
    assert_eq!(
        decode_wait_reply(&rig.reply(ticket).expect("answered")).map(|report| report.position),
        Ok(u64::from(2 * PERIOD))
    );
}

#[test]
fn coalesced_interrupts_count_every_boundary_the_channel_passed() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    // Three periods pass before the one interrupt is serviced, then two more
    // wrap past the buffer's end.
    rig.model.advance(usize::from(channel), 3 * PERIOD + 100);
    endpoint.interrupt(1 << channel);
    rig.model.advance(usize::from(channel), 2 * PERIOD);
    endpoint.interrupt(1 << channel);
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert_eq!(
        decode_wait_reply(&reply.expect("answered")).map(|report| report.position),
        Ok(u64::from(5 * PERIOD))
    );
}

#[test]
fn an_interrupt_on_another_channels_line_counts_nothing() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    rig.model.advance(usize::from(channel), PERIOD);
    endpoint.interrupt(1 << 2);
    assert!(
        rig.model.pending(usize::from(channel)),
        "not this line's channel"
    );
    endpoint.interrupt(1 << channel);
    assert!(!rig.model.pending(usize::from(channel)));
}

#[test]
fn stop_answers_the_posted_wait_as_stopped() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    rig.model.advance(usize::from(channel), PERIOD);
    endpoint.interrupt(1 << channel);
    let (ticket, _) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait {
            channel,
            after: u64::from(PERIOD),
        },
    );
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Stop { channel })
        .expect("stops");
    assert_eq!(
        decode_wait_reply(&rig.reply(ticket).expect("answered"))
            .map(|report| (report.end, report.position)),
        Ok((WaitEnd::Stopped, u64::from(PERIOD)))
    );
    assert!(!rig.model.active(usize::from(channel)));
    // A wait on a stopped channel is answered as such at once.
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert_eq!(
        decode_wait_reply(&reply.expect("answered")).map(|report| report.end),
        Ok(WaitEnd::Stopped)
    );
    // Stopping twice is not an error; restarting counts from zero.
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Stop { channel })
        .expect("stops again");
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel })
        .expect("restarts");
    rig.model.advance(usize::from(channel), PERIOD);
    endpoint.interrupt(1 << channel);
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    assert_eq!(
        decode_wait_reply(&reply.expect("answered")).map(|report| report.position),
        Ok(u64::from(PERIOD))
    );
}

#[test]
fn a_fault_stops_the_channel_and_reports_the_controllers_bits() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    let (ticket, _) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    rig.model.fault(usize::from(channel), DEBUG_READ_ERROR);
    endpoint.interrupt(1 << channel);
    let bits = core::num::NonZeroU32::new(CS_ERROR | DEBUG_READ_ERROR).expect("non-zero");
    assert_eq!(
        decode_wait_reply(&rig.reply(ticket).expect("answered")).map(|report| report.end),
        Ok(WaitEnd::Faulted(bits))
    );
    assert!(rig.records().contains(&Record::Faulted { channel, bits }));
    assert_eq!(
        rig.model.writes(usize::from(channel), CS).last(),
        Some(&CS_RESET)
    );
}

#[test]
fn a_wait_that_cannot_be_answered_stops_the_channel() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    let (ticket, _) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Wait { channel, after: 0 },
    );
    rig.kernel.borrow_mut().unanswerable.push(ticket);
    rig.model.advance(usize::from(channel), PERIOD);
    endpoint.interrupt(1 << channel);
    assert!(!rig.model.active(usize::from(channel)));
}

#[test]
fn an_ended_owner_releases_every_channel_it_held_after_resetting_it() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    rig.model.advance(usize::from(channel), 700);
    endpoint.peer_exited(PLAYER);
    assert!(!rig.model.active(usize::from(channel)));
    assert!(rig.kernel.borrow().live.is_empty());
    assert_eq!(rig.model.live_tables(), 0);
    assert!(rig.records().contains(&Record::Abandoned { channel }));
    let timeline = rig.model.timeline();
    let timeline = timeline.borrow();
    let reset = timeline
        .iter()
        .rposition(|trace| {
            *trace
                == Trace::Write {
                    channel: usize::from(channel),
                    register: CS,
                    value: CS_RESET,
                }
        })
        .expect("the channel was reset");
    let released = timeline
        .iter()
        .position(|trace| matches!(trace, Trace::Released { .. }))
        .expect("the buffer was released");
    assert!(reset < released, "the device stops before the buffer goes");
    drop(timeline);
    // The channel is free for the next holder.
    assert_eq!(rig.open(&mut endpoint, RECORDER, 3), Ok(channel));
}

#[test]
fn close_releases_the_chain_and_the_buffer_and_forgets_the_owner() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Close { channel })
        .expect("closes");
    assert!(rig.kernel.borrow().live.is_empty());
    assert_eq!(rig.model.live_tables(), 0);
    assert_eq!(rig.model.writes(usize::from(channel), CONBLK_AD).len(), 1);
    assert!(!rig.kernel.borrow().watched.contains(&PLAYER));
    assert_eq!(
        rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel }),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn prepare_on_a_stopped_channel_replaces_its_buffer() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.running(&mut endpoint);
    assert_eq!(
        rig.prepare(&mut endpoint, PLAYER, channel, params()),
        Err(Errno::Busy)
    );
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Stop { channel })
        .expect("stops");
    let replaced = rig
        .prepare(
            &mut endpoint,
            PLAYER,
            channel,
            CyclicParams {
                period_bytes: 960,
                ..params()
            },
        )
        .expect("prepares");
    assert_eq!(replaced.grant, 0x1002);
    let live: Vec<u64> = rig
        .kernel
        .borrow()
        .live
        .iter()
        .map(|live| live.region)
        .collect();
    assert_eq!(live, [2]);
    assert_eq!(rig.model.live_tables(), 1);
}

#[test]
fn position_reports_the_live_offset() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let channel = rig.open(&mut endpoint, PLAYER, 2).expect("opens");
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Position { channel },
    );
    assert_eq!(
        decode_position_reply(&reply.expect("answered")),
        Err(Errno::NotFound)
    );
    rig.prepare(&mut endpoint, PLAYER, channel, params())
        .expect("prepares");
    rig.model.own(
        usize::from(channel),
        carved_bus(1),
        u64::from(PERIOD * PERIODS),
    );
    rig.done(&mut endpoint, PLAYER, &DmaEngineRequest::Start { channel })
        .expect("starts");
    rig.model.advance(usize::from(channel), 2 * PERIOD + 12);
    let (_, reply) = rig.call(
        &mut endpoint,
        PLAYER,
        &DmaEngineRequest::Position { channel },
    );
    assert_eq!(
        decode_position_reply(&reply.expect("answered")),
        Ok(u64::from(2 * PERIOD + 12))
    );
}

#[test]
fn a_malformed_frame_is_refused_and_recorded() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    let (_, reply) = rig.send(&mut endpoint, PLAYER, &[0u8; 5]);
    assert_eq!(
        decode_done_reply(&reply.expect("answered"), DmaEngineOp::Start),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(
        rig.records(),
        [Record::Refused {
            op: None,
            caller: Some(PLAYER),
            reason: Errno::BufferTooSmall,
        }]
    );
}

#[test]
fn a_call_no_longer_in_service_is_refused_without_a_caller() {
    let rig = Rig::new();
    let mut endpoint = rig.endpoint(MASK);
    endpoint.serve(999, &[0u8; 5]);
    assert_eq!(
        rig.records(),
        [Record::Refused {
            op: None,
            caller: None,
            reason: Errno::NotFound,
        }]
    );
}

/// Steps one walk takes.
const WALK_STEPS: usize = 6_000;

/// Mostly shapes the engine admits, with every kind of refusal mixed in.
const WALK_PERIODS: [u32; 7] = [2, 4, 4, 128, 0, 1, 129];
const WALK_PERIOD_BYTES: [u32; 7] = [4, PERIOD, PERIOD, LITE_MAX_BLOCK * 3, 0, 6, u32::MAX];
const WALK_FIFOS: [u64; 6] = [FIFO, FIFO, FIFO, PCM + 0x24, PCM + 2, 0xFE20_4004];

/// A random walk over the endpoint: two holders and a stranger calling,
/// the channels running and faulting, holders ending and being succeeded.
struct Walk<'r> {
    rig: &'r Rig,
    endpoint: Endpoint<'r>,
    rng: tairix_fuzzseed::Prng,
    /// Each holder's live instance and its request line's DREQ.
    holders: [(ProcId, u32); 2],
    successors: u64,
    /// Channels opened so far, which most steps aim at so the walk reaches
    /// prepared, running and interrupted channels rather than refusals.
    opened: Vec<u8>,
    /// Waits answered by an interrupt rather than a call.
    served_boundaries: usize,
}

impl<'r> Walk<'r> {
    fn new(rig: &'r Rig, seed: u64) -> Self {
        Self {
            rig,
            endpoint: rig.endpoint(MASK),
            rng: tairix_fuzzseed::Prng::new(seed),
            holders: [(PLAYER, 2), (RECORDER, 3)],
            successors: 0,
            opened: Vec::new(),
            served_boundaries: 0,
        }
    }

    fn step(&mut self) {
        let (holder, dreq) = *self.rng.pick(&self.holders);
        let caller = if self.rng.below(16) == 0 {
            instance(0xEE)
        } else {
            holder
        };
        let channel = if self.opened.is_empty() || self.rng.below(8) == 0 {
            u8::try_from(self.rng.below(12)).expect("small")
        } else {
            *self.rng.pick(&self.opened)
        };
        let rig = self.rig;
        let endpoint = &mut self.endpoint;
        match self.rng.below(100) {
            0..10 => {
                if let Ok(channel) = rig.open(endpoint, caller, dreq) {
                    self.opened.push(channel);
                }
            }
            10..25 => self.prepare(caller, channel),
            25..37 => {
                let _ = rig.done(endpoint, caller, &DmaEngineRequest::Start { channel });
            }
            37..40 => {
                let _ = rig.done(endpoint, caller, &DmaEngineRequest::Stop { channel });
            }
            40..44 => {
                let _ = rig.call(endpoint, caller, &DmaEngineRequest::Position { channel });
            }
            44..46 => {
                let _ = rig.done(endpoint, caller, &DmaEngineRequest::Close { channel });
            }
            46..60 => {
                let after = self.rng.next_u64() % 4_000;
                let _ = rig.call(endpoint, caller, &DmaEngineRequest::Wait { channel, after });
            }
            60..80 => {
                let bytes = u32::try_from(self.rng.below(2 * PERIOD as usize)).expect("small");
                rig.model.advance(usize::from(channel).min(10), bytes);
            }
            80..97 => self.interrupt(),
            97..99 => rig
                .model
                .fault(usize::from(channel).min(10), DEBUG_READ_ERROR),
            _ => self.succeed(),
        }
        // One buffer and one chain at most per served channel, whatever the
        // interleaving.
        let served = MASK.count_ones() as usize;
        assert!(rig.kernel.borrow().live.len() <= served);
        assert!(rig.model.live_tables() <= served);
    }

    fn prepare(&mut self, caller: ProcId, channel: u8) {
        let params = CyclicParams {
            fifo: *self.rng.pick(&WALK_FIFOS),
            direction: if self.rng.below(2) == 0 {
                DmaDirection::MemoryToDevice
            } else {
                DmaDirection::DeviceToMemory
            },
            period_bytes: *self.rng.pick(&WALK_PERIOD_BYTES),
            periods: *self.rng.pick(&WALK_PERIODS),
        };
        let before = self.rig.kernel.borrow().next_region;
        if self
            .rig
            .prepare(&mut self.endpoint, caller, channel, params)
            .is_ok()
        {
            let region = self.rig.kernel.borrow().next_region;
            assert_eq!(region, before + 1, "one carve per prepare");
            let bytes = params.buffer_bytes().expect("admitted");
            self.rig
                .model
                .own(usize::from(channel), carved_bus(region), u64::from(bytes));
        }
    }

    fn interrupt(&mut self) {
        let fired = if self.rng.below(2) == 0 {
            MASK
        } else {
            self.rng.next_u64() & MASK
        };
        let answered = self.rig.kernel.borrow().replies.len();
        self.endpoint.interrupt(fired);
        self.served_boundaries += self.rig.kernel.borrow().replies.len() - answered;
    }

    /// A holder ends, and a successor holding the same grants takes its place,
    /// as a restarted driver would.
    fn succeed(&mut self) {
        let slot = self.rng.below(self.holders.len());
        let (ended, dreq) = self.holders[slot];
        self.end(ended);
        self.successors += 1;
        let mut raw = [0x5A; PROC_ID_LEN];
        raw[..8].copy_from_slice(&self.successors.to_le_bytes());
        let successor = ProcId::from_raw(raw);
        let mut kernel = self.rig.kernel.borrow_mut();
        kernel
            .holdings
            .push((successor, HwResource::request(&line(dreq, 0))));
        kernel
            .holdings
            .push((successor, HwResource::mmio(PCM, 0x24)));
        self.holders[slot] = (successor, dreq);
    }

    fn end(&mut self, holder: ProcId) {
        self.rig.kernel.borrow_mut().ended.push(holder);
        self.endpoint.peer_exited(holder);
    }
}

#[test]
fn a_random_walk_never_reaches_outside_a_buffer_or_leaks_one() {
    let rig = Rig::new();
    let mut walk = Walk::new(
        &rig,
        tairix_fuzzseed::start(
            "a_random_walk_never_reaches_outside_a_buffer_or_leaks_one",
            tairix_fuzzseed::FUZZ_SEED_ENV,
        ),
    );
    for _ in 0..WALK_STEPS {
        walk.step();
    }
    assert!(
        walk.served_boundaries > 0,
        "the walk never answered a wait from an interrupt"
    );
    for (holder, _) in walk.holders {
        walk.end(holder);
    }
    assert!(
        rig.kernel.borrow().live.is_empty(),
        "every buffer was released"
    );
    assert_eq!(rig.model.live_tables(), 0, "every chain was freed");
    for channel in 0..11 {
        assert!(!rig.model.active(channel), "channel {channel} left running");
    }
}
