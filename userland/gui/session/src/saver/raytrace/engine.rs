//! A ray-traced reveal's work, free of threads: composing each scene, filling
//! its grids, and tracing it coarse to fine, a slice at a time.
//!
//! A slice is what fits half a desktop frame at the pace the last one kept,
//! so one engine serves both a tracing thread of its own, which runs slice
//! after slice, and the serve loop, which runs one a frame when the machine
//! grants no thread. A reveal whose pace would outrun its budget takes fewer
//! samples a pixel for the rest of it. Once whole, the scene is let go.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_raster::Pixel;
use tairix_raytrace::{Block, Draft, Encoder, Quality, Reveal, Scene, Setting, Tracer};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_theme::Timeline;
use tairix_util::fallible;

use super::crew::{Request, Status};

/// How long a reveal may take before the rest of it is traced with fewer
/// samples a pixel: long enough that a desktop-class machine never reaches
/// it, short enough that a slow one still shows a new scene every few
/// minutes.
pub(super) const REVEAL_BUDGET_NS: u64 = 240_000_000_000;

/// How much work a slice may be: half of one desktop frame, so a serve loop
/// tracing slices itself still answers input and every client within the
/// frame, and a tracing thread notices the loop has gone within one.
pub(super) const SLICE_NS: u64 = Timeline::FRAME_NS / 2;

/// The fewest pixels a slice traces, which each reveal starts from.
const MIN_BATCH: u32 = 1;

/// The fewest pixels worth handing another core: one, for a pixel's samples
/// cost far more than the hand-off.
const GRAIN: usize = 1;

/// Of the picture, how much is traced between the governor's judgements of
/// the pace: enough, scattered as each pass is, to stand for the whole.
const JUDGED_SHARE: u32 = 64;

// A launched engine is moved onto the thread that traces it.
const _: () = {
    const fn sendable<T: Send>() {}
    sendable::<Engine>();
};

/// One traced step of a reveal: the part of the picture it covers, and the
/// colour it shows there.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Traced {
    /// Where the colour goes.
    pub block: Block,
    /// The traced pixel.
    pub pixel: Pixel,
}

impl Traced {
    /// A block covering no pixel, which paints nothing.
    const NONE: Self = Self {
        block: Block {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
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

/// When tracing began, and when and how far through it the pace was last
/// judged.
#[derive(Copy, Clone, Debug)]
struct Timing {
    began_ns: u64,
    judged: (u64, u32),
}

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
    quality: Quality,
    /// `None` until the reveal's first slice reads the clock.
    timing: Option<Timing>,
}

impl Engine {
    /// The reveals of a `size` screen, their scenes drawn from `seed`; `None`
    /// when the screen has no pixels or the heap will not hold the encoder.
    #[must_use]
    pub fn new(size: (u32, u32), seed: u64) -> Option<Self> {
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
            quality: Quality::Fine,
            timing: None,
        })
    }

    /// Take up `request`: another scene, or the current one again from its
    /// first step, recomposed from its plan if it has already been let go.
    pub(super) fn apply(&mut self, request: Request) {
        match request {
            Request::Next => {
                self.plan = Plan::draw(&mut self.dice, Some(self.plan.setting));
                if let Some(reveal) = Reveal::new(self.size, self.plan.order) {
                    self.reveal = reveal;
                }
                self.compose();
            }
            Request::Again => match self.stage {
                Stage::Tracing(_) => self.restart_trace(),
                Stage::Whole | Stage::Failed => self.compose(),
                Stage::Composing | Stage::Preparing(_) => {}
            },
        }
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
            Stage::Composing => Draft::new(self.plan.setting, self.plan.seed, self.size)
                .map_or(Stage::Failed, Stage::Preparing),
            Stage::Preparing(draft) => self.prepare(draft, runner, clock),
            Stage::Tracing(scene) => self.trace(scene, runner, out, clock),
            done @ (Stage::Whole | Stage::Failed) => done,
        };
        match self.stage {
            Stage::Whole => Status::Whole,
            Stage::Failed => Status::Failed,
            Stage::Composing | Stage::Preparing(_) | Stage::Tracing(_) => Status::Working,
        }
    }

    fn compose(&mut self) {
        self.stage = Stage::Composing;
        self.batch = MIN_BATCH;
    }

    fn restart_trace(&mut self) {
        self.shown = 0;
        self.batch = MIN_BATCH;
        self.quality = Quality::Fine;
        self.timing = None;
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
                    self.restart_trace();
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
        self.timing.get_or_insert(Timing {
            began_ns: started,
            judged: (started, self.shown),
        });
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
        let now_ns = clock();
        self.batch = pace(batch, now_ns.saturating_sub(started), total);
        self.shown = self.shown.saturating_add(batch);
        if self.shown >= total {
            return Stage::Whole;
        }
        self.govern(now_ns, total);
        Stage::Tracing(scene)
    }

    /// Trace the steps from the next untraced one into `slots`, spread over
    /// `runner`.
    fn trace_into(&self, scene: &Scene, runner: &dyn JobRunner, slots: &mut [Traced]) {
        let tracer = Tracer::new(scene, &self.encoder, self.size, self.plan.key);
        let (reveal, quality) = (&self.reveal, self.quality);
        let work = |start: u32, out: &mut [Traced]| {
            let mut index = start;
            for slot in out {
                if let Some(block) = reveal.block(index) {
                    let (pixel, _) = tracer.pixel((block.x, block.y), quality);
                    *slot = Traced { block, pixel };
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

    /// Take fewer samples a pixel for the rest of the reveal when, at the
    /// pace kept since the last judgement, it would run past its budget.
    fn govern(&mut self, now_ns: u64, total: u32) {
        let Some(timing) = self.timing.as_mut() else {
            return;
        };
        let (since_ns, from) = timing.judged;
        let done = self.shown.saturating_sub(from);
        if done < (total / JUDGED_SHARE).max(1) {
            return;
        }
        let left = u64::from(total.saturating_sub(self.shown));
        let rest_ns = now_ns.saturating_sub(since_ns).saturating_mul(left) / u64::from(done);
        let ends_ns = now_ns
            .saturating_sub(timing.began_ns)
            .saturating_add(rest_ns);
        if ends_ns > REVEAL_BUDGET_NS {
            if let Some(lower) = Quality::ALL
                .into_iter()
                .rev()
                .find(|quality| *quality < self.quality)
            {
                self.quality = lower;
            }
        }
        timing.judged = (now_ns, self.shown);
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
