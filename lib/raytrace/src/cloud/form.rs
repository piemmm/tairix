//! What a deck's cloud is made of where it stands: its weather, the heaps or
//! sheets it lays over its columns, their turrets, and the extinction their
//! water or ice holds.
//!
//! A deck's weather map says how much of each column it covers and where its
//! base and top lie there. Over each footprint a cloud stands as an envelope —
//! steep-sided domes for heaps, a plateau for sheets — on a flat base at the
//! level its air condenses, the envelope's surface pushed out into turrets on
//! turrets by billowing noise at three scales, each settling to its mean where
//! a footprint is too coarse to see it. Water holds extinction rising with
//! height as adiabatic liquid water does, so a heap is thinnest at its base
//! and densest high in its body; ice holds it evenly.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::{Deck, Matter};
use crate::band;
use crate::heightfield::bilinear;
use crate::noise::{cell, fbm2_resolved, hash3, noise2, resolved, smoothstep};
use crate::sample::{mix32, unit};
use crate::vector::{power, real, single, Vec3};

/// Texels along each edge of the billowing noise, and of the finer one.
pub(super) const SHAPE_SIDE: usize = 64;
pub(super) const DETAIL_SIDE: usize = 32;

const _: () = assert!(SHAPE_SIDE.is_power_of_two() && DETAIL_SIDE.is_power_of_two());

/// How far apart the finer noise's features lie, over the billows'.
const DETAIL_SCALE: f64 = 0.2;
/// The wisps' cellular octaves: the cells along a period of their texture,
/// the weight of each and its seed's salt, the finest last.
const WISP_FINEST: f64 = 16.0;
const WISP_OCTAVES: [(f64, f64, u32); 3] =
    [(4.0, 0.625, 5), (8.0, 0.25, 6), (WISP_FINEST, 0.125, 7)];
/// The finer reads of the wisps' texture a sharp edge is cut by beyond its
/// own octaves: how many times finer each is read, how much it adds, and
/// where in the texture it starts, so no two line up.
pub(super) const FINER: [(f64, f64, Vec3); 2] = [
    (4.13, 0.34, Vec3::new(11.3, 5.7, 23.1)),
    (16.9, 0.17, Vec3::new(3.9, 17.2, 8.6)),
];

/// The cover at which a cloud's edge stands: its footprint is where the
/// cover passes it.
const FOOTPRINT: f64 = 0.5;
/// How a deck's envelope rises from a footprint's edge, as the power of how
/// far into the footprint a column lies: a sheet's soon as high as it
/// stands, a plateau; a heap's on up toward its middle, steep-sided domes.
const PROFILE: (f64, f64) = (0.08, 0.35);
/// How steeply the envelope falls below the base beyond a footprint's edge,
/// so turrets rise from the heap and nowhere else.
const FALL: f64 = 4.0;
/// The spread of the field a deck's weather is drawn from, as measured over
/// it: near enough normal that its quantiles say how much of the sky each
/// cover leaves covered. And how fast a column's cover rises past a
/// footprint's edge, reaching its whole two spreads in.
const COVER_SPREAD: f64 = 0.11;
const COVER_GAIN: f64 = (1.0 - FOOTPRINT) / (2.0 * COVER_SPREAD);
/// The bubbles pushing out an envelope's surface into turrets on turrets,
/// scale on scale: how many times finer each scale is read than the first,
/// where in the texture it starts so no two line up, and its reach as a
/// share of a billow, at its least for a sheet and its most for a heap.
const BUBBLES: [(f64, Vec3, (f64, f64)); 4] = [
    (1.0, Vec3::new(0.0, 0.0, 0.0), (0.45, 1.4)),
    (2.31, Vec3::new(17.3, 5.1, 29.7), (0.2, 0.62)),
    (5.27, Vec3::new(3.7, 29.3, 11.9), (0.1, 0.28)),
    (11.9, Vec3::new(23.1, 13.7, 7.3), (0.05, 0.12)),
];
/// How far the wisps fray a cloud's edge, as a share of a billow: as far for
/// a sheet as for a heap.
const FRAYING: f64 = 0.08;
/// The bubbles' cells along each side of a period of their texture, and the
/// least and greatest radius a bubble swells to, as a share of a cell.
const BUBBLE_CELLS: f64 = 4.0;
const BUBBLE_RADII: (f64, f64) = (0.45, 0.95);
/// How sharply a cloud's edge stands, in metres, before a footprint widens
/// it: its droplets evaporate into the dry air about it over so little.
const SKIN: f64 = 12.0;
/// The footprint a march too coarse to resolve a cloud's edges takes them at,
/// as a share of a billow: its steps' length.
pub(super) const AVERAGED: f64 = 0.25;
/// Liquid water rises evenly with height above a cloud's base, as rising air
/// condenses it, and extinction with its two-thirds power, as the droplets it
/// holds grow (Stephens, 1978): a deck's extinction is taken this high above
/// its base, and no column is taken as thinner than this above it.
const ADIABATIC: f64 = 1000.0;
const LEAST_RISE: f64 = 1.0;

