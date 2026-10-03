//! The colour-well grid: its layout, both marks, and what each interaction
//! reports.

use alloc::vec;
use alloc::vec::Vec;

use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::damage;
use crate::paint::authority_rgba;
use crate::state::{AuthorityState, ControlState};
use crate::swatch_grid::{SwatchAction, SwatchGrid, SwatchMark};
use crate::testkit::{has_pixel, premul};

const W: u32 = 400;
const H: u32 = 200;
const COUNT: usize = 20;

fn bounds() -> Rect {
    Rect::new(0, 0, W, H)
}

fn colours(count: usize) -> Vec<Color> {
    (0..count)
        .map(|i| {
            let i = u8::try_from(i * 11 % 256).expect("a byte");
            Color::rgb(i, 255 - i, i / 2)
        })
        .collect()
}

fn grid() -> SwatchGrid {
    SwatchGrid::new(5, colours(COUNT))
}

fn moved(point: Point) -> InputEvent {
    InputEvent::PointerMoved { to: point }
}

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};
const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

/// The centre of well `index` within [`bounds`].
fn centre(grid: &SwatchGrid, index: usize) -> Point {
    let cell = grid.cell_rect(bounds(), index).expect("a well");
    Point::new(
        cell.left() + i32::try_from(cell.width / 2).expect("fits"),
        cell.top() + i32::try_from(cell.height / 2).expect("fits"),
    )
}

/// Press and release over well `index`, moving `mark`.
fn click(grid: &mut SwatchGrid, index: usize, mark: SwatchMark) -> Option<SwatchAction> {
    let point = centre(grid, index);
    grid.on_pointer(&moved(point), bounds(), mark, &mut damage::sink());
    grid.on_pointer(&PRESS, bounds(), mark, &mut damage::sink());
    grid.on_pointer(&RELEASE, bounds(), mark, &mut damage::sink())
}

#[test]
fn a_new_grid_selects_its_first_well_and_shows_no_secondary_mark() {
    let grid = grid();
    assert_eq!((grid.selected(), grid.secondary()), (Some(0), None));
    assert_eq!((grid.len(), grid.columns(), grid.rows()), (COUNT, 5, 4));
}

#[test]
fn every_pixel_of_the_bounds_belongs_to_the_well_the_layout_draws_there() {
    // Hit-testing reads the layout's inverse rather than testing every well,
    // so the two are checked against each other everywhere.
    for (columns, count, width, height) in [(5, 20, 400, 200), (16, 256, 191, 97), (3, 7, 10, 9)] {
        let grid = SwatchGrid::new(columns, colours(count));
        let bounds = Rect::new(3, 2, width, height);
        for y in 0..i32::try_from(height).expect("fits") {
            for x in 0..i32::try_from(width).expect("fits") {
                let point = Point::new(3 + x, 2 + y);
                let found = (0..count).find(|&index| {
                    grid.cell_rect(bounds, index)
                        .is_some_and(|cell| cell.contains(point))
                });
                assert_eq!(grid.well_at(bounds, point), found, "at {point:?}");
            }
        }
    }
}

#[test]
fn a_press_and_release_over_one_well_moves_the_mark_it_names() {
    let mut grid = grid();
    assert_eq!(
        click(&mut grid, 4, SwatchMark::Primary),
        Some(SwatchAction::Selected {
            mark: SwatchMark::Primary,
            index: 4
        })
    );
    assert_eq!(
        click(&mut grid, 9, SwatchMark::Secondary),
        Some(SwatchAction::Selected {
            mark: SwatchMark::Secondary,
            index: 9
        })
    );
    assert_eq!((grid.selected(), grid.secondary()), (Some(4), Some(9)));
}

