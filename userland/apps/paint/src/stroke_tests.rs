use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_image::IndexDepth;

use super::{lay_over, Blend, Change, Coat, Stroke};
use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::Ink;
use crate::shape::{Point, Shape, FX};

fn over(ink: Ink) -> Coat {
    Coat {
        ink,
        blend: Blend::Over,
    }
}

fn dab(x: i64, y: i64, radius: i64) -> Shape {
    let centre = Point::centre_of(x, y);
    Shape::Capsule {
        a: centre,
        b: centre,
        radius,
    }
}

#[test]
fn going_over_a_pixel_twice_in_one_stroke_does_not_darken_it() {
    let mut canvas = Canvas::new(40, 40, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let mut stroke = Stroke::new(over(Ink::Colour([0, 0, 0, 128])), None, None);
    stroke
        .cover(&mut canvas, 0, &dab(20, 20, 4 * FX), true)
        .expect("room");
    let once = canvas.sample(20, 20);
    stroke
        .cover(&mut canvas, 0, &dab(20, 20, 4 * FX), true)
        .expect("room");
    assert_eq!(canvas.sample(20, 20), once);
    assert_eq!(once, Some(Sample::Rgba([127, 127, 127, 255])));
}

#[test]
fn a_stroke_remembers_each_tile_as_it_was_and_can_be_taken_back() {
    let mut canvas = Canvas::new(130, 70, Kind::Rgba, Sample::Rgba([10; 4])).expect("fits");
    let original = canvas.try_clone().expect("room");
    let mut stroke = Stroke::new(over(Ink::Colour([200, 0, 0, 255])), None, None);
    let line = Shape::Capsule {
        a: Point::centre_of(2, 2),
        b: Point::centre_of(128, 66),
        radius: 2 * FX,
    };
    stroke.cover(&mut canvas, 0, &line, false).expect("room");
    assert_ne!(canvas, original);
    let damage = stroke.take_damage().expect("something changed");
    assert!(damage.x0 <= 2 && damage.x1 >= 128);
    let befores = stroke.finish();
    assert_eq!(
        befores
            .iter()
            .map(|(index, _)| *index)
            .collect::<alloc::vec::Vec<_>>(),
        [0, 1, 4, 5],
        "the tiles the line crosses, and no others"
    );
    for (index, tile) in befores {
        canvas.replace_tile(index, tile);
    }
    assert_eq!(canvas, original);
}

#[test]
fn a_reverted_stroke_leaves_the_canvas_as_it_found_it() {
    let mut canvas = Canvas::new(64, 64, Kind::Rgba, Sample::Rgba([10; 4])).expect("fits");
    let original = canvas.try_clone().expect("room");
    let mut stroke = Stroke::new(over(Ink::Clear), None, None);
    stroke
        .cover(&mut canvas, 0, &dab(5, 5, 3 * FX), true)
        .expect("room");
    assert_ne!(canvas, original);
    stroke.revert(&mut canvas);
    assert_eq!(canvas, original);
}

#[test]
fn the_tile_a_stroke_writes_is_copied_so_what_shared_it_is_untouched() {
    let mut canvas = Canvas::new(64, 64, Kind::Rgba, Sample::Rgba([10; 4])).expect("fits");
    let shared = Arc::clone(canvas.tile(0));
    let mut stroke = Stroke::new(over(Ink::Colour([0, 0, 0, 255])), None, None);
    stroke.cover_pixel(&mut canvas, 0, 3, 3).expect("room");
    assert_eq!(shared.samples()[..4], [10; 4]);
    assert_eq!(canvas.sample(3, 3), Some(Sample::Rgba([0, 0, 0, 255])));
}

#[test]
fn an_outline_lies_over_its_fill() {
    let mut canvas = Canvas::new(8, 8, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let fill = over(Ink::Colour([0, 0, 255, 255]));
    let outline = over(Ink::Colour([255, 0, 0, 255]));
    let mut stroke = Stroke::new(fill, Some(outline), None);
    // The outline first: the order the layers go down is the stroke's, not
    // the order they were covered in.
    stroke.cover_pixel(&mut canvas, 1, 2, 2).expect("room");
    stroke.cover_pixel(&mut canvas, 0, 2, 2).expect("room");
    stroke.cover_pixel(&mut canvas, 0, 3, 3).expect("room");
    assert_eq!(canvas.sample(2, 2), Some(Sample::Rgba([255, 0, 0, 255])));
    assert_eq!(canvas.sample(3, 3), Some(Sample::Rgba([0, 0, 255, 255])));
}

#[test]
fn a_palette_picture_takes_an_entry_where_the_ink_covers_most_of_a_pixel() {
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0; 4], [255; 4]],
        masked: true,
    };
    let mut canvas = Canvas::new(20, 20, kind, Sample::Index(0, 255)).expect("fits");
    let mut stroke = Stroke::new(over(Ink::Index(1)), None, None);
    stroke
        .cover(&mut canvas, 0, &dab(10, 10, 4 * FX), true)
        .expect("room");
    for y in 0..20 {
        for x in 0..20 {
            let sample = canvas.sample(x, y).expect("on the canvas");
            assert!(matches!(sample, Sample::Index(0 | 1, 255)), "{sample:?}");
        }
    }
    assert_eq!(canvas.sample(10, 10), Some(Sample::Index(1, 255)));
    let mut erase = Stroke::new(over(Ink::Clear), None, None);
    erase.cover_pixel(&mut canvas, 0, 10, 10).expect("room");
    assert_eq!(canvas.sample(10, 10), Some(Sample::Index(1, 0)));
}

