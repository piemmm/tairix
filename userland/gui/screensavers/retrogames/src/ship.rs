//! The spaceship: a sleek wireframe interceptor that now and then sweeps into
//! view on one long, smooth curve and out again, banking through its turns,
//! its twin engines trailing clean blue-white plumes.
//!
//! Its course is a cubic Bézier curve set relative to the camera, so it is
//! framed the same at any pace of flight; it faces the way it truly moves,
//! the flight's own forward speed counted.

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;
use tairix_util::space::{Frame, Pose, Vec3};

use super::wire::{Corners, HullId, Ink, Lines, Part, Stage};
use super::{pick, Rgb};

/// The ship's edges and faces.
static INK: Ink = Ink {
    edge: Rgb::new(222.0, 236.0, 255.0),
    face: Rgb::new(9.0, 14.0, 30.0),
};

/// The stern's depth along the ship, which nothing of it reaches past, and
/// the fuselage's tail just short of it; the wing roots' distance either side
/// of its middle, which parts the fuselage from each wing; and each engine
/// pod's distance out, its radius, and its nozzle's.
const STERN: f64 = -1.3;
const TAIL: f64 = STERN + 0.1;
const ROOT: f64 = 0.4;
const POD: f64 = 1.05;
const POD_RADIUS: f64 = 0.16;
const NOZZLE_RADIUS: f64 = 0.11;
const NOZZLES: [Vec3; 2] = [Vec3::new(-POD, 0.0, STERN), Vec3::new(POD, 0.0, STERN)];

/// A plume's length, in cells, how far it wavers either way as a share of
/// that, and how often.
const PLUME: f64 = 2.6;
const FLICKER: f64 = 0.04;
const FLICKER_HZ: f64 = 1.3;

/// The plumes' light: the outer flame, its white-hot core, and the glow at
/// each nozzle; and each one's width, as a share of a nozzle's, and length,
/// as a share of a plume's.
const FLAME: Rgb = Rgb::new(64.0, 146.0, 255.0);
const FLAME_CORE: Rgb = Rgb::new(232.0, 248.0, 255.0);
const NOZZLE_GLOW: Rgb = Rgb::new(170.0, 220.0, 255.0);
const FLAME_WIDTH: f64 = 2.3;
const CORE_WIDTH: f64 = 0.95;
const CORE_LENGTH: f64 = 0.5;

/// How hard the ship banks into a turn: radians a cell a second a second of
/// sideways pull, and the most it banks.
const BANK: f64 = 0.09;
const MOST_BANK: f64 = 0.9;

/// How much of its course the ship takes to come out of the haze or go into
/// it, where its course begins or ends far off.
const FADE: f64 = 0.14;

/// The fuselage: a long pointed nose, a raised canopy, and a fin swept back
/// over the stern, all within the wing roots.
pub(super) fn fuselage() -> (Corners, Lines) {
    let mut corners = Corners::new();
    for corner in [
        Vec3::new(0.0, 0.0, 2.1),
        Vec3::new(0.0, 0.36, 0.55),
        Vec3::new(0.0, 0.4, -0.1),
        Vec3::new(0.0, 0.78, -1.15),
        Vec3::new(0.0, -0.26, TAIL),
    ] {
        let _ = corners.try_push(corner);
    }
    for side in [-1.0, 1.0] {
        for corner in [
            Vec3::new(0.2, 0.15, 1.1),
            Vec3::new(0.28, 0.0, 1.1),
            Vec3::new(0.2, -0.14, 1.1),
            Vec3::new(0.36, 0.22, TAIL),
            Vec3::new(ROOT, 0.0, TAIL),
            Vec3::new(0.34, -0.2, TAIL),
        ] {
            let _ = corners.try_push(Vec3::new(side * corner.x, corner.y, corner.z));
        }
    }
    (corners, Lines::new())
}

/// The left wing and its engine pod, as the right's mirror.
pub(super) fn port_wing() -> (Corners, Lines) {
    wing(-1.0)
}

/// The right wing: thin at its root, rising to the six-sided engine pod at
/// its tip, whose nozzle is drawn on its stern.
pub(super) fn starboard_wing() -> (Corners, Lines) {
    wing(1.0)
}

