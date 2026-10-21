//! How the launcher moves: its window rising into place and sinking away, its
//! content arriving, and the keyboard's mark moving between tiles.
//!
//! Every movement is a state change over one of the theme's durations, so a
//! reduced-motion theme — every duration zero — lands each at its end state
//! and asks for no frame. Nothing here reads a clock: the embedder hands the
//! instant in, and [`Motion::advance`] fixes the instant a frame is drawn at,
//! so painting and placing agree.

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::NANOS_PER_MILLI;
use tairix_geometry::Scale;
use tairix_proglib::EntryId;
use tairix_theme::motion::ease_out;
use tairix_theme::{Fade, Glide, MotionInteraction, MotionTheme, Timeline};

use super::view::SectionKind;

/// How far the window rises as it opens, in logical pixels.
pub(crate) const RISE: u32 = 16;

/// How far the window sinks as it closes, in logical pixels.
pub(crate) const SINK: u32 = 8;

/// How far each program rises as the first page cascades in, in logical
/// pixels.
pub(crate) const CASCADE_RISE: u32 = 10;

/// How long after its predecessor each program of a cascade sets off.
pub(crate) const CASCADE_STAGGER_MS: u16 = 14;

/// How many programs a cascade staggers; those after arrive with the last.
pub(crate) const CASCADE_STEPS: u16 = 12;

/// How far a page arriving from the sidebar travels, in logical pixels.
pub(crate) const SLIDE: u32 = 24;

/// What one program — or a section's heading — is drawn at in a frame of an
/// entrance: how strongly, and how far from its place.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ItemFrame {
    /// How strongly it lands, `0` not at all.
    pub(crate) strength: u8,
    /// How far below its place it stands, negative for above.
    pub(crate) rise: i32,
}

impl ItemFrame {
    /// Drawn whole, in place.
    pub(crate) const ARRIVED: Self = Self {
        strength: u8::MAX,
        rise: 0,
    };
}

/// The content arriving: one movement every program makes, each setting off
/// a stagger after the one before it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Entrance {
    started_ns: u64,
    item_ns: u64,
    stagger_ns: u64,
    steps: u16,
    rise: i32,
    span: Timeline,
}

impl Entrance {
    fn new(now_ns: u64, item_ms: u16, (stagger_ms, steps): (u16, u16), rise: i32) -> Option<Self> {
        if item_ms == 0 {
            return None;
        }
        let total = u32::from(item_ms).saturating_add(u32::from(stagger_ms) * u32::from(steps));
        Some(Self {
            started_ns: now_ns,
            item_ns: u64::from(item_ms).saturating_mul(NANOS_PER_MILLI),
            stagger_ns: u64::from(stagger_ms).saturating_mul(NANOS_PER_MILLI),
            steps,
            rise,
            span: Timeline::start(now_ns, u16::try_from(total).unwrap_or(u16::MAX)),
        })
    }

    /// Where the program shown `order`th in reading order is at `now_ns`.
    pub(crate) fn frame(&self, order: usize, now_ns: u64) -> ItemFrame {
        let step = u64::try_from(order.min(usize::from(self.steps))).unwrap_or(0);
        let leaves = self
            .started_ns
            .saturating_add(step.saturating_mul(self.stagger_ns));
        let Some(elapsed) = now_ns.checked_sub(leaves) else {
            return ItemFrame {
                strength: 0,
                rise: self.rise,
            };
        };
        let progress = if elapsed >= self.item_ns {
            u8::MAX
        } else {
            let scaled = elapsed.saturating_mul(u64::from(u8::MAX)) / self.item_ns.max(1);
            u8::try_from(scaled).unwrap_or(u8::MAX)
        };
        let left = i64::from(u8::MAX - ease_out(progress));
        let rise = i64::from(self.rise) * left / i64::from(u8::MAX);
        ItemFrame {
            strength: progress,
            rise: i32::try_from(rise).unwrap_or(0),
        }
    }
}

