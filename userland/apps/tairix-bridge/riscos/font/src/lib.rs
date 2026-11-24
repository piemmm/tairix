//! TAIRiX Bridge's Font Manager and ROM fonts (`plans/RISCOS-EMULATOR.md`
//! §8): RISC OS 5.30's Font Manager 3.80 over guest memory, its standard
//! families TAIRiX's own faces and a guest's own fonts read from
//! `Font$Path`, held to what it was recorded doing.
//!
//! - [`MODULES`]: the built-in modules this crate provides.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod bitmaps;
mod buffer;
#[cfg(any(test, feature = "test-support"))]
pub mod build;
mod cache;
mod catalogue;
mod colours;
mod commands;
mod desktop;
#[cfg(test)]
mod drawfile_tests;
mod encoding;
mod encodings;
mod faces;
mod find;
#[cfg(test)]
mod fixture;
mod fontdata;
mod glyph_names;
mod glyphs;
mod grid;
mod handles;
mod identifier;
mod layout;
mod listing;
mod makebitmap;
mod measure;
mod menu;
mod metrics;
mod module;
mod outlines;
mod paint;
mod resolve;
mod rom;
mod rom_encodings;
mod scan;
mod settings;
mod store;
mod transform;
mod units;
mod x90y45;

use alloc::string::String;
use alloc::vec::Vec;

use tairix_riscos_kernel::module::Builtin;

/// The built-in modules this crate provides, for the session's
/// `Config::modules`.
pub static MODULES: [&Builtin; 2] = [&module::FONT_MANAGER, &rom::ROM_FONTS];

/// The encoding files `ROMFonts` holds, made afresh from their tables, each
/// by its leaf in `Resources:$.Fonts.Encodings`: what
/// `cargo xtask riscos --write-encodings` commits.
#[must_use]
pub fn encoding_files() -> Vec<(String, Vec<u8>)> {
    encodings::files(&glyph_names::GlyphNames::read())
}
