//! Explosions: a white-hot flash, a ring of light racing out across the
//! floor, sparks, and a wreck's shards tumbling away to lie where they land
//! and fade.
//!
//! Every piece keeps its own ballistic course from the instant the blast went
//! off, so a blast is the same whenever and however often it is drawn.

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_inline::ArrayVec;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::space::{Frame, Pose, Vec3};
use tairix_util::{fallible, mathf};

use super::wire::{HullId, Ink, Models, Part, Stage};
use super::Rgb;

/// The most blasts going off at once; a new one past it takes the place of
/// the one furthest spent.
const MAX_BLASTS: usize = 8;

/// The most shards and sparks one blast throws.
const MAX_SHARDS: usize = 12;
const MAX_SPARKS: usize = 10;

/// How fast things fall, in cells a second a second, and how high above the
/// floor a shard comes to rest.
const GRAVITY: f64 = 7.5;
const REST: f64 = 0.02;

/// How long a flash, its white core, the ring, a spark and a shard last, in
/// seconds, and how long a shard burns on whole before it fades.
const FLASH_S: f64 = 0.7;
const CORE_S: f64 = 0.4;
const RING_S: f64 = 1.25;
const SPARK_S: f64 = 0.85;
const SHARD_S: f64 = 2.1;
const SHARD_WHOLE_S: f64 = 0.9;

/// The flash's colours: its hot core, and the fire about it.
const CORE: Rgb = Rgb::new(255.0, 244.0, 214.0);
const FIRE: Rgb = Rgb::new(255.0, 132.0, 46.0);

/// How long a spark's streak is, as a share of its speed.
const STREAK: f64 = 0.045;

/// A spark's light.
static SPARK: Ink = Ink {
    edge: Rgb::new(255.0, 196.0, 110.0),
    face: Rgb::new(0.0, 0.0, 0.0),
};

/// The ring's plane: level on the floor.
const FLOOR: Frame = Frame {
    x: Vec3::new(1.0, 0.0, 0.0),
    y: Vec3::new(0.0, 0.0, 1.0),
    z: Vec3::new(0.0, -1.0, 0.0),
};

/// How big a blast is: a craft's wreck, or a shot striking the ground.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Size {
    Wreck,
    Strike,
}

impl Size {
    /// How far it reaches and hurls, against a wreck's.
    const fn scale(self) -> f64 {
        match self {
            Self::Wreck => 1.0,
            Self::Strike => 0.45,
        }
    }
}

/// Where and when a blast goes off, how big it is, and what it throws
/// is drawn in.
#[derive(Copy, Clone, Debug)]
pub(super) struct Burst {
    pub(super) at: Vec3,
    pub(super) start: f64,
    pub(super) size: Size,
    pub(super) ink: &'static Ink,
}

/// Something a wreck throws: one face of a solid, or the solid whole, as it
/// stood when the blast went off.
#[derive(Copy, Clone, Debug)]
pub(super) struct Piece {
    pub(super) hull: HullId,
    pub(super) face: Option<u8>,
    pub(super) pose: Pose,
}

/// One tumbling shard: what it is, the middle it turns about in its solid's
/// own coordinates, and how it flies and turns until it lands.
#[derive(Copy, Clone, Debug)]
struct Shard {
    piece: Piece,
    middle: Vec3,
    velocity: Vec3,
    axis: Vec3,
    spin: f64,
    landing: f64,
}

/// One blast.
#[derive(Debug)]
struct Blast {
    burst: Burst,
    shards: ArrayVec<Shard, MAX_SHARDS>,
    sparks: ArrayVec<Vec3, MAX_SPARKS>,
}

/// Every blast going off.
pub(super) struct Blasts {
    live: Vec<Blast>,
}

impl Blasts {
    /// Room for every blast that can go off at once; `None` when the heap
    /// will not give it.
    pub(super) fn new() -> Option<Self> {
        let mut live = Vec::new();
        fallible::reserve(&mut live, MAX_BLASTS).then_some(Self { live })
    }

    /// Set off `burst`, throwing `pieces` of the solids in `models`, its
    /// course scattered from `rng`.
    pub(super) fn spawn(
        &mut self,
        burst: Burst,
        pieces: &[Piece],
        models: &Models,
        rng: &mut NonCryptoRng,
    ) {
        let force = burst.size.scale();
        let mut blast = Blast {
            burst,
            shards: ArrayVec::new(),
            sparks: ArrayVec::new(),
        };
        for piece in pieces.iter().take(MAX_SHARDS) {
            let Some(hull) = models.get(piece.hull) else {
                continue;
            };
            let middle = piece.face.map_or(hull.centre(), |face| {
                let (sum, count) = hull
                    .face_corners(usize::from(face))
                    .fold((Vec3::ZERO, 0.0), |(sum, count), corner| {
                        (sum + corner, count + 1.0)
                    });
                if count > 0.0 {
                    sum * (1.0 / count)
                } else {
                    hull.centre()
                }
            });
            let start = piece.pose.point_to_world(middle);
            let outward = start - burst.at;
            let level = Vec3::new(outward.x, 0.0, outward.z).normalized();
            let velocity = level * (force * (0.8 + 1.8 * rng.next_f64()))
                + Vec3::UP * (force * (2.2 + 2.6 * rng.next_f64()));
            let spin =
                (2.0 + 7.0 * rng.next_f64()) * if rng.next_below(2) == 0 { 1.0 } else { -1.0 };
            let _ = blast.shards.try_push(Shard {
                piece: *piece,
                middle,
                velocity,
                axis: scatter(rng),
                spin,
                landing: landing(start.y, velocity.y),
            });
        }
        let sparks = if burst.size == Size::Wreck {
            MAX_SPARKS
        } else {
            MAX_SPARKS / 2
        };
        for _ in 0..sparks {
            let direction = scatter(rng);
            let _ = blast.sparks.try_push(
                Vec3::new(direction.x, mathf::fabs(direction.y) + 0.4, direction.z)
                    * (force * (3.0 + 4.0 * rng.next_f64())),
            );
        }
        if self.live.len() < MAX_BLASTS {
            self.live.push(blast);
        } else if let Some(oldest) = self
            .live
            .iter_mut()
            .min_by(|a, b| a.burst.start.total_cmp(&b.burst.start))
        {
            *oldest = blast;
        }
    }

