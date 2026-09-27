//! `WinterSun`'s client shell: the window a player looks through, and the
//! frame they see in it.
#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::float_arithmetic)]

extern crate alloc;

#[cfg(feature = "settings")]
pub mod appbar;
pub mod budget;
pub mod camera;
pub mod cli;
pub mod digest;
pub mod error;
pub mod figures;
pub mod frame;
#[cfg(feature = "settings")]
pub mod graphics;
pub mod input;
pub mod landfall;
pub mod light;
pub mod pacing;
pub mod presets;
pub mod quality;
pub mod reference;
#[cfg(feature = "settings")]
pub mod settings;
pub mod shell;
pub mod terrain;
pub mod view;
