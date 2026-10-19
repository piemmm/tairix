//! The perspective a mesh is seen through: world points carried into view
//! space, a triangle clipped to what the eye and the converter can take, and
//! each corner projected onto the screen the mesh path draws.
//!
//! View space has the eye at its origin, `x` across the screen, `y` down it
//! and `z` away from the eye. A point projects to `centre + focal·(x, y)/z`,
//! the centre being the middle of the screen,
//! and its [`Depth`](crate::mesh::Depth) is `scale/z`, the near plane mapping
//! to [`Depth::NEAREST`](crate::mesh::Depth::NEAREST): the reciprocal the mesh
//! path interpolates is the one produced here.
//!
//! Set up and evaluated in `f64` with the basic operations in a fixed order,
//! so a point projects to the same bits on every target.

use tairix_inline::ArrayVec;
use tairix_util::mathf;

use crate::mesh::{to_depth, MeshVertex, MESH_SUB_UNITS};
use crate::scan::MAX_DRAWING_EXTENT;

/// The most corners a clipped triangle has: each of the five planes it is cut
/// by adds at most one.
pub const MAX_CLIPPED: usize = 8;

/// How far from the screen's centre a clipped corner may land, in pixels.
///
/// A quarter of the converter's extent, so with the centre anywhere a surface
/// can put it, every corner that survives the clip projects to where the
/// converter's arithmetic stays exact.
const GUARD_PIXELS: u32 = MAX_DRAWING_EXTENT / 4;

/// The planes a triangle is clipped to.
#[derive(Copy, Clone, Debug)]
enum Bound {
    Near,
    Left,
    Right,
    Top,
    Bottom,
}

const BOUNDS: [Bound; 5] = [
    Bound::Near,
    Bound::Left,
    Bound::Right,
    Bound::Top,
    Bound::Bottom,
];

/// A perspective view: where the eye is, which way it looks, and the screen
/// it projects onto.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Perspective {
    eye: [f64; 3],
    right: [f64; 3],
    down: [f64; 3],
    forward: [f64; 3],
    focal: f64,
    centre: (f64, f64),
    /// Half the screen's width and height over the focal length: how far
    /// across and down the screen's edges lie one unit from the eye.
    spread: (f64, f64),
    near: f64,
    scale: f64,
    guard: f64,
}

/// Where a view-space point lands: its pixel position, a pixel `(x, y)`'s
/// centre lying at `(x + ½, y + ½)`, and its reciprocal depth, unrounded.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Projected {
    /// Pixels across.
    pub x: f64,
    /// Pixels down.
    pub y: f64,
    /// Reciprocal depth, in [`Depth`](crate::mesh::Depth)'s units.
    pub depth: f64,
}

/// One corner of a clipped triangle.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Clipped {
    /// Where it is in view space.
    pub view: [f64; 3],
    /// How much of each of the triangle's corners it is, summing to one.
    pub weights: [f64; 3],
    /// Where it lands on the screen.
    pub vertex: MeshVertex,
}

/// A corner while the clip is still cutting: not yet projected.
#[derive(Copy, Clone, Debug)]
struct Cut {
    view: [f64; 3],
    weights: [f64; 3],
}

