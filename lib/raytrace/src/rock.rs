//! Rocks: boulders and stones built once as meshes and placed as often as a
//! scene strews them.
//!
//! A rock starts as an icosphere subdivided until its facets are a few
//! centimetres across a metre-wide stone, pushed in and out by noise into
//! rounded shoulders and shallow hollows, broken by a few cleavage planes
//! into the flat faces a fracture leaves, and flattened as its bedding lies.
//! Its vertex normals are its faces' blended, so it shades smooth between the
//! edges its fractures leave sharp.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::noise::noise3;
use crate::prototype::{stored, Facet, Part, Prototype};
use crate::vector::Vec3;

/// How many times the icosahedron is subdivided: 5120 facets.
const LEVELS: u32 = 4;

/// The most cleavage planes a rock is broken by.
pub(crate) const MOST_FRACTURES: u32 = 6;

/// The shape a rock takes: how flat its bedding leaves it, and how many
/// planes it has broken along.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Habit {
    /// Its height over its breadth.
    pub(crate) squash: f64,
    pub(crate) fractures: u32,
}

/// A rock of `habit`, about a unit across either way of its middle, in
/// material `stock`, shaped under `seed`; `None` when the heap will not
/// hold it.
pub(crate) fn rock(habit: Habit, stock: u16, seed: u64) -> Option<Prototype> {
    let (mut vertices, mut faces) = icosahedron()?;
    for _ in 0..LEVELS {
        (vertices, faces) = subdivide(&vertices, &faces)?;
    }
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let salt = u32::try_from(seed & 0xffff_ffff).unwrap_or(0);
    let mut planes = [(Vec3::UP, 1.0f64); MOST_FRACTURES as usize];
    let fractures = habit.fractures.min(MOST_FRACTURES) as usize;
    for plane in planes.iter_mut().take(fractures) {
        let rise = 2.0 * dice.next_f64() - 1.0;
        let around = core::f64::consts::TAU * dice.next_f64();
        let level = mathf::sqrt(1.0 - rise * rise);
        let normal = Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around));
        *plane = (normal, 0.62 + 0.25 * dice.next_f64());
    }
    for vertex in &mut vertices {
        *vertex = shape(*vertex, (habit.squash, &planes[..fractures]), salt);
    }
    let normals = normals_of(&vertices, &faces)?;
    let mut parts = Vec::new();
    parts.try_reserve_exact(faces.len()).ok()?;
    parts.extend(faces.iter().map(|&corners| {
        Part::Facet(Facet {
            corners,
            material: stock,
        })
    }));
    let mut stored_vertices = Vec::new();
    stored_vertices.try_reserve_exact(vertices.len()).ok()?;
    stored_vertices.extend(vertices.iter().map(|&vertex| stored(vertex)));
    Prototype::new(parts, stored_vertices, normals)
}

/// The point of the unit sphere along `direction` moved to the rock's
/// surface: pushed out and in by noise at three scales, squashed to its
/// bedding, and cut back to each cleavage plane it lies beyond.
fn shape(direction: Vec3, (squash, planes): (f64, &[(Vec3, f64)]), seed: u32) -> Vec3 {
    let swell = 0.26 * noise3(direction * 1.3, seed)
        + 0.1 * noise3(direction * 3.1, seed ^ 0x51)
        + 0.035 * noise3(direction * 8.0, seed ^ 0xa3)
        + 0.012 * noise3(direction * 19.0, seed ^ 0x3c);
    let mut point = direction * (1.0 + swell);
    point.y *= squash;
    for &(normal, offset) in planes {
        let beyond = point.dot(normal) - offset;
        if beyond > 0.0 {
            // Not quite to the plane, so a fracture face keeps a little of
            // the stone's roughness.
            point = point - normal * (0.94 * beyond);
        }
    }
    point
}

/// The twelve corners of an icosahedron on the unit sphere, and its twenty
/// faces wound outward.
fn icosahedron() -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
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
fn subdivide(vertices: &[Vec3], faces: &[[u32; 3]]) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
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

/// Each vertex's normal: the sum of its faces' area-weighted normals, made
/// unit.
fn normals_of(vertices: &[Vec3], faces: &[[u32; 3]]) -> Option<Vec<[f32; 3]>> {
    let mut sums = Vec::new();
    sums.try_reserve_exact(vertices.len()).ok()?;
    sums.resize(vertices.len(), Vec3::ZERO);
    for &[a, b, c] in faces {
        let (pa, pb, pc) = (
            *vertices.get(a as usize)?,
            *vertices.get(b as usize)?,
            *vertices.get(c as usize)?,
        );
        let weighted = (pb - pa).cross(pc - pa);
        for index in [a, b, c] {
            let sum = sums.get_mut(index as usize)?;
            *sum += weighted;
        }
    }
    let mut normals = Vec::new();
    normals.try_reserve_exact(sums.len()).ok()?;
    normals.extend(sums.iter().map(|&sum| stored(sum.normalized())));
    Some(normals)
}

#[cfg(test)]
#[path = "rock_tests.rs"]
mod tests;
