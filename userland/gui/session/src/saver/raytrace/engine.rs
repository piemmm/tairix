//! A ray-traced reveal's work, free of threads: composing each scene, filling
//! its grids, and tracing it coarse to fine, a slice at a time.
//!
//! A slice is what fits half a desktop frame at the pace the last one kept,
//! so one engine serves both a tracing thread of its own, which runs slice
//! after slice, and the serve loop, which runs one a frame when the machine
//! grants no thread. Every pixel is traced at the tracer's best quality. Once
//! whole, the scene is let go — and, where pictures are kept, the picture
//! handed over to be kept, once for each scene. Each scene is composed at the
//! detail asked, unless the memory band says its peak is not free.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_raster::Pixel;
use tairix_raytrace::{Detail, Draft, Encoder, Quality, Reveal, Scene, Setting, Step, Tracer};
use tairix_reclaim::pressure::{PressureGauge, PressureThresholds};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_theme::Timeline;
use tairix_util::fallible;

use super::album::{Picture, Unkept};
use super::crew::Status;

/// How much work a slice may be: half of one desktop frame, so a serve loop
/// tracing slices itself still answers input and every client within the
/// frame, and a tracing thread notices the loop has gone within one.
pub(super) const SLICE_NS: u64 = Timeline::FRAME_NS / 2;

/// How every pixel is traced: the best the tracer has.
const QUALITY: Quality = Quality::Fine;

/// The fewest pixels a slice traces, which each reveal starts from.
const MIN_BATCH: u32 = 1;

/// The fewest pixels worth handing another core: one, for a pixel's samples
/// cost far more than the hand-off.
const GRAIN: usize = 1;

/// A reveal's progress once it is whole, in thousandths.
const WHOLE: u16 = 1000;

// A launched engine is moved onto the thread that traces it.
const _: () = {
    const fn sendable<T: Send>() {}
    sendable::<Engine>();
};

/// One traced step of a reveal: the point of its pass's grid it traced, and
/// the colour it shows there.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Traced {
    /// Where the colour goes, and the grid it is a point of.
    pub step: Step,
    /// The traced pixel.
    pub pixel: Pixel,
}

impl Traced {
    /// A step of no grid, which paints nothing.
    const NONE: Self = Self {
        step: Step {
            x: 0,
            y: 0,
            side: 0,
        },
        pixel: Pixel::TRANSPARENT,
    };
}

/// What a scene is drawn from: its setting and seed, the key of the order it
/// is revealed in, and the key every pixel's samples are hashed under.
#[derive(Copy, Clone, Debug)]
struct Plan {
    setting: Setting,
    seed: u64,
    order: u64,
    key: u32,
}

impl Plan {
    /// A plan drawn from `dice`, in a setting other than `last`.
    fn draw(dice: &mut NonCryptoRng, last: Option<Setting>) -> Self {
        Self {
            setting: draw_setting(dice, last),
            seed: dice.next_u64(),
            order: dice.next_u64(),
            key: dice.next_u32(),
        }
    }
}

/// Where the scene stands.
#[allow(
    clippy::large_enum_variant,
    reason = "an engine holds one stage, so the room a draft sets is paid once; boxing it \
              would trade that for an allocation that cannot fail gracefully"
)]
enum Stage {
    /// Still to be composed.
    Composing,
    /// Its grids being filled.
    Preparing(Draft),
    /// Its pixels being traced.
    Tracing(Scene),
    /// Every pixel traced, and the scene let go.
    Whole,
    /// The heap would not hold the scene, or what it traced.
    Failed,
}

/// The copy of a picture an engine keeps as it traces it, to hand over once
/// it is whole.
enum Album {
    /// Pictures are not kept.
    Off,
    /// The plan's picture is to be kept: its pixels as traced so far, or
    /// `None` until tracing begins, once it is handed over, or when the heap
    /// would not hold them.
    Filling(Option<Vec<Pixel>>),
}

/// What the machine's memory can spare a scene: the pressure band the
/// kernel last reported, over the memory the machine holds.
#[derive(Copy, Clone)]
pub struct Memory {
    /// The bytes of memory the machine holds.
    pub total: u64,
    /// Where the band is read.
    pub gauge: &'static dyn PressureGauge,
}

impl Memory {
    /// Whether `peak` bytes may be free now. A band deeper than normal holds
    /// at most its exit watermark free, being left once more than that is.
    #[must_use]
    pub fn spares(self, peak: u64) -> bool {
        let total = usize::try_from(self.total).unwrap_or(usize::MAX);
        let most_free = match self.gauge.sample().depth().checked_sub(1) {
            None => total,
            Some(deeper) => PressureThresholds::from_total(total)
                .exit_watermarks()
                .get(usize::from(deeper))
                .copied()
                .unwrap_or(0),
        };
        u64::try_from(most_free).unwrap_or(u64::MAX) >= peak
    }
}