#[test]
fn a_release_over_another_well_or_none_moves_nothing() {
    let mut grid = grid();
    let (first, other) = (centre(&grid, 0), centre(&grid, 10));
    grid.on_pointer(
        &moved(first),
        bounds(),
        SwatchMark::Primary,
        &mut damage::sink(),
    );
    grid.on_pointer(&PRESS, bounds(), SwatchMark::Primary, &mut damage::sink());
    grid.on_pointer(
        &moved(other),
        bounds(),
        SwatchMark::Primary,
        &mut damage::sink(),
    );
    assert_eq!(
        grid.on_pointer(&RELEASE, bounds(), SwatchMark::Primary, &mut damage::sink()),
        None
    );
    grid.on_pointer(
        &moved(Point::new(-5, -5)),
        bounds(),
        SwatchMark::Primary,
        &mut damage::sink(),
    );
    grid.on_pointer(&PRESS, bounds(), SwatchMark::Primary, &mut damage::sink());
    assert_eq!(
        grid.on_pointer(&RELEASE, bounds(), SwatchMark::Primary, &mut damage::sink()),
        None
    );
    assert_eq!(grid.selected(), Some(0));
}

#[test]
fn a_press_completes_on_the_mark_it_armed_even_if_asked_for_another() {
    let mut grid = grid();
    let point = centre(&grid, 6);
    grid.on_pointer(
        &moved(point),
        bounds(),
        SwatchMark::Secondary,
        &mut damage::sink(),
    );
    grid.on_pointer(&PRESS, bounds(), SwatchMark::Secondary, &mut damage::sink());
    assert_eq!(
        grid.on_pointer(&RELEASE, bounds(), SwatchMark::Primary, &mut damage::sink()),
        Some(SwatchAction::Selected {
            mark: SwatchMark::Secondary,
            index: 6
        })
    );
}

#[test]
fn moving_a_mark_reports_the_two_wells_it_moves_between_and_nothing_else() {
    let mut grid = grid();
    let point = centre(&grid, 7);
    let mut arming = damage::sink();
    grid.on_pointer(&moved(point), bounds(), SwatchMark::Primary, &mut arming);
    grid.on_pointer(&PRESS, bounds(), SwatchMark::Primary, &mut arming);
    assert!(arming.is_empty(), "arming draws nothing");
    let mut moving = damage::sink();
    grid.on_pointer(&RELEASE, bounds(), SwatchMark::Primary, &mut moving);
    let reported = moving.bounds();
    assert!(reported.contains(centre(&grid, 0)));
    assert!(reported.contains(centre(&grid, 7)));
    assert!(!moving
        .rects()
        .iter()
        .any(|rect| rect.contains(centre(&grid, 12))));
}

#[test]
fn arrow_keys_move_the_primary_mark_and_wrap() {
    let mut grid = grid();
    grid.set_focused(true);
    let key = |grid: &mut SwatchGrid, named| match grid.on_key(
        Key::Named(named),
        bounds(),
        &mut damage::sink(),
    ) {
        Some(SwatchAction::Selected { index, .. }) => index,
        None => usize::MAX,
    };
    assert_eq!(key(&mut grid, NamedKey::Right), 1);
    assert_eq!(key(&mut grid, NamedKey::Left), 0);
    assert_eq!(key(&mut grid, NamedKey::Left), COUNT - 1);
    assert_eq!(key(&mut grid, NamedKey::Right), 0);
    assert_eq!(key(&mut grid, NamedKey::Down), 5);
    assert_eq!(key(&mut grid, NamedKey::Up), 0);
    assert_eq!(key(&mut grid, NamedKey::Up), 15);
    assert_eq!(
        grid.on_key(Key::Char('x'), bounds(), &mut damage::sink()),
        None
    );
}

#[test]
fn adopting_colours_keeps_the_marks_and_a_press_that_still_name_wells() {
    let mut grid = grid();
    grid.adopt_selected(Some(7));
    grid.adopt_secondary(Some(19));
    let point = centre(&grid, 4);
    grid.on_pointer(
        &moved(point),
        bounds(),
        SwatchMark::Primary,
        &mut damage::sink(),
    );
    grid.on_pointer(&PRESS, bounds(), SwatchMark::Primary, &mut damage::sink());
    grid.adopt_colours(5, colours(COUNT));
    assert_eq!((grid.selected(), grid.secondary()), (Some(7), Some(19)));
    assert_eq!(
        grid.on_pointer(&RELEASE, bounds(), SwatchMark::Primary, &mut damage::sink()),
        Some(SwatchAction::Selected {
            mark: SwatchMark::Primary,
            index: 4
        })
    );
    grid.adopt_colours(4, colours(8));
    assert_eq!((grid.selected(), grid.secondary()), (Some(4), None));
}

