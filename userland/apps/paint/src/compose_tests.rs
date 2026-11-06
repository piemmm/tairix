use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_parallel::{Reversed, SERIAL};

use super::{between, compose_run, flatten, laid};
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::Layer;
use crate::op::Halted;

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
    let made = flatten(&[shown, hidden], &SERIAL).expect("flattens");
    assert_eq!(at(&made, 0, 0), [10, 20, 30, 255]);
    let only = layer([10, 20, 30, 255], 255, true);
    let shared = flatten(core::slice::from_ref(&only), &SERIAL).expect("flattens");
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
    let made = flatten(&layers, &SERIAL).expect("flattens");
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
    let made = flatten(&layers, &SERIAL).expect("flattens");
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
    let apart = flatten(&[ground, middle, top], &SERIAL).expect("flattens");
    let upper = [
        layer([200, 10, 10, 180], 140, true),
        layer([10, 10, 250, 90], 200, true),
    ];
    let pair = flatten(&upper, &SERIAL).expect("merges");
    let merged = [
        layer([20, 200, 40, 255], 255, true),
        Layer::new(pair, String::from("merged")),
    ];
    let together = flatten(&merged, &SERIAL).expect("flattens");
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

/// Layers laid over the composite of those beneath them show, to the bit,
/// what laying every layer from nothing does: what a layer's opacity changed
/// live is drawn over.
#[test]
fn layers_laid_over_their_composite_beneath_match_every_layer_laid() {
    let layers = [
        layer([10, 200, 30, 90], 255, true),
        layer([250, 5, 60, 255], 77, true),
        layer([0, 0, 0, 0], 255, true),
        layer([90, 90, 250, 200], 130, true),
        layer([40, 40, 40, 255], 200, false),
    ];
    let read = |canvas: &Canvas, into: &mut [[u8; 4]]| canvas.row_colours(0, 0, into);
    let mut whole = [[0u8; 4]; 3];
    let mut scratch = [[0u8; 4]; 3];
    compose_run(&layers, None, &mut whole, &mut scratch, read);
    for split in 1..layers.len() {
        let beneath = super::laid_together(&layers[..split], &SERIAL, &|| true).expect("room");
        let mut over = [[0u8; 4]; 3];
        beneath.row_colours(0, 0, &mut over);
        super::lay_run(&layers[split..], None, &mut over, &mut scratch, read);
        assert_eq!(over, whole, "laid over the {split} beneath");
    }
}

/// Laid together a tile at a time across several cores, in any order, the
/// layers come out to the bit as laid on one, and a tile nothing shows in is
/// left the one clear tile rather than a copy of it.
#[test]
fn layers_laid_together_across_cores_match_and_share_what_shows_nothing() {
    let ground = Layer::new(crate::effect::patterned(150, 100), String::from("ground"));
    let mut faint = Layer::new(crate::effect::patterned(150, 100), String::from("faint"));
    faint.opacity = 90;
    let layers = [ground, faint];
    let across = Reversed::new(4);
    let spread = super::laid_together(&layers, &across, &|| true).expect("room");
    assert!(across.widest() > 1, "a tile a piece");
    assert_eq!(
        spread,
        super::laid_together(&layers, &SERIAL, &|| true).expect("room")
    );
    let mut spot = crate::canvas::CanvasBuilder::new(150, 100, Kind::Rgba, Sample::Rgba([0; 4]))
        .expect("fits");
    spot.set(5, 5, Sample::Rgba([200, 10, 10, 180]));
    let clear = Canvas::new(150, 100, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let sparse = [
        Layer::new(clear, String::from("clear")),
        Layer::new(spot.finish(), String::from("spot")),
    ];
    let laid = super::laid_together(&sparse, &SERIAL, &|| true).expect("room");
    assert_eq!(
        laid.colour_at(5, 5).map(|[_, _, _, alpha]| alpha),
        Some(180)
    );
    assert!(
        Arc::ptr_eq(laid.tile(3), laid.tile(4)),
        "the clear tiles shared, not copied"
    );
}

/// Layers laid together stop between tiles once that is no longer wanted,
/// answering it given up.
#[test]
fn layers_laid_together_no_longer_wanted_are_given_up() {
    let ground = Layer::new(crate::effect::patterned(150, 100), String::from("ground"));
    let layers = [ground];
    let asked = AtomicUsize::new(0);
    let wanted = || asked.fetch_add(1, Ordering::Relaxed) < 2;
    assert_eq!(
        super::laid_together(&layers, &SERIAL, &wanted).err(),
        Some(Halted::Abandoned)
    );
}
