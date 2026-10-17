//! What a deck's cloud is made of where it stands: its weather, the sheets it
//! lays over its columns, the envelope of a heap or a sheet, its turrets,
//! and the extinction its water or ice holds.
//!
//! A field deck's weather map says how far within a footprint each column
//! stands and where its base and top lie there; a heaped deck's heaps say it
//! for themselves (`heaps`). Over each footprint a cloud stands as an
//! envelope — a dome for a heap, a plateau for a sheet — on a flat base at
//! the level its air condenses, the envelope's surface pushed out into
//! turrets on turrets by billowing noise at six scales, each settling to its
//! mean where a footprint is too coarse to see it, and past them a fringe of
//! diluted droplets the wisps cut into puffs and filaments, a coarse read
//! averaging that edge across its footprint so it holds the water a sharp
//! read does. Water holds
//! extinction rising with height as adiabatic liquid water does, so a heap is
//! thinnest at its base and densest high in its body; ice holds it evenly.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_mix::{mix32, unit};
use tairix_noise::{cell, fbm2_resolved, hash3, noise2, resolved, smoothstep};
use tairix_parallel::JobRunner;
use tairix_util::fallible;
use tairix_util::mathf::{self, bilinear};

use super::{Deck, Matter};
use crate::vector::{cell_of, power, real, single, Vec3};

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
/// How far in from its footprint's edge a deck's envelope rises to its
/// whole depth: a sheet's a share of that depth in, a plateau soon after the
/// edge; a heap's some billows in however deep it stands, so a shallow heap
/// is a low dome and a deep one stands steep-walled. And how steeply any
/// envelope falls on below its base outside its footprint.
const SHEET_RUN: f64 = 0.08;
const HEAP_RUN: f64 = 4.0;
const FALL: f64 = 2.0;
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
/// Each scale's reach falls as the 0.85th power of its size, a little slower
/// than the size itself, so the finer turrets stand out as they do on a
/// real heap (whose surface measures some 2.35 dimensions, Malinowski and
/// Zawadzki 1993) while each stays near as broad as it is tall.
const BUBBLES: [(f64, Vec3, (f64, f64)); 6] = [
    (1.0, Vec3::new(0.0, 0.0, 0.0), (0.45, 1.4)),
    (2.31, Vec3::new(17.3, 5.1, 29.7), (0.22, 0.69)),
    (5.27, Vec3::new(3.7, 29.3, 11.9), (0.11, 0.34)),
    (11.9, Vec3::new(23.1, 13.7, 7.3), (0.055, 0.17)),
    (27.1, Vec3::new(9.1, 21.7, 15.3), (0.027, 0.084)),
    (61.8, Vec3::new(27.9, 2.3, 19.1), (0.013, 0.042)),
];
/// How far a cloud's fringe of diluted droplets reaches out past its
/// turrets, as a share of a billow: least at a heap's top, which its
/// thermals push up into the dry air, and most low down, where it mixes
/// away into it; and up how much of its depth the fringe turns from the one
/// to the other.
const FRINGE: (f64, f64) = (0.05, 0.12);
const BILLOWING: (f64, f64) = (0.1, 0.45);
/// The wisps' values over which a fringe goes from kept to cut away as far
/// as it is cut at all: low down their cells' middles cut out, their walls
/// left as filaments, and high up the walls cut out, their middles left as
/// puffs. A cut never reaches the fringe's inner edge, so the cloud thickens
/// into its body without a step.
const CUTTING: (f64, f64) = (0.45, 0.7);
const DEEPEST_CUT: f64 = 0.85;
/// The fringe's mean, as a footprint too coarse for its wisps reads it, is
/// tabulated over how far into it a place lies and how billowing it is
/// there, from the wisps' own values counted into bins.
const FRINGE_DEPTHS: usize = 17;
const FRINGE_BILLOWS: usize = 9;
const WISP_BINS: usize = 256;
/// The depths into the fringe its mean is summed to, for a blurred read to
/// average what a sharp one holds.
const FRINGE_SUMS: usize = 33;
/// The most texels, or blocks of them, a box's most is read over along each
/// side; past it a coarser block of a texture's peaks is read.
const MOST_BLOCKS: i64 = 4;
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
pub(super) const LEAST_RISE: f64 = 1.0;

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

    /// The texture's most over blocks two texels across, then four, and on up
    /// to the whole tile; `None` when the heap will not hold them.
    fn peaks(&self) -> Option<Vec<Tile>> {
        let mut peaks: Vec<Tile> = Vec::new();
        let mut side = self.side;
        while side > 1 {
            let finer = peaks.last().unwrap_or(self);
            side /= 2;
            let mut coarser = Tile::new(side)?;
            for (index, slot) in coarser.texels.iter_mut().enumerate() {
                let (x, y, z) = (index % side, index / side % side, index / (side * side));
                let mut most = 0u8;
                for corner in 0..8 {
                    let (dx, dy, dz) = (corner & 1, (corner >> 1) & 1, corner >> 2);
                    let at = ((2 * z + dz) * finer.side + 2 * y + dy) * finer.side + 2 * x + dx;
                    most = most.max(finer.texels.get(at).copied().unwrap_or(u8::MAX));
                }
                *slot = most;
            }
            peaks.try_reserve(1).ok()?;
            peaks.push(coarser);
        }
        Some(peaks)
    }

    /// The most the texture, read trilinearly as [`Tile::at`] reads it, holds
    /// anywhere over the box `low..high` of its texels, wrapping: over the
    /// texels a read in the box touches, read from the finest of itself and
    /// `peaks` that holds them in no more than [`MOST_BLOCKS`] along each
    /// side, else `peak`.
    fn most_over(&self, (peaks, peak): (&[Tile], f64), (low, high): (Vec3, Vec3)) -> f64 {
        // A read touches the texel it lies in and the next along each axis.
        let first = |value: f64| i64::from(cell(value).0.cast_signed());
        let spans = [(low.x, high.x), (low.y, high.y), (low.z, high.z)]
            .map(|(low, high)| (first(low), first(high) + 1));
        let widest = spans
            .iter()
            .map(|(from, to)| to - from + 1)
            .max()
            .unwrap_or(0);
        let mut breadth = 1i64;
        let mut grid = self;
        for coarser in peaks {
            if widest <= MOST_BLOCKS * breadth {
                break;
            }
            breadth *= 2;
            grid = coarser;
        }
        if widest > MOST_BLOCKS * breadth || grid.side == 0 {
            return peak;
        }
        let mask = i64::try_from(grid.side - 1).unwrap_or(0);
        let blocks = |(from, to): (i64, i64)| (from.div_euclid(breadth), to.div_euclid(breadth));
        let ((x0, x1), (y0, y1), (z0, z1)) = (blocks(spans[0]), blocks(spans[1]), blocks(spans[2]));
        let at = |block: i64| usize::try_from(block & mask).unwrap_or(0);
        let mut most = 0u8;
        for z in z0..=z1 {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let index = (at(z) * grid.side + at(y)) * grid.side + at(x);
                    most = most.max(grid.texels.get(index).copied().unwrap_or(u8::MAX));
                }
            }
        }
        f64::from(most) / 255.0
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
/// once built: the billows' mean, peak and peaks over ever broader blocks,
/// and the wisps' mean.
#[derive(Clone, Debug)]
pub(super) struct Textures {
    pub(super) shape: Tile,
    pub(super) detail: Tile,
    shape_mean: f64,
    shape_peak: f64,
    /// The billows' most over blocks two, four and on up to the whole tile
    /// across; none where the heap would not hold them, the peak bounding
    /// every block alone.
    shape_peaks: Vec<Tile>,
    pub(super) wisp_mean: f64,
    fringe_mean: [f64; FRINGE_DEPTHS * FRINGE_BILLOWS],
    /// The fringe's mean summed from its outer reach to each of the
    /// [`FRINGE_SUMS`] depths into it, in shares of its span, and the mean
    /// there; by billowing.
    fringe_sum: [(f64, f64); FRINGE_SUMS * FRINGE_BILLOWS],
}

