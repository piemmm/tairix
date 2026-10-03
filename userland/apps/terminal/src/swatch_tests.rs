//! The scheme editor's wells: the order a scheme is laid out in and read
//! back from. The grid's own behaviour is the shared control's, tested with
//! it.

use tairix_theme::Theme;

use crate::scheme::{ColorScheme, Scheme};
use crate::swatch::{adopt, apply, colour, grid_for, COLUMNS, WELL_COUNT};
use tairix_colour::Rgb;

fn scheme() -> ColorScheme {
    Scheme::Contrast.palette().expect("contrast has a palette")
}

#[test]
fn a_grid_read_back_onto_a_fresh_scheme_reproduces_it() {
    let original = scheme();
    let grid = grid_for(&original);
    let mut round_tripped = ColorScheme::from_theme(&Theme::dark());
    apply(&grid, &mut round_tripped);
    assert_eq!(round_tripped, original);
    assert_eq!((grid.len(), grid.columns()), (WELL_COUNT, COLUMNS));
}

#[test]
fn the_twenty_wells_are_laid_out_in_the_documented_order() {
    let original = scheme();
    let grid = grid_for(&original);
    assert_eq!(colour(&grid, 0), Some(original.background));
    assert_eq!(colour(&grid, 3), Some(original.cursor_text));
    assert_eq!(colour(&grid, 4), Some(original.ansi[0]));
    assert_eq!(colour(&grid, WELL_COUNT - 1), Some(original.ansi[15]));
    assert_eq!(colour(&grid, WELL_COUNT), None);
}

#[test]
fn an_edited_well_is_what_the_scheme_reads_back() {
    let mut grid = grid_for(&scheme());
    let orange = Rgb::new(0xff, 0x80, 0x00);
    grid.set_colour(0, tairix_raster::Color::from(orange));
    let mut applied = scheme();
    apply(&grid, &mut applied);
    assert_eq!(applied.background, orange);
}

#[test]
fn adopting_a_scheme_keeps_the_well_being_edited() {
    let mut grid = grid_for(&scheme());
    grid.adopt_selected(Some(7));
    let mut other = scheme();
    other.ansi[3] = Rgb::new(0x65, 0x43, 0x21);
    adopt(&mut grid, &other);
    assert_eq!(grid.selected(), Some(7));
    let mut read = scheme();
    apply(&grid, &mut read);
    assert_eq!(read, other);
}
