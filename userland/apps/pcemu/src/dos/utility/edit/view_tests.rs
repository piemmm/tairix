//! The screen: the menu bar, the window with its title, the text with its
//! selection, the bars, and the status line.

use super::super::editor::Editor;
use super::super::fixture::{empty, store};
use super::*;

fn row(cells: &Cells, row: u8) -> Vec<u8> {
    let columns = usize::from(cells.columns);
    let start = usize::from(row) * columns;
    cells.grid[start..start + columns]
        .iter()
        .map(|&cell| cell.to_le_bytes()[0])
        .collect()
}

fn attribute(cells: &Cells, row: u8, column: u8) -> u8 {
    cells.grid[usize::from(row) * usize::from(cells.columns) + usize::from(column)].to_le_bytes()[1]
}

#[test]
fn the_editor_draws_its_window_text_bars_and_status() {
    let mut store = store(256);
    let mut text = empty(&mut store);
    text.insert(0, b"hello\r\n\tworld").unwrap();
    let mut editor = Editor {
        rows: 21,
        columns: 78,
        lines: 2,
        ..Editor::default()
    };
    editor.cursor.column = 3;
    editor.anchor = Some(super::super::editor::Spot::default());
    let mut cells = Cells::new(25, 80);
    menu_bar(&mut cells, &Palette::COLOUR, Bar::Keys);
    let look = Look {
        palette: &Palette::COLOUR,
        title: b"Untitled",
        scroll_bars: true,
    };
    draw(&mut cells, look, &editor, &mut text, b"<F1=Help>");
    assert_eq!(&row(&cells, 0)[..30], b"  File  Edit  Search  Options ");
    assert!(row(&cells, 0).ends_with(b"Help  "), "{:?}", row(&cells, 0));
    assert_eq!(
        (attribute(&cells, 0, 2), attribute(&cells, 0, 3)),
        (Palette::COLOUR.hot, Palette::COLOUR.menu),
        "the key letter marked"
    );
    assert!(row(&cells, 1)
        .windows(10)
        .any(|window| window == b" Untitled "));
    assert_eq!(&row(&cells, 2)[1..6], b"hello");
    assert_eq!(
        &row(&cells, 3)[1..14],
        b"        world",
        "a tab to the next stop"
    );
    assert_eq!(
        (
            attribute(&cells, 2, 1),
            attribute(&cells, 2, 3),
            attribute(&cells, 2, 4)
        ),
        (
            Palette::COLOUR.selected,
            Palette::COLOUR.selected,
            Palette::COLOUR.text
        )
    );
    assert_eq!(cells.grid[2 * 80 + 79].to_le_bytes()[0], glyph::UP_ARROW);
    assert_eq!(cells.grid[23 * 80 + 1].to_le_bytes()[0], glyph::LEFT_ARROW);
    assert!(
        row(&cells, 24).starts_with(b" <F1=Help>") && row(&cells, 24).ends_with(b"00001:004 "),
        "{:?}",
        row(&cells, 24)
    );
}

#[test]
fn a_frame_centres_its_title_and_a_shadow_darkens_beside_it() {
    let mut cells = Cells::new(10, 20);
    cells.frame(1, 2, 5, 12, 0x70, b"Find");
    assert_eq!(
        &row(&cells, 1)[2..13],
        b"\xDA\xC4 Find \xC4\xC4\xBF",
        "eleven cells, the title's six as near the middle as they fit"
    );
    cells.shadow(1, 2, 5, 12, 0x08);
    assert_eq!(
        (
            attribute(&cells, 3, 13),
            attribute(&cells, 6, 5),
            attribute(&cells, 6, 3)
        ),
        (0x08, 0x08, 0x07),
        "right and below, offset"
    );
}

#[test]
fn the_bars_go_when_asked_and_a_long_line_stops_at_the_window() {
    let mut store = store(4096);
    let mut text = empty(&mut store);
    text.insert(0, &[b'y'; 3000]).unwrap();
    let editor = Editor {
        rows: 21,
        columns: 78,
        left: 10,
        ..Editor::default()
    };
    let mut cells = Cells::new(25, 80);
    let look = Look {
        palette: &Palette::COLOUR.with_text(0x1E),
        title: b"LONG.TXT",
        scroll_bars: false,
    };
    draw(&mut cells, look, &editor, &mut text, b"");
    assert_eq!(&row(&cells, 2)[1..79], &[b'y'; 78][..]);
    assert_eq!(attribute(&cells, 2, 1), 0x1E, "the text in the colours set");
    assert_eq!(
        (row(&cells, 2)[79], row(&cells, 23)[1]),
        (glyph::DOWN, glyph::ACROSS),
        "the frame plain"
    );
    assert_eq!(
        Palette::COLOUR.with_text(0x1E).selected,
        0x61,
        "the selection reversed"
    );
}
