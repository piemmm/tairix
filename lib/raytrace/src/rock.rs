//! Rocks: boulders and stones built once as meshes and placed as often as a
//! scene strews them, broken as their rock breaks and worn as far as water
//! has carried them.
//!
//! A rock is a radius in every direction of an icosphere subdivided until
//! its facets are a few centimetres across a metre-wide stone: pushed in and
//! out by noise into rounded shoulders and shallow hollows, and cut back to
//! the few planes it broke along into the flat faces a fracture leaves. Wear
//! then takes it down where it stands proud of the stone about it, and only
//! there: corners first, then edges, its hollows last. Its length, breadth
//! and height are set last, so a flat stone is worn as it lies. Its vertex
//! normals are its faces' blended, so it shades smooth between the edges its
//! fractures leave sharp.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::{fallible, mathf};

use crate::noise::noise3;
use crate::prototype::{normals_of, Building, Facet, Part, Prototype};
use crate::vector::{singles, Vec3};

/// How many times the icosahedron is subdivided: 5120 facets.
const LEVELS: u32 = 4;

/// The most planes a rock is broken by: a slate's two cleavage planes and
/// the fractures across them, or another rock's fractures.
pub(crate) const MOST_FRACTURES: u32 = 8;

/// How long a stone's surface moves under its curvature, in its own unit of
/// size, for each unit of wear: a stone worn round by one, its edges rounded
/// to about half its inscribed radius; and the most wear any stone takes,
/// past which it has long since rounded.
const TIME_PER_WEAR: f64 = 0.1;
const MOST_WEAR: f64 = 4.0;

/// A step of wear as a share of the square of the shortest edge, which
/// keeps the explicit flow stable; the steps a unit of the work takes; and
/// how many steps the surface's normals, areas and weights serve before they
/// are worked out afresh, a step moving it far less than its facets' size.
const STABLE_STEP: f64 = 0.2;
const WEAR_UNIT: u32 = 48;
const REFRESH: u32 = 8;

/// The least share of a vertex's ray its surface is taken to face along,
/// so where a stone's surface turns nearly along the ray a step moves its
/// vertex at most a few times as far as the surface itself moves.
const LEAST_FACING: f64 = 0.25;

/// The rock a stone is of: how it breaks, and how readily water wears it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Lithology {
    Granite,
    Sandstone,
    Limestone,
    /// Splits along its cleavage into thin plates.
    Slate,
}

impl Lithology {
    /// Every rock a stream's bed can be of.
    pub(crate) const ALL: [Self; 4] =
        [Self::Granite, Self::Sandstone, Self::Limestone, Self::Slate];

    /// How far a stream carries a stone of this rock before it is worn round,
    /// in metres: the softer rocks round in a few kilometres, granite takes
    /// several times as far (Kuenen 1956; Domokos et al. 2014).
    pub(crate) const fn rounding(self) -> f64 {
        match self {
            Self::Granite => 8_000.0,
            Self::Sandstone => 3_000.0,
            Self::Limestone => 2_500.0,
            Self::Slate => 4_000.0,
        }
    }

    /// The most wear a stone of this rock shows however far it has come: a
    /// slate plate splits along its cleavage and breaks across as fast as its
    /// edges round, so it stays flat and angular.
    pub(crate) const fn most_wear(self) -> f64 {
        match self {
            Self::Slate => 0.08,
            Self::Granite | Self::Sandstone | Self::Limestone => f64::INFINITY,
        }
    }

    /// A habit of this rock drawn under `dice`: how flat and how long it
    /// broke, and along how many planes.
    pub(crate) fn habit(self, dice: &mut NonCryptoRng) -> Habit {
        let mut range = |low: f64, high: f64| low + (high - low) * dice.next_f64();
        match self {
            Self::Slate => Habit {
                squash: range(0.1, 0.28),
                elongation: range(0.45, 0.85),
                fractures: 3 + u32::from(range(0.0, 1.0) > 0.4) + u32::from(range(0.0, 1.0) > 0.7),
                cleaved: true,
            },
            // A block broken free of its outcrop is faceted all round.
            Self::Sandstone => Habit {
                squash: range(0.38, 0.72),
                elongation: range(0.6, 0.92),
                fractures: 4 + u32::from(range(0.0, 1.0) > 0.35) + u32::from(range(0.0, 1.0) > 0.7),
                cleaved: false,
            },
            Self::Granite | Self::Limestone => Habit {
                squash: range(0.48, 0.85),
                elongation: range(0.62, 0.95),
                fractures: 5 + u32::from(range(0.0, 1.0) > 0.3) + u32::from(range(0.0, 1.0) > 0.6),
                cleaved: false,
            },
        }
    }
}

