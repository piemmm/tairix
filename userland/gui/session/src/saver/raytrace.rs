//! The ray tracer: a scene composed at random in one of the tracer's
//! settings, revealed a pixel at a time in a random order until the picture
//! is whole, held for a minute, faded out, and followed by a scene set
//! elsewhere.
//!
//! A scene is prepared first — the grids its land, sea and cloud are traced
//! over, filled a band of rows a frame — and then its pixels traced. Each
//! frame of either does as much as fits half a desktop frame, spread over
//! the desktop's own worker pool: the pace is measured rather than assumed,
//! so the work runs as fast as the machine allows while the desktop still
//! answers within a frame. A reveal that would run past its budget on a slow
//! machine takes fewer samples a pixel for the rest of it. The picture lives
//! in the window's buffer alone, and the order is a keyed bijection, so how
//! many pixels are shown is all that is kept of which; once whole, the scene
//! itself is let go. Under reduced motion the picture is cut to black rather
//! than faded.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_raster::DitherRow;
use tairix_raytrace::{Draft, Encoder, Quality, Reveal, Scene, Setting, Tracer};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_theme::{Fade, Timeline};
use tairix_util::fallible;
use tairix_wm::{Color, Compositor, Pixel, Rect, Region, WindowId};

use super::{seed_from, SAVER_FRAME_NS};

/// How long the whole picture is held before it fades.
const HOLD_NS: u64 = 60_000_000_000;

/// How long the picture takes to fade out.
const FADE_MS: u16 = 3_000;

/// How long a reveal may take before the rest of it is traced with fewer
/// samples a pixel: long enough that a desktop-class machine never reaches
/// it, short enough that a slow one still shows a new scene every few
/// minutes.
const REVEAL_BUDGET_NS: u64 = 240_000_000_000;

/// How much of a frame the work may take: half of one desktop frame, so the
/// loop still answers input and every client within the frame.
const SLICE_NS: u64 = Timeline::FRAME_NS / 2;

/// The fewest pixels or vertices a frame does, which each phase starts
/// from, and the most pixels, which the room for a frame's results is sized
/// to.
const MIN_BATCH: u32 = 1;
const MAX_BATCH: u32 = 1 << 15;

/// The most vertices of a scene's grids a frame fills: a whole grid's worth.
const MAX_VERTICES: u32 = 1 << 20;

/// The fewest pixels worth handing another core: one, for a pixel's samples
/// cost far more than the hand-off.
const GRAIN: usize = 1;

/// The fewest pixels worth handing another core to dim: dimming one costs
/// next to nothing, so a hand-off must carry many.
const FADE_GRAIN: usize = 16_384;

/// Of the picture, how much is traced between the governor's judgements of
/// the pace: enough, scattered as the order is, to stand for the whole.
const JUDGED_SHARE: u32 = 64;

/// The most pixels a frame's damage lists one by one, rather than as the box
/// they span: a slow machine's few pixels a frame are repainted alone, while
/// a fast one's thousands, scattered as they are, span the screen anyway, and
/// merging each into a region would cost more than it saves.
const DAMAGE_BUDGET: usize = 256;

/// Where the saver stands.
enum Phase {
    /// Black until `until_ns`, when the next scene is composed.
    Resting { until_ns: u64 },
    /// The next scene's grids being filled.
    Preparing(Draft),
    /// The scene's pixels being revealed.
    Revealing(Scene),
    /// The picture is whole, and held until `until_ns`.
    Holding { until_ns: u64 },
    /// Fading to black: `strength` of the picture is on screen now.
    Fading { fade: Fade, strength: u8 },
}

/// The ray-traced screensaver.
pub(super) struct Raytrace {
    size: (u32, u32),
    phase: Phase,
    /// The setting of the scene last composed.
    setting: Setting,
    order: Reveal,
    /// What every pixel's samples are hashed under, new with each scene.
    key: u32,
    /// How many pixels of the order are on screen.
    shown: u32,
    /// How much the next frame does.
    batch: u32,
    quality: Quality,
    /// When the reveal began, and when and how far through it the pace was
    /// last judged.
    began_ns: u64,
    judged: (u64, u32),
    /// A frame's traced pixels, each with where it goes.
    traced: Vec<(u32, Pixel)>,
    /// Where they go, kept for its buffers from frame to frame.
    damage: Region,
    encoder: Encoder,
    dice: NonCryptoRng,
    calm: bool,
    due_ns: u64,
}

