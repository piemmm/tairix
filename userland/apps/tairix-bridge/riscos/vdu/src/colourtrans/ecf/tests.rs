use alloc::vec::Vec;

use super::*;
use crate::colourtrans::matching::Loadings;
use crate::colourtrans::palettes::Holding;
use crate::palette::default_words;

/// A mode's default palette, as ColourTrans matches against it.
fn default_palette(log2_bpp: u32) -> Against {
    let holding = match log2_bpp {
        0..=2 => Holding::Palette(log2_bpp),
        _ => Holding::Grouped,
    };
    let entries = if log2_bpp >= 3 {
        16
    } else {
        1 << (1 << log2_bpp)
    };
    let words: Vec<u32> = default_words(log2_bpp, false)
        .iter()
        .take(entries)
        .map(|[first, _]| *first)
        .collect();
    Against::of_words(holding, &words)
}

fn word(w: u32) -> Rgb {
    Rgb::from_riscos_word(w)
}

#[test]
fn two_colours_light_bayer_s_cells_by_the_luma_s_fifteenths() {
    // Each as RISC OS 5 gave it in mode 0: rows 0 to 3, then again.
    let mode_0 = default_palette(0);
    for (wanted, rows) in [
        (
            0x8888_8800,
            [0x7777_7777, 0xAAAA_AAAA, 0x5555_5555, 0xAAAA_AAAA],
        ),
        (0x0000_FF00, [0x5555_5555, 0x2222_2222, 0x5555_5555, 0]),
        (0x1020_3000, [0x1111_1111, 0, 0x4444_4444, 0]),
        (
            0x2AD8_A900,
            [0xFFFF_FFFF, 0xAAAA_AAAA, 0xFFFF_FFFF, 0xAAAA_AAAA],
        ),
        (0xFFFF_FF00, [u32::MAX; 4]),
        (0, [0; 4]),
    ] {
        let pattern = pattern(&mode_0, word(wanted), Loadings::DEFAULT).expect("palettised");
        assert_eq!(pattern[..4], rows, "{wanted:08X}");
        assert_eq!(pattern[4..], rows, "{wanted:08X} repeats");
    }
}

#[test]
fn deeper_modes_dither_each_channel_on_a_tile() {
    // Mode 12, as RISC OS 5 gave them: a channel alone, a grey, and three
    // channels at once.
    let mode_12 = default_palette(2);
    for (wanted, [row_0, row_1]) in [
        (0x0000_4200, [0x1010_1010, 0]),
        (0x0000_6C00, [0x0101_0101, 0x1010_1010]),
        (0x7777_7700, [0x0707_0707, 0x7070_7070]),
        (0xA7E0_8F00, [0x6767_6767, 0x7272_7272]),
        (0xFFFF_FF00, [0x7777_7777; 2]),
    ] {
        let pattern = pattern(&mode_12, word(wanted), Loadings::DEFAULT).expect("palettised");
        for (row, &got) in pattern.iter().enumerate() {
            assert_eq!(got, [row_0, row_1][row % 2], "{wanted:08X} row {row}");
        }
    }
}

#[test]
fn a_level_is_on_at_three_five_seven_and_nine_twelfths() {
    let on = |v| cells_on(0, 255, v);
    assert_eq!(
        [
            on(63),
            on(64),
            on(106),
            on(107),
            on(148),
            on(149),
            on(191),
            on(192)
        ],
        [0, 1, 1, 2, 2, 3, 3, 4]
    );
    assert_eq!(cells_on(68, 68, 68), 0, "no span, no cells");
}

#[test]
fn a_channel_past_every_level_of_the_palette_takes_the_nearest_undithered() {
    let all_white = Against::of_words(Holding::Palette(2), &[0xFFFF_FF00; 16]);
    let lifted = pattern(&all_white, Rgb::BLACK, Loadings::DEFAULT).expect("palettised");
    assert_eq!(lifted, [0; 8], "every cell the first white");
    let all_black = Against::of_words(Holding::Palette(1), &[0; 4]);
    let lowered = pattern(&all_black, word(0xFFFF_FF00), Loadings::DEFAULT).expect("palettised");
    assert_eq!(lowered, [0; 8], "every cell the first black");
}
