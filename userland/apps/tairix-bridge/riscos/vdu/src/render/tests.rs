use super::*;
use tairix_colour::Rgb;
use tairix_vdu::palette::default_256;

/// The screen blended over in every recorded case: GCOL `&4080C000`, the
/// supremacy byte as each case set it.
const SCREEN_16M: u32 = 0x0040_80C0;

#[test]
fn in_256_colours_a_blend_mixes_five_bit_colours_and_finds_the_result_in_the_32k_table() {
    // RISC OS 5.30, Font_Paint bit 11 over the default palette with the
    // foreground black: each background colour number, level and result.
    let recorded: &[(u32, u8, u32)] = &[
        (0x00, 3, 0x00),
        (0x00, 9, 0x00),
        (0x00, 11, 0x00),
        (0x00, 12, 0x00),
        (0x00, 13, 0x00),
        (0x00, 14, 0x00),
        (0x00, 15, 0x00),
        (0x15, 3, 0x11),
        (0x15, 9, 0x04),
        (0x15, 11, 0x04),
        (0x15, 12, 0x04),
        (0x15, 13, 0x00),
        (0x15, 14, 0x00),
        (0x15, 15, 0x00),
        (0x47, 3, 0x45),
        (0x47, 9, 0x24),
        (0x47, 11, 0x02),
        (0x47, 12, 0x01),
        (0x47, 13, 0x01),
        (0x47, 14, 0x00),
        (0x47, 15, 0x00),
        (0x8B, 3, 0x8A),
        (0x8B, 9, 0x09),
        (0x8B, 11, 0x08),
        (0x8B, 12, 0x01),
        (0x8B, 13, 0x00),
        (0x8B, 14, 0x00),
        (0x8B, 15, 0x00),
        (0xFF, 3, 0xFC),
        (0xFF, 9, 0x2D),
        (0xFF, 11, 0x03),
        (0xFF, 12, 0x02),
        (0xFF, 13, 0x01),
        (0xFF, 14, 0x00),
        (0xFF, 15, 0x00),
    ];
    let colours: alloc::vec::Vec<Rgb> = (0..=u8::MAX)
        .map(|pixel| {
            let [r, g, b] = default_256(pixel);
            Rgb::new(r, g, b)
        })
        .collect();
    let palettised = Palettised::of(&colours).expect("room");
    let towards = Towards::palettised(&palettised, 0);
    for &(screen, level, wanted) in recorded {
        assert_eq!(
            towards.blended(screen, level),
            wanted,
            "colour {screen:#x} at level {level}"
        );
    }
}

#[test]
fn in_32k_colours_a_blend_mixes_each_five_bit_channel() {
    let pixel = |word: u32| Direct::FIFTEEN.pack(Rgb::from_riscos_word(word).to_array());
    let recorded: &[(u32, u8, u32)] = &[
        (0x0000_FF00, 3, 0x0019),
        (0x0000_FF00, 9, 0x000B),
        (0x0000_FF00, 11, 0x0007),
        (0x0000_FF00, 12, 0x0005),
        (0x0000_FF00, 13, 0x0003),
        (0x0000_FF00, 14, 0x0001),
        (0x0000_FF00, 15, 0x0000),
        (0x1010_1000, 3, 0x0421),
        (0x1010_1000, 9, 0x0000),
        (0x1010_1000, 15, 0x0000),
        (0xCF6F_2700, 3, 0x5143),
        (0xCF6F_2700, 9, 0x2481),
        (0xCF6F_2700, 11, 0x1861),
        (0xCF6F_2700, 12, 0x1040),
        (0xCF6F_2700, 13, 0x0C20),
        (0xCF6F_2700, 14, 0x0400),
        (0xCF6F_2700, 15, 0x0000),
        (0xFFFF_FF00, 3, 0x6739),
        (0xFFFF_FF00, 9, 0x2D6B),
        (0xFFFF_FF00, 11, 0x1CE7),
        (0xFFFF_FF00, 12, 0x14A5),
        (0xFFFF_FF00, 13, 0x0C63),
        (0xFFFF_FF00, 14, 0x0421),
        (0xFFFF_FF00, 15, 0x0000),
    ];
    let towards = Towards::direct(Direct::FIFTEEN, 0x8000, 0, false);
    for &(background, level, wanted) in recorded {
        assert_eq!(
            towards.blended(pixel(background), level),
            wanted,
            "{background:#x} at {level}"
        );
    }
}