/// How a run's scenes are set out: at the detail asked, unless the memory
/// cannot spare its peak.
#[derive(Copy, Clone)]
pub struct Detailing {
    /// The detail asked for.
    pub asked: Detail,
    /// What the machine's memory can spare.
    pub memory: Memory,
    /// Told the detail scenes are set out at whenever it changes from the
    /// last scene's.
    pub tell: fn(Detail),
}

/// How tests set their engines' scenes out: simply, as the default does, on a
/// machine whose memory spares anything, telling nothing.
#[cfg(test)]
pub(crate) const PLAIN: Detailing = Detailing {
    asked: Detail::Simple,
    memory: Memory {
        total: u64::MAX,
        gauge: &tairix_reclaim::pressure::Unpressured,
    },
    tell: quiet,
};

#[cfg(test)]
const fn quiet(_: Detail) {}

/// The reveals of one screen, one scene after another.
pub struct Engine {
    size: (u32, u32),
    encoder: Encoder,
    dice: NonCryptoRng,
    plan: Plan,
    reveal: Reveal,
    stage: Stage,
    /// How many steps of the reveal are traced.
    shown: u32,
    /// How much the next slice does.
    batch: u32,
    album: Album,
    /// A whole picture, or the reason it is not, for the keeper to collect.
    finished: Option<Result<Picture, Unkept>>,
    detailing: Detailing,
    /// The detail the last scene was composed at.
    composed: Detail,
}

impl Engine {
    /// The reveals of a `size` screen, their scenes drawn from `seed` and set
    /// out as `detailing` has them; `None` when the screen has no pixels or
    /// the heap will not hold the encoder.
    #[must_use]
    pub fn new(size: (u32, u32), seed: u64, detailing: Detailing) -> Option<Self> {
        let mut dice = NonCryptoRng::seed_from_u64(seed);
        let plan = Plan::draw(&mut dice, None);
        Some(Self {
            size,
            reveal: Reveal::new(size, plan.order)?,
            encoder: Encoder::new()?,
            dice,
            plan,
            stage: Stage::Composing,
            shown: 0,
            batch: MIN_BATCH,
            album: Album::Off,
            finished: None,
            detailing,
            composed: detailing.asked,
        })
    }

    /// Keep every whole picture from here on, for
    /// [`take_finished`](Self::take_finished) to hand over.
    pub fn keep_pictures(&mut self) {
        if matches!(self.album, Album::Off) {
            self.album = Album::Filling(None);
        }
    }

    /// The last whole picture to be kept, or why it could not be, once.
    pub fn take_finished(&mut self) -> Option<Result<Picture, Unkept>> {
        self.finished.take()
    }

    /// Begin a scene in another setting, composed from its start.
    pub(super) fn next(&mut self) {
        self.plan = Plan::draw(&mut self.dice, Some(self.plan.setting));
        if let Some(reveal) = Reveal::new(self.size, self.plan.order) {
            self.reveal = reveal;
        }
        if !matches!(self.album, Album::Off) {
            self.album = Album::Filling(None);
        }
        self.compose();
    }

    /// Do one slice, appending what it traces to `out`, and answer where the
    /// reveal then stands; `clock` reads the monotonic clock, to pace it.
    pub(super) fn step(
        &mut self,
        runner: &dyn JobRunner,
        out: &mut Vec<Traced>,
        clock: &mut dyn FnMut() -> u64,
    ) -> Status {
        let stage = core::mem::replace(&mut self.stage, Stage::Failed);
        self.stage = match stage {
            Stage::Composing => {
                let detail = self.detail();
                Draft::new(self.plan.setting, self.plan.seed, self.size, detail)
                    .map_or(Stage::Failed, Stage::Preparing)
            }
            Stage::Preparing(draft) => self.prepare(draft, runner, clock),
            Stage::Tracing(scene) => self.trace(scene, runner, out, clock),
            done @ (Stage::Whole | Stage::Failed) => done,
        };
        match &self.stage {
            Stage::Composing => Status::Preparing(0),
            Stage::Preparing(draft) => Status::Preparing(draft.progress()),
            Stage::Tracing(_) => Status::Tracing(thousandths(self.shown, self.reveal.count())),
            Stage::Whole => Status::Whole,
            Stage::Failed => Status::Failed,
        }
    }

    fn compose(&mut self) {
        self.stage = Stage::Composing;
        self.batch = MIN_BATCH;
    }

    /// The detail the next scene is composed at: Simple, where the memory
    /// cannot spare the peak of the detail asked; told when it changes.
    fn detail(&mut self) -> Detail {
        let Detailing {
            asked,
            memory,
            tell,
        } = self.detailing;
        let detail = if memory.spares(asked.peak()) {
            asked
        } else {
            Detail::Simple
        };
        if detail != self.composed {
            self.composed = detail;
            tell(detail);
        }
        detail
    }

    /// Begin the reveal from its first step, readying the copy of the picture
    /// if one is kept.
    fn begin_trace(&mut self) {
        self.shown = 0;
        self.batch = MIN_BATCH;
        if let Album::Filling(pixels @ None) = &mut self.album {
            let count = usize::try_from(u64::from(self.size.0) * u64::from(self.size.1)).ok();
            *pixels = count.and_then(|count| fallible::filled(count, Pixel::TRANSPARENT));
        }
    }

