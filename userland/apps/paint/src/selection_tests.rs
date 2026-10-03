use alloc::vec;
use alloc::vec::Vec;

use tairix_image::IndexDepth;

use super::{adapt_pasted, cut_out, floating_kind, over, Floating};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::colour::Ink;
use crate::mask::Mask;
use crate::shape::{Bounds, Shape, ShapeScratch, Span};

fn marked(width: u32, height: u32) -> Canvas {
    let mut built =
        CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..height {
        for x in 0..width {
            let (x8, y8) = (
                u8::try_from(x).expect("small"),
                u8::try_from(y).expect("small"),
            );
            built.set(x, y, Sample::Rgba([x8, y8, 0, 255]));
        }
    }
    built.finish()
}

fn area(x0: i64, y0: i64, x1: i64, y1: i64) -> Mask {
    Mask::rect(Bounds { x0, y0, x1, y1 }).expect("pixels")
}

/// The ellipse inside pixels `from` to `to`, its edge smoothed.
fn oval(from: (i64, i64), to: (i64, i64)) -> Mask {
    let shape = Shape::Ellipse {
        span: Span { from, to },
        outline: None,
    };
    let within = Bounds {
        x0: 0,
        y0: 0,
        x1: 1000,
        y1: 1000,
    };
    Mask::shape(&shape, true, within, &mut ShapeScratch::default())
        .expect("room")
        .expect("pixels")
}

/// Put the tiles `put` answers in `picture`, as the window adopts them.
fn adopt(picture: &mut Canvas, put: Vec<(usize, alloc::sync::Arc<crate::canvas::Tile>)>) {
    for (index, tile) in put {
        picture.replace_tile(index, tile);
    }
}

/// A lift writes nothing: the picture shows the ink left behind only through
/// the layer, and putting it down elsewhere moves the pixels and leaves the
/// ink, in tiles the put-down answers.
#[test]
fn lifting_leaves_the_ink_behind_and_putting_down_elsewhere_moves_the_pixels() {
    let mut picture = marked(100, 80);
    let original = picture.try_clone().expect("room");
    let mut floating = Floating::lift(&picture, &area(10, 10, 20, 15), Ink::Clear).expect("room");
    assert_eq!(picture, original, "the lift wrote nothing");
    let below = picture.sample(12, 12).expect("on the picture");
    assert_eq!(
        floating.shows(12, 12, below, false),
        Sample::Rgba([12, 12, 0, 255]),
        "the layer lies over where it came from"
    );
    floating.shift(60, 50);
    assert_eq!(
        floating.shows(12, 12, below, false),
        Sample::Rgba([0; 4]),
        "and moved off it, the ink left shows"
    );
    let mut down = picture.try_clone().expect("room");
    let put = floating.put_down(&mut down).expect("room");
    adopt(&mut picture, put);
    assert_eq!(picture, down, "every tile written is answered");
    assert_eq!(picture.sample(72, 62), Some(Sample::Rgba([12, 12, 0, 255])));
    assert_eq!(picture.sample(12, 12), Some(Sample::Rgba([0; 4])));
}

#[test]
fn a_layer_put_down_partly_off_the_picture_keeps_what_lands_on_it() {
    let mut picture = marked(40, 40);
    let mut floating = Floating::lift(&picture, &area(0, 0, 10, 10), Ink::Clear).expect("room");
    floating.shift(35, -5);
    let put = floating.put_down(&mut picture).expect("room");
    assert!(!put.is_empty());
    assert_eq!(picture.sample(39, 0), Some(Sample::Rgba([4, 5, 0, 255])));
}

/// A row composed through the layer is the pixels [`Floating::shows`]
/// answers one at a time, the lift's leavings and the layer both.
#[test]
fn a_row_composed_through_the_layer_is_what_each_pixel_shows() {
    let picture = marked(60, 30);
    let mut floating = Floating::lift(&picture, &area(5, 5, 25, 15), Ink::Clear).expect("room");
    floating.shift(8, 2);
    for y in 0..30 {
        let mut row = vec![Sample::Rgba([0; 4]); 50];
        picture.row_samples(y, 3, &mut row);
        let mut above = vec![Sample::Rgba([0; 4]); 50];
        let mut composed = row.clone();
        floating.compose_row(i64::from(y), 3, &mut composed, &mut above, false);
        for (x, (shown, below)) in (3..).zip(composed.iter().zip(&row)) {
            assert_eq!(
                *shown,
                floating.shows(x, i64::from(y), *below, false),
                "({x}, {y})"
            );
        }
    }
}

