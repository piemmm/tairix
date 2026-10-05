//! The ray tracer: a scene composed at random in one of the tracer's
//! settings, revealed coarse to fine — the whole picture soft within moments,
//! each pass then sharpening it — held for a minute, faded out, and followed
//! by a scene set elsewhere, a readout in the corner saying how far the scene
//! is prepared and then traced.
//!
//! The tracing runs on threads of its own where the embedder grants them: one
//! core under the idle setting, every core under performance, and whole
//! pictures are kept there when asked. The serve loop only paints what they
//! have finished: half a second apart while the picture forms, each paint's
//! change faded in over the wait until the next, then further apart and laid
//! straight on as only finer detail is left to show. Where no thread is
//! granted, it traces a slice a frame itself and keeps nothing. The painter
//! keeps every traced pixel, so a buffer the compositor lets go is painted
//! afresh rather than traced again.
//! Under reduced motion changes are laid straight on and the picture is cut
//! to black rather than faded.

mod album;
mod crew;
mod crossfade;
mod engine;
mod preview;
mod readout;
mod tiles;

pub use album::{keep, Picture, PictureFiles, Unkept, FOLDERS};
pub use crew::{
    run_tracing_thread, DeskLink, DeskLock, Keeper, Status, TraceDesk, TraceHost, TraceLink,
};
#[cfg(test)]
pub(crate) use engine::PLAIN;
pub use engine::{Detailing, Engine, Memory, Traced};

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::time::NANOS_PER_MILLI;
use tairix_parallel::JobRunner;
use tairix_raster::DitherRow;
use tairix_raytrace::Detail;
use tairix_theme::{Fade, Theme, Timeline};
use tairix_wallpaper::{CpuUse, RaytraceOptions, SceneDetail};
use tairix_wm::{Color, Compositor, Rect, Region, Scale, WindowId};

use super::seed_from;
use crossfade::Crossfade;
use preview::Preview;
use readout::{Doing, Readout};
use tairix_theme::motion::SceneClock;

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

// Clamping to the cadence's bounds cannot panic, its anchor lies within, and
// its longest wait is a fade's span in milliseconds.
const _: () = assert!(
    SceneClock::FRAME_NS <= FIRST_WAIT_NS
        && FIRST_WAIT_NS <= DETAIL_WAIT_NS
        && DETAIL_WAIT_NS <= MOST_WAIT_NS
        && (MOST_WAIT_NS / NANOS_PER_MILLI) >> u16::BITS == 0
);

/// How many thousandths of a reveal's steps are shown before its paints are
/// laid straight on: by then a change is dots too small for a fade to show.
const FADED_THOUSANDTHS: u64 = 50;

/// How long apart the readout is brought up to date while a scene is prepared
/// on a thread of its own, which wakes the loop the moment the scene is ready.
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
    /// On threads of its own.
    Crew(Box<dyn TraceLink>),
    /// On the serve loop, a slice a frame, and on its own thread alone when
    /// `cpu` is idle.
    Inline { engine: Engine, cpu: CpuUse },
}

