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
//!
//! # Two levels
//!
//! A decorated window resting behind the family the user is working in casts
//! a lesser shadow than the window in front and every floating surface:
//! lighter and shorter, so it reads as standing lower. The kit holds a
//! `Level` for each. A window easing between them after a change of focus
//! casts both at once, each weighted by how far it has risen, so the change is
//! a crossfade between two exact shadows rather than a guess at one between.

use alloc::vec::Vec;

use tairix_raster::{blend_solid_span, round_rect_coverage, DitherRow};
use tairix_theme::{Fade, Theme};
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

/// How high a window's shadow stands: [`LIFTED`](Self::LIFTED) for the window
/// being worked in and every floating surface,
/// [`RESTING`](Self::RESTING) for a decorated window behind another family,
/// and between the two while it eases from one to the other.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Elevation(pub(crate) u8);

impl Elevation {
    /// The shadow of a surface standing in front.
    pub(crate) const LIFTED: Self = Self(u8::MAX);
    /// The shadow of a decorated window resting behind the family in front.
    pub(crate) const RESTING: Self = Self(0);
}

/// A window's shadow easing between the levels: the level drawn, the one it
/// is going to, and the fade carrying it there.
///
/// A change of target waits for the next [`step`](Self::step) to start
/// easing, from where the shadow is drawn then — the compositor reads no
/// clock, so the first frame after a change is where its ease begins.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Lift {
    drawn: u8,
    target: u8,
    fade: Fade,
}

impl Lift {
    /// A shadow standing still at `elevation`.
    pub(crate) fn at(elevation: Elevation) -> Self {
        let Elevation(level) = elevation;
        Self {
            drawn: level,
            target: level,
            fade: Fade::start(0, 0, level, level),
        }
    }

    /// The level the shadow is drawn at.
    pub(crate) const fn drawn(&self) -> Elevation {
        Elevation(self.drawn)
    }

    /// Send the shadow toward `elevation`: eased from the next step, or
    /// there at once when `at_once`, for a window nobody can watch rise.
    pub(crate) fn aim(&mut self, elevation: Elevation, at_once: bool) {
        if at_once {
            *self = Self::at(elevation);
        } else {
            self.target = elevation.0;
        }
    }

    /// Advance the ease to `now_ns`, starting one of `duration_ms` toward a
    /// target that changed, and answer whether the level drawn moved. A
    /// zero duration — reduced motion — arrives on this step.
    pub(crate) fn step(&mut self, now_ns: u64, duration_ms: u16) -> bool {
        if self.target != self.fade.target() {
            self.fade = Fade::start(now_ns, duration_ms, self.drawn, self.target);
        }
        let level = self.fade.strength(now_ns);
        if level == self.fade.target() {
            self.fade.settle();
        }
        let moved = level != self.drawn;
        self.drawn = level;
        moved
    }

    /// Whether the shadow stands at the level it is aimed at, with no ease
    /// begun or running.
    pub(crate) const fn still(&self) -> bool {
        self.target == self.fade.target() && !self.fade.running()
    }

    /// Nanoseconds until the next step is owed, or `None` for a shadow at
    /// rest: at once for a change no step has started.
    pub(crate) fn next_frame_in(&self, now_ns: u64) -> Option<u64> {
        if self.target != self.fade.target() {
            return Some(0);
        }
        self.fade.next_frame_in(now_ns)
    }
}

/// How far a shadow of logical `reach`, at its darkest `alpha`, reaches at
/// `scale`, in physical pixels: `0` where it casts none.
fn physical_reach(scale: Scale, alpha: u8, reach: u32) -> u32 {
    if alpha == 0 {
        return 0;
    }
    scale.scale_length(reach).min(MAX_REACH)
}

