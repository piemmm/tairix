//! Soft drop shadows: the one definition the compositor lays beneath its
//! floating surfaces and the pinned `shadow` pass is held to.
//!
//! The light is overhead, so a surface's shadow is its own silhouette dropped
//! by its reach `ρ` and softened by a smooth compact kernel of the same reach:
//! nothing shows above the top edge, `ρ` beside each side, and `2ρ` below. The
//! shadow only darkens what is *outside* the silhouette, so a translucent or
//! frosted surface never shows its own shadow through itself.
//!
//! # Exact by linearity, evaluated where it varies
//!
//! Blurring is linear, and a rounded silhouette is its rectangle less four
//! corner notches. The rectangle's blur is separable — a horizontal and a
//! vertical cumulative kernel, `H(x)·V(y)` — and each notch's blur is a small
//! tile computed once per corner radius and mirrored to all four corners, the
//! supersampled corner coverage being symmetric. So a shadow pixel is two
//! table lookups, a multiply, and at most four tile reads, and nothing per
//! surface is retained at all.
//!
//! A row beneath the surface is the kernel's vertical term alone across its
//! middle, so that span is one solid blend; per-pixel work is confined to the
//! columns a ramp or a corner can reach, and beside a straight edge the ramp
//! is read from a profile computed once.
//!
//! # Between two shadows
//!
//! A surface easing from one shadow to another casts both at once, each
//! weighted by how far it has risen ([`shadow_row`]), so the change is a
//! crossfade between two exact shadows rather than a guess at one between.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::color::{blend_solid_span, div255, Color, Pixel};
use crate::dither::DitherRow;
use crate::round::RoundRect;

/// The binary places the kernel's running sum and a shadow's strength are
/// carried in.
pub const SHADOW_SHIFT: u32 = 15;

/// The fixed-point one both the kernel's running sum and a shadow's strength
/// are carried in.
pub const SHADOW_ONE: u32 = 1 << SHADOW_SHIFT;

/// The furthest a shadow reaches, in pixels.
///
/// A bound on what a theme may ask for, not a capacity: a shadow reaching
/// further has stopped reading as one, and the kernel's integer arithmetic is
/// sized for it.
pub const MAX_REACH: u32 = 255;

/// How far a shadow of `reach` pixels in `color` reaches: nought where the
/// colour darkens nothing, and never past [`MAX_REACH`].
#[must_use]
pub const fn shadow_reach(reach: u32, color: Color) -> u32 {
    if color.a == 0 {
        0
    } else if reach > MAX_REACH {
        MAX_REACH
    } else {
        reach
    }
}

/// A surface casting a shadow: its bounds, the silhouette it is cut to within
/// them, and the opacity it is drawn at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Caster {
    /// Its leftmost column.
    pub left: i32,
    /// Its top row.
    pub top: i32,
    /// The column past its rightmost.
    pub right: i32,
    /// The row past its lowest.
    pub bottom: i32,
    /// The rounded rectangle it is cut to, its own pixels counted from its
    /// top left; `None` where it is square.
    pub silhouette: Option<RoundRect>,
    /// The opacity it is drawn at.
    pub opacity: u8,
}

impl Caster {
    /// Whether it covers no pixel at all.
    const fn is_empty(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }
}

/// One corner's notch, blurred: the tile a rounded silhouette's shadow
/// subtracts from its rectangle's at each corner.
struct Tile {
    /// The corner radius the notch is cut by.
    radius: u32,
    /// The tile's side: the notch widened by the kernel's reach either way.
    side: u32,
    /// `side × side` strengths in [`SHADOW_ONE`] units, row-major, laid out
    /// for the top-left corner; the other three read it mirrored.
    values: Vec<u16>,
}

impl Tile {
    /// Row `row` of the tile, which the caller has checked is inside it.
    fn row(&self, row: i64) -> &[u16] {
        let side = usize::try_from(self.side).unwrap_or(0);
        let start = usize::try_from(row).unwrap_or(0).saturating_mul(side);
        self.values
            .get(start..start.saturating_add(side))
            .unwrap_or(&[])
    }
}

