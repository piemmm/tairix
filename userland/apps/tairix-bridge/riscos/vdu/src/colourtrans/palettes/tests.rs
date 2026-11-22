use super::*;

/// Mode 12's default palette, its flashing colours in their first state.
fn mode_12() -> Against {
    let words: alloc::vec::Vec<u32> = default_words(2, false)
        .iter()
        .take(16)
        .map(|[first, _]| *first)
        .collect();
    Against::of_words(Holding::Palette(2), &words)
}

#[test]
fn a_32k_table_takes_the_nearest_but_the_last_entry_for_a_palette_s_own_colour() {
    // As RISC OS 5 built mode 12's: black's own cell is the flashing 8, the
    // reds below pure red are colour 1, pure red's cell is 9.
    let table = mode_12().inverse().expect("room");
    assert_eq!(table.len(), 1 << 15);
    assert_eq!(table[0], 8);
    assert!(table[1..16].iter().all(|&n| n == 0));
    assert!(table[16..31].iter().all(|&n| n == 1));
    assert_eq!(table[31], 9);
    assert_eq!(table[0x7FFF], 15);
}

#[test]
fn a_palettised_mode_s_pixel_is_a_byte_and_a_deep_one_s_two_or_four() {
    assert_eq!(mode_12().pixel_bytes(), 1);
    let sixteen = Against::of_words(Holding::Direct(Direct::of_parts(4, 0xFFFF, 0)), &[]);
    let thirty_two = Against::of_words(Holding::Direct(Direct::of_parts(5, u32::MAX, 0)), &[]);
    assert_eq!([sixteen.pixel_bytes(), thirty_two.pixel_bytes()], [2, 4]);
    assert!(sixteen.palette().is_none() && sixteen.inverse().is_none());
}

#[test]
fn a_grouped_palette_is_matched_at_vidc1_s_four_bits_a_channel() {
    // The 256 words RISC OS 5 was given in mode 15: its nearest to
    // &30,&20,&10 and its furthest from &00,&80,&FF as it answered them.
    let words: alloc::vec::Vec<u32> = (0..256u32)
        .map(|i| (((i * 0x3B5) ^ (i << 7)) & 0xFF_FFFF) << 8)
        .collect();
    let grouped = Against::of_words(Holding::Grouped, &words[..16]);
    let near = grouped.colour_number(Rgb::new(0x30, 0x20, 0x10), Loadings::DEFAULT, Look::Nearest);
    let far = grouped.colour_number(Rgb::new(0, 0x80, 0xFF), Loadings::DEFAULT, Look::Furthest);
    assert_eq!([near, far], [0x0B, 0x17]);
}

#[test]
fn only_a_grouped_palette_shuffles_its_gcols_though_set_gcol_reports_both_shuffled() {
    let grouped = Against::of_words(Holding::Grouped, &[0; 16]);
    let full = Against::of_words(Holding::Full, &[0; 256]);
    assert_eq!([grouped.gcol_of(0x15), full.gcol_of(0x15)], [0x0D, 0x15]);
    assert_eq!(
        [grouped.reported_gcol(0x15), full.reported_gcol(0x15)],
        [0x0D, 0x0D]
    );
    assert_eq!(mode_12().reported_gcol(9), 9);
}