/// How a read of the cloud takes its edges: bounding every read as finely
/// resolved as a footprint so many metres across; as a march too coarse for
/// them takes them; or resolved to a footprint so many metres across, each
/// scale finer than it settled to its mean.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) enum Edges {
    Bounding(f64),
    Averaged,
    Resolved(f64),
}

/// A texture of noise that repeats along each axis, a power of two texels
/// on a side.
#[derive(Clone, Debug)]
pub(super) struct Tile {
    pub(super) side: usize,
    texels: Vec<u8>,
}

impl Tile {
    pub(super) fn new(side: usize) -> Option<Self> {
        Some(Self {
            side,
            texels: fallible::filled(side.checked_mul(side)?.checked_mul(side)?, 0)?,
        })
    }

    /// The most the texture holds anywhere, which no read of it passes.
    fn peak(&self) -> f64 {
        f64::from(self.texels.iter().copied().max().unwrap_or(u8::MAX)) / 255.0
    }

    /// The mean of its texels.
    fn mean(&self) -> f64 {
        let sum: usize = self.texels.iter().map(|&texel| usize::from(texel)).sum();
        real(sum) / (255.0 * real(self.texels.len().max(1)))
    }

    /// The texture read trilinearly at `p`, in texels, wrapping.
    pub(super) fn at(&self, p: Vec3) -> f64 {
        let (side, mask) = (self.side, self.side - 1);
        let wrap = |value: f64| {
            let (whole, fraction) = cell(value);
            (usize::try_from(whole).unwrap_or(0) & mask, fraction)
        };
        let ((x, fx), (y, fy), (z, fz)) = (wrap(p.x), wrap(p.y), wrap(p.z));
        let (x1, y1, z1) = ((x + 1) & mask, (y + 1) & mask, (z + 1) & mask);
        let texel = |x: usize, y: usize, z: usize| {
            f64::from(
                self.texels
                    .get((z * side + y) * side + x)
                    .copied()
                    .unwrap_or(0),
            )
        };
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        let face = |z: usize| {
            lerp(
                lerp(texel(x, y, z), texel(x1, y, z), fx),
                lerp(texel(x, y1, z), texel(x1, y1, z), fx),
                fy,
            )
        };
        lerp(face(z), face(z1), fz) / 255.0
    }
}

/// The two noises a bank's clouds are cut from, and what is known of them
/// once built: the billows' mean and peak, and the wisps' mean.
#[derive(Clone, Debug)]
pub(super) struct Textures {
    pub(super) shape: Tile,
    pub(super) detail: Tile,
    shape_mean: f64,
    shape_peak: f64,
    pub(super) wisp_mean: f64,
}

impl Textures {
    pub(super) fn new() -> Option<Self> {
        Some(Self {
            shape: Tile::new(SHAPE_SIDE)?,
            detail: Tile::new(DETAIL_SIDE)?,
            shape_mean: 0.5,
            shape_peak: 1.0,
            wisp_mean: 0.5,
        })
    }

    /// Layers `layers` of the billows, drawn from `seed`; their mean and
    /// peak once the last is.
    pub(super) fn fill_shape(&mut self, layers: Range<usize>, runner: &dyn JobRunner, seed: u32) {
        let last = layers.end >= self.shape.side;
        fill_tile(&mut self.shape, layers, runner, &|p| bubbles(p, seed));
        if last {
            (self.shape_mean, self.shape_peak) = (self.shape.mean(), self.shape.peak());
        }
    }