impl Raytrace {
    /// A reveal for a `size` screen beginning at `now_ns`, `calm` under
    /// reduced motion; `None` when the screen has no pixels or the heap will
    /// not give the first scene.
    pub(super) fn new(size: (u32, u32), calm: bool, now_ns: u64) -> Option<Self> {
        let count = size.0.checked_mul(size.1).filter(|count| *count > 0)?;
        let mut dice = NonCryptoRng::seed_from_u64(seed_from(now_ns));
        let setting = draw_setting(&mut dice, None);
        let draft = Draft::new(setting, dice.next_u64(), aspect(size))?;
        let mut traced = Vec::new();
        if !fallible::reserve(&mut traced, MAX_BATCH as usize) {
            return None;
        }
        Some(Self {
            size,
            phase: Phase::Preparing(draft),
            setting,
            order: Reveal::new(count, dice.next_u64()),
            key: dice.next_u32(),
            shown: 0,
            batch: MIN_BATCH,
            quality: Quality::Fine,
            began_ns: now_ns,
            judged: (now_ns, 0),
            traced,
            damage: Region::new(),
            encoder: Encoder::new()?,
            dice,
            calm,
            due_ns: now_ns,
        })
    }

    /// When the next frame is due.
    pub(super) const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Carry the saver on to `now_ns`, if a frame is due; `clock` reads the
    /// monotonic clock, to measure how long the frame's work took.
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
        // The phase is taken to move on from; resting until now is where
        // any path that sets no other leaves it.
        let phase = core::mem::replace(&mut self.phase, Phase::Resting { until_ns: now_ns });
        self.phase = match phase {
            Phase::Resting { until_ns } if now_ns < until_ns => {
                self.due_ns = until_ns;
                Phase::Resting { until_ns }
            }
            Phase::Resting { .. } => self.compose(now_ns),
            Phase::Preparing(draft) => self.prepare(draft, now_ns, compositor.job_runner(), clock),
            Phase::Revealing(scene) => self
                .reveal(&scene, now_ns, wm, compositor, clock)
                .unwrap_or(Phase::Revealing(scene)),
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
        };
    }

    /// Compose a scene in another setting, to prepare from `now_ns`; when
    /// the heap will not give one, rest a while before trying another.
    fn compose(&mut self, now_ns: u64) -> Phase {
        let setting = draw_setting(&mut self.dice, Some(self.setting));
        self.setting = setting;
        self.batch = MIN_BATCH;
        self.due_ns = now_ns;
        match Draft::new(setting, self.dice.next_u64(), aspect(self.size)) {
            Some(draft) => Phase::Preparing(draft),
            None => self.rest(now_ns),
        }
    }

    /// Rest a while from `now_ns`, the heap having refused a scene, before
    /// composing another.
    fn rest(&mut self, now_ns: u64) -> Phase {
        let until_ns = now_ns.saturating_add(HOLD_NS);
        self.due_ns = until_ns;
        Phase::Resting { until_ns }
    }

    /// Fill the next frame's rows of the scene's grids across `runner`, and
    /// once they are all filled, begin revealing it.
    fn prepare(
        &mut self,
        mut draft: Draft,
        now_ns: u64,
        runner: &dyn JobRunner,
        clock: &mut dyn FnMut() -> u64,
    ) -> Phase {
        let before = draft.remaining();
        let started = clock();
        let remaining = draft.prepare(runner, self.batch);
        let done = before.saturating_sub(remaining);
        self.batch = pace(done, clock().saturating_sub(started), MAX_VERTICES);
        if remaining > 0 {
            self.due_ns = now_ns.saturating_add(SAVER_FRAME_NS);
            return Phase::Preparing(draft);
        }
        let Some(scene) = draft.finish() else {
            return self.rest(now_ns);
        };
        self.order = Reveal::new(self.order.count(), self.dice.next_u64());
        self.key = self.dice.next_u32();
        self.shown = 0;
        self.batch = MIN_BATCH;
        self.quality = Quality::Fine;
        self.began_ns = now_ns;
        self.judged = (now_ns, 0);
        self.due_ns = now_ns;
        Phase::Revealing(scene)
    }

    /// Trace the next frame's pixels of `scene` and put them on screen: the
    /// phase that follows, or `None` while the reveal goes on.
    fn reveal(
        &mut self,
        scene: &Scene,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
        clock: &mut dyn FnMut() -> u64,
    ) -> Option<Phase> {
        let total = self.order.count();
        // A buffer the compositor no longer keeps holds none of the picture,
        // so the picture starts again rather than being kept twice over.
        let kept = compositor.keeps_content(wm, self.size);
        if !kept {
            self.shown = 0;
        }
        let batch = self.batch.min(total.saturating_sub(self.shown));
        let started = clock();
        self.trace(scene, compositor.job_runner(), batch);
        self.batch = pace(batch, clock().saturating_sub(started), MAX_BATCH);
        let width = self.size.0.max(1);
        self.damage.clear();
        if self.traced.len() <= DAMAGE_BUDGET {
            for &(at, _) in &self.traced {
                self.damage.add(pixel_rect(at, width));
            }
        } else {
            let span = self.traced.iter().fold(Rect::EMPTY, |span, &(at, _)| {
                span.union(&pixel_rect(at, width))
            });
            self.damage.add(span);
        }
        let traced = &self.traced;
        let _ = compositor.repaint_window(wm, self.size, &self.damage, |surface, _| {
            if !kept {
                surface.fill(Color::rgb(0, 0, 0));
            }
            for &(at, pixel) in traced {
                surface.set(at % width, at / width, pixel);
            }
        });
        self.shown = self.shown.saturating_add(batch);
        if self.shown >= total {
            let until_ns = now_ns.saturating_add(HOLD_NS);
            self.due_ns = until_ns;
            return Some(Phase::Holding { until_ns });
        }
        self.govern(now_ns, total);
        self.due_ns = now_ns.saturating_add(SAVER_FRAME_NS);
        None
    }

    /// Take fewer samples a pixel for the rest of the reveal when, at the
    /// pace kept since the last judgement, it would run past its budget.
    fn govern(&mut self, now_ns: u64, total: u32) {
        let (since_ns, from) = self.judged;
        let done = self.shown.saturating_sub(from);
        if done < (total / JUDGED_SHARE).max(1) {
            return;
        }
        let left = u64::from(total.saturating_sub(self.shown));
        let rest_ns = now_ns.saturating_sub(since_ns).saturating_mul(left) / u64::from(done);
        let ends_ns = now_ns.saturating_sub(self.began_ns).saturating_add(rest_ns);
        if ends_ns > REVEAL_BUDGET_NS {
            if let Some(lower) = Quality::ALL
                .into_iter()
                .rev()
                .find(|quality| *quality < self.quality)
            {
                self.quality = lower;
            }
        }
        self.judged = (now_ns, self.shown);
    }

    /// Trace the next `batch` pixels of the order into `traced`, spread over
    /// `runner`.
    fn trace(&mut self, scene: &Scene, runner: &dyn JobRunner, batch: u32) {
        self.traced.clear();
        self.traced
            .resize(batch.min(MAX_BATCH) as usize, (0, Pixel::TRANSPARENT));
        let tracer = Tracer::new(scene, &self.encoder, self.size, self.key);
        let (order, quality, width) = (self.order, self.quality, self.size.0.max(1));
        let first = self.shown;
        let work = |start: u32, out: &mut [(u32, Pixel)]| {
            let mut index = start;
            for slot in out {
                let at = order.pixel(index);
                let (pixel, _) = tracer.pixel((at % width, at / width), quality);
                *slot = (at, pixel);
                index = index.saturating_add(1);
            }
        };
        let pieces = tairix_parallel::bands(runner, self.traced.len(), GRAIN);
        let per = self.traced.len().div_ceil(pieces.max(1)).max(1);
        let stride = u32::try_from(per).unwrap_or(u32::MAX);
        let split = fallible::collected(
            pieces,
            self.traced
                .chunks_mut(per)
                .zip((0u32..).map(|piece| first.saturating_add(piece.saturating_mul(stride))))
                .map(|(out, start)| (start, out)),
        );
        match split {
            Some(mut split) if split.len() > 1 => {
                tairix_parallel::for_each(runner, &mut split, &|(start, out)| work(*start, out));
            }
            _ => work(first, &mut self.traced),
        }
    }

    /// Dim the picture, now carrying `strength`, toward black as `fade` has
    /// it at `now_ns`; once it is black, compose the next scene.
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
            return self.compose(now_ns);
        }
        self.due_ns = now_ns.saturating_add(SAVER_FRAME_NS);
        Phase::Fading {
            fade,
            strength: target.min(strength),
        }
    }
}

