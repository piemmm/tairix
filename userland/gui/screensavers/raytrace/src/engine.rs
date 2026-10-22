//! A ray-traced reveal's work, free of threads: composing each scene, filling
//! its grids, and tracing it coarse to fine.
//!
//! The frame loop, where the saver is granted no thread, traces a slice a
//! frame: what fits half a display frame at the pace the last one kept. A
//! tracing thread of its own traces a stretch at a time instead, every core it
//! was given taking the next untraced step as it finishes the last, so no core
//! waits on another's costly pixel but at a pass's end; a slice holds too few
//! pixels where a pixel costs milliseconds to keep more than one core busy.
//! Every pixel is traced at the tracer's best quality. Once whole, the scene
//! is let go. Each scene is composed at the detail asked, unless the memory
//! band says its peak is not free.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tairix_parallel::JobRunner;
use tairix_raster::Pixel;
use tairix_raytrace::{Detail, Draft, Encoder, Quality, Reveal, Scene, Setting, Step, Tracer};
use tairix_reclaim::pressure::{PressureGauge, PressureThresholds};
use tairix_reclaim::{CacheBudget, ReclaimClass};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::fallible;

use super::crew::Status;

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

/// What memory can be spared a scene: the pressure band last relayed, over
/// the memory the saver was granted.
#[derive(Copy, Clone)]
pub struct Memory {
    /// The bytes of address space the saver was granted.
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

    /// Whether the band lets a buffer the drawing could do without hold
    /// `bytes` now, as it lets a disposable cache of the granted memory hold
    /// them.
    #[must_use]
    pub fn holds_disposable(self, bytes: u64) -> bool {
        let budget = CacheBudget::from_backing(usize::try_from(self.total).unwrap_or(usize::MAX));
        usize::try_from(bytes).is_ok_and(|bytes| {
            self.gauge
                .growth_permitted(ReclaimClass::DisposableUi, budget, bytes)
        })
    }
}

/// How a run's scenes are set out: at the detail asked, unless the memory
/// cannot spare its peak.
#[derive(Copy, Clone)]
pub struct Detailing {
    /// The detail asked for.
    pub asked: Detail,
    /// What the saver's memory can spare.
    pub memory: Memory,
}

/// How tests set their engines' scenes out: simply, as the default does, with
/// memory that spares anything.
#[cfg(test)]
pub(crate) const PLAIN: Detailing = Detailing {
    asked: Detail::Simple,
    memory: Memory {
        total: u64::MAX,
        gauge: &tairix_reclaim::pressure::Unpressured,
    },
};

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
    /// How long a slice may take.
    slice_ns: u64,
    detailing: Detailing,
}

impl Engine {
    /// The reveals of a `size` screen, their scenes drawn from `seed` and set
    /// out as `detailing` has them, traced in slices of `slice_ns` — a
    /// frame loop's own slice, so a tracing thread also notices within one
    /// that the loop has gone; `None` when the screen has no pixels or the
    /// heap will not hold the encoder.
    #[must_use]
    pub fn new(size: (u32, u32), seed: u64, detailing: Detailing, slice_ns: u64) -> Option<Self> {
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
            slice_ns,
            detailing,
        })
    }

    /// The detail its scenes are asked to be set out at.
    #[cfg(test)]
    pub(super) const fn asked(&self) -> Detail {
        self.detailing.asked
    }

    /// Begin a scene in another setting, composed from its start.
    pub(super) fn next(&mut self) {
        self.plan = Plan::draw(&mut self.dice, Some(self.plan.setting));
        if let Some(reveal) = Reveal::new(self.size, self.plan.order) {
            self.reveal = reveal;
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
        let reveal = &self.reveal;
        runner.run(runner.width().max(1), &|_| {
            let mut handful = [Traced::NONE; HANDFUL];
            let mut held = 0;
            while let Some(index) = stretch.claim() {
                let Some(step) = reveal.step(index) else {
                    continue;
                };
                let (pixel, _) = tracer.pixel((step.x, step.y), QUALITY);
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
            Stage::Whole => Status::Whole(self.plan.setting),
            Stage::Failed => Status::Failed,
        }
    }

    fn compose(&mut self) {
        self.stage = Stage::Composing;
        self.batch = MIN_BATCH;
    }

    /// The detail the next scene is composed at: Simple, where the memory
    /// cannot spare the peak of the detail asked.
    fn detail(&self) -> Detail {
        let Detailing { asked, memory } = self.detailing;
        if memory.spares(asked.peak()) {
            asked
        } else {
            Detail::Simple
        }
    }

    /// Begin the reveal from its first step.
    fn begin_trace(&mut self) {
        self.shown = 0;
        self.batch = MIN_BATCH;
    }

    /// Do a slice of `draft`'s work across `runner`, and once it is all done,
    /// begin tracing the scene.
    fn prepare(
        &mut self,
        mut draft: Draft,
        runner: &dyn JobRunner,
        clock: &mut dyn FnMut() -> u64,
    ) -> Stage {
        let until = clock().saturating_add(self.slice_ns);
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
        }
        self.batch = pace(batch, clock().saturating_sub(started), self.slice_ns, total);
        self.shown = self.shown.saturating_add(batch);
        if self.shown >= total {
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

/// How much the next slice of `slice_ns` does, the last having done `done`
/// in `spent_ns`: as much as fits a slice at that pace, but at most twice as
/// much, so one quick slice cannot commit the next to far more than it proved
/// the machine can do, and never more than `most`.
fn pace(done: u32, spent_ns: u64, slice_ns: u64, most: u32) -> u32 {
    let fits = u64::from(done) * slice_ns / spent_ns.max(1);
    let grown = u64::from(done.max(MIN_BATCH)).saturating_mul(2);
    u32::try_from(fits.min(grown)).map_or(most, |fits| fits.clamp(MIN_BATCH, most.max(MIN_BATCH)))
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
