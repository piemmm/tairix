//! Per-window rounded-corner selection.
//!
//! A window (and the taskbar, through this same path) may request rounded
//! corners of a given radius. Composition multiplies each source pixel's
//! alpha by a *coverage* value in `0..=255`: `255` deep inside the rounded
//! rectangle, `0` outside it, and a smooth ramp across the corner arc.
//!
//! The coverage arithmetic itself is **not** defined here: a window's shape is
//! a `tairix_raster::RoundRect` of the radius its style names, the single
//! rounded-rectangle definition the compositor and the Reactive Alloy control
//! plates both round through, so window corners and control corners can never
//! diverge. This type only carries the per-window corner *choice*.

/// Per-window corner-rounding selection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Corners {
    /// Square corners; coverage is always fully opaque (the opt-out).
    Square,
    /// Rounded corners with the given radius in pixels, cut by the compositor:
    /// the content is square and the compositor rounds it. A radius is clamped
    /// to half the shorter side at evaluation time.
    Rounded {
        /// Corner radius in pixels.
        radius: u32,
    },
    /// Rounded corners the surface has already painted into its own pixels.
    ///
    /// The shape is the window's silhouette — what its frosted backdrop is
    /// weighted by and what its shadow is cut to — but the compositor never
    /// cuts the content itself: a plate that anti-aliases its own arc and is
    /// then cut again has its edge weakened twice.
    Painted {
        /// Corner radius in pixels, clamped as [`Rounded`](Self::Rounded)'s.
        radius: u32,
    },
}

impl Corners {
    /// The compositor-cut corner style for a theme corner radius.
    ///
    /// A radius of `0` is the square opt-out; any other radius rounds.
    /// This lets a window or the taskbar take its corner radius straight
    /// from the active theme's [`Metrics`](tairix_theme::Metrics) without
    /// the caller re-deciding what "no rounding" means.
    #[must_use]
    pub const fn from_radius(radius: u32) -> Self {
        if radius == 0 {
            Self::Square
        } else {
            Self::Rounded { radius }
        }
    }

    /// The corner style of a surface whose own pixels already round by
    /// `radius`, with `0` the square opt-out as for
    /// [`from_radius`](Self::from_radius).
    #[must_use]
    pub const fn painted(radius: u32) -> Self {
        if radius == 0 {
            Self::Square
        } else {
            Self::Painted { radius }
        }
    }

    /// Whether the compositor cuts the content to this shape, rather than
    /// taking the content as already shaped.
    #[must_use]
    pub(crate) const fn cuts_content(self) -> bool {
        matches!(self, Self::Rounded { .. })
    }
}