/// One blurred corner notch as a device reads it: its side, and its
/// `side × side` strengths in [`SHADOW_ONE`] units, row by row, for the top
/// left corner.
#[derive(Copy, Clone, Debug)]
pub struct Notch<'a> {
    /// The tile's side.
    pub side: u32,
    /// Its strengths, row by row.
    pub values: &'a [u16],
}

/// One shadow a surface can cast: its reach, its colour, the kernel it is
/// softened by and the corner tiles it is cut by.
pub struct DropShadow {
    /// The reach, in pixels; `0` draws no shadow at all.
    reach: u32,
    /// The shadow colour at its darkest, premultiplied.
    color: Pixel,
    /// The kernel's running sum over `-reach..=reach`, in [`SHADOW_ONE`]
    /// units.
    ramp: Vec<u32>,
    /// The shadow's level across a straight edge the vertical term reaches in
    /// full: `reach` columns leading up to a left edge, then `reach` leading
    /// away from a right one. Empty where the allocator refused it, which
    /// leaves every pixel to the general evaluation.
    edge: Vec<u8>,
    /// A notch tile per corner radius a caster has needed.
    tiles: Vec<Tile>,
}

impl DropShadow {
    /// A shadow that draws nothing.
    pub const NONE: Self = Self {
        reach: 0,
        color: Pixel::TRANSPARENT,
        ramp: Vec::new(),
        edge: Vec::new(),
        tiles: Vec::new(),
    };

    /// The shadow in `color` reaching [`shadow_reach`] of `reach` pixels,
    /// holding no corner tile yet. Casts nothing when the colour darkens
    /// nothing, the reach is nought, or the allocator refuses its kernel —
    /// which costs a desktop its depth and never a frame.
    #[must_use]
    pub fn new(reach: u32, color: Color) -> Self {
        let reach = shadow_reach(reach, color);
        let Some(ramp) = kernel_ramp(reach) else {
            return Self::NONE;
        };
        let edge = edge_profile(&ramp, reach).unwrap_or_default();
        Self {
            reach,
            color: color.premultiply(),
            ramp,
            edge,
            tiles: Vec::new(),
        }
    }

    /// The reach, in pixels; `0` for a shadow that draws nothing.
    #[must_use]
    pub const fn reach(&self) -> u32 {
        self.reach
    }

    /// The colour at its darkest, premultiplied.
    #[must_use]
    pub const fn color(&self) -> Pixel {
        self.color
    }

    /// The kernel's running sum over `-reach..=reach`, in [`SHADOW_ONE`]
    /// units: nothing before its first entry, all of it past its last.
    #[must_use]
    pub fn ramp(&self) -> &[u32] {
        &self.ramp
    }

