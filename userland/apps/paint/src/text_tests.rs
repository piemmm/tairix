use tairix_font::{BitmapFont, FamilyKey};

use super::{TextEntry, MOST_CHARS};

fn face() -> BitmapFont {
    BitmapFont::new(FamilyKey::MONO, 20)
}

#[test]
fn typing_moves_the_caret_and_keys_take_back_and_walk() {
    let mut entry = TextEntry::new((10, 20));
    assert!(entry.is_empty());
    for ch in "héllo".chars() {
        assert!(entry.insert(ch));
    }
    assert_eq!(entry.text(), "héllo");
    for _ in 0..3 {
        assert!(entry.step(false));
    }
    assert!(entry.backspace(), "the é before the caret");
    assert_eq!(entry.text(), "hllo");
    assert!(entry.delete());
    assert_eq!(entry.text(), "hlo");
    assert!(entry.insert('\n'));
    entry.to_line_edge(true);
    assert!(entry.insert('!'));
    assert_eq!(entry.text(), "h\nlo!");
    entry.to_line_edge(false);
    assert!(entry.insert('>'));
    assert_eq!(entry.text(), "h\n>lo!");
    let mut empty = TextEntry::new((0, 0));
    assert!(!empty.backspace() && !empty.delete() && !empty.step(false) && !empty.step(true));
}

#[test]
fn text_holds_no_more_than_its_bound() {
    let mut entry = TextEntry::new((0, 0));
    for _ in 0..MOST_CHARS {
        assert!(entry.insert('x'));
    }
    assert!(!entry.insert('x'), "past the bound");
    assert_eq!(entry.text().len(), MOST_CHARS);
}

#[test]
fn set_text_covers_its_lines_and_places_its_caret() {
    let face = face();
    let mut entry = TextEntry::new((10, 20));
    entry.set(face, true).expect("room");
    assert!(entry.bounds().is_empty(), "nothing typed covers nothing");
    for ch in "ab\nc".chars() {
        entry.insert(ch);
    }
    entry.set(face, true).expect("room");
    let bounds = entry.bounds();
    let line = i64::from(face.line_height());
    assert_eq!((bounds.x0, bounds.y0), (10, 20));
    assert_eq!(bounds.y1 - bounds.y0, line * 2, "two lines");
    assert_eq!(
        bounds.x1 - bounds.x0,
        i64::from(face.text_width("ab")),
        "the wider line"
    );
    let (x, top, bottom) = entry.caret();
    assert_eq!(
        (top, bottom),
        (20 + line, 20 + line * 2),
        "on the second line"
    );
    assert_eq!(
        x,
        10 + i64::from(face.text_width("c")),
        "after its one letter"
    );
    let mut covered = 0u64;
    let width = usize::try_from(bounds.x1 - bounds.x0).expect("a width");
    let mut row = alloc::vec![0u8; width + 4];
    for y in bounds.y0..bounds.y1 {
        entry.row(y, bounds.x0 - 2, &mut row);
        assert_eq!((row[0], row[1]), (0, 0), "nothing before it");
        covered += row.iter().map(|&a| u64::from(a)).sum::<u64>();
    }
    assert!(covered > 0, "its glyphs cover something");
    let mut hard = TextEntry::new((0, 0));
    hard.insert('a');
    hard.set(face, false).expect("room");
    let hard_width = usize::try_from(hard.bounds().x1).expect("a width");
    let mut row = alloc::vec![0u8; hard_width];
    for y in 0..hard.bounds().y1 {
        hard.row(y, 0, &mut row);
        assert!(
            row.iter().all(|&a| a == 0 || a == 255),
            "unsmoothed text is whole or nothing"
        );
    }
}
