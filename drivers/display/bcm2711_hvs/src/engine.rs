//! The engine: the seat's display, driven through the HVS, the pixel valves
//! feeding the HDMI outputs, and the encoders behind them.
//!
//! The display's surface takes the mode of the first output with a sink it
//! can drive, in valve order; every other connected output shows the same
//! surface scaled to its own mode, its aspect kept. A present names buffers
//! the presenter filled, and each running channel is handed a list of one
//! plane per layer, with the pointer on top, in the stack slot it does not
//! composite; the present is queued until every one of them latches it at a
//! frame start. A mode set goes up and comes down in the order the encoder
//! holds its phases to, interleaved with the valve's and the channel's steps.

use tairix_abi::driver::clock;
use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, AcceleratedDisplay, AllocatedBuffer, BufferId, CursorCaps, DamageRect,
    Display, DisplayFormat, DisplayMode, DisplayPower, HardwareCursor, Presented, RefreshRate,
    Scan, Serviced, Timing, MAX_ACCEL_LAYERS,
};
use tairix_abi::driver::encoder::{Connector, EncoderOp, Phase, PipeLimits};
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::seat::ReleaseSurface;
use tairix_abi::{Delay, DriverError};
use tairix_display::{copy_rows, BufferMemory, DeviceBuffer, SharedBuffer};
use tairix_drvrt::Recorder;
use tairix_inline::ArrayVec;
use tairix_linkclient::{ClockClient, EncoderClient, LinkCall};

use crate::buffers::{carve, Buffers};
use crate::dlist::{
    lbm_words, narrower, plane, position, size, Alpha, Place, PlaneWords, Scaling, Surface,
    LBM_WORDS, UNITY_POINTER, UNITY_POSITION, UNITY_SIZE,
};
use crate::hvs::{Hvs, List, KERNEL_AT, STACKS, STACK_PLANES};
use crate::regs::hvs::CHANNELS;
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

/// The pointer's side at most, and so its images'.
const CURSOR_SIDE: u32 = 64;

/// Bytes from one line of a pointer image to the next.
const CURSOR_STRIDE: u32 = CURSOR_SIDE * 4;

/// Pointer images held: the clear one shown while the pointer is hidden, and
/// room for the one listed and those a list may still be read from.
const CURSOR_IMAGES: usize = 5;

/// Frame ends every running channel sees before an image a list stopped
/// naming is no longer read: the frame that latched the change may already
/// have been under way.
const RETIRE_FRAMES: u64 = 2;

/// The largest buffer side a plane's fields hold.
const SIDE_MAX: u32 = (1 << 13) - 1;

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
    /// Most bytes one presenter's buffers may hold together: a share of the
    /// memory the HVS reaches.
    pub budget: usize,
    /// The timed waits a mode set takes.
    pub delay: D,
    /// Where decisions are recorded.
    pub log: L,
}

/// One layer of the stack the outputs show: part of a buffer, placed on the
/// surface.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shown {
    buffer: BufferId,
    src: DamageRect,
    dst: (i32, i32, u32, u32),
    opacity: u8,
}

/// A stack's list for one output, built and not yet written.
struct Built {
    planes: ArrayVec<PlaneWords, STACK_PLANES>,
    named: ArrayVec<BufferId, MAX_ACCEL_LAYERS>,
    /// Which of `planes` is the pointer's.
    pointer_plane: usize,
}

/// What one channel's stack slots hold, and what it is about to latch.
#[derive(Clone, Default)]
struct Slots {
    /// The buffers each slot's list names.
    named: [ArrayVec<BufferId, MAX_ACCEL_LAYERS>; STACKS],
    /// Where each slot's pointer plane starts.
    pointer_at: [Option<usize>; STACKS],
    /// The slot a queued present waits for this channel to latch.
    waiting: Option<usize>,
    /// Frame ends this channel has seen.
    ends: u64,
}

/// A pointer image the HVS reads, and the frame ends after which no list can
/// still be read from it, once one stopped naming it.
struct Image<M> {
    memory: M,
    bus: u32,
    retire: Option<[u64; OUTPUTS_MAX]>,
}

/// The pointer: its images, the one the lists show, and where.
struct Cursor<M> {
    /// The clear image first, shown while the pointer is hidden.
    images: ArrayVec<Image<M>, CURSOR_IMAGES>,
    listed: usize,
    size: (u32, u32),
    hot: (u32, u32),
    at: (i32, i32),
    shown: bool,
}

