use super::teletext::{encode, Saa5050, SLOTS};

/// A chip on scan line `line` of a row, the line's attributes fresh.
fn at_line(line: u8) -> Saa5050 {
    let mut chip = Saa5050::new();
    for _ in 0..line {
        chip.end_line();
    }
    chip.start_line();
    chip
}

/// Draw `bytes` along a line, returning each character's slots.
fn draw(chip: &mut Saa5050, bytes: &[u8]) -> alloc::vec::Vec<[u8; SLOTS]> {
    bytes
        .iter()
        .map(|&byte| {
            let mut out = [0; SLOTS];
            chip.character(byte, false, &mut out);
            out
        })
        .collect()
}

fn colours(cell: &[u8; SLOTS]) -> (u8, u8) {
    (cell[0] & 7, (cell[0] >> 3) & 7)
}

/// Slots where a dot shows: some coverage of a foreground unlike the
/// background.
fn lit_slots(cell: &[u8; SLOTS]) -> usize {
    cell.iter()
        .filter(|&&slot| slot >> 6 != 0 && slot & 7 != (slot >> 3) & 7)
        .count()
}

#[test]
fn a_letter_lights_its_dots_on_its_rows() {
    let mut chip = at_line(3);
    let cells = draw(&mut chip, b"A");
    // Row 3 of 'A' is "#...#": its outer columns are lit.
    assert_eq!(cells[0][0] >> 6, 3, "{:02X?}", cells[0]);
    assert_eq!(cells[0][7] >> 6, 0, "{:02X?}", cells[0]);
}

#[test]
fn a_colour_takes_effect_after_its_own_cell_which_shows_as_a_space() {
    let mut chip = at_line(3);
    let cells = draw(&mut chip, &[b'A', 0x01, b'A']);
    assert_eq!(colours(&cells[0]).0, 7, "white to start a line");
    assert_eq!(cells[1], [encode(0, 0, 3); SLOTS]);
    assert_eq!(colours(&cells[2]).0, 1);
}

#[test]
fn new_background_takes_the_foreground_on_its_own_cell_and_black_ends_it() {
    let mut chip = at_line(0);
    let cells = draw(&mut chip, &[0x03, 0x1D, b' ', 0x1C, b' ']);
    assert_eq!(cells[1], [encode(3, 3, 3); SLOTS]);
    assert_eq!(colours(&cells[2]).1, 3);
    assert_eq!(colours(&cells[4]).1, 0);
}

#[test]
fn a_full_mosaic_fills_its_cell_and_separated_leaves_gaps() {
    let mut chip = at_line(1);
    let cells = draw(&mut chip, &[0x17, 0x7F, 0x1A, 0x7F]);
    assert_eq!(lit_slots(&cells[1]), SLOTS);
    assert!(lit_slots(&cells[3]) < SLOTS);
    assert!(lit_slots(&cells[3]) > 0);
}

#[test]
fn capitals_blast_through_graphics_mode() {
    let mut graphics = at_line(3);
    let mut text = at_line(3);
    assert_eq!(
        draw(&mut graphics, &[0x17, b'A'])[1],
        draw(&mut text, &[0x07, b'A'])[1]
    );
}

#[test]
fn held_graphics_show_the_last_mosaic_in_place_of_control_codes() {
    let mut chip = at_line(1);
    let cells = draw(&mut chip, &[0x17, 0x7F, 0x1E, 0x11, 0x1F, 0x12]);
    assert_eq!(lit_slots(&cells[2]), SLOTS, "hold takes effect at once");
    assert_eq!(
        colours(&cells[3]).0,
        7,
        "a colour change is held in its old colour"
    );
    assert_eq!(
        (lit_slots(&cells[4]), colours(&cells[4]).0),
        (SLOTS, 1),
        "release takes effect after its cell"
    );
    assert_eq!(lit_slots(&cells[5]), 0);
    // Changing to alphanumerics forgets the held mosaic.
    let mut chip = at_line(1);
    let cells = draw(&mut chip, &[0x17, 0x7F, 0x1E, 0x01, 0x1C]);
    assert_eq!(lit_slots(&cells[4]), 0);
}

#[test]
fn concealed_characters_are_drawn_as_spaces() {
    let mut chip = at_line(3);
    let cells = draw(&mut chip, &[0x18, b'A']);
    assert_eq!(lit_slots(&cells[1]), 0);
}

#[test]
fn flashing_characters_are_hidden_for_a_quarter_of_each_cycle() {
    let shown = |fields: u32| {
        let mut chip = Saa5050::new();
        for _ in 0..fields {
            chip.set_dew(true);
            chip.set_dew(false);
        }
        for _ in 0..3 {
            chip.end_line();
        }
        chip.start_line();
        lit_slots(&draw(&mut chip, &[0x08, b'A'])[1]) > 0
    };
    let visible = (0..64).filter(|&fields| shown(fields)).count();
    assert_eq!(visible, 48);
}

#[test]
fn a_double_height_row_shows_its_top_then_hides_normal_text_below() {
    let mut chip = Saa5050::new();
    chip.start_line();
    let top: alloc::vec::Vec<_> = (0..10)
        .map(|_| {
            let cells = draw(&mut chip, &[0x0D, b'A', 0x0C, b'A']);
            chip.end_line();
            chip.start_line();
            cells
        })
        .collect();
    let lower: alloc::vec::Vec<_> = (0..10)
        .map(|_| {
            let cells = draw(&mut chip, &[0x0D, b'A', 0x0C, b'A']);
            chip.end_line();
            chip.start_line();
            cells
        })
        .collect();
    let lit = |rows: &[alloc::vec::Vec<[u8; SLOTS]>], cell: usize| {
        rows.iter()
            .map(|cells| lit_slots(&cells[cell]))
            .sum::<usize>()
    };
    assert!(
        lit(&top, 1) > 0 && lit(&lower, 1) > 0,
        "both halves of the tall letter"
    );
    assert_ne!(top[0][1], lower[0][1], "different halves");
    assert!(lit(&top, 3) > 0, "normal height on the upper row");
    assert_eq!(lit(&lower, 3), 0, "hidden on the lower row");
}