/// The shape a rock broke to: its breadth and its height over its length,
/// how many planes it broke along, and whether two of them are a cleavage
/// splitting it into a plate.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Habit {
    pub(crate) squash: f64,
    pub(crate) elongation: f64,
    pub(crate) fractures: u32,
    pub(crate) cleaved: bool,
}

/// A rock being worn a bounded number of steps at a time, as broken, before
/// its proportions are set.
#[derive(Debug)]
pub(crate) struct Wearing {
    vertices: Vec<Vec3>,
    faces: Vec<[u32; 3]>,
    habit: Habit,
    /// How long the surface has yet to move, the step it moves by, and the
    /// steps taken.
    left: f64,
    step: f64,
    taken: u32,
    surface: Surface,
}

/// A worn mesh's surface as its last refresh left it: where each vertex's
/// neighbours start in `around`, one past the last for the last; each
/// neighbour and the cotangent weight of the edge to it; and each vertex's
/// outward normal and its share of the surface.
#[derive(Debug, Default)]
struct Surface {
    starts: Vec<usize>,
    around: Vec<(u32, f64)>,
    normals: Vec<Vec3>,
    areas: Vec<f64>,
    /// The positions a step is worked out from.
    before: Vec<Vec3>,
}

impl Wearing {
    /// A rock of `habit`, shaped under `seed` and broken as it breaks, to be
    /// worn by `wear`; `None` when the heap will not hold it.
    pub(crate) fn new(habit: Habit, wear: f64, seed: u64) -> Option<Self> {
        let (mut directions, mut faces) = icosahedron()?;
        for _ in 0..LEVELS {
            (directions, faces) = subdivide(&directions, &faces)?;
        }
        let mut dice = NonCryptoRng::seed_from_u64(seed);
        let salt = u32::try_from(seed & 0xffff_ffff).unwrap_or(0);
        let mut planes = [(Vec3::UP, 1.0f64); MOST_FRACTURES as usize];
        let count = breaks(habit, &mut dice, &mut planes);
        let cut = planes.get(..count)?;
        let mut vertices = Vec::new();
        vertices.try_reserve_exact(directions.len()).ok()?;
        for &direction in &directions {
            vertices.push(direction * radius(direction, cut, salt)?);
        }
        let shortest = shortest_edge(&vertices, &faces)?;
        let left = if wear.is_nan() {
            0.0
        } else {
            TIME_PER_WEAR * wear.clamp(0.0, MOST_WEAR)
        };
        let surface = if left > 0.0 {
            Surface::new(vertices.len(), &faces)?
        } else {
            Surface::default()
        };
        Some(Self {
            vertices,
            faces,
            habit,
            left,
            step: STABLE_STEP * shortest * shortest,
            taken: 0,
            surface,
        })
    }

    /// Wear the rock a unit of steps on: whether it is worn as far as it is
    /// to be, or `None` when the heap will not hold the work.
    pub(crate) fn step(&mut self) -> Option<bool> {
        for _ in 0..WEAR_UNIT {
            if self.left <= 0.0 || self.step <= 0.0 {
                return Some(true);
            }
            if self.taken.is_multiple_of(REFRESH) {
                self.surface.refresh(&self.vertices, &self.faces)?;
            }
            let step = self.step.min(self.left);
            self.surface.flow(&mut self.vertices, step)?;
            self.left -= step;
            self.taken += 1;
        }
        Some(self.left <= 0.0)
    }

    /// The worn rock, a unit long either way of its middle, as broad as its
    /// elongation and as high as its squash, its hierarchy still to build;
    /// `None` when the heap will not hold it.
    pub(crate) fn finish(self) -> Option<Building> {
        let vertices = proportioned(self.vertices, self.habit)?;
        let normals = normals_of(&vertices, &self.faces)?;
        let mut parts = Vec::new();
        parts.try_reserve_exact(self.faces.len()).ok()?;
        parts.extend(self.faces.iter().map(|&corners| {
            // Made in whatever each stone is placed in: wet, dry or mossed.
            Part::Facet(Facet {
                corners,
                material: None,
            })
        }));
        let mut stored_vertices = Vec::new();
        stored_vertices.try_reserve_exact(vertices.len()).ok()?;
        stored_vertices.extend(vertices.iter().map(|&vertex| singles(vertex)));
        Prototype::building(parts, stored_vertices, normals)
    }
}

