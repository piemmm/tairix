//! The ray tracer: a scene composed at random in one of the tracer's
//! settings, revealed coarse to fine — the whole picture in rough blocks
//! within moments, then each pass halving them — held for a minute, faded
//! out, and followed by a scene set elsewhere.
//!
//! The tracing runs on threads of its own where the embedder grants them: one
//! core under the idle setting, every core under performance. The serve loop
//! only paints what they have finished, once a frame; where no thread is
//! granted, it traces a slice a frame itself. The picture lives in the
//! window's buffer alone, and a buffer the compositor lets go starts the
//! reveal again. Under reduced motion the picture is cut to black rather than
//! faded.

mod crew;
mod engine;

pub use crew::{
    run_tracing_thread, DeskLink, DeskLock, Request, Status, TraceDesk, TraceHost, TraceLink,
};
pub use engine::{Engine, Traced};

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_raster::DitherRow;
use tairix_raytrace::Block;
use tairix_theme::Fade;
use tairix_util::fallible;
use tairix_wallpaper::CpuUse;
use tairix_wm::{Color, Compositor, Rect, Region, Surface, WindowId};

use super::seed_from;
use tairix_theme::motion::SceneClock;

/// How long the whole picture is held before it fades, and how long the
/// screen rests black after the heap refused a scene.
const HOLD_NS: u64 = 60_000_000_000;

/// How long the picture takes to fade out.
const FADE_MS: u16 = 3_000;

/// The fewest pixels worth handing another core to dim: dimming one costs
/// next to nothing, so a hand-off must carry many.
const FADE_GRAIN: usize = 16_384;

/// The most blocks a frame's damage lists one by one, rather than as the box
/// they span: a few blocks a frame are repainted alone, while thousands,
/// scattered as a pass is, span the screen anyway, and merging each into a
/// region would cost more than it saves.
const DAMAGE_BUDGET: usize = 256;

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

    fn request(&mut self, request: Request) {
        match self {
            Self::Crew(link) => link.request(request),
            Self::Inline { engine, .. } => engine.apply(request),
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
    calm: bool,
    due_ns: u64,
}

impl Raytrace {
    /// A reveal for a `size` screen beginning at `now_ns`, `calm` under
    /// reduced motion, traced on `cpu`'s share of the machine — on threads
    /// `host` grants where it grants any. `None` when the screen has no pixels
    /// or the heap will not hold the reveal.
    pub(super) fn new(
        size: (u32, u32),
        calm: bool,
        now_ns: u64,
        cpu: CpuUse,
        host: Option<&dyn TraceHost>,
    ) -> Option<Self> {
        let engine = Engine::new(size, seed_from(now_ns))?;
        let feed = match host {
            Some(host) => match host.launch(engine, cpu) {
                Ok(link) => Feed::Crew(link),
                Err(engine) => Feed::Inline { engine, cpu },
            },
            None => Feed::Inline { engine, cpu },
        };
        Some(Self {
            size,
            phase: Phase::Revealing,
            feed,
            drawn: Vec::new(),
            damage: Region::new(),
            calm,
            due_ns: now_ns,
        })
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
        self.feed.request(Request::Next);
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
        // A buffer the compositor no longer keeps holds none of the picture,
        // so the picture starts again rather than being kept twice over.
        let kept = compositor.keeps_content(wm, self.size);
        if !kept {
            self.feed.request(Request::Again);
        }
        let status = self
            .feed
            .collect(&mut self.drawn, compositor.job_runner(), clock);
        // A picture the heap will not give a buffer is refused like a scene:
        // restarting it every frame would retry the allocation with it, and
        // keep the tracing threads busy on steps nothing can show.
        if !self.paint(wm, compositor, kept) {
            return self.rest(now_ns);
        }
        match status {
            Status::Working => {
                self.due_ns = now_ns.saturating_add(SceneClock::FRAME_NS);
                Phase::Revealing
            }
            Status::Whole => {
                let until_ns = now_ns.saturating_add(HOLD_NS);
                self.due_ns = until_ns;
                Phase::Holding { until_ns }
            }
            Status::Failed => self.rest(now_ns),
        }
    }

    /// Rest the screen from `now_ns` after the heap refused the reveal, before
    /// the next scene is tried.
    fn rest(&mut self, now_ns: u64) -> Phase {
        let until_ns = now_ns.saturating_add(HOLD_NS);
        self.due_ns = until_ns;
        Phase::Resting { until_ns }
    }

    /// Paint the frame's steps over the picture — over black, in a buffer the
    /// compositor let go — marking the blocks they cover, or the box they
    /// span once they are many; `false` when the heap would not give the
    /// picture a buffer.
    fn paint(&mut self, wm: WindowId, compositor: &mut Compositor, kept: bool) -> bool {
        if kept && self.drawn.is_empty() {
            return true;
        }
        self.damage.clear();
        if self.drawn.len() <= DAMAGE_BUDGET {
            for traced in &self.drawn {
                self.damage.add(block_rect(traced.block));
            }
        } else {
            let span = self.drawn.iter().fold(Rect::EMPTY, |span, traced| {
                span.union(&block_rect(traced.block))
            });
            self.damage.add(span);
        }
        let drawn = &self.drawn;
        let painted = compositor.repaint_window(wm, self.size, &self.damage, |surface, _| {
            if !kept {
                surface.fill(Color::rgb(0, 0, 0));
            }
            for traced in drawn {
                fill_block(surface, traced);
            }
        });
        self.drawn.clear();
        painted
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

/// The rectangle of the picture `block` covers.
fn block_rect(block: Block) -> Rect {
    Rect::new(
        i32::try_from(block.x).unwrap_or(i32::MAX),
        i32::try_from(block.y).unwrap_or(i32::MAX),
        block.width,
        block.height,
    )
}

/// Paint `traced`'s colour over its block.
fn fill_block(surface: &mut Surface, traced: &Traced) {
    let Block {
        x,
        y,
        width,
        height,
    } = traced.block;
    for row in y..y.saturating_add(height) {
        if let Some((_, span)) = surface.row_span_mut(row, x, width) {
            span.fill(traced.pixel);
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
