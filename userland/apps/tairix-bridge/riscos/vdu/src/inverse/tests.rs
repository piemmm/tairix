use super::*;
use crate::colourtrans::matching::{search, Look};

/// The table by its definition: every cell against every colour.
fn by_definition(colours: &[Rgb]) -> Vec<u8> {
    let five = |colour: Rgb| colour.to_array().map(|c| i32::from(c >> 3));
    let mut table: Vec<u8> = (0..CELLS)
        .map(|cell| {
            let at = [0, 5, 10].map(|shift| i32::try_from((cell >> shift) & 31).unwrap_or(0));
            let mut best = (i32::MAX, 0u8);
            for (index, &colour) in (0..=u8::MAX).zip(colours) {
                let near: i32 = at
                    .iter()
                    .zip(five(colour))
                    .map(|(a, b)| (a - b).abs())
                    .sum();
                if near < best.0 {
                    best = (near, index);
                }
            }
            best.1
        })
        .collect();
    for (index, &colour) in (0..=u8::MAX).zip(colours) {
        table[fifteen(colour)] = index;
    }
    table
}

/// `count` colours from a seeded sequence.
fn colours(count: usize, seed: u32) -> Vec<Rgb> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            Rgb::new(r, g, b)
        })
        .collect()
}

#[test]
fn the_sweeps_give_the_table_by_its_definition() {
    for (count, seed) in [(2, 1), (4, 7), (16, 99), (16, 3), (256, 12345), (256, 6)] {
        let palette = colours(count, seed);
        assert!(
            table(&palette) == Some(by_definition(&palette)),
            "{count} colours, seed {seed}"
        );
    }
}

#[test]
fn a_custom_palette_s_cells_are_risc_os_5_s() {
    // Cells of the table RISC OS 5's InverseTable built for this palette.
    let palette: Vec<Rgb> = (0..256u32)
        .map(|i| Rgb::from_riscos_word((((i * 0x3B5) ^ (i << 9)) & 0xFF_FFFF) << 8))
        .collect();
    let built = table(&palette).expect("room");
    for (cell, number) in [
        (0, 0),
        (2047, 0xB5),
        (4094, 0x59),
        (10235, 0x2D),
        (16376, 0x86),
        (30705, 0xB3),
    ] {
        assert_eq!(built[cell], number, "{cell}");
    }
}

#[test]
fn equally_near_colours_take_the_first_but_a_colour_s_own_cell_its_last() {
    let black_twice = [Rgb::BLACK, Rgb::new(255, 0, 0), Rgb::BLACK];
    let built = table(&black_twice).expect("room");
    assert_eq!(built[0], 2, "black's own cell");
    assert_eq!(built[1], 0, "a near black the first black");
    // Both eight steps from the cell, the one in red alone and the other in
    // green: whichever comes first wins.
    let cell = fifteen(Rgb::new(128, 0, 0));
    let (below, beside) = (Rgb::new(64, 0, 0), Rgb::new(128, 64, 0));
    assert_eq!(table(&[below, beside]).expect("room")[cell], 0);
    assert_eq!(table(&[beside, below]).expect("room")[cell], 0);
}

#[test]
fn the_default_256_colour_palette_s_cells_are_its_nearest_by_the_default_loadings() {
    let default: Vec<Rgb> = (0..=u8::MAX)
        .map(|pixel| {
            let [r, g, b] = default_256(pixel);
            Rgb::new(r, g, b)
        })
        .collect();
    let built = table(&default).expect("room");
    for (cell, &number) in built.iter().enumerate() {
        let [r, g, b] =
            [0, 5, 10].map(|shift| eight_of_five(len32((cell >> shift) & 31)).to_le_bytes()[0]);
        let nearest = search(
            &default,
            Rgb::new(r, g, b),
            Loadings::DEFAULT,
            Look::Nearest,
        );
        assert_eq!(usize::from(number), nearest, "{cell}");
    }
}

#[test]
fn a_kept_table_is_built_again_only_once_its_colours_differ() {
    let mut kept = Kept::default();
    let first = colours(16, 3);
    let (table, built) = kept.table(&first).expect("room");
    assert!(built);
    assert!(
        *table == super::table(&first).expect("room"),
        "the first colours' table"
    );
    let (again, built) = kept.table(&first).expect("room");
    assert!(!built, "the same colours");
    assert!(Rc::ptr_eq(&table, &again));
    let second = colours(16, 4);
    let (other, built) = kept.table(&second).expect("room");
    assert!(built, "other colours");
    assert!(
        *other == super::table(&second).expect("room"),
        "the second colours' table"
    );
}
