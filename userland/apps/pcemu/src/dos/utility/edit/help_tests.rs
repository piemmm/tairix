//! The help window: scrolled within its topic and drawn over the text.

use super::*;
use alloc::vec::Vec;

#[test]
fn the_window_scrolls_within_its_topic() {
    let mut help = Help::of(Topic::Keys);
    help.key(HelpKey::Up, 25);
    assert_eq!(help.top, 0);
    help.key(HelpKey::PageDown, 25);
    assert_eq!(
        help.top, 11,
        "no further than shows the last line at the foot"
    );
    help.key(HelpKey::End, 25);
    assert_eq!(help.top, 11);
    help.key(HelpKey::PageUp, 25);
    assert_eq!(help.top, 0);
    let mut short = Help::of(Topic::Print);
    short.key(HelpKey::Down, 25);
    assert_eq!(
        short.top, 0,
        "a topic the window holds whole does not scroll"
    );
    assert_eq!(Topic::of(Topic::Display.number()), Topic::Display);
    assert_eq!(Topic::of(200), Topic::Started);
}

#[test]
fn every_topic_fits_the_window_across() {
    for n in 0..9 {
        let topic = Topic::of(n);
        assert!(
            topic.lines().iter().all(|line| line.len() <= 76),
            "{topic:?}"
        );
    }
}

#[test]
fn the_window_shows_its_title_and_lines() {
    let mut cells = Cells::new(25, 80);
    Help::of(Topic::Started).draw(&mut cells, &Palette::COLOUR);
    let row = |n: usize| -> Vec<u8> {
        cells.grid[n * 80..n * 80 + 80]
            .iter()
            .map(|cell| cell.to_le_bytes()[0])
            .collect()
    };
    assert!(row(1)
        .windows(23)
        .any(|window| window == b" HELP: Getting Started "));
    assert_eq!(&row(2)[2..35], b"Welcome to the TAIRiX DOS Editor.");
}