#[test]
fn refitting_the_columns_keeps_the_colours_and_marks_and_relays_the_wells() {
    let mut grid = grid();
    grid.adopt_selected(Some(7));
    grid.adopt_secondary(Some(19));
    grid.set_columns(10);
    assert_eq!((grid.columns(), grid.rows()), (10, 2));
    assert_eq!((grid.selected(), grid.secondary()), (Some(7), Some(19)));
    assert_eq!(grid.colour(19), colours(COUNT).get(19).copied());
    assert_eq!(
        grid.cell_rect(bounds(), 10),
        Some(Rect::new(0, 100, 40, 100)),
        "the eleventh well starts the second row"
    );
    grid.set_columns(0);
    assert_eq!(grid.columns(), 1, "a grid has at least one column");
}

#[test]
fn an_out_of_range_mark_marks_nothing() {
    let mut grid = grid();
    grid.adopt_selected(Some(COUNT));
    grid.adopt_secondary(Some(COUNT));
    assert_eq!((grid.selected(), grid.secondary()), (None, None));
}

#[test]
fn each_mark_repaints_a_visible_shape() {
    let theme = Theme::dark();
    let render = |grid: &SwatchGrid| {
        let mut surface = Surface::new(W, H).expect("surface");
        grid.render(&mut surface, bounds(), Scale::ONE, &theme);
        surface
    };
    let mut grid = grid();
    let plain = render(&grid);
    grid.adopt_selected(Some(3));
    let moved = render(&grid);
    assert_ne!(plain.pixels(), moved.pixels());
    grid.adopt_secondary(Some(8));
    let both = render(&grid);
    assert_ne!(moved.pixels(), both.pixels());
}

#[test]
fn a_transparent_well_shows_a_checker_rather_than_a_flat_colour() {
    let theme = Theme::dark();
    let mut grid = SwatchGrid::new(2, alloc::vec![Color::rgba(0, 0, 0, 0), Color::rgb(9, 9, 9)]);
    grid.adopt_selected(Some(1));
    let mut surface = Surface::new(100, 50).expect("surface");
    let bounds = Rect::new(0, 0, 100, 50);
    grid.render(&mut surface, bounds, Scale::ONE, &theme);
    let cell = grid.cell_rect(bounds, 0).expect("a well");
    let quarter = |fx: u32, fy: u32| {
        surface
            .get(cell.width * fx / 4, cell.height * fy / 4)
            .expect("inside")
    };
    assert_ne!(quarter(1, 1), quarter(3, 1), "the checker's two tones");
}

#[test]
fn a_palette_sized_grid_fits_square_wells_to_a_width() {
    let grid = SwatchGrid::new(16, colours(256));
    assert_eq!(grid.height_for_width(160), 160);
    assert_eq!(SwatchGrid::new(16, colours(20)).height_for_width(160), 20);
    let theme = Theme::dark();
    let one = grid.preferred_height(Scale::ONE, &theme);
    let two = grid.preferred_height(Scale::from_percent(200).expect("a scale"), &theme);
    assert!(one > 0 && two >= one);
}

#[test]
fn renders_without_faulting_at_a_tiny_size_or_with_no_wells() {
    let theme = Theme::dark();
    let mut tiny = Surface::new(6, 5).expect("surface");
    SwatchGrid::new(16, colours(256)).render(&mut tiny, Rect::new(0, 0, 6, 5), Scale::ONE, &theme);
    let mut empty = SwatchGrid::new(4, Vec::new());
    empty.set_focused(true);
    empty.render(&mut tiny, Rect::new(0, 0, 6, 5), Scale::ONE, &theme);
    assert_eq!(
        empty.on_key(Key::Named(NamedKey::Right), bounds(), &mut damage::sink()),
        None
    );
}

