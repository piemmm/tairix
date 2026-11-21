use super::*;

#[test]
fn a_vga_monitor_shows_the_modes_risc_os_5_shows_on_one() {
    for mode in [0, 6, 12, 15, 25, 28, 41, 47, 53] {
        assert!(shown_on(mode, 3), "{mode}");
    }
    for mode in [16, 18, 23, 29, 33, 37] {
        assert!(!shown_on(mode, 3), "{mode}");
    }
}

#[test]
fn a_mode_the_monitor_lacks_becomes_its_recorded_substitute() {
    assert_eq!(substitute(18, 3), Some(Spec::Number(25)));
    assert_eq!(substitute(31, 3), Some(Spec::Number(27)));
    let Some(Spec::Selector(sixteen)) = substitute(16, 3) else {
        panic!("a selector");
    };
    assert_eq!(
        (
            sixteen.width,
            sixteen.height,
            sixteen.log2_bpp,
            sixteen.frame_rate
        ),
        (640, 480, 2, u32::MAX)
    );
    assert_eq!(sixteen.pairs(), [(0, 0), (3, 15), (4, 1), (5, 2)]);
    let Some(Spec::Selector(hi_res)) = substitute(23, 3) else {
        panic!("a selector");
    };
    assert_eq!(
        hi_res.pairs()[0],
        (0, 0x20),
        "the double-height flag kept, hi-res mono dropped"
    );
    let Some(Spec::Selector(wide)) = substitute(37, 3) else {
        panic!("a selector");
    };
    assert_eq!(
        wide.frame_rate, 60,
        "a multisync mode's substitute runs at sixty"
    );
}

#[test]
fn a_selector_s_text_follows_its_flags_and_its_timings_the_monitor() {
    let double = Selector::new(640, 480, 0, u32::MAX)
        .with([(var::MODE_FLAGS, flag::DOUBLE_VERTICAL)])
        .expect("held");
    let format = double.format().expect("described");
    assert_eq!(format.rows(), 30);
    assert!(timed(&double, &format, 3));
    let big = Selector::new(800, 600, 3, u32::MAX);
    let format = big.format().expect("described");
    assert!(!timed(&big, &format, 3), "no VGA timings for 800 by 600");
    let mut buffer = [0; SELECTOR_BYTES as usize];
    let words: alloc::vec::Vec<u32> = big
        .bytes(&mut buffer)
        .chunks(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    assert_eq!(
        words,
        [1, 800, 600, 3, u32::MAX, u32::MAX],
        "its words, then the end"
    );
}

#[test]
fn a_selector_holds_as_many_pairs_as_a_copy_has_room_for() {
    let full = Selector::new(640, 480, 2, u32::MAX).with((0..32).map(|_| (var::N_COLOUR, 15)));
    assert!(full.is_some());
    assert_eq!(
        full.and_then(|full| full.with([(var::N_COLOUR, 15)])),
        None,
        "one more is refused"
    );
}

#[test]
fn a_selector_naming_variables_no_screen_can_have_is_refused() {
    let format = |pairs: &[(u32, u32)]| Format::selector(640, 480, 3, pairs.iter().copied());
    assert!(format(&[]).is_some());
    for (index, value) in [
        (var::X_WIND_LIMIT, u32::MAX),
        (var::Y_WIND_LIMIT, u32::MAX),
        (var::SCR_R_COL, u32::MAX),
        (var::SCR_R_COL, 80),
        (var::SCR_B_ROW, 60),
        (var::LINE_LENGTH, 100),
        (var::LOG2_BPC, 32),
        (var::LOG2_BPC, 0),
        (var::X_EIG, 4),
        (var::Y_EIG, 32),
        (var::N_COLOUR, 256),
    ] {
        assert_eq!(
            format(&[(index, value)]),
            None,
            "variable {index} as {value:#x}"
        );
    }
    assert!(
        format(&[(var::SCR_R_COL, 79), (var::SCR_B_ROW, 59)]).is_some(),
        "text that fits"
    );
    assert!(
        format(&[(var::LINE_LENGTH, 1024)]).is_some(),
        "lines longer than their pixels"
    );
    let teletext = Format::selector(640, 480, 3, [(var::MODE_FLAGS, flag::TELETEXT)]);
    assert_eq!(teletext, None, "Teletext is drawn in sixteen colours");
    let page = Format::selector(
        640,
        500,
        2,
        [
            (var::MODE_FLAGS, flag::TELETEXT | flag::GAP),
            (var::N_COLOUR, 15),
        ],
    )
    .expect("a page");
    assert_eq!(
        (page.columns(), page.rows()),
        (40, 25),
        "as many sixteen by twenty cells as fit"
    );
    let gap = Format::selector(
        640,
        250,
        2,
        [(var::MODE_FLAGS, flag::GAP), (var::N_COLOUR, 15)],
    )
    .expect("a gap mode");
    assert_eq!(gap.rows(), 25, "rows of ten lines");
}
