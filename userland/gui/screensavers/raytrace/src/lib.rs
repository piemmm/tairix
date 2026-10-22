//! Raytrace, a TAIRiX screensaver: a scene composed at random in one of the
//! tracer's settings, revealed coarse to fine — the whole picture soft within
//! moments, each pass then sharpening it — held for a minute, faded out, and
//! followed by a scene set elsewhere, a readout in the corner saying how far
//! the scene is prepared and then traced.
//!
//! The tracing runs on a thread of the saver's own where one is granted: on
//! that thread alone under the idle setting, across every thread granted under
//! performance. Frames only paint what it has finished: half a second apart
//! while the picture forms, each paint's change faded in over the wait until
//! the next, then further apart and laid straight on as only finer detail is
//! left to show. Where no thread is granted, each frame traces a slice itself.
//! A whole picture is handed to the host to keep where the user asked for
//! that. Under reduced motion changes are laid straight on and the picture is
//! cut to black rather than faded.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

mod crew;
mod crossfade;
mod engine;
mod preview;
mod readout;
mod tiles;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::time::NANOS_PER_MILLI;
use tairix_abi::Errno;
use tairix_geometry::{Rect, Region};
use tairix_parallel::{JobRunner, Pool};
use tairix_raster::{DitherRow, Surface};
use tairix_raytrace::{Detail, Setting};
use tairix_saver::{Frame, Host, NextFrame, Scene, Setup};
use tairix_theme::motion::{work_slice_ns, SceneClock};
use tairix_theme::{Fade, Timeline};

use crew::{run_tracing_thread, DeskLink, DeskLock, Status, TraceDesk, TraceHost, TraceLink};
use crossfade::Crossfade;
use engine::{Detailing, Engine, Memory, Traced};
use preview::Preview;
use readout::{Doing, Readout};

/// How much of the machine the tracing takes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CpuUse {
    /// One thread, leaving the rest of the machine free.
    Idle,
    /// Every thread the host granted, so each picture is finished sooner.
    Performance,
}

/// How long the whole picture is held before it fades, and how long the
/// screen rests black after the heap refused a scene.
const HOLD_NS: u64 = 60_000_000_000;

/// How long the picture takes to fade out.
const FADE_MS: u16 = 3_000;

/// The fewest pixels worth handing another core to dim: dimming one costs
/// next to nothing, so a hand-off must carry many.
const FADE_GRAIN: usize = 16_384;

/// The finest grid traced whole before a reveal's fine detail begins: the
/// 2 px pass follows it.
const DETAIL_GRID: u32 = 4;

/// How long apart a reveal's first paints come: the wait each one's change
/// is faded in over.
const FIRST_WAIT_NS: u64 = 500_000_000;

/// How long apart a reveal's paints come as its fine detail begins: too
/// little changes between them for a quicker cadence to show.
const DETAIL_WAIT_NS: u64 = 1_500_000_000;

/// The longest a reveal goes between paints, so a picture still sharpening
/// never looks stalled.
const MOST_WAIT_NS: u64 = 3_000_000_000;

// Clamping to the cadence's bounds cannot panic, and its longest wait is a
// fade's span in milliseconds.
const _: () = assert!(
    FIRST_WAIT_NS <= DETAIL_WAIT_NS
        && DETAIL_WAIT_NS <= MOST_WAIT_NS
        && (MOST_WAIT_NS / NANOS_PER_MILLI) >> u16::BITS == 0
);

/// How many thousandths of a reveal's steps are shown before its paints are
/// laid straight on: by then a change is dots too small for a fade to show.
const FADED_THOUSANDTHS: u64 = 50;

/// How long apart the readout is brought up to date while a scene is prepared
/// on a thread of its own, and so the most a readied scene waits to be
/// collected.
const READOUT_WAIT_NS: u64 = 250_000_000;

/// Where the saver stands.
enum Phase {
    /// The scene being prepared and traced, painted as each paint comes due.
    Revealing,
    /// The picture is whole, and held until `until_ns`.
    Holding { until_ns: u64 },
    /// Fading to black: `strength` of the picture is on screen now.
    Fading { fade: Fade, strength: u8 },
    /// Black until `until_ns`, the heap having refused a scene.
    Resting { until_ns: u64 },
}