    /// Layers `layers` of the wisps, drawn from `seed`; their mean once the
    /// last is.
    pub(super) fn fill_detail(&mut self, layers: Range<usize>, runner: &dyn JobRunner, seed: u32) {
        let last = layers.end >= self.detail.side;
        fill_tile(&mut self.detail, layers, runner, &|p| wisps(p, seed));
        if last {
            self.wisp_mean = self.detail.mean();
        }
    }

    /// The wisps at `texel` of their texture, each of the [`FINER`] reads
    /// added about the texture's mean as far as `kept` resolves it.
    pub(super) fn wisp_at(&self, texel: Vec3, kept: [f64; FINER.len()]) -> f64 {
        let mut wisp = self.detail.at(texel);
        for (&(times, share, start), kept) in FINER.iter().zip(kept) {
            if kept > 0.0 {
                wisp += share * kept * (self.detail.at(texel * times + start) - self.wisp_mean);
            }
        }
        wisp.clamp(0.0, 1.0)
    }

    /// The most the turrets of `deck` push its envelope out anywhere.
    pub(super) fn reach(&self, deck: &Deck) -> f64 {
        let bubbles: f64 = bubble_reach(deck).iter().sum();
        bubbles * (self.shape_peak - self.shape_mean)
            + FRAYING * deck.billow * (1.0 - self.wisp_mean)
    }

    /// How far `deck`'s turrets push its envelope out at `grained`, its
    /// billows' own coordinates, each scale finer than `footprint` metres
    /// settled to its mean; or, `bounding`, the most any finer scale could
    /// past the first.
    fn turrets(&self, deck: &Deck, grained: Vec3, footprint: f64, bounding: bool) -> f64 {
        let reach = bubble_reach(deck);
        let period = deck.billow * 8.0;
        let texel = grained * (real(SHAPE_SIDE) / period);
        let rise = self.shape_peak - self.shape_mean;
        let mut out = 0.0;
        for (index, (&(times, start, _), reach)) in BUBBLES.iter().zip(reach).enumerate() {
            if bounding && index > 0 {
                out += reach * rise;
                continue;
            }
            // A scale's bubbles are some half a cell across.
            let kept = if index == 0 {
                1.0
            } else {
                resolved(footprint, 0.5 * period / (BUBBLE_CELLS * times))
            };
            if kept > 0.0 {
                out += kept * reach * (self.shape.at(texel * times + start) - self.shape_mean);
            }
        }
        let fraying = FRAYING * deck.billow;
        if bounding {
            return out + fraying * (1.0 - self.wisp_mean);
        }
        let span = period * DETAIL_SCALE;
        let finest = span / WISP_FINEST;
        let kept = resolved(footprint, finest);
        if kept > 0.0 {
            let finer = FINER.map(|(times, ..)| resolved(footprint, finest / times));
            let wisp = self.wisp_at(grained * (real(DETAIL_SIDE) / span), finer);
            out += kept * fraying * (wisp - self.wisp_mean);
        }
        out
    }
}

/// How far `deck`'s bubbles reach at each scale, in metres: a heap's the
/// most, a sheet's the least.
fn bubble_reach(deck: &Deck) -> [f64; BUBBLES.len()] {
    let heap = deck.heap.clamp(0.0, 1.0);
    BUBBLES.map(|(_, _, (sheet, heaped))| deck.billow * (sheet + (heaped - sheet) * heap))
}

/// One column of a deck's weather map: how much the deck covers there, and
/// where its cloud's base and top lie.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct Column {
    pub(super) cover: f32,
    pub(super) base: f32,
    pub(super) top: f32,
}

impl Column {
    pub(super) fn lerp(self, other: Self, t: f64) -> Self {
        let mix = |a: f32, b: f32| single(f64::from(a) + (f64::from(b) - f64::from(a)) * t);
        Self {
            cover: mix(self.cover, other.cover),
            base: mix(self.base, other.base),
            top: mix(self.top, other.top),
        }
    }
}

