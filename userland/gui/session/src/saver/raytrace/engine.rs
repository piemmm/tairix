//! A ray-traced reveal's work, free of threads: composing each scene, filling
//! its grids, and tracing it coarse to fine.
//!
//! The serve loop, where the machine grants no thread, traces a slice a
//! frame: what fits half a desktop frame at the pace the last one kept. A
//! tracing thread of its own traces a stretch at a time instead, every core it
//! was given taking the next untraced step as it finishes the last, so no core
//! waits on another's costly pixel but at a pass's end; a slice holds too few
//! pixels where a pixel costs milliseconds to keep more than one core busy.
//! Every pixel is traced at the tracer's best quality. Once whole, the scene
//! is let go — and, where pictures are kept, the picture handed over to be
//! kept, once for each scene. Each scene is composed at the detail asked,
//! unless the memory band says its peak is not free.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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

/// The longest a stretch runs before its thread takes its turn again: long
/// enough that the pixels a stretch's last cores are still tracing, which the
/// others wait out, are a sliver of it.
pub(super) const STRETCH_NS: u64 = 1_000_000_000;

/// How many steps a stretch traces, across all its cores, between readings of
/// the clock, which is a call into the kernel.
const STEPS_PER_READING: u32 = 16;

/// How many steps a core gathers before handing them over together.
const HANDFUL: usize = 4;

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

/// Where a stretch's cores hand what they trace, from whichever thread traced
/// it.
pub(super) trait Handing: Sync {
    /// Take `steps`, the reveal then standing at `status`; whether to trace
    /// on.
    fn take(&self, steps: &[Traced], status: Status) -> bool;
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
    /// The plan's picture is to be kept: its pixels as traced so far, each a
    /// word whichever core traced it stores, or `None` until tracing begins,
    /// once it is handed over, or when the heap would not hold them.
    Filling(Option<Vec<AtomicU32>>),
}

impl Album {
    /// The picture being filled, if one is.
    fn pixels(&self) -> Option<&[AtomicU32]> {
        match self {
            Self::Filling(Some(pixels)) => Some(pixels),
            Self::Filling(None) | Self::Off => None,
        }
    }
}

/// Lay `pixel` at `step`'s place in `album`, a picture `width` across, if one
/// is kept: every step is a pixel of its own, so no two cores store one word.
fn keep(album: Option<&[AtomicU32]>, width: u32, step: Step, pixel: Pixel) {
    let Some(word) = album.and_then(|album| {
        let at = (step.y as usize)
            .saturating_mul(width as usize)
            .saturating_add(step.x as usize);
        album.get(at)
    }) else {
        return;
    };
    word.store(
        u32::from_le_bytes([pixel.r, pixel.g, pixel.b, pixel.a]),
        Ordering::Relaxed,
    );
}

