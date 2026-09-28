//! Continental plates, and the uplift field their boundaries produce.
//!
//! This is the stage that stops a heightfield looking like noise. Plain
//! fractal noise has no reason for a range to be where it is; a plate
//! partition does. Two plates converging raise a belt along the seam they
//! share, two diverging open a rift and the inland sea that floods it, and
//! two sliding past each other leave a lineament that later stages follow.
//!
//! # Why a jittered grid rather than a scattered point set
//!
//! The partition is a Voronoi diagram over one seed point per cell of a
//! coarse square grid, each jittered inside its cell. That keeps the
//! nearest-seed search to the nine cells around a query — a bounded,
//! position-anchored neighbourhood — where a freely scattered point set
//! would need either a global index or an unbounded search. Terrain loses
//! nothing by it: the jitter is most of a cell, so the cells are not
//! visible in the result.

use tairix_util::mathf;

use crate::params::RealmParams;
use crate::seed::{SeedKey, Stage};
use crate::voronoi::{self, wrap, SITE_JITTER};

/// How a plate moves, and what it is made of.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Plate {
    /// Grid cell the plate's seed was drawn in.
    pub cell: (i32, i32),
    /// The seed point, in plate-grid units.
    pub site: (f64, f64),
    /// Drift direction and speed, in plate-grid units.
    pub drift: (f64, f64),
    /// `0.0` oceanic through `1.0` cratonic. Continental plates ride
    /// higher and resist subduction, which is why an ocean-continent
    /// convergence gives a coastal range and an ocean-ocean one an arc.
    pub buoyancy: f64,
    /// `0.0` youngest through `1.0` oldest. An old, buoyant plate's interior
    /// has been planed down to its crystalline shield.
    pub age: f64,
}

/// What two plates are doing where they meet.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Boundary {
    /// Closing speed along the seam normal: positive converging, negative
    /// diverging.
    pub convergence: f64,
    /// Distance to the seam, in plate-grid units.
    pub distance: f64,
    /// Mean buoyancy of the two plates.
    pub buoyancy: f64,
}

/// The tectonic answer at one point.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tectonics {
    /// Vertical displacement, in `-1.0..1.0`: positive mountain belt,
    /// negative rift or basin.
    pub uplift: f64,
    /// How strongly this point sits in a mountain belt, `0.0..1.0`. The
    /// relief stage ridges its noise by this.
    pub belt: f64,
    /// How strongly this point sits in a rift, where two plates are pulling
    /// apart, `0.0..1.0`.
    pub rift: f64,
    /// The buoyancy of the plate the point belongs to.
    pub buoyancy: f64,
}

/// Width of the deformed zone either side of a seam, in plate-grid units.
///
/// A seam is a line, but a mountain belt is not: this is what gives the
/// belt an across-strike profile instead of a crease.
const BELT_HALF_WIDTH: f64 = 0.34;

/// The plate field of a realm.
///
/// Holds no plates: every plate is recomputed from its grid cell on
/// demand, which is cheaper than a lookup and makes the field a pure
/// function rather than a structure whose build order could matter.
#[derive(Copy, Clone, Debug)]
pub struct Plates {
    key: SeedKey,
    /// Plate-grid cells along one edge of the realm.
    grid: u32,
}

impl Plates {
    /// The plate field for `params`.
    ///
    /// The grid is sized so its cell count is the requested plate count as
    /// closely as a square grid allows: plates are a coarse structural
    /// choice, and an operator asking for twelve is asking for "about a
    /// dozen", not for a partition that has to be exactly twelve.
    #[must_use]
    pub fn new(params: RealmParams) -> Self {
        let grid = isqrt_round(params.plates()).max(2);
        Self {
            key: SeedKey::new(params.seed()),
            grid,
        }
    }

    /// Plate-grid cells along one edge of the realm.
    #[must_use]
    pub const fn grid(self) -> u32 {
        self.grid
    }

    /// The plate whose seed the grid cell `(cx, cy)` holds.
    ///
    /// Cells outside the realm are wrapped onto it, so a query at the edge
    /// still sees nine neighbours and the partition has no boundary
    /// artefact — the wrap is in the *seed lookup* only, so the field
    /// itself does not repeat inside the realm.
    #[must_use]
    pub fn plate(self, cx: i32, cy: i32) -> Plate {
        let (wx, wy) = (wrap(cx, self.grid), wrap(cy, self.grid));
        let mut stream = self.key.stream(Stage::Plates, wx, wy);
        let jx = stream.signed() * SITE_JITTER;
        let jy = stream.signed() * SITE_JITTER;
        let angle = stream.unit() * core::f64::consts::TAU;
        let speed = 0.25 + stream.unit() * 0.75;
        let buoyancy = stream.unit();
        let age = stream.unit();
        Plate {
            cell: (wx, wy),
            site: (f64::from(cx) + 0.5 + jx, f64::from(cy) + 0.5 + jy),
            drift: (mathf::cos(angle) * speed, mathf::sin(angle) * speed),
            buoyancy,
            age,
        }
    }