impl Textures {
    pub(super) fn new() -> Option<Self> {
        Some(Self {
            shape: Tile::new(SHAPE_SIDE)?,
            detail: Tile::new(DETAIL_SIDE)?,
            shape_mean: 0.5,
            shape_peak: 1.0,
            shape_peaks: Vec::new(),
            wisp_mean: 0.5,
            fringe_mean: [0.0; FRINGE_DEPTHS * FRINGE_BILLOWS],
            fringe_sum: [(0.0, 0.0); FRINGE_SUMS * FRINGE_BILLOWS],
        })
    }

    /// Layers `layers` of the billows, drawn from `seed`; their mean and
    /// peak once the last is.
    pub(super) fn fill_shape(&mut self, layers: Range<usize>, runner: &dyn JobRunner, seed: u32) {
        let last = layers.end >= self.shape.side;
        fill_tile(&mut self.shape, layers, runner, &|p| bubbles(p, seed));
        if last {
            (self.shape_mean, self.shape_peak) = (self.shape.mean(), self.shape.peak());
            self.shape_peaks = self.shape.peaks().unwrap_or_default();
        }
    }

    /// Layers `layers` of the wisps, drawn from `seed`; their mean, and the
    /// fringe's they cut, once the last is.
    pub(super) fn fill_detail(&mut self, layers: Range<usize>, runner: &dyn JobRunner, seed: u32) {
        let last = layers.end >= self.detail.side;
        fill_tile(&mut self.detail, layers, runner, &|p| wisps(p, seed));
        if last {
            self.wisp_mean = self.detail.mean();
            let counts = self.wisp_counts();
            self.fringe_mean = fringe_means(&counts);
            self.fringe_sum = fringe_sums(&counts);
        }
    }

    /// The wisps as every place reads them wholly resolved, counted into
    /// bins of their value.
    fn wisp_counts(&self) -> [u32; WISP_BINS] {
        let side = self.detail.side;
        let mut counts = [0u32; WISP_BINS];
        for index in 0..side * side * side {
            let texel = Vec3::new(
                real(index % side) + 0.5,
                real(index / side % side) + 0.5,
                real(index / (side * side)) + 0.5,
            );
            let wisp = self.wisp_at(texel, [1.0; FINER.len()]);
            let bin = cell_of(wisp * real(WISP_BINS)).0.min(WISP_BINS - 1);
            if let Some(count) = counts.get_mut(bin) {
                *count += 1;
            }
        }
        counts
    }