    /// The blurred notch of a corner rounded by `radius`, where it is held.
    #[must_use]
    pub fn notch(&self, radius: u32) -> Option<Notch<'_>> {
        self.notches()
            .find_map(|(held, notch)| (held == radius).then_some(notch))
    }

    /// Every notch held, with the corner radius it is cut by.
    pub fn notches(&self) -> impl Iterator<Item = (u32, Notch<'_>)> {
        self.tiles.iter().map(|tile| {
            (
                tile.radius,
                Notch {
                    side: tile.side,
                    values: &tile.values,
                },
            )
        })
    }

    /// Build the tile for `radius` unless it is already held. A tile the
    /// allocator refuses is simply absent: that shadow is drawn without its
    /// corner notches, a little dark under the corners, rather than not at
    /// all.
    pub fn ensure_tile(&mut self, radius: u32) {
        if radius == 0 || self.reach == 0 || self.tile(radius).is_some() {
            return;
        }
        if let Some(tile) = notch_tile(&self.ramp, radius) {
            if self.tiles.try_reserve(1).is_ok() {
                self.tiles.push(tile);
            }
        }
    }

    /// Drop every tile but those for the radii `keep` answers for.
    pub fn retain_tiles(&mut self, keep: impl Fn(u32) -> bool) {
        self.tiles.retain(|tile| keep(tile.radius));
    }

    fn tile(&self, radius: u32) -> Option<&Tile> {
        self.tiles.iter().find(|tile| tile.radius == radius)
    }

    /// The kernel's running sum up to offset `d`: nothing before its reach,
    /// all of it past.
    fn cum(&self, d: i64) -> u32 {
        let reach = i64::from(self.reach);
        if d < -reach {
            return 0;
        }
        usize::try_from(d + reach)
            .ok()
            .and_then(|index| self.ramp.get(index))
            .copied()
            .unwrap_or(SHADOW_ONE)
    }

    /// The shadow `caster` casts across row `y`, or `None` where it casts
    /// nothing there.
    fn row(&self, caster: &Caster, y: i32) -> Option<LevelRow<'_>> {
        if self.reach == 0 || caster.opacity == 0 || caster.is_empty() {
            return None;
        }
        let reach = i64::from(self.reach);
        let (top, bottom) = (i64::from(caster.top), i64::from(caster.bottom));
        let y = i64::from(y);
        if y < top || y >= bottom + 2 * reach {
            return None;
        }
        let v = self
            .cum(y - reach - top)
            .saturating_sub(self.cum(y - reach - bottom));
        if v == 0 {
            return None;
        }
        let radius = caster.silhouette.map_or(0, RoundRect::corner_reach);
        let mut notches: [&[u16]; 2] = [&[], &[]];
        if let Some(tile) = self.tile(radius) {
            // The top notches' shadow starts at the top edge; the bottom
            // notches' is that one turned upside down, ending 2ρ below.
            let rows = [y - top, bottom + 2 * reach - 1 - y];
            for (notch, row) in notches.iter_mut().zip(rows) {
                if (0..i64::from(tile.side)).contains(&row) {
                    *notch = tile.row(row);
                }
            }
        }
        let inside = (y < bottom).then(|| {
            let ly = u32::try_from(y - top).unwrap_or(0);
            (caster.silhouette, ly)
        });
        // Beneath the caster the corners' reach varies per column; across it,
        // only an arc the row crosses leaves the silhouette's edge uneven.
        let arc = match inside {
            None => radius.saturating_add(self.reach),
            Some((silhouette, ly)) => silhouette
                .filter(|silhouette| silhouette.clips_row(ly))
                .map_or(0, |_| radius),
        };
        Some(LevelRow {
            shadow: self,
            left: caster.left,
            right: caster.right,
            arc,
            v,
            notches,
            inside,
            opacity: caster.opacity,
        })
    }
}

impl core::fmt::Debug for DropShadow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DropShadow")
            .field("reach", &self.reach)
            .field("color", &self.color)
            .field("tiles", &self.tiles.len())
            .finish_non_exhaustive()
    }
}

/// The shadow `caster` casts across row `y` while it stands `lift` of the
/// way from `resting` to `lifted`, or `None` where it casts nothing there.
///
/// At either end the row is that shadow alone; between them both are cast,
/// each weighted by its share of the lift.
#[must_use]
pub fn shadow_row<'a>(
    lifted: &'a DropShadow,
    resting: &'a DropShadow,
    caster: &Caster,
    lift: u8,
    y: i32,
) -> Option<ShadowRow<'a>> {
    let high = if lift > 0 {
        lifted.row(caster, y)
    } else {
        None
    };
    let low = if lift < u8::MAX {
        resting.row(caster, y)
    } else {
        None
    };
    let row = match (high, low) {
        (None, None) => return None,
        (Some(row), None) if lift == u8::MAX => Row::One(row),
        (None, Some(row)) if lift == 0 => Row::One(row),
        (lifted, resting) => Row::Mixed {
            lifted,
            resting,
            lift,
        },
    };
    Some(ShadowRow(row))
}

