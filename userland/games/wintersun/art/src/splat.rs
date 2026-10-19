//! The splat: turning the weight field into pixels.
//!
//! Terrain has no tile grid. A pixel is the blend of the materials the
//! weight field holds where it lies — but blended by **height**, not by
//! weight alone. Each material carries its own surface relief, and where one
//! stands proud of the others it wins the pixel outright. That is the
//! difference between gravel emerging through grass in patches and the two
//! averaging into a grey that is neither.
//!
//! # A quad, then a fragment
//!
//! The weight field is a texture of the cells' blends, sampled bilinearly. A
//! fragment inside one quad of it samples only that quad's four corners, so
//! the grounds it can draw are planned once per quad ([`QuadPlan`]) and each
//! fragment interpolates their weights and reads one texel per ground.
//!
//! # A mip is the same ground, coarser
//!
//! Each ground is read at the mip whose texels are as far apart as its
//! pixel's footprint on the ground, the choice dithered between neighbouring
//! levels, and addressed at that mip's own texel spacing: a coarser tile is
//! the same field sampled further apart, so the ground keeps its scale with
//! distance rather than shrinking into repetition.
//!
//! # Repetition, and the one field that breaks it
//!
//! A synthesised tile is finite, so a lookup that was an affine function
//! of world position would show the tile's period across a large
//! grassland. The lookup is therefore displaced by a smooth, low-frequency
//! vector field ([`Warp`]) before it is taken.
//!
//! One warp field is all three of the jitters this needs. Its Jacobian
//! carries a local rotation, a local scale and a local offset together, so
//! there is no separate rotation to seam at a lattice boundary and no
//! separate scale to fight it: the field is continuous, therefore the
//! distortion is, therefore there is no edge anywhere for the eye to find.

use tairix_raster::color::Pixel;
use tairix_wintersun_net::value::WorldPoint;
use tairix_wintersun_world::blend::{Kind, BLEND_SLOTS};
use tairix_wintersun_world::ground::{Ground, GROUND_COUNT};

use crate::cache::{MaterialCache, TileKey};
use crate::material::{self, MaterialTile, Mip, Quality, Texel, MIP_LEVELS};
use crate::noise::{self, Field, Tiled};
use crate::weight::{WeightField, TOTAL};

/// How much of a material's height counts toward winning a pixel,
/// as a right shift of the `0..=255` height.
///
/// A height contributes at most a quarter of the weight range, so a
/// material has to already hold a comparable share of the cell before its
/// relief can punch through. Without that bound a trace of sand would
/// stand proud across an entire grassland; with it, the height decides
/// where two materials genuinely meet and nothing else.
const HEIGHT_SHIFT: u32 = 2;

/// The most a material's relief can add to its score.
///
/// A height is a `u8`, so this is the whole range shifted down by
/// [`HEIGHT_SHIFT`]. It bounds how far relief alone can lift a slot,
/// which is what makes a slot's irrelevance decidable from its weight.
const MAX_HEIGHT_SCORE: u16 = (u8::MAX as u16) >> HEIGHT_SHIFT;

/// The band below the winning score within which materials still blend.
///
/// Zero would be a hard argmax — one material per pixel, and a boundary
/// that aliases into stair-steps. The whole weight range would be a plain
/// linear blend with the height doing nothing. This is the value between
/// them: sharp enough that gravel reads as gravel, soft enough that the
/// transition is anti-aliased by the blend itself.
const BLEND_DEPTH: u16 = 24;

/// The largest divisor a pixel's blend can produce.
///
/// Every slot's share is its score above the blend floor, which is at
/// most [`BLEND_DEPTH`], and there are [`BLEND_SLOTS`] of them. The
/// divisor is therefore small and bounded, which is what lets the three
/// channel means below be taken without dividing.
const MAX_BLEND_TOTAL: usize = BLEND_SLOTS * BLEND_DEPTH as usize;

/// Fixed-point fraction bits of [`RECIPROCAL`].
const RECIPROCAL_BITS: u32 = 32;

