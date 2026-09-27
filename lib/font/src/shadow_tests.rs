//! The soft text shadow: where it lands, how it falls off, that cutting it
//! to a surface or a stated origin changes no pixel it draws, and that it
//! never moves the pen.

use tairix_geometry::Scale;
use tairix_raster::{Color, Pixel, Surface};

use super::TextShadow;
use crate::client::install_test_transport;
use crate::font::BitmapFont;

const INK: Color = Color::rgb(40, 90, 250);
const BLACK: Color = Color::rgb(0, 0, 0);
const WHITE: Color = Color::rgb(255, 255, 255);

/// The console face, whose every glyph but space the shared test transport
/// serves as one solid `CELL_WIDTH` × `CELL_HEIGHT` block.
fn font() -> BitmapFont {
    install_test_transport();
    BitmapFont::console()
}

fn ground(width: u32, height: u32) -> Surface {
    Surface::filled(width, height, WHITE.premultiply()).expect("a test surface")
}

fn shadow() -> TextShadow {
    TextShadow::new(BLACK, Scale::ONE)
}

/// How dark `pixel` is against the white ground, `0` for untouched.
fn darkness(pixel: Option<Pixel>) -> u32 {
    pixel.map_or(0, |pixel| 255 - u32::from(pixel.g))
}

#[test]
fn a_shadow_inks_ground_the_plain_draw_left_alone() {
    let font = font();
    let (mut plain, mut shadowed) = (ground(64, 40), ground(64, 40));
    font.draw_text(&mut plain, 4, 4, "Hi", INK);
    font.draw_text_shadowed(&mut shadowed, 4, 4, "Hi", INK, shadow());
    let gained = (0..40).any(|y| {
        (0..64).any(|x| plain.get(x, y) != shadowed.get(x, y) && darkness(plain.get(x, y)) == 0)
    });
    assert!(gained, "the shadow reached no ground the ink had not");
}

#[test]
fn the_shadow_falls_away_softly_below_the_ink() {
    let font = font();
    let mut surface = ground(40, 48);
    font.draw_shadow(&mut surface, 8, 4, "H", shadow());
    let bottom = 4 + crate::atlas::CELL_HEIGHT;
    let column = 8 + crate::atlas::CELL_WIDTH / 2;
    let reach = shadow().reach() + shadow().drop();
    let under: alloc::vec::Vec<u32> = (bottom..bottom + reach)
        .map(|y| darkness(surface.get(column, y)))
        .collect();
    assert!(under[0] > 0, "the shadow does not reach below the ink");
    assert!(
        under.windows(2).all(|pair| pair[0] >= pair[1]) && under[0] > under[under.len() - 1],
        "the shadow does not fall off away from the ink: {under:?}"
    );
    assert_eq!(
        darkness(surface.get(column, bottom + reach)),
        0,
        "the shadow reaches past its reach"
    );
}

#[test]
fn the_shadow_stays_inside_the_ink_grown_by_its_reach() {
    let font = font();
    let (mut plain, mut shadowed) = (ground(64, 48), ground(64, 48));
    let (x, y) = (10u32, 12u32);
    font.draw_text(&mut plain, 10, 12, "Hi", INK);
    font.draw_text_shadowed(&mut shadowed, 10, 12, "Hi", INK, shadow());
    let reach = shadow().reach();
    let across = x - reach..x + 2 * crate::atlas::CELL_WIDTH + reach;
    let down = y + shadow().drop() - reach..y + crate::atlas::CELL_HEIGHT + shadow().drop() + reach;
    for row in 0..48 {
        for column in 0..64 {
            if !across.contains(&column) || !down.contains(&row) {
                assert_eq!(
                    plain.get(column, row),
                    shadowed.get(column, row),
                    "({column}, {row}) lies past the shadow's reach"
                );
            }
        }
    }
}