/// A rock of `habit` worn by `wear`, made whole on the calling thread,
/// shaped under `seed`; `None` when the heap will not hold it.
#[cfg(test)]
pub(crate) fn rock(habit: Habit, wear: f64, seed: u64) -> Option<Building> {
    let mut wearing = Wearing::new(habit, wear, seed)?;
    while !wearing.step()? {}
    wearing.finish()
}

/// The shortest edge among `faces` of `vertices`.
fn shortest_edge(vertices: &[Vec3], faces: &[[u32; 3]]) -> Option<f64> {
    let mut shortest = f64::INFINITY;
    for &[a, b, c] in faces {
        let (pa, pb, pc) = (
            *vertices.get(a as usize)?,
            *vertices.get(b as usize)?,
            *vertices.get(c as usize)?,
        );
        shortest = shortest
            .min((pb - pa).length())
            .min((pc - pb).length())
            .min((pa - pc).length());
    }
    shortest.is_finite().then_some(shortest)
}

/// The planes `habit` breaks a rock along, drawn under `dice` into `planes`:
/// how many there are. A cleaved rock's first two are its cleavage, nearly
/// level either side of its middle, and the rest break it across.
fn breaks(habit: Habit, dice: &mut NonCryptoRng, planes: &mut [(Vec3, f64)]) -> usize {
    let mut drawn = 0;
    let mut add = |normal: Vec3, offset: f64| {
        if let Some(slot) = planes.get_mut(drawn) {
            *slot = (normal.normalized(), offset);
            drawn += 1;
        }
    };
    if habit.cleaved {
        for side in [1.0, -1.0] {
            let lean = 0.08 * (2.0 * dice.next_f64() - 1.0);
            let around = core::f64::consts::TAU * dice.next_f64();
            // Within the stone's least reach, so each cuts a face right
            // across it.
            add(
                Vec3::new(lean * mathf::cos(around), side, lean * mathf::sin(around)),
                0.5 + 0.12 * dice.next_f64(),
            );
        }
    }
    let across = habit.fractures.min(MOST_FRACTURES) as usize;
    for _ in 0..across {
        let rise = if habit.cleaved {
            0.25 * (2.0 * dice.next_f64() - 1.0)
        } else {
            2.0 * dice.next_f64() - 1.0
        };
        let around = core::f64::consts::TAU * dice.next_f64();
        let level = mathf::sqrt(1.0 - rise * rise);
        add(
            Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around)),
            0.62 + 0.25 * dice.next_f64(),
        );
    }
    drawn
}

/// How far the rock stands from its middle along `direction`: pushed out
/// and in by noise at three scales, and cut back to each plane it broke
/// along, a fracture's face keeping a little of the stone's grain.
fn radius(direction: Vec3, planes: &[(Vec3, f64)], seed: u32) -> Option<f64> {
    let grain = 0.012 * noise3(direction * 19.0, seed ^ 0x3c);
    let mut reach = 1.0
        + 0.26 * noise3(direction * 1.3, seed)
        + 0.1 * noise3(direction * 3.1, seed ^ 0x51)
        + 0.035 * noise3(direction * 8.0, seed ^ 0xa3)
        + grain;
    for &(normal, offset) in planes {
        let toward = direction.dot(normal);
        if toward > 1e-9 {
            reach = reach.min((offset + grain) / toward);
        }
    }
    (reach.is_finite() && reach > 0.0).then_some(reach)
}