/// How a reveal's changes reach the screen.
#[allow(
    clippy::large_enum_variant,
    reason = "one laying is held for a screensaver's life; boxing the crossfade would add an \
              allocation that cannot fail gracefully beside the two it already makes fallibly"
)]
enum Laying {
    /// Nothing painted yet: the first change is faded in where the heap and
    /// the memory band hold the room.
    Unbegun,
    /// Each change faded in over the wait until the next paint.
    Fading(Crossfade),
    /// Each change laid straight on.
    Straight,
}

/// Where the reveal's steps are traced.
#[allow(
    clippy::large_enum_variant,
    reason = "one feed is held for a screensaver's life, so the room the engine sets is paid \
              once; boxing it would trade that for an allocation that cannot fail gracefully"
)]
enum Feed {
    /// On a thread of its own.
    Crew(Box<dyn TraceLink>),
    /// On the frames themselves, a slice each paced by `clock`, across
    /// `runner` under performance and serially otherwise.
    Inline {
        engine: Engine,
        runner: Option<Pool>,
        clock: Box<dyn FnMut() -> u64>,
    },
}

impl Feed {
    /// Move what has been traced since the last collection onto the end of
    /// `into`, tracing a slice first on the frames' own feed.
    fn collect(&mut self, into: &mut Vec<Traced>) -> Status {
        match self {
            Self::Crew(link) => link.collect(into),
            Self::Inline {
                engine,
                runner,
                clock,
            } => {
                let runner: &dyn JobRunner = match runner {
                    Some(pool) => pool,
                    None => &tairix_parallel::SERIAL,
                };
                engine.step(runner, into, &mut **clock)
            }
        }
    }

    /// Ask for a scene set elsewhere, dropping what was traced of this one.
    fn next(&mut self) {
        match self {
            Self::Crew(link) => link.next(),
            Self::Inline { engine, .. } => engine.next(),
        }
    }

    /// When the frames are to come back to a reveal that wants them back at
    /// `wanted_ns`: on the frames' own feed a scene frame of `frame_ns` later
    /// at the latest, to trace the next slice.
    fn due(&self, now_ns: u64, wanted_ns: u64, frame_ns: u64) -> u64 {
        match self {
            Self::Crew(_) => wanted_ns,
            Self::Inline { .. } => wanted_ns.min(now_ns.saturating_add(frame_ns)),
        }
    }

    /// The runner the frames paint across: the frames' own pool where they
    /// trace across one, and serially beside a tracing thread, which holds
    /// the threads.
    fn painter(&self) -> &dyn JobRunner {
        match self {
            Self::Inline {
                runner: Some(pool), ..
            } => pool,
            _ => &tairix_parallel::SERIAL,
        }
    }
}

/// The ray-traced screensaver.
pub struct Raytrace {
    size: (u32, u32),
    phase: Phase,
    feed: Feed,
    /// The steps collected and not yet painted, the buffer kept from paint to
    /// paint.
    drawn: Vec<Traced>,
    /// Where they go, kept likewise.
    cover: Region,
    preview: Preview,
    laying: Laying,
    readout: Readout,
    calm: bool,
    /// What the saver's memory can spare the crossfade.
    memory: Memory,
    /// How long a scene frame stands on the display.
    frame_ns: u64,
    due_ns: u64,
    /// How many steps come before the fine detail: every point of the
    /// `DETAIL_GRID` grid.
    detail_from: u64,
    /// How many steps are shown before changes are laid straight on.
    faded_until: u64,
    /// How many steps of the scene under way have been painted.
    shown: u64,
    paint_due_ns: u64,
    /// Whether each whole picture is handed to the host to keep.
    keeping: bool,
    /// The setting of a whole picture on screen still to be handed over.
    to_keep: Option<Setting>,
}

impl Raytrace {
    /// The reveal `setup` describes, traced on a thread of its own where one
    /// is granted.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] or [`Errno::OutOfRange`] for an option the host
    /// did not send as the manifest declares it, [`Errno::OutOfMemory`] when
    /// the heap will not hold the reveal.
    pub fn new(setup: &Setup<'_>) -> Result<Self, Errno> {
        let threads = setup.start.threads.min(u32::from(setup.start.cores));
        let host = ThreadHost {
            participants: usize::try_from(threads.saturating_sub(1)).unwrap_or(0),
        };
        Self::with_host(
            setup,
            (threads > 1).then_some(&host as &dyn TraceHost),
            Box::new(tairix_rt::clock_get),
        )
    }

