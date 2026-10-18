//! Unit tests for a menu taller than its plate: the window of whole rows, the
//! keyboard and wheel that move it, the typed seek, and the position mark.

use alloc::format;
use alloc::vec::Vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::damage::sink;
use crate::menu::{Menu, MenuItem, SEEK_PAUSE_NS};
use crate::testkit::{has_pixel, premul};

const W: u32 = 200;
const ROW_H: u32 = 24;
const BORDER: u32 = 1;

/// How many whole rows the test plate holds.
const SHOWN: u32 = 5;

/// A plate holding [`SHOWN`] whole rows and part of another.
fn plate() -> Rect {
    Rect::new(0, 0, W, BORDER * 2 + ROW_H * SHOWN + ROW_H / 2)
}

/// A menu of `count` rows labelled `Row 00`, `Row 01`, ….
fn rows(count: usize) -> Menu {
    Menu::new(
        (0..count)
            .map(|index| MenuItem::new(format!("Row {index:02}")))
            .collect(),
    )
}

/// The rows the plate shows, in order.
fn shown(menu: &Menu, theme: &Theme) -> Vec<usize> {
    (0..menu.len())
        .filter(|&index| menu.row_rect(index, plate(), Scale::ONE, theme).is_some())
        .collect()
}

/// The surface y at the middle of the `slot`th row the plate shows.
fn slot_y(slot: u32) -> i32 {
    i32::try_from(BORDER + slot * ROW_H + ROW_H / 2).expect("fits")
}

fn press(menu: &mut Menu, key: NamedKey, theme: &Theme) -> Region {
    let mut damage = sink();
    menu.on_key(Key::Named(key), plate(), Scale::ONE, theme, &mut damage);
    damage
}

/// The rows area of [`plate`]: what moves when the window does.
fn rows_area() -> Rect {
    Rect::new(1, 1, W - 2, ROW_H * SHOWN + ROW_H / 2)
}

#[test]
fn a_plate_too_short_for_every_row_shows_whole_rows_and_hits_only_those() {
    let theme = Theme::dark();
    let menu = rows(20);
    assert_eq!(
        shown(&menu, &theme),
        [0, 1, 2, 3, 4],
        "the half row is not drawn"
    );
    let below = Point::new(10, slot_y(SHOWN));
    assert_eq!(menu.row_at(plate(), Scale::ONE, &theme, below), None);
    let last = Point::new(10, slot_y(SHOWN - 1));
    assert_eq!(menu.row_at(plate(), Scale::ONE, &theme, last), Some(4));
}

#[test]
fn a_plate_opens_with_its_highlighted_row_in_the_middle() {
    let theme = Theme::dark();
    let mut menu = rows(20);
    menu.adopt_current(Some(10));
    assert_eq!(shown(&menu, &theme), [8, 9, 10, 11, 12]);
    menu.adopt_current(Some(19));
    assert_eq!(
        shown(&menu, &theme),
        [15, 16, 17, 18, 19],
        "kept within the rows"
    );
}

#[test]
fn moving_the_highlight_within_the_window_leaves_the_rows_where_they_are() {
    let theme = Theme::dark();
    let mut menu = rows(20);
    menu.adopt_current(Some(10));
    let damage = press(&mut menu, NamedKey::Down, &theme);
    assert_eq!(menu.current(), Some(11));
    assert_eq!(shown(&menu, &theme), [8, 9, 10, 11, 12]);
    assert!(
        !damage.contains(Point::new(10, slot_y(0))),
        "only the two rows the highlight moved between are redrawn"
    );
}

#[test]
fn the_keyboard_brings_its_row_into_the_window() {
    let theme = Theme::dark();
    let mut menu = rows(20);
    menu.adopt_current(Some(0));
    for _ in 0..4 {
        press(&mut menu, NamedKey::Down, &theme);
    }
    assert_eq!(shown(&menu, &theme), [0, 1, 2, 3, 4]);
    let damage = press(&mut menu, NamedKey::Down, &theme);
    assert_eq!(menu.current(), Some(5));
    assert_eq!(shown(&menu, &theme), [1, 2, 3, 4, 5], "the least movement");
    let area = rows_area();
    assert!(damage.contains(Point::new(area.left(), area.top())));
    press(&mut menu, NamedKey::Up, &theme);
    press(&mut menu, NamedKey::Up, &theme);
    press(&mut menu, NamedKey::Up, &theme);
    press(&mut menu, NamedKey::Up, &theme);
    press(&mut menu, NamedKey::Up, &theme);
    assert_eq!(menu.current(), Some(0));
    assert_eq!(shown(&menu, &theme), [0, 1, 2, 3, 4]);
    press(&mut menu, NamedKey::Up, &theme);
    assert_eq!(menu.current(), Some(19), "Up from the first row wraps");
    assert_eq!(shown(&menu, &theme), [15, 16, 17, 18, 19]);
}

#[test]
fn the_paging_keys_move_a_windows_worth_and_stop_at_the_ends() {
    let theme = Theme::dark();
    let mut menu = rows(20);
    menu.adopt_current(Some(0));
    press(&mut menu, NamedKey::PageDown, &theme);
    assert_eq!(menu.current(), Some(4), "a window less one row");
    press(&mut menu, NamedKey::PageDown, &theme);
    assert_eq!(menu.current(), Some(8));
    assert_eq!(shown(&menu, &theme), [4, 5, 6, 7, 8]);
    press(&mut menu, NamedKey::PageUp, &theme);
    assert_eq!(menu.current(), Some(4));
    press(&mut menu, NamedKey::End, &theme);
    press(&mut menu, NamedKey::PageDown, &theme);
    assert_eq!(menu.current(), Some(19), "no wrap past the end");
    press(&mut menu, NamedKey::Home, &theme);
    press(&mut menu, NamedKey::PageUp, &theme);
    assert_eq!(menu.current(), Some(0));
    assert_eq!(shown(&menu, &theme), [0, 1, 2, 3, 4]);
}