    /// The two nearest plates to `(x, y)` in plate-grid units, nearest
    /// first.
    #[must_use]
    pub fn nearest_two(self, x: f64, y: f64) -> (Plate, Plate) {
        let [(_, near), (_, far)] = voronoi::nearest::<Plate, 2>((x, y), |cx, cy| {
            let plate = self.plate(cx, cy);
            (plate.site, plate)
        });
        (near, far)
    }

    /// What the plates are doing where they meet at `(x, y)`.
    #[must_use]
    pub fn boundary(self, x: f64, y: f64) -> Boundary {
        let (near, far) = self.nearest_two(x, y);
        boundary_between(near, far, (x, y))
    }

    /// The uplift and belt strength at `(x, y)` in plate-grid units.
    #[must_use]
    pub fn tectonics(self, x: f64, y: f64) -> Tectonics {
        let (near, far) = self.nearest_two(x, y);
        let boundary = boundary_between(near, far, (x, y));

        // A seam's influence falls off across strike; beyond the belt
        // half-width the interior of a plate is tectonically quiet and its
        // relief is whatever the noise stages give it.
        let proximity = 1.0 - mathf::clamp(boundary.distance / BELT_HALF_WIDTH, 0.0, 1.0);
        let across = proximity * proximity * (3.0 - 2.0 * proximity);

        // A convergence between two buoyant plates has nowhere to put the
        // shortening but up; one involving an oceanic plate subducts, which
        // raises less and narrower. Divergence thins the crust either way.
        let closing = mathf::clamp(boundary.convergence, -1.0, 1.0);
        let raised = if closing > 0.0 {
            closing * (0.45 + 0.55 * boundary.buoyancy)
        } else {
            closing * 0.8
        };

        Tectonics {
            uplift: raised * across,
            belt: if closing > 0.0 { closing * across } else { 0.0 },
            rift: if closing < 0.0 {
                -closing * across
            } else {
                0.0
            },
            buoyancy: near.buoyancy,
        }
    }
}

/// What two plates are doing where they meet, at one point.
fn boundary_between(near: Plate, far: Plate, point: (f64, f64)) -> Boundary {
    let seam = seam_normal(near.site, far.site);
    let relative = (far.drift.0 - near.drift.0, far.drift.1 - near.drift.1);
    Boundary {
        convergence: -(relative.0 * seam.0 + relative.1 * seam.1),
        distance: seam_distance(near.site, far.site, point),
        buoyancy: f64::midpoint(near.buoyancy, far.buoyancy),
    }
}

/// The unit normal of the seam between two sites, pointing from the first
/// toward the second. A degenerate pair — which the jitter bound excludes —
/// yields a fixed axis rather than a division by zero.
fn seam_normal(near: (f64, f64), far: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (far.0 - near.0, far.1 - near.1);
    let length = mathf::hypot(dx, dy);
    if length <= f64::EPSILON {
        return (1.0, 0.0);
    }
    (dx / length, dy / length)
}

/// Distance from `point` to the perpendicular bisector of two sites — the
/// seam itself, which is where the Voronoi cells meet.
fn seam_distance(near: (f64, f64), far: (f64, f64), point: (f64, f64)) -> f64 {
    let normal = seam_normal(near, far);
    let midpoint = (f64::midpoint(near.0, far.0), f64::midpoint(near.1, far.1));
    let offset = (point.0 - midpoint.0, point.1 - midpoint.1);
    mathf::fabs(offset.0 * normal.0 + offset.1 * normal.1)
}

/// The integer square root of `value`, rounded to nearest.
///
/// Integer arithmetic throughout: a floating-point square root followed by
/// a round is the one place a plate grid could differ by one between two
/// targets, and one plate more or less is a different world.
fn isqrt_round(value: u32) -> u32 {
    let floor = u64::from(value).isqrt();
    let rounded = if u64::from(value) > floor * floor + floor {
        floor + 1
    } else {
        floor
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the rounded root of a u32 is at most 65536"
    )]
    {
        rounded as u32
    }
}

#[cfg(test)]
mod tests;
