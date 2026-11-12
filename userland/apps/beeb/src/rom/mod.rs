//! The ROMs the emulator ships, written in Rust: the 6502 builder and the
//! clean-room ROMs built with it. Host-only: the freestanding binary loads
//! the committed images and never builds them.

pub mod asm;
pub mod mos;