/// The display engine.
pub struct Engine<H, V, C: LinkCall, A: BufferMemory, D, L> {
    hvs: Hvs<H>,
    pipes: ArrayVec<Pipe<V, C>, OUTPUTS_MAX>,
    clock: ClockClient<C>,
    core_max_hz: u64,
    floor_hz: u64,
    /// The floor the clock last refused, not asked again until another is
    /// needed.
    floor_refused: Option<u64>,
    surface: Option<DisplayMode>,
    /// The rate of the timing the surface is shaped for.
    refresh: Option<RefreshRate>,
    buffers: Buffers<A::Region>,
    alloc: A,
    /// Most bytes the presenter's buffers may hold together.
    budget: usize,
    console: Option<Surface>,
    shows: Shows,
    /// The layers the last present asked for, bottom first.
    stack: ArrayVec<Shown, MAX_ACCEL_LAYERS>,
    /// The stack every running output last latched: what they composite
    /// while a later one waits.
    latched: ArrayVec<Shown, MAX_ACCEL_LAYERS>,
    slots: [Slots; OUTPUTS_MAX],
    cursor: Cursor<A::Region>,
    /// Whether the channels raise an interrupt at each frame end.
    watching: bool,
    /// An output a queued present waited on was brought down, so it latched
    /// there.
    wait_dropped: bool,
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
    let (width, height) = timing.active_size();
    DisplayMode {
        width_px: width,
        height_px: height,
        stride_bytes: (width * 4).next_multiple_of(STRIDE_ALIGN),
        format: DisplayFormat::Bgra8888,
    }
}