/// One row of a surface's shadow, resolved once for the whole row.
pub struct ShadowRow<'a>(Row<'a>);

enum Row<'a> {
    /// A shadow standing at one level.
    One(LevelRow<'a>),
    /// A shadow between the levels while its caster eases from one to the
    /// other: each level weighted by how far it has risen, `lift` out of
    /// [`u8::MAX`] toward the lifted one. A level that casts nothing on this
    /// row is absent.
    Mixed {
        lifted: Option<LevelRow<'a>>,
        resting: Option<LevelRow<'a>>,
        lift: u8,
    },
}

impl ShadowRow<'_> {
    /// The shadow's own pixel at column `x`, or `None` where it draws
    /// nothing: the oracle a row's blend is held to.
    #[cfg(test)]
    pub(crate) fn sample(&self, x: i32, bias: u32) -> Option<Pixel> {
        match &self.0 {
            Row::One(row) => row.sample(x, bias),
            Row::Mixed {
                lifted,
                resting,
                lift,
            } => {
                let pixel = mixed_pixel(lifted.as_ref(), resting.as_ref(), *lift, x, bias);
                (pixel.a > 0).then_some(pixel)
            }
        }
    }

    /// Whether this row is a crossfade between the two shadows.
    #[cfg(test)]
    pub(crate) const fn is_mixed(&self) -> bool {
        matches!(self.0, Row::Mixed { .. })
    }

    /// Whether this row's ramps are read from the straight-edge profile.
    #[cfg(test)]
    pub(crate) fn is_straight(&self) -> bool {
        matches!(&self.0, Row::One(row) if row.straight_edge().is_some())
    }

    /// Blend this row's shadow over `dst`, whose first pixel is column
    /// `first_x`, reporting how many pixels it darkened.
    pub fn blend_into(&self, dst: &mut [Pixel], first_x: i32, dither: DitherRow) -> u64 {
        match &self.0 {
            Row::One(row) => row.blend_into(dst, first_x, dither),
            Row::Mixed {
                lifted,
                resting,
                lift,
            } => blend_mixed(
                lifted.as_ref(),
                resting.as_ref(),
                *lift,
                (dst, first_x),
                dither,
            ),
        }
    }
}

/// One row of a surface's shadow at one level.
struct LevelRow<'a> {
    shadow: &'a DropShadow,
    left: i32,
    right: i32,
    /// How far in from each side the shadow varies column by column: past
    /// it, the row is the vertical term alone beneath the caster and nothing
    /// at all across it.
    arc: u32,
    /// The kernel's vertical term for this row, in [`SHADOW_ONE`] units.
    v: u32,
    /// The row of the corner tile the top corners read here, then the one the
    /// bottom corners read, each empty where this row misses them. A left
    /// corner reads its row from the footprint's left edge, a right one
    /// mirrored from its right.
    notches: [&'a [u16]; 2],
    /// Where the row crosses the caster, its silhouette and the row within
    /// it, which the shadow gives way to.
    inside: Option<(Option<RoundRect>, u32)>,
    opacity: u8,
}