    /// The reveal `setup` describes, traced on threads `host` grants where it
    /// grants any and otherwise on the frames, a slice each paced by `clock`;
    /// errors as [`new`](Self::new).
    fn with_host(
        setup: &Setup<'_>,
        host: Option<&dyn TraceHost>,
        clock: Box<dyn FnMut() -> u64>,
    ) -> Result<Self, Errno> {
        let cpu = match setup.choice("cpu")?.as_str() {
            "idle" => CpuUse::Idle,
            "performance" => CpuUse::Performance,
            _ => return Err(Errno::OutOfRange),
        };
        let asked = match setup.choice("detail")?.as_str() {
            "simple" => Detail::Simple,
            "maximum" => Detail::Maximum,
            _ => return Err(Errno::OutOfRange),
        };
        let keeping = setup.switch("save")? && setup.start.feeds.keep();
        let start = setup.start;
        let size = (start.region.width(), start.region.height());
        let memory = Memory {
            total: start.memory,
            gauge: tairix_rt::pressure::gauge(),
        };
        let detailing = Detailing { asked, memory };
        let oom = || Errno::OutOfMemory;
        let preview = Preview::new(size).ok_or_else(oom)?;
        let engine = Engine::new(size, setup.seed, detailing, work_slice_ns(start.refresh))
            .ok_or_else(oom)?;
        let inline = |engine| Feed::Inline {
            engine,
            runner: (cpu == CpuUse::Performance).then(|| setup.pool()),
            clock,
        };
        let feed = match host {
            Some(host) => match host.launch(engine, cpu) {
                Ok(link) => Feed::Crew(link),
                Err(engine) => inline(engine),
            },
            None => inline(engine),
        };
        let now_ns = start.now_ns;
        Ok(Self {
            size,
            phase: Phase::Revealing,
            feed,
            drawn: Vec::new(),
            cover: Region::new(),
            preview,
            laying: unbegun(start.reduced_motion),
            readout: Readout::new(&setup.theme(), setup.scale, size).ok_or_else(oom)?,
            calm: start.reduced_motion,
            memory,
            frame_ns: SceneClock::frame_on(start.refresh),
            due_ns: now_ns,
            detail_from: u64::from(size.0.div_ceil(DETAIL_GRID))
                * u64::from(size.1.div_ceil(DETAIL_GRID)),
            faded_until: u64::from(size.0) * u64::from(size.1) * FADED_THOUSANDTHS / 1000,
            shown: 0,
            paint_due_ns: now_ns,
            keeping,
            to_keep: None,
        })
    }
}

impl Scene for Raytrace {
    /// Carry the saver on to the frame's instant: collect and paint the
    /// reveal, hold, fade, or rest. The readout is lifted off the picture
    /// before it changes and laid back on after.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        host: &mut Host<'_>,
    ) -> NextFrame {
        let now_ns = frame.now_ns;
        self.readout.lift(surface);
        if now_ns >= self.due_ns {
            let phase = core::mem::replace(&mut self.phase, Phase::Revealing);
            self.phase = match phase {
                Phase::Revealing => self.reveal(now_ns, surface, damage),
                Phase::Holding { until_ns } if now_ns < until_ns => {
                    self.due_ns = until_ns;
                    Phase::Holding { until_ns }
                }
                Phase::Holding { .. } => {
                    let span = if self.calm { 0 } else { FADE_MS };
                    let fade = Fade::start(now_ns, span, u8::MAX, 0);
                    self.fade(fade, u8::MAX, now_ns, surface, damage)
                }
                Phase::Fading { fade, strength } => {
                    self.fade(fade, strength, now_ns, surface, damage)
                }
                Phase::Resting { until_ns } if now_ns < until_ns => {
                    self.due_ns = until_ns;
                    Phase::Resting { until_ns }
                }
                Phase::Resting { .. } => self.next(now_ns),
            };
        }
        self.readout.lay(surface, damage);
        self.hand_over(surface, host);
        NextFrame::At(self.due_ns)
    }
}

