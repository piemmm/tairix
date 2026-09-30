//! What colour a surface is where a ray meets it: one colour, or a pattern
//! laid through the object's own texture space.
//!
//! Every pattern is averaged over the patch one pixel's view covers, so a
//! floor far off or seen edge on fades to its mean rather than shimmering.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::grass::FLOWER;
use crate::noise::{
    cell, cells2, cells3, fbm2, noise2, noise3, octaves_within, smoothstep, turbulence3,
};
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// Where a pigment is looked up.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Spot {
    /// The point, in the object's own texture space.
    pub(crate) p: Vec3,
    /// The shading normal, in the object's texture frame.
    pub(crate) normal: Vec3,
    /// The point's height in the world, which land changes with.
    pub(crate) height: f64,
    /// How wide a patch one pixel's view of the surface covers.
    pub(crate) width: f64,
    /// Which one of a crowd of instances the ray met: a leaf, a blade.
    pub(crate) mark: u32,
    /// How far along its instance the ray met it, root to tip.
    pub(crate) along: f64,
}

/// Grass, earth, rock, sand and snow, as the lie of the land has them.
#[derive(Clone, Debug)]
pub(crate) struct Land {
    pub(crate) grass: Vec3,
    /// The dry, sun-faded grass patches blend toward.
    pub(crate) dry: Vec3,
    pub(crate) earth: Vec3,
    pub(crate) rock: Vec3,
    /// The darker bands of the rock's strata.
    pub(crate) strata: Vec3,
    pub(crate) sand: Vec3,
    pub(crate) snow: Vec3,
    /// The height up to which the shore is sand.
    pub(crate) shore: f64,
    /// The height from which snow lies on all but the steepest ground.
    pub(crate) snow_line: f64,
    /// How upright the ground must be for its rock to show through, as the
    /// vertical part of its normal.
    pub(crate) cliff: f64,
    /// How far the land spans, which the size of its patches follows.
    pub(crate) scale: f64,
    pub(crate) seed: u32,
}

/// A surface's colour.
#[derive(Clone, Debug)]
pub(crate) enum Pigment {
    Solid(Vec3),
    /// Squares of two colours on the texture's x–z plane, `size` across.
    Checker {
        a: Vec3,
        b: Vec3,
        size: f64,
    },
    /// Turbulent veins of `vein` through `base`.
    Marble {
        base: Vec3,
        vein: Vec3,
        scale: f64,
        seed: u32,
    },
    /// Growth rings about the texture's y axis, `scale` to a unit.
    Wood {
        light: Vec3,
        dark: Vec3,
        scale: f64,
        seed: u32,
    },
    /// Square tiles `size` across on the texture's x–z plane, each a shade
    /// between `a` and `b`, set in grout lines `gap` wide.
    Tiles {
        a: Vec3,
        b: Vec3,
        grout: Vec3,
        size: f64,
        gap: f64,
        seed: u32,
    },
    /// Bricks `size` long and high in running bond, each a shade between `a`
    /// and `b`, in mortar; laid on whichever face of the texture frame the
    /// surface turns toward.
    Bricks {
        a: Vec3,
        b: Vec3,
        mortar: Vec3,
        size: (f64, f64),
        seed: u32,
    },
    /// Floorboards on the texture's x–z plane, `width` wide and `length`
    /// long, their ends staggered, each with a grain of its own.
    Planks {
        light: Vec3,
        dark: Vec3,
        width: f64,
        length: f64,
        seed: u32,
    },
    /// Irregular stones `size` across, each a shade between `a` and `b`, in
    /// joints of `joint` `gap` wide: flagstones, cobbles, crazed glaze.
    Stones {
        a: Vec3,
        b: Vec3,
        joint: Vec3,
        size: f64,
        gap: f64,
        seed: u32,
    },
    /// A crystalline stone flecked with two other minerals: granite,
    /// terrazzo.
    Speckle {
        base: Vec3,
        flecks: [Vec3; 2],
        scale: f64,
        seed: u32,
    },
    /// Bark, its ridges running along the texture's y axis.
    Bark {
        light: Vec3,
        dark: Vec3,
        scale: f64,
        seed: u32,
    },
    /// Bands of `a` and `b` across the texture's y axis, each `width` high.
    Stripes {
        a: Vec3,
        b: Vec3,
        width: f64,
    },
    /// One of `colours` for each instance of a crowd, leaning toward `tip`
    /// along it and darkening toward its root, where its neighbours shade it:
    /// leaves, blades. A flower among them is one of `blossoms`.
    Crowd {
        colours: [Vec3; 4],
        tip: Vec3,
        blossoms: [Vec3; 4],
    },
    /// Land, by its slope and height.
    Land(Land),
}