impl LevelRow<'_> {
    /// The shadow's weight at column `x`, `0` where it draws nothing.
    fn weight(&self, x: i32) -> u8 {
        let covered = self.covered(x);
        if covered == u8::MAX {
            return 0;
        }
        let h = self
            .shadow
            .cum(i64::from(x) - i64::from(self.left))
            .saturating_sub(self.shadow.cum(i64::from(x) - i64::from(self.right)));
        let full = (u64::from(h) * u64::from(self.v) + u64::from(SHADOW_ONE / 2)) >> SHADOW_SHIFT;
        let reach = i64::from(self.shadow.reach);
        let from_left = i64::from(x) - i64::from(self.left) + reach;
        let from_right = i64::from(self.right) + reach - 1 - i64::from(x);
        let notches: u64 = self
            .notches
            .iter()
            .map(|row| notch_at(row, from_left) + notch_at(row, from_right))
            .sum();
        strength(full.saturating_sub(notches), covered, self.opacity)
    }

    /// How much of column `x` of this row the silhouette covers.
    fn covered(&self, x: i32) -> u8 {
        let Some((silhouette, ly)) = self.inside else {
            return 0;
        };
        let Ok(lx) = u32::try_from(i64::from(x) - i64::from(self.left)) else {
            return 0;
        };
        if x >= self.right {
            return 0;
        }
        silhouette.map_or(u8::MAX, |silhouette| silhouette.coverage(lx, ly))
    }

    /// The shadow's own pixel at column `x`, or `None` where it draws
    /// nothing.
    #[cfg(test)]
    fn sample(&self, x: i32, bias: u32) -> Option<Pixel> {
        let weight = self.weight(x);
        (weight > 0).then(|| self.shadow.color.scale_alpha_biased(weight, bias))
    }

    /// The columns this row can darken.
    fn span(&self) -> (i64, i64) {
        let reach = i64::from(self.shadow.reach);
        (i64::from(self.left) - reach, i64::from(self.right) + reach)
    }

    /// Across the caster, the columns between its arcs, which the silhouette
    /// covers whole so no shadow is laid there; `None` beneath it.
    fn interior(&self) -> Option<(i64, i64)> {
        let arc = i64::from(self.arc);
        self.inside
            .map(|_| (i64::from(self.left) + arc, i64::from(self.right) - arc))
    }

    /// Blend this row's shadow over `dst`, whose first pixel is column
    /// `first_x`, reporting how many pixels it darkened.
    ///
    /// Only the columns a ramp, a corner or an arc can reach are evaluated one
    /// by one. Below the caster, the span between the corners is the kernel's
    /// vertical term alone, so it is one solid blend at the same rounding.
    fn blend_into(&self, dst: &mut [Pixel], first_x: i32, dither: DitherRow) -> u64 {
        let (left, right) = (i64::from(self.left), i64::from(self.right));
        let arc = i64::from(self.arc);
        let (lo, hi) = self.span();
        // A row is laid a segment at a time, and most segments are nowhere near
        // a given shadow, or wholly across the surface that casts it.
        let start = i64::from(first_x);
        let end = start.saturating_add(i64::try_from(dst.len()).unwrap_or(i64::MAX));
        if end <= lo
            || start >= hi
            || (self.inside.is_some() && start >= left + arc && end <= right - arc)
        {
            return 0;
        }
        if let Some(edge) = self.straight_edge() {
            return self.blend_straight(dst, first_x, dither, edge);
        }
        let mut blended = 0u64;
        let mut per_pixel = |dst: &mut [Pixel], from: i64, until: i64| {
            for (dst, x) in clipped(dst, first_x, from, until) {
                blended += darken(
                    dst,
                    self.shadow.color,
                    self.weight(x),
                    dither.bias(x.cast_unsigned()),
                );
            }
        };
        if left + arc >= right - arc {
            per_pixel(dst, lo, hi);
            return blended;
        }
        per_pixel(dst, lo, left + arc);
        per_pixel(dst, right - arc, hi);
        if self.inside.is_none() {
            let weight = strength(u64::from(self.v), 0, self.opacity);
            if let Some((span, column)) = middle(dst, first_x, left + arc, right - arc) {
                if weight > 0 {
                    blend_solid_span(span, self.shadow.color, weight, dither, column);
                    blended += u64::try_from(span.len()).unwrap_or(u64::MAX);
                }
            }
        }
        blended
    }

    /// The straight-edge profile, where this row's ramps can be read from it:
    /// across the caster clear of its corners, with the vertical term whole,
    /// beside a caster at least as wide as the reach, so each ramp sees only
    /// its own edge.
    fn straight_edge(&self) -> Option<&[u8]> {
        let wide = i64::from(self.right) - i64::from(self.left) >= i64::from(self.shadow.reach);
        let straight = self.inside.is_some()
            && self.arc == 0
            && self.v == SHADOW_ONE
            && self.notches.iter().all(|row| row.is_empty())
            && wide;
        (straight && !self.shadow.edge.is_empty()).then_some(self.shadow.edge.as_slice())
    }

    /// [`blend_into`](Self::blend_into) for a row [`straight_edge`](Self::straight_edge)
    /// answers: the ramp beside each edge laid from `edge`, bit for bit what
    /// the general evaluation would weigh it at.
    fn blend_straight(
        &self,
        dst: &mut [Pixel],
        first_x: i32,
        dither: DitherRow,
        edge: &[u8],
    ) -> u64 {
        let reach = i64::from(self.shadow.reach);
        let (left, right) = (i64::from(self.left), i64::from(self.right));
        let Some((leading, trailing)) = edge.split_at_checked(edge.len() / 2) else {
            return 0;
        };
        let mut blended = 0u64;
        for (from, profile) in [(left - reach, leading), (right, trailing)] {
            for (dst, x) in clipped(dst, first_x, from, from + reach) {
                let level = usize::try_from(i64::from(x) - from)
                    .ok()
                    .and_then(|at| profile.get(at))
                    .copied()
                    .unwrap_or(0);
                let weight = div255(u32::from(level) * u32::from(self.opacity));
                blended += darken(
                    dst,
                    self.shadow.color,
                    weight,
                    dither.bias(x.cast_unsigned()),
                );
            }
        }
        blended
    }
}

