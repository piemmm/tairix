//! The motion model: where every card is drawn this frame, and when the next
//! frame is due.
//!
//! The table says where each card rests; this says where it is *drawn*.
//! Whenever a card's resting place or face changes, [`Motion::retarget`]
//! starts a flight from wherever the card is drawn at that moment — its rest,
//! the middle of another flight, or under the pointer — to its new rest, so
//! every change to the table arrives on screen as movement, undo and an
//! automatic move home included, with no code path of its own.
//!
//! # It never runs when nothing is moving
//!
//! Every flight, glow and cascade is finite and begins at a change.
//! [`Motion::deadline_ns`] answers `None` the moment the last ends, and the
//! loop then parks with no deadline at all. Reduced motion starts nothing: a
//! card simply rests where the table puts it.

use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_geometry::{to_i32, Point, Rect, Region};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_theme::motion::ease_out;
use tairix_util::mathf;

use crate::card::CardId;
use crate::layout::Placement;

/// How one card's flight is paced.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Timing {
    /// How long after the change the card sets off.
    pub delay_ns: u64,
    /// How long it is in the air.
    pub span_ns: u64,
    /// How high it rises over its path, per thousand of the distance: a
    /// thrown card arcs, a slid one does not.
    pub arc: u32,
}

impl Timing {
    /// A card slid to its new place.
    pub const SLIDE: Self = Self {
        delay_ns: 0,
        span_ns: 170_000_000,
        arc: 0,
    };
}

/// Which side of a card is drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Facing {
    /// Its face.
    Up,
    /// Its back.
    Down,
    /// Part-way over: drawn `width` per thousand of its width, showing its
    /// face when `face` is set.
    Turning {
        /// Whether the face is the side showing.
        face: bool,
        /// How wide it is drawn, per thousand.
        width: u32,
    },
}

/// Where and how a card is drawn this frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Shown {
    /// Its top-left corner.
    pub at: Point,
    /// Which side shows.
    pub facing: Facing,
    /// Whether it is in the air, and so drawn above every card at rest with
    /// a lifted shadow.
    pub flying: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Flight {
    from: Point,
    to: Point,
    /// When it sets off.
    start_ns: u64,
    span_ns: u64,
    /// The arc's peak, in pixels.
    rise: i32,
    /// The side it shows at the start, when it turns over on the way.
    turns_from: Option<bool>,
    /// The side it lands on.
    up: bool,
    /// Where it was last drawn, for the damage its next frame owes.
    drawn: Option<Point>,
}

impl Flight {
    fn progress(&self, now_ns: u64) -> u8 {
        if self.finished(now_ns) {
            return u8::MAX;
        }
        let elapsed = now_ns.saturating_sub(self.start_ns);
        // An unfinished flight of no span has not set off yet.
        let share = elapsed
            .saturating_mul(255)
            .checked_div(self.span_ns)
            .unwrap_or(0);
        u8::try_from(share).unwrap_or(u8::MAX)
    }

    fn at(&self, now_ns: u64) -> Point {
        let linear = self.progress(now_ns);
        let eased = i64::from(ease_out(linear));
        let lerp = |a: i32, b: i32| {
            let value = i64::from(a) + (i64::from(b) - i64::from(a)) * eased / 255;
            i32::try_from(value).unwrap_or(b)
        };
        let t = i64::from(linear);
        let lift = i64::from(self.rise) * 4 * t * (255 - t) / (255 * 255);
        Point::new(
            lerp(self.from.x, self.to.x),
            lerp(self.from.y, self.to.y) - i32::try_from(lift).unwrap_or(0),
        )
    }

    fn facing(&self, now_ns: u64) -> Facing {
        let Some(from) = self.turns_from else {
            return if self.up { Facing::Up } else { Facing::Down };
        };
        let progress = u32::from(self.progress(now_ns));
        // The card narrows to an edge and widens again with the other side
        // showing: the cosine of the turn, drawn as its width.
        let angle = core::f64::consts::PI * f64::from(progress) / 255.0;
        let width = mathf::fabs(mathf::cos(angle)) * 1000.0;
        Facing::Turning {
            face: if progress < 128 { from } else { self.up },
            width: u32::try_from(mathf::round_i32(width).clamp(0, 1000)).unwrap_or(1000),
        }
    }

    fn finished(&self, now_ns: u64) -> bool {
        now_ns >= self.start_ns.saturating_add(self.span_ns)
    }
}

/// A point carried from one place to another as a card slides: where the
/// bottom card of a hand the keyboard holds is drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Glide(Flight);

