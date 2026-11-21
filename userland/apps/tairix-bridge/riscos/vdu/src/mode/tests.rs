use super::*;

fn numbered(n: u32) -> Format {
    Format::numbered(tairix_vdu::mode::numbered(n).expect("a numbered mode"))
}

#[test]
fn a_numbered_mode_reads_as_its_table_and_the_current_screen_as_it_is() {
    let three = numbered(3);
    assert_eq!(three.var(var::SCREEN_SIZE), Some(40_960));
    assert_eq!(
        three.current_var(var::SCREEN_SIZE),
        Some(40_000),
        "the lines it shows"
    );
    assert_eq!(three.current_var(var::MODE_FLAGS), Some(0x20D));
    assert_eq!(three.var(var::MODE_FLAGS), Some(0x0D));
    let seven = numbered(7);
    assert_eq!(
        seven.current_var(var::SCREEN_SIZE),
        Some(320_000),
        "two frames"
    );
    assert_eq!(seven.current_var(var::MODE_FLAGS), Some(0x27));
    assert_eq!(numbered(25).current_var(var::MODE_FLAGS), Some(0x200));
    assert_eq!(numbered(28).var(var::MIN_SCREEN_BANKS), Some(0));
    assert_eq!(numbered(28).var(14), None);
}

#[test]
fn the_text_cell_follows_the_gap_double_and_teletext_flags() {
    let cell = |n| numbered(n).cell();
    assert_eq!(
        cell(12),
        Cell {
            size_x: 8,
            size_y: 8,
            space_y: 8
        }
    );
    assert_eq!(
        cell(3),
        Cell {
            size_x: 8,
            size_y: 8,
            space_y: 10
        }
    );
    assert_eq!(
        cell(7),
        Cell {
            size_x: 16,
            size_y: 16,
            space_y: 20
        }
    );
    assert_eq!(
        cell(23),
        Cell {
            size_x: 8,
            size_y: 16,
            space_y: 16
        }
    );
}

#[test]
fn a_selector_takes_a_standard_mode_s_variables_then_its_pairs() {
    let plain = Format::selector(640, 480, 3, []).expect("described");
    assert_eq!(
        (0..VARIABLES)
            .map(|n| plain.var(n))
            .collect::<alloc::vec::Vec<_>>(),
        [0x80, 79, 59, 255, 1, 1, 640, 307_200, 0, 3, 3, 639, 479, 0]
            .map(Some)
            .to_vec()
    );
    let wide = Format::selector(640, 200, 2, [(var::N_COLOUR, 15), (var::SCR_B_ROW, 9)])
        .expect("described");
    assert_eq!(wide.y_eig(), 2, "a short screen's tall pixels");
    assert_eq!(wide.rows(), 10);
    assert_eq!(Format::selector(640, 480, 3, [(var::LOG2_BPP, 3)]), None);
    assert_eq!(Format::selector(640, 480, 3, [(13, 0)]), None);
    assert_eq!(Format::selector(640, 480, 6, []), None);
}

#[test]
fn a_sprite_mode_word_decides_its_depth_and_eigen_factors() {
    let word = (4 << 27) | (90 << 14) | (90 << 1) | 1;
    let sprite = Format::sprite_word(word).expect("a type");
    assert_eq!(sprite.var(var::N_COLOUR), Some(255));
    assert_eq!(
        (sprite.var(var::X_EIG), sprite.var(var::Y_EIG)),
        (Some(1), Some(1))
    );
    assert_eq!(sprite.var(var::LINE_LENGTH), None);
    let wide = (6 << 27) | (45 << 14) | (180 << 1) | 1;
    let sprite = Format::sprite_word(wide).expect("a type");
    assert_eq!(
        (sprite.var(var::X_EIG), sprite.var(var::Y_EIG)),
        (Some(0), Some(2))
    );
    assert_eq!(sprite.var(var::N_COLOUR), Some(0xFFFF_FFFF));
    assert_eq!(Format::sprite_word((7 << 27) | 1), None);
    assert_eq!(
        Format::sprite_word((1 << 27) | (91 << 1) | (90 << 14) | 1),
        None
    );
}
