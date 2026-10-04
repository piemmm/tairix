use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::{between, compose_run, flatten, laid};
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::Layer;

fn layer(colour: [u8; 4], opacity: u8, visible: bool) -> Layer {
    let canvas = Canvas::new(3, 2, Kind::Rgba, Sample::Rgba(colour)).expect("fits");
    let mut layer = Layer::new(canvas, String::from("layer"));
    layer.opacity = opacity;
    layer.visible = visible;
    layer
}

fn at(canvas: &Canvas, x: u32, y: u32) -> [u8; 4] {
    canvas.colour_at(x, y).expect("on the canvas")
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(&b).all(|(&p, &q)| p.abs_diff(q) <= 1)
}

#[test]
fn a_layer_shows_as_much_as_its_opacity_says() {
    let below = [0, 0, 255, 255];
    assert_eq!(laid(below, [255, 0, 0, 255], 0), below, "none of it");
    assert_eq!(
        laid(below, [255, 0, 0, 255], 255),
        [255, 0, 0, 255],
        "all of it"
    );
    assert!(close(
        laid(below, [255, 0, 0, 255], 128),
        [128, 0, 127, 255]
    ));
    assert!(
        close(laid([0; 4], [255, 0, 0, 255], 128), [255, 0, 0, 128]),
        "over nothing"
    );
}

#[test]
fn one_layer_showing_alone_and_wholly_is_its_own_pixels_shared() {
    let shown = layer([10, 20, 30, 255], 255, true);
    let hidden = layer([200, 0, 0, 255], 255, false);
    let made = flatten(&[shown, hidden]).expect("flattens");
    assert_eq!(at(&made, 0, 0), [10, 20, 30, 255]);
    let only = layer([10, 20, 30, 255], 255, true);
    let shared = flatten(core::slice::from_ref(&only)).expect("flattens");
    assert!(
        Arc::ptr_eq(shared.tile(0), only.canvas.tile(0)),
        "no pixel is copied"
    );
}

#[test]
fn layers_are_laid_bottom_first_each_at_its_opacity() {
    let layers = [
        layer([0, 0, 255, 255], 255, true),
        layer([255, 0, 0, 255], 128, true),
        layer([0, 255, 0, 255], 255, false),
    ];
    let made = flatten(&layers).expect("flattens");
    assert_eq!(made.kind(), &Kind::Rgba);
    assert!(
        close(at(&made, 2, 1), [128, 0, 127, 255]),
        "{:?}",
        at(&made, 2, 1)
    );
}

#[test]
fn nothing_showing_is_clear() {
    let layers = [
        layer([9, 9, 9, 255], 255, false),
        layer([9, 9, 9, 255], 0, true),
    ];
    let made = flatten(&layers).expect("flattens");
    assert_eq!(at(&made, 1, 1), [0; 4]);
}

#[test]
fn the_layer_painted_on_shows_as_given() {
    let layers = [
        layer([0, 0, 255, 255], 255, true),
        layer([0, 0, 0, 0], 255, true),
    ];
    let shown = [[255, 255, 255, 255]; 3];
    let (mut out, mut scratch) = (vec![[0u8; 4]; 3], vec![[0u8; 4]; 3]);
    compose_run(
        &layers,
        Some((1, &shown)),
        &mut out,
        &mut scratch,
        |canvas, into| {
            canvas.row_colours(0, 0, into);
        },
    );
    assert_eq!(
        out,
        vec![[255, 255, 255, 255]; 3],
        "a stroke under way shows through every layer"
    );
}

/// Two layers laid together onto nothing, then over the rest, show as the
/// two laid over the rest one by one: what keeps a merge's look.
#[test]
fn a_merge_keeps_the_look() {
    let ground = layer([20, 200, 40, 255], 255, true);
    let middle = layer([200, 10, 10, 180], 140, true);
    let top = layer([10, 10, 250, 90], 200, true);
    let apart = flatten(&[ground, middle, top]).expect("flattens");
    let pair = flatten(&[
        layer([200, 10, 10, 180], 140, true),
        layer([10, 10, 250, 90], 200, true),
    ])
    .expect("merges");
    let together = flatten(&[
        layer([20, 200, 40, 255], 255, true),
        Layer::new(pair, String::from("merged")),
    ])
    .expect("flattens");
    let (a, b): (Vec<_>, Vec<_>) = (0..3)
        .map(|x| (at(&apart, x, 0), at(&together, x, 0)))
        .unzip();
    assert!(a.iter().zip(&b).all(|(&p, &q)| close(p, q)), "{a:?} {b:?}");
}

#[test]
fn a_colour_fading_to_clear_keeps_its_hue() {
    let red = [200, 40, 10, 255];
    let half = between(red, [0; 4], 128);
    assert_eq!(&half[..3], &red[..3], "no darkening toward the clear end");
    assert_eq!(half[3], 127);
    assert_eq!(
        between([0, 255, 0, 0], red, 50)[..3],
        red[..3],
        "a clear colour lends no hue"
    );
    assert_eq!(between([0; 4], [9, 9, 9, 0], 99), [0; 4]);
    let (black, white) = ([0, 0, 0, 255], [255; 4]);
    assert_eq!(between(black, white, 0), black);
    assert_eq!(between(black, white, 255), white);
    assert_eq!(
        between(black, white, 51),
        [51, 51, 51, 255],
        "opaque colours mix plainly"
    );
}
