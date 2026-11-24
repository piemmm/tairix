use super::*;

/// An `x90y45` file of one size, 12 points at 90 by 45, holding `A` as a
/// three by two pixel glyph from (1, -1).
fn file() -> Vec<u8> {
    crate::fixture::old_bitmaps(12, (90, 45), b'A', (1, -1), 3, &[1, 2, 3, 4, 5, 6])
}

#[test]
fn a_size_s_character_is_its_nibbles_from_its_bottom_row_up() {
    let File::Sizes(sizes) = File::read(file()).expect("read") else {
        panic!("a file of sizes");
    };
    assert_eq!(sizes.sizes.len(), 1);
    let size = sizes.sizes[0];
    assert_eq!((size.points, size.resolution), (12, (90, 45)));
    let glyph = sizes
        .glyph(&size, u32::from(b'A'))
        .expect("decoded")
        .expect("defined");
    assert_eq!((glyph.left, glyph.top, glyph.width), (1, 0, 3));
    assert_eq!(glyph.levels, [4, 5, 6, 1, 2, 3], "the top row first");
    assert_eq!(sizes.glyph(&size, u32::from(b'B')), Ok(None));
    assert_eq!(sizes.glyph(&size, 300), Ok(None));
}

#[test]
fn a_short_file_names_its_master_bitmaps() {
    let File::Master(name) = File::read(b"f240x120\r".to_vec()).expect("read") else {
        panic!("a master's name");
    };
    assert_eq!(name, b"f240x120");
}

#[test]
fn what_does_not_hold_together_is_refused() {
    let good = file();
    let mut wrong_depth = good.clone();
    wrong_depth[BPP] = 1;
    assert!(File::read(wrong_depth).is_err());
    let mut past_the_end = good.clone();
    past_the_end[DATA_SIZE] = 0xFF;
    past_the_end[DATA_SIZE + 1] = 0xFF;
    assert!(File::read(past_the_end).is_err());
    let mut unterminated = good.clone();
    unterminated.truncate(ENTRY);
    unterminated.resize(300, 7);
    assert!(File::read(unterminated).is_err());
    let mut wild_character = good;
    let at = 32 + BLOCK_TABLE + 2 * usize::from(b'A');
    wild_character[at..at + 2].copy_from_slice(&0xFFF0u16.to_le_bytes());
    let File::Sizes(sizes) = File::read(wild_character).expect("the index holds") else {
        panic!("a file of sizes");
    };
    assert_eq!(
        sizes.glyph(&sizes.sizes[0], u32::from(b'A')),
        Ok(None),
        "a character whose data does not hold together draws nothing"
    );
}
