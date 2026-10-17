//! Procedural noise: gradient noise in two and three dimensions, the sums of
//! octaves that patterns and landforms are made of, and cellular noise for
//! stones, cobbles and cracks.
//!
//! Every lattice point is hashed rather than read from a table, so a pattern
//! needs no memory and differs under every seed, and every value is computed
//! in one fixed order of basic operations, so it is the same on every target.

#![no_std]
#![forbid(unsafe_code)]
// Bit for bit on every target: no float max or min, which may pick either zero.
#![deny(clippy::disallowed_methods)]
#![deny(missing_docs)]

use core::f64::consts::FRAC_1_SQRT_2;

use tairix_mix::{mix32, unit};
use tairix_util::mathf;
use tairix_util::space::Vec3;

/// The whole lattice cell `x` falls in, as bits to hash, and how far into
/// it `x` lies.
///
/// Truncation, then a step down for a negative with a fraction, is exactly
/// the floor over the `i32` range every coordinate a scene spans lies deep
/// inside, without the rounding routine that stands in for an instruction on
/// the baseline x86-64 target.
#[must_use]
#[inline]
pub fn cell(x: f64) -> (u32, f64) {
    // Saturating, and far outside anything a scene spans where it saturates.
    #[allow(clippy::cast_possible_truncation)]
    let truncated = x as i32;
    let whole = if f64::from(truncated) > x {
        truncated.saturating_sub(1)
    } else {
        truncated
    };
    (
        whole.cast_unsigned(),
        (x - f64::from(whole)).clamp(0.0, 1.0),
    )
}

/// Perlin's quintic fade.
#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The odd multipliers a lattice coordinate is hashed by, one to an axis.
const AXES: [u32; 3] = [0x8da6_b343, 0xd816_3841, 0xcb1a_b31f];

/// What salts a cellular feature's jitter along each axis, apart from its
/// cell's id and from each other.
const JITTER: [u32; 3] = [1, 2, 3];

/// The hash of lattice cell `(x, z)` under `seed`: [`hash3`]'s of the cell
/// `(x, 0, z)`, so a plane field and a space's drawn under one seed share
/// their draws there, and are kept apart by their seeds.
#[must_use]
#[inline]
pub fn hash2(x: u32, z: u32, seed: u32) -> u32 {
    mix32(x.wrapping_mul(AXES[0]) ^ z.wrapping_mul(AXES[2]) ^ seed)
}

/// The hash of lattice cell `(x, y, z)` under `seed`.
#[must_use]
#[inline]
pub fn hash3(x: u32, y: u32, z: u32, seed: u32) -> u32 {
    mix32(x.wrapping_mul(AXES[0]) ^ y.wrapping_mul(AXES[1]) ^ z.wrapping_mul(AXES[2]) ^ seed)
}

/// Each axis hashed once for both of a cell's walls.
fn walls(at: u32, odd: u32) -> [u32; 2] {
    [at.wrapping_mul(odd), at.wrapping_add(1).wrapping_mul(odd)]
}

/// Perlin's twelve cube-edge gradients, padded to sixteen with four repeated
/// so the top four bits of a hash pick one with neither a division nor a
/// branch.
const GRADIENTS: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

/// The eight directions of the plane's gradients, the diagonals shortened so
/// every one has the same length.
const GRADIENTS_2D: [[f64; 2]; 8] = [
    [1.0, 0.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [0.0, -1.0],
    [FRAC_1_SQRT_2, FRAC_1_SQRT_2],
    [-FRAC_1_SQRT_2, FRAC_1_SQRT_2],
    [FRAC_1_SQRT_2, -FRAC_1_SQRT_2],
    [-FRAC_1_SQRT_2, -FRAC_1_SQRT_2],
];

/// Which gradient a corner's hashed coordinates pick: the top bits of their
/// combination mixed through. A single multiply would leave the combination
/// of two axes' progressions regular enough to repeat a gradient along
/// lattice lines, which a surface lit at a grazing angle shows as stripes.
fn pick(hashed: u32, bits: u32) -> usize {
    (mix32(hashed) >> (32 - bits)) as usize
}

/// The steepest either gradient noise rises, a lattice cell to a lattice
/// cell, any way: past the most any choice of its corners' gradients makes
/// of it, space's 3.75 and the plane's 2.82.
pub const NOISE_SLOPE: f64 = 3.8;

/// Perlin's improved gradient noise in three dimensions: roughly
/// `-1.0..=1.0`, smooth everywhere, nought on every lattice point. Its
/// lattice's `y = 0` layer picks the gradients [`noise2`]'s does under the
/// same seed, as [`hash2`] and [`hash3`] share their draws there.
#[must_use]
#[inline]
pub fn noise3(p: Vec3, seed: u32) -> f64 {
    let ((x0, fx), (y0, fy), (z0, fz)) = (cell(p.x), cell(p.y), cell(p.z));
    let (xs, ys, zs) = (walls(x0, AXES[0]), walls(y0, AXES[1]), walls(z0, AXES[2]));
    // Folded in with the corner, not the cell: a wall's gradient is then the
    // same from the cells either side of it.
    let salt = mix32(seed);
    let corner = |dx: usize, dy: usize, dz: usize| {
        let (Some(hx), Some(hy), Some(hz)) = (xs.get(dx), ys.get(dy), zs.get(dz)) else {
            return 0.0;
        };
        let [gx, gy, gz] = GRADIENTS[pick(hx ^ hy ^ hz ^ salt, 4)];
        let step = |d: usize| if d == 0 { 0.0 } else { 1.0 };
        gx * (fx - step(dx)) + gy * (fy - step(dy)) + gz * (fz - step(dz))
    };
    let (u, v, w) = (fade(fx), fade(fy), fade(fz));
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u);
    mathf::bilinear([x00, x10, x01, x11], (v, w))
}

