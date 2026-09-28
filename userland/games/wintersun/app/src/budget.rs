//! What a frame is allowed to cost, and how `auto` answers a frame that
//! costs more.
//!
//! "Playable" is not a budget and a renderer without one cannot be
//! reviewed, so the passes each have an allocation at the baseline —
//! 1280×720 at 60 Hz on a four-core machine. The headroom is *derived* from
//! the frame and the named passes rather than stated beside them: two
//! numbers that must add up are one number and a subtraction.
//!
//! # Reading the machine over seconds, not frames
//!
//! A frame that runs late says the machine was late, not why. A few slow
//! frames are usually something else on the machine having a moment, and a
//! renderer that shed detail on them gives the picture away every time a
//! window is dragged. So [`Governor`] works from what frames have cost over
//! seconds:
//!
//! - **Cost is measured per render pixel**, so a window that grows or shrinks
//!   changes the frame's predicted cost at once without any history being
//!   thrown away, and a larger window does not look like a slower machine.
//! - **Each step of the ladder remembers the cheapest frame it drew.** That
//!   is what the machine does at that detail when nothing else wants it, and
//!   the ratio of what frames cost now to it is how busy the machine is,
//!   smoothed over several seconds.
//! - **It sheds one notch at a time, and slowly when the machine has shown
//!   it can keep up**: a step whose best frame fits is only given up after
//!   overrunning for several seconds, and one whose best frame does not fit
//!   after one.
//! - **It gives a notch back one at a time, on a prediction**: the finer
//!   step's best frame, scaled by how busy the machine is now and by its
//!   pixels, must fit well inside the budget for several seconds. A notch
//!   given back is on trial, and one that overruns during it is taken away
//!   again after a second. What a step once cost is believed less the longer
//!   ago it was seen, so a step found too dear is tried again only once the
//!   evidence against it has aged.
//! - **Frames just after a move or a resize are not counted**: the first
//!   frames at a new size or detail pay for buffers and textures once, and
//!   that is not what drawing costs.
//!
//! Work the frame paid for once to make its inputs resident — a material
//! tile synthesised — is recorded apart from the passes ([`FrameTimes::warm`])
//! and not counted either: it is not a steady cost of drawing, and shedding
//! detail to pay for it would give the picture away for nothing.

use crate::quality::Ladder;
use crate::view::Viewport;

/// Nanoseconds in a second.
const NS_PER_SEC: u64 = 1_000_000_000;

/// The refresh the budget is stated at.
pub const BASELINE_HZ: u64 = 60;

/// The whole frame's budget, in nanoseconds.
pub const FRAME_NS: u64 = NS_PER_SEC / BASELINE_HZ;

/// The width the budget is stated at.
pub const BASELINE_WIDTH: u32 = 1280;

/// The height the budget is stated at.
pub const BASELINE_HEIGHT: u32 = 720;

/// One measured stage of a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Pass {
    /// Ground blend and detail: the ground itself.
    Terrain,
    /// Light, fog and atmosphere composite over the ground.
    Light,
    /// Ground scenery, entities and figures, each shaded by its own
    /// surfaces and veiled by the air at its feet.
    Scenery,
    /// Particles and weather.
    Particles,
    /// Overlays the player reads rather than plays in.
    Ui,
}

impl Pass {
    /// Every pass, in the order a frame runs them.
    pub const ALL: [Self; 5] = [
        Self::Terrain,
        Self::Light,
        Self::Scenery,
        Self::Particles,
        Self::Ui,
    ];

    /// What this pass is allowed to cost at the baseline, in nanoseconds.
    #[must_use]
    pub const fn budget_ns(self) -> u64 {
        match self {
            Self::Terrain => 5_000_000,
            Self::Scenery => 3_500_000,
            Self::Particles | Self::Light => 2_000_000,
            Self::Ui => 1_000_000,
        }
    }

    /// Its index in a [`FrameTimes`].
    const fn slot(self) -> usize {
        self as usize
    }
}