/// How far the further of the two shadows `theme` casts at `scale` reaches,
/// in physical pixels: `0` where it casts none.
fn reach(scale: Scale, theme: &Theme) -> u32 {
    let (palette, metrics) = (theme.palette(), theme.metrics());
    physical_reach(scale, palette.drop_shadow.a, metrics.drop_shadow_reach).max(physical_reach(
        scale,
        palette.inactive_drop_shadow.a,
        metrics.inactive_drop_shadow_reach,
    ))
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
/// darkens beside and below it, at whichever level it stands.
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
/// or theme changes: a [`Level`] for a surface in front and one for a window
/// resting behind it.
pub(crate) struct ShadowKit {
    lifted: Level,
    resting: Level,
}

impl ShadowKit {
    /// The kit for an output at `scale` under `theme`: each level's kernel,
    /// and its notch tiles for the window and popup radii the theme rounds by.
    ///
    /// An allocation refused anywhere leaves that level casting no shadows,
    /// which costs the desktop its depth and never a frame.
    pub(crate) fn new(scale: Scale, theme: &Theme) -> Self {
        let (palette, metrics) = (theme.palette(), theme.metrics());
        let standard = [
            scale.scale_length(metrics.window_corner_radius),
            scale.scale_length(metrics.popup_corner_radius),
        ];
        Self {
            lifted: Level::new(
                scale,
                Color::from(palette.drop_shadow),
                metrics.drop_shadow_reach,
                standard,
            ),
            resting: Level::new(
                scale,
                Color::from(palette.inactive_drop_shadow),
                metrics.inactive_drop_shadow_reach,
                standard,
            ),
        }
    }

    /// How far the further of the two shadows reaches on this output, `0` for
    /// none: the bound every caster's footprint is grown by.
    pub(crate) fn reach(&self) -> u32 {
        self.lifted.reach.max(self.resting.reach)
    }

    /// Drop every tile but the theme's own radii and those `in_use` answers
    /// for, in both levels.
    pub(crate) fn retain_tiles(&mut self, in_use: impl Fn(u32) -> bool) {
        self.lifted.retain_tiles(&in_use);
        self.resting.retain_tiles(&in_use);
    }

    /// Build the tiles for `radius` that are not already held.
    pub(crate) fn ensure_tile(&mut self, radius: u32) {
        self.lifted.ensure_tile(radius);
        self.resting.ensure_tile(radius);
    }

    /// The shadow the silhouette `shape` at `bounds`, drawn at `opacity` and
    /// standing at `elevation`, casts across screen row `y`, or `None` where
    /// it casts nothing there.
    pub(crate) fn row(
        &self,
        bounds: Rect,
        shape: Option<WindowShape>,
        opacity: u8,
        elevation: Elevation,
        y: i32,
    ) -> Option<ShadowRow<'_>> {
        let Elevation(lift) = elevation;
        let lifted = if lift > 0 {
            self.lifted.row(bounds, shape, opacity, y)
        } else {
            None
        };
        let resting = if lift < u8::MAX {
            self.resting.row(bounds, shape, opacity, y)
        } else {
            None
        };
        match (lifted, resting) {
            (None, None) => None,
            (Some(row), None) if lift == u8::MAX => Some(ShadowRow::One(row)),
            (None, Some(row)) if lift == 0 => Some(ShadowRow::One(row)),
            (lifted, resting) => Some(ShadowRow::Mixed {
                lifted,
                resting,
                lift,
            }),
        }
    }

    #[cfg(test)]
    fn level(&self, elevation: Elevation) -> &Level {
        if elevation == Elevation::RESTING {
            &self.resting
        } else {
            &self.lifted
        }
    }

    #[cfg(test)]
    fn level_mut(&mut self, elevation: Elevation) -> &mut Level {
        if elevation == Elevation::RESTING {
            &mut self.resting
        } else {
            &mut self.lifted
        }
    }
}

/// One shadow a surface can cast: its reach, its colour, the kernel it is
/// softened by and the corner tiles it is cut by.
struct Level {
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

impl Level {
    /// The level of a shadow in `color` reaching a logical `reach` at
    /// `scale`, holding the tiles for the `standard` radii. Casts nothing
    /// when the colour darkens nothing, the reach is zero, or the allocator
    /// refuses its kernel.
    fn new(scale: Scale, color: Color, reach: u32, standard: [u32; 2]) -> Self {
        let reach = physical_reach(scale, color.a, reach);
        let Some(ramp) = kernel_ramp(reach) else {
            return Self::none();
        };
        let edge = edge_profile(&ramp, reach).unwrap_or_default();
        let mut level = Self {
            reach,
            color: color.premultiply(),
            ramp,
            edge,
            tiles: Vec::new(),
            standard,
        };
        for radius in standard {
            level.ensure_tile(radius);
        }
        level
    }

    /// A level that draws nothing.
    const fn none() -> Self {
        Self {
            reach: 0,
            color: Pixel::TRANSPARENT,
            ramp: Vec::new(),
            edge: Vec::new(),
            tiles: Vec::new(),
            standard: [0; 2],
        }
    }

    /// Drop every tile but the theme's own radii and those `in_use` answers
    /// for.
    ///
    /// Tiles for other radii exist because a window too small for its corners
    /// rounds by less, so what this keeps is bounded by what is on screen
    /// rather than by every window that ever cast a shadow.
    fn retain_tiles(&mut self, in_use: &impl Fn(u32) -> bool) {
        let standard = self.standard;
        self.tiles
            .retain(|tile| standard.contains(&tile.radius) || in_use(tile.radius));
    }

