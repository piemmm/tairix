//! A register-level model of the HVS and its pixel valves, frames the HVS
//! sees only once flushed, encoders answered by the real `encoder-v1` server,
//! and a firmware core clock: what the engine's tests drive it over.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::clock::{
    encode_describe_reply, encode_release_reply, encode_run_reply, ClockRequest, ClockState,
    CLOCK_CONTROLLER_ENDPOINTS,
};
use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::{Connector, Encoder, Phase, PipeLimits, ENCODER_ENDPOINTS};
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{Delay, DriverError, Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::Supplier;
use tairix_encoder::{EncoderServer, Record as EncoderRecord};
use tairix_linkclient::{ClockClient, EncoderClient, LinkCall};

use crate::frames::{FrameAllocator, FrameMemory};
use crate::regs::{hvs, pv};

/// One write: the block (`None` for the HVS, else the valve), the offset and
/// the value.
pub type Write = (Option<usize>, usize, u32);

/// The HVS and both valves' registers, and every write in order.
#[derive(Default)]
pub struct Registers {
    hvs: Vec<u32>,
    valves: [Vec<u32>; 2],
    pub writes: Vec<Write>,
    /// A valve whose video, once on, never reads back off.
    pub stuck: Option<usize>,
}

/// The model, shared by its blocks and the test.
#[derive(Clone)]
pub struct Model(pub Rc<RefCell<Registers>>);

impl Model {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Registers {
            hvs: vec![0; hvs::WINDOW_LEN / 4],
            valves: [vec![0; pv::WINDOW_LEN / 4], vec![0; pv::WINDOW_LEN / 4]],
            ..Registers::default()
        })))
    }

    pub fn hvs(&self) -> Block {
        Block(self.clone(), None)
    }

    pub fn valve(&self, index: usize) -> Block {
        Block(self.clone(), Some(index))
    }

    pub fn read(&self, block: Option<usize>, offset: usize) -> u32 {
        let state = self.0.borrow();
        match block {
            None => state.hvs[offset / 4],
            Some(index) => state.valves[index][offset / 4],
        }
    }

    /// Set a register as the firmware left it.
    pub fn set(&self, block: Option<usize>, offset: usize, value: u32) {
        let mut state = self.0.borrow_mut();
        match block {
            None => state.hvs[offset / 4] = value,
            Some(index) => state.valves[index][offset / 4] = value,
        }
    }

    /// A frame starts on `channel`: an enabled channel latches its list.
    pub fn frame(&self, channel: usize) {
        if self.read(None, hvs::dispctrlx(channel)) & hvs::DISPCTRLX_ENABLE != 0 {
            let list = self.read(None, hvs::displist(channel));
            self.set(None, hvs::displact(channel), list);
        }
    }

    /// The word at `at` of display-list memory.
    pub fn list_word(&self, at: usize) -> u32 {
        self.read(None, hvs::DLIST + 4 * at)
    }

    /// The bus address the plane `channel` composites reads, if any.
    pub fn scanned(&self, channel: usize) -> Option<u32> {
        let at = self.read(None, hvs::displact(channel)) as usize;
        let control = self.list_word(at);
        (control & (1 << 30) != 0)
            .then(|| self.list_word(at + 6 - usize::from(control & (1 << 15) != 0)))
    }

    pub fn writes(&self) -> Vec<Write> {
        self.0.borrow().writes.clone()
    }
}

pub struct Block(Model, Option<usize>);

impl RegisterBlock for Block {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        if !offset.is_multiple_of(4) || offset >= self.block_len() {
            return Err(DriverError::OutOfRange);
        }
        Ok(self.0.read(self.1, offset))
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        if !offset.is_multiple_of(4) || offset >= self.block_len() {
            return Err(DriverError::OutOfRange);
        }
        let mut state = self.0 .0.borrow_mut();
        state.writes.push((self.1, offset, value));
        match self.1 {
            None if (hvs::displact(0)..=hvs::displact(2)).contains(&offset) => {}
            None => state.hvs[offset / 4] = value,
            Some(index) => {
                let stuck = state.stuck == Some(index) && offset == pv::V_CONTROL;
                let old = state.valves[index][offset / 4];
                state.valves[index][offset / 4] = if stuck {
                    value | (old & pv::VCONTROL_VIDEN)
                } else {
                    value
                };
            }
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        match self.1 {
            None => hvs::WINDOW_LEN,
            Some(_) => pv::WINDOW_LEN,
        }
    }
}