/// Clearing writes the ink over the area and nothing beside it, answering
/// the tiles it wrote.
#[test]
fn clearing_lays_the_ink_over_the_area_alone() {
    let mut picture = marked(80, 80);
    let put = super::cleared(&mut picture, &area(70, 70, 75, 75), Ink::Clear).expect("room");
    assert_eq!(put.len(), 1, "one tile");
    assert_eq!(picture.sample(72, 72), Some(Sample::Rgba([0; 4])));
    assert_eq!(picture.sample(69, 72), Some(Sample::Rgba([69, 72, 0, 255])));
}

/// A palette layer put straight back where it was lifted keeps its soft
/// mask: over nothing at all it is itself, not its mask's threshold.
#[test]
fn a_soft_mask_put_straight_back_is_kept() {
    let kind = palette_kind(true);
    let mut built = CanvasBuilder::new(4, 1, kind.clone(), Sample::Index(1, 255)).expect("fits");
    built.set(0, 0, Sample::Index(2, 200));
    built.set(1, 0, Sample::Index(2, 100));
    let mut picture = built.finish();
    let floating = Floating::lift(&picture, &area(0, 0, 2, 1), Ink::Clear).expect("room");
    let put = floating.put_down(&mut picture).expect("room");
    assert!(!put.is_empty());
    assert_eq!(picture.sample(0, 0), Some(Sample::Index(2, 200)));
    assert_eq!(picture.sample(1, 0), Some(Sample::Index(2, 100)));
}

#[test]
fn what_a_floating_selection_does_not_cover_leaves_the_picture_showing() {
    let below = Sample::Rgba([9, 9, 9, 255]);
    assert_eq!(over(below, Sample::Rgba([1, 2, 3, 0])), below);
    assert_eq!(
        over(below, Sample::Rgba([1, 2, 3, 255])),
        Sample::Rgba([1, 2, 3, 255])
    );
    assert_eq!(
        over(Sample::Index(4, 255), Sample::Index(2, 0)),
        Sample::Index(4, 255)
    );
    assert_eq!(
        over(Sample::Index(4, 255), Sample::Index(2, 200)),
        Sample::Index(2, 255)
    );
}

fn palette_kind(masked: bool) -> Kind {
    Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 255], [255, 0, 0, 255]],
        masked,
    }
}

#[test]
fn a_layer_over_an_unmasked_palette_picture_holds_its_own_transparency() {
    assert!(floating_kind(&palette_kind(false)).masked());
    let mut picture = Canvas::new(8, 8, palette_kind(false), Sample::Index(1, 255)).expect("fits");
    let mut floating = Floating::lift(&picture, &area(0, 0, 4, 4), Ink::Index(0)).expect("room");
    assert_eq!(floating.sample_at(1, 1), Some(Sample::Index(1, 255)));
    floating.shift(4, 4);
    let put = floating.put_down(&mut picture).expect("room");
    assert!(!put.is_empty());
    assert_eq!(
        picture.sample(1, 1),
        Some(Sample::Index(0, 255)),
        "the secondary left behind"
    );
}