/// The primary well carries its mark at its centre, drawn in place; a well
/// without one shows its colour there.
#[test]
fn the_primary_mark_sits_at_the_wells_centre() {
    let theme = Theme::dark();
    let grid = grid();
    let mut surface = Surface::new(W, H).expect("a surface");
    grid.render(&mut surface, bounds(), Scale::ONE, &theme);
    let (cell_w, cell_h) = (W / 5, H / 4);
    let centre = |index: u32| {
        (
            (index % 5) * cell_w + cell_w / 2,
            (index / 5) * cell_h + cell_h / 2,
        )
    };
    let (x, y) = centre(0);
    let marked = surface.get(x, y).expect("on the surface");
    assert_ne!(marked, colours(COUNT)[0].premultiply(), "the mark is drawn");
    let (x, y) = centre(7);
    assert_eq!(surface.get(x, y), Some(colours(COUNT)[7].premultiply()));
}

/// A mark moved by a key reports both wells it moves between, as a pointer
/// does.
#[test]
fn a_keyed_move_reports_both_wells() {
    let mut grid = grid();
    grid.set_focused(true);
    let mut damage = damage::sink();
    assert!(matches!(
        grid.on_key(Key::Named(NamedKey::Right), bounds(), &mut damage),
        Some(SwatchAction::Selected { index: 1, .. })
    ));
    let reported = damage.bounds();
    assert!(
        reported.contains(centre(&grid, 0)),
        "the well the mark left"
    );
    assert!(
        reported.contains(centre(&grid, 1)),
        "and the well it reached"
    );
}

/// The pointer's place is hit-testing input, never drawn: an event over no
/// well reports nothing.
#[test]
fn a_pointer_event_over_no_well_reports_nothing() {
    let mut grid = grid();
    let mut damage = damage::sink();
    grid.on_pointer(
        &moved(Point::new(-100, -100)),
        bounds(),
        SwatchMark::Primary,
        &mut damage,
    );
    assert_eq!(
        grid.on_pointer(&RELEASE, bounds(), SwatchMark::Primary, &mut damage),
        None
    );
    assert!(damage.is_empty());
}

/// The grid's arithmetic holds at the extremes: a rectangle wider than any
/// coordinate difference, one with no area, and a grid of no wells.
#[test]
fn the_layout_holds_at_the_extremes() {
    let grid = grid();
    let vast = Rect::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX);
    let last = grid.cell_rect(vast, COUNT - 1).expect("a cell");
    assert!(last.width > 0 && last.height > 0);
    let corner = Point::new(i32::MAX - 1, i32::MAX - 1);
    assert_eq!(grid.well_at(vast, corner), Some(COUNT - 1));
    assert_eq!(grid.well_at(vast, Point::new(i32::MIN, i32::MIN)), Some(0));
    let flat = Rect::new(0, 0, W, 0);
    assert_eq!(grid.cell_rect(flat, 0), None);
    assert_eq!(grid.well_at(flat, Point::new(1, 0)), None);
    let empty = SwatchGrid::new(0, Vec::new());
    assert_eq!(empty.rows(), 0);
    assert_eq!(empty.cell_rect(bounds(), 0), None);
    assert_eq!(empty.well_at(bounds(), Point::new(10, 10)), None);
    assert_eq!(empty.height_for_width(u32::MAX), 0);
    let mut surface = Surface::new(W, H).expect("a surface");
    empty.render(&mut surface, bounds(), Scale::ONE, &Theme::dark());
}

/// The primary mark is black over a light well and white over a dark one,
/// judged by the colour as it shows: a well nearly clear shows the surface
/// beneath it, so its mark follows the surface.
#[test]
fn the_mark_reads_over_the_colour_as_it_shows() {
    let theme = Theme::dark();
    let mark_over = |colour: Color| {
        let grid = SwatchGrid::new(1, vec![colour]);
        let mut surface = Surface::new(W, H).expect("a surface");
        grid.render(&mut surface, bounds(), Scale::ONE, &theme);
        surface.get(W / 2, H / 2).expect("on the surface")
    };
    let black = Color::rgb(0, 0, 0).premultiply();
    let white = Color::rgb(255, 255, 255).premultiply();
    assert_eq!(mark_over(Color::rgb(250, 250, 250)), black);
    assert_eq!(mark_over(Color::rgb(5, 5, 5)), white);
    assert_eq!(
        mark_over(Color::rgba(250, 250, 250, 10)),
        white,
        "a nearly clear light well shows the dark surface"
    );
}