impl Surface {
    /// The surface of a mesh of `count` vertices joined by `faces`, its
    /// weights still to work out; `None` when the heap will not hold it.
    fn new(count: usize, faces: &[[u32; 3]]) -> Option<Self> {
        let mut edges: Vec<(u32, u32)> = Vec::new();
        edges.try_reserve_exact(6 * faces.len()).ok()?;
        for &[a, b, c] in faces {
            for (p, q) in [(a, b), (b, c), (c, a)] {
                edges.push((p, q));
                edges.push((q, p));
            }
        }
        edges.sort_unstable();
        edges.dedup();
        let mut starts = Vec::new();
        starts.try_reserve_exact(count + 1).ok()?;
        let mut next = 0;
        for vertex in 0..=count {
            while edges
                .get(next)
                .is_some_and(|&(from, _)| (from as usize) < vertex)
            {
                next += 1;
            }
            starts.push(next);
        }
        let mut around = Vec::new();
        around.try_reserve_exact(edges.len()).ok()?;
        around.extend(edges.iter().map(|&(_, to)| (to, 0.0)));
        let mut normals = Vec::new();
        let mut areas = Vec::new();
        let mut before = Vec::new();
        if !(fallible::reserve(&mut normals, count)
            && fallible::reserve(&mut areas, count)
            && fallible::reserve(&mut before, count))
        {
            return None;
        }
        Some(Self {
            starts,
            around,
            normals,
            areas,
            before,
        })
    }

    /// The weights, normals and areas of the mesh `vertices` of `faces` as
    /// it stands: each edge weighed by the cotangents of the angles across
    /// from it (Meyer et al. 2003), kept from turning negative so a step
    /// never pushes a vertex out.
    fn refresh(&mut self, vertices: &[Vec3], faces: &[[u32; 3]]) -> Option<()> {
        for (_, weight) in &mut self.around {
            *weight = 0.0;
        }
        self.normals.clear();
        self.normals.resize(vertices.len(), Vec3::ZERO);
        self.areas.clear();
        self.areas.resize(vertices.len(), 0.0);
        for &[a, b, c] in faces {
            let corners = [a, b, c];
            let [pa, pb, pc] = [
                *vertices.get(a as usize)?,
                *vertices.get(b as usize)?,
                *vertices.get(c as usize)?,
            ];
            let normal = (pb - pa).cross(pc - pa);
            let area = 0.5 * normal.length();
            if area <= 0.0 {
                continue;
            }
            for (corner, (at, one, other)) in [(pa, pb, pc), (pb, pc, pa), (pc, pa, pb)]
                .into_iter()
                .enumerate()
            {
                let (u, v) = (one - at, other - at);
                let cotangent = (u.dot(v) / u.cross(v).length()).max(0.0);
                let (i, j) = (corners[(corner + 1) % 3], corners[(corner + 2) % 3]);
                *self.weight(i, j)? += cotangent;
                *self.weight(j, i)? += cotangent;
            }
            for corner in corners {
                *self.normals.get_mut(corner as usize)? += normal;
                *self.areas.get_mut(corner as usize)? += area / 3.0;
            }
        }
        for normal in &mut self.normals {
            *normal = normal.normalized();
        }
        Some(())
    }

    /// The weight of the edge from vertex `from` to vertex `to`.
    fn weight(&mut self, from: u32, to: u32) -> Option<&mut f64> {
        let (start, end) = (
            *self.starts.get(from as usize)?,
            *self.starts.get(from as usize + 1)?,
        );
        self.around
            .get_mut(start..end)?
            .iter_mut()
            .find(|(other, _)| *other == to)
            .map(|(_, weight)| weight)
    }

    /// One step of wear `step` long over `vertices`, in the mean-curvature
    /// term of Bloore's flow (Bloore 1977; Domokos and Gibbons 2012): the
    /// surface moves in as fast as it curves out there, and a flat face or a
    /// hollow, which curves in, stays where it is — the sand and gravel about
    /// a stone take away what stands out, rounding its edges first and
    /// reaching a hollow only once the stone about it has worn down to it.
    /// Each vertex keeps to its own ray from the stone's middle, moving along
    /// it as far as carries the surface in by its curvature, so the facets
    /// keep their shape however far the stone wears.
    fn flow(&mut self, vertices: &mut [Vec3], step: f64) -> Option<()> {
        self.before.clear();
        self.before.extend_from_slice(vertices);
        for (index, vertex) in vertices.iter_mut().enumerate() {
            let own = *self.before.get(index)?;
            let ring = self
                .around
                .get(*self.starts.get(index)?..*self.starts.get(index + 1)?)?;
            let pull = ring.iter().fold(Vec3::ZERO, |sum, &(other, weight)| {
                sum + (self.before.get(other as usize).copied().unwrap_or(own) - own) * weight
            });
            let (normal, area) = (*self.normals.get(index)?, *self.areas.get(index)?);
            if area <= 0.0 {
                continue;
            }
            // The Laplacian of the position is twice the mean curvature
            // along the inward normal.
            let curvature = -pull.dot(normal) / (4.0 * area);
            let reach = own.length();
            if curvature <= 0.0 || reach <= 0.0 {
                continue;
            }
            let ray = own * (1.0 / reach);
            let facing = normal.dot(ray).max(LEAST_FACING);
            *vertex = ray * (reach - step * curvature / facing).max(0.0);
        }
        Some(())
    }
}