    /// The fringe's mean `into` of the way into it where it is `billowing`.
    fn fringe_mean_at(&self, into: f64, billowing: f64) -> f64 {
        let (row, across) = (
            into.clamp(0.0, 1.0) * real(FRINGE_DEPTHS - 1),
            billowing.clamp(0.0, 1.0) * real(FRINGE_BILLOWS - 1),
        );
        let ((i, u), (j, v)) = (cell_of(row), cell_of(across));
        let at = |i: usize, j: usize| {
            self.fringe_mean
                .get(j.min(FRINGE_BILLOWS - 1) * FRINGE_DEPTHS + i.min(FRINGE_DEPTHS - 1))
                .copied()
                .unwrap_or(0.0)
        };
        bilinear(
            [at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)],
            (u, v),
        )
    }

    /// The fringe's mean summed from its outer reach to `into` of the way
    /// through it where it is `billowing`, in shares of its span, the whole
    /// body counted on past it: between the tabulated depths the cubic whose
    /// rate is the mean at each.
    fn fringe_sum_at(&self, into: f64, billowing: f64) -> f64 {
        if into <= 0.0 {
            return 0.0;
        }
        let beyond = (into - 1.0).max(0.0);
        let step = 1.0 / real(FRINGE_SUMS - 1);
        let (i, u) = cell_of(into.min(1.0) / step);
        let (i, u) = if i >= FRINGE_SUMS - 1 {
            (FRINGE_SUMS - 2, 1.0)
        } else {
            (i, u)
        };
        let (j, v) = cell_of(billowing.clamp(0.0, 1.0) * real(FRINGE_BILLOWS - 1));
        let along = |j: usize| {
            let at = |i: usize| {
                self.fringe_sum
                    .get(j.min(FRINGE_BILLOWS - 1) * FRINGE_SUMS + i)
                    .copied()
                    .unwrap_or_default()
            };
            let ((from, rate), (to, next)) = (at(i), at(i + 1));
            let (squared, cubed) = (u * u, u * u * u);
            from * (2.0 * cubed - 3.0 * squared + 1.0)
                + rate * step * (cubed - 2.0 * squared + u)
                + to * (3.0 * squared - 2.0 * cubed)
                + next * step * (cubed - squared)
        };
        let (here, next) = (along(j), along(j + 1));
        here + (next - here) * v + beyond
    }

    /// How dense a cloud stands `surface` metres within its turrets'
    /// surface across `edge`, its fringe `billowing` there: what the wisps
    /// leave of it on the whole, averaged over the edge's blur so a blurred
    /// read holds what a sharp one does, and as far as `turrets` resolves
    /// them, the wisps' own cut, thinned so it takes no more than that
    /// whole.
    fn fringed(&self, surface: f64, edge: Edge, billowing: f64, turrets: &Turrets) -> f64 {
        let into = edge.density(surface);
        let Some((wisp, kept)) = turrets.wisps.filter(|_| into > 0.0 && into < 1.0) else {
            return into;
        };
        // Interpolation never lifts the mean past the uncut edge.
        let mean = edge
            .average(surface, |at| self.fringe_sum_at(at, billowing))
            .clamp(0.0, into);
        if kept <= 0.0 {
            return mean;
        }
        let typical = self.fringe_mean_at(into, billowing);
        let thinning = if into > typical {
            ((into - mean) / (into - typical)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let cut = into - thinning * (into - eroded(into, wisp, billowing));
        mean + (cut - mean) * kept.min(1.0)
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
    }

    /// The most the turrets of `deck` push its envelope out anywhere over the
    /// box `low..high` of its billows' own coordinates, read through a
    /// footprint `footprint` metres across: each scale by the most its
    /// bubbles stand there, as far as such a read resolves it.
    pub(super) fn reach_over(&self, deck: &Deck, (low, high): (Vec3, Vec3), footprint: f64) -> f64 {
        let period = deck.billow * 8.0;
        let per = real(SHAPE_SIDE) / period;
        BUBBLES
            .iter()
            .zip(bubble_reach(deck))
            .enumerate()
            .map(|(index, (&(times, start, _), reach))| {
                let weight = resolved_scale(index, footprint, period) * reach;
                if weight <= 0.0 {
                    return 0.0;
                }
                let most = self.shape.most_over(
                    (&self.shape_peaks, self.shape_peak),
                    (low * (per * times) + start, high * (per * times) + start),
                );
                weight * (most - self.shape_mean)
            })
            .sum()
    }

    /// Where a place `inside` metres within `deck`'s envelope stands against
    /// its turrets at `grained`, its billows' own coordinates, across `edge`:
    /// each scale finer than
    /// `footprint` metres settled to its mean, or, `bounding`, pushed out as
    /// far as any finer scale could past the first. Read no further once the
    /// scales still to come could neither bring the surface out to the place
    /// nor take it back past it.
    fn surface(
        &self,
        deck: &Deck,
        grained: Vec3,
        (footprint, bounding): (f64, bool),
        (inside, edge): (f64, Edge),
    ) -> Surface {
        let reach = bubble_reach(deck);
        let period = deck.billow * 8.0;
        let texel = grained * (real(SHAPE_SIDE) / period);
        let (rise, fall) = (self.shape_peak - self.shape_mean, self.shape_mean);
        let kept: [f64; BUBBLES.len()] = core::array::from_fn(|index| {
            if bounding {
                1.0
            } else {
                resolved_scale(index, footprint, period)
            }
        });
        let mut ahead: f64 = reach
            .iter()
            .zip(kept)
            .map(|(reach, kept)| reach * kept)
            .sum();
        let mut out = 0.0;
        for (index, ((&(times, start, _), reach), kept)) in
            BUBBLES.iter().zip(reach).zip(kept).enumerate()
        {
            if bounding && index > 0 {
                out += ahead.max(0.0) * rise;
                ahead = 0.0;
            } else {
                let weight = kept * reach;
                ahead = (ahead - weight).max(0.0);
                if weight > 0.0 {
                    out += weight * (self.shape.at(texel * times + start) - self.shape_mean);
                }
            }
            if inside + out + ahead * rise + edge.outer() <= 0.0 {
                return Surface::Clear;
            }
            let least = inside + out - ahead * fall - edge.inner();
            if least >= 0.0 {
                return Surface::Solid(least);
            }
            if bounding && index > 0 {
                break;
            }
        }
        if bounding {
            return Surface::Edge(Turrets { out, wisps: None });
        }
        let span = period * DETAIL_SCALE;
        let finest = span / WISP_FINEST;
        let kept = resolved(footprint, finest);
        let wisp = if kept > 0.0 {
            let finer = FINER.map(|(times, ..)| resolved(footprint, finest / times));
            self.wisp_at(grained * (real(DETAIL_SIDE) / span), finer)
        } else {
            self.wisp_mean
        };
        Surface::Edge(Turrets {
            out,
            wisps: Some((wisp, kept)),
        })
    }
}

/// Where a place stands against a cloud's surface: clear of it, however its
/// finer scales fall; within its body, at least so many metres past its
/// skin, however they fall; or at its edge, as its turrets have it.
enum Surface {
    Clear,
    Solid(f64),
    Edge(Turrets),
}

/// What a cloud's turrets do at a place: how far they push its envelope out,
/// in metres; and the wisps cutting its fringe there, `0.0..=1.0`, with how
/// far the read resolves them, or none for a bounding read, which cuts
/// nothing from it.
struct Turrets {
    out: f64,
    wisps: Option<(f64, f64)>,
}

/// What is left of a cloud's fringe `into` of the way into it, `0.0..=1.0`,
/// where its wisps stand at `wisp` and it is `billowing`: thinning outward,
/// with the wisps' cells cut out of it where it mixes away, leaving
/// filaments, and the walls between them where it billows, leaving puffs.
fn eroded(into: f64, wisp: f64, billowing: f64) -> f64 {
    thinned(into, cut(wisp, billowing))
}

/// How much of a fringe's depth the wisps standing at `wisp` cut away where
/// it is `billowing`.
fn cut(wisp: f64, billowing: f64) -> f64 {
    let wisp = wisp.clamp(0.0, 1.0);
    DEEPEST_CUT
        * smoothstep(
            CUTTING.0,
            CUTTING.1,
            wisp + (1.0 - 2.0 * wisp) * billowing.clamp(0.0, 1.0),
        )
}

/// What a cut of `cut` leaves of a fringe `into` of the way into it.
fn thinned(into: f64, cut: f64) -> f64 {
    ((into - cut) / (1.0 - cut)).clamp(0.0, 1.0)
}

/// The wisps' wholly resolved values, counted as `counts`, summed through
/// `per_bin` of each bin, as a share of them all.
fn over_wisps(counts: &[u32; WISP_BINS], per_bin: impl Fn(usize) -> f64) -> f64 {
    let total = f64::from(counts.iter().sum::<u32>().max(1));
    counts
        .iter()
        .enumerate()
        .map(|(bin, &count)| f64::from(count) * per_bin(bin))
        .sum::<f64>()
        / total
}

/// The wisps' value in the middle of bin `bin`.
fn wisp_of(bin: usize) -> f64 {
    (real(bin) + 0.5) / real(WISP_BINS)
}

/// The fringe's mean over the wisps counted as `counts`, at each of the
/// tabulated depths into it and billowings.
fn fringe_means(counts: &[u32; WISP_BINS]) -> [f64; FRINGE_DEPTHS * FRINGE_BILLOWS] {
    core::array::from_fn(|index| {
        let into = real(index % FRINGE_DEPTHS) / real(FRINGE_DEPTHS - 1);
        let billowing = real(index / FRINGE_DEPTHS) / real(FRINGE_BILLOWS - 1);
        over_wisps(counts, |bin| eroded(into, wisp_of(bin), billowing))
    })
}

/// The fringe's mean over the wisps counted as `counts`, summed from its
/// outer reach to each of the [`FRINGE_SUMS`] depths into it in shares of
/// its span, and the mean there, at each tabulated billowing: each wisp's
/// cut summed exactly from where the fringe first thickens past it.
fn fringe_sums(counts: &[u32; WISP_BINS]) -> [(f64, f64); FRINGE_SUMS * FRINGE_BILLOWS] {
    let mut sums = [(0.0, 0.0); FRINGE_SUMS * FRINGE_BILLOWS];
    for (row, depths) in sums.as_chunks_mut::<FRINGE_SUMS>().0.iter_mut().enumerate() {
        let billowing = real(row) / real(FRINGE_BILLOWS - 1);
        // Each bin's cut, where the fringe first thickens past it, and what
        // the fringe sums to there.
        let cuts: [(f64, f64, f64); WISP_BINS] = core::array::from_fn(|bin| {
            let cut = cut(wisp_of(bin), billowing);
            let from = undiluted(cut);
            (cut, from, diluted_sum(from))
        });
        for (depth, slot) in depths.iter_mut().enumerate() {
            let into = real(depth) / real(FRINGE_SUMS - 1);
            let (summed, share) = (diluted_sum(into), diluted(into));
            let sum = over_wisps(counts, |bin| match cuts.get(bin) {
                Some(&(cut, from, before)) if into > from => {
                    (summed - before - cut * (into - from)) / (1.0 - cut)
                }
                _ => 0.0,
            });
            let mean = over_wisps(counts, |bin| {
                cuts.get(bin).map_or(0.0, |&(cut, ..)| thinned(share, cut))
            });
            *slot = (sum, mean);
        }
    }
    sums
}

/// How far a read through a footprint `footprint` metres across resolves
/// scale `index` of bubbles whose texture repeats every `period` metres: the
/// first always, and each finer one as far as the footprint sees it, a
/// scale's bubbles some half a cell across.
fn resolved_scale(index: usize, footprint: f64, period: f64) -> f64 {
    let times = BUBBLES.get(index).map_or(1.0, |&(times, ..)| times);
    if index == 0 {
        1.0
    } else {
        resolved(footprint, 0.5 * period / (BUBBLE_CELLS * times))
    }
}

/// How far `deck`'s bubbles reach at each scale, in metres: a heap's the
/// most, a sheet's the least.
fn bubble_reach(deck: &Deck) -> [f64; BUBBLES.len()] {
    let heap = deck.heap.clamp(0.0, 1.0);
    BUBBLES.map(|(_, _, (sheet, heaped))| deck.billow * (sheet + (heaped - sheet) * heap))
}

/// One column of a deck's weather map: how far within its footprint it
/// stands, in metres, negative outside it, found from the covers about it
/// once its level's weather is laid; and where its cloud's base and top lie.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct Column {
    pub(super) edge: f32,
    pub(super) base: f32,
    pub(super) top: f32,
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

    /// How a place `altitude` above the scene's level over the point stands
    /// against `deck`'s envelope there.
    pub(super) fn placed(&self, deck: &Deck, altitude: f64) -> Option<Placed> {
        let base = self.at(|corner| corner.base);
        let depth = (self.at(|corner| corner.top) - base).max(0.0);
        Placed::of(
            self.at(|corner| corner.edge),
            altitude - base,
            shape(deck, depth),
        )
    }
}