impl Pigment {
    /// The colour at `spot`.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let (p, width) = (spot.p, spot.width);
        match self {
            &Self::Solid(colour) => colour,
            &Self::Checker { a, b, size } => {
                a.lerp(b, filtered_checker(p.x / size, p.z / size, width / size))
            }
            &Self::Marble {
                base,
                vein,
                scale,
                seed,
            } => marble(p * scale, (base, vein), seed, width * scale),
            &Self::Wood {
                light,
                dark,
                scale,
                seed,
            } => wood(p * scale, (light, dark), seed, width * scale),
            &Self::Tiles {
                a,
                b,
                grout,
                size,
                gap,
                seed,
            } => tile(p, (a, b, grout), (size, gap, seed), width),
            &Self::Bricks {
                a,
                b,
                mortar,
                size,
                seed,
            } => brick(spot, (a, b, mortar), size, seed),
            &Self::Planks {
                light,
                dark,
                width: board,
                length,
                seed,
            } => plank(p, (light, dark), (board, length), seed, width),
            &Self::Stones {
                a,
                b,
                joint,
                size,
                gap,
                seed,
            } => stone(p, (a, b, joint), (size, gap, seed), width),
            &Self::Speckle {
                base,
                flecks,
                scale,
                seed,
            } => speckle(p * scale, (base, flecks), seed, width * scale),
            &Self::Bark {
                light,
                dark,
                scale,
                seed,
            } => bark(p * scale, (light, dark), seed),
            &Self::Stripes { a, b, width: band } => {
                a.lerp(b, 0.5 - 0.5 * square_wave(p.y / band, width / band))
            }
            &Self::Crowd {
                colours,
                tip,
                blossoms,
            } => crowd(spot, (colours, tip, blossoms)),
            Self::Land(land) => land.colour(spot),
        }
    }
}

impl Land {
    fn colour(&self, spot: &Spot) -> Vec3 {
        let Self {
            grass,
            dry,
            earth,
            rock,
            strata,
            sand,
            snow,
            shore,
            snow_line,
            cliff,
            scale,
            seed,
        } = *self;
        let (p, upright) = (spot.p, spot.normal.y);
        let patch = fbm2(
            p.x / (0.08 * scale),
            p.z / (0.08 * scale),
            seed,
            (4, 0.5, 2.1),
        );
        let fine = noise2(p.x * 1.7, p.z * 1.7, seed ^ 3);
        let meadow = grass.lerp(dry, smoothstep(-0.1, 0.5, patch)) * (0.9 + 0.1 * fine);
        // Bare earth where the grass thins on a rise, rock where the ground
        // stands too steep for either.
        let bare = smoothstep(cliff + 0.12, cliff + 0.02, upright + 0.05 * fine);
        let ground = meadow.lerp(earth, bare);
        let banded =
            0.5 + 0.5 * mathf::sin(p.y * 1.7 + 2.0 * noise2(p.x * 0.05, p.z * 0.05, seed ^ 5));
        let stone = rock.lerp(strata, banded * banded) * (0.85 + 0.15 * fine);
        let rocky = smoothstep(cliff + 0.02, cliff - 0.08, upright + 0.04 * patch);
        let mut colour = ground.lerp(stone, rocky);
        let beach = smoothstep(shore + 1.2, shore + 0.2, spot.height + 0.6 * patch);
        colour = colour.lerp(sand, beach);
        // Snow settles on the flatter ground above its line, and on less of
        // it the lower it lies.
        let lying = smoothstep(
            snow_line - 0.04 * scale,
            snow_line + 0.04 * scale,
            spot.height + 0.06 * scale * patch,
        );
        let settles = smoothstep(0.55, 0.8, upright);
        colour.lerp(snow, lying * settles)
    }
}

/// Marble at `q`, in its own units, a footprint `detail` of them across.
fn marble(q: Vec3, (base, vein): (Vec3, Vec3), seed: u32, detail: f64) -> Vec3 {
    let swirl = turbulence3(q, seed, octaves_within(detail));
    let wave = 0.5 + 0.5 * mathf::sin(q.x * 1.3 + q.y * 0.7 + q.z * 0.4 + 5.0 * swirl);
    // A high power of the wave, so the veins run thin through broad fields
    // of the base stone.
    let (w2, w4) = (wave * wave, wave * wave * wave * wave);
    base.lerp(vein, w4 * w4 * w2)
}