/// `ceil(2^RECIPROCAL_BITS / d)` for every divisor `d` a blend can
/// produce, so a channel mean is a multiply and a shift.
///
/// Exact, not approximate: a channel sum is at most `255 * d`, and
/// `255 * d * d` stays far below `2^RECIPROCAL_BITS` for every divisor in
/// range, which is the condition under which the rounded-up reciprocal
/// floors exactly as the division does. `reciprocals_are_exact` checks
/// that over the whole domain rather than leaving it to the argument.
const RECIPROCAL: [u64; MAX_BLEND_TOTAL + 1] = {
    let mut table = [0u64; MAX_BLEND_TOTAL + 1];
    let mut divisor = 1usize;
    while divisor <= MAX_BLEND_TOTAL {
        let d = divisor as u64;
        table[divisor] = (1u64 << RECIPROCAL_BITS).div_ceil(d);
        divisor += 1;
    }
    table
};

/// One channel's weighted mean, `sum / total`, without a division.
///
/// `total` is clamped into the table's domain; a blend never produces a
/// divisor outside it, and clamping rather than indexing blind keeps a
/// future change to the blend from reading past the table.
fn mean(sum: u32, total: u32) -> u8 {
    let total = (total.max(1) as usize).min(MAX_BLEND_TOTAL);
    let scaled = (u64::from(sum) * RECIPROCAL[total]) >> RECIPROCAL_BITS;
    u8::try_from(scaled.min(u64::from(u8::MAX))).unwrap_or(u8::MAX)
}

/// A smooth, low-frequency displacement of the material lookup.
///
/// Sampled in world sub-units, so two chunks agree across their seam
/// without either knowing about the other.
#[derive(Copy, Clone, Debug)]
pub struct Warp {
    field: Tiled,
    amplitude: i32,
    cell_log2: u32,
}

/// Log2 of the warp lattice's cell, in world sub-units.
///
/// Deliberately far coarser than any material tile: the warp has to vary
/// slowly enough that the distortion reads as terrain rather than as a
/// ripple, while still being incommensurate with every tile's period.
pub const WARP_CELL_LOG2: u32 = 14;

/// Half-width of the warp's displacement, in world sub-units.
///
/// Comparable to a material tile's world extent, which is what it takes
/// to actually break the period rather than merely soften it.
pub const WARP_AMPLITUDE: i32 = 1 << 12;

impl Warp {
    /// The warp for a realm.
    ///
    /// Keyed on the realm seed, so two realms do not wear the same
    /// distortion, and every client of one realm draws the same ground.
    #[must_use]
    pub const fn new(realm_seed: u64) -> Self {
        Self {
            field: Tiled::unbounded(realm_seed),
            amplitude: WARP_AMPLITUDE,
            cell_log2: WARP_CELL_LOG2,
        }
    }

    /// The displacement at a world point, in sub-units.
    #[must_use]
    pub fn at(&self, point: WorldPoint) -> (i32, i32) {
        let x = self
            .field
            .value(Field::WarpX, point.x, point.y, self.cell_log2);
        let y = self
            .field
            .value(Field::WarpY, point.x, point.y, self.cell_log2);
        (
            noise::centred(x, self.amplitude),
            noise::centred(y, self.amplitude),
        )
    }
}

/// The grounds a quad of the weight field draws, and each one's weight at the
/// quad's four corners.
///
/// The weight field is a texture whose texels are the cells' blends; a
/// fragment inside a quad samples it bilinearly from the quad's corners, so
/// the grounds a quad can draw are those its corners hold. Planned once per
/// quad and read per fragment, which keeps the per-pixel cost to a weight
/// interpolation, the texel reads and a blend.
#[derive(Copy, Clone, Debug)]
pub struct QuadPlan {
    slots: [PlanSlot; BLEND_SLOTS],
    used: usize,
}

/// One ground's part in a quad: its weight at each corner, in the order
/// [`QuadPlan::new`] takes them.
#[derive(Copy, Clone, Debug)]
struct PlanSlot {
    ground: Ground,
    corners: [u16; CORNERS],
}

