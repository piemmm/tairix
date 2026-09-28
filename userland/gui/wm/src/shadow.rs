//! Soft drop shadows under floating surfaces.
//!
//! The light is overhead, so a surface's shadow is its own silhouette dropped
//! by the theme's reach `ρ` and softened by a smooth compact kernel of the same
//! reach: nothing shows above the top edge, `ρ` beside each side, and `2ρ`
//! below. The shadow only darkens what is *outside* the silhouette, so a
//! translucent or frosted surface never shows its own shadow through itself.
//!
//! # Exact by linearity, evaluated where it varies
//!
//! Blurring is linear, and a rounded silhouette is its rectangle less four
//! corner notches. The rectangle's blur is separable — a horizontal and a
//! vertical cumulative kernel, `H(x)·V(y)` — and each notch's blur is a small
//! tile the kit computes once per corner radius and mirrors to all four
//! corners, the supersampled corner coverage being symmetric. So a shadow
//! pixel is two table lookups, a multiply, and at most four tile reads, and
//! nothing per window is retained at all.
//!
//! A below-window row is the kernel's vertical term alone across its middle,
//! so that span is one solid blend; per-pixel work is confined to the columns
//! a ramp or a corner can reach, and beside a straight edge the ramp is read
//! from a profile the kit computes once.

use alloc::vec::Vec;

use tairix_raster::{blend_solid_span, round_rect_coverage, DitherRow};
use tairix_theme::Theme;
use tairix_util::fallible;

use crate::color::{div255, Color, Pixel};
use crate::geometry::{Rect, Scale};
use crate::window::WindowShape;

/// The fixed-point one both the kernel's running sum and a shadow's strength
/// are carried in.
const ONE: u32 = 1 << 15;

/// The furthest a shadow reaches, in physical pixels.
///
/// A bound on theme data, not a capacity: a shadow reaching further than this
/// has stopped reading as one, and the kernel's integer arithmetic is sized
/// for it.
const MAX_REACH: u32 = 255;

/// How far a shadow cast at `scale` under `theme` reaches, in physical pixels:
/// `0` where the theme casts none.
fn reach(scale: Scale, theme: &Theme) -> u32 {
    if theme.palette().drop_shadow.a == 0 {
        return 0;
    }
    scale
        .scale_length(theme.metrics().drop_shadow_reach)
        .min(MAX_REACH)
}

/// `bounds` grown by what a shadow of `reach` darkens: nothing above it,
/// `reach` beside each side and twice that below.
pub(crate) fn spread(bounds: Rect, reach: u32) -> Rect {
    if reach == 0 {
        return bounds;
    }
    Rect::new(
        bounds.left().saturating_sub_unsigned(reach),
        bounds.top(),
        bounds.width.saturating_add(reach.saturating_mul(2)),
        bounds.height.saturating_add(reach.saturating_mul(2)),
    )
}

/// The screen area a surface at `bounds` covers once its shadow at `scale`
/// under `theme` is counted: its own rectangle and everything the shadow
/// darkens beside and below it.
///
/// The one footprint definition: the compositor marks it when a caster
/// changes, and a caller that must keep clear of every pixel a window can
/// change — a test sampling the bare desktop beside one — reads it here
/// rather than guessing a margin.
#[must_use]
pub fn shadow_footprint(bounds: Rect, scale: Scale, theme: &Theme) -> Rect {
    spread(bounds, reach(scale, theme))
}

/// One corner's notch, blurred: the tile a rounded silhouette's shadow
/// subtracts from its rectangle's at each corner.
struct Tile {
    /// The corner radius the notch is cut by.
    radius: u32,
    /// The tile's side: the notch widened by the kernel's reach either way.
    side: u32,
    /// `side × side` strengths in [`ONE`] units, row-major, laid out for the
    /// top-left corner; the other three read it mirrored.
    values: Vec<u16>,
}