impl Perspective {
    /// The view from `eye` along `forward`, with `right` across the screen
    /// and `down` down it, onto a `screen` of width and height in pixels
    /// `focal` pixels from the eye, seeing nothing nearer than `near`.
    ///
    /// The directions need not be unit or square to one another: `forward` is
    /// normalised and the other two are squared off against it, so a caller
    /// may hand over the vectors it holds. Which way `down` points is the
    /// caller's, so the view keeps whatever handedness its world has.
    ///
    /// `None` for a value that is not finite, a direction with no length or
    /// lying in the span of those before it, a focal length or near distance
    /// that is not positive, or a screen that is empty or wider than the
    /// converter can keep a clipped corner around.
    #[must_use]
    pub fn new(
        eye: [f64; 3],
        forward: [f64; 3],
        right: [f64; 3],
        down: [f64; 3],
        focal: f64,
        screen: (u32, u32),
        near: f64,
    ) -> Option<Self> {
        let finite = |v: [f64; 3]| v.iter().all(|c| c.is_finite());
        if !finite(eye) || !finite(forward) || !finite(right) || !finite(down) {
            return None;
        }
        if !(focal.is_finite() && focal > 0.0 && near.is_finite() && near > 0.0) {
            return None;
        }
        let guard = f64::from(GUARD_PIXELS);
        let centre = (f64::from(screen.0) * 0.5, f64::from(screen.1) * 0.5);
        if screen.0 == 0 || screen.1 == 0 || centre.0 > guard || centre.1 > guard {
            return None;
        }
        let forward = squared_off(forward, &[])?;
        let right = squared_off(right, &[forward])?;
        let down = squared_off(down, &[forward, right])?;
        Some(Self {
            eye,
            right,
            down,
            forward,
            focal,
            centre,
            spread: (centre.0 / focal, centre.1 / focal),
            near,
            scale: near * f64::from(u32::MAX),
            guard: guard / focal,
        })
    }

    /// Where the eye is.
    #[must_use]
    pub const fn eye(&self) -> [f64; 3] {
        self.eye
    }

    /// The nearest distance it sees.
    #[must_use]
    pub const fn near(&self) -> f64 {
        self.near
    }

    /// Pixels a unit of view-space offset spans one unit from the eye.
    #[must_use]
    pub const fn focal(&self) -> f64 {
        self.focal
    }

    /// The screen's centre, in pixels.
    #[must_use]
    pub const fn centre(&self) -> (f64, f64) {
        self.centre
    }

    /// `world` in view space.
    #[must_use]
    pub fn view(&self, world: [f64; 3]) -> [f64; 3] {
        self.direction(minus(world, self.eye))
    }

    /// A world direction in view space: turned, not moved.
    #[must_use]
    pub fn direction(&self, world: [f64; 3]) -> [f64; 3] {
        [
            dot(world, self.right),
            dot(world, self.down),
            dot(world, self.forward),
        ]
    }

    /// The unit world direction from the eye through the screen point
    /// `(x, y)`, in pixels.
    #[must_use]
    pub fn ray(&self, x: f64, y: f64) -> [f64; 3] {
        let (across, down) = (
            (x - self.centre.0) / self.focal,
            (y - self.centre.1) / self.focal,
        );
        let direction = [
            self.forward[0] + self.right[0] * across + self.down[0] * down,
            self.forward[1] + self.right[1] * across + self.down[1] * down,
            self.forward[2] + self.right[2] * across + self.down[2] * down,
        ];
        scaled(direction, 1.0 / mathf::sqrt(dot(direction, direction)))
    }

    /// The reciprocal depth of a point `distance` from the eye along its
    /// view, unrounded.
    #[must_use]
    pub fn depth(&self, distance: f64) -> f64 {
        self.scale / distance
    }

    /// Where `view` lands, or `None` nearer than the near plane.
    #[must_use]
    pub fn project(&self, view: [f64; 3]) -> Option<Projected> {
        let [x, y, z] = view;
        if !(x.is_finite() && y.is_finite() && z.is_finite() && z >= self.near) {
            return None;
        }
        let spread = self.focal / z;
        Some(Projected {
            x: self.centre.0 + x * spread,
            y: self.centre.1 + y * spread,
            depth: self.depth(z),
        })
    }

    /// `view` as a corner the mesh path takes without clipping, or `None`
    /// where [`Self::clip`] would cut it: nearer than the near plane, or past
    /// the band around the screen the clip keeps. A triangle is therefore
    /// drawn whole only where the clip would leave it whole, so it meets a
    /// clipped neighbour along their edge.
    #[must_use]
    pub fn vertex(&self, view: [f64; 3]) -> Option<MeshVertex> {
        if BOUNDS.iter().any(|&bound| self.distance(bound, view) < 0.0) {
            return None;
        }
        self.place(view)
    }