impl Raytrace {
    /// Hand the whole picture on `surface` to the host to keep, once: while it
    /// is held whole, the readout off it. A host still holding the last one is
    /// asked again at the frame its answer brings.
    fn hand_over(&mut self, surface: &Surface, host: &mut Host<'_>) {
        if !matches!(self.phase, Phase::Holding { .. }) {
            self.to_keep = None;
            return;
        }
        let Some(setting) = self.to_keep else {
            return;
        };
        if host.keep(setting.name(), surface) != Err(Errno::Busy) {
            self.to_keep = None;
        }
    }

    /// Ask for a scene set elsewhere, to reveal from `now_ns`.
    fn next(&mut self, now_ns: u64) -> Phase {
        self.feed.next();
        self.preview.reset();
        self.laying = unbegun(self.calm);
        self.drawn.clear();
        self.shown = 0;
        self.paint_due_ns = now_ns;
        self.due_ns = now_ns;
        Phase::Revealing
    }

    /// Collect what has been traced, and put it on screen if a paint is due,
    /// or draw the frame of the change fading in: the phase that follows.
    fn reveal(&mut self, now_ns: u64, surface: &mut Surface, damage: &mut Region) -> Phase {
        let status = self.feed.collect(&mut self.drawn);
        let whole = matches!(status, Status::Whole(_));
        // A picture's last steps cannot wait.
        if whole || now_ns >= self.paint_due_ns {
            self.paint(now_ns, (surface, damage), whole);
        } else {
            self.fade_in(now_ns, surface, damage);
        }
        match status {
            Status::Preparing(done) => {
                self.readout.show(Doing::Generating, done);
                self.due_ns = self.feed.due(
                    now_ns,
                    now_ns.saturating_add(READOUT_WAIT_NS),
                    self.frame_ns,
                );
                Phase::Revealing
            }
            Status::Tracing(done) => {
                self.readout.show(Doing::Rendering, done);
                self.due_ns = self.feed.due(now_ns, self.wanted_ns(now_ns), self.frame_ns);
                Phase::Revealing
            }
            Status::Whole(setting) => {
                self.readout.take_down();
                if self.keeping {
                    self.to_keep = Some(setting);
                }
                let until_ns = now_ns.saturating_add(HOLD_NS);
                self.due_ns = until_ns;
                Phase::Holding { until_ns }
            }
            Status::Failed => self.rest(now_ns, surface, damage),
        }
    }

    /// When the reveal wants the frames back after `now_ns`: at the next
    /// paint, and a scene frame on while a change is fading in.
    fn wanted_ns(&self, now_ns: u64) -> u64 {
        match &self.laying {
            Laying::Fading(fade) if !fade.settled() => {
                self.paint_due_ns.min(now_ns.saturating_add(self.frame_ns))
            }
            _ => self.paint_due_ns,
        }
    }

    /// Rest the screen black from `now_ns` after the heap refused the reveal,
    /// before the next scene is tried.
    fn rest(&mut self, now_ns: u64, surface: &mut Surface, damage: &mut Region) -> Phase {
        self.readout.take_down();
        self.laying = Laying::Straight;
        // Nothing shown of the scene leaves the black it began over.
        if self.shown > 0 {
            dim(surface, damage, self.feed.painter(), 0);
        }
        let until_ns = now_ns.saturating_add(HOLD_NS);
        self.due_ns = until_ns;
        Phase::Resting { until_ns }
    }

    /// Put what the steps collected since the last paint change on screen:
    /// faded in over the wait until the next paint while a twentieth of the
    /// picture is yet to be shown, and laid straight on from there and once
    /// the picture is `whole`.
    fn paint(&mut self, now_ns: u64, (surface, damage): (&mut Surface, &mut Region), whole: bool) {
        let fading = !whole
            && self.shown < self.faded_until
            && self.memory.holds_disposable(Crossfade::bytes(self.size));
        let laying = match core::mem::replace(&mut self.laying, Laying::Straight) {
            // The first paint with something to show decides for the reveal.
            Laying::Unbegun if self.drawn.is_empty() => Laying::Unbegun,
            Laying::Unbegun if fading => match Crossfade::new(self.size) {
                Some(fade) => self.paint_faded(fade, now_ns, (surface, damage)),
                None => self.paint_straight(surface, damage),
            },
            Laying::Fading(fade) if fading => self.paint_faded(fade, now_ns, (surface, damage)),
            // Its change goes on whole, and its room with it.
            Laying::Fading(mut fade) if !fade.settled() => {
                fade.show(u8::MAX, (surface, damage), self.feed.painter());
                self.paint_straight(surface, damage)
            }
            Laying::Unbegun | Laying::Fading(_) | Laying::Straight => {
                self.paint_straight(surface, damage)
            }
        };
        self.laying = laying;
        self.paint_due_ns =
            now_ns.saturating_add(paint_wait(self.shown, self.detail_from, self.frame_ns));
    }