impl Feed {
    /// Move what has been traced since the last collection onto the end of
    /// `into`, tracing a slice first on the loop's own feed, across `wide` only
    /// under performance.
    fn collect(
        &mut self,
        into: &mut Vec<Traced>,
        wide: &dyn JobRunner,
        clock: &mut dyn FnMut() -> u64,
    ) -> Status {
        match self {
            Self::Crew(link) => link.collect(into),
            Self::Inline { engine, cpu } => {
                let runner: &dyn JobRunner = match cpu {
                    CpuUse::Idle => &tairix_parallel::SERIAL,
                    CpuUse::Performance => wide,
                };
                engine.step(runner, into, clock)
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

    /// When the loop is to come back to a reveal that wants it back at
    /// `wanted_ns`: on the loop's own feed a scene frame later at the latest,
    /// to trace the next slice.
    fn due(&self, now_ns: u64, wanted_ns: u64) -> u64 {
        match self {
            Self::Crew(_) => wanted_ns,
            Self::Inline { .. } => wanted_ns.min(now_ns.saturating_add(SceneClock::FRAME_NS)),
        }
    }
}

/// The ray-traced screensaver.
pub(super) struct Raytrace {
    size: (u32, u32),
    phase: Phase,
    feed: Feed,
    /// The steps collected and not yet painted, the buffer kept from paint to
    /// paint.
    drawn: Vec<Traced>,
    /// Where they go, kept likewise.
    damage: Region,
    preview: Preview,
    laying: Laying,
    readout: Readout,
    calm: bool,
    /// What the machine's memory can spare the crossfade.
    memory: Memory,
    due_ns: u64,
    /// How many steps come before the fine detail: every point of the
    /// `DETAIL_GRID` grid.
    detail_from: u64,
    /// How many steps are shown before changes are laid straight on.
    faded_until: u64,
    /// How many steps of the scene under way have been painted.
    shown: u64,
    paint_due_ns: u64,
}

impl Raytrace {
    /// A reveal for a `size` screen beginning at `now_ns`, `calm` under
    /// reduced motion, traced as `options` ask — on threads `host` grants
    /// where it grants any, each scene at the detail `memory` can spare,
    /// `tell` told when that changes — its readout in `theme`'s type at
    /// `scale`. `None` when the screen has no pixels or the heap will not
    /// hold the reveal.
    pub(super) fn new(
        size: (u32, u32),
        (calm, now_ns): (bool, u64),
        (options, memory, tell): (RaytraceOptions, Memory, fn(Detail)),
        host: Option<&dyn TraceHost>,
        (theme, scale): (&Theme, Scale),
    ) -> Option<Self> {
        let preview = Preview::new(size)?;
        let asked = match options.detail {
            SceneDetail::Simple => Detail::Simple,
            SceneDetail::Maximum => Detail::Maximum,
        };
        let detailing = Detailing {
            asked,
            memory,
            tell,
        };
        let engine = Engine::new(size, seed_from(now_ns), detailing)?;
        let feed = match host {
            Some(host) => match host.launch(engine, options) {
                Ok(link) => Feed::Crew(link),
                Err(engine) => Feed::Inline {
                    engine,
                    cpu: options.cpu,
                },
            },
            None => Feed::Inline {
                engine,
                cpu: options.cpu,
            },
        };
        Some(Self {
            size,
            phase: Phase::Revealing,
            feed,
            drawn: Vec::new(),
            damage: Region::new(),
            preview,
            laying: unbegun(calm),
            readout: Readout::new(theme, scale, size),
            calm,
            memory,
            due_ns: now_ns,
            detail_from: u64::from(size.0.div_ceil(DETAIL_GRID))
                * u64::from(size.1.div_ceil(DETAIL_GRID)),
            faded_until: u64::from(size.0) * u64::from(size.1) * FADED_THOUSANDTHS / 1000,
            shown: 0,
            paint_due_ns: now_ns,
        })
    }

    /// Take down what the saver shows beside the picture's own window.
    pub(super) fn take_down(&mut self, compositor: &mut Compositor) {
        self.readout.take_down(compositor);
    }

    /// When the next frame is due.
    pub(super) const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Come back at `now_ns` rather than when next due, if revealing: the wake
    /// a tracing thread gives on readying a scene, whose first passes are
    /// shown as they come.
    pub(super) fn landed(&mut self, now_ns: u64) {
        if matches!(self.phase, Phase::Revealing) {
            self.due_ns = self.due_ns.min(now_ns);
        }
    }

    /// Carry the saver on to `now_ns`, if a frame is due; `clock` reads the
    /// monotonic clock, to pace a slice the loop traces itself.
    pub(super) fn advance(
        &mut self,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
        clock: &mut dyn FnMut() -> u64,
    ) {
        if now_ns < self.due_ns {
            return;
        }
        let phase = core::mem::replace(&mut self.phase, Phase::Revealing);
        self.phase = match phase {
            Phase::Revealing => self.reveal(now_ns, wm, compositor, clock),
            Phase::Holding { until_ns } if now_ns < until_ns => {
                self.due_ns = until_ns;
                Phase::Holding { until_ns }
            }
            Phase::Holding { .. } => {
                let span = if self.calm { 0 } else { FADE_MS };
                let fade = Fade::start(now_ns, span, u8::MAX, 0);
                self.fade(fade, u8::MAX, now_ns, wm, compositor)
            }
            Phase::Fading { fade, strength } => self.fade(fade, strength, now_ns, wm, compositor),
            Phase::Resting { until_ns } if now_ns < until_ns => {
                self.due_ns = until_ns;
                Phase::Resting { until_ns }
            }
            Phase::Resting { .. } => self.next(now_ns),
        };
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
    fn reveal(
        &mut self,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
        clock: &mut dyn FnMut() -> u64,
    ) -> Phase {
        let kept = compositor.keeps_content(wm, self.size);
        let status = self
            .feed
            .collect(&mut self.drawn, compositor.job_runner(), clock);
        // A buffer to lay afresh and a picture's last steps cannot wait.
        if !kept || status == Status::Whole || now_ns >= self.paint_due_ns {
            // A picture the heap will not give a buffer is refused like a
            // scene: restarting it every frame would retry the allocation
            // with it, and keep the tracing threads busy on steps nothing can
            // show.
            if !self.paint(now_ns, wm, compositor, (kept, status)) {
                return self.rest(now_ns, wm, compositor);
            }
        } else {
            self.fade_in(now_ns, wm, compositor);
        }
        match status {
            Status::Preparing(done) => {
                self.readout.show(compositor, wm, Doing::Generating, done);
                self.due_ns = self
                    .feed
                    .due(now_ns, now_ns.saturating_add(READOUT_WAIT_NS));
                Phase::Revealing
            }
            Status::Tracing(done) => {
                self.readout.show(compositor, wm, Doing::Rendering, done);
                self.due_ns = self.feed.due(now_ns, self.wanted_ns(now_ns));
                Phase::Revealing
            }
            Status::Whole => {
                self.readout.take_down(compositor);
                let until_ns = now_ns.saturating_add(HOLD_NS);
                self.due_ns = until_ns;
                Phase::Holding { until_ns }
            }
            Status::Failed => self.rest(now_ns, wm, compositor),
        }
    }

    /// When the reveal wants the loop back after `now_ns`: at the next paint,
    /// and a scene frame on while a change is fading in.
    fn wanted_ns(&self, now_ns: u64) -> u64 {
        match &self.laying {
            Laying::Fading(fade) if !fade.settled() => self
                .paint_due_ns
                .min(now_ns.saturating_add(SceneClock::FRAME_NS)),
            _ => self.paint_due_ns,
        }
    }

    /// Rest the screen black from `now_ns` after the heap refused the reveal,
    /// before the next scene is tried.
    fn rest(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) -> Phase {
        self.readout.take_down(compositor);
        self.laying = Laying::Straight;
        // Nothing shown of the scene leaves the black it began over.
        if self.shown > 0 {
            dim(compositor, wm, self.size, 0);
        }
        let until_ns = now_ns.saturating_add(HOLD_NS);
        self.due_ns = until_ns;
        Phase::Resting { until_ns }
    }

    /// Put what the steps collected since the last paint change on screen as
    /// the reveal, `status`, stands: faded in over the wait until the next
    /// paint while a twentieth of the picture is yet to be shown, laid
    /// straight on from there, and the whole picture painted afresh in a
    /// buffer the compositor let go, which was not `kept`. `false` when the
    /// heap would not give the picture a buffer.
    fn paint(
        &mut self,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
        (kept, status): (bool, Status),
    ) -> bool {
        let fading = kept
            && status != Status::Whole
            && self.shown < self.faded_until
            && self.memory.holds_disposable(Crossfade::bytes(self.size));
        let (laying, painted) = match core::mem::replace(&mut self.laying, Laying::Straight) {
            // The first paint with something to show decides for the reveal.
            Laying::Unbegun if kept && self.drawn.is_empty() => (Laying::Unbegun, true),
            Laying::Unbegun if fading => match Crossfade::new(self.size) {
                Some(fade) => self.paint_faded(fade, now_ns, wm, compositor),
                None => (Laying::Straight, self.paint_straight(wm, compositor, kept)),
            },
            Laying::Fading(fade) if fading => self.paint_faded(fade, now_ns, wm, compositor),
            // Its change goes on whole, and its room with it.
            Laying::Fading(mut fade) if kept && !fade.settled() => {
                let _ = fade.show(u8::MAX, (wm, self.size), compositor);
                (Laying::Straight, self.paint_straight(wm, compositor, kept))
            }
            Laying::Unbegun | Laying::Fading(_) | Laying::Straight => {
                (Laying::Straight, self.paint_straight(wm, compositor, kept))
            }
        };
        self.laying = laying;
        if painted {
            self.paint_due_ns = now_ns.saturating_add(paint_wait(self.shown, self.detail_from));
        }
        painted
    }

    /// Settle `fade`'s change under way, then begin fading in what the steps
    /// collected since the last paint change, over the wait until the next
    /// paint: how the reveal's changes are then laid, and `false` when the
    /// picture's window has gone.
    fn paint_faded(
        &mut self,
        mut fade: Crossfade,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
    ) -> (Laying, bool) {
        if !fade.settled() && !fade.show(u8::MAX, (wm, self.size), compositor) {
            return (Laying::Fading(fade), false);
        }
        if !self.drawn.is_empty() {
            self.take_drawn(true);
            let wait_ms = paint_wait(self.shown, self.detail_from) / NANOS_PER_MILLI;
            let timeline = Timeline::start(now_ns, u16::try_from(wait_ms).unwrap_or(u16::MAX));
            let runner = compositor.job_runner();
            let picture = fade.begin((self.preview.changed(), &mut self.damage), timeline, runner);
            self.preview.paint(picture, runner);
        }
        (Laying::Fading(fade), true)
    }

    /// Lay what the steps collected since the last paint change straight on,
    /// marking it — the whole picture, painted afresh, in a buffer that was
    /// not `kept`; `false` when the heap would not give the picture a buffer.
    fn paint_straight(&mut self, wm: WindowId, compositor: &mut Compositor, kept: bool) -> bool {
        if kept && self.drawn.is_empty() {
            return true;
        }
        self.take_drawn(kept);
        let runner = compositor.job_runner();
        let preview = &mut self.preview;
        compositor.repaint_window(wm, self.size, &self.damage, |surface, _| {
            preview.paint(surface, runner);
        })
    }

    /// Take the steps collected since the last paint into the painter,
    /// marking what they change in `damage` — all of it where the buffer was
    /// not `kept` — and count them shown.
    fn take_drawn(&mut self, kept: bool) {
        self.damage.clear();
        self.preview.take(&self.drawn, kept, &mut self.damage);
        let taken = u64::try_from(self.drawn.len()).unwrap_or(u64::MAX);
        self.shown = self.shown.saturating_add(taken);
        self.drawn.clear();
    }

    /// Draw the frame due at `now_ns` of the change fading in — the whole of
    /// it, its room then let go, once the memory band wants the room back.
    fn fade_in(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) {
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
            let _ = fade.show(weight, (wm, self.size), compositor);
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
        wm: WindowId,
        compositor: &mut Compositor,
    ) -> Phase {
        let target = fade.strength(now_ns);
        if target < strength {
            // What is on screen already carries `strength`; scaling it by
            // this share brings it to `target`.
            let share =
                (u32::from(target) * 255 + u32::from(strength) / 2) / u32::from(strength.max(1));
            dim(compositor, wm, self.size, u8::try_from(share).unwrap_or(0));
        }
        if target == 0 {
            return self.next(now_ns);
        }
        self.due_ns = now_ns.saturating_add(SceneClock::FRAME_NS);
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
/// `detail_from` coming before its fine detail: a scene frame while none is,
/// so the first are shown as soon as they are traced, then in proportion to
/// the steps shown, so each pass, four times as long as the one before, is
/// shown in about as many paints — `FIRST_WAIT_NS` at least, `DETAIL_WAIT_NS`
/// as the fine detail begins, and `MOST_WAIT_NS` at most.
fn paint_wait(shown: u64, detail_from: u64) -> u64 {
    if shown == 0 {
        return SceneClock::FRAME_NS;
    }
    let wait = u128::from(shown) * u128::from(DETAIL_WAIT_NS) / u128::from(detail_from.max(1));
    u64::try_from(wait).map_or(MOST_WAIT_NS, |wait| wait.clamp(FIRST_WAIT_NS, MOST_WAIT_NS))
}

/// Scale the whole of window `wm`'s picture by `share` of 255, dithered, the
/// rows spread over the desktop's pool. A buffer the compositor no longer
/// keeps is simply black: the fade was going there anyway.
fn dim(compositor: &mut Compositor, wm: WindowId, size: (u32, u32), share: u8) {
    let runner = compositor.job_runner();
    let kept = compositor.keeps_content(wm, size);
    let (width, height) = size;
    let mut whole = Region::new();
    whole.add(Rect::new(0, 0, width, height));
    let _ = compositor.repaint_window(wm, size, &whole, |surface, _| {
        if !kept {
            surface.fill(Color::rgb(0, 0, 0));
            return;
        }
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
    });
}

#[cfg(test)]
#[path = "raytrace_tests.rs"]
mod tests;
