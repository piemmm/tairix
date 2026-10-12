//! The engine: the seat's display, driven through the HVS, the pixel valves
//! feeding the HDMI outputs, and the encoders behind them.
//!
//! The display's surface takes the mode of the first output with a sink it
//! can drive, in valve order; every other connected output shows the same
//! surface scaled to its own mode, its aspect kept. A present copies into a
//! frame no channel reads, and each channel latches it at its next frame
//! start. A mode set goes up and comes down in the order the encoder holds
//! its phases to, interleaved with the valve's and the channel's steps.

use tairix_abi::driver::clock;
use tairix_abi::driver::display::{
    DamageRect, Display, DisplayFormat, DisplayMode, DisplayPower, Scan, Timing,
};
use tairix_abi::driver::encoder::{Connector, EncoderOp, Phase, PipeLimits};
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::seat::ReleaseSurface;
use tairix_abi::{Delay, DriverError};
use tairix_drvrt::Recorder;
use tairix_inline::ArrayVec;
use tairix_linkclient::{ClockClient, EncoderClient, LinkCall};

use crate::dlist::{lbm_words, plane, Place, Scaling, Surface, LBM_WORDS};
use crate::frames::{FrameAllocator, Frames};
use crate::hvs::{Hvs, List, FRAMES_MAX, KERNEL_AT};
use crate::valve::{Load, Valve, ValveKind};

/// The outputs one engine drives.
pub const OUTPUTS_MAX: usize = 2;

/// The core clock a mode set runs at, at least: Linux's `vc4_kms` raises it
/// for the span of one.
const MODESET_CORE_HZ: u64 = 500_000_000;

/// The margin the HVS needs over an active-loaded output's pixel rate.
const ACTIVE_MARGIN_KHZ: u32 = 8_000;

/// Bytes a scan-out line's start is aligned to.
const STRIDE_ALIGN: u32 = 64;

/// Line-buffer words each output's channel scales through.
const LBM_SHARE: usize = LBM_WORDS as usize / OUTPUTS_MAX;

/// A decision the engine records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// The pipe the firmware left running on a valve was taken down.
    TookOver {
        /// The valve's window.
        valve: &'static [u8],
    },
    /// An output came up.
    Up {
        /// The encoder's link.
        encoder: &'static [u8],
        /// Its mode's active pixels across.
        width: u32,
        /// Its mode's active lines.
        height: u32,
        /// Its frame rate, in millihertz.
        refresh_mhz: u32,
    },
    /// An output went down.
    Down {
        /// The encoder's link.
        encoder: &'static [u8],
    },
    /// A mode set failed, leaving the output dark until its connector moves.
    Failed {
        /// The encoder's link.
        encoder: &'static [u8],
        /// Why.
        reason: DriverError,
    },
    /// The encoder's driver ended.
    EncoderGone {
        /// The encoder's link.
        encoder: &'static [u8],
    },
    /// A valve told to stop outran its budget finishing its frame.
    StopOverran {
        /// The valve's window.
        valve: &'static [u8],
    },
    /// The core clock refused the floor the outputs need.
    ClockRefused {
        /// The floor asked for.
        hz: u64,
        /// Why.
        reason: DriverError,
    },
}

/// What the engine's outputs show.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Shows {
    /// Black: a presenter is taking over.
    Nothing,
    /// The boot console's surface: the seat shows text.
    Console,
    /// The frames presented.
    Frames,
}

/// One output: its valve, the encoder it feeds, and what the engine knows of
/// both.
struct Pipe<V, C: LinkCall> {
    valve: Valve<V>,
    encoder: EncoderClient<C>,
    connector: Connector,
    phase: Phase,
    seen: u64,
    runs: Option<Timing>,
    /// The connector's link generation the output was brought up under.
    runs_link: u32,
    /// A mode set failed; the output waits for its connector to move.
    failed: bool,
    /// The encoder's driver ended.
    gone: bool,
}