#[test]
fn laying_over_follows_the_blend() {
    let white = Sample::Rgba([255, 255, 255, 255]);
    let red = Ink::Colour([255, 0, 0, 255]);
    let replace = Coat {
        ink: red,
        blend: Blend::Replace,
    };
    assert_eq!(lay_over(white, replace, 127, false), white);
    assert_eq!(
        lay_over(white, replace, 128, false),
        Sample::Rgba([255, 0, 0, 255])
    );
    assert_eq!(
        lay_over(
            Sample::Rgba([0; 4]),
            over(Ink::Colour([0, 0, 255, 102])),
            255,
            false
        ),
        Sample::Rgba([0, 0, 255, 102]),
        "over nothing, the ink as it is"
    );
    assert_eq!(
        lay_over(
            Sample::Rgba([10, 20, 30, 200]),
            over(Ink::Clear),
            255,
            false
        ),
        Sample::Rgba([0; 4])
    );
    assert_eq!(
        lay_over(
            Sample::Rgba([10, 20, 30, 200]),
            over(Ink::Clear),
            128,
            false
        ),
        Sample::Rgba([10, 20, 30, 100])
    );
    assert_eq!(
        lay_over(Sample::Index(3, 255), over(Ink::Clear), 255, false),
        Sample::Index(3, 255),
        "a picture with no mask cannot be cleared"
    );
}