/// The wing on `side` of the fuselage, `-1.0` the left and `1.0` the right.
fn wing(side: f64) -> (Corners, Lines) {
    let mut corners = Corners::new();
    for (y, z) in [(0.04, 0.3), (-0.04, 0.3), (0.04, -1.1), (-0.04, -1.1)] {
        let _ = corners.try_push(Vec3::new(side * ROOT, y, z));
    }
    let around = |at: u8, radius: f64| {
        let turn = core::f64::consts::TAU * (f64::from(at) + 0.5) / 6.0;
        (radius * mathf::cos(turn), radius * mathf::sin(turn))
    };
    for z in [0.2, STERN] {
        for at in 0..6u8 {
            let (x, y) = around(at, POD_RADIUS);
            let _ = corners.try_push(Vec3::new(side * (POD + x), y, z));
        }
    }
    let _ = corners.try_push(Vec3::new(side * POD, 0.0, 0.45));
    let mut lines = Lines::new();
    for at in 0..6u8 {
        let ((x0, y0), (x1, y1)) = (around(at, NOZZLE_RADIUS), around(at + 1, NOZZLE_RADIUS));
        let _ = lines.try_push([
            Vec3::new(side * (POD + x0), y0, STERN),
            Vec3::new(side * (POD + x1), y1, STERN),
        ]);
    }
    (corners, lines)
}

/// How a course begins and ends: far off in the haze, or out of sight.
#[derive(Copy, Clone, Debug)]
struct Fades {
    arrives: bool,
    leaves: bool,
}

/// One pass of the ship.
#[derive(Clone, Debug)]
pub(super) struct Ship {
    start: f64,
    duration: f64,
    /// The course's control points, right of, above and ahead of the camera.
    course: [Vec3; 4],
    fades: Fades,
    flicker: f64,
}

/// The screen shape the courses are set for: sixteen wide to nine tall. A
/// wider screen spreads their sideways reach to match, so a course still
/// begins and ends out of sight.
const SET_FOR: f64 = 16.0 / 9.0;

/// How far a course's inner control points and its time may vary either way,
/// as a share of each.
const VARY: f64 = 0.1;

/// The courses a pass may take: its control points for a pass from the left,
/// how long it takes, and how it begins and ends.
const COURSES: [([[f64; 3]; 4], f64, Fades); 4] = [
    // From behind the camera, overhead, away into the distance.
    (
        [
            [-3.2, 2.4, -5.0],
            [-1.6, 2.1, 6.0],
            [1.2, 1.5, 22.0],
            [0.6, 1.1, 75.0],
        ],
        8.0,
        Fades {
            arrives: false,
            leaves: true,
        },
    ),
    // Out of the distance, nearer, and up over the camera.
    (
        [
            [2.5, 0.9, 85.0],
            [1.4, 1.0, 32.0],
            [0.4, 1.7, 9.0],
            [-0.2, 5.5, -3.0],
        ],
        7.5,
        Fades {
            arrives: true,
            leaves: false,
        },
    ),
    // Across the sky from side to side, rising and falling.
    (
        [
            [-22.0, 2.3, 16.0],
            [-6.0, 1.5, 11.0],
            [6.0, 2.9, 18.0],
            [22.0, 2.5, 15.0],
        ],
        6.5,
        Fades {
            arrives: false,
            leaves: false,
        },
    ),
    // In from the side, across in front, and away into the distance.
    (
        [
            [17.0, 1.9, 10.0],
            [4.0, 1.1, 8.0],
            [-4.0, 1.6, 26.0],
            [-1.5, 2.2, 80.0],
        ],
        8.5,
        Fades {
            arrives: false,
            leaves: true,
        },
    ),
];

impl Ship {
    /// A pass beginning at `start` across a screen `aspect` times as wide as
    /// it is tall, its course picked and varied from `rng`.
    pub(super) fn new(rng: &mut NonCryptoRng, start: f64, aspect: f64) -> Self {
        let (points, duration, fades) = COURSES[pick(rng, COURSES.len()) % COURSES.len()];
        let side = if rng.next_below(2) == 0 { 1.0 } else { -1.0 } * (aspect / SET_FOR).max(1.0);
        let flicker = rng.next_f64() * core::f64::consts::TAU;
        let duration = duration * (1.0 + VARY * (rng.next_f64() * 2.0 - 1.0));
        let mut course = points.map(|[x, y, z]| Vec3::new(side * x, y, z));
        for inner in &mut course[1..3] {
            *inner = *inner * (1.0 + VARY * (rng.next_f64() * 2.0 - 1.0));
        }
        Self {
            start,
            duration,
            course,
            fades,
            flicker,
        }
    }

