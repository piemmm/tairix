use alloc::vec::Vec;

use super::*;

/// A colour from a palette word, `&BBGGRR00`.
const fn word(w: u32) -> Rgb {
    Rgb::from_riscos_word(w)
}

#[test]
fn the_error_weighs_each_channel_by_its_loading() {
    let error = Loadings::DEFAULT.error(word(0x0000_0000), word(0x0302_0100));
    assert_eq!(error, 2 + 4 * 4 + 9);
    assert_eq!(Loadings(0).error(Rgb::BLACK, Rgb::WHITE), 0);
    assert_eq!(
        Loadings(0xFFFF_FFFF).error(Rgb::BLACK, Rgb::WHITE),
        3 * 255 * 255 * 255,
        "the largest error fits a word"
    );
}

#[test]
fn a_search_takes_the_first_of_equally_near_and_the_last_of_equally_far() {
    // Mode 12's default palette, its flashing colours in their first state.
    let sixteen: Vec<Rgb> = [
        0,
        0xFF00,
        0xFF_0000,
        0xFF_FF00,
        0xFF00_0000,
        0xFF00_FF00,
        0xFFFF_0000,
        0xFFFF_FF00,
    ]
    .iter()
    .chain(&[
        0,
        0xFF00,
        0xFF_0000,
        0xFF_FF00,
        0xFF00_0000,
        0xFF00_FF00,
        0xFFFF_0000,
        0xFFFF_FF00,
    ])
    .map(|&w| word(w))
    .collect();
    let at = |wanted, look| search(&sixteen, word(wanted), Loadings::DEFAULT, look);
    assert_eq!(at(0x0000_FF00, Look::Nearest), 1);
    assert_eq!(at(0, Look::Furthest), 15);
    assert_eq!(
        at(0x8080_8000, Look::Nearest),
        7,
        "grey nearer white than black"
    );
}

#[test]
fn the_default_256_colour_match_is_its_tint_loop_as_recorded() {
    // Each colour, its nearest colour number and its furthest one, as RISC OS
    // 5's ColourTrans gave them in mode 15 with the default palette.
    for (wanted, nearest, furthest) in [
        (0, 0, 3),
        (0xFFFF_FF00, 0xFF, 0xFC),
        (0x0000_FF00, 0x15, 0x17),
        (0x00FF_0000, 0x62, 0x60),
        (0xFF00_0000, 0x88, 0x8B),
        (0x92B6_9200, 0xF0, 0xD1),
        (0xFFFF_0000, 0xEA, 0xE8),
        (0x2449_6D00, 0x25, 0x26),
    ] {
        let wanted = word(wanted);
        assert_eq!(
            default_256_match(wanted, Loadings::DEFAULT, Look::Nearest),
            nearest
        );
        assert_eq!(
            default_256_match(wanted, Loadings::DEFAULT, Look::Furthest),
            furthest
        );
    }
}

#[test]
fn a_palette_is_the_default_where_each_channel_s_top_nibble_is_its_own() {
    let default: Vec<Rgb> = (0..=u8::MAX)
        .map(|p| {
            let [r, g, b] = default_256(p);
            Rgb::new(r, g, b)
        })
        .collect();
    assert!(is_default_256(&default));
    let mut low_nibbles = default.clone();
    for colour in &mut low_nibbles {
        colour.r |= 0x0F;
    }
    assert!(is_default_256(&low_nibbles), "low nibbles are not compared");
    let mut moved = default.clone();
    moved[1].r = 0x21;
    assert!(!is_default_256(&moved));
    assert!(!is_default_256(&default[..16]));
}

#[test]
fn a_direct_match_cuts_each_channel_and_looking_furthest_inverts_it() {
    let fifteen = Direct::of_parts(4, 0xFFFF, 0);
    assert_eq!(
        direct_match(fifteen, word(0x2400_0000), Look::Nearest),
        0x1000
    );
    assert_eq!(
        direct_match(fifteen, word(0x2400_0000), Look::Furthest),
        0x6FFF
    );
    let thirty_two = Direct::of_parts(5, u32::MAX, 0);
    assert_eq!(
        direct_match(thirty_two, word(0x2400_0000), Look::Nearest),
        0x24_0000
    );
    assert_eq!(
        direct_match(thirty_two, word(0x2400_0000), Look::Furthest),
        0xDB_FFFF
    );
}