/// Up and Down keep to the mark's column when the last row is short: a row
/// shorter than the columns moves nowhere, and a column the last row does
/// not reach wraps to the row above it.
#[test]
fn up_and_down_keep_to_the_column_over_a_short_last_row() {
    let key = |grid: &mut SwatchGrid, from: usize, named| {
        grid.set_focused(true);
        grid.adopt_selected(Some(from));
        match grid.on_key(Key::Named(named), bounds(), &mut damage::sink()) {
            Some(SwatchAction::Selected { index, .. }) => index,
            None => usize::MAX,
        }
    };
    let mut one_row = SwatchGrid::new(4, colours(3));
    assert_eq!(key(&mut one_row, 0, NamedKey::Down), 0);
    assert_eq!(key(&mut one_row, 2, NamedKey::Up), 2);
    let mut short = SwatchGrid::new(8, colours(17));
    assert_eq!(key(&mut short, 9, NamedKey::Down), 1, "wraps up its column");
    assert_eq!(key(&mut short, 8, NamedKey::Down), 16);
    assert_eq!(key(&mut short, 0, NamedKey::Up), 16);
    assert_eq!(
        key(&mut short, 1, NamedKey::Up),
        9,
        "the last row stops short"
    );
}

/// The primary mark is centred in its well at odd sizes too — mirrored
/// across both of the well's centre lines pixel for pixel — not a kite with
/// its apexes half a pixel off.
#[test]
fn the_primary_mark_is_centred_at_odd_sizes() {
    let theme = Theme::dark();
    let side = 35;
    let grid = SwatchGrid::new(1, colours(1));
    let mut surface = Surface::new(side, side).expect("a surface");
    grid.render(
        &mut surface,
        Rect::new(0, 0, side, side),
        Scale::ONE,
        &theme,
    );
    let at = |x: u32, y: u32| surface.get(x, y).expect("on the surface");
    for y in 0..side {
        for x in 0..side {
            assert_eq!(
                at(x, y),
                at(side - 1 - x, y),
                "mirrored across at ({x}, {y})"
            );
            assert_eq!(at(x, y), at(x, side - 1 - y), "mirrored down at ({x}, {y})");
        }
    }
}

/// Hit-testing a single row or column as wide as any extent holds: no share
/// arithmetic overflows.
#[test]
fn a_single_row_as_wide_as_any_extent_is_hit_tested() {
    let row = SwatchGrid::new(4, colours(3));
    let vast = Rect::new(i32::MIN, 0, u32::MAX, u32::MAX);
    assert_eq!(row.well_at(vast, Point::new(i32::MIN, 0)), Some(0));
    let column = SwatchGrid::new(1, colours(1));
    assert_eq!(column.well_at(vast, Point::new(0, i32::MAX - 1)), Some(0));
}

#[test]
fn an_unfocused_grid_takes_no_keys() {
    let mut grid = grid();
    let mut damage = damage::sink();
    assert_eq!(
        grid.on_key(Key::Named(NamedKey::Right), bounds(), &mut damage),
        None
    );
    assert_eq!(grid.selected(), Some(0));
    assert!(damage.is_empty());
}

#[test]
fn a_grid_that_is_not_actionable_takes_no_input() {
    for state in [
        ControlState::disabled(),
        ControlState::idle().with_authority(AuthorityState::Denied),
    ] {
        let mut grid = grid();
        grid.set_state(state);
        grid.set_focused(true);
        assert_eq!(click(&mut grid, 6, SwatchMark::Primary), None, "{state:?}");
        assert_eq!(
            grid.on_key(Key::Named(NamedKey::Right), bounds(), &mut damage::sink()),
            None
        );
        assert_eq!(grid.selected(), Some(0));
    }
}

