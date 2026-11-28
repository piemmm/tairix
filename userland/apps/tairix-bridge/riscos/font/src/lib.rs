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
use tairix_util::endian::le_u16;

/// The built-in modules this crate provides, for the session's
/// `Config::modules`.
pub static MODULES: [&Builtin; 2] = [&module::FONT_MANAGER, &rom::ROM_FONTS];

/// The most the Font Manager holds of the host's memory beside a session of
/// `ceiling` bytes: its caches.
#[must_use]
pub fn host_bytes(ceiling: u64) -> u64 {
    store::Store::most(ceiling)
}

/// The encoding files `ROMFonts` holds, made afresh from their tables, each
/// by its leaf in `Resources:$.Fonts.Encodings`: what
/// `cargo xtask riscos --write-encodings` commits.
#[must_use]
pub fn encoding_files() -> Vec<(String, Vec<u8>)> {
    encodings::files(&glyph_names::GlyphNames::read())
}

/// The two's-complement 16-bit value at `at` of `bytes`, as a font file
/// signs its halfwords; `None` where they hold fewer than two bytes there.
pub(crate) fn le_i16(bytes: &[u8], at: usize) -> Option<i16> {
    le_u16(bytes, at).map(u16::cast_signed)
}

/// `value` written low byte first at `at` of `out`.
pub(crate) fn put_word(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