/// A point among a deck's weather columns: the four about it, and how far
/// east and north across them it lies; each field blended only as it is
/// read.
pub(super) struct Blend {
    pub(super) corners: [Column; 4],
    pub(super) across: (f64, f64),
}

impl Blend {
    /// The deck's `field` at the point.
    pub(super) fn at(&self, field: fn(&Column) -> f32) -> f64 {
        bilinear(
            self.corners.map(|corner| f64::from(field(&corner))),
            self.across,
        )
    }

    /// The deck's whole column at the point.
    pub(super) fn column(&self) -> Column {
        Column {
            cover: single(self.at(|corner| corner.cover)),
            base: single(self.at(|corner| corner.base)),
            top: single(self.at(|corner| corner.top)),
        }
    }
}

/// How high above its base `deck`'s envelope stands over a column `cover`
/// covered and `depth` deep: up toward `depth` within its footprint, falling
/// away below the base beyond it.
fn envelope(deck: &Deck, cover: f64, depth: f64) -> f64 {
    let into = (cover - FOOTPRINT) / (1.0 - FOOTPRINT);
    if into <= 0.0 {
        return depth * into * FALL;
    }
    let heap = deck.heap.clamp(0.0, 1.0);
    let rising = PROFILE.0 + (PROFILE.1 - PROFILE.0) * heap;
    depth * power(into.min(1.0), rising)
}

/// What a deck's cloud holds where it stands: its extinction per metre, and
/// how far within its surface the place lies, past the edge its droplets
/// fray at, in metres.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Held {
    pub(super) extinction: f64,
    pub(super) within: f64,
}

/// What `deck`'s cloud holds at `altitude` over `column`, read at `grained`
/// in its billows' coordinates with its edges as `edges` has them; `None`
/// where none stands.
pub(super) fn cloud(
    deck: &Deck,
    textures: &Textures,
    (column, altitude): (&Blend, f64),
    grained: Vec3,
    edges: Edges,
) -> Option<Held> {
    let base = column.at(|corner| corner.base);
    let rise = altitude - base;
    if rise <= 0.0 {
        return None;
    }
    let depth = (column.at(|corner| corner.top) - base).max(0.0);
    let held = envelope(deck, column.at(|corner| corner.cover), depth) - rise;
    let footprint = edges_footprint(deck, edges);
    let skin = skin(deck, footprint);
    if held + textures.reach(deck) + 0.5 * skin <= 0.0 {
        return None;
    }
    let reach = textures.turrets(
        deck,
        grained,
        footprint,
        matches!(edges, Edges::Bounding(_)),
    );
    // A step centred on the surface, so a coarser footprint widens it about
    // the surface rather than eating into the cloud.
    let surface = held + reach;
    let density = smoothstep(-0.5 * skin, 0.5 * skin, surface);
    (density > 0.0).then(|| Held {
        extinction: density * extinction(deck, rise),
        within: surface - 0.5 * skin,
    })
}

/// How wide `deck`'s cloud's edge stands, in metres, read through a
/// `footprint` metres across: never sharper than its droplets make it, and
/// never wider than a billow, so its bands hold every read.
fn skin(deck: &Deck, footprint: f64) -> f64 {
    SKIN.max(footprint.min(deck.billow))
}

/// The footprint, in metres, a read of `deck` taken as `edges` has them
/// resolves its edges to.
fn edges_footprint(deck: &Deck, edges: Edges) -> f64 {
    match edges {
        Edges::Bounding(footprint) | Edges::Resolved(footprint) => footprint.max(0.0),
        Edges::Averaged => AVERAGED * deck.billow,
    }
}

/// `deck`'s extinction per metre at full density `rise` metres above its
/// base: water's rising as its liquid water does, ice's even.
pub(super) fn extinction(deck: &Deck, rise: f64) -> f64 {
    match deck.matter {
        Matter::Water => deck.thickness * power(rise.max(LEAST_RISE) / ADIABATIC, 2.0 / 3.0),
        Matter::Ice => deck.thickness,
    }
}