/// The keyboard's mark moving between two programs, each from the strength
/// it was drawn at when the move began.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Marks {
    arriving: Option<(SectionKind, EntryId)>,
    arriving_from: u8,
    leaving: Option<(SectionKind, EntryId)>,
    leaving_from: u8,
    timeline: Timeline,
}

/// What a step of the motion changed. A window that finished leaving is no
/// longer [`departing`](Motion::departing).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Stepped {
    /// The window's place or strength.
    pub(crate) placement: bool,
    /// The content's frame: an entrance moved on or ended.
    pub(crate) content: bool,
    /// The selection mark's strength.
    pub(crate) marks: bool,
}

/// Everything about the launcher in motion. See the [module docs](self).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Motion {
    at_ns: u64,
    window: Glide,
    opacity: Fade,
    departing: bool,
    entrance: Option<Entrance>,
    marks: Option<Marks>,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            at_ns: 0,
            window: Glide::at(0),
            opacity: Fade::start(0, 0, u8::MAX, u8::MAX),
            departing: false,
            entrance: None,
            marks: None,
        }
    }
}

impl Motion {
    /// The window rises into place and fades in from wherever it stands — a
    /// departure interrupted part-way resumes from what is on screen — while
    /// the first page cascades in.
    pub(crate) fn open(&mut self, now_ns: u64, motion: MotionTheme, scale: Scale) {
        let span = motion.duration(MotionInteraction::PanelOpen);
        let (from, strength) = if self.departing {
            (self.window.value(now_ns), self.opacity.strength(now_ns))
        } else {
            (to_i32(scale.scale_length(RISE)), 0)
        };
        self.at_ns = now_ns;
        self.window = Glide::start(now_ns, span, from, 0);
        self.opacity = Fade::start(now_ns, span, strength, u8::MAX);
        self.departing = false;
        self.marks = None;
        self.entrance = Entrance::new(
            now_ns,
            span,
            (CASCADE_STAGGER_MS, CASCADE_STEPS),
            to_i32(scale.scale_length(CASCADE_RISE)),
        );
    }

    /// The window sinks and fades away. A reduced-motion theme has it gone at
    /// once.
    pub(crate) fn close(&mut self, now_ns: u64, motion: MotionTheme, scale: Scale) {
        let span = motion.duration(MotionInteraction::PanelClose);
        let (from, strength) = (self.window.value(now_ns), self.opacity.strength(now_ns));
        self.at_ns = now_ns;
        self.window = Glide::start(now_ns, span, from, to_i32(scale.scale_length(SINK)));
        self.opacity = Fade::start(now_ns, span, strength, 0);
        self.departing = span != 0;
        self.entrance = None;
        self.marks = None;
    }

    /// A page arrives from the sidebar, travelling from below when `down` the
    /// list and from above when up it.
    pub(crate) fn arrive(&mut self, now_ns: u64, motion: MotionTheme, scale: Scale, down: bool) {
        let travel = to_i32(scale.scale_length(SLIDE));
        self.entrance = Entrance::new(
            now_ns,
            motion.duration(MotionInteraction::StageTransition),
            (0, 0),
            if down { travel } else { -travel },
        );
        self.at_ns = now_ns;
    }

    /// The keyboard's mark moves from `leaving` to `arriving`. A mark still
    /// moving hands each its strength as drawn, so neither jumps.
    pub(crate) fn mark(
        &mut self,
        now_ns: u64,
        motion: MotionTheme,
        leaving: Option<(SectionKind, EntryId)>,
        arriving: Option<(SectionKind, EntryId)>,
    ) {
        let drawn = |mark: &Option<(SectionKind, EntryId)>, at_rest: u8| {
            mark.as_ref()
                .and_then(|(kind, id)| self.mark_strength(*kind, id))
                .unwrap_or(at_rest)
        };
        let (leaving_from, arriving_from) = (drawn(&leaving, u8::MAX), drawn(&arriving, 0));
        let timeline = Timeline::start(now_ns, motion.duration(MotionInteraction::SelectionChange));
        self.marks = timeline.running().then_some(Marks {
            arriving,
            arriving_from,
            leaving,
            leaving_from,
            timeline,
        });
        self.at_ns = now_ns;
    }