/// What is left of a frame once every pass has had its allocation:
/// present, input and jitter.
#[must_use]
pub const fn headroom_ns() -> u64 {
    let mut spent = 0;
    let mut i = 0;
    while i < Pass::ALL.len() {
        spent += Pass::ALL[i].budget_ns();
        i += 1;
    }
    FRAME_NS.saturating_sub(spent)
}

/// What a frame's passes may cost together, leaving the headroom the
/// present needs.
#[must_use]
pub const fn drawing_ns() -> u64 {
    FRAME_NS.saturating_sub(headroom_ns())
}

/// What one frame actually cost, per pass.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameTimes {
    spent: [u64; Pass::ALL.len()],
    warm: u64,
}

impl FrameTimes {
    /// Nothing measured yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spent: [0; Pass::ALL.len()],
            warm: 0,
        }
    }

    /// Record what a pass cost.
    pub fn record(&mut self, pass: Pass, ns: u64) {
        self.spent[pass.slot()] = ns;
    }

    /// Record what the frame spent making its inputs resident, which no
    /// pass is charged for.
    pub fn record_warm(&mut self, ns: u64) {
        self.warm = ns;
    }

    /// What a pass cost.
    #[must_use]
    pub const fn spent(&self, pass: Pass) -> u64 {
        self.spent[pass.slot()]
    }

    /// What the frame spent making its inputs resident: synthesising the
    /// material tiles its view needed and the cache did not hold.
    #[must_use]
    pub const fn warm(&self) -> u64 {
        self.warm
    }

    /// What every pass cost together: the frame's steady cost of drawing.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.spent.iter().copied().fold(0, u64::saturating_add)
    }

    /// Whether the frame fitted, leaving the headroom the present needs.
    #[must_use]
    pub fn within_budget(&self) -> bool {
        self.total() <= drawing_ns()
    }

    /// Every pass that cost more than its own allocation.
    pub fn overruns(&self) -> impl Iterator<Item = (Pass, u64)> + '_ {
        Pass::ALL
            .into_iter()
            .filter(move |p| self.spent(*p) > p.budget_ns())
            .map(move |p| (p, self.spent(p)))
    }
}

/// The fixed point a per-pixel cost is held in: nanoseconds a render pixel,
/// times this.
const RATE_ONE: u64 = 1 << 16;

/// The fixed point a busy factor is held in: the machine drawing at its best.
const LOAD_ONE: u64 = 1 << 16;

/// The busiest a single frame is read as, so one stalled frame cannot swing
/// the factor past what any smoothing would bring back.
const LOAD_CAP: u64 = 16 * LOAD_ONE;

/// How long a frame's cost is smoothed over before it is acted on.
const RECENT_TAU_NS: u64 = 3 * NS_PER_SEC / 2;

/// How long the machine's busy factor is smoothed over: the longer view of
/// what else it is doing.
const LOAD_TAU_NS: u64 = 6 * NS_PER_SEC;

/// How long a step's best frame is believed: a best seen that long ago
/// counts for half of what the evidence now says.
const EVIDENCE_TAU_NS: u64 = 30 * NS_PER_SEC;

/// How long after a move or a resize frames are not counted, together with
/// [`SETTLE_FRAMES`]: both must pass, so a slow machine still skips the
/// frames that paid for the change.
const SETTLE_NS: u64 = NS_PER_SEC / 2;

/// Frames after a move or a resize that are not counted.
const SETTLE_FRAMES: u8 = 4;

/// The longest gap between two frames that is read as time passing: a window
/// that drew nothing for a while was not drawing slowly.
const MAX_GAP_NS: u64 = NS_PER_SEC / 4;

/// How long frames must overrun before a notch goes where the step's best
/// frame would not fit either: the detail is simply too dear here.
const SHED_UNABLE_NS: u64 = NS_PER_SEC;

/// How long frames must overrun before a notch goes where the step's best
/// frame fits: the machine has shown it can draw this, so what slows it is
/// something else it is doing.
const SHED_BUSY_NS: u64 = 6 * NS_PER_SEC;

/// How long a finer step must have looked affordable before it is tried.
const RESTORE_AFTER_NS: u64 = 4 * NS_PER_SEC;

/// How long a notch given back is on trial.
const TRIAL_NS: u64 = 3 * NS_PER_SEC;