    /// `view` on the converter's grid, or `None` where it does not project
    /// within the converter's extent.
    fn place(&self, view: [f64; 3]) -> Option<MeshVertex> {
        let at = self.project(view)?;
        let extent = f64::from(MAX_DRAWING_EXTENT);
        if !(mathf::fabs(at.x) <= extent && mathf::fabs(at.y) <= extent) {
            return None;
        }
        let sub = f64::from(MESH_SUB_UNITS);
        Some(MeshVertex {
            x: mathf::round_i32(at.x * sub),
            y: mathf::round_i32(at.y * sub),
            depth: to_depth(at.depth),
        })
    }

    /// Whether anything of the world-space box spanning `low` to `high`
    /// within `reach` of the eye along its view can be on the screen.
    ///
    /// `false` where the whole box lies outside one of the view's planes —
    /// nearer than the near plane, past `reach`, beyond a screen edge — or
    /// the whole view lies outside one of the box's faces. The second half is
    /// what culls a tall box standing behind the eye, which every one of the
    /// view's planes has corners of on its inside.
    #[must_use]
    pub fn sees(&self, low: [f64; 3], high: [f64; 3], reach: f64) -> bool {
        let mut corners = [[0.0; 3]; 8];
        for (index, corner) in corners.iter_mut().enumerate() {
            let pick = |axis: usize| {
                if index & (1 << axis) == 0 {
                    low[axis]
                } else {
                    high[axis]
                }
            };
            *corner = self.view([pick(0), pick(1), pick(2)]);
        }
        let (sx, sy) = self.spread;
        let outside =
            |inside: &dyn Fn(&[f64; 3]) -> f64| corners.iter().all(|corner| inside(corner) < 0.0);
        if outside(&|p| p[2] - self.near)
            || outside(&|p| reach - p[2])
            || outside(&|p| sx * p[2] + p[0])
            || outside(&|p| sx * p[2] - p[0])
            || outside(&|p| sy * p[2] + p[1])
            || outside(&|p| sy * p[2] - p[1])
        {
            return false;
        }
        // The view as the pyramid from the eye to its four far corners.
        let mut pyramid = [self.eye; 5];
        for (index, corner) in pyramid.iter_mut().skip(1).enumerate() {
            let across = if index & 1 == 0 { -sx } else { sx } * reach;
            let down = if index & 2 == 0 { -sy } else { sy } * reach;
            for (axis, value) in corner.iter_mut().enumerate() {
                *value +=
                    self.forward[axis] * reach + self.right[axis] * across + self.down[axis] * down;
            }
        }
        (0..3).all(|axis| {
            !(pyramid.iter().all(|point| point[axis] < low[axis])
                || pyramid.iter().all(|point| point[axis] > high[axis]))
        })
    }

