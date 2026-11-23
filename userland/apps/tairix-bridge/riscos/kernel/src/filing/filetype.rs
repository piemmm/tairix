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

/// File type `kind`'s three hex digits, as `File$Type_` and `@LoadType_`
/// names spell them.
#[must_use]
pub fn digits(kind: u32) -> [u8; 3] {
    let mut digits = [0; 3];
    tairix_util::hex::fill_upper(&mut digits, kind.into());
    digits
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_type_is_three_hex_digits_its_higher_bits_ignored() {
        assert_eq!(super::digits(super::TEXT), *b"FFF");
        assert_eq!(super::digits(0x5), *b"005");
        assert_eq!(super::digits(0xFFFF_F0AB), *b"0AB");
    }
}