/// How a place stands against the envelope of a deck's cloud over it: how
/// far above its base, how deep the envelope stands, and how far within it
/// the place lies, in metres, negative outside.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Placed {
    pub(super) rise: f64,
    pub(super) depth: f64,
    pub(super) inside: f64,
}

impl Placed {
    /// A place `rise` above the base of an envelope shaped `shape`, `edge`
    /// metres within its footprint; `None` at or below its base.
    pub(super) fn of(edge: f64, rise: f64, shape: (f64, f64)) -> Option<Self> {
        (rise > 0.0).then(|| Self {
            rise,
            depth: shape.0,
            inside: within_envelope(edge, rise, shape),
        })
    }
}

/// The shape of `deck`'s envelope over a column `depth` deep: that depth,
/// and the run in from the footprint's edge over which it rises to it.
pub(super) fn shape(deck: &Deck, depth: f64) -> (f64, f64) {
    let heap = deck.heap.clamp(0.0, 1.0);
    let depth = depth.max(LEAST_RISE);
    let (sheet, heaped) = (SHEET_RUN * depth, HEAP_RUN * deck.billow);
    (depth, (sheet + (heaped - sheet) * heap).max(LEAST_RISE))
}

/// How high above its base an envelope shaped `(depth, run)` stands `edge`
/// metres within its footprint: a dome rising from the edge to its whole
/// depth `run` in, and on below the base outside at [`FALL`].
fn envelope(edge: f64, (depth, run): (f64, f64)) -> f64 {
    if edge <= 0.0 {
        return FALL * edge;
    }
    let into = (edge / run).min(1.0);
    depth * into * (2.0 - into)
}

