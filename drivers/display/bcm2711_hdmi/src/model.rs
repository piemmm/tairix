//! A register-level model of one BCM2711 HDMI block, the clocks and
//! interrupt lines its node links to, and the sink on its DDC bus.
//!
//! A register reached while the state machine clock is stopped is counted as
//! the bus stall it would be on the device.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::clock::{
    encode_describe_reply, encode_error_reply, encode_release_reply, encode_run_reply,
    ClockRequest, ClockState, CLOCK_CONTROLLER_ENDPOINTS,
};
use tairix_abi::driver::i2c::{Direction, I2cTransaction, I2C_BUFFER_LEN};
use tairix_abi::driver::irqline::{encode_reply, INTERRUPT_LINE_ENDPOINTS};
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{Delay, DriverError, Errno};
use tairix_audiochan::cyclic::mock::Channel;
use tairix_drvrt::testkit::Records;
use tairix_linkclient::{ClockClient, IrqLineClient, LinkCall};

use crate::regs::core;
use crate::{AudioLinks, Ddc, Hdmi, Links, Record, Variant, Windows};

/// The transmitter as the tests drive it.
pub type Tx = Hdmi<Regs, Link, Bus, ModelDelay, Records<Record>, Channel>;

/// The rate the MAI's sample clock runs at, the DVP's parent.
pub const AUDIO_CLOCK_HZ: u64 = 108_000_000;

/// A register window of the block.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Window {
    Core,
    Dvp,
    Phy,
    Rm,
    Packet,
    Csc,
    Hd,
}

impl Window {
    /// Its length, as the BCM2711's tree gives it.
    const fn len(self) -> usize {
        match self {
            Self::Core => 0x300,
            Self::Dvp | Self::Packet => 0x200,
            Self::Phy | Self::Rm | Self::Csc => 0x80,
            Self::Hd => 0x100,
        }
    }
}

/// One clock's controller-side state.
#[derive(Default)]
pub struct Clock {
    pub hz: u64,
    pub held: bool,
    /// Every floor asked, a release as `0`.
    pub asked: Vec<u64>,
    /// A floor is refused with this.
    pub refuses: Option<Errno>,
}

/// The sink on the DDC bus.
pub struct Sink {
    /// Its EDID, segment after segment.
    pub edid: Vec<u8>,
    /// It answers its addresses.
    pub present: bool,
    /// Reads of the EDID it leaves unanswered before it answers.
    pub slow: u32,
    /// Its SCDC registers.
    pub scdc: [u8; 256],
    /// Every segment an EDID read named.
    pub segments: Vec<u8>,
}

/// Everything the model holds.
pub struct State {
    pub regs: BTreeMap<(Window, usize), u32>,
    /// Registers reached with the state machine clock stopped.
    pub stalls: usize,
    pub hotplug: bool,
    /// The scheduler never reports HDMI active.
    pub scheduler_stuck: bool,
    pub hsm: Clock,
    pub bvb: Clock,
    pub audio: Clock,
    pub sink: Sink,
    /// Each hotplug line's posted wait and whether it fired.
    pub posted: [Option<u64>; 2],
    pub fired: [bool; 2],
    pub next_ticket: u64,
    pub now_us: u64,
}

/// The model, shared by every window and link.
#[derive(Clone)]
pub struct Model(pub Rc<RefCell<State>>);

impl Model {
    /// A block whose state machine clock the firmware left at `hsm_hz`, no
    /// sink on its connector.
    pub fn new(hsm_hz: u64) -> Self {
        Self(Rc::new(RefCell::new(State {
            regs: BTreeMap::new(),
            stalls: 0,
            hotplug: false,
            scheduler_stuck: false,
            hsm: Clock {
                hz: hsm_hz,
                ..Clock::default()
            },
            bvb: Clock::default(),
            audio: Clock::default(),
            sink: Sink {
                edid: Vec::new(),
                present: false,
                slow: 0,
                scdc: [0; 256],
                segments: Vec::new(),
            },
            posted: [None; 2],
            fired: [false; 2],
            next_ticket: 1,
            now_us: 0,
        })))
    }

    /// Plug in a sink describing itself with `blocks`.
    pub fn plug(&self, blocks: &[[u8; 128]]) {
        let mut state = self.0.borrow_mut();
        state.hotplug = true;
        state.sink.present = true;
        state.sink.edid = blocks.concat();
        state.sink.scdc = [0; 256];
    }

    /// Pull the sink out.
    pub fn unplug(&self) {
        let mut state = self.0.borrow_mut();
        state.hotplug = false;
        state.sink.present = false;
    }