/// A deck's column at `(x, z)`, its weather drawn only as finely as columns
/// `spacing` apart hold: how much it covers, and its base and top.
pub(super) fn weather(deck: &Deck, (x, z): (f64, f64), spacing: f64) -> Column {
    let (cos, sin) = (mathf::cos(deck.heading), mathf::sin(deck.heading));
    let along = (x * cos + z * sin) / (deck.scale * deck.stretch);
    let across = (-x * sin + z * cos) / deck.scale;
    // Its features across the wind are its narrowest.
    let footprint = spacing / deck.scale;
    // Pushed about by a slower field, so the cover gathers in drifts.
    let (u, v) = (
        along + 0.5 * noise2(along * 0.3, across * 0.3, deck.seed ^ 0x51),
        across + 0.5 * noise2(along * 0.3 + 3.1, across * 0.3 + 1.7, deck.seed ^ 0x93),
    );
    let field = fbm2_resolved(u, v, deck.seed, (5, 0.55, 2.02), footprint);
    // The same field's broadest octaves alone: high where a cloud is broad,
    // low over a blob the finer octaves alone lift past its edge.
    let broad = fbm2_resolved(u, v, deck.seed, (2, 0.55, 2.02), footprint);
    let drift = fbm2_resolved(
        u * 0.23,
        v * 0.23,
        deck.seed ^ 0x77,
        (3, 0.5, 2.0),
        footprint * 0.23,
    );
    // How far into a cloud's footprint a column lies: its edge where the
    // drifting field passes the quantile that leaves the deck's cover of the
    // sky covered, its middle as far again beyond.
    let edge = COVER_SPREAD * normal_quantile(deck.cover);
    let into = |field: f64| {
        (FOOTPRINT + (0.55 * field + 0.35 * drift + edge) * COVER_GAIN).clamp(0.0, 1.0)
    };
    let cover = into(field);
    let drift = 0.5 + 0.5 * drift;
    let lift =
        noise2(x / 5_000.0, z / 5_000.0, deck.seed ^ 0xba5e) * resolved(spacing / 5_000.0, 1.0);
    let base = deck.base + deck.base_spread * lift;
    // Deeper the broader the cloud: heaps grow about as tall as they are
    // broad, and tallest at the middle of a field of cloud.
    let grown = smoothstep(FOOTPRINT, 1.0, into(broad));
    let depth = deck.depth.0 + (deck.depth.1 - deck.depth.0) * grown * (0.6 + 0.4 * drift);
    Column {
        cover: single(cover),
        base: single(base),
        top: single(base + depth),
    }
}

/// The value below which a normal variate falls `share` of the time, through
/// the logistic curve that stands within a percent of the normal's (Bowling
/// et al., "A logistic approximation to the cumulative normal distribution",
/// 2009): a sky wholly covered or clear held a little short of its limits.
fn normal_quantile(share: f64) -> f64 {
    let share = share.clamp(1e-3, 1.0 - 1e-3);
    mathf::ln(share / (1.0 - share)) / 1.702
}

/// A band of heights no cloud stands in.
pub(super) const EMPTY_BAND: (f32, f32) = (f32::INFINITY, f32::NEG_INFINITY);

/// The heights between which `deck`'s cloud over the weather cell whose
/// corners are `corners` can stand, its turrets reaching no further out than
/// `reach`: bilinear between the corners, its cover, base and top lie within
/// theirs, and the envelope rises with the cover. Rounded out to whole
/// metres, which single precision holds exactly; empty where no cloud can
/// stand.
pub(super) fn band_over(deck: &Deck, corners: [Column; 4], reach: f64) -> (f32, f32) {
    let values = |field: fn(&Column) -> f32| corners.map(|corner| f64::from(field(&corner)));
    let most = |values: [f64; 4]| values.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let least = |values: [f64; 4]| values.into_iter().fold(f64::INFINITY, f64::min);
    let (bases, tops) = (values(|corner| corner.base), values(|corner| corner.top));
    let depths = corners.map(|corner| f64::from(corner.top - corner.base).max(0.0));
    let cover = most(values(|corner| corner.cover));
    // Within a footprint the envelope rises with the depth; beyond it, it
    // falls the further the deeper the column.
    let depth = if cover > FOOTPRINT {
        most(depths)
    } else {
        least(depths)
    };
    let margin = reach + 0.5 * skin(deck, f64::INFINITY);
    let highest = envelope(deck, cover, depth) + margin;
    if highest <= 0.0 {
        return EMPTY_BAND;
    }
    // No column's envelope stands higher than its own top.
    let top = (most(bases) + highest).min(most(tops) + margin);
    (single(mathf::floor(least(bases))), single(mathf::ceil(top)))
}

