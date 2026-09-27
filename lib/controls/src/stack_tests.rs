//! Unit tests for the plate column: placement, the height it measures, and
//! the width it inverts.

use tairix_geometry::{to_i32, Rect, Scale};
use tairix_theme::Theme;

use crate::stack::{column_width, gap, height, place, plate_width};

const HEIGHTS: [u32; 3] = [40, 70, 25];

fn plate(index: usize) -> u32 {
    HEIGHTS.get(index).copied().unwrap_or(0)
}

#[test]
fn plates_sit_a_gap_apart_and_a_gap_inside_the_column() {
    let theme = Theme::dark();
    let scale = Scale::ONE;
    let g = gap(scale, &theme);
    let bounds = Rect::new(10, 20, 300, 1000);
    let placed = place(bounds, HEIGHTS.len(), scale, &theme, plate);
    assert_eq!(placed.len(), HEIGHTS.len());
    let mut top = bounds.top() + to_i32(g);
    for (index, rect) in &placed {
        assert_eq!(rect.left(), bounds.left() + to_i32(g));
        assert_eq!(rect.width, plate_width(bounds.width, scale, &theme));
        assert_eq!(rect.top(), top, "plate {index}");
        assert_eq!(rect.height, plate(*index));
        top += to_i32(plate(*index) + g);
    }
}

#[test]
fn a_column_shorter_than_its_plates_still_places_each_at_its_natural_size() {
    // What does not fit is its owner's to scroll to, never dropped or squeezed.
    let theme = Theme::dark();
    let scale = Scale::ONE;
    let placed = place(
        Rect::new(0, 0, 300, 10),
        HEIGHTS.len(),
        scale,
        &theme,
        plate,
    );
    assert_eq!(placed.len(), HEIGHTS.len());
    assert!(placed
        .iter()
        .all(|(index, rect)| rect.height == plate(*index)));
}

#[test]
fn the_measured_height_is_exactly_what_the_placed_column_reaches() {
    for scale in [Scale::ONE, Scale::from_percent(200).expect("scale")] {
        let theme = Theme::dark();
        let g = gap(scale, &theme);
        let tall = height(HEIGHTS, scale, &theme);
        assert_eq!(tall, HEIGHTS.iter().sum::<u32>() + g * 4);
        let placed = place(
            Rect::new(0, 0, 300, tall),
            HEIGHTS.len(),
            scale,
            &theme,
            plate,
        );
        let bottom = placed.last().map_or(0, |(_, rect)| rect.bottom());
        assert_eq!(
            bottom + to_i32(g),
            to_i32(tall),
            "a gap beneath the last plate"
        );
    }
}

#[test]
fn the_column_width_inverts_the_plate_width() {
    for scale in [Scale::ONE, Scale::from_percent(150).expect("scale")] {
        let theme = Theme::dark();
        for width in [0, 1, 17, 300] {
            assert_eq!(
                plate_width(column_width(width, scale, &theme), scale, &theme),
                width
            );
        }
    }
}