/// The share of the drawing budget, in percent, a step's best frame must
/// fit inside for the step to count as one the machine can draw.
const CAPABLE_PERCENT: u64 = 90;

/// The share of the drawing budget, in percent, a finer step's predicted
/// cost must fit inside before it is tried.
const RESTORE_PERCENT: u64 = 80;

/// How much dearer per pixel, in percent, a finer step is assumed to be than
/// the one beneath it before it has been measured: a prior its own frames
/// replace.
const FINER_PERCENT: u64 = 125;

/// What a step has been seen to cost at its best.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Evidence {
    /// The cheapest per-pixel cost, in [`RATE_ONE`] units.
    rate: u64,
    /// When it was last brought up to date.
    at_ns: u64,
}

/// Turns the degradation ladder from what frames cost over the last several
/// seconds, no deeper than the floor the view allows.
///
/// Every dwell is counted in time frames were being drawn, not in time that
/// passed: a window that drew nothing for a while was neither overrunning nor
/// comfortable in it. How far back a step's best frame was seen is the one
/// thing read in time that passed, because the machine changes whether or not
/// anything is watching it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Governor {
    ladder: Ladder,
    floor: Ladder,
    floored: bool,
    /// The window the last frame was drawn in.
    extent: Option<(u32, u32)>,
    /// When the last frame was drawn, counted or not.
    last_ns: Option<u64>,
    /// Frame time still to pass before a frame is counted.
    settle_ns: u64,
    /// Frames still to pass before a frame is counted.
    settle_frames: u8,
    /// The smoothed per-pixel cost at the current step and size.
    recent: Option<u64>,
    /// How much slower than its best the machine is drawing, smoothed.
    load: u64,
    /// Per step: its best frame.
    best: [Option<Evidence>; Ladder::STEPS],
    /// Frame time smoothed frames have overrun for, while the frames drawn
    /// in it overran too.
    over_ns: u64,
    /// Frame time the finer step has looked affordable for.
    affordable_ns: u64,
    /// The step given back on trial, and the frame time its trial has left.
    trial: Option<(Ladder, u64)>,
}

impl Default for Governor {
    fn default() -> Self {
        Self::new()
    }
}