/// A cloud's bubbles at texel `p` of a tiling texture: how high above the
/// level a union of spheres stands there, one swelling from each cell's own
/// point to its own radius, as thermals rising through a heap swell it, and
/// meeting in creases where they press together.
fn bubbles(p: Vec3, seed: u32) -> f64 {
    let at = p * (BUBBLE_CELLS / real(SHAPE_SIDE));
    let whole = |value: f64| mathf::floor(value);
    let (cx, cy, cz) = (whole(at.x), whole(at.y), whole(at.z));
    let wrap = |value: f64| {
        let tiled = value - mathf::floor(value / BUBBLE_CELLS) * BUBBLE_CELLS;
        mathf::round_i32(tiled).cast_unsigned()
    };
    let (least, most) = BUBBLE_RADII;
    let mut highest = 0.0f64;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y, z) = (cx + f64::from(dx), cy + f64::from(dy), cz + f64::from(dz));
                let key = hash3(wrap(x), wrap(y), wrap(z), seed);
                let middle = Vec3::new(
                    x + unit(key),
                    y + unit(mix32(key ^ 1)),
                    z + unit(mix32(key ^ 2)),
                );
                let radius = least + (most - least) * unit(mix32(key ^ 3));
                let offset = middle - at;
                let left = radius * radius - offset.dot(offset);
                if left > 0.0 {
                    highest = highest.max(mathf::sqrt(left));
                }
            }
        }
    }
    highest / most
}

/// The fine wisps that cut a cloud's edge, at texel `p` of a tiling texture.
fn wisps(p: Vec3, seed: u32) -> f64 {
    let period = real(DETAIL_SIDE);
    WISP_OCTAVES
        .iter()
        .map(|&(cells, weight, salt)| {
            weight * (1.0 - worley_tiled(p * (cells / period), cells, seed ^ salt))
        })
        .sum()
}

/// Worley's cellular noise that repeats every `period` cells: the distance
/// to the nearest feature point, about `0.0..1.0`.
pub(super) fn worley_tiled(p: Vec3, period: f64, seed: u32) -> f64 {
    let whole = |value: f64| mathf::floor(value);
    let (cx, cy, cz) = (whole(p.x), whole(p.y), whole(p.z));
    let wrap = |value: f64| {
        let tiled = value - mathf::floor(value / period) * period;
        mathf::round_i32(tiled).cast_unsigned()
    };
    let mut nearest = f64::INFINITY;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y, z) = (cx + f64::from(dx), cy + f64::from(dy), cz + f64::from(dz));
                let key = hash3(wrap(x), wrap(y), wrap(z), seed);
                let feature = Vec3::new(
                    x + unit(key),
                    y + unit(mix32(key ^ 1)),
                    z + unit(mix32(key ^ 2)),
                );
                let offset = feature - p;
                nearest = nearest.min(offset.dot(offset));
            }
        }
    }
    mathf::sqrt(nearest).min(1.0)
}

/// Fill layers `layers` of `tile` with `value` at each texel's centre.
fn fill_tile(
    tile: &mut Tile,
    layers: Range<usize>,
    runner: &dyn JobRunner,
    value: &(dyn Fn(Vec3) -> f64 + Sync),
) {
    let side = tile.side;
    let per_layer = side * side;
    let Some(texels) = tile
        .texels
        .get_mut(layers.start * per_layer..layers.end * per_layer)
    else {
        return;
    };
    band::for_each(runner, texels, (layers.start, per_layer), &|layer, band| {
        for (index, texel) in band.iter_mut().enumerate() {
            let p = Vec3::new(
                real(index % side) + 0.5,
                real(index / side) + 0.5,
                real(layer) + 0.5,
            );
            let level = mathf::round_i32(value(p).clamp(0.0, 1.0) * 255.0);
            *texel = u8::try_from(level).unwrap_or(u8::MAX);
        }
    });
}

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;
