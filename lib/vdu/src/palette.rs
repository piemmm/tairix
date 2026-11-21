//! The default palettes, and the 256-colour modes' palette arithmetic as RISC
//! OS 5 programs it: the default palette, and the sixteen-entry arrangement in
//! which a pixel's top four bits override those of the entry its low four
//! select.

/// The physical colours a two-colour mode's logical colours start as: black
/// and white.
pub const DEFAULT_2: [u8; 2] = [0, 7];
/// The physical colours a four-colour mode's logical colours start as: black,
/// red, yellow and white.
pub const DEFAULT_4: [u8; 4] = [0, 1, 3, 7];

/// The colour 256-colour pixel `pixel` shows when the entry its low four bits
/// select is `base`: the pixel's bits 4 to 7 give red's bit 3, green's bits 2
/// and 3 and blue's bit 3, in both nibbles of each channel, and every other
/// bit is the base's.
#[must_use]
pub const fn grouped(base: [u8; 3], pixel: u8) -> [u8; 3] {
    const fn both(set: bool, bits: u8) -> u8 {
        if set {
            bits
        } else {
            0
        }
    }
    [
        base[0] & 0x77 | both(pixel & 0x10 != 0, 0x88),
        base[1] & 0x33 | both(pixel & 0x20 != 0, 0x44) | both(pixel & 0x40 != 0, 0x88),
        base[2] & 0x77 | both(pixel & 0x80 != 0, 0x88),
    ]
}

/// The default palette's colour for 256-colour pixel `pixel`: four bits of
/// each channel, widened to eight, its top bit red's, green's or blue's own
/// and its low two, the tint, shared.
#[must_use]
pub const fn default_256(pixel: u8) -> [u8; 3] {
    let tint = pixel & 3;
    let red = (pixel >> 4 & 1) << 3 | (pixel >> 2 & 1) << 2 | tint;
    let green = (pixel >> 6 & 1) << 3 | (pixel >> 5 & 1) << 2 | tint;
    let blue = (pixel >> 7 & 1) << 3 | (pixel >> 3 & 1) << 2 | tint;
    [red * 0x11, green * 0x11, blue * 0x11]
}

#[cfg(test)]
mod tests {
    use super::{default_256, grouped};

    #[test]
    fn a_group_keeps_its_base_s_low_bits_as_risc_os_5_reads_them_back() {
        // Entries 49, 161 and 168 as RISC OS 5.30's OS_ReadPalette read them
        // after VDU 19,17,16,&10,&20,&30 and VDU 19,200,16,1,2,3 in mode 28.
        let base_1 = [0x10, 0x20, 0x30];
        assert_eq!(grouped(base_1, 49), [0x98, 0x64, 0x30]);
        assert_eq!(grouped(base_1, 161), [0x10, 0x64, 0xB8]);
        assert_eq!(grouped([1, 2, 3], 168), [0x01, 0x46, 0x8B]);
    }

    #[test]
    fn the_default_palette_is_its_own_base_grouped() {
        for pixel in 0..=u8::MAX {
            let base = default_256(pixel & 0xF);
            assert_eq!(grouped(base, pixel), default_256(pixel), "pixel {pixel}");
        }
    }

    #[test]
    fn the_default_palette_runs_from_black_to_white_through_its_tints() {
        assert_eq!(default_256(0), [0, 0, 0]);
        assert_eq!(default_256(0xFF), [0xFF, 0xFF, 0xFF]);
        assert_eq!(default_256(0x10), [0x88, 0, 0], "red's top bit");
        assert_eq!(default_256(0x03), [0x33, 0x33, 0x33], "the tint");
    }
}