#[test]
fn a_disabled_grid_is_veiled_and_a_denied_one_carries_the_authority_bead() {
    let theme = Theme::dark();
    let render = |state: ControlState| {
        let mut grid = grid();
        grid.set_state(state);
        let mut surface = Surface::new(W, H).expect("surface");
        grid.render(&mut surface, bounds(), Scale::ONE, &theme);
        surface
    };
    let plain = render(ControlState::idle());
    let disabled = render(ControlState::disabled());
    let denied = render(ControlState::idle().with_authority(AuthorityState::Denied));
    assert_ne!(plain.pixels(), disabled.pixels(), "the veil shows");
    let bead = premul(authority_rgba(theme.palette(), AuthorityState::Denied));
    assert!(has_pixel(&denied, bead), "the lock bead shows");
    assert!(!has_pixel(&plain, bead));
}

#[test]
fn focus_rings_the_marked_well_outside_the_well_itself() {
    let theme = Theme::dark();
    let mut grid = grid();
    grid.adopt_selected(Some(7));
    let render = |grid: &SwatchGrid| {
        let mut surface = Surface::new(W, H).expect("surface");
        grid.render(&mut surface, bounds(), Scale::ONE, &theme);
        surface
    };
    let resting = render(&grid);
    grid.set_focused(true);
    let focused = render(&grid);
    let cell = grid.cell_rect(bounds(), 7).expect("a well");
    let margin = Scale::ONE.scale_length(theme.metrics().control_gap) / 2;
    let well = cell.inset(margin);
    let mut changed = 0;
    for y in 0..H {
        for x in 0..W {
            if resting.get(x, y) != focused.get(x, y) {
                let at = Point::new(
                    i32::try_from(x).expect("fits"),
                    i32::try_from(y).expect("fits"),
                );
                assert!(cell.contains(at), "({x}, {y}) is outside the marked cell");
                assert!(
                    !well.contains(at),
                    "({x}, {y}) covers the well and its mark"
                );
                changed += 1;
            }
        }
    }
    assert!(changed > 0, "focus draws a ring");
}

#[test]
fn with_no_well_marked_the_first_key_marks_an_end() {
    let mut grid = grid();
    grid.set_focused(true);
    grid.adopt_selected(None);
    let mut damage = damage::sink();
    assert_eq!(
        grid.on_key(Key::Named(NamedKey::Right), bounds(), &mut damage),
        Some(SwatchAction::Selected {
            mark: SwatchMark::Primary,
            index: 0
        })
    );
    assert!(
        damage.bounds().contains(centre(&grid, 0)),
        "the new mark repaints"
    );
    grid.adopt_selected(None);
    assert_eq!(
        grid.on_key(Key::Named(NamedKey::Left), bounds(), &mut damage::sink()),
        Some(SwatchAction::Selected {
            mark: SwatchMark::Primary,
            index: COUNT - 1
        })
    );
}

#[test]
fn an_unmarked_grid_draws_no_primary_mark() {
    let theme = Theme::dark();
    let render = |grid: &SwatchGrid| {
        let mut surface = Surface::new(W, H).expect("surface");
        grid.render(&mut surface, bounds(), Scale::ONE, &theme);
        surface
    };
    let mut grid = grid();
    let marked = render(&grid);
    grid.adopt_selected(None);
    let unmarked = render(&grid);
    let rim = premul(theme.palette().rim_active);
    assert!(has_pixel(&marked, rim));
    assert!(
        !has_pixel(&unmarked, rim),
        "no well carries the mark's ring"
    );
}

/// The pointer and the press latch are hit-testing bookkeeping: grids that
/// differ only there compare equal and draw the same pixels.
#[test]
fn the_pointer_and_the_latch_are_not_drawn() {
    let theme = Theme::dark();
    let mut pressed = grid();
    let point = centre(&pressed, 3);
    pressed.on_pointer(
        &moved(point),
        bounds(),
        SwatchMark::Primary,
        &mut damage::sink(),
    );
    pressed.on_pointer(&PRESS, bounds(), SwatchMark::Primary, &mut damage::sink());
    let resting = grid();
    assert_eq!(pressed, resting);
    let render = |grid: &SwatchGrid| {
        let mut surface = Surface::new(W, H).expect("surface");
        grid.render(&mut surface, bounds(), Scale::ONE, &theme);
        surface
    };
    assert_eq!(render(&pressed).pixels(), render(&resting).pixels());
}
