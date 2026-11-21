use super::*;

#[test]
fn a_dot_is_two_half_dots_across_and_two_lines_down() {
    let a = &GLYPHS[alphanumeric(b'A')];
    assert_eq!(a[0], 0, "the blank top row");
    assert!(
        a.iter().all(|line| line.trailing_zeros() >= 2),
        "the sixth column is blank"
    );
    let bar = &GLYPHS[alphanumeric(0x7F)];
    assert_eq!(bar[2], 0b1111_1111_1100, "five dots, ten half-dots");
    assert_eq!(bar[2], bar[3]);
}

#[test]
fn rounding_fills_the_half_dots_beside_a_diagonal() {
    // '/' runs diagonally: each join gains half-dots on the lines between.
    let slash = &GLYPHS[alphanumeric(b'/')];
    let dots: u32 = slash.iter().map(|line| line.count_ones()).sum();
    let unrounded: u32 = (0..10).map(|row| 2 * slash[2 * row].count_ones()).sum();
    assert!(dots > unrounded / 2, "{dots} {unrounded}");
}

#[test]
fn mosaics_are_six_blocks_and_separated_ones_lose_an_edge() {
    let full = &GLYPHS[mosaic(0x7F, false)];
    assert!(full.iter().all(|&line| line == 0b1111_1111_1111));
    let separated = &GLYPHS[mosaic(0x7F, true)];
    assert_eq!(
        separated[0], 0b0011_1100_1111,
        "each block's left column gone"
    );
    assert_eq!(separated[3], separated[0], "its second row");
    assert_eq!(separated[4], 0, "the top band's bottom row gone");
    assert_eq!(separated[6], separated[0], "the middle band");
    assert_eq!(mosaic(0x21, false), mosaic(0x21, false));
    assert_ne!(
        mosaic(0x61, false),
        mosaic(0x21, false),
        "bit 6 is the sixth block"
    );
}

#[test]
fn a_colour_code_takes_effect_on_the_next_cell_and_hold_keeps_the_mosaic() {
    let mut row = Attributes::LINE_START;
    let shown = row.character(0x91);
    assert_eq!(shown.cell, None, "a control code shows nothing unheld");
    assert_eq!(
        shown.attributes.foreground, 7,
        "the red takes effect after it"
    );
    let block = row.character(0xFF);
    assert_eq!(block.cell, Some(mosaic(0x7F, false)));
    assert_eq!(block.attributes.foreground, 1);
    let _ = row.character(0x9E);
    let held = row.character(0x92);
    assert_eq!(held.cell, Some(mosaic(0x7F, false)), "the held mosaic");
    assert!(row.character(0x8D).double_height);
    assert!(row.double);
    assert!(is_double_height(0x0D) && is_double_height(0x8D));
    assert!(!is_double_height(0x0C) && !is_double_height(b'M'));
}