    /// Whether the pass is over by `time`.
    pub(super) fn is_over(&self, time: f64) -> bool {
        time >= self.start + self.duration
    }

    /// Where the ship stands at `time`, relative to the camera, and how it is
    /// turned while the flight goes `speed` cells a second.
    fn pose(&self, time: f64, speed: f64) -> (Vec3, Frame, f64) {
        let share = ((time - self.start) / self.duration).clamp(0.0, 1.0);
        let (at, pace, bend) = curve(&self.course, share);
        let pull = bend * (1.0 / (self.duration * self.duration));
        let velocity = pace * (1.0 / self.duration) + Vec3::new(0.0, 0.0, speed);
        let ahead = velocity.normalized();
        let level = Vec3::UP.cross(ahead);
        let right = if level.length() > 1e-6 {
            level.normalized()
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        let up = ahead.cross(right);
        let bank = mathf::clamp(-BANK * pull.dot(right), -MOST_BANK, MOST_BANK);
        let frame = Frame {
            x: right,
            y: up,
            z: ahead,
        }
        .rotated_by(Frame::about(ahead, bank));
        let mut alpha: f64 = 1.0;
        if self.fades.arrives {
            alpha = alpha.min(mathf::smoothstep(share / FADE));
        }
        if self.fades.leaves {
            alpha = alpha.min(mathf::smoothstep((1.0 - share) / FADE));
        }
        (at, frame, alpha)
    }

    /// Set the ship out as it stands at `time`, the flight going `speed`
    /// cells a second.
    pub(super) fn stage(&self, time: f64, speed: f64, stage: &mut Stage) {
        let (seen, frame, alpha) = self.pose(time, speed);
        if alpha <= 0.0 {
            return;
        }
        let camera = stage.camera().position();
        let pose = Pose::new(camera + seen, frame);
        let from = pose.point_to_local(camera);
        let astern = from.z < STERN;
        let solid = |hull| Part::Solid {
            hull,
            pose,
            ink: &INK,
            alpha,
        };
        // The wing roots part the three: a wing on the camera's own side of
        // its root stands in front of the fuselage, the other behind it.
        let (port, body, starboard) = (
            solid(HullId::ShipPortWing),
            solid(HullId::ShipFuselage),
            solid(HullId::ShipStarboardWing),
        );
        let hull = if from.x > ROOT {
            [port, body, starboard]
        } else if from.x < -ROOT {
            [starboard, body, port]
        } else {
            [port, starboard, body]
        };
        stage.begin(pose.at);
        if astern {
            for part in hull {
                stage.push(part);
            }
        }
        let waver =
            1.0 + FLICKER * mathf::sin(core::f64::consts::TAU * FLICKER_HZ * time + self.flicker);
        let behind = -frame.z;
        for nozzle in NOZZLES {
            let base = pose.point_to_world(nozzle);
            let length = PLUME * waver;
            for (light, width, reach, strength) in [
                (FLAME, FLAME_WIDTH, 1.0, 0.6),
                (FLAME_CORE, CORE_WIDTH, CORE_LENGTH, 0.95),
            ] {
                stage.push(Part::Glow {
                    base,
                    tip: base + behind * (length * reach),
                    radius: NOZZLE_RADIUS * width,
                    light,
                    alpha: alpha * strength,
                });
            }
            stage.push(Part::Glow {
                base,
                tip: base,
                radius: NOZZLE_RADIUS * 1.7,
                light: NOZZLE_GLOW,
                alpha: alpha * 0.85,
            });
        }
        if !astern {
            for part in hull {
                stage.push(part);
            }
        }
    }
}

/// Where the cubic Bézier curve with control points `points` stands `share` of
/// the way along it, and its first and second derivatives there.
fn curve(points: &[Vec3; 4], share: f64) -> (Vec3, Vec3, Vec3) {
    let [p0, p1, p2, p3] = *points;
    let rest = 1.0 - share;
    let at = p0 * (rest * rest * rest)
        + p1 * (3.0 * rest * rest * share)
        + p2 * (3.0 * rest * share * share)
        + p3 * (share * share * share);
    let pace = (p1 - p0) * (3.0 * rest * rest)
        + (p2 - p1) * (6.0 * rest * share)
        + (p3 - p2) * (3.0 * share * share);
    let bend = (p2 - p1 * 2.0 + p0) * (6.0 * rest) + (p3 - p2 * 2.0 + p1) * (6.0 * share);
    (at, pace, bend)
}

#[cfg(test)]
#[path = "ship_tests.rs"]
mod tests;
