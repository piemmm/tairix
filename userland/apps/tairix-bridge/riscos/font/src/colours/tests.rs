use super::*;

fn colours(background: u32, foreground: u32, offset: i32) -> Colours {
    Colours {
        font: 1,
        background,
        foreground,
        offset,
        rgb: None,
    }
}

#[test]
fn each_offset_has_risc_os_s_thresholds_and_only_offset_0_s_can_change() {
    let mut palette = Palette::INITIAL;
    let table = |p: &Palette, offset| p.thresholds(offset).collect::<alloc::vec::Vec<u8>>();
    assert_eq!(table(&palette, 0), [5]);
    assert_eq!(table(&palette, 1), [4, 9]);
    assert_eq!(table(&palette, -7), [2, 4, 6, 8, 9, 11, 13, 15]);
    assert_eq!(
        table(&palette, 14),
        (1..=15).collect::<alloc::vec::Vec<u8>>()
    );
    palette.threshold = 9;
    assert_eq!(table(&palette, 0), [9]);
    assert_eq!(table(&palette, 1), [4, 9]);
    assert!(table(&palette, 15).is_empty());
}

#[test]
fn up_to_sixteen_colours_the_levels_step_up_the_foreground_s_run_as_risc_os_5_30_s_do() {
    let palette = Palette::INITIAL;
    for (background, foreground, offset, recorded) in [
        (
            0,
            1,
            14,
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        ),
        (0, 7, 0, [0, 0, 0, 0, 0, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7]),
        (0, 1, 1, [0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2]),
        (0, 1, 6, [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7]),
        (3, 5, 2, [3, 3, 3, 3, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7]),
        (
            0,
            15,
            -14,
            [0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1],
        ),
        (2, 9, -3, [2, 2, 2, 9, 9, 9, 8, 8, 8, 7, 7, 7, 6, 6, 6, 6]),
        (0, 1, 3, [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 4]),
    ] {
        let table = palette.table(&colours(background, foreground, offset), 2);
        assert_eq!(table, Ok(recorded), "{background} {foreground} {offset}");
    }
}

#[test]
fn a_deeper_mode_paints_the_foreground_s_entry_at_the_depth_it_was_matched_at() {
    let mut palette = Palette::INITIAL;
    assert_eq!(
        palette.table(&colours(0, 1, 6), 3),
        Err(Unpainted::Undefined),
        "nothing set"
    );
    let table = core::array::from_fn(|i| u32::try_from(i).unwrap_or(0) + 0x40);
    palette.entries[1] = Some(Entry { log2_bpp: 3, table });
    assert_eq!(
        palette.table(&colours(7, 1, 0), 3),
        Ok(table),
        "the background and offset do not matter"
    );
    assert_eq!(
        palette.table(&colours(0, 1, 6), 5),
        Ok([0; LEVELS]),
        "another depth's"
    );
    assert_eq!(
        palette.table(&colours(0, 2, 6), 3),
        Err(Unpainted::Undefined)
    );
    assert_eq!(
        palette.table(&colours(0, 17, 6), 3),
        Err(Unpainted::Undefined),
        "a foreground past the sixteen has no entry, not entry 1's"
    );
}

#[test]
fn in_sixteen_colours_or_fewer_a_colour_outside_the_mode_s_is_refused() {
    let palette = Palette::INITIAL;
    for (background, foreground, offset, log2_bpp, valid) in [
        (0, 2, 14, 2, false),
        (16, 1, 1, 2, false),
        (0, 13, -14, 2, false),
        (0, 15, -14, 2, true),
        (15, 1, 14, 2, true),
        (0, 15, 0, 2, true),
        (0, 16, 0, 2, false),
        (0, 1, 2, 1, true),
        (0, 1, 3, 1, false),
        (0, 3, 0, 1, true),
        (0, 4, 0, 1, false),
    ] {
        let table = palette.table(&colours(background, foreground, offset), log2_bpp);
        assert_eq!(
            table.is_ok(),
            valid,
            "{background} {foreground} {offset} at {log2_bpp}"
        );
        if !valid {
            assert_eq!(table, Err(Unpainted::Invalid));
        }
    }
}

