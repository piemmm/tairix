//! The TAIRiX ray tracer: scenes composed at random from a seed and traced a
//! pixel at a time.
//!
//! A [`Draft`] composes a scene in one of the [`Setting`]s — still lifes,
//! architecture, and landscapes of terrain, sea, trees, grass and cloud —
//! from its seed alone. What it cannot set out at once, the height grids a
//! terrain or a swell is traced over and the map a cloud layer is drawn from,
//! it prepares a band of rows at a time, so a caller on an interactive loop
//! spreads the work over as many frames as it needs. [`Draft::finish`] then
//! builds the hierarchy a ray finds the objects through, and the [`Scene`] is
//! read-only from there on, shared by every core tracing it.
//!
//! A [`Tracer`] answers what one pixel shows: distributed ray tracing with
//! stratified, adaptive sampling, toned for the screen by the [`Encoder`].
//! Every pixel is traced from its own index and the scene's key alone, so it
//! comes out the same on whichever core takes it and in whatever order;
//! [`Reveal`] is one such order, scattered over the whole picture.
//!
//! `no_std` + `alloc`, with no `unsafe`. The design, the settings and the
//! measurements behind its sampling and its budgets are in
//! `docs/src/lib/raytrace.md`.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod bvh;
mod camera;
mod compose;
mod foliage;
mod grass;
mod heightfield;
mod light;
mod material;
mod noise;
mod pigment;
mod sample;
mod scene;
mod shape;
mod sky;
mod terrain;
mod tone;
mod trace;
mod vector;

pub use compose::Setting;
pub use sample::Reveal;
pub use scene::{Draft, Scene};
pub use tone::Encoder;
pub use trace::{Quality, Tracer};