/// A setting drawn at random, other than `last`.
fn draw_setting(dice: &mut NonCryptoRng, last: Option<Setting>) -> Setting {
    let choices = Setting::ALL.len() - usize::from(last.is_some());
    let mut pick =
        usize::try_from(dice.next_below(u64::try_from(choices).unwrap_or(1))).unwrap_or(0);
    for setting in Setting::ALL {
        if Some(setting) == last {
            continue;
        }
        if pick == 0 {
            return setting;
        }
        pick -= 1;
    }
    Setting::ALL[0]
}

/// A screen's width over its height.
fn aspect((width, height): (u32, u32)) -> f64 {
    f64::from(width.max(1)) / f64::from(height.max(1))
}

/// How much the next frame does, the last having done `done` in `spent_ns`:
/// as much as fits a slice at that pace, but at most twice as much, so one
/// quick frame cannot commit the next to far more than it proved the
/// machine can do, and never more than `most`.
fn pace(done: u32, spent_ns: u64, most: u32) -> u32 {
    let fits = u64::from(done) * SLICE_NS / spent_ns.max(1);
    let grown = u64::from(done.max(MIN_BATCH)).saturating_mul(2);
    u32::try_from(fits.min(grown)).map_or(most, |fits| fits.clamp(MIN_BATCH, most))
}

/// The one-pixel rectangle of pixel `at` of a picture `width` across.
fn pixel_rect(at: u32, width: u32) -> Rect {
    let (x, y) = (at % width, at / width);
    Rect::new(
        i32::try_from(x).unwrap_or(i32::MAX),
        i32::try_from(y).unwrap_or(i32::MAX),
        1,
        1,
    )
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