/// A frame whose bytes the HVS sees only once flushed.
pub struct ModelFrame {
    bus: u32,
    cpu: Vec<u8>,
    device: Rc<RefCell<Vec<u8>>>,
}

impl FrameMemory for ModelFrame {
    fn bus(&self) -> u32 {
        self.bus
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.cpu
    }

    fn flush(&self, offset: usize, len: usize) {
        self.device.borrow_mut()[offset..offset + len]
            .copy_from_slice(&self.cpu[offset..offset + len]);
    }
}

/// The bytes of a frame the HVS sees.
pub type DeviceView = Rc<RefCell<Vec<u8>>>;

/// Frames at bus addresses the model hands out, each one's device view kept.
#[derive(Clone, Default)]
pub struct ModelFrames {
    pub frames: Rc<RefCell<Vec<(u32, DeviceView)>>>,
    /// Frames the allocator gives before it runs out.
    pub limit: Option<usize>,
}

/// Where the model's frames start on the bus: the legacy alias of RAM.
const FRAMES_BUS: u32 = 0xC000_0000;

impl ModelFrames {
    /// What the HVS sees at `bus`.
    pub fn seen(&self, bus: u32) -> Vec<u8> {
        self.frames
            .borrow()
            .iter()
            .find(|(at, _)| *at == bus)
            .map(|(_, device)| device.borrow().clone())
            .expect("a frame at that address")
    }
}

impl FrameAllocator for ModelFrames {
    type Memory = ModelFrame;

    fn alloc(&self, len: usize) -> Result<ModelFrame, DriverError> {
        let mut frames = self.frames.borrow_mut();
        if self.limit.is_some_and(|limit| frames.len() >= limit) {
            return Err(DriverError::OutOfMemory);
        }
        let bus = frames.last().map_or(FRAMES_BUS, |(at, device)| {
            at + u32::try_from(device.borrow().len()).expect("small")
        });
        let device = Rc::new(RefCell::new(vec![0; len]));
        frames.push((bus, device.clone()));
        Ok(ModelFrame {
            bus,
            cpu: vec![0; len],
            device,
        })
    }
}

/// A clock that moves only when waited on, and moves on every reading so a
/// poll that never succeeds still runs out its budget.
#[derive(Clone, Default)]
pub struct ModelDelay {
    pub now_us: Rc<Cell<u64>>,
    pub slept_us: Rc<Cell<u64>>,
}

impl Delay for ModelDelay {
    fn delay_us(&self, us: u32) {
        self.now_us.set(self.now_us.get() + u64::from(us));
        self.slept_us.set(self.slept_us.get() + u64::from(us));
    }

    fn now_us(&self) -> u64 {
        self.now_us.set(self.now_us.get() + 10);
        self.now_us.get()
    }
}

pub const fn axis(active: u16, front: u16, sync: u16, back: u16) -> Axis {
    Axis {
        active,
        front,
        sync,
        back,
    }
}

/// 1920x1080 at 60 Hz.
pub const FULL_HD: Timing = Timing {
    clock_khz: 148_500,
    horizontal: axis(1920, 88, 44, 148),
    vertical: axis(1080, 4, 5, 36),
    hsync: Polarity::Positive,
    vsync: Polarity::Positive,
    scan: Scan::Progressive,
    repetition: 1,
};

/// 1280x720 at 60 Hz.
pub const HD: Timing = Timing {
    clock_khz: 74_250,
    horizontal: axis(1280, 110, 40, 220),
    vertical: axis(720, 5, 5, 20),
    ..FULL_HD
};

/// 3840x2160 at 60 Hz.
pub const UHD: Timing = Timing {
    clock_khz: 594_000,
    horizontal: axis(3840, 176, 88, 296),
    vertical: axis(2160, 8, 10, 72),
    ..FULL_HD
};

