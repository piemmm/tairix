//! The flying saucer: a spinning wireframe disc that now and then darts into
//! view, zips from place to place about the screen with pauses to hover
//! between, and shoots off out of sight.
//!
//! It keeps its place relative to the camera, as something that hunts the
//! flight would. Each dart eases in and out, and the saucer tilts into its
//! own acceleration like anything hanging in the air, its rim lights chasing
//! round as it spins.

use core::f64::consts::TAU;

use tairix_inline::ArrayVec;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;
use tairix_util::space::{Frame, Pose, Vec3};

use super::wire::{Corners, HullId, Ink, Lines, Part, Stage};
use super::{between, pick, Rgb};

/// The saucer's edges and faces.
static INK: Ink = Ink {
    edge: Rgb::new(120.0, 255.0, 205.0),
    face: Rgb::new(4.0, 22.0, 20.0),
};

/// The sides of its lens and dome, and the height of the lens's crown, which
/// the dome stands on.
const SIDES: u8 = 16;
const DOME_SIDES: u8 = 12;
const CROWN: f64 = 0.12;

/// Its rim lights: every other corner of the rim, how often the chase steps
/// round, how many lights a chase lights one of, and their colours and reach.
const LIGHT_EVERY: u8 = 2;
const CHASE_HZ: f64 = 7.0;
const CHASE_EVERY: i64 = 4;
const LIGHTS: [Rgb; 2] = [Rgb::new(255.0, 196.0, 84.0), Rgb::new(255.0, 255.0, 236.0)];
const LIGHT_REACH: f64 = 0.16;

/// How fast it spins, in radians a second.
const SPIN: f64 = 2.4;

/// How far it tilts into its acceleration, in radians a cell a second a
/// second, and the most it tilts.
const TILT: f64 = 0.012;
const MOST_TILT: f64 = 0.42;

/// How high it bobs while it hovers, in cells, and how often.
const BOB: f64 = 0.05;
const BOB_HZ: f64 = 0.9;

/// The fewest and most darts it makes, how long one takes and a hover lasts,
/// in seconds, and how many places it weighs for each dart, taking the
/// farthest, so no dart is a shuffle on the spot.
const DARTS: (usize, usize) = (4, 7);
const DART_S: (f64, f64) = (0.35, 0.7);
const HOVER_S: (f64, f64) = (0.45, 1.3);
const CANDIDATES: usize = 3;

/// How far ahead it keeps, in cells, and how far towards the screen's sides
/// and top it roams, as shares of what is in sight there.
const DEPTH: (f64, f64) = (7.0, 15.0);
const ROAM: f64 = 0.7;
const RISE: (f64, f64) = (0.12, 0.6);

/// How long it takes to arrive and to leave, in seconds.
const ARRIVE_S: f64 = 0.9;
const LEAVE_S: f64 = 1.25;

/// The most legs a visit holds: the arrival, the darts and their hovers, and
/// the departure.
const MAX_LEGS: usize = 2 * DARTS.1 + 3;

/// The lens: a disc thickest at its middle, flat on top where the dome sits.
pub(super) fn lens() -> (Corners, Lines) {
    let mut corners = Corners::new();
    for (height, radius) in [(CROWN, 0.62), (0.0, 1.0), (-0.16, 0.5)] {
        ring(&mut corners, SIDES, height, radius, 0.0);
    }
    (corners, Lines::new())
}

/// The dome standing on the lens.
pub(super) fn dome() -> (Corners, Lines) {
    let mut corners = Corners::new();
    for (height, radius) in [(CROWN, 0.42), (0.3, 0.36), (0.42, 0.18)] {
        ring(&mut corners, DOME_SIDES, height, radius, 0.5);
    }
    (corners, Lines::new())
}

/// Push a level ring of `sides` corners `radius` out at `height`, the first
/// `offset` of a side round.
fn ring(corners: &mut Corners, sides: u8, height: f64, radius: f64, offset: f64) {
    for side in 0..sides {
        let turn = TAU * (f64::from(side) + offset) / f64::from(sides);
        let _ = corners.try_push(Vec3::new(
            radius * mathf::cos(turn),
            height,
            radius * mathf::sin(turn),
        ));
    }
}

/// How one leg of a visit moves.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Ease {
    /// Arriving at speed and slowing to a stop.
    Arrive,
    /// A dart: from rest to rest, as fast as it can.
    Dart,
    /// Holding still, but for a bob.
    Hover,
    /// Pulling away, faster and faster.
    Leave,
}

/// One leg of a visit, relative to the camera.
#[derive(Copy, Clone, Debug)]
struct Leg {
    from: Vec3,
    to: Vec3,
    begins: f64,
    ends: f64,
    ease: Ease,
}

impl Leg {
    /// How far along the leg the saucer is `t` of the way through its time,
    /// and how fast and hard that share grows.
    fn along(&self, t: f64) -> (f64, f64, f64) {
        match self.ease {
            Ease::Arrive => {
                let u = 1.0 - t;
                (1.0 - u * u * u, 3.0 * u * u, -6.0 * u)
            }
            Ease::Dart => (
                t * t * t * (t * (t * 6.0 - 15.0) + 10.0),
                30.0 * t * t * (t - 1.0) * (t - 1.0),
                60.0 * t * (t - 1.0) * (2.0 * t - 1.0),
            ),
            Ease::Hover => (0.0, 0.0, 0.0),
            Ease::Leave => (t * t * t, 3.0 * t * t, 6.0 * t),
        }
    }
}