    /// Settle `fade`'s change under way, then begin fading in what the steps
    /// collected since the last paint change, over the wait until the next
    /// paint: how the reveal's changes are then laid.
    fn paint_faded(
        &mut self,
        mut fade: Crossfade,
        now_ns: u64,
        (surface, damage): (&mut Surface, &mut Region),
    ) -> Laying {
        if !fade.settled() {
            fade.show(u8::MAX, (surface, damage), self.feed.painter());
        }
        if !self.drawn.is_empty() {
            self.take_drawn();
            let wait_ms = paint_wait(self.shown, self.detail_from, self.frame_ns) / NANOS_PER_MILLI;
            let timeline = Timeline::start(now_ns, u16::try_from(wait_ms).unwrap_or(u16::MAX));
            let runner = self.feed.painter();
            let picture = fade.begin((self.preview.changed(), &mut self.cover), timeline, runner);
            self.preview.paint(picture, runner);
        }
        Laying::Fading(fade)
    }

    /// Lay what the steps collected since the last paint change straight on
    /// `surface`, marking it in `damage`.
    fn paint_straight(&mut self, surface: &mut Surface, damage: &mut Region) -> Laying {
        if !self.drawn.is_empty() {
            self.take_drawn();
            self.preview.paint(surface, self.feed.painter());
            for rect in self.cover.rects() {
                damage.add(*rect);
            }
        }
        Laying::Straight
    }

    /// Take the steps collected since the last paint into the painter,
    /// marking what they change in the cover, and count them shown.
    fn take_drawn(&mut self) {
        self.cover.clear();
        self.preview.take(&self.drawn, &mut self.cover);
        let taken = u64::try_from(self.drawn.len()).unwrap_or(u64::MAX);
        self.shown = self.shown.saturating_add(taken);
        self.drawn.clear();
    }

    /// Draw the frame due at `now_ns` of the change fading in — the whole of
    /// it, its room then let go, once the memory band wants the room back.
    fn fade_in(&mut self, now_ns: u64, surface: &mut Surface, damage: &mut Region) {
        let Laying::Fading(fade) = &mut self.laying else {
            return;
        };
        let held = self.memory.holds_disposable(Crossfade::bytes(self.size));
        let weight = if held {
            fade.due(now_ns)
        } else {
            (!fade.settled()).then_some(u8::MAX)
        };
        if let Some(weight) = weight {
            fade.show(weight, (surface, damage), self.feed.painter());
        }
        if !held {
            self.laying = Laying::Straight;
        }
    }

    /// Dim the picture, now carrying `strength`, toward black as `fade` has
    /// it at `now_ns`; once it is black, ask for the next scene.
    fn fade(
        &mut self,
        fade: Fade,
        strength: u8,
        now_ns: u64,
        surface: &mut Surface,
        damage: &mut Region,
    ) -> Phase {
        let target = fade.strength(now_ns);
        if target < strength {
            // What is on screen already carries `strength`; scaling it by
            // this share brings it to `target`.
            let share =
                (u32::from(target) * 255 + u32::from(strength) / 2) / u32::from(strength.max(1));
            dim(
                surface,
                damage,
                self.feed.painter(),
                u8::try_from(share).unwrap_or(0),
            );
        }
        if target == 0 {
            return self.next(now_ns);
        }
        self.due_ns = now_ns.saturating_add(self.frame_ns);
        Phase::Fading {
            fade,
            strength: target.min(strength),
        }
    }
}

/// How a reveal begins laying its changes on: straight on under reduced
/// motion, which `calm` is.
fn unbegun(calm: bool) -> Laying {
    if calm {
        Laying::Straight
    } else {
        Laying::Unbegun
    }
}