/// Everything shadows are drawn from on one output, rebuilt whenever its scale
/// or theme changes.
pub(crate) struct ShadowKit {
    /// The reach, in physical pixels; `0` draws no shadow at all.
    reach: u32,
    /// The shadow colour at its darkest, premultiplied.
    color: Pixel,
    /// The kernel's running sum over `-reach..=reach`, in [`ONE`] units.
    ramp: Vec<u32>,
    /// The shadow's level across a straight edge the vertical term reaches in
    /// full: `reach` columns leading up to a left edge, then `reach` leading
    /// away from a right one. Empty where the allocator refused it, which
    /// leaves every pixel to the general evaluation.
    edge: Vec<u8>,
    /// A notch tile per corner radius a caster has needed.
    tiles: Vec<Tile>,
    /// The radii the theme itself rounds by, whose tiles are kept whatever is
    /// on screen.
    standard: [u32; 2],
}

impl ShadowKit {
    /// The kit for an output at `scale` under `theme`: its kernel, and the
    /// notch tiles for the window and popup radii the theme rounds by.
    ///
    /// An allocation refused anywhere leaves the kit casting no shadows, which
    /// costs the desktop its depth and never a frame.
    pub(crate) fn new(scale: Scale, theme: &Theme) -> Self {
        let reach = reach(scale, theme);
        let metrics = theme.metrics();
        let standard = [
            scale.scale_length(metrics.window_corner_radius),
            scale.scale_length(metrics.popup_corner_radius),
        ];
        let Some(ramp) = kernel_ramp(reach) else {
            return Self::none();
        };
        let edge = edge_profile(&ramp, reach).unwrap_or_default();
        let mut kit = Self {
            reach,
            color: Color::from(theme.palette().drop_shadow).premultiply(),
            ramp,
            edge,
            tiles: Vec::new(),
            standard,
        };
        for radius in standard {
            kit.ensure_tile(radius);
        }
        kit
    }

    /// A kit that draws nothing.
    pub(crate) const fn none() -> Self {
        Self {
            reach: 0,
            color: Pixel::TRANSPARENT,
            ramp: Vec::new(),
            edge: Vec::new(),
            tiles: Vec::new(),
            standard: [0; 2],
        }
    }

    /// How far a shadow reaches on this output, `0` for none.
    pub(crate) const fn reach(&self) -> u32 {
        self.reach
    }

    /// Drop every tile but the theme's own radii and those `in_use` answers
    /// for.
    ///
    /// Tiles for other radii exist because a window too small for its corners
    /// rounds by less, so what this keeps is bounded by what is on screen
    /// rather than by every window that ever cast a shadow.
    pub(crate) fn retain_tiles(&mut self, in_use: impl Fn(u32) -> bool) {
        let standard = self.standard;
        self.tiles
            .retain(|tile| standard.contains(&tile.radius) || in_use(tile.radius));
    }

    /// Build the tile for `radius` unless it is already held. A tile the
    /// allocator refuses is simply absent: that shadow is drawn without its
    /// corner notches, a little dark under the corners, rather than not at
    /// all.
    pub(crate) fn ensure_tile(&mut self, radius: u32) {
        if radius == 0 || self.reach == 0 || self.tile(radius).is_some() {
            return;
        }
        if let Some(tile) = notch_tile(&self.ramp, radius) {
            if self.tiles.try_reserve(1).is_ok() {
                self.tiles.push(tile);
            }
        }
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
            .unwrap_or(ONE)
    }

    /// The shadow the silhouette `shape` at `bounds`, drawn at `opacity`,
    /// casts across screen row `y`, or `None` where it casts nothing there.
    pub(crate) fn row(
        &self,
        bounds: Rect,
        shape: Option<WindowShape>,
        opacity: u8,
        y: i32,
    ) -> Option<ShadowRow<'_>> {
        if self.reach == 0 || opacity == 0 || bounds.is_empty() {
            return None;
        }
        let reach = i64::from(self.reach);
        let (top, bottom) = (i64::from(bounds.top()), i64::from(bounds.bottom()));
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
        let radius = shape.map_or(0, WindowShape::corner_reach);
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
            (shape, ly)
        });
        // Beneath the window the corners' reach varies per column; across it,
        // only an arc the row crosses leaves the silhouette's edge uneven.
        let arc = match inside {
            None => radius.saturating_add(self.reach),
            Some((shape, ly)) => shape
                .filter(|shape| shape.clips_row(ly))
                .map_or(0, |_| radius),
        };
        Some(ShadowRow {
            kit: self,
            left: bounds.left(),
            right: bounds.right(),
            arc,
            v,
            notches,
            inside,
            opacity,
        })
    }
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