impl Glide {
    /// From `from` to `to`, setting off at `now_ns`; there at once when
    /// motion is `reduced`.
    #[must_use]
    pub const fn new(from: Point, to: Point, now_ns: u64, reduced: bool) -> Self {
        Self(Flight {
            from,
            to,
            start_ns: now_ns,
            span_ns: if reduced { 0 } else { Timing::SLIDE.span_ns },
            rise: 0,
            turns_from: None,
            up: true,
            drawn: None,
        })
    }

    /// Where the point is at `now_ns`.
    #[must_use]
    pub fn at(&self, now_ns: u64) -> Point {
        self.0.at(now_ns)
    }

    /// Where it is going.
    #[must_use]
    pub const fn to(&self) -> Point {
        self.0.to
    }

    /// Whether it has yet to arrive.
    #[must_use]
    pub fn moving(&self, now_ns: u64) -> bool {
        !self.0.finished(now_ns)
    }
}

/// A card thrown off its foundation when the game is won, bouncing along the
/// foot of the table until it leaves it.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Bouncer {
    card: CardId,
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
}

/// The victory: every card thrown from home in turn, each bouncing off the
/// table's foot and leaving a trail of itself, until the last has left.
#[derive(Debug)]
pub struct Cascade {
    /// The cards still at home, the next to be thrown last.
    waiting: Vec<(CardId, Point)>,
    flying: Vec<Bouncer>,
    next_launch_ns: u64,
    stepped_ns: u64,
    begun: bool,
    floor: f64,
    left: f64,
    right: f64,
    size: (u32, u32),
    rng: NonCryptoRng,
}

/// Nanoseconds per physics step: fine enough that a bounce is the same
/// however the frames fall.
const STEP_NS: u64 = 1_000_000_000 / 240;
/// The same step in seconds.
const STEP: f64 = 1.0 / 240.0;
/// The most steps one frame catches up: a frame that arrives very late jumps
/// the cascade on rather than computing every step it missed.
const MOST_STEPS: u64 = 240;
/// Time between two cards leaving home.
const LAUNCH_NS: u64 = 150_000_000;
/// Of the speed a bounce keeps.
const RESTITUTION: f64 = 0.72;

impl Cascade {
    /// The cascade over cards `home`, at their places, thrown last first,
    /// across a table spanning `left..right` with its foot at `floor`.
    #[must_use]
    pub fn new(
        home: Vec<(CardId, Point)>,
        (left, right, floor): (i32, i32, i32),
        size: (u32, u32),
        now_ns: u64,
        seed: u64,
    ) -> Self {
        Self {
            waiting: home,
            flying: Vec::new(),
            next_launch_ns: now_ns,
            stepped_ns: now_ns,
            begun: false,
            floor: f64::from(floor) - f64::from(size.1),
            left: f64::from(left),
            right: f64::from(right),
            size,
            rng: NonCryptoRng::seed_from_u64(seed),
        }
    }

    /// Whether every card has left the table.
    #[must_use]
    pub fn done(&self) -> bool {
        self.waiting.is_empty() && self.flying.is_empty()
    }

    /// Whether the first card has been thrown: from then on a frame draws
    /// only the cards bouncing, over the trail already on screen.
    #[must_use]
    pub const fn begun(&self) -> bool {
        self.begun
    }

    /// Put off the first throw to a frame of `frame_ns` after `now_ns` at the
    /// earliest: the trail is drawn over the table as it last stood, so every
    /// card must have landed first.
    fn hold(&mut self, now_ns: u64, frame_ns: u64) {
        if !self.begun {
            self.next_launch_ns = self.next_launch_ns.max(now_ns.saturating_add(frame_ns));
            self.stepped_ns = now_ns;
        }
    }

