use alloc::vec::Vec;

use tairix_image::{SpriteMode, SpriteName, SpritePalette};

use super::{of_canvas, thumbnail};
use crate::canvas::{CanvasBuilder, Kind, Sample};
use crate::document::{Picture, SpriteInfo};

const RED: [u8; 4] = [200, 10, 10, 255];
const BLUE: [u8; 4] = [10, 10, 200, 255];

/// A `width`×`height` truecolour picture, red but for a blue pixel at
/// `blue`, its pixels of eigen factors `eig`.
fn sprite(width: u32, height: u32, blue: (u32, u32), eig: (u8, u8)) -> Picture {
    let mut built =
        CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba(RED)).expect("a canvas");
    built.set(blue.0, blue.1, Sample::Rgba(BLUE));
    let mut picture = Picture::plain(built.finish());
    picture.sprite = Some(SpriteInfo {
        name: SpriteName::new("s").expect("a name"),
        mode: SpriteMode::truecolour(eig, false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    picture
}

fn at(pixels: &[u8], side: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * side + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

#[test]
fn a_small_sprite_grows_by_whole_pixels_and_stays_sharp() {
    let (pixels, placed) = thumbnail(&sprite(4, 2, (3, 1), (1, 1)), 16).expect("drawn");
    assert_eq!(placed, (0, 4, 16, 8), "four times, centred down the square");
    assert_eq!(at(&pixels, 16, 0, 4), RED);
    // The blue pixel is a sharp four-by-four block, its neighbours red.
    for y in 8..12 {
        for x in 12..16 {
            assert_eq!(at(&pixels, 16, x, y), BLUE, "({x}, {y})");
        }
        assert_eq!(at(&pixels, 16, 11, y), RED);
    }
    assert_eq!(at(&pixels, 16, 0, 3)[3], 0, "clear above");
    assert_eq!(at(&pixels, 16, 0, 12)[3], 0, "clear below");
}

#[test]
fn a_sprites_tall_pixels_are_shown_tall() {
    // Mode 12's pixels are twice as tall as they are wide.
    let (_, placed) = thumbnail(&sprite(4, 2, (0, 0), (1, 2)), 16).expect("drawn");
    assert_eq!(
        placed,
        (0, 0, 16, 16),
        "four wide by two tall pixels is square"
    );
    let (_, flat) = thumbnail(&sprite(4, 2, (0, 0), (1, 1)), 16).expect("drawn");
    assert_eq!(flat, (0, 4, 16, 8));
}

#[test]
fn a_large_picture_is_reduced_to_fit_its_square() {
    let (pixels, placed) = thumbnail(&sprite(64, 32, (0, 0), (1, 1)), 16).expect("drawn");
    assert_eq!(placed, (0, 4, 16, 8));
    let corner = at(&pixels, 16, 15, 11);
    assert_eq!(corner, RED, "far from the blue pixel");
    let reds: Vec<[u8; 4]> = (4..12).map(|y| at(&pixels, 16, 8, y)).collect();
    assert!(
        reds.iter().all(|pixel| pixel[3] == 255),
        "the picture is opaque"
    );
}

#[test]
fn no_square_draws_nothing() {
    assert!(thumbnail(&sprite(2, 2, (0, 0), (1, 1)), 0).is_none());
}

/// A layer's thumbnail is drawn from its own pixels at its picture's pixel
/// shape, as a picture of that one layer is.
#[test]
fn a_layer_is_drawn_as_a_picture_of_it_alone_is() {
    let picture = sprite(6, 3, (2, 1), (1, 2));
    assert_eq!(
        of_canvas(picture.canvas(), picture.pixel_aspect(), 16),
        thumbnail(&picture, 16)
    );
    assert!(of_canvas(picture.canvas(), (1, 1), 0).is_none());
}