/// Gradient noise over the plane: roughly `-1.0..=1.0`.
#[must_use]
#[inline]
pub fn noise2(x: f64, z: f64, seed: u32) -> f64 {
    let ((x0, fx), (z0, fz)) = (cell(x), cell(z));
    let (xs, zs) = (walls(x0, AXES[0]), walls(z0, AXES[2]));
    let salt = mix32(seed);
    let corner = |dx: usize, dz: usize| {
        let (Some(hx), Some(hz)) = (xs.get(dx), zs.get(dz)) else {
            return 0.0;
        };
        let [gx, gz] = GRADIENTS_2D[pick(hx ^ hz ^ salt, 3)];
        let step = |d: usize| if d == 0 { 0.0 } else { 1.0 };
        gx * (fx - step(dx)) + gz * (fz - step(dz))
    };
    let (u, w) = (fade(fx), fade(fz));
    // The plane's gradients reach about 0.7 at most; this brings them to 1.
    1.41 * mathf::bilinear(
        [corner(0, 0), corner(1, 0), corner(0, 1), corner(1, 1)],
        (u, w),
    )
}

/// Octaves of plane noise summed, each `lacunarity` times finer and `gain`
/// times fainter than the last, divided by the weight they carry: roughly
/// `-1.0..=1.0`.
#[must_use]
pub fn fbm2(x: f64, z: f64, seed: u32, shape: (u32, f64, f64)) -> f64 {
    fbm2_resolved(x, z, seed, shape, 0.0)
}

/// How much of a noise whose lattice cells are `lattice` across survives
/// sampling `footprint` apart: none finer than two footprints, which would
/// only alias, all of it by four.
#[must_use]
#[inline]
pub fn resolved(footprint: f64, lattice: f64) -> f64 {
    smoothstep(2.0 * footprint, 4.0 * footprint, lattice)
}

/// [`fbm2`] sampled `footprint` of its own units apart: each octave
/// [`resolved`] there, the rest settling to their mean of nought while still
/// counting in the weight divided by, so the pattern keeps its scale.
#[must_use]
pub fn fbm2_resolved(
    x: f64,
    z: f64,
    seed: u32,
    (octaves, gain, lacunarity): (u32, f64, f64),
    footprint: f64,
) -> f64 {
    plane_octaves(
        (x, z),
        (octaves, gain, lacunarity),
        |octave, (scale, weight), (rx, rz)| {
            let kept = resolved(footprint, 1.0 / scale);
            if kept > 0.0 {
                weight * kept * noise2(rx, rz, seed.wrapping_add(octave))
            } else {
                0.0
            }
        },
    )
}

/// How much fainter and finer each octave of the ridged and billowed noises
/// is than the last: finer by just past two, so no octave's lattice lies on
/// another's.
const FOLDED: (f64, f64) = (0.5, 2.03);

/// Ridged octaves: each folded at nought so its zero lines become crests,
/// and weighted by the crest below, so ridges branch as eroded mountains
/// do: `0.0..=1.0` for a finite place.
#[must_use]
pub fn ridged2(x: f64, z: f64, seed: u32, octaves: u32) -> f64 {
    let mut previous = 1.0;
    plane_octaves(
        (x, z),
        (octaves, FOLDED.0, FOLDED.1),
        |octave, (_, weight), (rx, rz)| {
            let crest = 1.0 - noise2(rx, rz, seed.wrapping_add(octave)).abs();
            let crest = crest * crest * previous;
            previous = crest.clamp(0.0, 1.0);
            weight * crest
        },
    )
    .clamp(0.0, 1.0)
}