/// The biweight kernel `((ρ+1)² − t²)²` over `-ρ..=ρ`, as its running sum in
/// [`SHADOW_ONE`] units — smooth, compactly supported, and nowhere zero
/// inside its reach. `None` for no reach, or when the allocator refuses the
/// table.
fn kernel_ramp(reach: u32) -> Option<Vec<u32>> {
    if reach == 0 {
        return None;
    }
    let span = u64::from(reach) + 1;
    let weight = |t: i64| {
        let t = t.unsigned_abs();
        let base = span * span - t * t;
        base * base
    };
    let reach_i = i64::from(reach);
    let total: u64 = (-reach_i..=reach_i).map(weight).sum();
    let taps = usize::try_from(reach)
        .ok()?
        .checked_mul(2)?
        .checked_add(1)?;
    let mut ramp = fallible::filled(taps, 0u32)?;
    let mut running = 0u64;
    for (slot, t) in ramp.iter_mut().zip(-reach_i..=reach_i) {
        running += weight(t);
        let scaled = (running * u64::from(SHADOW_ONE) + total / 2) / total;
        *slot = u32::try_from(scaled).unwrap_or(SHADOW_ONE);
    }
    Some(ramp)
}

/// The top-left notch of a `radius`-rounded corner — the part of the corner
/// square outside the arc, from the shared rounded-rectangle coverage —
/// blurred by the kernel whose running sum is `ramp`.
///
/// The blur is two separable passes over the notch, with the kernel's taps
/// taken as the differences of `ramp`, so a tile subtracts exactly what the
/// rectangle's own `H·V` term counted there. `None` when the allocator
/// refuses any of the three buffers.
fn notch_tile(ramp: &[u32], radius: u32) -> Option<Tile> {
    let r = usize::try_from(radius).ok().filter(|r| *r > 0)?;
    let taps = ramp.len();
    let side = r.checked_add(taps.checked_sub(1)?)?;
    let tap = |index: usize| -> u64 {
        let before = index
            .checked_sub(1)
            .and_then(|i| ramp.get(i))
            .copied()
            .unwrap_or(0);
        u64::from(
            ramp.get(index)
                .copied()
                .unwrap_or(SHADOW_ONE)
                .saturating_sub(before),
        )
    };
    let square = radius.saturating_mul(2);
    let corner = RoundRect {
        width: square,
        height: square,
        radius,
    };
    let mut notch = fallible::filled(r.checked_mul(r)?, 0u64)?;
    for v in 0..radius {
        for u in 0..radius {
            let at = usize::try_from(v).ok()? * r + usize::try_from(u).ok()?;
            if let Some(slot) = notch.get_mut(at) {
                *slot = u64::from(255 - corner.coverage(u, v));
            }
        }
    }
    // Horizontal pass: each notch row spread across the tile's columns.
    let mut across = fallible::filled(r.checked_mul(side)?, 0u64)?;
    for v in 0..r {
        for a in 0..side {
            let from = a.saturating_sub(taps - 1);
            let sum: u64 = (from..=a.min(r - 1))
                .map(|u| notch.get(v * r + u).copied().unwrap_or(0) * tap(a - u))
                .sum();
            if let Some(slot) = across.get_mut(v * side + a) {
                *slot = sum;
            }
        }
    }
    // Vertical pass, and back from `255 · ONE²` to `ONE` units.
    let unit = 255 * u64::from(SHADOW_ONE);
    let mut values = fallible::filled(side.checked_mul(side)?, 0u16)?;
    for b in 0..side {
        let from = b.saturating_sub(taps - 1);
        for a in 0..side {
            let sum: u64 = (from..=b.min(r - 1))
                .map(|v| across.get(v * side + a).copied().unwrap_or(0) * tap(b - v))
                .sum();
            if let Some(slot) = values.get_mut(b * side + a) {
                *slot = u16::try_from((sum + unit / 2) / unit).unwrap_or(u16::MAX);
            }
        }
    }
    Some(Tile {
        radius,
        side: u32::try_from(side).ok()?,
        values,
    })
}