impl<V, C: LinkCall> Pipe<V, C> {
    /// The mode the output should run.
    fn wanted(&self) -> Option<Timing> {
        if self.gone || self.failed || !self.connector.connected {
            return None;
        }
        self.connector.mode
    }

    /// The output runs, but not as it should: another mode, or a link its
    /// sink has lost since it came up.
    fn stale(&self, wanted: Option<Timing>) -> bool {
        self.runs.is_some() && (self.runs != wanted || self.runs_link != self.connector.link)
    }

    const fn name(&self) -> &'static [u8] {
        self.valve.kind().encoder
    }
}

/// What the engine is built from.
pub struct Parts<H, V, C: LinkCall, A, D, L> {
    /// The HVS's window.
    pub hvs: H,
    /// Each valve's window, what it is, and the client of the encoder it
    /// feeds.
    pub valves: ArrayVec<(V, ValveKind, EncoderClient<C>), OUTPUTS_MAX>,
    /// The core clock the HVS runs from.
    pub clock: ClockClient<C>,
    /// The boot console's surface, which the outputs show while the seat
    /// shows text.
    pub console: Option<Surface>,
    /// Where scan-out frames come from.
    pub alloc: A,
    /// The timed waits a mode set takes.
    pub delay: D,
    /// Where decisions are recorded.
    pub log: L,
}

/// The display engine.
pub struct Engine<H, V, C: LinkCall, A: FrameAllocator, D, L> {
    hvs: Hvs<H>,
    pipes: ArrayVec<Pipe<V, C>, OUTPUTS_MAX>,
    clock: ClockClient<C>,
    core_max_hz: u64,
    floor_hz: u64,
    surface: Option<DisplayMode>,
    frames: Option<Frames<A::Memory>>,
    alloc: A,
    console: Option<Surface>,
    shows: Shows,
    power: DisplayPower,
    delay: D,
    log: L,
}

/// What the pipe a valve of `load` feeds carries when the core clock runs at
/// `core_hz` at most: Linux's `vc4_crtc_atomic_check` load, inverted.
#[must_use]
pub fn pipe_limits(load: Load, core_hz: u64) -> PipeLimits {
    let ceiling = u32::try_from(core_hz / 1_000).unwrap_or(u32::MAX);
    match load {
        Load::Active => PipeLimits {
            max_clock_khz: u32::try_from(u64::from(ceiling) * 10 / 9).unwrap_or(u32::MAX),
            max_active_khz: ceiling.saturating_sub(ACTIVE_MARGIN_KHZ).max(1),
        },
        Load::Clock => PipeLimits {
            max_clock_khz: ceiling.max(1),
            max_active_khz: ceiling.max(1),
        },
    }
}

/// The core clock an output of `load` running `timing` needs.
fn output_load_hz(load: Load, timing: &Timing) -> u64 {
    let clock_khz = u64::from(timing.clock_khz);
    let khz = match load {
        Load::Active => (u64::from(timing.active_rate_khz()) + u64::from(ACTIVE_MARGIN_KHZ))
            .max(clock_khz * 9 / 10),
        Load::Clock => clock_khz,
    };
    khz * 1_000
}

/// The surface a timing's sink shows.
fn surface_of(timing: &Timing) -> DisplayMode {
    let width = u32::from(timing.horizontal.active);
    DisplayMode {
        width_px: width,
        height_px: u32::from(timing.vertical.active),
        stride_bytes: (width * 4).next_multiple_of(STRIDE_ALIGN),
        format: DisplayFormat::Bgra8888,
    }
}