    /// Clip the view-space triangle `triangle` to the near plane and to the
    /// band around the screen its corners may project into, leaving the
    /// convex polygon that remains in `out`, each corner projected, or
    /// nothing where none does.
    ///
    /// Each corner carries its share of the triangle's own three, so whatever
    /// a caller interpolates across the triangle it interpolates across what
    /// remains. An edge is cut from its inside end, so two triangles sharing
    /// an edge cut it at the same point and stay watertight. A polygon whose
    /// rounding would have it need more than [`MAX_CLIPPED`] corners, or a
    /// corner that would not project, leaves nothing rather than a wrong
    /// shape.
    pub fn clip(&self, triangle: [[f64; 3]; 3], out: &mut ArrayVec<Clipped, MAX_CLIPPED>) {
        out.clear();
        if triangle.iter().any(|v| v.iter().any(|c| !c.is_finite())) {
            return;
        }
        let mut kept = ArrayVec::<Cut, MAX_CLIPPED>::new();
        for (index, view) in triangle.into_iter().enumerate() {
            let mut weights = [0.0; 3];
            weights[index] = 1.0;
            if kept.try_push(Cut { view, weights }).is_err() {
                return;
            }
        }
        let mut cut = ArrayVec::<Cut, MAX_CLIPPED>::new();
        for bound in BOUNDS {
            if kept
                .iter()
                .all(|corner| self.distance(bound, corner.view) >= 0.0)
            {
                continue;
            }
            cut.clear();
            for (index, &from) in kept.iter().enumerate() {
                let to = kept[(index + 1) % kept.len()];
                let (d_from, d_to) = (
                    self.distance(bound, from.view),
                    self.distance(bound, to.view),
                );
                let inside = d_from >= 0.0;
                if inside && cut.try_push(from).is_err() {
                    return;
                }
                if inside != (d_to >= 0.0) {
                    let (a, b, d_a, d_b) = if inside {
                        (from, to, d_from, d_to)
                    } else {
                        (to, from, d_to, d_from)
                    };
                    let t = d_a / (d_a - d_b);
                    let corner = Cut {
                        view: self.onto(bound, between(a.view, b.view, t)),
                        weights: between(a.weights, b.weights, t),
                    };
                    if cut.try_push(corner).is_err() {
                        return;
                    }
                }
            }
            core::mem::swap(&mut kept, &mut cut);
            if kept.len() < 3 {
                return;
            }
        }
        for corner in &kept {
            let Some(vertex) = self.place(corner.view) else {
                out.clear();
                return;
            };
            // Room for every corner kept: both hold the same number.
            let _ = out.try_push(Clipped {
                view: corner.view,
                weights: corner.weights,
                vertex,
            });
        }
    }

    /// How far inside `bound` the view-space point `p` lies.
    fn distance(&self, bound: Bound, p: [f64; 3]) -> f64 {
        match bound {
            Bound::Near => p[2] - self.near,
            Bound::Left => self.guard * p[2] + p[0],
            Bound::Right => self.guard * p[2] - p[0],
            Bound::Top => self.guard * p[2] + p[1],
            Bound::Bottom => self.guard * p[2] - p[1],
        }
    }

    /// `p`, a cut on `bound`, put exactly on it: the interpolated point lies
    /// on the plane only to within a rounding, and a corner a hair nearer
    /// than the near plane would not project. A side cut toward a corner the
    /// near plane cut can round nearer than it, so its distance is held there
    /// first; that only moves it further inside the other sides.
    fn onto(&self, bound: Bound, mut p: [f64; 3]) -> [f64; 3] {
        if !matches!(bound, Bound::Near) {
            p[2] = p[2].max(self.near);
        }
        match bound {
            Bound::Near => p[2] = self.near,
            Bound::Left => p[0] = -(self.guard * p[2]),
            Bound::Right => p[0] = self.guard * p[2],
            Bound::Top => p[1] = -(self.guard * p[2]),
            Bound::Bottom => p[1] = self.guard * p[2],
        }
        p
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn minus(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scaled(a: [f64; 3], by: f64) -> [f64; 3] {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn between(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// How much of a direction must be left once the axes before it are taken
/// out for it to name a direction of its own: below this it lies in their
/// span to within rounding, and what remains is noise.
const INDEPENDENT: f64 = 1e-9;

/// `v` with its share along each unit axis of `axes` removed, scaled to unit
/// length, or `None` where too little of it remains.
fn squared_off(v: [f64; 3], axes: &[[f64; 3]]) -> Option<[f64; 3]> {
    let length = mathf::sqrt(dot(v, v));
    let mut rest = v;
    for axis in axes {
        rest = minus(rest, scaled(*axis, dot(rest, *axis)));
    }
    let left = mathf::sqrt(dot(rest, rest));
    (left > length * INDEPENDENT && left.is_finite()).then(|| scaled(rest, 1.0 / left))
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