    /// End any entrance at once, answering whether one was running: the
    /// content is being interacted with, and must stand where it is.
    pub(crate) fn settle_content(&mut self) -> bool {
        self.entrance.take().is_some()
    }

    /// Land a departure at its end at once, answering whether one was
    /// leaving.
    pub(crate) fn finish_departure(&mut self) -> bool {
        if !self.departing {
            return false;
        }
        self.window.settle();
        self.opacity.settle();
        self.departing = false;
        true
    }

    /// Whether the closed window is still leaving the screen.
    pub(crate) const fn departing(&self) -> bool {
        self.departing
    }

    /// Whether the window is still moving or fading.
    pub(crate) const fn placing(&self) -> bool {
        self.window.running() || self.opacity.running()
    }

    /// Whether the keyboard's mark is moving.
    pub(crate) const fn marking(&self) -> bool {
        self.marks.is_some()
    }

    /// The content's entrance, while one runs.
    pub(crate) const fn entrance(&self) -> Option<&Entrance> {
        self.entrance.as_ref()
    }

    /// The instant the current frame is drawn at.
    pub(crate) const fn at_ns(&self) -> u64 {
        self.at_ns
    }

    /// How far toward the bar the window stands, and how strongly it is
    /// drawn, at the current frame.
    pub(crate) fn placement(&self) -> (i32, u8) {
        (
            self.window.value(self.at_ns),
            self.opacity.strength(self.at_ns),
        )
    }

    /// The strength the mark of `(kind, id)` is drawn at in the current frame,
    /// or `None` where it follows the selection.
    pub(crate) fn mark_strength(&self, kind: SectionKind, id: &EntryId) -> Option<u8> {
        let marks = self.marks.as_ref()?;
        let progress = marks.timeline.progress(self.at_ns);
        let is = |mark: &Option<(SectionKind, EntryId)>| {
            mark.as_ref()
                .is_some_and(|(at_kind, at_id)| *at_kind == kind && at_id == id)
        };
        if is(&marks.arriving) {
            Some(between(marks.arriving_from, u8::MAX, progress))
        } else if is(&marks.leaving) {
            Some(between(marks.leaving_from, 0, progress))
        } else {
            None
        }
    }

    /// Step to `now_ns`, answering what changed. A movement that has run out
    /// is drawn at its end in this frame and owes nothing after it.
    pub(crate) fn advance(&mut self, now_ns: u64) -> Stepped {
        self.at_ns = now_ns;
        let placement = self.placing();
        if self.window.finished(now_ns) {
            self.window.settle();
        }
        if self.opacity.finished(now_ns) {
            self.opacity.settle();
        }
        if self.departing && !self.placing() {
            self.departing = false;
        }
        let content = self.entrance.is_some();
        if self
            .entrance
            .is_some_and(|entrance| entrance.span.finished(now_ns))
        {
            self.entrance = None;
        }
        let marks = self.marks.is_some();
        if self
            .marks
            .as_ref()
            .is_some_and(|marks| marks.timeline.finished(now_ns))
        {
            self.marks = None;
        }
        Stepped {
            placement,
            content,
            marks,
        }
    }

    /// Nanoseconds until the next frame is worth drawing on a display
    /// refreshing at `refresh`, or `None` when nothing moves.
    pub(crate) fn next_frame_in(&self, now_ns: u64, refresh: RefreshRate) -> Option<u64> {
        [
            self.window.next_frame_in(now_ns, refresh),
            self.opacity.next_frame_in(now_ns, refresh),
            self.entrance
                .and_then(|entrance| entrance.span.next_frame_in(now_ns, refresh)),
            self.marks
                .as_ref()
                .and_then(|marks| marks.timeline.next_frame_in(now_ns, refresh)),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// The strength `progress` of the way from `from` to `to`.
fn between(from: u8, to: u8, progress: u8) -> u8 {
    let travelled = (i32::from(to) - i32::from(from)) * i32::from(progress) / i32::from(u8::MAX);
    u8::try_from(i32::from(from) + travelled).unwrap_or(to)
}