    /// Do a slice of `draft`'s work across `runner`, and once it is all done,
    /// begin tracing the scene.
    fn prepare(
        &mut self,
        mut draft: Draft,
        runner: &dyn JobRunner,
        clock: &mut dyn FnMut() -> u64,
    ) -> Stage {
        let until = clock().saturating_add(SLICE_NS);
        match draft.prepare(runner, &mut || clock() >= until) {
            Some(false) => Stage::Preparing(draft),
            Some(true) => match draft.finish() {
                Some(scene) => {
                    self.begin_trace();
                    Stage::Tracing(scene)
                }
                None => Stage::Failed,
            },
            None => Stage::Failed,
        }
    }

    /// Trace the slice's steps of `scene` into `out`, letting the scene go
    /// once its last step is traced.
    fn trace(
        &mut self,
        scene: Scene,
        runner: &dyn JobRunner,
        out: &mut Vec<Traced>,
        clock: &mut dyn FnMut() -> u64,
    ) -> Stage {
        let total = self.reveal.count();
        let started = clock();
        let batch = self.batch.min(total.saturating_sub(self.shown));
        let first = out.len();
        let Some(end) = first.checked_add(batch as usize) else {
            return Stage::Failed;
        };
        if !fallible::grow_to(out, end, Traced::NONE) {
            return Stage::Failed;
        }
        if let Some(slots) = out.get_mut(first..) {
            self.trace_into(&scene, runner, slots);
            self.copy_into_album(slots);
        }
        self.batch = pace(batch, clock().saturating_sub(started), total);
        self.shown = self.shown.saturating_add(batch);
        if self.shown >= total {
            self.finish_album();
            return Stage::Whole;
        }
        Stage::Tracing(scene)
    }

    /// Trace the steps from the next untraced one into `slots`, spread over
    /// `runner`.
    fn trace_into(&self, scene: &Scene, runner: &dyn JobRunner, slots: &mut [Traced]) {
        let tracer = Tracer::new(scene, &self.encoder, self.size, self.plan.key);
        let reveal = &self.reveal;
        let work = |start: u32, out: &mut [Traced]| {
            let mut index = start;
            for slot in out {
                if let Some(step) = reveal.step(index) {
                    let (pixel, _) = tracer.pixel((step.x, step.y), QUALITY);
                    *slot = Traced { step, pixel };
                }
                index = index.saturating_add(1);
            }
        };
        let pieces = tairix_parallel::bands(runner, slots.len(), GRAIN);
        let per = slots.len().div_ceil(pieces.max(1)).max(1);
        let stride = u32::try_from(per).unwrap_or(u32::MAX);
        let first = self.shown;
        let split = fallible::collected(
            pieces,
            slots
                .chunks_mut(per)
                .zip((0u32..).map(|piece| first.saturating_add(piece.saturating_mul(stride))))
                .map(|(out, start)| (start, out)),
        );
        match split {
            Some(mut split) if split.len() > 1 => {
                tairix_parallel::for_each(runner, &mut split, &|(start, out)| work(*start, out));
            }
            _ => work(first, slots),
        }
    }

    /// Lay what `traced` traced into the copy of the picture, if one is kept.
    fn copy_into_album(&mut self, traced: &[Traced]) {
        let Album::Filling(Some(pixels)) = &mut self.album else {
            return;
        };
        let width = self.size.0 as usize;
        for traced in traced {
            let at = (traced.step.y as usize)
                .saturating_mul(width)
                .saturating_add(traced.step.x as usize);
            if let Some(pixel) = pixels.get_mut(at) {
                *pixel = traced.pixel;
            }
        }
    }

    /// Hand the whole picture over to be kept, or why it cannot be.
    fn finish_album(&mut self) {
        let Album::Filling(pixels) = &mut self.album else {
            return;
        };
        self.finished = Some(match pixels.take() {
            Some(pixels) => Ok(Picture {
                setting: self.plan.setting,
                seed: self.plan.seed,
                size: self.size,
                pixels,
            }),
            None => Err(Unkept::Unheld(self.plan.setting)),
        });
    }
}

/// `done` of `total` in thousandths, below the whole until all are done.
fn thousandths(done: u32, total: u32) -> u16 {
    let share = u64::from(done) * u64::from(WHOLE) / u64::from(total.max(1));
    u16::try_from(share).map_or(WHOLE - 1, |share| share.min(WHOLE - 1))
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

/// How much the next slice does, the last having done `done` in `spent_ns`:
/// as much as fits a slice at that pace, but at most twice as much, so one
/// quick slice cannot commit the next to far more than it proved the machine
/// can do, and never more than `most`.
fn pace(done: u32, spent_ns: u64, most: u32) -> u32 {
    let fits = u64::from(done) * SLICE_NS / spent_ns.max(1);
    let grown = u64::from(done.max(MIN_BATCH)).saturating_mul(2);
    u32::try_from(fits.min(grown)).map_or(most, |fits| fits.clamp(MIN_BATCH, most.max(MIN_BATCH)))
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