#[test]
fn shades_step_in_sixteen_sixteenths_each_rounded_down_as_risc_os_5_30_s_do() {
    let red = |r: u32| r << 8;
    let up: alloc::vec::Vec<u32> = (0..=15).map(|k| shade(0, red(100), k, 15) >> 8).collect();
    assert_eq!(
        up,
        [0, 6, 13, 19, 26, 33, 39, 46, 53, 59, 66, 73, 79, 86, 93, 100]
    );
    let down: alloc::vec::Vec<u32> = (0..=15).map(|k| shade(red(100), 0, k, 15) >> 8).collect();
    assert_eq!(
        down,
        [100, 93, 86, 79, 73, 66, 59, 53, 46, 39, 33, 26, 19, 13, 6, 0]
    );
    let eighths: alloc::vec::Vec<u32> = (0..=8).map(|k| shade(0, red(20), k, 8) >> 8).collect();
    assert_eq!(eighths, [0, 2, 5, 7, 10, 12, 15, 17, 20]);
    let blue_to_red = |k| shade(0xFF00_0000, 0x0000_FF00, k, 4);
    assert_eq!(
        [1, 2, 3, 4].map(blue_to_red),
        [0xBF00_3F00, 0x7F00_7F00, 0x3F00_BF00, 0x0000_FF00]
    );
}

#[test]
fn without_its_magic_word_a_colour_s_channels_are_their_top_four_bits_twice() {
    assert_eq!(nibbled(0x1234_5600), 0x1133_5500);
    assert_eq!(nibbled(0xFEDC_BA00), 0xFFDD_BB00);
    assert_eq!(nibbled(0x0000_12FF), 0x0000_1100);
}

mod session {
    use alloc::vec::Vec;

    use tairix_riscos_filing::session::Session;

    use crate::fixture::{call, deep_mode, fonts, mode, BLOCK, WHITE};

    use crate::module::call::{READ_COLOUR_TABLE, SET_FONT_COLOURS, SET_PALETTE};
    const TRUE: u32 = tairix_riscos_kernel::swi::font::TRUE_COLOURS;

    /// The colour table read, or its error's number.
    fn table(s: &mut Session) -> Result<Vec<u8>, u32> {
        call(s, READ_COLOUR_TABLE, &[0, BLOCK])?;
        let mut bytes = [0u8; 16];
        s.kernel.os().read_bytes(BLOCK, &mut bytes).expect("read");
        Ok(bytes.to_vec())
    }

    fn palette(s: &mut Session, registers: [u32; 6]) -> Vec<u8> {
        let mut low = [0u32; 7];
        low[1..7].copy_from_slice(&registers);
        call(s, SET_PALETTE, &low).expect("the palette");
        table(s).expect("a table")
    }