    /// Build the tile for `radius` unless it is already held. A tile the
    /// allocator refuses is simply absent: that shadow is drawn without its
    /// corner notches, a little dark under the corners, rather than not at
    /// all.
    fn ensure_tile(&mut self, radius: u32) {
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
    /// casts across screen row `y` at this level, or `None` where it casts
    /// nothing there.
    fn row(
        &self,
        bounds: Rect,
        shape: Option<WindowShape>,
        opacity: u8,
        y: i32,
    ) -> Option<LevelRow<'_>> {
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
        Some(LevelRow {
            level: self,
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
pub(crate) enum ShadowRow<'a> {
    /// A shadow standing at one level.
    One(LevelRow<'a>),
    /// A shadow between the levels while its window eases from one to the
    /// other: each level weighted by how far the window has risen, `lift` out
    /// of [`u8::MAX`] toward the lifted one. A level that casts nothing on
    /// this row is absent.
    Mixed {
        /// The lifted level's row.
        lifted: Option<LevelRow<'a>>,
        /// The resting level's row.
        resting: Option<LevelRow<'a>>,
        /// How far toward the lifted level the window has risen.
        lift: u8,
    },
}

impl ShadowRow<'_> {
    /// The shadow's own pixel at screen column `x`, or `None` where it draws
    /// nothing: the oracle a row's blend is held to.
    #[cfg(test)]
    pub(crate) fn sample(&self, x: i32, bias: u32) -> Option<Pixel> {
        match self {
            Self::One(row) => row.sample(x, bias),
            Self::Mixed {
                lifted,
                resting,
                lift,
            } => {
                let pixel = mixed_pixel(lifted.as_ref(), resting.as_ref(), *lift, x, bias);
                (pixel.a > 0).then_some(pixel)
            }
        }
    }

    /// Blend this row's shadow over `dst`, whose first pixel is screen column
    /// `first_x`, reporting how many pixels it darkened.
    pub(crate) fn blend_into(&self, dst: &mut [Pixel], first_x: i32, dither: DitherRow) -> u64 {
        match self {
            Self::One(row) => row.blend_into(dst, first_x, dither),
            Self::Mixed {
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

/// One screen row of a window's shadow at one level.
pub(crate) struct LevelRow<'a> {
    level: &'a Level,
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

impl LevelRow<'_> {
    /// The shadow's weight at screen column `x`, `0` where it draws nothing.
    fn weight(&self, x: i32) -> u8 {
        let covered = self.covered(x);
        if covered == u8::MAX {
            return 0;
        }
        let h = self
            .level
            .cum(i64::from(x) - i64::from(self.left))
            .saturating_sub(self.level.cum(i64::from(x) - i64::from(self.right)));
        let full = (u64::from(h) * u64::from(self.v) + u64::from(ONE / 2)) >> 15;
        let reach = i64::from(self.level.reach);
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

    /// The shadow's own pixel at screen column `x`, or `None` where it draws
    /// nothing.
    #[cfg(test)]
    fn sample(&self, x: i32, bias: u32) -> Option<Pixel> {
        let weight = self.weight(x);
        (weight > 0).then(|| self.level.color.scale_alpha_biased(weight, bias))
    }

    /// The screen columns this row can darken.
    fn span(&self) -> (i64, i64) {
        let reach = i64::from(self.level.reach);
        (i64::from(self.left) - reach, i64::from(self.right) + reach)
    }

    /// Across the window, the columns between its arcs, which the silhouette
    /// covers whole so no shadow is laid there; `None` beneath it.
    fn interior(&self) -> Option<(i64, i64)> {
        let arc = i64::from(self.arc);
        self.inside
            .map(|_| (i64::from(self.left) + arc, i64::from(self.right) - arc))
    }

    /// Blend this row's shadow over `dst`, whose first pixel is screen column
    /// `first_x`, reporting how many pixels it darkened.
    ///
    /// Only the columns a ramp, a corner or an arc can reach are evaluated one
    /// by one. Below the window, the span between the corners is the kernel's
    /// vertical term alone, so it is one solid blend at the same rounding.
    fn blend_into(&self, dst: &mut [Pixel], first_x: i32, dither: DitherRow) -> u64 {
        let (left, right) = (i64::from(self.left), i64::from(self.right));
        let arc = i64::from(self.arc);
        let (lo, hi) = self.span();
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
                blended += darken(
                    dst,
                    self.level.color,
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
                    blend_solid_span(span, self.level.color, weight, dither, column);
                    blended += u64::try_from(span.len()).unwrap_or(u64::MAX);
                }
            }
        }
        blended
    }

    /// The level's straight-edge profile, where this row's ramps can be read
    /// from it: across the window clear of its corners, with the vertical term
    /// whole, beside a window at least as wide as the reach, so each ramp sees
    /// only its own edge.
    fn straight_edge(&self) -> Option<&[u8]> {
        let wide = i64::from(self.right) - i64::from(self.left) >= i64::from(self.level.reach);
        let straight = self.inside.is_some()
            && self.arc == 0
            && self.v == ONE
            && self.notches.iter().all(|row| row.is_empty())
            && wide;
        (straight && !self.level.edge.is_empty()).then_some(self.level.edge.as_slice())
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
        let reach = i64::from(self.level.reach);
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
                    self.level.color,
                    weight,
                    dither.bias(x.cast_unsigned()),
                );
            }
        }
        blended
    }
}

/// The pixel a shadow between its levels lays at screen column `x`: each
/// level's own pixel scaled by its share of `lift`, summed.
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
            row.level.color.scale_alpha_biased(weight, bias)
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

/// [`ShadowRow::blend_into`] for a shadow between its levels: every column
/// either level can reach is weighed one by one — across the window only
/// those its arcs and ramps reach — which only a window easing between levels
/// pays, and only while it eases.
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
    // Both levels share the window's silhouette, so they share its interior.
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
    *dst = color
        .scale_alpha_biased(weight, bias)
        .over_biased(*dst, bias);
    1
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

/// The level's straight-edge profile ([`Level::edge`]) from the kernel's
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