impl Governor {
    /// A governor at full quality, free to shed the whole ladder until it is
    /// told a floor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ladder: Ladder::FULL,
            floor: Ladder::new(Ladder::MAX_STEP),
            floored: false,
            extent: None,
            last_ns: None,
            settle_ns: SETTLE_NS,
            settle_frames: SETTLE_FRAMES,
            recent: None,
            load: LOAD_ONE,
            best: [None; Ladder::STEPS],
            over_ns: 0,
            affordable_ns: 0,
            trial: None,
        }
    }

    /// Where the ladder currently stands.
    #[must_use]
    pub const fn ladder(&self) -> Ladder {
        self.ladder
    }

    /// Whether frames went on overrunning with the ladder at its floor, and
    /// nothing has moved since: the frame rate is what is giving way,
    /// because the next notch would shed a detail the player needs to read.
    #[must_use]
    pub const fn floored(&self) -> bool {
        self.floored
    }

    /// Go no deeper than `floor` — the deepest the window's size and the
    /// zoom let the ladder go without shedding a detail the player needs —
    /// returning whether the ladder moved.
    ///
    /// Called before a frame is drawn, so no frame is drawn past its own
    /// floor. A floor that has come up past the ladder takes it back at once,
    /// since what it is shedding is now something the player needs to read.
    pub fn hold(&mut self, floor: Ladder) -> bool {
        self.floor = floor;
        if self.ladder > floor {
            self.trial = None;
            self.move_to(floor);
            return true;
        }
        if self.ladder < floor {
            self.floored = false;
        }
        false
    }

    /// Start again from full detail, keeping what every step has been seen to
    /// cost: for a player who has just chosen `auto`.
    pub fn restart(&mut self) {
        self.trial = None;
        self.move_to(Ladder::FULL);
    }

    /// Account for a frame drawn in a `window` at `now_ns`, returning whether
    /// the ladder moved.
    ///
    /// The ladder moves at most one notch a frame here, shedding only on a
    /// frame that itself overran and giving back only on one that fitted;
    /// only [`Self::hold`] moves it further, and only toward full.
    pub fn observe(&mut self, times: &FrameTimes, window: (u32, u32), now_ns: u64) -> bool {
        let Some(pixels) = render_pixels(window, self.ladder) else {
            return false;
        };
        let gap = self
            .last_ns
            .map_or(0, |last| now_ns.saturating_sub(last).min(MAX_GAP_NS));
        self.last_ns = Some(now_ns);
        if self.extent != Some(window) {
            // What frames cost here is what the smoothed cost describes, so a
            // new size starts it afresh; what steps cost at their best is per
            // pixel, and carries over.
            self.extent = Some(window);
            self.recent = None;
            self.settle();
        }
        if self.settling(gap) {
            return false;
        }

        let spent = times.total();
        let rate = per_pixel(spent, pixels);
        let best = self.learn(rate, now_ns);
        let ratio = scaled(rate, LOAD_ONE, best.max(1)).min(LOAD_CAP);
        self.load = smoothed(self.load, ratio, gap, LOAD_TAU_NS);
        let recent = self
            .recent
            .map_or(rate, |held| smoothed(held, rate, gap, RECENT_TAU_NS));
        self.recent = Some(recent);
        if let Some((step, left)) = &mut self.trial {
            if *step == self.ladder {
                *left = left.saturating_sub(gap);
            }
        }

        let budget = drawing_ns();
        if cost(recent, pixels) > budget {
            self.affordable_ns = 0;
            if spent <= budget {
                return false;
            }
            self.over_ns = self.over_ns.saturating_add(gap);
            return self.consider_shedding(best, pixels);
        }
        self.over_ns = 0;
        if self
            .trial
            .is_some_and(|(step, left)| step == self.ladder && left == 0)
        {
            self.trial = None;
        }
        if spent > budget {
            return false;
        }
        self.consider_restoring(window, best, gap, now_ns)
    }

    /// Frames at the current step have overrun: shed a notch once they have
    /// done so for long enough.
    fn consider_shedding(&mut self, best: u64, pixels: u64) -> bool {
        let on_trial = self
            .trial
            .is_some_and(|(step, left)| step == self.ladder && left > 0);
        let capable =
            cost(best, pixels).saturating_mul(100) <= drawing_ns().saturating_mul(CAPABLE_PERCENT);
        let patience = if capable && !on_trial {
            SHED_BUSY_NS
        } else {
            SHED_UNABLE_NS
        };
        if self.over_ns < patience {
            return false;
        }
        let Some(next) = self.ladder.shed().filter(|next| *next <= self.floor) else {
            self.floored = true;
            return false;
        };
        self.trial = None;
        self.move_to(next);
        true
    }

    /// Frames fit: give a notch back once the finer step's predicted cost has
    /// fitted well inside the budget for long enough.
    fn consider_restoring(&mut self, window: (u32, u32), best: u64, gap: u64, now_ns: u64) -> bool {
        let Some(finer) = self.ladder.restore() else {
            self.affordable_ns = 0;
            return false;
        };
        let Some(pixels) = render_pixels(window, finer) else {
            return false;
        };
        let prior = scaled(best, FINER_PERCENT, 100);
        let rate = self.believed(finer, prior, now_ns);
        let predicted = cost(scaled(rate, self.load, LOAD_ONE), pixels);
        if predicted.saturating_mul(100) > drawing_ns().saturating_mul(RESTORE_PERCENT) {
            self.affordable_ns = 0;
            return false;
        }
        self.affordable_ns = self.affordable_ns.saturating_add(gap);
        if self.affordable_ns < RESTORE_AFTER_NS {
            return false;
        }
        self.move_to(finer);
        self.trial = Some((finer, TRIAL_NS));
        true
    }

    /// Fold a counted frame's per-pixel `rate` into the current step's best,
    /// answering the best.
    ///
    /// A step entered for the first time starts from the best of the step
    /// above it, or a step first drawn while the machine is busy would take
    /// the busy frames for its best. Where a frame's costs do not all shrink
    /// with its pixels that start is optimistic, which only makes the
    /// governor slower to move either way until the step's own frames
    /// correct it.
    fn learn(&mut self, rate: u64, now_ns: u64) -> u64 {
        let step = usize::from(self.ladder.step());
        let inherited = self
            .ladder
            .restore()
            .and_then(|finer| self.best[usize::from(finer.step())]);
        // A best falls at once and rises toward what frames now cost only as
        // fast as evidence ages, so a machine that has genuinely become slower
        // is believed in time and a lucky frame is not believed for ever.
        let rate = match (self.best[step], inherited) {
            (Some(own), _) if rate < own.rate => rate,
            (Some(own), _) => smoothed(
                own.rate,
                rate,
                now_ns.saturating_sub(own.at_ns),
                EVIDENCE_TAU_NS,
            ),
            (None, Some(bound)) => rate.min(bound.rate),
            (None, None) => rate,
        };
        self.best[step] = Some(Evidence {
            rate,
            at_ns: now_ns,
        });
        rate
    }

    /// What `step` is believed to cost per pixel at its best: its own best
    /// frame, drawn toward `prior` the longer ago it was seen, or `prior`
    /// where it has not been seen at all.
    fn believed(&self, step: Ladder, prior: u64, now_ns: u64) -> u64 {
        match self.best[usize::from(step.step())] {
            Some(evidence) => {
                let age = now_ns.saturating_sub(evidence.at_ns);
                smoothed(evidence.rate, prior, age, EVIDENCE_TAU_NS)
            }
            None => prior,
        }
    }

    /// Stand at `ladder`, and count nothing until the change has been paid
    /// for.
    fn move_to(&mut self, ladder: Ladder) {
        self.ladder = ladder;
        self.recent = None;
        self.floored = false;
        self.settle();
    }

    /// Count no frame until [`SETTLE_NS`] of frames and [`SETTLE_FRAMES`]
    /// have been drawn.
    fn settle(&mut self) {
        self.settle_ns = SETTLE_NS;
        self.settle_frames = SETTLE_FRAMES;
        self.over_ns = 0;
        self.affordable_ns = 0;
    }

    /// Whether the frame drawn `gap` after the last is still paying for a
    /// change.
    fn settling(&mut self, gap: u64) -> bool {
        let settling = self.settle_frames > 0 || self.settle_ns > 0;
        self.settle_frames = self.settle_frames.saturating_sub(1);
        self.settle_ns = self.settle_ns.saturating_sub(gap);
        settling
    }
}

