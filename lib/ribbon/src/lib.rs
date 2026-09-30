//! The ribbon of light: five soft strands of ember light roaming a dark
//! screen, kept clear of the text drawn over it.
//!
//! Two embedders draw it, and neither may depend on the other: the desktop
//! session's minimal-clock screensaver keeps it clear of the time, and the
//! graphical login screen keeps it clear of its login column. One scene
//! drawn in two places is the duplication the charter forbids, so the scene
//! lives here and each embedder owns only where its pixels go.
//!
//! * [`Light`] — the ribbon at an instant: placed at a time in seconds around
//!   a clear space, painted into any part of a surface, and stepped from one
//!   frame to the next with the strips that step changed.
//! * [`Motion`] — the ribbon's own clock: when its next frame is due and how
//!   far a frame moves it, so every embedder paces it alike.
//! * [`SKY`] — the black wherever its light does not reach, and over the
//!   text's clear space.
//!
//! `no_std` with `alloc`. Every buffer a frame needs is reserved when the
//! ribbon is made, and a heap that refuses one is a `None`, never a panic.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod light;
mod motion;

pub use light::Light;
pub use motion::{Motion, FRAME_NS};

use tairix_raster::Color;

/// What the ribbon shows wherever its light does not reach, and over the
/// whole of the text's clear space.
///
/// Text drawn over the ribbon is set against this, so an embedder that
/// shadows its lines for legibility shadows them in it: over the dark the
/// shadow composes to exactly what is already there.
pub const SKY: Color = Color::rgb(0, 0, 0);