    /// Raise hotplug line `line`: 0 when a sink arrives, 1 when it leaves.
    pub fn fire(&self, line: usize) {
        self.0.borrow_mut().fired[line] = true;
    }

    pub fn reg(&self, window: Window, offset: usize) -> u32 {
        self.0
            .borrow()
            .regs
            .get(&(window, offset))
            .copied()
            .unwrap_or(0)
    }

    pub fn set(&self, window: Window, offset: usize, value: u32) {
        self.0.borrow_mut().regs.insert((window, offset), value);
    }

    pub fn stalls(&self) -> usize {
        self.0.borrow().stalls
    }

    pub fn windows(&self) -> Windows<Regs> {
        let regs = |window| Regs {
            model: self.clone(),
            window,
        };
        Windows {
            core: regs(Window::Core),
            dvp: regs(Window::Dvp),
            phy: regs(Window::Phy),
            rm: regs(Window::Rm),
            packet: regs(Window::Packet),
            csc: regs(Window::Csc),
            hd: regs(Window::Hd),
        }
    }

    /// The node's links, its sound's among them where it carries `sound`.
    pub fn links(&self, sound: bool) -> Links<Link, Bus, Channel> {
        let clock = |which: Which, index| {
            let link = LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(3), index, &[13], b"")
                .expect("valid");
            ClockClient::new(Link::Clock(self.clone(), which), link)
        };
        let line = |index: u8| {
            let link = LinkRequest::new(
                INTERRUPT_LINE_ENDPOINTS.endpoint(5),
                index,
                &[u32::from(index)],
                b"",
            )
            .expect("valid");
            IrqLineClient::new(Link::Line(self.clone(), usize::from(index - 4)), link)
        };
        Links {
            hsm: clock(Which::Hsm, 0),
            bvb: clock(Which::Bvb, 1),
            ddc: Bus {
                model: self.clone(),
                buffer: vec![0; I2C_BUFFER_LEN],
            },
            hotplug: [line(4), line(5)],
            audio: sound.then(|| AudioLinks {
                clock: clock(Which::Audio, 2),
                dma: Channel::default(),
            }),
        }
    }

    /// The transmitter `variant` over the model.
    pub fn open(
        &self,
        variant: Variant,
        coexists: bool,
        records: &Records<Record>,
    ) -> Result<Tx, DriverError> {
        self.open_with(variant, coexists, records, true)
    }

    /// [`Self::open`], carrying sound only where `sound` says.
    pub fn open_with(
        &self,
        variant: Variant,
        coexists: bool,
        records: &Records<Record>,
        sound: bool,
    ) -> Result<Tx, DriverError> {
        Hdmi::open(
            self.windows(),
            variant,
            self.links(sound),
            ModelDelay(self.clone()),
            coexists,
            records.clone(),
        )
    }
}

/// A window of the block.
pub struct Regs {
    model: Model,
    window: Window,
}