/// What a test sets an encoder's hardware to.
pub struct Sink {
    pub connector: Connector,
    /// The sink's best mode and the one it falls back to when the pipe
    /// cannot carry it.
    pub best: Timing,
    pub fallback: Timing,
    pub phase: Phase,
    /// The encoder steps run, in order.
    pub steps: Vec<&'static str>,
    pub moved: bool,
    /// Configure fails with this.
    pub refuses: Option<DriverError>,
    pub limits: Option<PipeLimits>,
}

#[derive(Clone)]
pub struct Part(pub Rc<RefCell<Sink>>);

impl Encoder for Part {
    fn connector(&self) -> Connector {
        self.0.borrow().connector
    }

    fn phase(&self) -> Phase {
        self.0.borrow().phase
    }

    fn limit(&mut self, limits: &PipeLimits) -> Result<bool, DriverError> {
        let mut sink = self.0.borrow_mut();
        sink.limits = Some(*limits);
        if !sink.connector.connected {
            return Ok(false);
        }
        let mode = if limits.carries(&sink.best) {
            sink.best
        } else {
            sink.fallback
        };
        let moved = sink.connector.mode != Some(mode);
        sink.connector.mode = Some(mode);
        Ok(moved)
    }

    fn service_event(&mut self, _index: usize) -> Result<bool, DriverError> {
        Ok(core::mem::take(&mut self.0.borrow_mut().moved))
    }

    fn configure(&mut self, _timing: &Timing) -> Result<(), DriverError> {
        let mut sink = self.0.borrow_mut();
        if let Some(err) = sink.refuses {
            return Err(err);
        }
        sink.steps.push("configure");
        Ok(())
    }

    fn prepare(&mut self) -> Result<(), DriverError> {
        self.0.borrow_mut().steps.push("prepare");
        Ok(())
    }

    fn enable(&mut self) -> Result<(), DriverError> {
        self.0.borrow_mut().steps.push("enable");
        Ok(())
    }

    fn disable(&mut self) -> Result<(), DriverError> {
        self.0.borrow_mut().steps.push("disable");
        Ok(())
    }

    fn power_down(&mut self) -> Result<(), DriverError> {
        self.0.borrow_mut().steps.push("power_down");
        Ok(())
    }
}

/// The engine's instance, as the encoders' kernel attests it.
pub const ENGINE: ProcId = ProcId::from_raw([7; PROC_ID_LEN]);

type Server = EncoderServer<Part, FakeKernel, Records<EncoderRecord>>;

/// One encoder: its hardware, its server, and the engine's link to it.
#[derive(Clone)]
pub struct Encoders {
    pub sink: Rc<RefCell<Sink>>,
    pub server: Rc<RefCell<Server>>,
    kernel: FakeKernel,
    link: LinkRequest,
    reaped: Rc<RefCell<BTreeSet<u64>>>,
    /// The encoder's driver ended: its endpoint answers nothing.
    pub gone: Rc<Cell<bool>>,
}

impl Encoders {
    /// An encoder on node `node` whose sink is `connected`, best running
    /// `best`, its hardware found in `phase`.
    pub fn new(node: u32, connected: bool, best: Timing, phase: Phase) -> Self {
        let sink = Rc::new(RefCell::new(Sink {
            connector: Connector {
                connected,
                mode: connected.then_some(best),
                link: 0,
            },
            best,
            fallback: HD,
            phase,
            steps: Vec::new(),
            moved: false,
            refuses: None,
            limits: None,
        }));
        let endpoint = ENCODER_ENDPOINTS.endpoint(node);
        let link = LinkRequest::new(endpoint, 0, &[], b"hdmi").expect("valid");
        let kernel = FakeKernel::new();
        kernel.grant(ENGINE, HwResource::request(&link));
        let server = EncoderServer::new(
            Part(sink.clone()),
            kernel.clone(),
            Records::default(),
            endpoint,
        );
        Self {
            sink,
            server: Rc::new(RefCell::new(server)),
            kernel,
            link,
            reaped: Rc::default(),
            gone: Rc::default(),
        }
    }

    pub fn client(&self) -> EncoderClient<Link> {
        EncoderClient::new(Link::Encoder(self.clone()), self.link)
    }