    /// Every blast going off, as each was set off.
    #[cfg(test)]
    pub(super) fn bursts(&self) -> impl Iterator<Item = &Burst> {
        self.live.iter().map(|blast| &blast.burst)
    }

    /// Let go of every blast spent by `time`.
    pub(super) fn retire(&mut self, time: f64) {
        self.live.retain(|blast| time < blast.burst.start + SHARD_S);
    }

    /// Set out every blast as it stands at `time`.
    pub(super) fn stage(&self, time: f64, stage: &mut Stage) {
        for blast in &self.live {
            blast.stage(time - blast.burst.start, stage);
        }
    }
}

impl Blast {
    /// Set out the blast `age` seconds after it went off.
    fn stage(&self, age: f64, stage: &mut Stage) {
        if !(0.0..SHARD_S).contains(&age) {
            return;
        }
        let Burst { at, size, ink, .. } = self.burst;
        let scale = size.scale();
        if age < RING_S {
            let remaining = 1.0 - age / RING_S;
            stage.begin(at);
            stage.push(Part::Ring {
                centre: at + Vec3::UP * REST,
                frame: FLOOR,
                radius: scale * 2.6 * (1.0 - mathf::exp(-3.2 * age)),
                sides: 24,
                ink,
                alpha: remaining * remaining,
            });
        }
        if age < SPARK_S {
            let remaining = 1.0 - age / SPARK_S;
            let lift = Vec3::UP * (0.25 * scale);
            for &velocity in &self.sparks {
                let head = at + lift + fall(velocity, age);
                let tail = head - (velocity - Vec3::UP * (GRAVITY * age)) * STREAK;
                stage.begin(head);
                stage.push(Part::Line {
                    ends: [tail, head],
                    ink: &SPARK,
                    alpha: remaining,
                });
            }
        }
        let alpha = 1.0 - mathf::smoothstep((age - SHARD_WHOLE_S) / (SHARD_S - SHARD_WHOLE_S));
        for shard in &self.shards {
            let flown = age.min(shard.landing);
            let frame = shard
                .piece
                .pose
                .frame
                .rotated_by(Frame::about(shard.axis, shard.spin * flown));
            let middle =
                shard.piece.pose.point_to_world(shard.middle) + fall(shard.velocity, flown);
            let pose = Pose::new(middle - frame.to_world(shard.middle), frame);
            stage.begin(middle);
            stage.push(match shard.piece.face {
                Some(face) => Part::Shard {
                    hull: shard.piece.hull,
                    face,
                    pose,
                    ink,
                    alpha,
                },
                None => Part::Solid {
                    hull: shard.piece.hull,
                    pose,
                    ink,
                    alpha,
                },
            });
        }
        if age < FLASH_S {
            let centre = at + Vec3::UP * (0.3 * scale);
            let spread = scale * (0.35 + 1.25 * (1.0 - mathf::exp(-9.0 * age)));
            stage.begin(centre);
            stage.push(Part::Glow {
                base: centre,
                tip: centre,
                radius: spread,
                light: FIRE,
                alpha: 1.0 - age / FLASH_S,
            });
            stage.push(Part::Glow {
                base: centre,
                tip: centre,
                radius: spread * 0.5,
                light: CORE,
                alpha: (1.0 - age / CORE_S).max(0.0),
            });
        }
    }
}

/// How far something thrown at `velocity` has flown `age` seconds on.
fn fall(velocity: Vec3, age: f64) -> Vec3 {
    velocity * age - Vec3::UP * (0.5 * GRAVITY * age * age)
}

/// How long something thrown up at `rising` from `height` flies before it
/// comes to rest on the floor.
fn landing(height: f64, rising: f64) -> f64 {
    let drop = (height - REST).max(0.0);
    (rising + mathf::sqrt(rising * rising + 2.0 * GRAVITY * drop)) / GRAVITY
}

/// A direction scattered evenly over the sphere.
fn scatter(rng: &mut NonCryptoRng) -> Vec3 {
    let z = rng.next_f64() * 2.0 - 1.0;
    let turn = rng.next_f64() * TAU;
    let across = mathf::sqrt((1.0 - z * z).max(0.0));
    Vec3::new(across * mathf::cos(turn), across * mathf::sin(turn), z)
}

#[cfg(test)]
#[path = "blast_tests.rs"]
mod tests;