    /// The cards in the air and where each is drawn now.
    pub fn bouncing(&self) -> impl Iterator<Item = (CardId, Point)> + '_ {
        self.flying.iter().map(|b| {
            (
                b.card,
                Point::new(mathf::round_i32(b.x), mathf::round_i32(b.y)),
            )
        })
    }

    /// The cards still waiting at home.
    pub fn waiting(&self) -> impl Iterator<Item = CardId> + '_ {
        self.waiting.iter().map(|&(card, _)| card)
    }

    /// Run the cascade on to `now_ns`, reporting where its cards are drawn
    /// now: the trail they leave is what is already on screen.
    pub fn advance(&mut self, now_ns: u64, damage: &mut Region) {
        while self.next_launch_ns <= now_ns {
            let Some((card, at)) = self.waiting.pop() else {
                break;
            };
            self.begun = true;
            let height = f64::from(self.size.1);
            let speed = 0.35 + unit(&mut self.rng) * 0.55;
            let direction = if self.rng.next_below(2) == 0 {
                -1.0
            } else {
                1.0
            };
            self.flying.push(Bouncer {
                card,
                x: f64::from(at.x),
                y: f64::from(at.y),
                vx: direction * speed * height * 2.2,
                vy: -unit(&mut self.rng) * height * 3.0,
            });
            self.next_launch_ns = self.next_launch_ns.saturating_add(LAUNCH_NS);
        }
        let height = f64::from(self.size.1);
        let gravity = height * 16.0;
        let steps = now_ns.saturating_sub(self.stepped_ns) / STEP_NS;
        for _ in 0..steps.min(MOST_STEPS) {
            for bouncer in &mut self.flying {
                bouncer.vy += gravity * STEP;
                bouncer.x += bouncer.vx * STEP;
                bouncer.y += bouncer.vy * STEP;
                if bouncer.y > self.floor {
                    bouncer.y = self.floor;
                    bouncer.vy = -bouncer.vy * RESTITUTION;
                }
            }
        }
        self.stepped_ns = self.stepped_ns.saturating_add(steps * STEP_NS);
        let (left, right, width) = (self.left, self.right, f64::from(self.size.0));
        self.flying.retain(|b| b.x + width > left && b.x < right);
        for (_, at) in self.bouncing() {
            damage.add(Rect::new(at.x, at.y, self.size.0, self.size.1));
        }
    }
}

/// A draw in `0..1` from `rng`.
fn unit(rng: &mut NonCryptoRng) -> f64 {
    tairix_rng::unit_from(rng.next_u64())
}

/// A hint shown: the cards it would move and the place they would go, lit
/// for a moment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Glow {
    /// The rectangles lit.
    pub rects: Vec<Rect>,
    start_ns: u64,
}

/// How long a hint stays lit.
pub(crate) const GLOW_NS: u64 = 1_800_000_000;
/// How long a refused card's hop takes.
const HOP_NS: u64 = 210_000_000;

impl Glow {
    /// How strongly it is lit at `now_ns`, out of 255: two soft pulses that
    /// end dark.
    #[must_use]
    pub fn strength(&self, now_ns: u64, reduced: bool) -> u8 {
        let elapsed = now_ns.saturating_sub(self.start_ns);
        if elapsed >= GLOW_NS {
            return 0;
        }
        if reduced {
            return 200;
        }
        // Below the glow's span, which fits a `u32` of nanoseconds.
        let share = f64::from(u32::try_from(elapsed).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(GLOW_NS).unwrap_or(u32::MAX));
        let phase = share * 2.0 * core::f64::consts::PI * 2.0;
        let fade = 1.0 - share;
        let pulse = (1.0 - mathf::cos(phase)) / 2.0;
        u8::try_from(mathf::round_i32(pulse * fade * 230.0).clamp(0, 255)).unwrap_or(0)
    }
}

/// Every card's motion over one table.
#[derive(Debug)]
pub struct Motion {
    reduced: bool,
    /// The rate the desktop composes at, which every frame is paced by.
    refresh: RefreshRate,
    rest: Vec<Placement>,
    flights: Vec<Option<Flight>>,
    /// The cards in the air, in the order they took off: later ones are
    /// drawn over earlier ones.
    order: Vec<CardId>,
    cascade: Option<Cascade>,
    glow: Option<Glow>,
    size: (u32, u32),
}

impl Motion {
    /// Motion for a deck of `cards`, drawn `size`, animated unless `reduced`
    /// on a desktop refreshing at `refresh`.
    #[must_use]
    pub fn new(cards: usize, size: (u32, u32), reduced: bool, refresh: RefreshRate) -> Self {
        Self {
            reduced,
            refresh,
            rest: alloc::vec![Placement::default(); cards],
            flights: alloc::vec![None; cards],
            order: Vec::new(),
            cascade: None,
            glow: None,
            size,
        }
    }

    /// Whether animation is suppressed.
    #[must_use]
    pub const fn reduced_motion(&self) -> bool {
        self.reduced
    }

    /// The rate the desktop composes at.
    #[must_use]
    pub const fn refresh(&self) -> RefreshRate {
        self.refresh
    }

    /// One frame of the desktop, in nanoseconds.
    #[must_use]
    pub const fn frame_ns(&self) -> u64 {
        self.refresh.frame_ns()
    }