#[test]
fn coverage_off_the_canvas_writes_nothing() {
    let mut canvas = Canvas::new(10, 10, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let mut stroke = Stroke::new(over(Ink::Colour([1; 4])), None, None);
    stroke
        .cover(&mut canvas, 0, &dab(-20, -20, 3 * FX), true)
        .expect("room");
    stroke.cover_pixel(&mut canvas, 0, 10, 3).expect("room");
    assert!(stroke.finish().is_empty());
}

/// A change keeps each tile's first state once, however often and in
/// whatever order the tile is written, and hands them back in tile order.
#[test]
fn a_change_keeps_each_tiles_first_state_once() {
    let mut canvas = Canvas::new(200, 70, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let original: Vec<_> = (0..canvas.tile_count())
        .map(|index| Arc::clone(canvas.tile(index)))
        .collect();
    let mut change = Change::new();
    for index in [5, 1, 5, 3, 1] {
        change.touch(&mut canvas, index).expect("room");
    }
    let kept = change.finish();
    let indices: Vec<usize> = kept.iter().map(|&(index, _)| index).collect();
    assert_eq!(indices, [1, 3, 5]);
    for (index, tile) in &kept {
        assert!(Arc::ptr_eq(tile, &original[*index]));
    }
}

/// A selection held clips a stroke: nothing lands outside it, and a soft
/// edge takes paint in proportion.
#[test]
fn a_selection_clips_a_stroke_and_its_soft_edge_takes_part() {
    use crate::mask::Mask;
    use crate::shape::Bounds;
    let picture = Bounds {
        x0: 0,
        y0: 0,
        x1: 40,
        y1: 40,
    };
    let held = Bounds {
        x0: 10,
        y0: 10,
        x1: 30,
        y1: 30,
    };
    let hard = Mask::rect(held).expect("pixels");
    let mut canvas = Canvas::new(40, 40, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let mut stroke = Stroke::new(over(Ink::Colour([9, 9, 9, 255])), None, Some(hard.clone()));
    stroke
        .cover(&mut canvas, 0, &dab(20, 20, 30 * FX), false)
        .expect("room");
    assert_eq!(canvas.sample(10, 10), Some(Sample::Rgba([9, 9, 9, 255])));
    assert_eq!(canvas.sample(9, 10), Some(Sample::Rgba([0; 4])), "outside");
    assert_eq!(canvas.sample(30, 29), Some(Sample::Rgba([0; 4])), "outside");
    let damage = stroke.take_damage().expect("drawn");
    assert_eq!(damage, held, "held to the selection");
    let soft = hard.feathered(4, picture).expect("room").expect("pixels");
    let mut canvas = Canvas::new(40, 40, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let mut stroke = Stroke::new(over(Ink::Colour([9, 9, 9, 255])), None, Some(soft.clone()));
    stroke.cover_pixel(&mut canvas, 0, 20, 20).expect("room");
    stroke.cover_pixel(&mut canvas, 0, 10, 20).expect("room");
    assert_eq!(canvas.sample(20, 20), Some(Sample::Rgba([9, 9, 9, 255])));
    let Some(Sample::Rgba(edge)) = canvas.sample(10, 20) else {
        panic!("a colour picture");
    };
    assert_eq!(edge[3], soft.at(10, 20), "as much as the edge chooses");
}

/// A clone stroke lays the picture as it stood when it began, from its
/// offset: what it has already laid is never copied again.
#[test]
fn a_clone_stroke_copies_from_its_offset_as_the_picture_stood() {
    use crate::canvas::CanvasBuilder;
    let mut built = CanvasBuilder::new(30, 4, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for x in 0..30 {
        for y in 0..4 {
            let shade = u8::try_from(x * 8).expect("small");
            built.set(x, y, Sample::Rgba([shade, 0, 0, 255]));
        }
    }
    let mut canvas = built.finish();
    let mut stroke = Stroke::cloning((1, 0), Blend::Over, 255, None);
    for x in (5..15).rev() {
        stroke.cover_pixel(&mut canvas, 0, x, 1).expect("room");
    }
    for x in 5..15 {
        let want = u8::try_from((x + 1) * 8).expect("small");
        assert_eq!(
            canvas.sample(u32::try_from(x).expect("on"), 1),
            Some(Sample::Rgba([want, 0, 0, 255])),
            "pixel {x} took its neighbour as it was"
        );
    }
    let mut stroke = Stroke::cloning((100, 0), Blend::Over, 255, None);
    let before = canvas.sample(3, 2);
    stroke.cover_pixel(&mut canvas, 0, 3, 2).expect("room");
    assert_eq!(
        canvas.sample(3, 2),
        before,
        "a source off the picture lays nothing"
    );
}
