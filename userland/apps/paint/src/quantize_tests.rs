use tairix_image::Rgba8;

use super::palette_for;
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};

fn gradient(width: u32, height: u32) -> Canvas {
    let mut built =
        CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..height {
        for x in 0..width {
            let r = u8::try_from(x * 255 / (width - 1)).expect("a byte");
            let g = u8::try_from(y * 255 / (height - 1)).expect("a byte");
            built.set(x, y, Sample::Rgba([r, g, 128, 255]));
        }
    }
    built.finish()
}

#[test]
fn a_picture_of_few_colours_keeps_them_exactly_in_the_order_met() {
    let mut built =
        CanvasBuilder::new(4, 2, Kind::Rgba, Sample::Rgba([9, 9, 9, 255])).expect("fits");
    built.set(1, 0, Sample::Rgba([1, 2, 3, 255]));
    built.set(3, 1, Sample::Rgba([200, 100, 50, 255]));
    built.set(2, 1, Sample::Rgba([7, 7, 7, 0]));
    let palette = palette_for(&built.finish(), 16).expect("room");
    let want: [Rgba8; 3] = [[9, 9, 9, 255], [1, 2, 3, 255], [200, 100, 50, 255]];
    assert_eq!(palette, want, "a masked pixel's colour is not among them");
}

#[test]
fn median_cut_divides_many_colours_into_as_many_as_asked() {
    let palette = palette_for(&gradient(64, 64), 16).expect("room");
    assert_eq!(palette.len(), 16);
    assert!(palette.iter().all(|entry| entry[3] == 255));
    // The gradient spans red and green: the palette spans them too.
    let reds = palette.iter().map(|entry| entry[0]);
    let (low, high) = (reds.clone().min().expect("some"), reds.max().expect("some"));
    assert!(low < 64 && high > 192, "{low}..{high}");
}

#[test]
fn nothing_shown_answers_one_black_entry() {
    let clear = Canvas::new(8, 8, Kind::Rgba, Sample::Rgba([5, 5, 5, 0])).expect("fits");
    assert_eq!(palette_for(&clear, 4).expect("room"), [[0, 0, 0, 255]]);
}

#[test]
fn two_colours_asked_of_many_gives_two() {
    let palette = palette_for(&gradient(32, 32), 2).expect("room");
    assert_eq!(palette.len(), 2);
    assert_ne!(palette[0], palette[1]);
}