/// The biweight kernel `((ρ+1)² − t²)²` over `-ρ..=ρ`, as its running sum in
/// [`ONE`] units — smooth, compactly supported, and nowhere zero inside its
/// reach. `None` for no reach, or when the allocator refuses the table.
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
        let scaled = (running * u64::from(ONE) + total / 2) / total;
        *slot = u32::try_from(scaled).unwrap_or(ONE);
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
                .unwrap_or(ONE)
                .saturating_sub(before),
        )
    };
    let mut notch = fallible::filled(r.checked_mul(r)?, 0u64)?;
    for v in 0..radius {
        for u in 0..radius {
            let square = radius.saturating_mul(2);
            let covered = round_rect_coverage(u, v, square, square, radius);
            let at = usize::try_from(v).ok()? * r + usize::try_from(u).ok()?;
            if let Some(slot) = notch.get_mut(at) {
                *slot = u64::from(255 - covered);
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
    let unit = 255 * u64::from(ONE);
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

/// One screen row of a window's shadow, resolved once for the whole row.
pub(crate) struct ShadowRow<'a> {
    kit: &'a ShadowKit,
    left: i32,
    right: i32,
    /// How far in from each side the shadow varies column by column: past
    /// it, the row is the vertical term alone beneath the window and nothing
    /// at all across it.
    arc: u32,
    /// The kernel's vertical term for this row, in [`ONE`] units.
    v: u32,
    /// The row of the corner tile the top corners read here, then the one the
    /// bottom corners read, each empty where this row misses them. A left
    /// corner reads its row from the footprint's left edge, a right one
    /// mirrored from its right.
    notches: [&'a [u16]; 2],
    /// Where the row crosses the window, the silhouette and the row within
    /// it, which the shadow gives way to.
    inside: Option<(Option<WindowShape>, u32)>,
    opacity: u8,
}

impl ShadowRow<'_> {
    /// The shadow's weight at screen column `x`, `0` where it draws nothing.
    fn weight(&self, x: i32) -> u8 {
        let covered = self.covered(x);
        if covered == u8::MAX {
            return 0;
        }
        let h = self
            .kit
            .cum(i64::from(x) - i64::from(self.left))
            .saturating_sub(self.kit.cum(i64::from(x) - i64::from(self.right)));
        let full = (u64::from(h) * u64::from(self.v) + u64::from(ONE / 2)) >> 15;
        let reach = i64::from(self.kit.reach);
        let from_left = i64::from(x) - i64::from(self.left) + reach;
        let from_right = i64::from(self.right) + reach - 1 - i64::from(x);
        let notches: u64 = self
            .notches
            .iter()
            .map(|row| notch_at(row, from_left) + notch_at(row, from_right))
            .sum();
        strength(full.saturating_sub(notches), covered, self.opacity)
    }

    /// How much of screen column `x` of this row the silhouette covers.
    fn covered(&self, x: i32) -> u8 {
        let Some((shape, ly)) = self.inside else {
            return 0;
        };
        let Ok(lx) = u32::try_from(i64::from(x) - i64::from(self.left)) else {
            return 0;
        };
        if x >= self.right {
            return 0;
        }
        shape.map_or(u8::MAX, |shape| shape.coverage(lx, ly))
    }

    /// The shadow's own pixel at screen column `x` — what a layer bakes under
    /// the window's body there — or `None` where it draws nothing.
    pub(crate) fn sample(&self, x: i32, bias: u32) -> Option<Pixel> {
        let weight = self.weight(x);
        (weight > 0).then(|| self.kit.color.scale_alpha_biased(weight, bias))
    }

    /// Blend this row's shadow over `dst`, whose first pixel is screen column
    /// `first_x`, reporting how many pixels it darkened.
    ///
    /// Only the columns a ramp, a corner or an arc can reach are evaluated one
    /// by one. Below the window, the span between the corners is the kernel's
    /// vertical term alone, so it is one solid blend at the same rounding.
    pub(crate) fn blend_into(&self, dst: &mut [Pixel], first_x: i32, dither: DitherRow) -> u64 {
        let reach = i64::from(self.kit.reach);
        let (left, right) = (i64::from(self.left), i64::from(self.right));
        let arc = i64::from(self.arc);
        let (lo, hi) = (left - reach, right + reach);
        // A row is laid a segment at a time, and most segments are nowhere near
        // a given shadow, or wholly across the window that casts it.
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
                blended += self.darken(dst, self.weight(x), dither.bias(x.cast_unsigned()));
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
                    blend_solid_span(span, self.kit.color, weight, dither, column);
                    blended += u64::try_from(span.len()).unwrap_or(u64::MAX);
                }
            }
        }
        blended
    }

    /// Lay the shadow colour over `dst` at `weight`, rounding at `bias`,
    /// reporting whether it darkened anything.
    fn darken(&self, dst: &mut Pixel, weight: u8, bias: u32) -> u64 {
        if weight == 0 {
            return 0;
        }
        *dst = self
            .kit
            .color
            .scale_alpha_biased(weight, bias)
            .over_biased(*dst, bias);
        1
    }

    /// The kit's straight-edge profile, where this row's ramps can be read
    /// from it: across the window clear of its corners, with the vertical term
    /// whole, beside a window at least as wide as the reach, so each ramp sees
    /// only its own edge.
    fn straight_edge(&self) -> Option<&[u8]> {
        let wide = i64::from(self.right) - i64::from(self.left) >= i64::from(self.kit.reach);
        let straight = self.inside.is_some()
            && self.arc == 0
            && self.v == ONE
            && self.notches.iter().all(|row| row.is_empty())
            && wide;
        (straight && !self.kit.edge.is_empty()).then_some(self.kit.edge.as_slice())
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
        let reach = i64::from(self.kit.reach);
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
                blended += self.darken(dst, weight, dither.bias(x.cast_unsigned()));
            }
        }
        blended
    }
}