/// The rock's `points`, given its habit's proportions: a unit long either
/// way along x, its elongation broad along z and its squash high along y.
fn proportioned(mut points: Vec<Vec3>, habit: Habit) -> Option<Vec<Vec3>> {
    let extent = |axis: fn(Vec3) -> f64| {
        points
            .iter()
            .map(|&point| axis(point).abs())
            .fold(0.0f64, f64::max)
    };
    let (x, y, z) = (extent(|p| p.x), extent(|p| p.y), extent(|p| p.z));
    if x <= 0.0 || y <= 0.0 || z <= 0.0 {
        return None;
    }
    let scale = Vec3::new(1.0 / x, habit.squash / y, habit.elongation / z);
    for point in &mut points {
        *point = Vec3::new(point.x * scale.x, point.y * scale.y, point.z * scale.z);
    }
    Some(points)
}

/// The twelve corners of an icosahedron on the unit sphere, and its twenty
/// faces wound outward.
pub(crate) fn icosahedron() -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let phi = crate::sample::GOLDEN_RATIO;
    let corners = [
        (-1.0, phi, 0.0),
        (1.0, phi, 0.0),
        (-1.0, -phi, 0.0),
        (1.0, -phi, 0.0),
        (0.0, -1.0, phi),
        (0.0, 1.0, phi),
        (0.0, -1.0, -phi),
        (0.0, 1.0, -phi),
        (phi, 0.0, -1.0),
        (phi, 0.0, 1.0),
        (-phi, 0.0, -1.0),
        (-phi, 0.0, 1.0),
    ];
    let faces = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let mut vertices = Vec::new();
    vertices.try_reserve_exact(corners.len()).ok()?;
    vertices.extend(
        corners
            .iter()
            .map(|&(x, y, z)| Vec3::new(x, y, z).normalized()),
    );
    let mut wound = Vec::new();
    wound.try_reserve_exact(faces.len()).ok()?;
    wound.extend_from_slice(&faces);
    Some((vertices, wound))
}

/// Each face of a sphere's mesh split into four, the new corners at its
/// edges' middles pushed out to the sphere.
pub(crate) fn subdivide(
    vertices: &[Vec3],
    faces: &[[u32; 3]],
) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    // Every edge once, its ends in order, so a face finds its neighbours'
    // midpoints by search rather than through a map.
    let mut edges: Vec<(u32, u32)> = Vec::new();
    edges.try_reserve_exact(3 * faces.len()).ok()?;
    for &[a, b, c] in faces {
        for (p, q) in [(a, b), (b, c), (c, a)] {
            edges.push((p.min(q), p.max(q)));
        }
    }
    edges.sort_unstable();
    edges.dedup();
    let mut grown = Vec::new();
    grown.try_reserve_exact(vertices.len() + edges.len()).ok()?;
    grown.extend_from_slice(vertices);
    for &(p, q) in &edges {
        let (a, b) = (*vertices.get(p as usize)?, *vertices.get(q as usize)?);
        grown.push((a + b).normalized());
    }
    let first = u32::try_from(vertices.len()).ok()?;
    let middle = |p: u32, q: u32| -> Option<u32> {
        let at = edges.binary_search(&(p.min(q), p.max(q))).ok()?;
        Some(first + u32::try_from(at).ok()?)
    };
    let mut split = Vec::new();
    split.try_reserve_exact(4 * faces.len()).ok()?;
    for &[a, b, c] in faces {
        let (ab, bc, ca) = (middle(a, b)?, middle(b, c)?, middle(c, a)?);
        split.extend_from_slice(&[[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
    }
    Some((grown, split))
}

#[cfg(test)]
#[path = "rock_tests.rs"]
mod tests;