/// Column `offset` of a tile row, `0` outside it.
fn notch_at(row: &[u16], offset: i64) -> u64 {
    usize::try_from(offset)
        .ok()
        .and_then(|index| row.get(index))
        .map_or(0, |&value| u64::from(value))
}

/// The pixel a shadow between its levels lays at column `x`: each level's own
/// pixel scaled by its share of `lift`, summed.
///
/// The two shares sum to one, so the sum is a premultiplied pixel no more
/// opaque than the darker level's, and at either end it is that level's own.
fn mixed_pixel(
    lifted: Option<&LevelRow<'_>>,
    resting: Option<&LevelRow<'_>>,
    lift: u8,
    x: i32,
    bias: u32,
) -> Pixel {
    let part = |row: Option<&LevelRow<'_>>, share: u8| {
        row.map_or(Pixel::TRANSPARENT, |row| {
            let weight = div255(u32::from(row.weight(x)) * u32::from(share));
            row.shadow.color.scale_alpha_biased(weight, bias)
        })
    };
    let (a, b) = (part(lifted, lift), part(resting, u8::MAX - lift));
    Pixel {
        r: a.r.saturating_add(b.r),
        g: a.g.saturating_add(b.g),
        b: a.b.saturating_add(b.b),
        a: a.a.saturating_add(b.a),
    }
}

/// A shadow between its levels laid over `dst`: every column either level can
/// reach is weighed one by one — across the caster only those its arcs and
/// ramps reach — which only a surface easing between levels pays, and only
/// while it eases.
fn blend_mixed(
    lifted: Option<&LevelRow<'_>>,
    resting: Option<&LevelRow<'_>>,
    lift: u8,
    (dst, first_x): (&mut [Pixel], i32),
    dither: DitherRow,
) -> u64 {
    let rows = [lifted, resting];
    let lo = rows.iter().flatten().map(|row| row.span().0).min();
    let hi = rows.iter().flatten().map(|row| row.span().1).max();
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return 0;
    };
    let mut blended = 0u64;
    let mut weigh = |dst: &mut [Pixel], from: i64, until: i64| {
        for (dst, x) in clipped(dst, first_x, from, until) {
            let bias = dither.bias(x.cast_unsigned());
            let pixel = mixed_pixel(lifted, resting, lift, x, bias);
            if pixel.a > 0 {
                *dst = pixel.over_biased(*dst, bias);
                blended += 1;
            }
        }
    };
    // Both levels share the caster's silhouette, so they share its interior.
    match rows.iter().flatten().find_map(|row| row.interior()) {
        Some((from, until)) if from < until => {
            weigh(dst, lo, from);
            weigh(dst, until, hi);
        }
        _ => weigh(dst, lo, hi),
    }
    blended
}

