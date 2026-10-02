//! The ray tracer: a scene composed at random in one of the tracer's
//! settings, revealed coarse to fine — the whole picture soft within moments,
//! each pass then sharpening it — held for a minute, faded out, and followed
//! by a scene set elsewhere, a readout in the corner saying how far the scene
//! is prepared and then traced.
//!
//! The tracing runs on threads of its own where the embedder grants them: one
//! core under the idle setting, every core under performance, and whole
//! pictures are kept there when asked. The serve loop only paints what they
//! have finished, once a frame; where no thread is granted, it traces a slice
//! a frame itself and keeps nothing. The painter keeps every traced pixel, so
//! a buffer the compositor lets go is painted afresh rather than traced again.
//! Under reduced motion the picture is cut to black rather than faded.

mod album;
mod crew;
mod engine;
mod preview;
mod readout;

pub use album::{keep, Picture, PictureFiles, Unkept, FOLDERS};
pub use crew::{
    run_tracing_thread, DeskLink, DeskLock, Keeper, Status, TraceDesk, TraceHost, TraceLink,
};
pub use engine::{Engine, Traced};

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_raster::DitherRow;
use tairix_theme::{Fade, Theme};
use tairix_util::fallible;
use tairix_wallpaper::{CpuUse, RaytraceOptions};
use tairix_wm::{Color, Compositor, Rect, Region, Scale, WindowId};

use super::seed_from;
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

/// Where the saver stands.
enum Phase {
    /// The scene being traced, and painted as it arrives.
    Revealing,
    /// The picture is whole, and held until `until_ns`.
    Holding { until_ns: u64 },
    /// Fading to black: `strength` of the picture is on screen now.
    Fading { fade: Fade, strength: u8 },
    /// Black until `until_ns`, the heap having refused a scene.
    Resting { until_ns: u64 },
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
    /// Move what has been traced since the last frame into `into`, tracing a
    /// slice first on the loop's own feed, across `wide` only under
    /// performance.
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
}

/// The ray-traced screensaver.
pub(super) struct Raytrace {
    size: (u32, u32),
    phase: Phase,
    feed: Feed,
    /// A frame's traced steps, kept for its buffer from frame to frame.
    drawn: Vec<Traced>,
    /// Where they go, kept likewise.
    damage: Region,
    preview: Preview,
    readout: Readout,
    calm: bool,
    due_ns: u64,
}

impl Raytrace {
    /// A reveal for a `size` screen beginning at `now_ns`, `calm` under
    /// reduced motion, traced as `options` ask — on threads `host` grants
    /// where it grants any — its readout in `theme`'s type at `scale`. `None`
    /// when the screen has no pixels or the heap will not hold the reveal.
    pub(super) fn new(
        size: (u32, u32),
        (calm, now_ns): (bool, u64),
        options: RaytraceOptions,
        host: Option<&dyn TraceHost>,
        (theme, scale): (&Theme, Scale),
    ) -> Option<Self> {
        let preview = Preview::new(size)?;
        let engine = Engine::new(size, seed_from(now_ns))?;
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
            readout: Readout::new(theme, scale, size),
            calm,
            due_ns: now_ns,
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
        self.due_ns = now_ns;
        Phase::Revealing
    }

    /// Put what has been traced since the last frame on screen: the phase
    /// that follows.
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
        // A picture the heap will not give a buffer is refused like a scene:
        // restarting it every frame would retry the allocation with it, and
        // keep the tracing threads busy on steps nothing can show.
        if !self.paint(wm, compositor, kept) {
            return self.rest(now_ns, compositor);
        }
        match status {
            Status::Preparing(done) => {
                self.readout.show(compositor, wm, Doing::Generating, done);
                self.due_ns = now_ns.saturating_add(SceneClock::FRAME_NS);
                Phase::Revealing
            }
            Status::Tracing(done) => {
                self.readout.show(compositor, wm, Doing::Rendering, done);
                self.due_ns = now_ns.saturating_add(SceneClock::FRAME_NS);
                Phase::Revealing
            }
            Status::Whole => {
                self.readout.take_down(compositor);
                let until_ns = now_ns.saturating_add(HOLD_NS);
                self.due_ns = until_ns;
                Phase::Holding { until_ns }
            }
            Status::Failed => self.rest(now_ns, compositor),
        }
    }

    /// Rest the screen from `now_ns` after the heap refused the reveal, before
    /// the next scene is tried.
    fn rest(&mut self, now_ns: u64, compositor: &mut Compositor) -> Phase {
        self.readout.take_down(compositor);
        let until_ns = now_ns.saturating_add(HOLD_NS);
        self.due_ns = until_ns;
        Phase::Resting { until_ns }
    }

    /// Paint what the frame's steps change, marking it — the whole picture,
    /// painted afresh, in a buffer the compositor let go; `false` when the
    /// heap would not give the picture a buffer.
    fn paint(&mut self, wm: WindowId, compositor: &mut Compositor, kept: bool) -> bool {
        if kept && self.drawn.is_empty() {
            return true;
        }
        self.damage.clear();
        self.preview.take(&self.drawn, kept, &mut self.damage);
        self.drawn.clear();
        let runner = compositor.job_runner();
        let preview = &mut self.preview;
        compositor.repaint_window(wm, self.size, &self.damage, |surface, _| {
            preview.paint(surface, runner);
        })
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
        let per_band = u32::try_from(rows.div_ceil(pieces.max(1)))
            .unwrap_or(height)
            .max(1);
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
        let mut bands: Vec<_> = Vec::new();
        if pieces > 1 && fallible::reserve(&mut bands, pieces) {
            bands.extend(surface.row_bands_mut(0..height, per_band));
            tairix_parallel::for_each(runner, &mut bands, &darken);
        } else {
            for mut band in surface.row_bands_mut(0..height, height.max(1)) {
                darken(&mut band);
            }
        }
    });
}

#[cfg(test)]
#[path = "raytrace_tests.rs"]
mod tests;
