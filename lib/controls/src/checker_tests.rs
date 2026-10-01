use tairix_geometry::Scale;
use tairix_raster::{Color, Surface};
use tairix_theme::ThemeRegistry;

use super::{Checker, CHECKER_SIDE};

#[test]
fn squares_alternate_from_a_dark_one_at_the_corner() {
    let registry = ThemeRegistry::with_builtins();
    let board = Checker::new(registry.active(), Scale::ONE);
    assert_eq!(board.side(), CHECKER_SIDE);
    let dark = board.at(0, 0);
    let light = board.at(CHECKER_SIDE, 0);
    assert_ne!(dark, light);
    assert_eq!(board.at(CHECKER_SIDE, CHECKER_SIDE), dark);
    assert_eq!(board.at(0, CHECKER_SIDE * 3), light);
    assert_eq!(dark, Color::from(registry.active().palette().surface));
}

#[test]
fn painting_draws_exactly_what_at_answers() {
    let registry = ThemeRegistry::with_builtins();
    let board = Checker::new(registry.active(), Scale::ONE).with_side(3);
    let mut surface = Surface::new(20, 11).expect("a surface");
    board.paint(&mut surface, 2, 1, 17, 9);
    for y in 1..10 {
        for x in 2..19 {
            assert_eq!(
                surface.get(x, y),
                Some(board.at(x - 2, y - 1).premultiply()),
                "({x}, {y})"
            );
        }
    }
    assert_eq!(surface.get(0, 0), Some(Color::TRANSPARENT.premultiply()));
}

#[test]
fn a_zero_side_is_one_pixel() {
    let registry = ThemeRegistry::with_builtins();
    assert_eq!(
        Checker::new(registry.active(), Scale::ONE)
            .with_side(0)
            .side(),
        1
    );
}
