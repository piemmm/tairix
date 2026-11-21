//! Soft drop shadows under floating surfaces: which shadow each casts.
//!
//! What a shadow is — the silhouette dropped by its reach and softened by a
//! compact kernel, exact by linearity — is `tairix_raster`'s
//! [`DropShadow`], which the pinned `shadow` pass is held to as well. This
//! module holds what the desktop decides: the two shadows its theme casts at
//! the output's scale, and which of them a window casts.
//!
//! # Two levels
//!
//! A decorated window resting behind the family the user is working in casts
//! a lesser shadow than the window in front and every floating surface:
//! lighter and shorter, so it reads as standing lower. The kit holds a
//! shadow for each. A window easing between them casts both at once, each
//! weighted by how far it has risen, so the change is a crossfade between two
//! exact shadows rather than a guess at one between.

use tairix_abi::driver::display::RefreshRate;
use tairix_raster::{shadow_reach, shadow_row, Caster, DropShadow, RoundRect, ShadowRow};
use tairix_theme::{Fade, Theme};

use crate::color::Color;
use crate::geometry::{Rect, Scale};

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

    /// Nanoseconds until the next step is owed on a display refreshing at
    /// `refresh`, or `None` for a shadow at rest: at once for a change no step
    /// has started.
    pub(crate) fn next_frame_in(&self, now_ns: u64, refresh: RefreshRate) -> Option<u64> {
        if self.target != self.fade.target() {
            return Some(0);
        }
        self.fade.next_frame_in(now_ns, refresh)
    }
}

/// How far a shadow of logical `reach` in `color` reaches at `scale`, in
/// physical pixels: `0` where it casts none.
fn physical_reach(scale: Scale, color: Color, reach: u32) -> u32 {
    shadow_reach(scale.scale_length(reach), color)
}

/// How far the further of the two shadows `theme` casts at `scale` reaches,
/// in physical pixels: `0` where it casts none.
fn reach(scale: Scale, theme: &Theme) -> u32 {
    let (palette, metrics) = (theme.palette(), theme.metrics());
    physical_reach(
        scale,
        Color::from(palette.drop_shadow),
        metrics.drop_shadow_reach,
    )
    .max(physical_reach(
        scale,
        Color::from(palette.inactive_drop_shadow),
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

/// Everything shadows are drawn from on one output, rebuilt whenever its scale
/// or theme changes: a shadow for a surface in front and one for a window
/// resting behind it, each holding the notch tiles for the radii the theme
/// rounds by.
pub(crate) struct ShadowKit {
    lifted: DropShadow,
    resting: DropShadow,
    /// The radii the theme itself rounds by, whose tiles are kept whatever is
    /// on screen.
    standard: [u32; 2],
}

impl ShadowKit {
    /// The kit for an output at `scale` under `theme`: each level's shadow,
    /// and its notch tiles for the window and popup radii the theme rounds by.
    ///
    /// An allocation refused anywhere leaves that level casting no shadows,
    /// which costs the desktop its depth and never a frame.
    pub(crate) fn new(scale: Scale, theme: &Theme) -> Self {
        let (palette, metrics) = (theme.palette(), theme.metrics());
        let level = |color, reach| DropShadow::new(scale.scale_length(reach), Color::from(color));
        let mut kit = Self {
            lifted: level(palette.drop_shadow, metrics.drop_shadow_reach),
            resting: level(
                palette.inactive_drop_shadow,
                metrics.inactive_drop_shadow_reach,
            ),
            standard: [
                scale.scale_length(metrics.window_corner_radius),
                scale.scale_length(metrics.popup_corner_radius),
            ],
        };
        for radius in kit.standard {
            kit.ensure_tile(radius);
        }
        kit
    }

    /// How far the further of the two shadows reaches on this output, `0` for
    /// none: the bound every caster's footprint is grown by.
    pub(crate) fn reach(&self) -> u32 {
        self.lifted.reach().max(self.resting.reach())
    }

    /// Drop every tile but the theme's own radii and those `in_use` answers
    /// for, in both levels.
    ///
    /// Tiles for other radii exist because a window too small for its corners
    /// rounds by less, so what this keeps is bounded by what is on screen
    /// rather than by every window that ever cast a shadow.
    pub(crate) fn retain_tiles(&mut self, in_use: impl Fn(u32) -> bool) {
        let standard = self.standard;
        let keep = |radius| standard.contains(&radius) || in_use(radius);
        self.lifted.retain_tiles(keep);
        self.resting.retain_tiles(keep);
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
        shape: Option<RoundRect>,
        opacity: u8,
        elevation: Elevation,
        y: i32,
    ) -> Option<ShadowRow<'_>> {
        let caster = Caster {
            left: bounds.left(),
            top: bounds.top(),
            right: bounds.right(),
            bottom: bounds.bottom(),
            silhouette: shape,
            opacity,
        };
        shadow_row(&self.lifted, &self.resting, &caster, elevation.0, y)
    }
}

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod tests;