/// How far in from its footprint's edge an envelope shaped `(depth, run)`
/// stands `rise` high, if it ever does.
fn reached(rise: f64, (depth, run): (f64, f64)) -> Option<f64> {
    (rise < depth).then(|| run * (1.0 - mathf::sqrt(1.0 - rise / depth)))
}

/// How far within an envelope shaped `shape` a place `edge` metres within
/// its footprint and `rise` above its base lies, in metres, negative
/// outside: the distance to the chord from the envelope straight above or
/// below it to the envelope level with it. On the dome's convex face that
/// is never nearer than the envelope itself, so no turret stands further
/// from the envelope than it reaches, and a wall's turrets stand out from
/// the wall as a top's rise from the top.
fn within_envelope(edge: f64, rise: f64, shape: (f64, f64)) -> f64 {
    let vertical = envelope(edge, shape) - rise;
    let Some(level) = reached(rise, shape) else {
        return vertical;
    };
    let (up, across) = (vertical.abs(), (edge - level).abs());
    let chord = up * across / mathf::sqrt(up * up + across * across).max(f64::MIN_POSITIVE);
    chord.copysign(vertical)
}

/// What a deck's cloud holds where it stands: its extinction per metre, and
/// how far within its surface the place lies, past the edge its droplets
/// fray at, in metres.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Held {
    pub(super) extinction: f64,
    pub(super) within: f64,
}

/// What `deck`'s cloud holds at a place standing against its envelope as
/// `placed` has it, read at `grained` in its billows' coordinates with its
/// edges as `edges` has them; `None` where none stands.
pub(super) fn cloud(
    deck: &Deck,
    textures: &Textures,
    placed: Placed,
    grained: Vec3,
    edges: Edges,
) -> Option<Held> {
    let Placed {
        rise,
        depth,
        inside,
    } = placed;
    let footprint = edges_footprint(deck, edges);
    let billowing = smoothstep(BILLOWING.0, BILLOWING.1, rise / depth);
    let edge = Edge::of(deck, billowing, footprint);
    if inside + textures.reach(deck) + edge.outer() <= 0.0 {
        return None;
    }
    let bounding = matches!(edges, Edges::Bounding(_));
    match textures.surface(deck, grained, (footprint, bounding), (inside, edge)) {
        Surface::Clear => None,
        Surface::Solid(within) => Some(Held {
            extinction: extinction(deck, rise),
            within,
        }),
        Surface::Edge(turrets) => {
            let surface = inside + turrets.out;
            let density = textures.fringed(surface, edge, billowing, &turrets);
            (density > 0.0).then(|| Held {
                extinction: density * extinction(deck, rise),
                within: surface - edge.inner(),
            })
        }
    }
}