impl<H, V, C, A, D, L> Engine<H, V, C, A, D, L>
where
    H: RegisterBlock,
    V: RegisterBlock,
    C: LinkCall,
    A: BufferMemory,
    A::Region: DeviceBuffer,
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
            floor_refused: None,
            surface: None,
            refresh: None,
            buffers: Buffers::new(),
            alloc: parts.alloc,
            budget: parts.budget,
            console: parts.console,
            shows: if parts.console.is_some() {
                Shows::Console
            } else {
                Shows::Nothing
            },
            stack: ArrayVec::new(),
            latched: ArrayVec::new(),
            slots: Default::default(),
            cursor: Cursor {
                images: ArrayVec::new(),
                listed: 0,
                size: (1, 1),
                hot: (0, 0),
                at: (0, 0),
                shown: false,
            },
            watching: false,
            wait_dropped: false,
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
        // A channel stopped reads nothing, so nothing it named is in use and
        // a present waiting on it has latched as far as it ever will.
        let ends = self.slots[at].ends;
        self.wait_dropped |= self.slots[at].waiting.is_some();
        self.slots[at] = Slots {
            ends,
            ..Slots::default()
        };
        first
    }

    /// The line buffer channel `channel` scales through.
    fn lbm_at(channel: usize) -> Result<u32, DriverError> {
        u32::try_from(channel * LBM_SHARE).map_err(|_| DriverError::OutOfRange)
    }

    /// The plane showing `surface` on an output of `width` by `height` as the
    /// console is shown: fitted, its aspect kept, opaque.
    fn fitted_plane(
        channel: usize,
        surface: &Surface,
        width: u32,
        height: u32,
    ) -> Result<PlaneWords, DriverError> {
        let place = Place::fit(surface.width, surface.height, width, height);
        let x = Scaling::of(surface.width, place.width);
        let y = Scaling::of(surface.height, place.height);
        if lbm_words(surface.width, x, y, place.width) as usize > LBM_SHARE {
            return Err(DriverError::OutOfRange);
        }
        plane(
            surface,
            &place,
            (kernel_at()?, Self::lbm_at(channel)?),
            Alpha::Opaque,
        )
    }

    /// Write every fixed list of pipe `at`, whose valve runs `timing`.
    fn write_lists(&self, at: usize, timing: &Timing) -> Result<(), DriverError> {
        let channel = self.pipes[at].valve.kind().channel;
        let (width, height) = timing.active_size();
        self.hvs.write_slot(channel, List::Black, &[])?;
        if let Some(console) = self.console {
            let words = Self::fitted_plane(channel, &console, width, height)?;
            self.hvs.write_slot(channel, List::Console, &words)?;
        }
        Ok(())
    }

    /// Hand pipe `at`, whose valve runs `timing`, what the outputs show: the
    /// stack written into the slot its channel does not composite, answering
    /// that slot, or the console or black.
    fn show_on(&mut self, at: usize, timing: &Timing) -> Result<Option<usize>, DriverError> {
        let channel = self.pipes[at].valve.kind().channel;
        match self.shows {
            Shows::Console if self.console.is_some() => {
                self.hvs.latch(channel, List::Console)?;
                Ok(None)
            }
            // An output coming up that cannot show the stack shows black, and
            // the presenter's next stack is built for it with the others.
            Shows::Frames if !self.stack.is_empty() => {
                if let Ok((built, index)) = self
                    .build_stack(at, timing)
                    .and_then(|built| Ok((built, self.free_slot(at)?)))
                {
                    self.latch_built(at, index, built).map(Some)
                } else {
                    self.hvs.latch(channel, List::Black)?;
                    Ok(None)
                }
            }
            _ => {
                self.hvs.latch(channel, List::Black)?;
                Ok(None)
            }
        }
    }

    /// The stack's list for pipe `at`: one plane per layer, placed on its
    /// output as the surface is, then the pointer. Everything a stack can be
    /// refused for is found here, before any list is written.
    fn build_stack(&mut self, at: usize, timing: &Timing) -> Result<Built, DriverError> {
        let surface = self.surface.ok_or(DriverError::Stale)?;
        let channel = self.pipes[at].valve.kind().channel;
        let output = timing.active_size();
        let fit = Place::fit(surface.width_px, surface.height_px, output.0, output.1);
        let mut planes: ArrayVec<PlaneWords, STACK_PLANES> = ArrayVec::new();
        let mut named: ArrayVec<BufferId, MAX_ACCEL_LAYERS> = ArrayVec::new();
        let mut lbm = 0usize;
        for layer in &self.stack {
            let held = self
                .buffers
                .get(layer.buffer)
                .ok_or(DriverError::NotFound)?;
            let Some((source, place)) = crop(layer, (held.bus, held.stride), &surface, &fit)?
            else {
                continue;
            };
            let x = Scaling::of(source.width, place.width);
            let y = Scaling::of(source.height, place.height);
            let words = lbm_words(source.width, x, y, place.width) as usize;
            let base =
                u32::try_from(channel * LBM_SHARE + lbm).map_err(|_| DriverError::OutOfRange)?;
            lbm += words;
            if lbm > LBM_SHARE {
                return Err(DriverError::OutOfRange);
            }
            planes
                .try_push(plane(
                    &source,
                    &place,
                    (kernel_at()?, base),
                    Alpha::Premultiplied(layer.opacity),
                )?)
                .map_err(|_| DriverError::OutOfRange)?;
            named
                .try_push(layer.buffer)
                .map_err(|_| DriverError::OutOfRange)?;
        }
        let pointer = self.pointer_plane(&fit)?;
        let pointer_plane = planes.len();
        planes
            .try_push(pointer)
            .map_err(|_| DriverError::OutOfRange)?;
        Ok(Built {
            planes,
            named,
            pointer_plane,
        })
    }

    /// The pointer's plane on an output the surface is placed on at `fit`:
    /// unscaled, cut to the output, or the clear image where it is hidden.
    fn pointer_plane(&mut self, fit: &Place) -> Result<PlaneWords, DriverError> {
        self.ensure_clear_image()?;
        let (surface, place) = self.pointer_at(fit);
        plane(
            &surface,
            &place,
            (kernel_at()?, 0),
            Alpha::Premultiplied(u8::MAX),
        )
    }

    /// Where the pointer's plane reads and lands on an output the surface is
    /// placed on at `fit`.
    fn pointer_at(&self, fit: &Place) -> (Surface, Place) {
        let clear = (
            Surface {
                bus: self.cursor.images.first().map_or(0, |image| image.bus),
                stride: CURSOR_STRIDE,
                width: 1,
                height: 1,
                format: DisplayFormat::Bgra8888,
            },
            Place {
                x: fit.x,
                y: fit.y,
                width: 1,
                height: 1,
            },
        );
        let Some(image) = self
            .cursor
            .images
            .get(self.cursor.listed)
            .filter(|_| self.cursor.shown)
        else {
            return clear;
        };
        let surface = self.surface.unwrap_or(DisplayMode {
            width_px: 1,
            height_px: 1,
            stride_bytes: 4,
            format: DisplayFormat::Bgra8888,
        });
        // The hot spot lands where the surface's point does on this output;
        // the image itself is drawn at its own size.
        let scale = |value: i32, from: u32, to: u32| {
            i64::from(value) * i64::from(to) / i64::from(from.max(1))
        };
        let left = i64::from(fit.x) + scale(self.cursor.at.0, surface.width_px, fit.width)
            - i64::from(self.cursor.hot.0);
        let top = i64::from(fit.y) + scale(self.cursor.at.1, surface.height_px, fit.height)
            - i64::from(self.cursor.hot.1);
        let (width, height) = self.cursor.size;
        let right = (left + i64::from(width)).min(i64::from(fit.x + fit.width));
        let bottom = (top + i64::from(height)).min(i64::from(fit.y + fit.height));
        let (from_x, from_y) = (left.max(i64::from(fit.x)), top.max(i64::from(fit.y)));
        let (Ok(cut_x), Ok(cut_y), Ok(seen_w), Ok(seen_h), Ok(x), Ok(y)) = (
            u32::try_from(from_x - left),
            u32::try_from(from_y - top),
            u32::try_from(right - from_x),
            u32::try_from(bottom - from_y),
            u32::try_from(from_x),
            u32::try_from(from_y),
        ) else {
            return clear;
        };
        if seen_w == 0 || seen_h == 0 {
            return clear;
        }
        (
            Surface {
                bus: image.bus + cut_y * CURSOR_STRIDE + cut_x * 4,
                stride: CURSOR_STRIDE,
                width: seen_w,
                height: seen_h,
                format: DisplayFormat::Bgra8888,
            },
            Place {
                x,
                y,
                width: seen_w,
                height: seen_h,
            },
        )
    }

    /// Bring the pointer's plane to the pointer as it now is, in place: in
    /// every list a running channel reads, or — while a present is queued on
    /// it — in the list it latches next alone, so the pointer reaches the
    /// screen with the frame that moves what it drags rather than ahead of it.
    fn rewrite_pointer(&self) -> Result<(), DriverError> {
        let Some(surface) = self.surface else {
            return Ok(());
        };
        for (at, pipe) in self.pipes.iter().enumerate() {
            let Some(timing) = pipe.runs else { continue };
            let output = timing.active_size();
            let fit = Place::fit(surface.width_px, surface.height_px, output.0, output.1);
            let (source, place) = self.pointer_at(&fit);
            let slots = &self.slots[at];
            let read = slots
                .pointer_at
                .iter()
                .enumerate()
                .filter_map(|(index, start)| {
                    slots
                        .waiting
                        .is_none_or(|waiting| waiting == index)
                        .then_some(*start)?
                });
            for start in read {
                self.rewrite_pointer_plane(start, &source, &place)?;
            }
        }
        Ok(())
    }

    /// Point the pointer plane at word `start` at `source`, landing at
    /// `place`. A changed size is narrowed to fit both the old and the new
    /// read first, so no state between the writes reads past either image.
    fn rewrite_pointer_plane(
        &self,
        start: usize,
        source: &Surface,
        place: &Place,
    ) -> Result<(), DriverError> {
        let next = size(source);
        let current = self.hvs.read_list(start + UNITY_SIZE)?;
        if current != next {
            self.hvs
                .write_list(start + UNITY_SIZE, &[narrower(current, next)])?;
        }
        self.hvs
            .write_list(start + UNITY_POSITION, &[position(place)])?;
        self.hvs.write_list(start + UNITY_POINTER, &[source.bus])?;
        if current != next {
            self.hvs.write_list(start + UNITY_SIZE, &[next])?;
        }
        Ok(())
    }

    /// Hold the clear pointer image every stack's pointer plane falls back to.
    fn ensure_clear_image(&mut self) -> Result<(), DriverError> {
        if !self.cursor.images.is_empty() {
            return Ok(());
        }
        let (memory, bus) = carve(
            &mut self.alloc,
            CURSOR_STRIDE as usize * CURSOR_SIDE as usize,
        )?;
        self.cursor
            .images
            .try_push(Image {
                memory,
                bus,
                retire: None,
            })
            .map_err(|_| DriverError::NoSpace)
    }

    /// A pointer image no list names or may still be read from, held anew
    /// where every one is.
    fn free_image(&mut self) -> Result<usize, DriverError> {
        let listed = self.cursor.listed;
        let free = (1..self.cursor.images.len()).find(|&index| {
            !(index == listed && self.cursor.shown)
                && self.cursor.images[index]
                    .retire
                    .is_none_or(|stamp| self.passed(&stamp))
        });
        if let Some(index) = free {
            self.cursor.images[index].retire = None;
            return Ok(index);
        }
        let (memory, bus) = carve(
            &mut self.alloc,
            CURSOR_STRIDE as usize * CURSOR_SIDE as usize,
        )?;
        self.cursor
            .images
            .try_push(Image {
                memory,
                bus,
                retire: None,
            })
            .map_err(|_| DriverError::Busy)?;
        Ok(self.cursor.images.len() - 1)
    }

    /// The frame ends each pipe has seen.
    fn ends(&self) -> [u64; OUTPUTS_MAX] {
        let mut ends = [0; OUTPUTS_MAX];
        for (end, slots) in ends.iter_mut().zip(&self.slots) {
            *end = slots.ends;
        }
        ends
    }

    /// Whether every running channel has seen the frame ends `stamp` waits
    /// for; one that is not running reads nothing.
    fn passed(&self, stamp: &[u64; OUTPUTS_MAX]) -> bool {
        self.pipes
            .iter()
            .zip(&self.slots)
            .zip(stamp)
            .all(|((pipe, slots), wanted)| pipe.runs.is_none() || slots.ends >= *wanted)
    }

    /// The listed image no list will name from the next frame: what it may
    /// still be read until.
    fn retire_listed(&mut self) {
        let stamp = self.ends().map(|ends| ends + RETIRE_FRAMES);
        let listed = self.cursor.listed;
        if listed != 0 {
            if let Some(image) = self.cursor.images.get_mut(listed) {
                image.retire = Some(stamp);
            }
        }
    }

    /// The buffers a list a running channel composites or latches next
    /// names.
    fn named_buffers(
        &self,
    ) -> Result<ArrayVec<BufferId, { 2 * OUTPUTS_MAX * MAX_ACCEL_LAYERS }>, DriverError> {
        let mut named = ArrayVec::new();
        for (at, pipe) in self.pipes.iter().enumerate() {
            if pipe.runs.is_none() {
                continue;
            }
            for list in self
                .hvs
                .lists(pipe.valve.kind().channel)?
                .into_iter()
                .flatten()
            {
                if let List::Stack(index) = list {
                    for &id in &self.slots[at].named[index] {
                        let _ = named.try_push(id);
                    }
                }
            }
        }
        Ok(named)
    }

    /// Whether anything waits for a frame to end: a queued present, a pointer
    /// image a list may still read, or a buffer given back that one names.
    fn waits_on_frames(&self) -> bool {
        self.slots.iter().any(|slots| slots.waiting.is_some())
            || self.buffers.any_doomed()
            || self
                .cursor
                .images
                .iter()
                .any(|image| image.retire.is_some_and(|stamp| !self.passed(&stamp)))
    }

    /// Have the running channels raise frame ends exactly while something
    /// waits on one.
    fn watch_frames(&mut self) -> Result<(), DriverError> {
        let wanted = self.waits_on_frames();
        if wanted == self.watching {
            return Ok(());
        }
        for channel in 0..CHANNELS {
            self.hvs.watch_frames(channel, wanted)?;
        }
        self.watching = wanted;
        Ok(())
    }

    /// Count the frames that ended, settle what waited on them, and answer
    /// whether a queued present has latched on every channel it went to.
    fn note_frames(&mut self) -> Result<bool, DriverError> {
        let ended = self.hvs.take_frame_ends()?;
        let mut waited = core::mem::take(&mut self.wait_dropped);
        let mut still = false;
        for at in 0..self.pipes.len() {
            let channel = self.pipes[at].valve.kind().channel;
            if ended.get(channel).copied().unwrap_or(false) {
                self.slots[at].ends += 1;
            }
            if let Some(index) = self.slots[at].waiting {
                waited = true;
                let running = self.pipes[at].runs.is_some();
                if !running || self.hvs.lists(channel)?[0] == Some(List::Stack(index)) {
                    self.slots[at].waiting = None;
                } else {
                    still = true;
                }
            }
        }
        let named = self.named_buffers()?;
        self.buffers.reap(|id| named.contains(&id));
        if waited && !still {
            self.latched.clone_from(&self.stack);
            self.settle_floor();
        }
        self.watch_frames()?;
        Ok(waited && !still)
    }

    /// Show `stack`, at least one layer, queued until every running channel
    /// latches it.
    fn present_stack(
        &mut self,
        stack: ArrayVec<Shown, MAX_ACCEL_LAYERS>,
    ) -> Result<Presented, DriverError> {
        if self.slots.iter().any(|slots| slots.waiting.is_some()) {
            return Err(DriverError::Busy);
        }
        let shown = (core::mem::replace(&mut self.stack, stack), self.shows);
        self.shows = Shows::Frames;
        // Every running output's list is built before any is latched, so a
        // stack one output cannot take leaves every output showing what it did.
        let mut built: ArrayVec<(usize, usize, Built), OUTPUTS_MAX> = ArrayVec::new();
        for at in 0..self.pipes.len() {
            let Some(timing) = self.pipes[at].runs else {
                continue;
            };
            match self
                .free_slot(at)
                .and_then(|index| Ok((index, self.build_stack(at, &timing)?)))
            {
                Ok((index, list)) => {
                    let _ = built.try_push((at, index, list));
                }
                Err(refused) => {
                    (self.stack, self.shows) = shown;
                    return Err(refused);
                }
            }
        }
        // The clock the new stack composites at is had before a channel
        // reads it.
        self.raise_floor();
        let mut queued = false;
        let mut latched = Ok(());
        for (at, index, list) in built {
            match self.latch_built(at, index, list) {
                Ok(index) => {
                    self.slots[at].waiting = Some(index);
                    queued = true;
                }
                Err(reason) => {
                    latched = Err(reason);
                    break;
                }
            }
        }
        // An output already latched still has its frame end watched.
        self.watch_frames()?;
        latched?;
        if queued {
            Ok(Presented::Queued)
        } else {
            self.latched.clone_from(&self.stack);
            Ok(Presented::Shown)
        }
    }

    /// The stack slot of pipe `at`'s channel that neither the list it
    /// composites nor the one it latches next names: the only one that may be
    /// written while the channel runs.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] while both are stack slots, until the next frame
    /// starts.
    fn free_slot(&self, at: usize) -> Result<usize, DriverError> {
        let lists = self.hvs.lists(self.pipes[at].valve.kind().channel)?;
        (0..STACKS)
            .find(|&index| !lists.contains(&Some(List::Stack(index))))
            .ok_or(DriverError::Busy)
    }

    /// Write `built` into pipe `at`'s free stack slot `index`, record what it
    /// names, and latch it, answering the slot.
    fn latch_built(&mut self, at: usize, index: usize, built: Built) -> Result<usize, DriverError> {
        let channel = self.pipes[at].valve.kind().channel;
        let starts = self.hvs.write_stack(channel, index, &built.planes)?;
        let slots = &mut self.slots[at];
        slots.named[index] = built.named;
        slots.pointer_at[index] = starts.get(built.pointer_plane).copied();
        self.hvs.latch(channel, List::Stack(index))?;
        Ok(index)
    }

    /// Bring the core clock to what the running outputs composite, once no
    /// channel still composites a stack it is to latch away from: lowering it
    /// earlier would starve the larger one.
    fn settle_floor(&mut self) {
        if self.slots.iter().any(|slots| slots.waiting.is_some()) {
            return;
        }
        let mut running = [None; OUTPUTS_MAX];
        for (runs, pipe) in running.iter_mut().zip(&self.pipes) {
            *runs = pipe.runs;
        }
        let settled = self.needed_hz(&running[..self.pipes.len()]);
        self.floor(settled);
    }

    /// Raise the core clock to what the stack now composites, where that is
    /// more than it runs at; lowering waits for the outputs to settle.
    fn raise_floor(&mut self) {
        let mut running = [None; OUTPUTS_MAX];
        for (runs, pipe) in running.iter_mut().zip(&self.pipes) {
            *runs = pipe.runs;
        }
        let needed = self.needed_hz(&running[..self.pipes.len()]);
        if needed > self.floor_hz {
            self.floor(needed);
        }
    }

    /// Bring pipe `at` up running `timing`.
    fn bring_up(&mut self, at: usize, timing: Timing) -> Result<(), DriverError> {
        let kind = *self.pipes[at].valve.kind();
        let (width, height) = timing.active_size();
        self.write_lists(at, &timing)?;
        self.hvs.mux(kind.output, Some(kind.channel))?;
        self.hvs
            .start(kind.channel, width, height, timing.scan == Scan::Interlaced)?;
        let _ = self.show_on(at, &timing)?;
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
            width,
            height,
            refresh_mhz: timing.refresh_mhz(),
        });
        Ok(())
    }

    /// The core clock the pipes running, or about to run, `timings` need:
    /// the most any one output loads it, or the share of every plane's
    /// pixels it composites, as Linux's `vc4_core_clock_atomic_check` has it.
    fn needed_hz(&self, timings: &[Option<Timing>]) -> u64 {
        let surface = self.surface;
        let (width, height) = surface.map_or((0, 0), |mode| (mode.width_px, mode.height_px));
        let surface_area = (u64::from(width) * u64::from(height)).max(1);
        // Every layer is composited, so the stack loads the core by its
        // layers' area, never less than the one plane the surface is.
        let stacked = self
            .stack
            .iter()
            .map(|layer| u64::from(layer.dst.2) * u64::from(layer.dst.3))
            .sum::<u64>()
            .max(surface_area);
        let mut most = 0;
        let mut pixels = 0;
        let mut running = 0;
        for (pipe, timing) in self.pipes.iter().zip(timings) {
            let Some(timing) = timing else { continue };
            running += 1;
            most = most.max(output_load_hz(pipe.valve.kind().load, timing));
            let (out_width, out_height) = timing.active_size();
            let place = Place::fit(width, height, out_width, out_height);
            let placed = u64::from(place.width) * u64::from(place.height);
            pixels += placed * stacked / surface_area * u64::from(timing.refresh_mhz()) / 1_000;
        }
        let share = if running > 1 { 40 } else { 60 };
        most.max(pixels * share / 100)
    }

    /// Hold the core clock at no less than `hz`, within what it can run at.
    fn floor(&mut self, hz: u64) {
        let hz = hz.min(self.core_max_hz);
        if hz == self.floor_hz || self.floor_refused == Some(hz) {
            return;
        }
        let result = if hz == 0 {
            self.clock.release()
        } else {
            self.clock.floor(hz).map(|_| ())
        };
        match result {
            Ok(()) => {
                self.floor_hz = hz;
                self.floor_refused = None;
            }
            Err(errno) => {
                self.floor_refused = Some(hz);
                self.log.record(Record::ClockRefused {
                    hz,
                    reason: clock::refusal(errno),
                });
            }
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
        let shaping = self.pipes.iter().find_map(Pipe::wanted);
        self.refresh = shaping.as_ref().and_then(RefreshRate::of);
        let surface = shaping.map(|t| surface_of(&t));
        if surface != self.surface {
            for at in 0..self.pipes.len() {
                if self.pipes[at].runs.is_some() {
                    let channel = Some(self.pipes[at].valve.kind().channel);
                    self.down_or_fail(at, channel);
                }
            }
            // The presenter's buffers are for the surface that went; nothing
            // of them is shown on the next.
            self.stack.clear();
            self.latched.clear();
            if self.shows == Shows::Frames {
                self.shows = Shows::Nothing;
            }
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
        self.settle_floor();
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
}

/// The word offset of the filter kernel, as a plane's field holds it.
fn kernel_at() -> Result<u32, DriverError> {
    u32::try_from(KERNEL_AT).map_err(|_| DriverError::OutOfRange)
}

/// Where `layer` reads its buffer, at `bus` and `stride` bytes a line, and
/// where it lands on an output the surface is placed on at `fit`: cut to the
/// surface, then scaled with it. `None` for a layer that lands nowhere.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a layer whose cut no address holds.
fn crop(
    layer: &Shown,
    (bus, stride): (u32, u32),
    surface: &DisplayMode,
    fit: &Place,
) -> Result<Option<(Surface, Place)>, DriverError> {
    let (dx, dy, dw, dh) = layer.dst;
    let (left, top) = (i64::from(dx), i64::from(dy));
    let (right, bottom) = (left + i64::from(dw), top + i64::from(dh));
    let (seen_left, seen_top) = (left.max(0), top.max(0));
    let seen_right = right.min(i64::from(surface.width_px));
    let seen_bottom = bottom.min(i64::from(surface.height_px));
    if seen_right <= seen_left || seen_bottom <= seen_top {
        return Ok(None);
    }
    // The source is cut in proportion, so a scaled layer keeps its scale.
    let src = &layer.src;
    let along = |cut: i64, extent: u32, src_extent: u32| {
        cut * i64::from(src_extent) / i64::from(extent.max(1))
    };
    let src_left = along(seen_left - left, dw, src.width_px);
    let src_top = along(seen_top - top, dh, src.height_px);
    let src_right = i64::from(src.width_px) - along(right - seen_right, dw, src.width_px);
    let src_bottom = i64::from(src.height_px) - along(bottom - seen_bottom, dh, src.height_px);
    let narrow = |value: i64| u32::try_from(value).map_err(|_| DriverError::OutOfRange);
    let (from_x, from_y) = (narrow(src_left)? + src.x, narrow(src_top)? + src.y);
    let (src_w, src_h) = (narrow(src_right - src_left)?, narrow(src_bottom - src_top)?);
    if src_w == 0 || src_h == 0 {
        return Ok(None);
    }
    let offset = u64::from(from_y) * u64::from(stride) + u64::from(from_x) * 4;
    let bus = u32::try_from(u64::from(bus) + offset).map_err(|_| DriverError::OutOfRange)?;
    // The surface's point lands on the output where `fit` scales it to.
    let to_x = |value: i64| {
        i64::from(fit.x) + value * i64::from(fit.width) / i64::from(surface.width_px.max(1))
    };
    let to_y = |value: i64| {
        i64::from(fit.y) + value * i64::from(fit.height) / i64::from(surface.height_px.max(1))
    };
    let (x, y) = (to_x(seen_left), to_y(seen_top));
    let (width, height) = (to_x(seen_right) - x, to_y(seen_bottom) - y);
    if width <= 0 || height <= 0 {
        return Ok(None);
    }
    Ok(Some((
        Surface {
            bus,
            stride,
            width: src_w,
            height: src_h,
            format: surface.format,
        },
        Place {
            x: narrow(x)?,
            y: narrow(y)?,
            width: narrow(width)?,
            height: narrow(height)?,
        },
    )))
}

impl<H, V, C, A, D, L> Display for Engine<H, V, C, A, D, L>
where
    H: RegisterBlock,
    V: RegisterBlock,
    C: LinkCall,
    A: BufferMemory,
    A::Region: DeviceBuffer,
    D: Delay,
    L: Recorder<Record>,
{
    fn output(&self) -> Option<DisplayMode> {
        self.surface
    }

    fn refresh(&self) -> Option<RefreshRate> {
        self.refresh
    }

    fn service_events(&mut self) -> Result<Serviced, DriverError> {
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
        let latched = self.note_frames()?;
        Ok(Serviced { latched })
    }

    fn frame_count(&self) -> u32 {
        // The HVS reads the frame it shows, so the presenter draws the next
        // into another while one present is queued.
        2
    }

    fn abandon_queued(&mut self) {
        let mut withdrawn = false;
        for at in 0..self.pipes.len() {
            if self.slots[at].waiting.take().is_none() {
                continue;
            }
            withdrawn = true;
            // The list it never latched is withdrawn before the slot can be
            // written again under a channel that may yet pick it up.
            let channel = self.pipes[at].valve.kind().channel;
            if let Ok([Some(active), _]) = self.hvs.lists(channel) {
                let _ = self.hvs.latch(channel, active);
            }
        }
        // The outputs go on compositing the stack they latched last, which is
        // what an output coming up shows and the clock is held to, with the
        // pointer where it went while the present waited.
        if withdrawn {
            self.stack.clone_from(&self.latched);
            let _ = self.rewrite_pointer();
        }
        // What only the withdrawn lists named is free to go.
        if let Ok(named) = self.named_buffers() {
            self.buffers.reap(|id| named.contains(&id));
        }
        let _ = self.watch_frames();
    }

    fn alloc_buffer(
        &mut self,
        width_px: u32,
        height_px: u32,
    ) -> Result<AllocatedBuffer, DriverError> {
        if width_px == 0 || height_px == 0 || width_px > SIDE_MAX || height_px > SIDE_MAX {
            return Err(DriverError::LengthOutOfRange);
        }
        let stride = (width_px * 4).next_multiple_of(STRIDE_ALIGN);
        self.buffers
            .alloc(&mut self.alloc, (width_px, height_px), stride, self.budget)
    }

    fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError> {
        let named = self.named_buffers()?;
        self.buffers.free(id, |id| named.contains(&id))?;
        self.watch_frames()
    }

    fn present(&mut self, id: BufferId, damage: &[DamageRect]) -> Result<Presented, DriverError> {
        let mode = self.surface.ok_or(DriverError::Stale)?;
        DamageRect::validate_list(damage, &mode)?;
        let held = self.buffers.get(id).ok_or(DriverError::NotFound)?;
        if (held.width, held.height) != (mode.width_px, mode.height_px) {
            return Err(DriverError::Stale);
        }
        let whole = DamageRect::full(&mode);
        let mut stack = ArrayVec::new();
        stack
            .try_push(Shown {
                buffer: id,
                src: whole,
                dst: (0, 0, mode.width_px, mode.height_px),
                opacity: u8::MAX,
            })
            .map_err(|_| DriverError::OutOfRange)?;
        self.present_stack(stack)
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if power == self.power {
            return Ok(());
        }
        self.power = power;
        let reconciled = self.reconcile();
        let named = self.named_buffers()?;
        self.buffers.reap(|id| named.contains(&id));
        self.watch_frames()?;
        reconciled
    }

    fn released(&mut self, surface: ReleaseSurface) {
        self.shows = match surface {
            ReleaseSurface::Text => Shows::Console,
            ReleaseSurface::Handover => Shows::Nothing,
        };
        // What a departed presenter left must never be shown to the next.
        self.stack.clear();
        self.latched.clear();
        self.cursor.shown = false;
        for slots in &mut self.slots {
            slots.waiting = None;
        }
        let mut moved = true;
        for at in 0..self.pipes.len() {
            if let Some(timing) = self.pipes[at].runs {
                moved &= self.show_on(at, &timing).is_ok();
            }
        }
        // A channel that cannot be moved off the departed presenter's frame
        // goes dark rather than go on showing it.
        if !moved {
            for at in 0..self.pipes.len() {
                if self.pipes[at].runs.is_some() {
                    let channel = Some(self.pipes[at].valve.kind().channel);
                    self.down_or_fail(at, channel);
                    self.pipes[at].failed = true;
                }
            }
        }
        // The buffers it leaves are given back once no list names them.
        let _ = self.watch_frames();
    }

    fn as_accelerated(&mut self) -> Option<&mut dyn AcceleratedDisplay> {
        Some(self)
    }

    fn hardware_cursor(&mut self) -> Option<&mut dyn HardwareCursor> {
        Some(self)
    }
}

impl<H, V, C, A, D, L> AcceleratedDisplay for Engine<H, V, C, A, D, L>
where
    H: RegisterBlock,
    V: RegisterBlock,
    C: LinkCall,
    A: BufferMemory,
    A::Region: DeviceBuffer,
    D: Delay,
    L: Recorder<Record>,
{
    fn accel_caps(&self) -> AccelCaps {
        // A stack slot holds every layer's plane and the pointer's, and each
        // plane's fields bound its sides.
        AccelCaps {
            max_layers: u32::try_from(MAX_ACCEL_LAYERS).unwrap_or(u32::MAX),
            max_width_px: SIDE_MAX,
            max_height_px: SIDE_MAX,
            per_layer_opacity: true,
            hw_scaling: true,
        }
    }

    fn present_layers(&mut self, layers: &[AccelLayer]) -> Result<Presented, DriverError> {
        if self.surface.is_none() {
            return Err(DriverError::Stale);
        }
        let caps = self.accel_caps();
        if layers.is_empty() || layers.len() > caps.max_layers as usize {
            return Err(DriverError::LengthOutOfRange);
        }
        let mut stack = ArrayVec::new();
        for layer in layers {
            let held = self
                .buffers
                .get(layer.buffer)
                .ok_or(DriverError::NotFound)?;
            layer.validate(&caps, held.width, held.height)?;
            stack
                .try_push(Shown {
                    buffer: layer.buffer,
                    src: layer.src,
                    dst: (
                        layer.dst_x,
                        layer.dst_y,
                        layer.dst_width_px,
                        layer.dst_height_px,
                    ),
                    opacity: layer.opacity,
                })
                .map_err(|_| DriverError::LengthOutOfRange)?;
        }
        self.present_stack(stack)
    }
}

impl<H, V, C, A, D, L> HardwareCursor for Engine<H, V, C, A, D, L>
where
    H: RegisterBlock,
    V: RegisterBlock,
    C: LinkCall,
    A: BufferMemory,
    A::Region: DeviceBuffer,
    D: Delay,
    L: Recorder<Record>,
{
    fn cursor_caps(&self) -> CursorCaps {
        CursorCaps {
            max_width_px: CURSOR_SIDE,
            max_height_px: CURSOR_SIDE,
        }
    }

    fn set_cursor(&mut self, id: BufferId, hot_x: u32, hot_y: u32) -> Result<(), DriverError> {
        let (width, height, stride) = {
            let held = self.buffers.get(id).ok_or(DriverError::NotFound)?;
            (held.width, held.height, held.stride)
        };
        if width > CURSOR_SIDE || height > CURSOR_SIDE || hot_x >= width || hot_y >= height {
            return Err(DriverError::LengthOutOfRange);
        }
        self.ensure_clear_image()?;
        let index = self.free_image()?;
        // The image is the device's once copied, so the presenter may draw
        // the next into its buffer at once.
        let (Some(held), Some(image)) = (self.buffers.get(id), self.cursor.images.get_mut(index))
        else {
            return Err(DriverError::NotFound);
        };
        held.memory.with_bytes(|source| {
            image.memory.with_bytes_mut(|target| {
                copy_rows(
                    source,
                    stride as usize,
                    target,
                    CURSOR_STRIDE as usize,
                    width as usize * 4,
                    height as usize,
                )
            })
        })?;
        if self.cursor.shown {
            self.retire_listed();
        }
        self.cursor.listed = index;
        self.cursor.size = (width, height);
        self.cursor.hot = (hot_x, hot_y);
        self.cursor.shown = true;
        self.rewrite_pointer()?;
        self.watch_frames()
    }

    fn move_cursor(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
        self.cursor.at = (x, y);
        if self.cursor.shown {
            self.rewrite_pointer()?;
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), DriverError> {
        if !self.cursor.shown {
            return Ok(());
        }
        self.retire_listed();
        self.cursor.shown = false;
        self.rewrite_pointer()?;
        self.watch_frames()
    }
}
