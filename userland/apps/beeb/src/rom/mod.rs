//! The ROMs the emulator ships, written in Rust: the 6502 builder and the
//! clean-room ROMs built with it. Host-only: the freestanding binary loads
//! the committed images and never builds them.

pub mod asm;
pub mod basic;
pub mod dfs;
pub mod mos;
pub mod os;

use asm::{AsmError, Image};

/// What makes one ROM image.
pub type Build = fn() -> Result<Image, AsmError>;

/// Every image the bundle ships, by its name in `Resources/`, with the
/// builder that makes it from this tree.
pub const SHIPPED: [(&str, Build); 3] = [
    (crate::firmware::MOS, mos::build),
    (crate::firmware::BASIC, basic::build),
    (crate::firmware::DFS, dfs::build),
];