#[test]
fn in_16m_colours_a_blend_keeps_the_supremacy_byte_and_supremacy_composites_it() {
    // RISC OS 5.30's every cell: whether supremacy, the foreground word, the
    // screen's supremacy byte, the level and the pixel left.
    let recorded: &[(bool, u32, u32, u8, u32)] = &[
        (false, 0x0000_0000, 0x00, 3, 0x0034_689C),
        (false, 0x0000_0000, 0x00, 13, 0x0008_1018),
        (false, 0x0000_0000, 0x00, 15, 0x0000_0000),
        (false, 0x0000_0000, 0x80, 3, 0x8034_689C),
        (false, 0x0000_0000, 0x80, 14, 0x8004_080C),
        (false, 0x0000_0000, 0x80, 15, 0x0000_0000),
        (false, 0x0000_0080, 0xFF, 9, 0xFF18_3048),
        (false, 0xFFFF_FF40, 0x00, 3, 0x0063_97CB),
        (false, 0xFFFF_FF40, 0x80, 13, 0x80E7_EFF7),
        (false, 0xFFFF_FF40, 0xFF, 15, 0x00FF_FFFF),
        (true, 0x0000_0000, 0x00, 3, 0x0034_689C),
        (true, 0x0000_0000, 0x00, 15, 0x0000_0000),
        (true, 0x0000_0000, 0x80, 3, 0x682C_5884),
        (true, 0x0000_0000, 0x80, 9, 0x3010_2030),
        (true, 0x0000_0000, 0x80, 11, 0x2008_1018),
        (true, 0x0000_0000, 0x80, 12, 0x1808_1018),
        (true, 0x0000_0000, 0x80, 13, 0x1004_080C),
        (true, 0x0000_0000, 0x80, 14, 0x0804_080C),
        (true, 0x0000_0000, 0xFF, 3, 0xCF00_0000),
        (true, 0x0000_0000, 0xFF, 9, 0x6000_0000),
        (true, 0x0000_0000, 0xFF, 12, 0x3000_0000),
        (true, 0x0000_0080, 0x00, 3, 0x003C_78B4),
        (true, 0x0000_0080, 0x00, 9, 0x002C_5884),
        (true, 0x0000_0080, 0x00, 13, 0x0024_486C),
        (true, 0x0000_0080, 0x00, 15, 0x0020_4060),
        (true, 0x0000_0080, 0x80, 3, 0x7838_70A8),
        (true, 0x0000_0080, 0x80, 9, 0x5820_4060),
        (true, 0x0000_0080, 0x80, 15, 0x4014_283C),
        (true, 0x0000_0080, 0xFF, 3, 0xEF00_0000),
        (true, 0x0000_0080, 0xFF, 15, 0x8000_0000),
        (true, 0xFFFF_FF40, 0x00, 3, 0x0057_8FC7),
        (true, 0xFFFF_FF40, 0x00, 9, 0x009F_BFDF),
        (true, 0xFFFF_FF40, 0x00, 14, 0x00C3_D7EB),
        (true, 0xFFFF_FF40, 0x00, 15, 0x00CF_DFEF),
        (true, 0xFFFF_FF40, 0x80, 3, 0x706F_9FCF),
        (true, 0xFFFF_FF40, 0x80, 9, 0x40C3_D7EB),
        (true, 0xFFFF_FF40, 0x80, 12, 0x30CF_DFEF),
        (true, 0xFFFF_FF40, 0x80, 15, 0x20E7_EFF7),
        (true, 0xFFFF_FF40, 0xFF, 3, 0xDFFF_FFFF),
        (true, 0xFFFF_FF40, 0xFF, 9, 0x80FF_FFFF),
        (true, 0xFFFF_FF40, 0xFF, 13, 0x50FF_FFFF),
        (true, 0xFFFF_FF40, 0xFF, 15, 0x40FF_FFFF),
    ];
    let direct = Direct::of_parts(5, u32::MAX, 0);
    for &(supremacy, foreground, top, level, wanted) in recorded {
        let towards = Towards::direct(direct, 0xFF00_0000, foreground, supremacy);
        let screen = SCREEN_16M | top << 24;
        assert_eq!(
            towards.blended(screen, level),
            wanted,
            "supremacy {supremacy}, foreground {foreground:#x}, top {top:#x}, level {level}"
        );
    }
}
