use super::*;

fn text(flags: u8) -> Text {
    let mut t = Text::for_screen(40, 32, 8, flags);
    t.window = Window {
        left: 5,
        bottom: 20,
        right: 30,
        top: 8,
    };
    t
}

#[test]
fn the_cursor_s_axes_turn_with_vdu_23_16() {
    let plain = text(0);
    assert_eq!(plain.extent(), (26, 13));
    assert_eq!(plain.cell(0, 0), (5, 8));
    assert_eq!(plain.cell(3, 2), (8, 10));
    let leftward = text(flags::X_REVERSED);
    assert_eq!(leftward.cell(0, 0), (30, 8), "home at the top right");
    let swapped = text(flags::SWAPPED);
    assert_eq!(swapped.extent(), (13, 26));
    assert_eq!(swapped.cell(2, 0), (5, 10), "characters run down");
    let up = text(flags::Y_REVERSED);
    assert_eq!(up.cell(0, 0), (5, 20), "lines advance up from the bottom");
}

#[test]
fn a_screen_cell_maps_back_along_the_axes() {
    for flags in [
        0,
        flags::X_REVERSED,
        flags::SWAPPED,
        flags::SWAPPED | flags::Y_REVERSED,
    ] {
        let t = text(flags);
        let (w, h) = t.extent();
        for (x, y) in [(0, 0), (w - 1, 0), (2, h - 1)] {
            let (column, row) = t.cell(x, y);
            assert_eq!(t.along(column, row), Some((x, y)), "flags {flags}");
        }
        assert_eq!(t.along(0, 0), None);
    }
}

#[test]
fn a_newline_past_the_last_line_moves_the_contents_against_it() {
    assert_eq!(text(0).shift_for_newline(), Shift::Up);
    assert_eq!(text(flags::Y_REVERSED).shift_for_newline(), Shift::Down);
    assert_eq!(text(flags::SWAPPED).shift_for_newline(), Shift::Left);
}

#[test]
fn a_flag_change_keeps_the_cursor_where_it_is() {
    let mut t = text(0);
    t.x = 3;
    t.y = 2;
    let at = t.cursor_cell();
    t.set_flags(flags::X_REVERSED);
    assert_eq!(t.cursor_cell(), at);
    assert_eq!((t.x, t.y), (22, 2));
}

#[test]
fn a_glyph_line_puts_its_leftmost_pixel_lowest() {
    let mut out = [0u8; 1];
    glyph_line(&mut out, 0x80, 1, 0, 0, None);
    assert_eq!(out, [0x01]);
    let mut out = [0u8; 4];
    glyph_line(&mut out, 0xC0, 0x7, 0x2, 2, None);
    assert_eq!(out, [0x77, 0x22, 0x22, 0x22]);
    glyph_line(&mut out, 0xFF, 0x7, 0x2, 2, Some(0x3));
    assert_eq!(out, [0x33; 4], "a gap's line");
}