/// A quad's corners: north-west, north-east, south-west, south-east.
pub const CORNERS: usize = 4;

/// A fragment's place across or down its quad, out of this.
pub const QUAD_ONE: u16 = 256;

impl QuadPlan {
    /// Plan the quad whose corners hold `corners`, north-west, north-east,
    /// south-west and south-east.
    ///
    /// The union of the corners' grounds is kept to the heaviest
    /// [`BLEND_SLOTS`] by their combined prominence and each corner is
    /// renormalised over that set, so every point the quad interpolates is
    /// itself normalised and the kernel needs no per-pixel renormalisation.
    #[must_use]
    pub fn new(corners: [&WeightField; CORNERS]) -> Self {
        let mut entries = [(corners[0].dominant(), [0u32; CORNERS]); BLEND_SLOTS * CORNERS];
        let mut count = 0;
        for (corner, field) in corners.iter().enumerate() {
            for slot in field.slots() {
                let index =
                    if let Some(held) = entries[..count].iter().position(|e| e.0 == slot.ground) {
                        held
                    } else {
                        entries[count] = (slot.ground, [0; CORNERS]);
                        count += 1;
                        count - 1
                    };
                entries[index].1[corner] = u32::from(slot.weight);
            }
        }
        entries[..count].sort_unstable_by(|a, b| {
            let sum = |w: &[u32; CORNERS]| w.iter().sum::<u32>();
            sum(&b.1)
                .cmp(&sum(&a.1))
                .then_with(|| a.0.id().cmp(&b.0.id()))
        });

        let kept = count.min(BLEND_SLOTS);
        let mut sums = [0u32; CORNERS];
        for entry in &entries[..kept] {
            for (sum, weight) in sums.iter_mut().zip(entry.1) {
                *sum += weight;
            }
        }
        let mut slots = [PlanSlot {
            ground: entries[0].0,
            corners: [0; CORNERS],
        }; BLEND_SLOTS];
        for (slot, entry) in slots.iter_mut().zip(entries[..kept].iter()) {
            let mut weights = [0u16; CORNERS];
            for ((weight, &raw), &sum) in weights.iter_mut().zip(&entry.1).zip(&sums) {
                *weight = renormalise(raw, sum);
            }
            *slot = PlanSlot {
                ground: entry.0,
                corners: weights,
            };
        }
        let mut plan = Self {
            slots,
            used: kept.max(1),
        };
        plan.settle();
        plan
    }

    /// The grounds this quad can draw, heaviest first.
    pub fn grounds(&self) -> impl Iterator<Item = Ground> + '_ {
        self.slots[..self.used].iter().map(|s| s.ground)
    }

    /// How many grounds the quad draws.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.used
    }

    /// Whether the quad draws nothing, which a planned quad never does.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// Each slot's weight at `across` and `down` within the quad, out of
    /// [`QUAD_ONE`]: the bilinear sample of its corners.
    #[must_use]
    pub fn weights_at(&self, across: u16, down: u16) -> [u16; BLEND_SLOTS] {
        let (across, down) = (
            i32::from(across.min(QUAD_ONE)),
            i32::from(down.min(QUAD_ONE)),
        );
        let one = i32::from(QUAD_ONE);
        let mut out = [0u16; BLEND_SLOTS];
        for (weight, slot) in out.iter_mut().zip(&self.slots[..self.used]) {
            let [nw, ne, sw, se] = slot.corners.map(i32::from);
            let north = nw * one + (ne - nw) * across;
            let south = sw * one + (se - sw) * across;
            let value = (north * one + (south - north) * down) / (one * one);
            *weight = u16::try_from(value.clamp(0, i32::from(TOTAL))).unwrap_or(0);
        }
        out
    }

    /// Hand each corner's rounding remainder to its own heaviest slot, so
    /// every corner sums to exactly [`TOTAL`].
    ///
    /// Per corner, not per plan: the slots are ordered by *combined*
    /// prominence, so slot zero can hold nothing at one corner — and giving
    /// it the remainder there would put a ground into a corner whose field
    /// never had it.
    fn settle(&mut self) {
        let slots = &mut self.slots[..self.used];
        for corner in 0..CORNERS {
            let held: u16 = slots.iter().map(|s| s.corners[corner]).sum();
            if let Some(slot) = slots.iter_mut().find(|s| s.corners[corner] > 0) {
                slot.corners[corner] += TOTAL - held.min(TOTAL);
            }
        }
    }
}