/// Whether `deck`'s cloud, read through a `footprint` metres across, can
/// stand anywhere over columns standing no further within their footprint
/// than `edge`, under envelopes no deeper or steeper than `shape`, no nearer
/// their base than `rise`, its turrets pushing out no further than `reach`:
/// as a place lies no further within its envelope for a column less far
/// within its footprint, a shallower or gentler envelope or a place higher
/// above its base, the envelope at those holds every place among them, its
/// turrets and fringe reaching out at their furthest.
pub(super) fn may_hold(
    deck: &Deck,
    (edge, shape, rise): (f64, (f64, f64), f64),
    (reach, footprint): (f64, f64),
) -> bool {
    let inside = within_envelope(edge, rise.max(0.0), shape);
    inside + reach + Edge::widest(deck, footprint).outer() > 0.0
}

/// How far outside its envelope a read of `deck`'s cloud taken as `edges` has
/// them can meet any of it: as far as its turrets reach, and the fringe at
/// its broadest through the read's footprint past them.
pub(super) fn read_margin(deck: &Deck, textures: &Textures, edges: Edges) -> f64 {
    textures.reach(deck) + Edge::widest(deck, edges_footprint(deck, edges)).outer()
}

/// How far past its envelope `deck`'s cloud can stand, its turrets reaching
/// `reach`: as far as they reach, and the fringe at its broadest past them.
pub(super) fn standing_margin(deck: &Deck, reach: f64) -> f64 {
    reach + Edge::widest(deck, f64::INFINITY).outer()
}

/// The least share of its way out past its footprint a place outside lies
/// from the envelope: below the base the envelope falls at [`FALL`], so the
/// chord to it is never shorter than that fall's share of the way out, and
/// no place outside lies further within than that share of its way out.
pub(super) fn outside_share() -> f64 {
    FALL / mathf::hypot(1.0, FALL)
}

/// A cloud's edge as a read through a footprint sees it: its fringe of
/// diluted droplets reaching `fringe` metres out past its turrets and its
/// [`SKIN`], the whole averaged over `blur` metres about each place, so a
/// coarse read holds the water a fine one does.
#[derive(Copy, Clone, Debug)]
pub(super) struct Edge {
    fringe: f64,
    blur: f64,
}

impl Edge {
    /// `deck`'s edge where it is `billowing`, read through a `footprint`
    /// metres across: averaged over no more than a billow, so its bands hold
    /// every read.
    fn of(deck: &Deck, billowing: f64, footprint: f64) -> Self {
        Self {
            fringe: deck.billow * (FRINGE.1 + (FRINGE.0 - FRINGE.1) * billowing.clamp(0.0, 1.0)),
            blur: footprint.clamp(0.0, deck.billow),
        }
    }

    /// The edge reaching furthest out, where the fringe is broadest.
    pub(super) fn widest(deck: &Deck, footprint: f64) -> Self {
        Self::of(deck, 0.0, footprint)
    }

    /// How far out past the turrets any of the edge reaches, in metres.
    pub(super) fn outer(self) -> f64 {
        self.fringe + 0.5 * self.blur
    }

    /// How far in past the turrets the cloud stands whole, in metres.
    fn inner(self) -> f64 {
        f64::midpoint(SKIN, self.blur)
    }

    /// The share of the body's density standing `surface` metres within the
    /// turrets: diluted outward from the skin's middle across the fringe as
    /// the cube of how far into it a place lies, so its outer half holds a
    /// few hundredths of the body's water and a grazing look through it
    /// shows the sky; averaged over the blur.
    fn density(self, surface: f64) -> f64 {
        self.average(surface, diluted_sum).clamp(0.0, 1.0)
    }

    /// A share of the body's density `summed` across the edge, from the
    /// fringe's outer reach to each share of the way to the skin's middle,
    /// averaged over the blur about `surface` metres within the turrets.
    fn average(self, surface: f64, summed: impl Fn(f64) -> f64) -> f64 {
        let span = self.fringe + 0.5 * SKIN;
        let half = 0.5 * self.blur.max(span * SHARPEST_BLUR);
        let into = |at: f64| (at + self.fringe) / span;
        span * (summed(into(surface + half)) - summed(into(surface - half))) / (2.0 * half)
    }
}

/// The least an edge is averaged over, as a share of its span: far sharper
/// than a read can tell, and wide enough that the difference
/// [`Edge::average`] takes does not round away.
const SHARPEST_BLUR: f64 = 1e-3;

/// The share of the body's density a fringe holds `into` of the way through
/// it, from its outer reach to the skin's middle.
fn diluted(into: f64) -> f64 {
    let ramp = mathf::smoothstep(into);
    ramp * ramp * ramp
}

/// [`diluted`] summed from the fringe's outer reach to `into` of the way
/// through it, the whole body counted on past it.
fn diluted_sum(into: f64) -> f64 {
    if into <= 0.0 {
        return 0.0;
    }
    if into >= 1.0 {
        return DILUTED_WHOLE + (into - 1.0);
    }
    // (3t² − 2t³)³ = 27t⁶ − 54t⁷ + 36t⁸ − 8t⁹, summed term by term.
    let squared = into * into;
    let seventh = squared * squared * squared * into;
    seventh * (27.0 / 7.0 + into * (-27.0 / 4.0 + into * (4.0 - 0.8 * into)))
}

/// What [`diluted`] sums to across the whole fringe.
const DILUTED_WHOLE: f64 = 43.0 / 140.0;

