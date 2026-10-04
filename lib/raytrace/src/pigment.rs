//! What colour a surface is where a ray meets it: one colour, or a pattern
//! laid through the object's own texture space.
//!
//! Every pattern is averaged over the patch one pixel's view covers, so a
//! floor far off or seen edge on fades to its mean rather than shimmering.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::bark::Bark;
use crate::grass::{grass_kind, vigour, FLOWER, GRASS_KINDS, HEAD, LITTER, WEED};
use crate::ground::{Ground, Rock};
use crate::noise::{cell, cells2, cells3, noise3, octaves_within, smoothstep, turbulence3};
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
    /// Where on its surface, in the surface's own terms: a limb's distance
    /// along its stem and its angle round it, a leaf's place along and across
    /// its midrib.
    pub(crate) uv: (f64, f64),
    /// A limb's radius there, which its bark is wrapped round; nought where
    /// the surface is no limb.
    pub(crate) girth: f64,
    /// The key the instance met was placed under, so each of a crowd wears
    /// its pattern differently; nought for a shape that is one thing.
    pub(crate) instance: u32,
    /// Whether the ray met the surface's front: a leaf's upper side.
    pub(crate) front: bool,
    /// What a land is like where it was met — wet, worn or built up, on a
    /// road or a path, how much grows there — each `0.0..=1.0`; off the land,
    /// and on one carrying none of this, plain ground's.
    pub(crate) ground: [f64; 4],
    /// How much of the sky a sward's blades hide from the point, `0.0` in the
    /// open: ground under it shows the thatch at its roots.
    pub(crate) thatch: f64,
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
    /// Stones of one rock, flecked as `Speckle` is, each its own shade:
    /// its key blends it between the two `bases` and lightens or darkens it
    /// by up to `shade`; and up to `moss.0` of the faces each turns to the
    /// sky mantled in patches of moss of `moss.1`.
    Pebbles {
        bases: [Vec3; 2],
        flecks: [Vec3; 2],
        scale: f64,
        shade: f64,
        moss: (f64, Vec3),
        seed: u32,
    },
    /// Bark, laid over a limb by its distance along and round it.
    Bark(Bark),
    /// Leaves: veined, paler beneath, and in autumn browning at their edges
    /// and spotted.
    Foliage(Foliage),
    /// Bands of `a` and `b` across the texture's y axis, each `width` high.
    Stripes {
        a: Vec3,
        b: Vec3,
        width: f64,
    },
    /// A crowd of blades, weeds, flowers and fallen leaves.
    Crowd(Crowd),
    /// A land's ground, by its lie and what water and wear left on it.
    Ground(Ground),
    /// Bare rock, bedded and jointed.
    Rock(Rock),
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
            &Self::Pebbles {
                bases,
                flecks,
                scale,
                shade,
                moss,
                seed,
            } => {
                let key = mix32(spot.instance ^ seed);
                // Each stone weathered unevenly over its faces as well as its
                // own shade overall.
                let weathered = 1.0
                    + WEATHERING
                        * noise3(p * WEATHERED, key ^ 0x3c)
                        * (1.0 - smoothstep(0.25, 1.0, width * WEATHERED));
                let tint = (1.0 + shade * (2.0 * unit(mix32(key ^ 1)) - 1.0)) * weathered;
                let base = bases[0].lerp(bases[1], unit(key)) * tint;
                let stone = speckle(
                    p * scale,
                    (base, flecks.map(|fleck| fleck * tint)),
                    key,
                    width * scale,
                );
                mossed(stone, spot, moss, key)
            }
            Self::Bark(bark) => bark.colour(spot),
            Self::Foliage(foliage) => foliage.colour(spot),
            &Self::Stripes { a, b, width: band } => {
                a.lerp(b, 0.5 - 0.5 * square_wave(p.y / band, width / band))
            }
            Self::Crowd(crowd) => crowd.colour(spot),
            Self::Ground(ground) => ground.colour(spot),
            Self::Rock(rock) => rock.colour(p, spot.normal, width),
        }
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

/// `stone` at `spot` under up to `share` of moss of `colour`: on the faces
/// turned to the sky, in patches drawn under `key`, its cushions darker
/// and paler by turns and settling to their mean far off.
fn mossed(stone: Vec3, spot: &Spot, (share, colour): (f64, Vec3), key: u32) -> Vec3 {
    if share <= 0.0 {
        return stone;
    }
    let upward = smoothstep(0.15, 0.7, spot.normal.y);
    let patches = smoothstep(-0.2, 0.35, noise3(spot.p * MOSS_PATCHES, key ^ 0x3a));
    let cushions = 1.0
        + 0.3
            * noise3(spot.p * MOSS_CUSHIONS, key ^ 0x3b)
            * (1.0 - smoothstep(0.25, 1.0, spot.width * MOSS_CUSHIONS));
    stone.lerp(colour * cushions, share * upward * patches)
}

/// How much a stone's shade wanders over its faces as it weathers, and how
/// many of those patches span a metre.
const WEATHERING: f64 = 0.22;
const WEATHERED: f64 = 7.0;

/// How many of a mossed stone's patches, and of the cushions within them,
/// span a metre.
const MOSS_PATCHES: f64 = 2.4;
const MOSS_CUSHIONS: f64 = 40.0;

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

/// Snow lying on a tree.
pub(crate) const SNOW: Vec3 = Vec3::new(0.88, 0.9, 0.93);

