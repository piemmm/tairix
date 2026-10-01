use alloc::vec;

use tairix_image::IndexDepth;

use super::{fill, region};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::colour::Ink;
use crate::stroke::{Blend, Layer};

/// A 200×100 white canvas split by a black wall down column 100 with a gap
/// at the bottom row.
fn walled() -> Canvas {
    let mut built = CanvasBuilder::new(200, 100, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    for y in 0..99 {
        built.set(100, y, Sample::Rgba([0, 0, 0, 255]));
    }
    built.finish()
}

#[test]
fn a_flood_goes_round_a_wall_through_its_gap() {
    let canvas = walled();
    let found = region(&canvas, 5, 5, 0)
        .expect("room")
        .expect("on the canvas");
    assert!(found.contains(199, 0), "reached round the wall");
    assert!(
        !found.contains(100, 50),
        "the wall itself is not like white"
    );
    assert!(found.contains(100, 99), "the gap is");
    let bounds = found.bounds();
    assert_eq!(
        (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
        (0, 0, 200, 100)
    );
}

#[test]
fn a_closed_wall_stops_the_flood() {
    let mut built = CanvasBuilder::new(50, 50, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    for y in 0..50 {
        built.set(25, y, Sample::Rgba([0, 0, 0, 255]));
    }
    let canvas = built.finish();
    let found = region(&canvas, 0, 0, 0)
        .expect("room")
        .expect("on the canvas");
    assert!(found.contains(24, 49));
    assert!(!found.contains(26, 0));
    assert_eq!(found.bounds().x1, 25);
}

#[test]
fn tolerance_admits_colours_near_the_one_chosen() {
    let mut built = CanvasBuilder::new(3, 1, Kind::Rgba, Sample::Rgba([100; 4])).expect("fits");
    built.set(1, 0, Sample::Rgba([104, 100, 100, 100]));
    built.set(2, 0, Sample::Rgba([110, 100, 100, 100]));
    let canvas = built.finish();
    let strict = region(&canvas, 0, 0, 0).expect("room").expect("on");
    assert!(!strict.contains(1, 0));
    let loose = region(&canvas, 0, 0, 5).expect("room").expect("on");
    assert!(loose.contains(1, 0) && !loose.contains(2, 0));
}

#[test]
fn clear_pixels_are_alike_whatever_colour_they_hide() {
    let mut built = CanvasBuilder::new(2, 1, Kind::Rgba, Sample::Rgba([1, 2, 3, 0])).expect("fits");
    built.set(1, 0, Sample::Rgba([200, 9, 9, 0]));
    let canvas = built.finish();
    let found = region(&canvas, 0, 0, 0).expect("room").expect("on");
    assert!(found.contains(1, 0));
}

#[test]
fn a_palette_flood_matches_the_entry_and_treats_masked_pixels_as_one() {
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0; 4], [255; 4], [9; 4]],
        masked: true,
    };
    let mut built = CanvasBuilder::new(4, 1, kind, Sample::Index(1, 255)).expect("fits");
    built.set(1, 0, Sample::Index(2, 255));
    built.set(2, 0, Sample::Index(0, 0));
    built.set(3, 0, Sample::Index(1, 0));
    let canvas = built.finish();
    let solid = region(&canvas, 0, 0, 200).expect("room").expect("on");
    assert!(
        !solid.contains(1, 0),
        "tolerance is for colours, not entries"
    );
    let clear = region(&canvas, 2, 0, 0).expect("room").expect("on");
    assert!(clear.contains(3, 0), "masked pixels are alike");
    assert!(!clear.contains(1, 0));
}

#[test]
fn filling_writes_the_region_and_records_what_it_replaced() {
    let mut canvas = walled();
    let original = canvas.try_clone().expect("room");
    let found = region(&canvas, 150, 5, 0).expect("room").expect("on");
    let layer = Layer {
        ink: Ink::Colour([0, 200, 0, 255]),
        blend: Blend::Over,
    };
    let stroke = fill(&mut canvas, &found, layer).expect("room");
    assert_eq!(canvas.sample(0, 0), Some(Sample::Rgba([0, 200, 0, 255])));
    assert_eq!(canvas.sample(100, 0), Some(Sample::Rgba([0, 0, 0, 255])));
    for (index, tile) in stroke.finish() {
        canvas.replace_tile(index, tile);
    }
    assert_eq!(canvas, original);
}

#[test]
fn a_pixel_off_the_canvas_reaches_nothing() {
    let canvas = walled();
    assert!(region(&canvas, 200, 0, 0).expect("room").is_none());
}

/// The flood reaches exactly the pixels joined to the seed through pixels
/// like it, however tangled the picture: checked against a plain search.
#[test]
fn a_flood_reaches_exactly_the_joined_pixels() {
    let (width, height) = (70u32, 50u32);
    let white = Sample::Rgba([255; 4]);
    let mut built = CanvasBuilder::new(width, height, Kind::Rgba, white).expect("fits");
    let mut state = 0x1234_5678u32;
    for y in 0..height {
        for x in 0..width {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            if (state >> 16).is_multiple_of(3) && (x, y) != (0, 0) {
                built.set(x, y, Sample::Rgba([0, 0, 0, 255]));
            }
        }
    }
    let canvas = built.finish();
    let found = region(&canvas, 0, 0, 0)
        .expect("room")
        .expect("on the canvas");
    let at = |x: u32, y: u32| (y * width + x) as usize;
    let mut joined = vec![false; at(0, height)];
    joined[0] = true;
    let mut pending = vec![(0u32, 0u32)];
    while let Some((x, y)) = pending.pop() {
        let next = [
            x.checked_sub(1).map(|x| (x, y)),
            (x + 1 < width).then_some((x + 1, y)),
            y.checked_sub(1).map(|y| (x, y)),
            (y + 1 < height).then_some((x, y + 1)),
        ];
        for (nx, ny) in next.into_iter().flatten() {
            if !joined[at(nx, ny)] && canvas.sample(nx, ny) == Some(white) {
                joined[at(nx, ny)] = true;
                pending.push((nx, ny));
            }
        }
    }
    for y in 0..height {
        for x in 0..width {
            assert_eq!(found.contains(x, y), joined[at(x, y)], "({x}, {y})");
        }
    }
}
