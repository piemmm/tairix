//! TAIRiX shared desktop theme definition (`lib/theme` — `PLAN.md` Stage 7).
//!
//! The charter requires "one shared theme definition" that drives the
//! colours, corner radii, fonts, and cursors of the window manager, the
//! taskbar, and the default apps, with a default light theme and a dark
//! theme switchable at runtime, and where "adding a theme is data, not new
//! code". This crate is that definition.
//!
//! It is pure *data*: a [`Theme`] is a table of `tairix_colour::Rgba` colour
//! roles ([`Palette`]), geometric [`Metrics`] (the corner radii the
//! compositor's single rounded-corner path consumes), [`Fonts`], and a
//! [`CursorSet`]. The colour itself is `lib/colour`'s and the rendering and
//! compositing arithmetic `lib/raster`'s, so nothing is duplicated.
//!
//! # Where it sits
//!
//! As a `lib/*` crate it has no dependencies and is depended on by the GUI
//! crates and the default apps, never the reverse — the bottom of the
//! layering. Living in `lib/*` (not `userland/gui/*`) is deliberate:
//! sibling userland crates may not depend on one another, so the one shared definition they all read belongs here, exactly
//! as `lib/procinfo` is the shared home for the System Information client
//! helpers.
//!
//! # Switching themes
//!
//! [`ThemeRegistry`] owns the available themes and the active one. It
//! always holds the two built-ins, switches with
//! [`set_active`](ThemeRegistry::set_active), and accepts custom themes with
//! [`register`](ThemeRegistry::register). Both mutators fail closed.
//!
//! ```
//! use tairix_theme::{Appearance, ThemeId, ThemeRegistry};
//!
//! let mut themes = ThemeRegistry::with_builtins();
//! assert_eq!(themes.active().appearance(), Appearance::Light);
//!
//! themes.set_active(ThemeId::DARK).expect("dark is built in");
//! assert_eq!(themes.active().appearance(), Appearance::Dark);
//! ```

#![no_std]
// `SyntaxRole::COUNT` is the compiler's own count of its variants, so the
// list `SyntaxRole::ALL` cannot fall out of step with the enum.
#![feature(variant_count)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod cursor;
pub mod metrics;
pub mod motion;
pub mod palette;
pub mod registry;
pub mod syntax;
pub mod theme;
pub mod typography;

#[cfg(test)]
mod tests;

pub use cursor::{CursorKind, CursorSet, CursorSetId, CURSOR_KINDS};
pub use metrics::Metrics;
pub use motion::{Contrast, Density, Fade, Motion, MotionInteraction, MotionTheme, Timeline};
pub use palette::{Palette, SignalRole};
pub use registry::{Grounds, ThemeError, ThemeRegistry};
pub use syntax::{SyntaxPalette, SyntaxRole};
pub use theme::{Accessibility, Appearance, SurfaceGround, Theme, ThemeId};
pub use typography::{
    lifted, line_box_px, points_of, DesktopText, FamilyKey, FontSpec, FontWeight, Fonts, TextRole,
    TEXT_WEIGHT_LIFT,
};