#[test]
fn a_pasted_picture_takes_the_nearest_colours_the_picture_holds() {
    let mut built = CanvasBuilder::new(3, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    built.set(0, 0, Sample::Rgba([250, 10, 10, 255]));
    built.set(1, 0, Sample::Rgba([240, 240, 240, 255]));
    built.set(2, 0, Sample::Rgba([240, 240, 240, 20]));
    let adapted = adapt_pasted(&built.finish(), &palette_kind(false)).expect("room");
    assert_eq!(adapted.sample(0, 0), Some(Sample::Index(2, 255)));
    assert_eq!(adapted.sample(1, 0), Some(Sample::Index(1, 255)));
    assert_eq!(
        adapted.sample(2, 0),
        Some(Sample::Index(0, 0)),
        "clear stays clear"
    );
    let colour = adapt_pasted(&marked(2, 2), &Kind::Rgba).expect("room");
    assert_eq!(colour, marked(2, 2));
}

#[test]
fn a_cut_out_is_the_area_alone_in_the_pictures_own_kind() {
    let picture = marked(30, 30);
    let piece = cut_out(&picture, &area(5, 6, 8, 10)).expect("room");
    assert_eq!((piece.width(), piece.height()), (3, 4));
    assert_eq!(piece.sample(2, 3), Some(Sample::Rgba([7, 9, 0, 255])));
    let clipped = cut_out(&picture, &area(25, 25, 40, 40)).expect("room");
    assert_eq!((clipped.width(), clipped.height()), (5, 5));
}

/// An opaque colour laid over a colour is exactly what the blend would make
/// of it, however the colour beneath is.
#[test]
fn an_opaque_colour_over_a_colour_is_that_colour() {
    use crate::stroke::{lay_over, Blend, Coat};
    let ink = [12, 200, 77, 255];
    for below in [[0, 0, 0, 0], [255, 255, 255, 255], [40, 50, 60, 128]] {
        let blended = lay_over(
            Sample::Rgba(below),
            Coat {
                ink: Ink::Colour(ink),
                blend: Blend::Over,
            },
            u8::MAX,
            false,
        );
        assert_eq!(over(Sample::Rgba(below), Sample::Rgba(ink)), blended);
    }
}

/// A soft selection lifts as much of each pixel as it chose: the corner of
/// an ellipse stays behind, its middle moves wholly, and its edge partly.
#[test]
fn a_soft_selection_moves_as_much_of_each_pixel_as_it_chose() {
    let mut picture = marked(60, 40);
    let chosen = oval((4, 4), (23, 15));
    let edge = (4..23)
        .find(|&x| (1..255).contains(&chosen.at(x, 9)))
        .expect("a soft edge");
    let mut floating = Floating::lift(&picture, &chosen, Ink::Clear).expect("room");
    floating.shift(30, 20);
    let put = floating.put_down(&mut picture).expect("room");
    assert!(!put.is_empty());
    assert_eq!(
        picture.sample(4, 4),
        Some(Sample::Rgba([4, 4, 0, 255])),
        "the corner stays"
    );
    assert_eq!(
        picture.sample(13, 9),
        Some(Sample::Rgba([0; 4])),
        "the middle moved"
    );
    assert_eq!(
        picture.sample(43, 29),
        Some(Sample::Rgba([13, 9, 0, 255])),
        "to here"
    );
    let Some(Sample::Rgba(left)) = picture.sample(u32::try_from(edge).expect("on it"), 9) else {
        panic!("a colour picture");
    };
    assert!(
        left[3] > 0 && left[3] < 255,
        "part of the edge stays: {left:?}"
    );
    assert_eq!(
        floating.selection().map(|mask| mask.bounds()),
        Some(Bounds {
            x0: chosen.bounds().x0 + 30,
            y0: chosen.bounds().y0 + 20,
            x1: chosen.bounds().x1 + 30,
            y1: chosen.bounds().y1 + 20,
        }),
        "the selection travels with the layer"
    );
}

#[test]
fn a_soft_cut_out_holds_its_edge_as_transparency() {
    let picture = marked(40, 30);
    let chosen = oval((2, 2), (21, 13));
    let piece = cut_out(&picture, &chosen).expect("room");
    let bounds = chosen.bounds();
    assert_eq!(
        (i64::from(piece.width()), i64::from(piece.height())),
        (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0)
    );
    assert_eq!(
        piece.sample(0, 0),
        Some(Sample::Rgba([0; 4])),
        "a corner the ellipse leaves out is clear, its colour not carried"
    );
    let middle = piece
        .sample(
            u32::try_from(12 - bounds.x0).expect("inside"),
            u32::try_from(7 - bounds.y0).expect("inside"),
        )
        .expect("on it");
    assert_eq!(middle, Sample::Rgba([12, 7, 0, 255]));
}

#[test]
fn clearing_a_soft_selection_erases_in_proportion() {
    let mut picture = marked(40, 30);
    let chosen = oval((2, 2), (21, 13));
    let put = super::cleared(&mut picture, &chosen, Ink::Clear).expect("room");
    assert!(!put.is_empty());
    assert_eq!(picture.sample(12, 7), Some(Sample::Rgba([0; 4])));
    assert_eq!(
        picture.sample(2, 2),
        Some(Sample::Rgba([2, 2, 0, 255])),
        "left out"
    );
    assert_eq!(picture.sample(30, 20), Some(Sample::Rgba([30, 20, 0, 255])));
}