/// Lay `color` over `dst` at `weight`, rounding at `bias`, reporting whether
/// it darkened anything.
fn darken(dst: &mut Pixel, color: Pixel, weight: u8, bias: u32) -> u64 {
    if weight == 0 {
        return 0;
    }
    *dst = color.laid_over(*dst, weight, bias);
    1
}

/// A shadow strength of `intensity` [`SHADOW_ONE`] units at a pixel the
/// silhouette covers by `covered`, drawn at `opacity`, as the weight the
/// shadow colour is laid at.
fn strength(intensity: u64, covered: u8, opacity: u8) -> u8 {
    let outside = u32::from(u8::MAX - covered);
    div255(u32::from(div255(u32::from(level(intensity)) * outside)) * u32::from(opacity))
}

/// A shadow strength of `intensity` [`SHADOW_ONE`] units on the `0..=255`
/// scale.
fn level(intensity: u64) -> u8 {
    let level =
        (intensity.min(u64::from(SHADOW_ONE)) * 255 + u64::from(SHADOW_ONE / 2)) >> SHADOW_SHIFT;
    u8::try_from(level).unwrap_or(u8::MAX)
}

/// The straight-edge profile ([`DropShadow::edge`]) from the kernel's running
/// sum: beside a left edge the rectangle's horizontal term is the sum so far,
/// beside a right one what remains of it. `None` when the allocator refuses
/// the table.
fn edge_profile(ramp: &[u32], reach: u32) -> Option<Vec<u8>> {
    let reach = usize::try_from(reach).ok()?;
    let mut edge = fallible::filled(reach.checked_mul(2)?, 0u8)?;
    for (at, slot) in edge.iter_mut().enumerate() {
        let sum = u64::from(*ramp.get(at)?);
        *slot = level(if at < reach {
            sum
        } else {
            u64::from(SHADOW_ONE).saturating_sub(sum)
        });
    }
    Some(edge)
}

/// The pixels of `dst` — which starts at column `first_x` — that lie in
/// columns `from..until`, each with its column.
fn clipped(
    dst: &mut [Pixel],
    first_x: i32,
    from: i64,
    until: i64,
) -> impl Iterator<Item = (&mut Pixel, i32)> {
    let start = i64::from(first_x);
    let len = i64::try_from(dst.len()).unwrap_or(i64::MAX);
    let lo = (from - start).clamp(0, len);
    let hi = (until - start).clamp(lo, len);
    let column = i32::try_from(start + lo).unwrap_or(i32::MAX);
    let span = usize::try_from(lo)
        .ok()
        .zip(usize::try_from(hi).ok())
        .and_then(|(lo, hi)| dst.get_mut(lo..hi))
        .unwrap_or_default();
    span.iter_mut().zip(column..)
}

/// The part of `dst` — which starts at column `first_x` — in columns
/// `from..until`, with the column it starts at as the dither reads it
/// (wrapped left of the surface), or `None` where they do not meet.
fn middle(dst: &mut [Pixel], first_x: i32, from: i64, until: i64) -> Option<(&mut [Pixel], u32)> {
    let start = i64::from(first_x);
    let len = i64::try_from(dst.len()).ok()?;
    let lo = (from - start).clamp(0, len);
    let hi = (until - start).clamp(0, len);
    if lo >= hi {
        return None;
    }
    let column = i32::try_from(start + lo).ok()?.cast_unsigned();
    let span = dst.get_mut(usize::try_from(lo).ok()?..usize::try_from(hi).ok()?)?;
    Some((span, column))
}

#[cfg(test)]
#[path = "drop_shadow_tests.rs"]
mod tests;