/// One visit of the saucer.
#[derive(Clone, Debug)]
pub(super) struct Saucer {
    legs: ArrayVec<Leg, MAX_LEGS>,
    spin: f64,
}

impl Saucer {
    /// A visit beginning at `start`, its course scattered from `rng` within
    /// the sight of a screen `aspect` times as wide as it is tall, whose
    /// horizon stands `sky` of its height below the top.
    pub(super) fn new(rng: &mut NonCryptoRng, start: f64, (aspect, sky): (f64, f64)) -> Self {
        let wide = aspect / 2.0;
        let place = |rng: &mut NonCryptoRng| {
            let depth = between(rng, DEPTH);
            Vec3::new(
                (rng.next_f64() * 2.0 - 1.0) * ROAM * wide * depth,
                between(rng, RISE) * sky * depth + 0.4,
                depth,
            )
        };
        let spot = |rng: &mut NonCryptoRng, from: Vec3| {
            (0..CANDIDATES)
                .map(|_| place(rng))
                .max_by(|a, b| (*a - from).length().total_cmp(&(*b - from).length()))
                .unwrap_or(from)
        };
        let mut legs = ArrayVec::new();
        let mut clock = start;
        let mut add =
            |legs: &mut ArrayVec<Leg, MAX_LEGS>, from: Vec3, to: Vec3, span: f64, ease: Ease| {
                let _ = legs.try_push(Leg {
                    from,
                    to,
                    begins: clock,
                    ends: clock + span,
                    ease,
                });
                clock += span;
            };
        let first = place(rng);
        let side = if rng.next_below(2) == 0 { -1.0 } else { 1.0 };
        let entry = Vec3::new(side * (wide * first.z + 4.0), first.y + 1.5, first.z * 0.8);
        add(&mut legs, entry, first, ARRIVE_S, Ease::Arrive);
        let mut at = first;
        let darts = DARTS.0 + pick(rng, DARTS.1 - DARTS.0 + 1);
        for _ in 0..darts {
            add(&mut legs, at, at, between(rng, HOVER_S), Ease::Hover);
            let next = spot(rng, at);
            add(&mut legs, at, next, between(rng, DART_S), Ease::Dart);
            at = next;
        }
        add(&mut legs, at, at, between(rng, HOVER_S), Ease::Hover);
        let away = Vec3::new((rng.next_f64() * 2.0 - 1.0) * 8.0, 14.0, 40.0);
        add(&mut legs, at, at + away, LEAVE_S, Ease::Leave);
        Self {
            legs,
            spin: rng.next_f64() * TAU,
        }
    }

    /// When the visit ends.
    fn ends(&self) -> f64 {
        self.legs.last().map_or(0.0, |leg| leg.ends)
    }

    /// Whether the visit is over by `time`.
    pub(super) fn is_over(&self, time: f64) -> bool {
        time >= self.ends()
    }

    /// Where the saucer stands at `time` relative to the camera, and how hard
    /// it is accelerating.
    fn course(&self, time: f64) -> (Vec3, Vec3) {
        let Some(leg) = self
            .legs
            .iter()
            .find(|leg| time < leg.ends)
            .or(self.legs.last())
        else {
            return (Vec3::ZERO, Vec3::ZERO);
        };
        let span = (leg.ends - leg.begins).max(1e-6);
        let t = ((time - leg.begins) / span).clamp(0.0, 1.0);
        let (share, _, pull) = leg.along(t);
        let travel = leg.to - leg.from;
        let mut at = leg.from + travel * share;
        if leg.ease == Ease::Hover {
            at.y += BOB * mathf::sin(TAU * BOB_HZ * time);
        }
        (at, travel * (pull / (span * span)))
    }

    /// Set the saucer out as it stands at `time`.
    pub(super) fn stage(&self, time: f64, stage: &mut Stage) {
        let (seen, pull) = self.course(time);
        let level = Vec3::new(pull.x, 0.0, pull.z);
        let tilt = (level.length() * TILT).min(MOST_TILT);
        let axis = Vec3::UP.cross(level).normalized();
        let yaw = self.spin + SPIN * time;
        let frame = if tilt > 1e-6 {
            Frame::turned(yaw, 0.0).rotated_by(Frame::about(axis, tilt))
        } else {
            Frame::turned(yaw, 0.0)
        };
        let camera = stage.camera().position();
        let pose = Pose::new(camera + seen, frame);
        let above = pose.point_to_local(camera).y > CROWN;
        let lens = Part::Solid {
            hull: HullId::SaucerLens,
            pose,
            ink: &INK,
            alpha: 1.0,
        };
        let dome = Part::Solid {
            hull: HullId::SaucerDome,
            pose,
            ink: &INK,
            alpha: 1.0,
        };
        stage.begin(pose.at);
        for part in if above { [lens, dome] } else { [dome, lens] } {
            stage.push(part);
        }
        // The rim is the lens's second ring of corners.
        let chase = i64::from(mathf::round_i32(mathf::floor(time * CHASE_HZ)));
        for light in (0..SIDES).step_by(usize::from(LIGHT_EVERY)) {
            let index = light / LIGHT_EVERY;
            if (chase + i64::from(index)).rem_euclid(CHASE_EVERY) != 0 {
                continue;
            }
            stage.push(Part::Beacon {
                hull: HullId::SaucerLens,
                corner: SIDES + light,
                pose,
                radius: LIGHT_REACH,
                light: LIGHTS[usize::from(index) % LIGHTS.len()],
                alpha: 1.0,
            });
        }
    }
}

#[cfg(test)]
#[path = "saucer_tests.rs"]
mod tests;