/// How far into a fringe [`diluted`] reaches `share` of the body's density:
/// the inverse of its smoothstep, through its cube root.
fn undiluted(share: f64) -> f64 {
    let ramp = power(share.clamp(0.0, 1.0), 1.0 / 3.0);
    0.5 - mathf::sin(mathf::asin(1.0 - 2.0 * ramp) / 3.0)
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

/// A deck's weather at `(x, z)`, drawn only as finely as columns `spacing`
/// apart hold: how much of it its footprints cover, and its column, whose
/// edge the covers about it give once they are laid.
pub(super) fn weather(deck: &Deck, (x, z): (f64, f64), spacing: f64) -> (f32, Column) {
    // Its features across the wind are its narrowest.
    let footprint = spacing / deck.scale;
    let (u, v) = drifting(deck, (x, z));
    let field = fbm2_resolved(u, v, deck.seed, (5, 0.55, 2.02), footprint);
    // The same field's broadest octaves alone: high where a cloud is broad,
    // low over a blob the finer octaves alone lift past its edge.
    let broad = fbm2_resolved(u, v, deck.seed, (2, 0.55, 2.02), footprint);
    let drift = drift(deck, (u, v), footprint);
    // How far into a cloud's footprint a column lies: its edge where the
    // drifting field passes the quantile that leaves the deck's cover of the
    // sky covered, its middle as far again beyond.
    let edge = COVER_SPREAD * normal_quantile(deck.cover);
    let into = |field: f64| {
        (FOOTPRINT + (0.55 * field + 0.35 * drift + edge) * COVER_GAIN).clamp(0.0, 1.0)
    };
    let cover = into(field);
    let drift = 0.5 + 0.5 * drift;
    let base = base_at(deck, (x, z), spacing);
    // Deeper the broader the field of cloud a heap stands in.
    let grown = smoothstep(FOOTPRINT, 1.0, into(broad));
    let depth = deck.depth.0 + (deck.depth.1 - deck.depth.0) * grown * (0.6 + 0.4 * drift);
    (
        single(cover),
        Column {
            edge: 0.0,
            base: single(base),
            top: single(base + depth),
        },
    )
}

/// Where `deck`'s weather lies at `(x, z)` in its features' own units, along
/// the wind and across it, pushed about by a slower field so it gathers in
/// drifts.
pub(super) fn drifting(deck: &Deck, (x, z): (f64, f64)) -> (f64, f64) {
    let (cos, sin) = (mathf::cos(deck.heading), mathf::sin(deck.heading));
    let along = (x * cos + z * sin) / (deck.scale * deck.stretch);
    let across = (-x * sin + z * cos) / deck.scale;
    (
        along + 0.5 * noise2(along * 0.3, across * 0.3, deck.seed ^ 0x51),
        across + 0.5 * noise2(along * 0.3 + 3.1, across * 0.3 + 1.7, deck.seed ^ 0x93),
    )
}

/// The slow field `deck`'s weather gathers in drifts by, roughly
/// `-1.0..=1.0`, at `(u, v)` in its features' units, as finely as a
/// footprint `footprint` of those units resolves it.
pub(super) fn drift(deck: &Deck, (u, v): (f64, f64), footprint: f64) -> f64 {
    fbm2_resolved(
        u * 0.23,
        v * 0.23,
        deck.seed ^ 0x77,
        (3, 0.5, 2.0),
        footprint * 0.23,
    )
}

/// The height of `deck`'s clouds' base over `(x, z)`, drawn only as finely
/// as columns `spacing` apart hold.
pub(super) fn base_at(deck: &Deck, (x, z): (f64, f64), spacing: f64) -> f64 {
    let lift =
        noise2(x / 5_000.0, z / 5_000.0, deck.seed ^ 0xba5e) * resolved(spacing / 5_000.0, 1.0);
    deck.base + deck.base_spread * lift
}

/// The value below which a normal variate falls `share` of the time, through
/// the logistic curve that stands within a percent of the normal's (Bowling
/// et al., "A logistic approximation to the cumulative normal distribution",
/// 2009): a sky wholly covered or clear held a little short of its limits.
fn normal_quantile(share: f64) -> f64 {
    let share = share.clamp(1e-3, 1.0 - 1e-3);
    mathf::ln(share / (1.0 - share)) / 1.702
}

/// Where between a vertex covered `here` and the next along a line, covered
/// `next`, the cover crosses a footprint's edge, as a share of the way;
/// `None` where it does not.
pub(super) fn crossed(here: f32, next: f32) -> Option<f64> {
    let (here, next) = (f64::from(here), f64::from(next));
    ((here > FOOTPRINT) != (next > FOOTPRINT))
        .then(|| ((FOOTPRINT - here) / (next - here)).clamp(0.0, 1.0))
}

/// Into `squares`, for each of a line's vertices `spacing` apart, covered as
/// `cover` has them, the squared distance along the line to the nearest
/// place its cover crosses a footprint's edge; infinite on a line it never
/// crosses.
pub(super) fn nearest_crossings(
    cover: impl Fn(usize) -> Option<f32>,
    spacing: f64,
    squares: &mut [f32],
) {
    let crossing = |at: usize| {
        let (here, next) = (cover(at)?, cover(at + 1)?);
        crossed(here, next).map(|share| real(at) + share)
    };
    let mut behind = f64::NEG_INFINITY;
    for (at, square) in squares.iter_mut().enumerate() {
        if let Some(place) = at.checked_sub(1).and_then(crossing) {
            behind = place;
        }
        let off = (real(at) - behind) * spacing;
        *square = single(off * off);
    }
    let mut ahead = f64::INFINITY;
    for (at, square) in squares.iter_mut().enumerate().rev() {
        if let Some(place) = crossing(at) {
            ahead = place;
        }
        let off = (ahead - real(at)) * spacing;
        *square = square.min(single(off * off));
    }
}

/// Room for the lower envelope of one line's parabolas: the seeds they rise
/// from, their vertices, and where along the line each starts to be the
/// lowest.
pub(super) struct Envelope {
    seeds: Vec<f64>,
    vertices: Vec<usize>,
    starts: Vec<f64>,
}

impl Envelope {
    /// Room for a line `length` long; `None` when the heap will not hold it.
    pub(super) fn new(length: usize) -> Option<Self> {
        Some(Self {
            seeds: fallible::filled(length, f64::INFINITY)?,
            vertices: fallible::filled(length, 0)?,
            starts: fallible::filled(length, 0.0)?,
        })
    }

    /// Hand `found`, for each place along a line of places `spacing` apart,
    /// the least over the line's places of the squared distance to one and
    /// that one's own `seed`, an infinite seed holding none: the lower
    /// envelope of the parabolas rising from the seeds (Felzenszwalb and
    /// Huttenlocher, "Distance Transforms of Sampled Functions", 2012), in
    /// time linear in the line's length.
    pub(super) fn sweep(
        &mut self,
        seed: impl Fn(usize) -> f64,
        spacing: f64,
        mut found: impl FnMut(usize, f64),
    ) {
        for (at, slot) in self.seeds.iter_mut().enumerate() {
            *slot = seed(at);
        }
        // In units of the spacing, so the parabolas meet where whole places do.
        let square = spacing * spacing;
        let seeds = &self.seeds;
        let lifted = |at: usize| {
            seeds.get(at).map_or(f64::INFINITY, |&seed| seed / square) + real(at) * real(at)
        };
        let meet =
            |near: usize, far: usize| (lifted(far) - lifted(near)) / (2.0 * real(far - near));
        let mut held = 0usize;
        for at in (0..seeds.len()).filter(|&at| seeds.get(at).is_some_and(|seed| seed.is_finite()))
        {
            let mut start = f64::NEG_INFINITY;
            while let Some(&last) = held.checked_sub(1).and_then(|top| self.vertices.get(top)) {
                start = meet(last, at);
                if start
                    > self
                        .starts
                        .get(held - 1)
                        .copied()
                        .unwrap_or(f64::NEG_INFINITY)
                {
                    break;
                }
                held -= 1;
                start = f64::NEG_INFINITY;
            }
            if let (Some(vertex), Some(begins)) =
                (self.vertices.get_mut(held), self.starts.get_mut(held))
            {
                (*vertex, *begins) = (at, start);
                held += 1;
            }
        }
        let mut lowest = 0usize;
        for at in 0..seeds.len() {
            while lowest + 1 < held
                && self
                    .starts
                    .get(lowest + 1)
                    .is_some_and(|&start| start <= real(at))
            {
                lowest += 1;
            }
            let squared = match self.vertices.get(lowest).filter(|_| held > 0) {
                Some(&vertex) => {
                    let off = real(at.abs_diff(vertex)) * spacing;
                    off * off + seeds.get(vertex).copied().unwrap_or(f64::INFINITY)
                }
                None => f64::INFINITY,
            };
            found(at, squared);
        }
    }
}

/// How far within a footprint a vertex covered `cover` lies, in metres, its
/// squared distance to the nearest edge `squared`: negative outside, and no
/// further either way than `farthest`.
pub(super) fn signed_edge(cover: f32, squared: f64, farthest: f64) -> f32 {
    let distance = mathf::sqrt(squared.max(0.0)).min(farthest);
    single(if f64::from(cover) > FOOTPRINT {
        distance
    } else {
        -distance
    })
}

/// A band of heights no cloud stands in.
pub(super) const EMPTY_BAND: (f32, f32) = (f32::INFINITY, f32::NEG_INFINITY);

/// The heights between which `deck`'s cloud over the weather cell whose
/// corners are `corners` can stand, its turrets reaching no further out than
/// `reach`: bilinear between the corners, its edge, base and top lie within
/// theirs, and as a place lies no further within its envelope for a column
/// less far within its footprint, a shallower column or a place higher
/// above its base, the envelope at their furthest, deepest and lowest holds
/// every place over the cell. Rounded out to whole metres, which single
/// precision holds exactly; empty where no cloud can stand.
pub(super) fn band_over(deck: &Deck, corners: [Column; 4], reach: f64) -> (f32, f32) {
    let values = |field: fn(&Column) -> f32| corners.map(|corner| f64::from(field(&corner)));
    let most = |values: [f64; 4]| values.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let least = |values: [f64; 4]| values.into_iter().fold(f64::INFINITY, f64::min);
    let (bases, tops) = (values(|corner| corner.base), values(|corner| corner.top));
    let depth = most(corners.map(|corner| f64::from(corner.top - corner.base).max(0.0)));
    let margin = standing_margin(deck, reach);
    let Some(highest) = highest_rise(
        most(values(|corner| corner.edge)),
        shape(deck, depth),
        margin,
    ) else {
        return EMPTY_BAND;
    };
    let top = (most(bases) + highest).min(most(tops).max(most(bases) + LEAST_RISE) + margin);
    (single(mathf::floor(least(bases))), single(mathf::ceil(top)))
}

/// The highest a place can stand over its base and still lie within
/// `margin` of an envelope shaped `shape` over a column `edge` metres within
/// its footprint, to within half a metre above; `None` where none can. A
/// place lies no further within the envelope the higher it stands, so the
/// height is found by halving.
pub(super) fn highest_rise(edge: f64, shape: (f64, f64), margin: f64) -> Option<f64> {
    let reaches = |rise: f64| within_envelope(edge, rise, shape) + margin > 0.0;
    if !reaches(0.0) {
        return None;
    }
    // Above the envelope's top a place lies as far outside it as it stands
    // over the envelope straight below.
    let over = envelope(edge, shape) + margin;
    if over >= shape.0 {
        return Some(over);
    }
    let (mut low, mut high) = (0.0, shape.0);
    while high - low > 0.5 {
        let middle = f64::midpoint(low, high);
        if reaches(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    Some(high)
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
    tairix_parallel::for_each_band(runner, texels, (layers.start, per_layer), &|layer, band| {
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