/// The pixel `word` holds.
const fn unpacked(word: u32) -> Pixel {
    let [r, g, b, a] = word.to_le_bytes();
    Pixel { r, g, b, a }
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
        self.status()
    }

    /// Whether the scene is ready and its pixels are being traced.
    pub(super) const fn is_tracing(&self) -> bool {
        matches!(self.stage, Stage::Tracing(_))
    }

    /// Trace a stretch of the reveal across every participant of `runner`,
    /// each taking the next untraced step as it finishes the last and handing
    /// what it traces to `handing` a handful at a time: to the end of the pass
    /// under way, until `clock` reads `until`, or until `handing` answers that
    /// the reveal is to stop. A pass's steps reach `handing` in no order, but
    /// none before every step of the passes before it. Answers where the
    /// reveal then stands.
    pub(super) fn trace_stretch(
        &mut self,
        runner: &dyn JobRunner,
        handing: &dyn Handing,
        (clock, until): (&(dyn Fn() -> u64 + Sync), u64),
    ) -> Status {
        let stage = core::mem::replace(&mut self.stage, Stage::Failed);
        let Stage::Tracing(scene) = stage else {
            self.stage = stage;
            return self.status();
        };
        let total = self.reveal.count();
        let stretch = Stretch {
            next: AtomicU32::new(self.shown),
            traced: AtomicU32::new(self.shown),
            over: AtomicBool::new(false),
            end: self.reveal.pass_end(self.shown),
            total,
            handing,
            clock,
            until,
        };
        let tracer = Tracer::new(&scene, &self.encoder, self.size, self.plan.key);
        let (reveal, album, width) = (&self.reveal, self.album.pixels(), self.size.0);
        runner.run(runner.width().max(1), &|_| {
            let mut handful = [Traced::NONE; HANDFUL];
            let mut held = 0;
            while let Some(index) = stretch.claim() {
                let Some(step) = reveal.step(index) else {
                    continue;
                };
                let (pixel, _) = tracer.pixel((step.x, step.y), QUALITY);
                keep(album, width, step, pixel);
                if let Some(slot) = handful.get_mut(held) {
                    *slot = Traced { step, pixel };
                    held += 1;
                }
                if held == HANDFUL {
                    stretch.hand(&handful);
                    held = 0;
                }
            }
            stretch.hand(handful.get(..held).unwrap_or(&[]));
        });
        // Every step claimed was traced and handed over before its core
        // looked for another.
        self.shown = stretch.next.load(Ordering::Relaxed);
        self.stage = if self.shown >= total {
            self.finish_album();
            Stage::Whole
        } else {
            Stage::Tracing(scene)
        };
        self.status()
    }

    /// Where the reveal stands.
    fn status(&self) -> Status {
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
            *pixels = count.and_then(|count| {
                fallible::collected(count, core::iter::repeat_with(|| AtomicU32::new(0)))
            });
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
        let per = tairix_parallel::piece_len(slots.len(), pieces);
        let stride = u32::try_from(per).unwrap_or(u32::MAX);
        let first = self.shown;
        let split = slots
            .chunks_mut(per)
            .zip((0u32..).map(|piece| first.saturating_add(piece.saturating_mul(stride))));
        tairix_parallel::for_each_drawn(runner, split, &|(out, start)| work(start, out));
    }

    /// Lay what `traced` traced into the copy of the picture, if one is kept.
    fn copy_into_album(&self, traced: &[Traced]) {
        let album = self.album.pixels();
        for traced in traced {
            keep(album, self.size.0, traced.step, traced.pixel);
        }
    }

    /// Hand the whole picture over to be kept, or why it cannot be.
    fn finish_album(&mut self) {
        let Album::Filling(words) = &mut self.album else {
            return;
        };
        let pixels = words.take().and_then(|words| {
            let pixels = words
                .iter()
                .map(|word| unpacked(word.load(Ordering::Relaxed)));
            fallible::collected(words.len(), pixels)
        });
        self.finished = Some(match pixels {
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

/// What the cores tracing one stretch share: the next step to claim, how many
/// are traced, whether the stretch is over, and where it ends.
struct Stretch<'a> {
    next: AtomicU32,
    traced: AtomicU32,
    over: AtomicBool,
    /// The step the stretch's pass ends before.
    end: u32,
    /// How many steps the whole reveal takes.
    total: u32,
    handing: &'a dyn Handing,
    clock: &'a (dyn Fn() -> u64 + Sync),
    until: u64,
}

impl Stretch<'_> {
    /// The next untraced step, unless the stretch is over: claimed only below
    /// its end, so the count never passes it.
    fn claim(&self) -> Option<u32> {
        if self.over.load(Ordering::Relaxed) {
            return None;
        }
        self.next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                (next < self.end).then(|| next + 1)
            })
            .ok()
    }

    /// Hand `steps` over, ending the stretch once `handing` answers it is to
    /// stop or the clock reaches its end.
    fn hand(&self, steps: &[Traced]) {
        if steps.is_empty() {
            return;
        }
        let count = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        let before = self.traced.fetch_add(count, Ordering::Relaxed);
        let done = before.saturating_add(count);
        let on = self
            .handing
            .take(steps, Status::Tracing(thousandths(done, self.total)));
        let read = before / STEPS_PER_READING != done / STEPS_PER_READING;
        if !on || (read && (self.clock)() >= self.until) {
            self.over.store(true, Ordering::Relaxed);
        }
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