fn stone(
    p: Vec3,
    (a, b, joint): (Vec3, Vec3, Vec3),
    (size, gap, seed): (f64, f64, u32),
    width: f64,
) -> Vec3 {
    let found = cells2(p.x / size, p.z / size, seed, 0.85);
    let soft = (0.5 * width / size).max(1e-4);
    let face = a.lerp(b, unit(found.id)) * (0.85 + 0.3 * unit(mix32(found.id)));
    let mottle = 0.9 + 0.1 * noise3(p * (4.0 / size), seed ^ found.id);
    let in_joint = 1.0 - smoothstep(gap - soft, gap + soft, found.wall());
    (face * mottle).lerp(joint, in_joint)
}

/// A flecked stone at `q`, in grains of its own units, a footprint `detail`
/// of them across.
fn speckle(q: Vec3, (base, flecks): (Vec3, [Vec3; 2]), seed: u32, detail: f64) -> Vec3 {
    let found = cells3(q, seed, 1.0);
    let grain = unit(found.id);
    // Grains too small to see average into the base.
    let resolved = 1.0 - smoothstep(0.3, 1.5, detail);
    let mineral = if grain < 0.2 {
        flecks[0]
    } else if grain < 0.32 {
        flecks[1]
    } else {
        base
    };
    let mean = base * 0.68 + flecks[0] * 0.2 + flecks[1] * 0.12;
    mean.lerp(mineral * (0.92 + 0.16 * unit(mix32(found.id))), resolved)
}

/// Bark at `q`, in its own units, its ridges along the y axis.
fn bark(q: Vec3, (light, dark): (Vec3, Vec3), seed: u32) -> Vec3 {
    let around = mathf::atan2(q.z, q.x);
    let ridge = noise2(around * 3.0, q.y * 0.35, seed) * 0.6
        + noise2(around * 9.0, q.y * 1.4, seed ^ 0x7) * 0.4;
    let groove = smoothstep(-0.2, 0.35, ridge);
    light.lerp(dark, 1.0 - groove) * (0.85 + 0.15 * noise3(q * 2.0, seed ^ 0xd))
}

/// The member of a crowd `spot` met, in one of `colours` lightening toward
/// `tip`, or a flower in one of `blossoms`.
fn crowd(spot: &Spot, (colours, tip, blossoms): ([Vec3; 4], Vec3, [Vec3; 4])) -> Vec3 {
    if spot.mark & FLOWER != 0 {
        return blossoms[((spot.mark >> 3) & 3) as usize];
    }
    let base = colours[(spot.mark & 3) as usize];
    let shade = 0.82 + 0.36 * unit(mix32(spot.mark));
    let lit = 0.45 + 0.55 * spot.along;
    (base * shade).lerp(tip, spot.along * spot.along * 0.7) * lit
}

/// How much of the second colour a checkerboard shows over a box `width`
/// squares across about `(x, z)`.
fn filtered_checker(x: f64, z: f64, width: f64) -> f64 {
    0.5 - 0.5 * square_wave(x, width) * square_wave(z, width)
}

/// A square wave of period two, `+1` then `-1`, averaged exactly over a box
/// `width` across about `at` (Quílez, "Filtering the checkerboard
/// pattern").
fn square_wave(at: f64, width: f64) -> f64 {
    let width = width.clamp(1e-4, 1e3);
    let edge = |at: f64| (cell(at * 0.5).1 - 0.5).abs();
    2.0 * (edge(at - 0.5 * width) - edge(at + 0.5 * width)) / width
}

/// The fine grain a wood's rings carry, finer than its rings by this.
const WOOD_GRAIN: f64 = 24.0;

/// Wood at `q`, in rings of its own units, a footprint `detail` of them across.
fn wood(q: Vec3, (light, dark): (Vec3, Vec3), seed: u32, detail: f64) -> Vec3 {
    let bent = mathf::sqrt(q.x * q.x + q.z * q.z) + 0.35 * noise3(q * 0.9, seed);
    let ring = 0.5 + 0.5 * mathf::cos(TAU * bent);
    // The fine grain averages to its middle once a pixel spans it.
    let grain = if detail * WOOD_GRAIN < 0.5 {
        0.5 + 0.5
            * noise3(
                Vec3::new(q.x * WOOD_GRAIN, q.y * 1.5, q.z * WOOD_GRAIN),
                seed ^ 0x51,
            )
    } else {
        0.5
    };
    light.lerp(
        dark,
        (ring * ring * ring * 0.8 + grain * 0.2).clamp(0.0, 1.0),
    )
}