#[test]
fn a_shadow_cut_by_the_surface_edge_draws_what_the_whole_one_does() {
    let font = font();
    let mut whole = ground(64, 40);
    font.draw_text_shadowed(&mut whole, 6, 8, "Hi", INK, shadow());
    // The same drawing's columns 10..30, so the first glyph and part of its
    // shadow fall off the left edge.
    let mut cut = ground(20, 40);
    font.draw_text_shadowed(&mut cut, 6 - 10, 8, "Hi", INK, shadow());
    for y in 0..40 {
        for x in 0..20 {
            assert_eq!(cut.get(x, y), whole.get(x + 10, y), "({x}, {y}) of the cut");
        }
    }
}

#[test]
fn a_shadowed_run_under_a_stated_origin_lands_where_the_drawing_says() {
    let font = font();
    let mut whole = ground(64, 40);
    font.draw_text_shadowed(&mut whole, 6, 8, "Hi", INK, shadow());
    // Rows 14..24 of the same drawing, drawn in the drawing's coordinates:
    // the ink and the shadow alike must land beyond the strip's own height.
    let mut strip = ground(64, 10);
    strip.with_origin(0, 14, |strip| {
        font.draw_text_shadowed(strip, 6, 8, "Hi", INK, shadow());
    });
    for y in 0..10 {
        for x in 0..64 {
            assert_eq!(
                strip.get(x, y),
                whole.get(x, y + 14),
                "({x}, {y}) of the strip"
            );
        }
    }
}

#[test]
fn every_shadow_then_every_ink_is_the_shadowed_draw() {
    let font = font();
    let (mut together, mut apart) = (ground(64, 40), ground(64, 40));
    font.draw_text_shadowed(&mut together, 3, 5, "Hi", INK, shadow());
    let pen = font.draw_shadow(&mut apart, 3, 5, "Hi", shadow());
    font.draw_text(&mut apart, 3, 5, "Hi", INK);
    assert_eq!(pen, font.draw_text(&mut ground(64, 40), 3, 5, "Hi", INK));
    assert_eq!(together.pixels(), apart.pixels());
}

#[test]
fn a_shadowed_run_returns_the_plain_runs_pen() {
    install_test_transport();
    let proportional = tairix_abi::font_ipc::FamilyKey::new("inter").expect("a family key");
    for font in [BitmapFont::console(), BitmapFont::new(proportional, 20)] {
        assert_eq!(
            font.draw_text_shadowed(&mut ground(64, 40), 3, 4, "Hi", INK, shadow()),
            font.draw_text(&mut ground(64, 40), 3, 4, "Hi", INK),
            "the shadow moved the pen"
        );
    }
}

#[test]
fn a_transparent_or_fully_faded_shadow_leaves_the_plain_frame() {
    let font = font();
    let mut plain = ground(64, 40);
    font.draw_text(&mut plain, 2, 2, "Hi", INK);
    for quiet in [
        TextShadow::new(Color::rgba(0, 0, 0, 0), Scale::ONE),
        shadow().faded(0),
    ] {
        let mut shadowed = ground(64, 40);
        font.draw_text_shadowed(&mut shadowed, 2, 2, "Hi", INK, quiet);
        assert_eq!(plain.pixels(), shadowed.pixels());
    }
}

#[test]
fn a_faded_shadow_is_lighter_and_a_full_strength_one_is_unchanged() {
    assert_eq!(shadow().faded(u8::MAX), shadow());
    let font = font();
    let (mut full, mut half) = (ground(40, 40), ground(40, 40));
    font.draw_shadow(&mut full, 8, 4, "H", shadow());
    font.draw_shadow(&mut half, 8, 4, "H", shadow().faded(128));
    let below = 4 + crate::atlas::CELL_HEIGHT + 1;
    let at = |surface: &Surface| darkness(surface.get(12, below));
    assert!(
        at(&half) > 0 && at(&half) < at(&full),
        "the shadow did not fade"
    );
}

#[test]
fn the_shadow_lengths_are_logical_pixels_and_never_nothing() {
    let at = |percent| TextShadow::new(BLACK, Scale::from_percent(percent).expect("a scale"));
    assert_eq!((at(100).drop(), at(100).reach()), (1, 3));
    assert_eq!((at(200).drop(), at(200).reach()), (2, 6));
    let smallest = at(Scale::MIN_PERCENT);
    assert!(
        smallest.drop() >= 1 && smallest.reach() >= 1,
        "the shadow vanished"
    );
}