/// How many pixels a frame at `ladder` renders in `window`, or `None` for a
/// window no frame is drawn in.
fn render_pixels(window: (u32, u32), ladder: Ladder) -> Option<u64> {
    let view = Viewport::new(window.0, window.1, ladder.detail().resolution.scale()).ok()?;
    u64::try_from(view.render_pixels()).ok().filter(|&n| n > 0)
}

/// `ns` spread over `pixels`, in [`RATE_ONE`] units.
fn per_pixel(ns: u64, pixels: u64) -> u64 {
    scaled(ns, RATE_ONE, pixels.max(1))
}

/// What `pixels` cost at `rate`, in nanoseconds.
fn cost(rate: u64, pixels: u64) -> u64 {
    scaled(rate, pixels, RATE_ONE)
}

/// `value × numerator / denominator`, exactly, saturating at `u64::MAX`.
fn scaled(value: u64, numerator: u64, denominator: u64) -> u64 {
    let wide = u128::from(value) * u128::from(numerator) / u128::from(denominator.max(1));
    u64::try_from(wide).unwrap_or(u64::MAX)
}

/// `held` moved toward `sample` by the share `elapsed` is of `elapsed + tau`:
/// an exponential smoothing whose weight follows time rather than frame
/// count, so the horizon is the same at any frame rate.
fn smoothed(held: u64, sample: u64, elapsed: u64, tau: u64) -> u64 {
    let span = u128::from(tau) + u128::from(elapsed);
    if span == 0 {
        return sample;
    }
    let mixed =
        (u128::from(held) * u128::from(tau) + u128::from(sample) * u128::from(elapsed)) / span;
    u64::try_from(mixed).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