fn tile(
    p: Vec3,
    (a, b, grout): (Vec3, Vec3, Vec3),
    (size, gap, seed): (f64, f64, u32),
    width: f64,
) -> Vec3 {
    let ((column, across), (row, down)) = (cell(p.x / size), cell(p.z / size));
    let edge = across.min(1.0 - across).min(down).min(1.0 - down) * size;
    let soft = 0.5 * width.max(1e-6);
    let in_grout = 1.0 - smoothstep(0.5 * gap - soft, 0.5 * gap + soft, edge);
    let key = mix32(column ^ mix32(row ^ seed));
    let shade = unit(key);
    let brightness = 0.88 + 0.24 * unit(mix32(key));
    // A faint cloud within each tile, so no two read as flat paint.
    let cloud = 0.93 + 0.07 * noise3(p * (3.0 / size), seed ^ key);
    let face = a.lerp(b, shade) * (brightness * cloud);
    // Seen from far enough that the grout is finer than the footprint, the
    // tile and the grout blend by the share of the floor each covers.
    let grout_share = (gap / size).clamp(0.0, 1.0) * 2.0;
    let far = smoothstep(0.5 * size, 2.0 * size, width);
    let mix = in_grout + (grout_share - in_grout) * far;
    face.lerp(grout, mix.clamp(0.0, 1.0))
}

/// The share of mortar a brick wall shows, and so what it averages to far
/// off.
const MORTAR: f64 = 0.06;

fn brick(
    spot: &Spot,
    (a, b, mortar): (Vec3, Vec3, Vec3),
    (long, high): (f64, f64),
    seed: u32,
) -> Vec3 {
    let (p, n) = (spot.p, spot.normal);
    // Laid on the face of the texture frame the surface turns most toward.
    let (along, up) = if n.x.abs() > n.z.abs() && n.x.abs() > n.y.abs() {
        (p.z, p.y)
    } else if n.y.abs() > n.z.abs() {
        (p.x, p.z)
    } else {
        (p.x, p.y)
    };
    let (row, down) = cell(up / high);
    let stagger = if row & 1 == 0 { 0.0 } else { 0.5 };
    let (column, across) = cell(along / long + stagger);
    let joint = (MORTAR * 0.5).max(1e-4);
    let edge = (across.min(1.0 - across) * long).min(down.min(1.0 - down) * high);
    let soft = (0.5 * spot.width).max(1e-5);
    let in_mortar = 1.0 - smoothstep(joint * high - soft, joint * high + soft, edge);
    let key = mix32(column ^ mix32(row ^ seed));
    let face = a.lerp(b, unit(key)) * (0.85 + 0.3 * unit(mix32(key)));
    let weathered = 0.9 + 0.1 * noise3(p * (6.0 / high), seed ^ key);
    let far = smoothstep(0.5 * high, 2.0 * high, spot.width);
    let mix = in_mortar + (MORTAR * 2.0 - in_mortar) * far;
    (face * weathered).lerp(mortar, mix.clamp(0.0, 1.0))
}

fn plank(
    p: Vec3,
    (light, dark): (Vec3, Vec3),
    (board, length): (f64, f64),
    seed: u32,
    width: f64,
) -> Vec3 {
    let (lane, across) = cell(p.x / board);
    let lane_key = mix32(lane ^ seed);
    let (piece, _) = cell(p.z / length + unit(lane_key));
    let key = mix32(piece ^ lane_key);
    // Each board cut from its own part of the log.
    let offset = Vec3::new(unit(key) * 40.0, 0.0, unit(mix32(key)) * 40.0);
    let grain = wood(
        Vec3::new(p.x / board * 1.4, p.y, p.z / length * 0.6) * 4.0 + offset,
        (light, dark),
        seed ^ key,
        width * 4.0 / board,
    );
    let tone = 0.85 + 0.3 * unit(mix32(key ^ 9));
    let soft = (0.5 * width / board).max(1e-4);
    let seam = 1.0 - smoothstep(0.012 - soft, 0.012 + soft, across.min(1.0 - across));
    (grain * tone).lerp(dark * 0.35, seam * 0.8)
}

#[cfg(test)]
#[path = "pigment_tests.rs"]
mod tests;
