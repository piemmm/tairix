use super::*;
use crate::mode::Format;

fn numbered(n: u32) -> Format {
    Format::numbered(tairix_vdu::mode::numbered(n).expect("a mode"))
}

#[test]
fn each_depth_starts_with_its_default_colours_held_bare() {
    let sixteen = Palette::for_mode(&numbered(12));
    assert_eq!(sixteen.entries[1], [0x0000_FF00; 2], "red");
    assert_eq!(
        sixteen.entries[9],
        [0x0000_FF00, 0xFFFF_0000],
        "red and cyan flashing"
    );
    let teletext = Palette::for_mode(&numbered(7));
    assert_eq!(teletext.entries[9], [0x0000_FFFF; 2], "red with supremacy");
    let four = Palette::for_mode(&numbered(1));
    assert_eq!(four.entries[2][0], 0x00FF_FF00, "yellow");
    let full = Palette::for_mode(&numbered(28));
    assert_eq!(full.entries[0x10][0], 0x0000_8800);
    assert!(sixteen.flashing(&numbered(12)));
    assert!(!four.flashing(&numbered(1)));
}

#[test]
fn vdu_19_s_forms_set_one_state_both_or_the_border() {
    let blue = vdu_19(4, [0; 3], [0; 2]);
    assert_eq!(blue, Some((16, [0xFF00_0000; 2])));
    let first = vdu_19(17, [0xFF, 0, 0], [1, 2]);
    assert_eq!(first, Some((16, [0x0000_FF00, 2])), "the second kept");
    assert_eq!(
        vdu_19(24, [0x80, 0x40, 0x20], [0; 2]),
        Some((24, [0x2040_8000; 2]))
    );
    assert_eq!(vdu_19(144, [1, 2, 3], [0; 2]), Some((16, [0x0302_01FF; 2])));
    assert_eq!(vdu_19(30, [0; 3], [0; 2]), None);
}

#[test]
fn an_entry_is_reported_with_how_it_shows_in_place_of_its_low_byte() {
    // As RISC OS 5's OS_ReadPalette reports them.
    assert_eq!(reported(16, [0x0000_FF00; 2]), [0x0000_FF10; 2], "steady");
    assert_eq!(
        reported(16, [0x0302_01FF; 2]),
        [0x0302_0190; 2],
        "of supremacy"
    );
    assert_eq!(
        reported(16, [0x0000_FF00, 0xFFFF_0000]),
        [0x0000_FF11, 0xFFFF_0012],
        "flashing"
    );
    assert_eq!(
        reported(16, [0x1111_1111, 0x2222_2282]),
        [0x1111_1111, 0x2222_2292],
        "bits 0 to 6 dropped"
    );
    assert_eq!(
        reported(24, [0x2040_8000; 2]),
        [0x2040_8018; 2],
        "the border"
    );
    assert_eq!(
        reported(25, [0xFF00_FF00; 2]),
        [0xFF00_FF19; 2],
        "a pointer colour"
    );
}

#[test]
fn a_256_colour_mode_s_group_shares_its_base_colour() {
    let format = numbered(28);
    let mut palette = Palette::for_mode(&format);
    palette.write(&format, 1, 16, [0xA090_855A; 2]);
    let entry = palette.entries[17][0];
    assert_eq!(entry, grouped(0xA090_855A, 17));
    assert_eq!(entry & 0xFF, 0x5A, "the base's low byte kept");
    assert_eq!(
        (entry >> 8) & 0xFF,
        0x8D,
        "red's low bits from the base, its top bits from bit 4"
    );
}
