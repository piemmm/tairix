//! What a deck's cloud is made of where it stands: its weather, the heaps or
//! sheets it lays over its columns, their turrets, and the extinction their
//! water or ice holds.
//!
//! A deck's weather map says how far within a footprint each column stands
//! and where its base and top lie there. Over each footprint a cloud stands
//! as an envelope — a dome for a heap, a plateau for a sheet — on a flat base
//! at the level its air condenses, the envelope's surface pushed out into
//! turrets on turrets by billowing noise at six scales, each settling to its
//! mean where a footprint is too coarse to see it, and past them a fringe of
//! diluted droplets the wisps cut into puffs and filaments. Water holds
//! extinction rising with height as adiabatic liquid water does, so a heap is
//! thinnest at its base and densest high in its body; ice holds it evenly.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::{Deck, Matter};
use crate::band;
use crate::heightfield::bilinear;
use crate::noise::{cell, fbm2_resolved, hash3, noise2, resolved, smoothstep};
use crate::sample::{mix32, unit};
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
    fringe_mean: [f64; FRINGE_DEPTHS * FRINGE_BILLOWS],
}

impl Textures {
    pub(super) fn new() -> Option<Self> {
        Some(Self {
            shape: Tile::new(SHAPE_SIDE)?,
            detail: Tile::new(DETAIL_SIDE)?,
            shape_mean: 0.5,
            shape_peak: 1.0,
            wisp_mean: 0.5,
            fringe_mean: [0.0; FRINGE_DEPTHS * FRINGE_BILLOWS],
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

    /// Layers `layers` of the wisps, drawn from `seed`; their mean, and the
    /// fringe's they cut, once the last is.
    pub(super) fn fill_detail(&mut self, layers: Range<usize>, runner: &dyn JobRunner, seed: u32) {
        let last = layers.end >= self.detail.side;
        fill_tile(&mut self.detail, layers, runner, &|p| wisps(p, seed));
        if last {
            self.wisp_mean = self.detail.mean();
            self.fringe_mean = self.fringe_means();
        }
    }

    /// The fringe's mean over every place its wisps are read wholly
    /// resolved, at each of the tabulated depths into it and billowings.
    fn fringe_means(&self) -> [f64; FRINGE_DEPTHS * FRINGE_BILLOWS] {
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
        let total = f64::from(counts.iter().sum::<u32>().max(1));
        let mut means = [0.0; FRINGE_DEPTHS * FRINGE_BILLOWS];
        for (index, mean) in means.iter_mut().enumerate() {
            let into = real(index % FRINGE_DEPTHS) / real(FRINGE_DEPTHS - 1);
            let billowing = real(index / FRINGE_DEPTHS) / real(FRINGE_BILLOWS - 1);
            *mean = counts
                .iter()
                .enumerate()
                .map(|(bin, &count)| {
                    let wisp = (real(bin) + 0.5) / real(WISP_BINS);
                    f64::from(count) * eroded(into, wisp, billowing)
                })
                .sum::<f64>()
                / total;
        }
        means
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

    /// How dense a cloud stands `surface` metres within its turrets'
    /// surface, its skin and fringe as wide as `widths` has them and its
    /// fringe `billowing` there: whole past its skin, and diluted outward
    /// across its fringe as the cube of how far into it the place lies, so
    /// its outer half holds a few hundredths of the body's water and a
    /// grazing look through it shows the sky; cut by its wisps as far as
    /// `turrets` resolves them.
    fn fringed(
        &self,
        surface: f64,
        (skin, fringe): (f64, f64),
        billowing: f64,
        turrets: &Turrets,
    ) -> f64 {
        let ramp = smoothstep(-fringe, 0.5 * skin, surface);
        let into = ramp * ramp * ramp;
        let Some((wisp, kept)) = turrets.wisps.filter(|_| into > 0.0 && into < 1.0) else {
            return into;
        };
        let cut = eroded(into, wisp, billowing);
        if kept >= 1.0 {
            return cut;
        }
        let mean = self.fringe_mean_at(into, billowing);
        mean + (cut - mean) * kept
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

    /// Where a place `inside` metres within `deck`'s envelope stands against
    /// its turrets at `grained`, its billows' own coordinates, its skin and
    /// fringe as wide as `widths` has them: each scale finer than
    /// `footprint` metres settled to its mean, or, `bounding`, pushed out as
    /// far as any finer scale could past the first. Read no further once the
    /// scales still to come could neither bring the surface out to the place
    /// nor take it back past it.
    fn surface(
        &self,
        deck: &Deck,
        grained: Vec3,
        (footprint, bounding): (f64, bool),
        (inside, (skin, fringe)): (f64, (f64, f64)),
    ) -> Surface {
        let reach = bubble_reach(deck);
        let period = deck.billow * 8.0;
        let texel = grained * (real(SHAPE_SIDE) / period);
        let (rise, fall) = (self.shape_peak - self.shape_mean, self.shape_mean);
        // A scale's bubbles are some half a cell across.
        let kept: [f64; BUBBLES.len()] = core::array::from_fn(|index| {
            let times = BUBBLES.get(index).map_or(1.0, |&(times, ..)| times);
            if index == 0 || bounding {
                1.0
            } else {
                resolved(footprint, 0.5 * period / (BUBBLE_CELLS * times))
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
            if inside + out + ahead * rise + fringe <= 0.0 {
                return Surface::Clear;
            }
            let least = inside + out - ahead * fall - 0.5 * skin;
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
    let wisp = wisp.clamp(0.0, 1.0);
    let cut = DEEPEST_CUT
        * smoothstep(
            CUTTING.0,
            CUTTING.1,
            wisp + (1.0 - 2.0 * wisp) * billowing.clamp(0.0, 1.0),
        );
    ((into - cut) / (1.0 - cut)).clamp(0.0, 1.0)
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
}

/// The shape of `deck`'s envelope over a column `depth` deep: that depth,
/// and the run in from the footprint's edge over which it rises to it.
fn shape(deck: &Deck, depth: f64) -> (f64, f64) {
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
    let shape = shape(deck, depth);
    let inside = within_envelope(column.at(|corner| corner.edge), rise, shape);
    let footprint = edges_footprint(deck, edges);
    let skin = skin(deck, footprint);
    let billowing = smoothstep(BILLOWING.0, BILLOWING.1, rise / shape.0);
    // Never narrower than half its skin, so a coarser footprint widens the
    // edge about the surface rather than eating into the cloud.
    let fringe = fringe(deck, billowing).max(0.5 * skin);
    if inside + textures.reach(deck) + fringe <= 0.0 {
        return None;
    }
    let bounding = matches!(edges, Edges::Bounding(_));
    match textures.surface(
        deck,
        grained,
        (footprint, bounding),
        (inside, (skin, fringe)),
    ) {
        Surface::Clear => None,
        Surface::Solid(within) => Some(Held {
            extinction: extinction(deck, rise),
            within,
        }),
        Surface::Edge(turrets) => {
            let surface = inside + turrets.out;
            let density = textures.fringed(surface, (skin, fringe), billowing, &turrets);
            (density > 0.0).then(|| Held {
                extinction: density * extinction(deck, rise),
                within: surface - 0.5 * skin,
            })
        }
    }
}

/// Whether `deck`'s cloud, read through a `footprint` metres across, can
/// stand anywhere over columns standing no further within their footprint
/// than `edge` and no deeper than `depth`, no nearer its base than `rise`:
/// as a place lies no further within its envelope for a column less far
/// within its footprint, a shallower column or a place higher above its
/// base, the envelope at those holds every place among them, its turrets and
/// fringe reaching out at their furthest.
pub(super) fn may_hold(
    deck: &Deck,
    textures: &Textures,
    (edge, depth, rise): (f64, f64, f64),
    footprint: f64,
) -> bool {
    let inside = within_envelope(edge, rise.max(0.0), shape(deck, depth));
    inside + textures.reach(deck) + fringe(deck, 0.0).max(0.5 * skin(deck, footprint)) > 0.0
}

/// How wide `deck`'s cloud's edge stands, in metres, read through a
/// `footprint` metres across: never sharper than its droplets make it, and
/// never wider than a billow, so its bands hold every read.
fn skin(deck: &Deck, footprint: f64) -> f64 {
    SKIN.max(footprint.min(deck.billow))
}

/// How far out past its turrets `deck`'s fringe reaches, in metres, where it
/// is `billowing`.
fn fringe(deck: &Deck, billowing: f64) -> f64 {
    deck.billow * (FRINGE.1 + (FRINGE.0 - FRINGE.1) * billowing.clamp(0.0, 1.0))
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
    let margin = reach + fringe(deck, 0.0).max(0.5 * skin(deck, f64::INFINITY));
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
fn highest_rise(edge: f64, shape: (f64, f64), margin: f64) -> Option<f64> {
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