#[test]
fn the_wheel_scrolls_the_window_and_the_hover_follows_the_rows() {
    let theme = Theme::dark();
    let mut menu = rows(20);
    let mut damage = sink();
    let over = InputEvent::PointerMoved {
        to: Point::new(10, slot_y(1)),
    };
    menu.on_pointer(&over, plate(), Scale::ONE, &theme, &mut damage);
    assert_eq!(menu.current(), Some(1));
    let turn = InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    };
    let mut damage = sink();
    menu.on_pointer(&turn, plate(), Scale::ONE, &theme, &mut damage);
    assert_eq!(
        shown(&menu, &theme),
        [2, 3, 4, 5, 6],
        "a detent is the nearest whole number of rows to the one wheel distance"
    );
    assert_eq!(menu.current(), Some(3), "the row now under the pointer");
    let area = rows_area();
    assert!(damage.contains(Point::new(area.left(), area.top())));
    let back = InputEvent::PointerScrolled {
        dx: 0,
        dy: -SCROLL_UNITS_PER_DETENT * 4,
    };
    menu.on_pointer(&back, plate(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        shown(&menu, &theme),
        [0, 1, 2, 3, 4],
        "kept within the rows"
    );
}

#[test]
fn the_wheel_moves_nothing_where_every_row_fits_or_the_pointer_is_elsewhere() {
    let theme = Theme::dark();
    let mut short = rows(3);
    let turn = InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    };
    let mut damage = sink();
    short.on_pointer(&turn, plate(), Scale::ONE, &theme, &mut damage);
    assert!(damage.is_empty());
    let mut long = rows(20);
    let away = InputEvent::PointerMoved {
        to: Point::new(10, 500),
    };
    long.on_pointer(&away, plate(), Scale::ONE, &theme, &mut sink());
    let mut damage = sink();
    long.on_pointer(&turn, plate(), Scale::ONE, &theme, &mut damage);
    assert!(damage.is_empty());
    assert_eq!(shown(&long, &theme), [0, 1, 2, 3, 4]);
}

fn cities() -> Menu {
    Menu::new(
        ["Lagos", "Lisbon", "London", "Madrid", "Lima"]
            .into_iter()
            .map(MenuItem::new)
            .collect(),
    )
}

#[test]
fn typing_seeks_a_row_by_its_label() {
    let theme = Theme::dark();
    let mut menu = cities();
    menu.adopt_current(Some(0));
    let seek = |menu: &mut Menu, typed: char, at_ns: u64| {
        menu.seek(typed, at_ns, plate(), Scale::ONE, &theme, &mut sink())
    };
    assert!(seek(&mut menu, 'l', 0));
    assert_eq!(
        menu.current(),
        Some(1),
        "a lone letter steps on from the row held"
    );
    assert!(seek(&mut menu, 'O', 100));
    assert_eq!(menu.current(), Some(2), "letters compare without case");
    assert!(seek(&mut menu, 'n', 200));
    assert_eq!(
        menu.current(),
        Some(2),
        "a longer prefix keeps a row that still matches"
    );
    let later = 200 + SEEK_PAUSE_NS + 1;
    assert!(seek(&mut menu, 'm', later));
    assert_eq!(menu.current(), Some(3), "a pause starts a new prefix");
    assert!(!seek(&mut menu, 'q', later + 1));
    assert_eq!(menu.current(), Some(3), "no match leaves the highlight");
}

#[test]
fn the_same_letter_again_steps_through_the_rows_it_begins() {
    let theme = Theme::dark();
    let mut menu = cities();
    menu.adopt_current(Some(0));
    let mut at = 0;
    let mut visited = Vec::new();
    for _ in 0..4 {
        at += 10;
        menu.seek('l', at, plate(), Scale::ONE, &theme, &mut sink());
        visited.push(menu.current());
    }
    assert_eq!(visited, [Some(1), Some(2), Some(4), Some(0)]);
}

#[test]
fn a_sought_row_outside_the_window_is_brought_into_it() {
    let theme = Theme::dark();
    let mut menu = rows(20);
    menu.adopt_current(Some(0));
    let mut damage = sink();
    assert!(menu.seek('r', 0, plate(), Scale::ONE, &theme, &mut damage));
    menu.seek('o', 1, plate(), Scale::ONE, &theme, &mut damage);
    menu.seek('w', 2, plate(), Scale::ONE, &theme, &mut damage);
    menu.seek(' ', 3, plate(), Scale::ONE, &theme, &mut damage);
    menu.seek('1', 4, plate(), Scale::ONE, &theme, &mut damage);
    menu.seek('7', 5, plate(), Scale::ONE, &theme, &mut damage);
    assert_eq!(menu.current(), Some(17));
    assert!(shown(&menu, &theme).contains(&17));
}

#[test]
fn the_position_mark_is_drawn_only_while_rows_are_hidden() {
    let theme = Theme::dark();
    let mark = premul(theme.palette().scroll_thumb);
    let painted = |menu: &Menu| {
        let mut surface = Surface::new(W, plate().height).expect("surface");
        menu.render(&mut surface, plate(), Scale::ONE, &theme);
        surface
    };
    assert!(has_pixel(&painted(&rows(20)), mark));
    assert!(!has_pixel(&painted(&rows(3)), mark));
}