    /// Pace every frame by a desktop now refreshing at `refresh`.
    pub fn set_refresh(&mut self, refresh: RefreshRate) {
        self.refresh = refresh;
    }

    /// Adopt the theme's reduced-motion policy, settling everything in
    /// flight when it is turned on.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced = reduced;
        if reduced {
            self.settle();
        }
    }

    /// Put every card at rest where `placements` say, drawn `size`, with
    /// nothing in the air: a new layout moves every card at once, and an
    /// animation of that is a window being resized, not a move.
    pub fn snap(&mut self, placements: &[Placement], size: (u32, u32)) {
        self.size = size;
        self.rest.clear();
        self.rest.extend_from_slice(placements);
        self.flights.clear();
        self.flights.resize(placements.len(), None);
        self.order.clear();
        self.glow = None;
    }

    /// Land everything in the air now.
    pub fn settle(&mut self) {
        self.flights.fill(None);
        self.order.clear();
        self.glow = None;
    }

    /// Move every card whose place or face `placements` changed, each from
    /// wherever it is drawn now, paced by `timing`. `held` names the cards
    /// under the pointer and where they are drawn.
    pub fn retarget(
        &mut self,
        now_ns: u64,
        placements: &[Placement],
        held: &[(CardId, Point)],
        timing: impl Fn(CardId) -> Timing,
        damage: &mut Region,
    ) {
        for (index, (&new, old)) in placements.iter().zip(self.rest.iter_mut()).enumerate() {
            let Some(card) = CardId::new(index) else {
                continue;
            };
            let holding = held.iter().find(|&&(c, _)| c == card).map(|&(_, at)| at);
            if new == *old && holding.is_none() {
                continue;
            }
            let flight = self.flights.get(index).copied().flatten();
            let (from, facing) = match (holding, flight) {
                (Some(at), _) => (at, old.up),
                (None, Some(flight)) => (flight.at(now_ns), flight.up),
                (None, None) => (old.at, old.up),
            };
            let size = self.size;
            damage.add(card_rect(from, size));
            damage.add(card_rect(new.at, size));
            *old = new;
            if flight.is_some() {
                self.order.retain(|&c| c != card);
            }
            if self.reduced {
                if let Some(slot) = self.flights.get_mut(index) {
                    *slot = None;
                }
                continue;
            }
            let pace = timing(card);
            let distance = mathf::hypot(f64::from(new.at.x - from.x), f64::from(new.at.y - from.y));
            let flight = Flight {
                from,
                to: new.at,
                start_ns: now_ns.saturating_add(pace.delay_ns),
                span_ns: pace.span_ns,
                rise: mathf::round_i32(distance * f64::from(pace.arc) / 1000.0),
                turns_from: (facing != new.up).then_some(facing),
                up: new.up,
                drawn: Some(from),
            };
            if let Some(slot) = self.flights.get_mut(index) {
                *slot = Some(flight);
            }
            self.order.push(card);
        }
    }

    /// Hop `cards` up and back down where they are, `rise` pixels high: the
    /// answer to a move the rules refuse. Reduced motion leaves them still.
    pub fn hop(&mut self, cards: &[CardId], rise: u32, now_ns: u64, damage: &mut Region) {
        if self.reduced {
            return;
        }
        for &card in cards {
            let index = card.index();
            let Some(rest) = self.rest.get(index).copied() else {
                continue;
            };
            let from = self.shown(card, now_ns).at;
            let size = self.size;
            damage.add(card_rect(from, size));
            let flight = Flight {
                from,
                to: rest.at,
                start_ns: now_ns,
                span_ns: HOP_NS,
                rise: to_i32(rise),
                turns_from: None,
                up: rest.up,
                drawn: Some(from),
            };
            if let Some(slot) = self.flights.get_mut(index) {
                if slot.replace(flight).is_some() {
                    self.order.retain(|&c| c != card);
                }
            }
            self.order.push(card);
        }
    }

    /// Bring `cards` out of the air where they are: the pointer or the
    /// keyboard has taken hold of them, and draws them from now on.
    pub fn ground(&mut self, cards: &[CardId]) {
        let mut grounded = false;
        for &card in cards {
            if let Some(slot) = self.flights.get_mut(card.index()) {
                grounded |= slot.take().is_some();
            }
        }
        if grounded {
            let flights = &self.flights;
            self.order
                .retain(|&card| flights.get(card.index()).is_some_and(Option::is_some));
        }
    }

    /// Where and how `card` is drawn at `now_ns`.
    #[must_use]
    pub fn shown(&self, card: CardId, now_ns: u64) -> Shown {
        let index = card.index();
        if let Some(Some(flight)) = self.flights.get(index) {
            return Shown {
                at: flight.at(now_ns),
                facing: flight.facing(now_ns),
                flying: true,
            };
        }
        let rest = self.rest.get(index).copied().unwrap_or_default();
        Shown {
            at: rest.at,
            facing: if rest.up { Facing::Up } else { Facing::Down },
            flying: false,
        }
    }

    /// Whether `card` is in the air.
    #[must_use]
    pub fn flying(&self, card: CardId) -> bool {
        self.flights.get(card.index()).is_some_and(Option::is_some)
    }

    /// The cards in the air, earliest off the ground first.
    #[must_use]
    pub fn airborne(&self) -> &[CardId] {
        &self.order
    }

    /// Light `rects` for a moment, as a hint does.
    pub fn glow(&mut self, rects: Vec<Rect>, now_ns: u64, damage: &mut Region) {
        if let Some(old) = self.glow.take() {
            for rect in old.rects {
                damage.add(rect);
            }
        }
        for &rect in &rects {
            damage.add(rect);
        }
        self.glow = Some(Glow {
            rects,
            start_ns: now_ns,
        });
    }

    /// The hint lit now, and how strongly.
    #[must_use]
    pub fn lit(&self, now_ns: u64) -> Option<(&Glow, u8)> {
        let glow = self.glow.as_ref()?;
        let strength = glow.strength(now_ns, self.reduced);
        (strength > 0).then_some((glow, strength))
    }

    /// Begin the victory over `home`, unless motion is reduced, in which
    /// case the won table simply stands.
    pub fn celebrate(&mut self, cascade: Cascade) {
        if !self.reduced {
            self.cascade = Some(cascade);
        }
    }

    /// The victory in progress.
    #[must_use]
    pub const fn cascade(&self) -> Option<&Cascade> {
        self.cascade.as_ref()
    }

    /// End the victory now, answering whether one was running.
    pub fn end_cascade(&mut self) -> bool {
        self.cascade.take().is_some()
    }

    /// When the next frame is due, or `None` when nothing moves.
    #[must_use]
    pub fn deadline_ns(&self, now_ns: u64) -> Option<u64> {
        if !self.order.is_empty() || self.cascade.is_some() {
            return Some(now_ns.saturating_add(self.frame_ns()));
        }
        let glow = self.glow.as_ref()?;
        let ends = glow.start_ns.saturating_add(GLOW_NS);
        // A reduced-motion hint holds still, so the one wake it owes is its
        // end.
        Some(if self.reduced {
            ends
        } else {
            now_ns.saturating_add(self.frame_ns()).min(ends)
        })
    }

    /// Run everything on to `now_ns`, adding what the frame must repaint to
    /// `damage`: each card in the air where it was and where it is, each
    /// landed card where it lands, and a hint while it is lit.
    pub fn advance(&mut self, now_ns: u64, damage: &mut Region) {
        let size = self.size;
        // The frame a last card lands on is drawn whole, so the victory
        // starts on the frame after.
        let calm = self.order.is_empty();
        let flights = &mut self.flights;
        self.order.retain(|&card| {
            let Some(slot) = flights.get_mut(card.index()) else {
                return false;
            };
            let Some(flight) = slot else {
                return false;
            };
            let at = flight.at(now_ns);
            if let Some(drawn) = flight.drawn {
                damage.add(card_rect(drawn, size));
            }
            damage.add(card_rect(at, size));
            flight.drawn = Some(at);
            if flight.finished(now_ns) {
                damage.add(card_rect(flight.to, size));
                *slot = None;
                return false;
            }
            true
        });
        if let Some(glow) = &self.glow {
            for &rect in &glow.rects {
                damage.add(rect);
            }
            if now_ns >= glow.start_ns.saturating_add(GLOW_NS) {
                self.glow = None;
            }
        }
        let frame_ns = self.frame_ns();
        if let Some(cascade) = &mut self.cascade {
            if calm {
                cascade.advance(now_ns, damage);
            } else {
                cascade.hold(now_ns, frame_ns);
            }
        }
    }
}

/// A card's rectangle drawn at `at`, grown by the shadow a card in the air
/// casts, which reaches past the card.
#[must_use]
pub fn card_rect(at: Point, (width, height): (u32, u32)) -> Rect {
    let reach = height / 12 + 2;
    Rect::new(
        at.x - to_i32(reach),
        at.y - to_i32(reach),
        width + reach * 2,
        height + reach * 3,
    )
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
