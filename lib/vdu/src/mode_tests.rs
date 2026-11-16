use super::*;

#[test]
fn a_row_holds_its_pixels_as_a_character_sees_them() {
    for (number, mode) in MODES.iter().enumerate() {
        let bits = u32::from(mode.width) << mode.log2_bpc;
        assert_eq!(u32::from(mode.line_length) * 8, bits, "mode {number}");
        assert!(mode.log2_bpc >= mode.log2_bpp, "mode {number}");
        let rows = u32::from(mode.line_length) * u32::from(mode.height);
        assert!(mode.screen_size >= rows, "mode {number}");
    }
}

#[test]
fn the_text_fills_the_screen_in_characters_of_its_mode_s_size() {
    for (number, mode) in MODES.iter().enumerate() {
        let across = if mode.flags.has(Flags::TELETEXT) {
            16
        } else {
            8
        };
        assert_eq!(
            u16::from(mode.columns) * across,
            mode.width,
            "mode {number}"
        );
        let gap = if mode.flags.has(Flags::GAP) { 2 } else { 0 };
        let double = if mode.flags.has(Flags::DOUBLE_VERTICAL) {
            2
        } else {
            1
        };
        assert_eq!(
            u16::from(mode.rows) * (8 + gap) * double,
            mode.height,
            "mode {number}"
        );
    }
}

#[test]
fn a_graphic_mode_has_the_colours_its_depth_holds() {
    for (number, mode) in MODES.iter().enumerate().filter(|(_, mode)| mode.graphic()) {
        let colours = match mode.log2_bpp {
            0..=2 => (1u16 << (1 << mode.log2_bpp)) - 1,
            _ if mode.flags.has(Flags::FULL_PALETTE) => 255,
            _ => 63,
        };
        assert_eq!(u16::from(mode.ncolour), colours, "mode {number}");
    }
}

#[test]
fn the_text_and_teletext_modes_are_not_graphic() {
    let text: [usize; 3] = [3, 6, 7];
    for (number, mode) in MODES.iter().enumerate() {
        assert_eq!(mode.graphic(), !text.contains(&number), "mode {number}");
    }
    let teletext = MODES[7];
    assert!(teletext.flags.has(Flags::TELETEXT));
    assert!(teletext.flags.has(Flags::FULL_PALETTE));
    assert!(!Flags::TELETEXT.has(Flags::TELETEXT.union(Flags::FULL_PALETTE)));
    assert_eq!(
        (teletext.width, teletext.height, teletext.ncolour),
        (640, 500, 255)
    );
    assert!(MODES[3].flags.has(Flags::BBC_GAP) && MODES[6].flags.has(Flags::BBC_GAP));
}

#[test]
fn a_mode_is_known_by_its_number_to_53() {
    let pick = |number| {
        numbered(number).map(|mode| {
            (
                mode.width,
                mode.height,
                mode.log2_bpp,
                mode.x_eig,
                mode.y_eig,
            )
        })
    };
    assert_eq!(pick(0), Some((640, 256, 0, 1, 2)));
    assert_eq!(pick(15), Some((640, 256, 3, 1, 2)));
    assert_eq!(pick(28), Some((640, 480, 3, 1, 1)));
    assert_eq!(pick(47), Some((360, 480, 3, 2, 2)), "as RISC OS 5 has it");
    assert_eq!(pick(53), Some((320, 240, 3, 2, 2)));
    assert_eq!(pick(54), None);
    assert_eq!(pick(u32::MAX), None);
}
