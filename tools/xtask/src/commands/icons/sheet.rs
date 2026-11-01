//! The sheet every file icon is drawn on: a page whose top right corner is
//! folded down in the file type's colour, and what stands at its foot.

use core::f64::consts::SQRT_2;

use super::canvas::{rgb, rounded, scaled, stroke, Canvas, Mask, Paint, Relief, Rgb};
use super::text::{Fonts, Typeface};

/// The page's left edge, in units.
pub const LEFT: f64 = 28.0;

/// The page's right edge.
pub const RIGHT: f64 = 228.0;

/// The page's top and bottom: the full height of the master, as the build
/// holds a master trimmed to its drawing.
const TOP: f64 = 0.5;
const BOTTOM: f64 = 255.5;

/// The page's corner radius.
const RADIUS: f64 = 18.0;

/// How far along each edge the corner is folded.
pub const FOLD: f64 = 62.0;

/// The width of the page's rim.
const RIM: f64 = 6.0;

/// The line emblems and feet are centred on.
pub const MIDDLE: f64 = f64::midpoint(LEFT, RIGHT);

/// The lowest an emblem reaches above a foot.
pub const FOOT_TOP: f64 = 176.0;

/// What stands at the foot of the page.
#[derive(Copy, Clone, Debug)]
pub enum Foot<'a> {
    /// Nothing: the emblem has the page.
    Bare,
    /// Two lines of text, for a document.
    Lines,
    /// The format's name on a plate in the sheet's colour.
    Badge(&'a str),
}

/// The page inset by `inset` units, its folded corner cut away.
fn page(inset: f64) -> Mask {
    let (left, top, right, bottom) = (LEFT + inset, TOP + inset, RIGHT - inset, BOTTOM - inset);
    let body = Mask::of(&[rounded(left, top, right, bottom, (RADIUS - inset).max(1.0))]);
    // The fold's crease, moved inward with the rest of the edge.
    let along = FOLD + inset * (SQRT_2 - 1.0);
    let cut = Mask::of(&[vec![
        (RIGHT - along, TOP - 4.0),
        (RIGHT + 4.0, TOP - 4.0),
        (RIGHT + 4.0, TOP + along),
    ]]);
    body.minus(&cut)
}

/// The page within its rim: where anything drawn on the page lies.
pub fn body() -> Mask {
    page(RIM)
}

/// The folded-down corner.
fn flap() -> Mask {
    let (left, bottom) = (RIGHT - FOLD, TOP + FOLD);
    let corner = Mask::of(&[vec![(left, TOP), (left, bottom), (RIGHT, bottom)]]);
    corner.within(&Mask::of(&[rounded(
        left,
        TOP - 40.0,
        RIGHT + 40.0,
        bottom,
        10.0,
    )]))
}

/// Draw the sheet with its corner folded in `colour` and `foot` at its foot.
pub fn draw(canvas: &mut Canvas, fonts: &Fonts, colour: Rgb, foot: Foot<'_>) -> Result<(), String> {
    let edge = Paint::vertical(rgb(0x3d_45_53), rgb(0x26_2c_36), 0.0, 256.0);
    canvas.paint(&page(0.0), edge, Relief::FLAT, 1.0);
    let rim = page(1.4);
    let metal = Paint::vertical(rgb(0xc3_ca_d5), rgb(0x7a_84_96), 0.0, 256.0);
    canvas.paint(
        &rim,
        metal,
        Relief {
            width: 4.0,
            highlight: 0.6,
            shade: 0.35,
        },
        1.0,
    );
    let body = body();
    canvas.paint(
        &page(RIM - 1.4).minus(&body),
        Paint::Solid(rgb(0x4a_53_63)),
        Relief::FLAT,
        0.9,
    );
    let face = Paint::vertical(rgb(0xfb_fc_fe), rgb(0xd3_d9_e2), 0.0, 256.0);
    canvas.paint(
        &body,
        face,
        Relief {
            width: 7.0,
            highlight: 0.7,
            shade: 0.18,
        },
        1.0,
    );
    fold(canvas, colour);
    match foot {
        Foot::Bare => {}
        Foot::Lines => lines(canvas),
        Foot::Badge(label) => badge(canvas, fonts, colour, label)?,
    }
    Ok(())
}

/// The folded corner, in `colour`, and the shadow it casts on the page.
fn fold(canvas: &mut Canvas, colour: Rgb) {
    let flap = flap();
    canvas.shadow(&flap, -3.0, 4.0, 4.0, 0.3);
    let shade = Paint::linear(
        (RIGHT - FOLD, 0.0),
        (RIGHT, FOLD),
        scaled(colour, 1.08),
        scaled(colour, 0.72),
    );
    canvas.paint(&flap, shade, Relief::of(6.0), 1.0);
    let crease = Mask::of(&stroke(&[(RIGHT - FOLD, TOP), (RIGHT, TOP + FOLD)], 2.0)).within(&flap);
    canvas.paint(
        &crease,
        Paint::Solid(scaled(colour, 1.35)),
        Relief::FLAT,
        0.8,
    );
}

/// Two lines of text at the foot of the page.
fn lines(canvas: &mut Canvas) {
    for (y, width) in [(200.0, 112.0), (220.0, 80.0)] {
        let bar = Mask::of(&[rounded(
            MIDDLE - width / 2.0,
            y - 4.5,
            MIDDLE + width / 2.0,
            y + 4.5,
            4.5,
        )]);
        canvas.paint(
            &bar,
            Paint::Solid(rgb(0x9b_a5_b4)),
            Relief {
                width: 3.0,
                highlight: 0.3,
                shade: 0.4,
            },
            1.0,
        );
    }
}

/// The format's name, white on a plate of the sheet's `colour`.
fn badge(canvas: &mut Canvas, fonts: &Fonts, colour: Rgb, label: &str) -> Result<(), String> {
    let (top, bottom) = (188.0, 228.0);
    let middle = f64::midpoint(top, bottom);
    let text = fonts.set(Typeface::Sans(800.0), label, 31.0, 1.5, (MIDDLE, middle))?;
    let half = (text.ink.2 - text.ink.0) / 2.0 + 16.0;
    let plate = Mask::of(&[rounded(MIDDLE - half, top, MIDDLE + half, bottom, 10.0)]);
    canvas.shadow(&plate, 1.5, 2.5, 2.5, 0.3);
    let fill = Paint::vertical(scaled(colour, 1.18), scaled(colour, 0.78), top, bottom);
    canvas.paint(&plate, fill, Relief::of(5.0), 1.0);
    canvas.shadow(&text.mask, 0.0, 1.5, 1.0, 0.35);
    let ink = Paint::vertical(rgb(0xff_ff_ff), rgb(0xe9_ed_f2), top, bottom);
    canvas.paint(
        &text.mask,
        ink,
        Relief {
            width: 2.0,
            highlight: 0.2,
            shade: 0.25,
        },
        1.0,
    );
    Ok(())
}