/// Billowed octaves: each folded at nought, so its crests round and its zero
/// lines crease into troughs, as wind-blown sand lies: `0.0..=1.0` for a
/// finite place, about a quarter on average.
#[must_use]
pub fn billow2(x: f64, z: f64, seed: u32, octaves: u32) -> f64 {
    plane_octaves(
        (x, z),
        (octaves, FOLDED.0, FOLDED.1),
        |octave, (_, weight), (rx, rz)| weight * noise2(rx, rz, seed.wrapping_add(octave)).abs(),
    )
    .clamp(0.0, 1.0)
}

/// `octaves` octaves of the plane about `(x, z)`, each `lacunarity` times
/// finer and `gain` times fainter than the last and turned, so no lattice
/// lines up from one to the next: `term` makes each octave's part of the sum
/// from its index, its scale and weight, and where it is read, and the sum is
/// divided by the weight they carry.
fn plane_octaves(
    (x, z): (f64, f64),
    (octaves, gain, lacunarity): (u32, f64, f64),
    mut term: impl FnMut(u32, (f64, f64), (f64, f64)) -> f64,
) -> f64 {
    let (mut sum, mut total, mut weight, mut scale) = (0.0, 0.0, 1.0, 1.0);
    for octave in 0..octaves {
        sum += term(octave, (scale, weight), turn(x * scale, z * scale, octave));
        total += weight;
        weight *= gain;
        scale *= lacunarity;
    }
    sum / mathf::fmax(total, f64::MIN_POSITIVE)
}

/// The plane turned by a fixed irrational angle, `atan(3/4)`, `octave`
/// times: the powers of `0.8 + 0.6i`, repeating every eight octaves, so no
/// octave's lattice lines up with one fewer than 256 times finer or coarser.
fn turn(x: f64, z: f64, octave: u32) -> (f64, f64) {
    const TURNS: [(f64, f64); 8] = [
        (1.0, 0.0),
        (0.8, 0.6),
        (0.28, 0.96),
        (-0.352, 0.936),
        (-0.8432, 0.5376),
        (-0.99712, -0.07584),
        (-0.752_192, -0.658_944),
        (-0.206_387_2, -0.978_470_4),
    ];
    let (cos, sin) = TURNS[(octave % 8) as usize];
    (x * cos - z * sin, x * sin + z * cos)
}

/// The most octaves a turbulent pattern sums.
pub const MAX_OCTAVES: u32 = 5;

/// How many octaves of a pattern are coarser than a footprint `detail` of the
/// pattern's own units across: finer ones would only shimmer, and cost as
/// much as the rest.
#[must_use]
pub fn octaves_within(detail: f64) -> u32 {
    let mut octaves = 1;
    let mut finest = 0.5;
    while octaves < MAX_OCTAVES && detail < finest * 0.5 {
        octaves += 1;
        finest *= 0.5;
    }
    octaves
}

/// Three-dimensional noise summed over `octaves`, each finer and fainter,
/// folded so its creases read as veins: roughly `0.0..1.0`.
#[must_use]
pub fn turbulence3(p: Vec3, seed: u32, octaves: u32) -> f64 {
    let (mut sum, mut scale, mut weight, mut total) = (0.0, 1.0, 1.0, 0.0);
    for octave in 0..octaves {
        sum += weight * noise3(p * scale, seed.wrapping_add(octave)).abs();
        total += weight;
        scale *= 2.0;
        weight *= 0.5;
    }
    sum / mathf::fmax(total, f64::MIN_POSITIVE)
}

/// What cellular noise knows at a point: the distances to the nearest two
/// feature points, the nearest one's own hash, and the way to it.
#[derive(Copy, Clone, Debug)]
pub struct Cells {
    /// The distance to the nearest feature point.
    pub nearest: f64,
    /// The distance to the second nearest.
    pub second: f64,
    /// The nearest feature point's hash.
    pub id: u32,
    /// The way from the point to the nearest feature point.
    pub toward: Vec3,
}

impl Cells {
    /// Nothing found yet: every feature point is nearer.
    const NONE: Self = Self {
        nearest: f64::INFINITY,
        second: f64::INFINITY,
        id: 0,
        toward: Vec3::ZERO,
    };

    /// How far the point is from the wall between its cell and the next,
    /// roughly: `0.0` on the wall.
    #[must_use]
    pub fn wall(&self) -> f64 {
        0.5 * (self.second - self.nearest)
    }
}