/// How much of the snow a surface facing `normal` holds, `snow` the most any
/// does: none on its underside, all where it faces the sky.
pub(crate) fn lying(snow: f64, normal: Vec3) -> f64 {
    snow * smoothstep(0.2, 0.7, normal.y)
}

/// Leaves of a kind: the colours each leaf is one of, how much paler its
/// underside is, how much its veins show, in autumn the colour its edge
/// browns to and how spotted it is, and in winter how much snow lies on it.
#[derive(Clone, Debug)]
pub(crate) struct Foliage {
    pub(crate) colours: [Vec3; 4],
    pub(crate) underside: f64,
    pub(crate) veins: f64,
    pub(crate) edge: Vec3,
    pub(crate) browning: f64,
    pub(crate) spots: f64,
    pub(crate) snow: f64,
    pub(crate) outline: crate::leaf::Outline,
}

impl Foliage {
    fn colour(&self, spot: &Spot) -> Vec3 {
        let key = spot.mark;
        let base = self.colours[(key & 3) as usize];
        // No two leaves quite alike, and those facing the sun lightest.
        let shade = 0.82 + 0.3 * unit(mix32(key));
        let mut colour = base * shade;
        let (u, v) = spot.uv;
        let from = self.outline.off_midrib(u, v);
        // The midrib, and the veins leaving it toward the tip.
        let midrib = 1.0 - smoothstep(0.02, 0.06, from);
        let (_, stripe) = cell((u - 0.55 * v.abs()) * 9.0);
        let vein = (1.0 - smoothstep(0.05, 0.12, (stripe - 0.5).abs() * 2.0 - 0.8).max(0.0))
            * (1.0 - from);
        colour = colour * (1.0 + self.veins * (0.35 * midrib + 0.12 * vein));
        if self.browning > 0.0 {
            let edge = smoothstep(0.55, 1.0, from + 0.25 * unit(mix32(key ^ 3)));
            colour = colour.lerp(self.edge, edge * self.browning);
            let spots = cells2(u * 7.0 + unit(key) * 50.0, v * 4.0, key, 0.9);
            let blot = (1.0 - smoothstep(0.08, 0.2, spots.nearest)) * self.spots;
            colour = colour.lerp(self.edge * 0.55, blot * unit(mix32(spots.id)));
        }
        if !spot.front {
            // The underside is paler and duller, its hairs scattering light.
            let grey = (colour.x + colour.y + colour.z) / 3.0;
            colour = colour.lerp(Vec3::splat(grey), 0.35) * (1.0 + self.underside);
        }
        // Snow settles in clumps on what faces the sky, and on the needles of
        // a shoot more than their tips.
        let clump = 0.6 + 0.4 * unit(mix32(key ^ 0x5a));
        colour.lerp(SNOW, lying(self.snow, spot.normal) * clump)
    }
}

/// A crowd of small things over the ground, each coloured as its mark says
/// it is: a kind of grass's leaves and seed heads; a wildflower among them,
/// one of `blossoms`; a weed's leaf, one of `weeds`; a fallen leaf, one of
/// `fallen`, browning to the dark of rot as it decays.
#[derive(Clone, Debug)]
pub(crate) struct Crowd {
    pub(crate) grasses: [Blades; GRASS_KINDS],
    pub(crate) blossoms: [Vec3; 4],
    pub(crate) weeds: [Vec3; 2],
    pub(crate) fallen: [Vec3; 4],
}

/// A kind of grass's colours: the two greens its leaves are one or other of,
/// the straw their tips dry to, and its seed heads'.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Blades {
    pub(crate) leaves: [Vec3; 2],
    pub(crate) tip: Vec3,
    pub(crate) head: Vec3,
}

/// What a fallen leaf browns to as it rots.
const ROTTEN: Vec3 = Vec3::new(0.075, 0.052, 0.03);

impl Crowd {
    fn colour(&self, spot: &Spot) -> Vec3 {
        let mark = spot.mark;
        let shade = 0.82 + 0.36 * unit(mix32(mark));
        if mark & FLOWER != 0 {
            return self.blossoms[((mark >> 3) & 3) as usize];
        }
        if mark & WEED != 0 {
            // Paler along the midrib, and toward the tip where it catches
            // the light.
            let base = self.weeds[(mark & 1) as usize] * shade;
            return base
                * (0.8 + 0.3 * spot.along)
                * (1.0 + 0.15 * (1.0 - smoothstep(0.0, 0.15, spot.uv.1.abs())));
        }
        if mark & LITTER != 0 {
            let fresh = self.fallen[(mark & 3) as usize] * shade;
            return fresh.lerp(ROTTEN, 0.9 * smoothstep(0.3, 1.0, spot.along));
        }
        let Some(blades) = self.grasses.get(grass_kind(mark)) else {
            return Vec3::ZERO;
        };
        if mark & HEAD != 0 {
            // Ripening from the stem up.
            return blades.head * shade * (0.8 + 0.3 * spot.along);
        }
        // Thin grass dries toward straw from the tip down; rank grass stays
        // green to near its tip.
        let base = blades.leaves[(mark & 1) as usize];
        let dry = 0.7 * spot.along * spot.along + 0.45 * (1.0 - vigour(mark)) * spot.along;
        (base * shade).lerp(blades.tip, dry.min(1.0))
    }
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