/// The wait after a paint leaving `shown` steps of a reveal on screen,
/// `detail_from` coming before its fine detail: a scene frame of `frame_ns`
/// while none is, so the first are shown as soon as they are traced, then in
/// proportion to the steps shown, so each pass, four times as long as the one
/// before, is shown in about as many paints — `FIRST_WAIT_NS` at least,
/// `DETAIL_WAIT_NS` as the fine detail begins, and `MOST_WAIT_NS` at most.
fn paint_wait(shown: u64, detail_from: u64, frame_ns: u64) -> u64 {
    if shown == 0 {
        return frame_ns.min(FIRST_WAIT_NS);
    }
    let wait = u128::from(shown) * u128::from(DETAIL_WAIT_NS) / u128::from(detail_from.max(1));
    u64::try_from(wait).map_or(MOST_WAIT_NS, |wait| wait.clamp(FIRST_WAIT_NS, MOST_WAIT_NS))
}

/// Scale the whole of `surface` by `share` of 255, dithered, the rows spread
/// across `runner`, marking it in `damage`.
fn dim(surface: &mut Surface, damage: &mut Region, runner: &dyn JobRunner, share: u8) {
    let (width, height) = (surface.width(), surface.height());
    damage.add(Rect::new(0, 0, width, height));
    let rows = usize::try_from(height).unwrap_or(0);
    let row_pixels = usize::try_from(width.max(1)).unwrap_or(1);
    let pieces = tairix_parallel::bands(runner, rows, FADE_GRAIN.div_ceil(row_pixels));
    let per_band = tairix_raster::band_rows(rows, pieces);
    let darken = |band: &mut tairix_raster::RowBand<'_>| {
        for y in band.rows() {
            let dither = DitherRow::at(y);
            if let Some((first, span)) = band.row_span_mut(y, 0, width) {
                let mut x = first;
                for pixel in span {
                    *pixel = pixel.dimmed_biased(share, dither.bias(x));
                    x = x.wrapping_add(1);
                }
            }
        }
    };
    tairix_parallel::for_each_drawn(
        runner,
        surface.row_bands_mut(0..height, per_band),
        &|mut band| darken(&mut band),
    );
}

/// The threads the saver traces on beside its frames: a tracing thread, and
/// under performance a worker for each other one of the `participants` the
/// host granted beyond the frames' own.
struct ThreadHost {
    participants: usize,
}

impl TraceHost for ThreadHost {
    fn launch(&self, engine: Engine, cpu: CpuUse) -> Result<Box<dyn TraceLink>, Engine> {
        let desk = Arc::new(ThreadDesk {
            desk: tairix_rt::sync::Mutex::new(TraceDesk::new()),
            turn: tairix_rt::sync::Condvar::new(),
        });
        let served = Arc::clone(&desk);
        let workers = match cpu {
            CpuUse::Idle => 0,
            CpuUse::Performance => self.participants.saturating_sub(1),
        };
        let spawned = tairix_rt::thread::Thread::spawn(move || {
            // Made and joined on this thread, so the frames wait on neither the
            // workers' creation nor their teardown.
            let pool = Pool::with_workers(workers);
            run_tracing_thread(&*served, &pool, &tairix_rt::clock_get);
        });
        match spawned {
            Ok(thread) => {
                // Detached: it leaves at its next turn once the link is
                // dropped, and the frames never wait for it.
                thread.detach();
                Ok(Box::new(DeskLink::hand_over(desk, engine)))
            }
            Err(_) => Err(engine),
        }
    }
}

/// A reveal's [`TraceDesk`] behind the runtime's futex mutex, with the
/// condition variable its tracing thread parks on when there is nothing to
/// trace (never a spin).
struct ThreadDesk {
    desk: tairix_rt::sync::Mutex<TraceDesk>,
    turn: tairix_rt::sync::Condvar,
}

impl DeskLock for ThreadDesk {
    type Guard<'a> = tairix_rt::sync::MutexGuard<'a, TraceDesk>;

    fn lock(&self) -> Self::Guard<'_> {
        self.desk.lock()
    }

    fn park<'a>(&'a self, held: Self::Guard<'a>) -> Self::Guard<'a> {
        self.turn.wait(held)
    }

    fn signal(&self) {
        self.turn.notify_one();
    }
}

#[cfg(test)]
mod tests;