impl<H, V, C, A, D, L> Engine<H, V, C, A, D, L>
where
    H: RegisterBlock,
    V: RegisterBlock,
    C: LinkCall,
    A: FrameAllocator,
    D: Delay,
    L: Recorder<Record>,
{
    /// Take the display over from the firmware: every pipe it left running
    /// brought down in order, the HVS set up, each encoder held to what its
    /// pipe carries, and every output with a sink brought up.
    ///
    /// # Errors
    ///
    /// A window too short for its registers, the core clock's refusal to
    /// describe itself, or the HVS refusing its set-up.
    pub fn open(parts: Parts<H, V, C, A, D, L>) -> Result<Self, DriverError> {
        let hvs = Hvs::new(parts.hvs)?;
        let mut clock = parts.clock;
        let core = clock.describe().map_err(clock::refusal)?;
        let mut pipes = ArrayVec::new();
        let mut log = parts.log;
        for (window, kind, mut encoder) in parts.valves {
            let valve = Valve::new(window, kind)?;
            let state = encoder
                .limit(&pipe_limits(kind.load, core.max_hz))
                .and_then(|()| encoder.describe());
            let pipe = match state {
                Ok(state) => Pipe {
                    valve,
                    encoder,
                    connector: state.connector,
                    phase: state.phase,
                    seen: state.moves,
                    runs: None,
                    runs_link: 0,
                    failed: false,
                    gone: false,
                },
                Err(reason) => {
                    log.record(Record::Failed {
                        encoder: kind.encoder,
                        reason,
                    });
                    Pipe {
                        valve,
                        encoder,
                        connector: Connector {
                            connected: false,
                            mode: None,
                            link: 0,
                        },
                        phase: Phase::Off,
                        seen: 0,
                        runs: None,
                        runs_link: 0,
                        failed: false,
                        gone: true,
                    }
                }
            };
            pipes
                .try_push(pipe)
                .map_err(|_| DriverError::LengthOutOfRange)?;
        }
        let mut engine = Self {
            hvs,
            pipes,
            clock,
            core_max_hz: core.max_hz,
            floor_hz: 0,
            surface: None,
            frames: None,
            alloc: parts.alloc,
            console: parts.console,
            shows: if parts.console.is_some() {
                Shows::Console
            } else {
                Shows::Nothing
            },
            power: DisplayPower::On,
            delay: parts.delay,
            log,
        };
        engine.take_over()?;
        for at in 0..engine.pipes.len() {
            engine.watch(at);
        }
        // An output that will not come up is recorded and left dark.
        let _ = engine.reconcile();
        Ok(engine)
    }

    /// Bring down every pipe the firmware left up, then set the HVS up.
    fn take_over(&mut self) -> Result<(), DriverError> {
        for at in 0..self.pipes.len() {
            let pipe = &self.pipes[at];
            let output = pipe.valve.kind().output;
            if pipe.valve.sending()? || pipe.phase != Phase::Off {
                self.log.record(Record::TookOver {
                    valve: pipe.valve.kind().window,
                });
                let channel = self.hvs.muxed(output)?;
                // A firmware pipe that will not come down in order is left to
                // the HVS's reset below; its output stays dark.
                let _ = self.bring_down(at, channel);
            }
        }
        self.hvs.take_over()
    }

    /// Post the wait that answers when pipe `at`'s connector moves.
    fn watch(&mut self, at: usize) {
        let pipe = &mut self.pipes[at];
        if pipe.gone || pipe.encoder.is_waiting() {
            return;
        }
        if pipe.encoder.post_wait(pipe.seen).is_err() {
            pipe.gone = true;
            self.log.record(Record::EncoderGone {
                encoder: pipe.name(),
            });
        }
    }

    /// Run encoder step `op` on pipe `at`, tracking the phase it reaches.
    fn step(&mut self, at: usize, op: EncoderOp) -> Result<(), DriverError> {
        let pipe = &mut self.pipes[at];
        let result = match op {
            EncoderOp::Prepare => pipe.encoder.prepare(),
            EncoderOp::Enable => pipe.encoder.enable(),
            EncoderOp::Disable => pipe.encoder.disable(),
            EncoderOp::PowerDown => pipe.encoder.power_down(),
            _ => Err(DriverError::OutOfRange),
        };
        if result.is_ok() {
            pipe.phase = pipe.phase.after(op).unwrap_or(pipe.phase);
        }
        result
    }

    /// Bring pipe `at` down in the order its encoder holds it to, `channel`
    /// the HVS channel feeding it. Every step is tried; the first refusal is
    /// answered.
    fn bring_down(&mut self, at: usize, channel: Option<usize>) -> Result<(), DriverError> {
        let mut first = Ok(());
        let mut keep = |result: Result<(), DriverError>| {
            if first.is_ok() {
                first = result;
            }
        };
        let pipe = &self.pipes[at];
        let (kind, sending) = (*pipe.valve.kind(), pipe.valve.sending());
        match sending.and_then(|on| {
            if on {
                pipe.valve.stop_video(&self.delay)
            } else {
                Ok(true)
            }
        }) {
            Ok(true) => {}
            Ok(false) => self.log.record(Record::StopOverran { valve: kind.window }),
            Err(err) => keep(Err(err)),
        }
        if self.pipes[at].phase == Phase::Enabled {
            keep(self.step(at, EncoderOp::Disable));
        }
        keep(self.pipes[at].valve.reset());
        if let Some(channel) = channel {
            keep(self.hvs.stop(channel));
        }
        keep(self.hvs.mux(kind.output, None));
        if self.pipes[at].phase != Phase::Off {
            keep(self.step(at, EncoderOp::PowerDown));
        }
        let pipe = &mut self.pipes[at];
        if pipe.runs.take().is_some() {
            self.log.record(Record::Down {
                encoder: pipe.name(),
            });
        }
        first
    }

    /// The list a channel latches for what the outputs show.
    fn list(&self) -> List {
        match self.shows {
            Shows::Console if self.console.is_some() => List::Console,
            Shows::Frames => self
                .frames
                .as_ref()
                .and_then(Frames::latest)
                .map_or(List::Black, List::Frame),
            _ => List::Black,
        }
    }

    /// The line buffer channel `channel` scales through.
    fn lbm_at(channel: usize) -> Result<u32, DriverError> {
        u32::try_from(channel * LBM_SHARE).map_err(|_| DriverError::OutOfRange)
    }

    /// Write channel `channel`'s list showing `surface` on an output of
    /// `width` by `height`.
    fn write_plane(
        &self,
        channel: usize,
        list: List,
        surface: &Surface,
        width: u32,
        height: u32,
    ) -> Result<(), DriverError> {
        let place = Place::fit(surface.width, surface.height, width, height);
        let x = Scaling::of(surface.width, place.width);
        let y = Scaling::of(surface.height, place.height);
        if lbm_words(surface.width, x, y, place.width) as usize > LBM_SHARE {
            return Err(DriverError::OutOfRange);
        }
        let kernel = u32::try_from(KERNEL_AT).map_err(|_| DriverError::OutOfRange)?;
        let words = plane(surface, &place, kernel, Self::lbm_at(channel)?)?;
        self.hvs.write_slot(channel, list, &words)
    }

    /// The frame list `index` of the current surface.
    fn frame_surface(&self, index: usize) -> Option<Surface> {
        let frames = self.frames.as_ref()?;
        let mode = frames.mode();
        Some(Surface {
            bus: frames.bus(index)?,
            stride: mode.stride_bytes,
            width: mode.width_px,
            height: mode.height_px,
            format: mode.format,
        })
    }

    /// Write every list of pipe `at`, whose valve runs `timing`.
    fn write_lists(&self, at: usize, timing: &Timing) -> Result<(), DriverError> {
        let channel = self.pipes[at].valve.kind().channel;
        let (width, height) = (
            u32::from(timing.horizontal.active),
            u32::from(timing.vertical.active),
        );
        self.hvs.write_slot(channel, List::Black, &[])?;
        if let Some(console) = self.console {
            self.write_plane(channel, List::Console, &console, width, height)?;
        }
        let count = self.frames.as_ref().map_or(0, Frames::len);
        for index in 0..count {
            if let Some(surface) = self.frame_surface(index) {
                self.write_plane(channel, List::Frame(index), &surface, width, height)?;
            }
        }
        Ok(())
    }

    /// Bring pipe `at` up running `timing`.
    fn bring_up(&mut self, at: usize, timing: Timing) -> Result<(), DriverError> {
        let kind = *self.pipes[at].valve.kind();
        self.write_lists(at, &timing)?;
        self.hvs.mux(kind.output, Some(kind.channel))?;
        self.hvs.start(
            kind.channel,
            u32::from(timing.horizontal.active),
            u32::from(timing.vertical.active),
            timing.scan == Scan::Interlaced,
        )?;
        self.hvs.latch(kind.channel, self.list())?;
        let pipe = &mut self.pipes[at];
        pipe.encoder.configure(&timing)?;
        pipe.phase = Phase::Configured;
        pipe.valve.configure(&timing)?;
        pipe.valve.enable()?;
        self.step(at, EncoderOp::Prepare)?;
        self.pipes[at].valve.start_video()?;
        self.step(at, EncoderOp::Enable)?;
        let pipe = &mut self.pipes[at];
        pipe.runs = Some(timing);
        pipe.runs_link = pipe.connector.link;
        self.log.record(Record::Up {
            encoder: kind.encoder,
            width: u32::from(timing.horizontal.active),
            height: u32::from(timing.vertical.active),
            refresh_mhz: timing.refresh_mhz(),
        });
        Ok(())
    }

    /// The core clock the pipes running, or about to run, `timings` need:
    /// the most any one output loads it, or the share of every plane's
    /// pixels it composites, as Linux's `vc4_core_clock_atomic_check` has it.
    fn needed_hz(&self, timings: &[Option<Timing>]) -> u64 {
        let surface = self.surface;
        let mut most = 0;
        let mut pixels = 0;
        let mut running = 0;
        for (pipe, timing) in self.pipes.iter().zip(timings) {
            let Some(timing) = timing else { continue };
            running += 1;
            most = most.max(output_load_hz(pipe.valve.kind().load, timing));
            let (width, height) = surface.map_or((0, 0), |mode| (mode.width_px, mode.height_px));
            let place = Place::fit(
                width,
                height,
                u32::from(timing.horizontal.active),
                u32::from(timing.vertical.active),
            );
            pixels +=
                u64::from(place.width) * u64::from(place.height) * u64::from(timing.refresh_mhz())
                    / 1_000;
        }
        let share = if running > 1 { 40 } else { 60 };
        most.max(pixels * share / 100)
    }

    /// Hold the core clock at no less than `hz`, within what it can run at.
    fn floor(&mut self, hz: u64) {
        let hz = hz.min(self.core_max_hz);
        if hz == self.floor_hz {
            return;
        }
        let result = if hz == 0 {
            self.clock.release()
        } else {
            self.clock.floor(hz).map(|_| ())
        };
        match result {
            Ok(()) => self.floor_hz = hz,
            Err(errno) => self.log.record(Record::ClockRefused {
                hz,
                reason: clock::refusal(errno),
            }),
        }
    }

    /// Bring every output to what its connector, the power and the surface
    /// ask: the surface follows the first output with a sink it can drive,
    /// and every output that runs shows it. Answers the first output that
    /// would not come up, which is recorded and left dark.
    fn reconcile(&mut self) -> Result<(), DriverError> {
        let mut first = Ok(());
        // An output that fails leaves the surface to the next; each pass
        // fails one more at most.
        for _ in 0..=OUTPUTS_MAX {
            match self.converge() {
                Ok(()) => break,
                Err(reason) => first = first.and(Err(reason)),
            }
        }
        first
    }

    /// One pass of [`Self::reconcile`], answering the first output that
    /// failed in it.
    fn converge(&mut self) -> Result<(), DriverError> {
        let surface = self
            .pipes
            .iter()
            .find_map(Pipe::wanted)
            .map(|t| surface_of(&t));
        if surface != self.surface {
            for at in 0..self.pipes.len() {
                if self.pipes[at].runs.is_some() {
                    let channel = Some(self.pipes[at].valve.kind().channel);
                    self.down_or_fail(at, channel);
                }
            }
            self.frames = None;
            self.surface = surface;
        }
        let mut wanted = [None; OUTPUTS_MAX];
        for (want, pipe) in wanted.iter_mut().zip(&self.pipes) {
            *want = if self.power == DisplayPower::On {
                pipe.wanted()
            } else {
                None
            };
        }
        for (at, want) in wanted.iter().enumerate().take(self.pipes.len()) {
            if self.pipes[at].stale(*want) {
                let channel = Some(self.pipes[at].valve.kind().channel);
                self.down_or_fail(at, channel);
            }
        }
        let wanted = &wanted[..self.pipes.len()];
        let needed = self.needed_hz(wanted);
        let rising =
            (0..self.pipes.len()).any(|at| self.pipes[at].runs.is_none() && wanted[at].is_some());
        if rising {
            self.floor(needed.max(self.floor_hz).max(MODESET_CORE_HZ));
        }
        let mut first = Ok(());
        for (at, &want) in wanted.iter().enumerate() {
            let Some(timing) = want else { continue };
            if self.pipes[at].runs.is_some() {
                continue;
            }
            if let Err(reason) = self.bring_up(at, timing) {
                let channel = Some(self.pipes[at].valve.kind().channel);
                let _ = self.bring_down(at, channel);
                // A connector that moved under the mode set is followed when
                // its move is reaped, not counted a failure.
                if reason != DriverError::Stale {
                    self.pipes[at].failed = true;
                    self.log.record(Record::Failed {
                        encoder: self.pipes[at].name(),
                        reason,
                    });
                    first = first.and(Err(reason));
                }
            }
        }
        let mut running = [None; OUTPUTS_MAX];
        for (runs, pipe) in running.iter_mut().zip(&self.pipes) {
            *runs = pipe.runs;
        }
        let settled = self.needed_hz(&running[..self.pipes.len()]);
        self.floor(settled);
        first
    }

    /// Bring pipe `at` down, failing it when it will not come down in order.
    fn down_or_fail(&mut self, at: usize, channel: Option<usize>) {
        if let Err(reason) = self.bring_down(at, channel) {
            self.pipes[at].failed = true;
            self.log.record(Record::Failed {
                encoder: self.pipes[at].name(),
                reason,
            });
        }
    }

    /// The channels running, each with what the engine feeds it.
    fn running_channels(&self) -> impl Iterator<Item = usize> + '_ {
        self.pipes
            .iter()
            .filter(|pipe| pipe.runs.is_some())
            .map(|pipe| pipe.valve.kind().channel)
    }

    /// Have every running channel latch what the outputs show.
    fn latch_all(&self) -> Result<(), DriverError> {
        let list = self.list();
        for channel in self.running_channels() {
            self.hvs.latch(channel, list)?;
        }
        Ok(())
    }

    /// Hold the frames a present needs for `mode`: one for each running
    /// channel to composite, one for each to latch, and the one written —
    /// every new frame's lists written on every running channel.
    fn frames_for(&mut self, mode: DisplayMode) -> Result<(), DriverError> {
        if self
            .frames
            .as_ref()
            .is_none_or(|frames| *frames.mode() != mode)
        {
            self.frames = Some(Frames::new(mode));
        }
        let running = self.running_channels().count().max(1);
        let Some(frames) = self.frames.as_mut() else {
            return Err(DriverError::DeviceFault);
        };
        let before = frames.grow(&self.alloc, (2 + running).min(FRAMES_MAX))?;
        let count = frames.len();
        for at in 0..self.pipes.len() {
            let Some(timing) = self.pipes[at].runs else {
                continue;
            };
            let channel = self.pipes[at].valve.kind().channel;
            for index in before..count {
                if let Some(surface) = self.frame_surface(index) {
                    self.write_plane(
                        channel,
                        List::Frame(index),
                        &surface,
                        u32::from(timing.horizontal.active),
                        u32::from(timing.vertical.active),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// A frame no running channel composites or latches next.
    fn free_frame(&self) -> Result<usize, DriverError> {
        let mut busy = [false; FRAMES_MAX];
        for channel in self.running_channels() {
            for list in self.hvs.lists(channel)? {
                if let Some(List::Frame(index)) = list {
                    if let Some(slot) = busy.get_mut(index) {
                        *slot = true;
                    }
                }
            }
        }
        let count = self.frames.as_ref().map_or(0, Frames::len);
        (0..count)
            .find(|&index| !busy[index])
            .ok_or(DriverError::Busy)
    }
}

impl<H, V, C, A, D, L> Display for Engine<H, V, C, A, D, L>
where
    H: RegisterBlock,
    V: RegisterBlock,
    C: LinkCall,
    A: FrameAllocator,
    D: Delay,
    L: Recorder<Record>,
{
    fn output(&self) -> Option<DisplayMode> {
        self.surface
    }

    fn service_events(&mut self) -> Result<(), DriverError> {
        for at in 0..self.pipes.len() {
            let pipe = &mut self.pipes[at];
            if pipe.gone {
                continue;
            }
            match pipe.encoder.reap_wait() {
                Ok(None) => {}
                Ok(Some(state)) => {
                    pipe.seen = state.moves;
                    pipe.connector = state.connector;
                    pipe.phase = state.phase;
                    pipe.failed = false;
                }
                Err(_) => {
                    pipe.gone = true;
                    self.log.record(Record::EncoderGone {
                        encoder: pipe.name(),
                    });
                }
            }
            self.watch(at);
        }
        // An output that will not come up is the engine's to record; the
        // events themselves were read.
        let _ = self.reconcile();
        Ok(())
    }

    fn present(&mut self, frame: &[u8]) -> Result<(), DriverError> {
        let mode = self.surface.ok_or(DriverError::Stale)?;
        self.present_rects(frame, &[DamageRect::full(&mode)])
    }

    fn present_rects(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError> {
        let mode = self.surface.ok_or(DriverError::Stale)?;
        DamageRect::validate_list(damage, &mode)?;
        let len = u64::from(mode.stride_bytes) * u64::from(mode.height_px);
        if (frame.len() as u64) < len {
            return Err(DriverError::BufferTooSmall);
        }
        self.frames_for(mode)?;
        let target = self.free_frame()?;
        let frames = self.frames.as_mut().ok_or(DriverError::DeviceFault)?;
        frames.present(target, frame, damage)?;
        self.shows = Shows::Frames;
        self.latch_all()
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if power == self.power {
            return Ok(());
        }
        self.power = power;
        self.reconcile()
    }

    fn released(&mut self, surface: ReleaseSurface) {
        self.shows = match surface {
            ReleaseSurface::Text => Shows::Console,
            ReleaseSurface::Handover => Shows::Nothing,
        };
        if let Some(frames) = self.frames.as_mut() {
            frames.forget();
        }
        // A channel that cannot be moved off the departed presenter's frame
        // goes dark rather than go on showing it.
        if self.latch_all().is_err() {
            for at in 0..self.pipes.len() {
                if self.pipes[at].runs.is_some() {
                    let channel = Some(self.pipes[at].valve.kind().channel);
                    self.down_or_fail(at, channel);
                    self.pipes[at].failed = true;
                }
            }
        }
    }
}