/// `value` as a share of [`TOTAL`], flooring; zero when there is nothing
/// to share.
fn renormalise(value: u32, sum: u32) -> u16 {
    if sum == 0 {
        return 0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a share of TOTAL is below it, and TOTAL is a u16"
    )]
    {
        (value * u32::from(TOTAL) / sum) as u16
    }
}

/// The resident tiles a frame draws its grounds from: for each ground and
/// each mip level, the finest tile at that level or coarser that the cache
/// holds.
///
/// Resolved once a frame, so a fragment's tile is one index and the pass
/// is total: a level the cache did not admit reads the next coarser one,
/// and a ground with none draws its flat tone.
#[derive(Clone, Debug)]
pub struct Materials<'a> {
    chains: [[Option<&'a MaterialTile>; MIP_COUNT]; GROUND_COUNT],
}

/// Levels in a ground's chain.
const MIP_COUNT: usize = MIP_LEVELS as usize;

impl<'a> Materials<'a> {
    /// No tile for anything: every ground in its flat tone.
    #[must_use]
    pub const fn flat() -> Self {
        Self {
            chains: [[None; MIP_COUNT]; GROUND_COUNT],
        }
    }

    /// The tiles `cache` holds at `quality` for each of `grounds`: at each
    /// level, the tile there or the nearest coarser one, and the nearest finer
    /// one where nothing coarser is held, so a ground with any tile at all is
    /// never drawn in its flat tone.
    pub fn resolve(
        cache: &'a MaterialCache,
        quality: Quality,
        grounds: impl Iterator<Item = Ground>,
    ) -> Self {
        let mut materials = Self::flat();
        for ground in grounds {
            let Some(chain) = materials.chains.get_mut(usize::from(ground.id())) else {
                continue;
            };
            let mut coarser = None;
            for (level, slot) in (0..MIP_LEVELS).rev().zip(chain.iter_mut().rev()) {
                if let Some(mip) = Mip::new(level) {
                    coarser = cache.peek(quality, &TileKey { ground, mip }).or(coarser);
                }
                *slot = coarser;
            }
            let mut finer = None;
            for slot in chain.iter_mut() {
                finer = slot.or(finer);
                *slot = finer;
            }
        }
        materials
    }

    /// Hold `tile` for its own ground: at its own level, and at every finer
    /// one holding nothing finer than it.
    pub fn hold(&mut self, tile: &'a MaterialTile) {
        let Some(chain) = self.chains.get_mut(usize::from(tile.ground().id())) else {
            return;
        };
        let level = tile.mip().level();
        for slot in chain.iter_mut().take(level as usize + 1) {
            if slot.is_none_or(|held| held.mip().level() > level) {
                *slot = Some(tile);
            }
        }
    }

    /// The tile `ground` is read from at `mip`, or `None` for its flat tone.
    #[must_use]
    pub fn tile(&self, ground: Ground, mip: Mip) -> Option<&'a MaterialTile> {
        self.chains
            .get(usize::from(ground.id()))
            .and_then(|chain| chain.get(mip.level() as usize))
            .copied()
            .flatten()
    }
}

/// One fragment of ground.
///
/// Where its material is read — a warped world position — how far across
/// and down its quad it lies, how much ground one pixel spans there, and the
/// fraction its mip choice is dithered by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Fragment {
    /// The warped world position the materials are read at.
    pub sample: WorldPoint,
    /// How far across the quad, out of [`QUAD_ONE`].
    pub across: u16,
    /// How far down the quad, out of [`QUAD_ONE`].
    pub down: u16,
    /// World sub-units one pixel spans on the ground here.
    pub footprint: u32,
    /// Sixteenths of a mip level the choice is raised by, `0..16`.
    pub dither: u8,
}