    /// The sink is plugged in, or out, and its hotplug line fires.
    pub fn plug(&self, connected: bool, best: Timing) {
        {
            let mut sink = self.sink.borrow_mut();
            sink.best = best;
            let limits = sink.limits;
            let mode = connected.then(|| {
                if limits.is_none_or(|limits| limits.carries(&best)) {
                    best
                } else {
                    sink.fallback
                }
            });
            let link = sink.connector.link;
            sink.connector = Connector {
                connected,
                mode,
                link,
            };
            sink.moved = true;
        }
        self.server.borrow_mut().woke(0).expect("serviced");
    }

    /// The sink loses what its link set it up with, and says so.
    pub fn lose_link(&self) {
        {
            let mut sink = self.sink.borrow_mut();
            sink.connector.link += 1;
            sink.moved = true;
        }
        self.server.borrow_mut().woke(0).expect("serviced");
    }

    pub fn steps(&self) -> Vec<&'static str> {
        self.sink.borrow().steps.clone()
    }
}

impl LinkCall for Encoders {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        if self.gone.get() {
            return Err(Errno::NotFound);
        }
        let ticket = self.kernel.call(ENGINE);
        self.server.borrow_mut().serve(ticket, request);
        let answer = self.kernel.answer(ticket).ok_or(Errno::NotFound)?;
        reply[..answer.len()].copy_from_slice(&answer);
        Ok(answer.len())
    }

    fn post(&mut self, request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        if self.gone.get() {
            return Err(Errno::NotFound);
        }
        let ticket = self.kernel.call(ENGINE);
        self.server.borrow_mut().serve(ticket, request);
        Ok(ticket)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        if self.gone.get() || self.reaped.borrow().contains(&ticket) {
            return Err(Errno::NotFound);
        }
        let Some(answer) = self.kernel.answer(ticket) else {
            return Ok(None);
        };
        self.reaped.borrow_mut().insert(ticket);
        reply[..answer.len()].copy_from_slice(&answer);
        Ok(Some(answer.len()))
    }
}

/// The firmware's core clock: its rate and ceiling, and every floor asked.
#[derive(Clone)]
pub struct Core {
    pub state: Rc<RefCell<(u64, u64, Vec<u64>)>>,
}

impl Core {
    pub fn new(hz: u64, max_hz: u64) -> Self {
        Self {
            state: Rc::new(RefCell::new((hz, max_hz, Vec::new()))),
        }
    }

    pub fn client(&self) -> ClockClient<Link> {
        let link =
            LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(3), 0, &[4], b"").expect("valid");
        ClockClient::new(Link::Clock(self.clone()), link)
    }

    pub fn floors(&self) -> Vec<u64> {
        self.state.borrow().2.clone()
    }
}

impl LinkCall for Core {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let mut state = self.state.borrow_mut();
        match ClockRequest::decode(request)? {
            ClockRequest::Describe(_) => encode_describe_reply(
                reply,
                ClockState {
                    hz: state.0,
                    max_hz: state.1,
                    held_elsewhere: false,
                },
            ),
            ClockRequest::Floor { hz, .. } => {
                state.2.push(hz);
                state.0 = state.0.max(hz);
                encode_run_reply(reply, state.0)
            }
            ClockRequest::Release(_) => {
                state.2.push(0);
                encode_release_reply(reply)
            }
            ClockRequest::Run { .. } => Err(Errno::NotSupported),
        }
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        Err(Errno::NotSupported)
    }

    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        Err(Errno::NotSupported)
    }
}

/// The engine's links, encoders and clock alike, as one type.
pub enum Link {
    Encoder(Encoders),
    Clock(Core),
}

impl LinkCall for Link {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        match self {
            Self::Encoder(link) => link.call(request, reply),
            Self::Clock(link) => link.call(request, reply),
        }
    }

    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno> {
        match self {
            Self::Encoder(link) => link.post(request, deadline_ns),
            Self::Clock(link) => link.post(request, deadline_ns),
        }
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        match self {
            Self::Encoder(link) => link.reap(ticket, reply),
            Self::Clock(link) => link.reap(ticket, reply),
        }
    }
}
