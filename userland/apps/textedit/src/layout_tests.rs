//! Host tests for the editor window's geometry. No test hard-codes a
//! coordinate: each asks where a band is and checks a property of it.

use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_theme::{TextRole, ThemeRegistry};

use super::{Faces, Layout, FIND_BUTTONS, MENUS, MIN_ROWS, STATUS_FIELDS, WINDOW_SIZE};

fn layout(width: u32, height: u32, find: bool, digits: u32) -> Layout {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let scale = Scale::ONE;
    let faces = Faces {
        grid: BitmapFont::for_role(theme.fonts(), TextRole::Monospace, scale),
        status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale),
    };
    Layout::for_window(width, height, theme, scale, faces, find, digits)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    !a.intersection(&b).is_empty()
}

/// Whether `band` lies within `window`; an empty band lies anywhere.
fn inside(band: Rect, window: Rect) -> bool {
    band.is_empty() || band.intersection(&window) == band
}

fn bands(layout: &Layout) -> [Rect; 7] {
    [
        layout.menu_row(),
        layout.find(),
        layout.gutter(),
        layout.grid(),
        layout.vertical_bar(),
        layout.horizontal_bar(),
        layout.status(),
    ]
}

#[test]
fn no_two_bands_share_a_pixel_and_all_lie_in_the_window() {
    for (find, digits) in [(false, 3), (true, 5), (true, 0)] {
        let layout = layout(900, 640, find, digits);
        let bands = bands(&layout);
        for (at, band) in bands.iter().enumerate() {
            assert!(
                inside(*band, layout.window()),
                "band {at} leaves the window"
            );
            for other in &bands[at + 1..] {
                assert!(!overlaps(*band, *other), "{band:?} and {other:?} overlap");
            }
        }
        assert!(!overlaps(layout.corner(), layout.grid()));
    }
}

#[test]
fn the_find_bar_takes_room_only_while_open() {
    let closed = layout(900, 640, false, 3);
    let open = layout(900, 640, true, 3);
    assert!(closed.find().is_empty());
    assert!(!open.find().is_empty());
    assert!(open.grid().height < closed.grid().height);
    assert!(open.find_field().width > open.replace_field().width);
    for (button, next) in open
        .find_buttons()
        .iter()
        .zip(open.find_buttons().iter().skip(1))
    {
        assert!(
            button.right() <= next.left(),
            "the buttons run left to right"
        );
    }
    assert!(
        open.find_buttons().iter().all(|button| !button.is_empty()),
        "an ordinary window fits every button"
    );
    assert_eq!(open.find_buttons().len(), FIND_BUTTONS.len());
}

#[test]
fn the_gutter_widens_with_the_line_numbers_and_vanishes_for_hex() {
    let narrow = layout(900, 640, false, 2);
    let wide = layout(900, 640, false, 7);
    let hex = layout(900, 640, false, 0);
    assert!(wide.gutter().width > narrow.gutter().width);
    assert!(hex.gutter().is_empty());
    assert_eq!(hex.grid().left(), 0);
    assert_eq!(narrow.grid().left(), narrow.gutter().right());
}

#[test]
fn menus_and_status_fields_hold_their_order() {
    let layout = layout(900, 640, false, 3);
    let menus = layout.menus();
    assert_eq!(menus.len(), MENUS.len());
    assert!(menus
        .windows(2)
        .all(|pair| pair[0].right() < pair[1].left()));
    let fields = layout.status_fields();
    assert_eq!(fields.len(), STATUS_FIELDS);
    assert!(
        fields
            .windows(2)
            .all(|pair| pair[1].right() <= pair[0].left()),
        "right to left"
    );
    assert!(layout.position().right() <= layout.message().left());
    assert!(layout.message().right() <= fields[STATUS_FIELDS - 1].left());
}

#[test]
fn cells_map_points_to_rows_and_columns() {
    let layout = layout(900, 640, false, 3);
    let (cell_w, cell_h) = layout.cell();
    let grid = layout.grid();
    let point = Point::new(
        grid.left() + to_i32(cell_w * 3) + 1,
        grid.top() + to_i32(cell_h * 2) + 1,
    );
    assert_eq!(
        layout.cell_at(point),
        Some((2, 6)),
        "the left half of column three"
    );
    let right_half = Point::new(
        grid.left() + to_i32(cell_w * 3 + cell_w * 3 / 4),
        grid.top(),
    );
    assert_eq!(
        layout.cell_at(right_half),
        Some((0, 7)),
        "the right half of column three"
    );
    assert_eq!(
        layout.cell_at(Point::new(grid.left() - 2, grid.top())),
        Some((0, 0)),
        "the gutter selects the row"
    );
    assert_eq!(
        layout.cell_at(Point::new(grid.left(), layout.status().top())),
        None
    );
    assert_eq!(layout.cell_near(Point::new(-50, -50)), (0, 0));
    assert!(layout.rows() > 0 && layout.columns() > 0);
    let row = layout.row_rect(1);
    assert_eq!(
        (row.top(), row.height),
        (grid.top() + to_i32(cell_h), cell_h)
    );
    assert!(
        layout.row_rect(10_000).is_empty(),
        "a row past the grid is nowhere"
    );
}

#[test]
fn a_tiny_window_keeps_its_menus_and_gives_up_the_grid() {
    let layout = layout(120, 60, true, 3);
    assert!(!layout.menu_row().is_empty());
    for band in bands(&layout) {
        assert!(inside(band, layout.window()), "{band:?}");
    }
    assert!(layout.rows() * layout.cell().1 as usize <= layout.grid().height as usize);
}

/// The smallest window still reaches every menu and shows a few rows of
/// grid, at every density, and the window a document opens at is larger.
#[test]
fn the_smallest_window_keeps_the_menus_and_a_few_rows() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    for scale in [Scale::ONE, Scale::from_percent(200).expect("a valid scale")] {
        let faces = Faces {
            grid: BitmapFont::for_role(theme.fonts(), TextRole::Monospace, scale),
            status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale),
        };
        let (width, height) = Layout::min_size(theme, scale, faces);
        let layout = Layout::for_window(width, height, theme, scale, faces, false, 3);
        for menu in layout.menus() {
            assert!(
                !menu.is_empty() && inside(*menu, layout.window()),
                "a menu is cut off at {scale:?}"
            );
        }
        assert!(
            layout.rows() >= MIN_ROWS as usize,
            "{} rows at {scale:?}",
            layout.rows()
        );
        let opens = (
            scale.scale_length(WINDOW_SIZE.0),
            scale.scale_length(WINDOW_SIZE.1),
        );
        assert!(
            opens.0 >= width && opens.1 >= height,
            "a new window opens smaller than it may be"
        );
    }
}