/// Shade one fragment of `plan`'s quad from `materials`.
///
/// Each ground is read at the mip whose texels its pixel's footprint spans,
/// addressed at that mip's own spacing, so a feature of the ground keeps its
/// world size at every distance.
#[must_use]
pub fn fragment(plan: &QuadPlan, materials: &Materials<'_>, at: Fragment) -> Pixel {
    // One ground wins its pixel whatever its weight, and the blend of one
    // share is exact, so its texel is the pixel.
    if let [only] = &plan.slots[..plan.used] {
        let texel = sample(only.ground, materials, at);
        return Pixel {
            r: texel.r,
            g: texel.g,
            b: texel.b,
            a: u8::MAX,
        };
    }
    let weights = plan.weights_at(at.across, at.down);
    let heaviest = weights.iter().copied().max().unwrap_or(0);
    let cutoff = cutoff_below(heaviest);
    let mut texels = [Texel::VOID; BLEND_SLOTS];
    let mut scores = [0u16; BLEND_SLOTS];
    let mut top = 0u16;
    for (index, slot) in plan.slots[..plan.used].iter().enumerate() {
        let weight = weights[index];
        if weight < cutoff {
            continue;
        }
        let texel = sample(slot.ground, materials, at);
        let score = weight.saturating_add(u16::from(texel.height) >> HEIGHT_SHIFT);
        texels[index] = texel;
        scores[index] = score;
        top = top.max(score);
    }

    let floor = top.saturating_sub(BLEND_DEPTH);
    let (mut red, mut green, mut blue, mut total) = (0u32, 0u32, 0u32, 0u32);
    for (texel, score) in texels[..plan.used].iter().zip(&scores) {
        let share = u32::from(score.saturating_sub(floor));
        if share == 0 {
            continue;
        }
        red += u32::from(texel.r) * share;
        green += u32::from(texel.g) * share;
        blue += u32::from(texel.b) * share;
        total += share;
    }
    // The winning slot's share is `BLEND_DEPTH`, so the divisor is never
    // zero for a planned quad.
    Pixel {
        r: mean(red, total),
        g: mean(green, total),
        b: mean(blue, total),
        // Ground is opaque, and a premultiply by a full alpha is the
        // identity, so the channels above are already premultiplied.
        a: u8::MAX,
    }
}

/// `ground`'s texel under fragment `at`, read at the mip its footprint asks
/// for, or its flat tone where `materials` holds no tile of it.
fn sample(ground: Ground, materials: &Materials<'_>, at: Fragment) -> Texel {
    let params = material::params(ground);
    let wanted = Mip::at(params.grain_shift, at.footprint, at.dither);
    match materials.tile(ground, wanted) {
        Some(tile) if tile.ground() == ground => {
            let shift = params.grain_shift + tile.mip().level();
            #[allow(
                clippy::cast_sign_loss,
                reason = "the tile's own mask makes the wrap two's-complement periodic, \
                          which is what a negative world coordinate needs"
            )]
            tile.texel((at.sample.x >> shift) as u32, (at.sample.y >> shift) as u32)
        }
        _ => params.flat(),
    }
}

/// The weight below which a slot cannot reach the blend floor, given the
/// `heaviest` weight in the span at this pixel.
///
/// The floor is the winning *score* less [`BLEND_DEPTH`], and the winning
/// score is at least `heaviest`, so a slot whose weight plus the most
/// relief could add still falls short of `heaviest - BLEND_DEPTH` would
/// contribute a zero share. Saturating, so a span whose weights are all
/// small skips nothing.
fn cutoff_below(heaviest: u16) -> u16 {
    heaviest.saturating_sub(BLEND_DEPTH.saturating_add(MAX_HEIGHT_SCORE))
}

#[cfg(test)]
mod tests;