/// A shadow strength of `intensity` [`ONE`] units at a pixel the silhouette
/// covers by `covered`, drawn at `opacity`, as the weight the shadow colour
/// is laid at.
fn strength(intensity: u64, covered: u8, opacity: u8) -> u8 {
    let outside = u32::from(u8::MAX - covered);
    div255(u32::from(div255(u32::from(level(intensity)) * outside)) * u32::from(opacity))
}

/// A shadow strength of `intensity` [`ONE`] units on the `0..=255` scale.
fn level(intensity: u64) -> u8 {
    let level = (intensity.min(u64::from(ONE)) * 255 + u64::from(ONE / 2)) >> 15;
    u8::try_from(level).unwrap_or(u8::MAX)
}

/// The kit's straight-edge profile ([`ShadowKit::edge`]) from the kernel's
/// running sum: beside a left edge the rectangle's horizontal term is the sum
/// so far, beside a right one what remains of it. `None` when the allocator
/// refuses the table.
fn edge_profile(ramp: &[u32], reach: u32) -> Option<Vec<u8>> {
    let reach = usize::try_from(reach).ok()?;
    let mut edge = fallible::filled(reach.checked_mul(2)?, 0u8)?;
    for (at, slot) in edge.iter_mut().enumerate() {
        let sum = u64::from(*ramp.get(at)?);
        *slot = level(if at < reach {
            sum
        } else {
            u64::from(ONE).saturating_sub(sum)
        });
    }
    Some(edge)
}

/// The pixels of `dst` — which starts at screen column `first_x` — that lie in
/// screen columns `from..until`, each with its column.
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

/// The part of `dst` — which starts at screen column `first_x` — in screen
/// columns `from..until`, with the screen column it starts at, or `None` where
/// they do not meet.
fn middle(dst: &mut [Pixel], first_x: i32, from: i64, until: i64) -> Option<(&mut [Pixel], u32)> {
    let start = i64::from(first_x);
    let len = i64::try_from(dst.len()).ok()?;
    let lo = (from - start).clamp(0, len);
    let hi = (until - start).clamp(0, len);
    if lo >= hi {
        return None;
    }
    let column = u32::try_from(start + lo).ok()?;
    let span = dst.get_mut(usize::try_from(lo).ok()?..usize::try_from(hi).ok()?)?;
    Some((span, column))
}

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod tests;
