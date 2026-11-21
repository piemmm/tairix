//! The file types the kernel and the filing systems name: twelve bits each,
//! as a load address carries them.

use tairix_abi::fs::Filetype;

/// `Obey`: commands, a line each.
pub const OBEY: u32 = 0xFEB;
/// `Absolute`: code loaded at the application space's start and run there.
pub const ABSOLUTE: u32 = 0xFF8;
/// `Sprite`: a sprite area's sprites.
pub const SPRITE: u32 = 0xFF9;
/// `Module`: a relocatable module.
pub const MODULE: u32 = 0xFFA;
/// `BASIC`: a tokenised BASIC program.
pub const BASIC: u32 = 0xFFB;
/// `Utility`: code run where it is loaded.
pub const UTILITY: u32 = 0xFFC;
/// `Data`: bytes of no particular kind, the type nothing else gives.
pub const DATA: u32 = Filetype::DATA.get() as u32;
/// `Text`.
pub const TEXT: u32 = 0xFFF;