    #[test]
    fn in_256_colours_the_shades_are_colour_trans_s_numbers_as_risc_os_5_30_s_were() {
        let mut s = fonts();
        mode(&mut s, 28);
        assert_eq!(
            palette(&mut s, [0, 1, 14, WHITE, 0, TRUE]),
            [
                0xFF, 0xFE, 0xFD, 0xFC, 0xD3, 0xD2, 0xD1, 0xD0, 0x2F, 0x2E, 0x2D, 0x2C, 0x03, 0x02,
                0x01, 0x00
            ]
        );
        assert_eq!(
            palette(&mut s, [0, 1, 6, 0xFF00_0000, 0x0000_FF00, TRUE]),
            [
                0x88, 0x88, 0x89, 0x89, 0x8C, 0x8C, 0x90, 0x90, 0x90, 0x90, 0x1C, 0x1C, 0x1C, 0x1C,
                0x15, 0x15
            ]
        );
        assert_eq!(
            palette(&mut s, [3, 9, 2, 0x00FF_0000, WHITE, TRUE]),
            [
                0x62, 0x62, 0x62, 0x62, 0x6E, 0x6E, 0x6E, 0x6E, 0xF3, 0xF3, 0xF3, 0xF3, 0xFF, 0xFF,
                0xFF, 0xFF
            ]
        );
        call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
        assert_eq!(
            table(&mut s).map(|t| t[2]),
            Ok(0x89),
            "the foreground's entry, whatever the offset"
        );
        call(&mut s, SET_FONT_COLOURS, &[0, 0, 9, 2]).expect("set");
        assert_eq!(table(&mut s).map(|t| t[8]), Ok(0xF3));
        call(&mut s, SET_FONT_COLOURS, &[0, 3, 10, 1]).expect("set");
        assert_eq!(table(&mut s), Err(0x205), "no entry for 10");
    }

    #[test]
    fn in_true_colour_the_shades_step_as_risc_os_5_30_s_did() {
        let mut s = fonts();
        deep_mode(&mut s, 5);
        assert_eq!(
            palette(&mut s, [0, 1, 14, 0, 100 << 8, TRUE]),
            [0, 6, 13, 19, 26, 33, 39, 46, 53, 59, 66, 73, 79, 86, 93, 100]
        );
        assert_eq!(
            palette(&mut s, [0, 1, 6, 0, 100 << 8, TRUE]),
            [0, 0, 14, 14, 28, 28, 42, 42, 57, 57, 71, 71, 85, 85, 100, 100]
        );
        assert_eq!(
            palette(&mut s, [0, 1, 14, 0x12 << 8, 0xFE << 8, 0]),
            [
                0x11, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x8F, 0x9F, 0xAF, 0xBF, 0xCF, 0xDF,
                0xEF, 0xFF
            ],
            "without its magic word each channel's top nibble is repeated"
        );
        deep_mode(&mut s, 4);
        assert_eq!(
            palette(&mut s, [0, 1, 14, 0, 0xF8 << 8, TRUE]),
            [0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 31]
        );
    }

    #[test]
    fn an_entry_read_at_another_depth_than_its_own_is_nought() {
        let mut s = fonts();
        mode(&mut s, 28);
        let shades = palette(&mut s, [0, 2, 6, 0xFF00_0000, 0x0000_FF00, TRUE]);
        deep_mode(&mut s, 5);
        call(&mut s, SET_FONT_COLOURS, &[0, 0, 2, 6]).expect("set");
        assert_eq!(table(&mut s), Ok(alloc::vec![0; 16]));
        call(&mut s, SET_FONT_COLOURS, &[0, 0, 3, 6]).expect("set");
        assert_eq!(table(&mut s), Err(0x205));
        mode(&mut s, 28);
        call(&mut s, SET_FONT_COLOURS, &[0, 0, 2, 6]).expect("set");
        assert_eq!(table(&mut s), Ok(shades), "back at its own depth");
    }

    #[test]
    fn up_to_sixteen_colours_the_palette_is_laid_between_the_colours() {
        let mut s = fonts();
        mode(&mut s, 12);
        palette(&mut s, [0, 1, 14, WHITE, 0, 0]);
        let mut os = s.kernel.os();
        let shown: Vec<u32> = (0..16)
            .map(|logical| {
                let mut regs = tairix_riscos_kernel::os::Regs::default();
                regs.r[..2].copy_from_slice(&[logical, 16]);
                os.perform(
                    tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::READ_PALETTE,
                    &mut regs,
                )
                .expect("read");
                regs.r[2] >> 8
            })
            .collect();
        let expected: Vec<u32> = (0..16).map(|k| (255 - 17 * k) * 0x01_0101).collect();
        assert_eq!(shown, expected);
    }
}