/// Worley's cellular noise over the plane, one jittered feature point per
/// unit square, `jitter` of the way from the square's middle to its edge:
/// the nearest two among the place's own square's and the eight about it,
/// which hold the true nearest wherever no feature strays past half way to
/// its square's edge, and nearly always beyond. A place that is no number
/// finds none.
#[must_use]
pub fn cells2(x: f64, z: f64, seed: u32, jitter: f64) -> Cells {
    let ((cx, fx), (cz, fz)) = (cell(x), cell(z));
    let mut found = Cells::NONE;
    for dz in [u32::MAX, 0, 1] {
        for dx in [u32::MAX, 0, 1] {
            let (ix, iz) = (cx.wrapping_add(dx), cz.wrapping_add(dz));
            let id = hash2(ix, iz, seed);
            let offset = |d: u32| if d == u32::MAX { -1.0 } else { f64::from(d) };
            // Each axis's jitter drawn apart from the id itself, so a field
            // shaded by its cells' ids does not follow where they stand.
            let px = offset(dx) + 0.5 + jitter * (unit(mix32(id ^ JITTER[0])) - 0.5);
            let pz = offset(dz) + 0.5 + jitter * (unit(mix32(id ^ JITTER[2])) - 0.5);
            record(&mut found, Vec3::new(px - fx, 0.0, pz - fz), id);
        }
    }
    found.nearest = mathf::sqrt(found.nearest);
    found.second = mathf::sqrt(found.second);
    found
}

/// Worley's cellular noise in three dimensions, searched as [`cells2`] is,
/// over the place's own cell and the 26 about it.
#[must_use]
pub fn cells3(p: Vec3, seed: u32, jitter: f64) -> Cells {
    cells3_among(p, seed, jitter, |_| true)
}

/// Worley's cellular noise in three dimensions among only the features
/// whose hashes `kept` holds, as a sparse scattering of things is found:
/// the nearest two of those within the cells about `p`.
///
/// The point's own cell is searched first, and a cell whose jittered
/// feature cannot lie nearer than the second nearest yet found is never
/// hashed, which spares most of the corners; nor is a feature not kept ever
/// placed.
#[must_use]
pub fn cells3_among(p: Vec3, seed: u32, jitter: f64, kept: impl Fn(u32) -> bool) -> Cells {
    let ((cx, fx), (cy, fy), (cz, fz)) = (cell(p.x), cell(p.y), cell(p.z));
    let mut found = Cells::NONE;
    let offset = |d: u32| if d == u32::MAX { -1.0 } else { f64::from(d) };
    // How near along one axis a feature of the cell `d` away can lie to `f`.
    let gap = |d: u32, f: f64| mathf::fmax((offset(d) + 0.5 - f).abs() - 0.5 * jitter, 0.0);
    for dz in [0, u32::MAX, 1] {
        let gz = gap(dz, fz);
        for dy in [0, u32::MAX, 1] {
            let gy = gap(dy, fy);
            for dx in [0, u32::MAX, 1] {
                let gx = gap(dx, fx);
                if gx * gx + gy * gy + gz * gz >= found.second {
                    continue;
                }
                let id = hash3(
                    cx.wrapping_add(dx),
                    cy.wrapping_add(dy),
                    cz.wrapping_add(dz),
                    seed,
                );
                if !kept(id) {
                    continue;
                }
                let jittered = |salt: u32, d: u32, f: f64| {
                    offset(d) + 0.5 + jitter * (unit(mix32(id ^ salt)) - 0.5) - f
                };
                let toward = Vec3::new(
                    jittered(JITTER[0], dx, fx),
                    jittered(JITTER[1], dy, fy),
                    jittered(JITTER[2], dz, fz),
                );
                record(&mut found, toward, id);
            }
        }
    }
    found.nearest = mathf::sqrt(found.nearest);
    found.second = mathf::sqrt(found.second);
    found
}

/// Take a feature point `toward` away, with its `id`, into what has been
/// found; its distances are squared until the search is done.
fn record(found: &mut Cells, toward: Vec3, id: u32) {
    let squared = toward.dot(toward);
    if squared < found.nearest {
        found.second = found.nearest;
        found.nearest = squared;
        found.id = id;
        found.toward = toward;
    } else if squared < found.second {
        found.second = squared;
    }
}

/// Hermite smoothstep from `edge0` to `edge1`: a step at `edge1` where the two
/// meet.
#[must_use]
#[inline]
pub fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let span = edge1 - edge0;
    if span.abs() > 0.0 {
        mathf::smoothstep((x - edge0) / span)
    } else if x >= edge1 {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