impl RegisterBlock for Regs {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        let mut state = self.model.0.borrow_mut();
        if state.hsm.hz == 0 {
            state.stalls += 1;
            return Ok(0);
        }
        let stored = |offset| state.regs.get(&(self.window, offset)).copied().unwrap_or(0);
        Ok(match (self.window, offset) {
            (Window::Core, core::HOTPLUG) => u32::from(state.hotplug),
            (Window::Core, core::SCHEDULER_CONTROL) => {
                let value = stored(offset) & !core::SCHEDULER_HDMI_ACTIVE;
                if !state.scheduler_stuck && value & core::SCHEDULER_MODE_HDMI != 0 {
                    value | core::SCHEDULER_HDMI_ACTIVE
                } else {
                    value
                }
            }
            (Window::Core, core::RAM_PACKET_STATUS) => stored(core::RAM_PACKET_CONFIG) & 0xFFFF,
            (Window::Core, core::FIFO_CTL) => stored(offset) | core::FIFO_RECENTER_DONE,
            _ => stored(offset),
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        let mut state = self.model.0.borrow_mut();
        if state.hsm.hz == 0 {
            state.stalls += 1;
            return Ok(());
        }
        state.regs.insert((self.window, offset), value);
        Ok(())
    }

    fn block_len(&self) -> usize {
        self.window.len()
    }
}

/// Which of the node's clocks a link names.
#[derive(Copy, Clone)]
pub enum Which {
    Hsm,
    Bvb,
    Audio,
}

/// The node's links to its clocks and its interrupt controller.
pub enum Link {
    Clock(Model, Which),
    Line(Model, usize),
}

impl LinkCall for Link {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let Self::Clock(model, which) = self else {
            return Err(Errno::NotSupported);
        };
        let mut state = model.0.borrow_mut();
        let (clock, max_hz) = match which {
            Which::Hsm => (&mut state.hsm, 600_000_000),
            Which::Bvb => (&mut state.bvb, 600_000_000),
            Which::Audio => (&mut state.audio, AUDIO_CLOCK_HZ),
        };
        match ClockRequest::decode(request)? {
            ClockRequest::Describe(_) => encode_describe_reply(
                reply,
                ClockState {
                    hz: clock.hz,
                    max_hz,
                    held_elsewhere: false,
                },
            ),
            ClockRequest::Floor { hz, .. } => {
                if let Some(refusal) = clock.refuses {
                    return encode_error_reply(reply, refusal);
                }
                clock.asked.push(hz);
                clock.hz = hz;
                clock.held = true;
                encode_run_reply(reply, hz)
            }
            ClockRequest::Release(_) => {
                if !clock.held {
                    return encode_error_reply(reply, Errno::NotFound);
                }
                clock.asked.push(0);
                clock.held = false;
                clock.hz = 0;
                encode_release_reply(reply)
            }
            ClockRequest::Run { .. } => encode_error_reply(reply, Errno::NotSupported),
        }
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        let Self::Line(model, line) = self else {
            return Err(Errno::NotSupported);
        };
        let mut state = model.0.borrow_mut();
        if state.posted[*line].is_some() {
            return Err(Errno::Busy);
        }
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.posted[*line] = Some(ticket);
        Ok(ticket)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        let Self::Line(model, line) = self else {
            return Err(Errno::NotSupported);
        };
        let mut state = model.0.borrow_mut();
        if state.posted[*line] != Some(ticket) {
            return Err(Errno::NotFound);
        }
        if !state.fired[*line] {
            return Ok(None);
        }
        state.fired[*line] = false;
        state.posted[*line] = None;
        encode_reply(reply, Ok(())).map(Some)
    }
}

/// The DDC bus, the sink on it answering.
pub struct Bus {
    model: Model,
    buffer: Vec<u8>,
}

const SEGMENT: u8 = 0x30;
const EDID: u8 = 0x50;
const SCDC: u8 = 0x54;
const SEGMENT_LEN: usize = 256;

impl Ddc for Bus {
    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    fn run(&mut self, transaction: &I2cTransaction) -> Result<(), DriverError> {
        let mut state = self.model.0.borrow_mut();
        let sink = &mut state.sink;
        if !sink.present {
            return Err(DriverError::NotFound);
        }
        let reads_edid = transaction
            .messages()
            .iter()
            .any(|message| message.address.get() == EDID);
        if reads_edid && sink.slow > 0 {
            sink.slow -= 1;
            return Err(DriverError::NotFound);
        }
        let (mut segment, mut pointer, mut register) = (0, 0, 0);
        for message in transaction.messages() {
            let span = message.span();
            match (message.address.get(), message.direction) {
                (SEGMENT, Direction::Write) => {
                    segment = usize::from(self.buffer[span.start]);
                    sink.segments.push(self.buffer[span.start]);
                }
                (EDID, Direction::Write) => pointer = usize::from(self.buffer[span.start]),
                (EDID, Direction::Read) => {
                    for (at, byte) in self.buffer[span].iter_mut().enumerate() {
                        let index = segment * SEGMENT_LEN + pointer + at;
                        *byte = sink.edid.get(index).copied().unwrap_or(0xFF);
                    }
                }
                (SCDC, Direction::Write) => {
                    let bytes = &self.buffer[span];
                    register = usize::from(bytes[0]);
                    for &byte in &bytes[1..] {
                        sink.scdc[register] = byte;
                        register += 1;
                    }
                }
                (SCDC, Direction::Read) => {
                    for (at, byte) in self.buffer[span].iter_mut().enumerate() {
                        *byte = sink.scdc[register + at];
                    }
                }
                _ => return Err(DriverError::NotFound),
            }
        }
        Ok(())
    }
}

/// The clock the transmitter's waits and deadlines run on: a pause moves it.
pub struct ModelDelay(pub Model);

impl Delay for ModelDelay {
    fn delay_us(&self, us: u32) {
        self.0 .0.borrow_mut().now_us += u64::from(us);
    }

    fn now_us(&self) -> u64 {
        self.0 .0.borrow().now_us
    }
}
