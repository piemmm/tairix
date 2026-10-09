use tairix_geometry::Scale;
use tairix_raster::{Color, Pixel};

use super::{snap_point, snap_span, GridLines};
use crate::preferences::{Grid, GridStyle};
use crate::shape::{Point as Fx, FX};

const CLEAR: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};

fn grid(spacing: u32, style: GridStyle) -> Grid {
    Grid {
        spacing: (spacing, spacing),
        offset: (0, 0),
        opacity: 1000,
        style,
        ..Grid::default()
    }
}

/// The screen pixels from `0` to `width` along row `y` the grid inks.
fn inked(lines: &GridLines, y: u32, width: u32) -> alloc::vec::Vec<u32> {
    let row = lines.row(y);
    (0..width)
        .filter(|&x| lines.over(row, x, y, CLEAR) != CLEAR)
        .collect()
}

#[test]
fn lines_stand_where_a_cells_first_pixel_starts_at_any_zoom() {
    // Four screen pixels a picture pixel: a line every 4 × 4 = 16 columns.
    let lines = GridLines::new(
        &grid(4, GridStyle::Lines),
        (0, 0),
        (4, 4, 1),
        (0, 64),
        Scale::ONE,
    )
    .expect("shown");
    assert_eq!(inked(&lines, 1, 64), [0, 16, 32, 48]);
    assert_eq!(
        inked(&lines, 16, 64).len(),
        64,
        "a horizontal line's whole row"
    );
    // At half size a 16-pixel cell is 8 columns.
    let halved = GridLines::new(
        &grid(16, GridStyle::Lines),
        (0, 0),
        (1, 1, 2),
        (0, 32),
        Scale::ONE,
    )
    .expect("shown");
    assert_eq!(inked(&halved, 3, 32), [0, 8, 16, 24]);
}

#[test]
fn an_offset_and_a_scrolled_origin_move_the_lines_with_the_picture() {
    let mut shifted = grid(4, GridStyle::Lines);
    shifted.offset = (1, 0);
    let lines = GridLines::new(&shifted, (-8, 0), (4, 4, 1), (0, 48), Scale::ONE).expect("shown");
    // Picture column 1 starts at screen -8 + 4 = -4: the next lines at 12 and
    // 28, and 44.
    assert_eq!(inked(&lines, 1, 48), [12, 28, 44]);
}

#[test]
fn a_grid_too_dense_to_read_is_not_drawn() {
    assert!(GridLines::new(
        &grid(2, GridStyle::Lines),
        (0, 0),
        (1, 1, 1),
        (0, 32),
        Scale::ONE
    )
    .is_none());
    assert!(GridLines::new(
        &grid(16, GridStyle::Lines),
        (0, 0),
        (1, 1, 8),
        (0, 32),
        Scale::ONE
    )
    .is_none());
}

#[test]
fn each_style_inks_its_own_pattern() {
    let at = |style| {
        GridLines::new(&grid(4, style), (0, 0), (4, 4, 1), (0, 64), Scale::ONE).expect("shown")
    };
    let dashes = at(GridStyle::Dashes);
    assert_eq!(inked(&dashes, 16, 12), [0, 1, 2, 3, 8, 9, 10, 11]);
    assert_eq!(inked(&dashes, 5, 64), [], "a vertical line's gap");
    let dots = at(GridStyle::Dots);
    assert_eq!(inked(&dots, 16, 10), [0, 3, 6, 9]);
    let crossings = at(GridStyle::Crossings);
    assert_eq!(
        inked(&crossings, 16, 24),
        [0, 1, 2, 3, 13, 14, 15, 16, 17, 18, 19]
    );
    assert_eq!(
        inked(&crossings, 2, 24),
        [0, 16],
        "an arm's reach from a crossing"
    );
    assert_eq!(inked(&crossings, 8, 24), [], "between crossings");
}

#[test]
fn the_ink_is_the_grids_colour_at_its_opacity() {
    let mut faint = grid(4, GridStyle::Lines);
    faint.colour = tairix_colour::Rgb::new(255, 0, 0);
    faint.opacity = 500;
    let lines = GridLines::new(&faint, (0, 0), (4, 4, 1), (0, 8), Scale::ONE).expect("shown");
    let white = Color::rgba(255, 255, 255, 255).premultiply();
    let row = lines.row(1);
    let over = lines.over(row, 0, 1, white);
    assert!(over.r == 255 && over.g < 200 && over.g > 100, "{over:?}");
}

#[test]
fn a_point_lands_on_the_pixel_at_the_nearest_crossing() {
    let cells = grid(16, GridStyle::Lines);
    assert_eq!(
        snap_point(
            &cells,
            Fx {
                x: 7 * FX,
                y: 9 * FX
            }
        ),
        Fx::centre_of(0, 16)
    );
    assert_eq!(
        snap_point(
            &cells,
            Fx {
                x: 8 * FX,
                y: 25 * FX
            }
        ),
        Fx::centre_of(16, 32)
    );
    let mut shifted = cells;
    shifted.offset = (4, 0);
    assert_eq!(
        snap_point(&shifted, Fx { x: 30 * FX, y: 0 }),
        Fx::centre_of(36, 0)
    );
}

#[test]
fn a_box_covers_whole_cells_whichever_way_it_is_dragged() {
    let cells = grid(16, GridStyle::Lines);
    assert_eq!(snap_span(&cells, (2, 3), (29, 40)), ((0, 0), (31, 47)));
    assert_eq!(snap_span(&cells, (29, 40), (2, 3)), ((31, 47), (0, 0)));
    assert_eq!(
        snap_span(&cells, (5, 5), (6, 6)),
        ((0, 0), (15, 15)),
        "never less than a cell"
    );
    assert_eq!(snap_span(&cells, (20, 20), (19, 19)), ((15, 15), (0, 0)));
}
