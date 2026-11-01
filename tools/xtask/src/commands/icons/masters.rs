//! The file-type icon masters, each a sheet and what is drawn on it.
//!
//! Every master shares the sheet, the light from the upper left, the one
//! emblem shadow and the one bevel scale, so the set reads as one family; a
//! type is told apart by its emblem first and the colour of its fold second.

use core::f64::consts::SQRT_2;
use core::ops::RangeInclusive;
use std::collections::BTreeMap;

use tairix_icon::IconKind;

use super::canvas::{
    circle, cubic, rect, rgb, rounded, scaled, stroke, Canvas, Contour, Mask, Paint, Relief, Rgb,
};
use super::sheet::{self, Foot, FOOT_TOP, MIDDLE};
use super::text::{Fonts, Typeface};

/// How a master is drawn.
pub type Draw = fn(&Fonts) -> Result<Canvas, String>;

/// Every master this generator draws, by the icon it is.
pub const MASTERS: &[(IconKind, Draw)] = &[
    (IconKind::File, file),
    (IconKind::Text, text),
    (IconKind::TextHtml, html),
    (IconKind::TextJava, java),
    (IconKind::TextRust, rust),
    (IconKind::Pdf, pdf),
    (IconKind::ImagePng, png),
    (IconKind::ImageJpeg, jpeg),
    (IconKind::ImageGif, gif),
    (IconKind::ImageSvg, svg),
    (IconKind::ImageSprite, sprite),
    (IconKind::BasicProgram, basic),
    (IconKind::RiscOsProgram, absolute),
    (IconKind::RiscOsUtility, utility),
    (IconKind::RiscOsModule, module),
    (IconKind::RiscOsObey, obey),
    (IconKind::Drawing, drawing),
    (IconKind::WindowTemplate, template),
    (IconKind::Font, font),
    (IconKind::PostScript, postscript),
    (IconKind::PrintData, printout),
    (IconKind::Palette, palette),
];

/// A canvas holding the sheet, folded in `colour`, with `foot`.
fn on_sheet(fonts: &Fonts, colour: u32, foot: Foot<'_>) -> Result<Canvas, String> {
    let mut canvas = Canvas::new();
    sheet::draw(&mut canvas, fonts, rgb(colour), foot)?;
    Ok(canvas)
}

/// `shape` raised off the page: the emblem shadow, then the surface.
fn raise(canvas: &mut Canvas, shape: &Mask, paint: Paint, relief: Relief) {
    canvas.shadow(shape, 2.0, 3.5, 3.0, 0.35);
    canvas.paint(shape, paint, relief, 1.0);
}

/// A rectangle, its corners rounded to `radius`.
fn block(x0: f64, y0: f64, x1: f64, y1: f64, radius: f64) -> Mask {
    Mask::of(&[rounded(x0, y0, x1, y1, radius)])
}

/// A disc.
fn disc(cx: f64, cy: f64, radius: f64) -> Mask {
    Mask::of(&[circle(cx, cy, radius)])
}

/// A polygon.
fn polygon(points: &[(f64, f64)]) -> Mask {
    Mask::of(&[points.to_vec()])
}

/// `colour` from light at `y0` to dark at `y1`.
fn lit(colour: Rgb, y0: f64, y1: f64) -> Paint {
    Paint::vertical(scaled(colour, 1.25), scaled(colour, 0.72), y0, y1)
}

/// A line of text on the page: a bar `width` long from `left`, centred on `y`.
fn text_line(canvas: &mut Canvas, left: f64, y: f64, width: f64, colour: Rgb) {
    let bar = block(left, y - 3.5, left + width, y + 3.5, 3.5);
    canvas.paint(
        &bar,
        Paint::Solid(colour),
        Relief {
            width: 2.5,
            highlight: 0.3,
            shade: 0.45,
        },
        1.0,
    );
}

/// The grey of text on the page.
const INK: u32 = 0x8c_97_a8;

fn file(fonts: &Fonts) -> Result<Canvas, String> {
    on_sheet(fonts, 0x8a_94_a6, Foot::Bare)
}

fn text(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x3b_7d_dd;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let left = MIDDLE - 75.0;
    text_line(&mut canvas, left, 50.0, 92.0, rgb(colour));
    for (y, width) in [
        (74.0, 150.0),
        (91.0, 138.0),
        (108.0, 150.0),
        (125.0, 112.0),
        (149.0, 150.0),
        (166.0, 144.0),
        (183.0, 150.0),
        (200.0, 86.0),
    ] {
        text_line(&mut canvas, left, y, width, rgb(INK));
    }
    Ok(canvas)
}

/// Source text: `marks` set large in the fold's colour above a plate naming
/// the language.
fn source(fonts: &Fonts, colour: u32, marks: &str, label: &str) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, colour, Foot::Badge(label))?;
    let set = fonts.set(Typeface::Sans(800.0), marks, 92.0, 6.0, (MIDDLE, 118.0))?;
    let paint = lit(rgb(colour), set.ink.1, set.ink.3);
    raise(&mut canvas, &set.mask, paint, Relief::of(5.0));
    Ok(canvas)
}

fn html(fonts: &Fonts) -> Result<Canvas, String> {
    source(fonts, 0xf7_67_07, "</>", "HTML")
}

fn java(fonts: &Fonts) -> Result<Canvas, String> {
    source(fonts, 0xd6_33_6c, "{ }", "JAVA")
}

fn rust(fonts: &Fonts) -> Result<Canvas, String> {
    source(fonts, 0xa8_45_13, "{;}", "RUST")
}

fn pdf(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0xe0_31_31;
    let mut canvas = on_sheet(fonts, colour, Foot::Badge("PDF"))?;
    // A laid-out page: a picture beside four lines, three lines beneath.
    let (x0, y0, x1, y1) = (50.0, 70.0, 118.0, 122.0);
    let picture = block(x0, y0, x1, y1, 5.0);
    raise(
        &mut canvas,
        &picture,
        Paint::vertical(rgb(0xff_e3_e3), rgb(0xff_a8_a8), y0, y1),
        Relief::of(3.0),
    );
    let hills = polygon(&[
        (x0, y1),
        (x0, 108.0),
        (70.0, 92.0),
        (86.0, 104.0),
        (98.0, 96.0),
        (x1, 110.0),
        (x1, y1),
    ])
    .within(&picture);
    canvas.paint(&hills, lit(rgb(colour), 92.0, y1), Relief::FLAT, 1.0);
    canvas.paint(
        &disc(100.0, 83.0, 7.0),
        Paint::Solid(rgb(0xff_f5_f5)),
        Relief::FLAT,
        1.0,
    );
    for (left, y, width) in [
        (128.0, 74.0, 74.0),
        (128.0, 88.0, 66.0),
        (128.0, 102.0, 74.0),
        (128.0, 116.0, 48.0),
        (50.0, 138.0, 152.0),
        (50.0, 152.0, 144.0),
        (50.0, 166.0, 98.0),
    ] {
        text_line(&mut canvas, left, y, width, rgb(INK));
    }
    Ok(canvas)
}

/// The light, land and sun of a photograph.
struct Scene {
    sky: (u32, u32),
    sun: (u32, f64, f64, f64),
    far: (u32, u32),
    near: (u32, u32),
    water: Option<(u32, u32)>,
}

/// A photograph of `scene` in a white mount from `(x0, y0)` to `(x1, y1)`.
fn photograph(canvas: &mut Canvas, (x0, y0, x1, y1): (f64, f64, f64, f64), scene: &Scene) {
    let mount = block(x0, y0, x1, y1, 6.0);
    raise(
        canvas,
        &mount,
        Paint::vertical(rgb(0xff_ff_ff), rgb(0xe4_e8_ee), y0, y1),
        Relief::of(3.0),
    );
    let (ix0, iy0, ix1, iy1) = (x0 + 6.0, y0 + 6.0, x1 - 6.0, y1 - 6.0);
    let image = block(ix0, iy0, ix1, iy1, 2.5);
    let at = |u: f64, v: f64| (ix0 + (ix1 - ix0) * u, iy0 + (iy1 - iy0) * v);
    canvas.paint(
        &image,
        Paint::vertical(rgb(scene.sky.0), rgb(scene.sky.1), iy0, iy1),
        Relief::FLAT,
        1.0,
    );
    let (sun, su, sv, sr) = scene.sun;
    let (sx, sy) = at(su, sv);
    canvas.paint(
        &disc(sx, sy, sr * 1.9).within(&image),
        Paint::Solid(rgb(sun)),
        Relief::FLAT,
        0.25,
    );
    canvas.paint(
        &disc(sx, sy, sr).within(&image),
        Paint::Solid(rgb(sun)),
        Relief::FLAT,
        1.0,
    );
    let ridge = |points: &[(f64, f64)]| {
        let mut outline: Vec<(f64, f64)> = points.iter().map(|&(u, v)| at(u, v)).collect();
        outline.push(at(1.0, 1.0));
        outline.push(at(0.0, 1.0));
        polygon(&outline).within(&image)
    };
    let far = ridge(&[
        (0.0, 0.62),
        (0.18, 0.36),
        (0.3, 0.5),
        (0.46, 0.28),
        (0.64, 0.54),
        (0.8, 0.42),
        (1.0, 0.6),
    ]);
    canvas.paint(
        &far,
        Paint::vertical(rgb(scene.far.0), rgb(scene.far.1), at(0.0, 0.28).1, iy1),
        Relief::FLAT,
        1.0,
    );
    let cap: Vec<(f64, f64)> = [
        (0.46, 0.28),
        (0.514, 0.358),
        (0.49, 0.335),
        (0.46, 0.36),
        (0.44, 0.33),
        (0.412, 0.346),
    ]
    .iter()
    .map(|&(u, v)| at(u, v))
    .collect();
    let snow = polygon(&cap).within(&far);
    canvas.paint(&snow, Paint::Solid(rgb(0xf8_f9_fa)), Relief::FLAT, 0.9);
    let near = ridge(&[
        (0.0, 0.78),
        (0.22, 0.66),
        (0.5, 0.76),
        (0.78, 0.64),
        (1.0, 0.72),
    ]);
    canvas.paint(
        &near,
        Paint::vertical(rgb(scene.near.0), rgb(scene.near.1), at(0.0, 0.64).1, iy1),
        Relief::FLAT,
        1.0,
    );
    if let Some((top, bottom)) = scene.water {
        let lake = ridge(&[(0.0, 0.86), (1.0, 0.86)]);
        canvas.paint(
            &lake,
            Paint::vertical(rgb(top), rgb(bottom), at(0.0, 0.86).1, iy1),
            Relief::FLAT,
            1.0,
        );
    }
}

/// The mount a photograph sits in on a sheet with a badge: below the fold
/// and above the plate.
const MOUNT: (f64, f64, f64, f64) = (46.0, 70.0, 210.0, 174.0);

const DAY: Scene = Scene {
    sky: (0x74_c0_fc, 0xe7_f5_ff),
    sun: (0xff_d4_3b, 0.78, 0.24, 9.0),
    far: (0x5c_7c_fa, 0x36_4f_c7),
    near: (0x51_cf_66, 0x2b_8a_3e),
    water: None,
};

fn png(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x2f_9e_44, Foot::Badge("PNG"))?;
    photograph(&mut canvas, MOUNT, &DAY);
    Ok(canvas)
}

fn jpeg(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x1c_7e_d6, Foot::Badge("JPG"))?;
    let dusk = Scene {
        sky: (0xf7_67_07, 0xff_d8_a8),
        sun: (0xff_f3_bf, 0.3, 0.46, 11.0),
        far: (0x70_48_e8, 0x5f_3d_c4),
        near: (0x5f_3d_c4, 0x3b_1f_9e),
        water: Some((0xff_a9_4d, 0x5f_3d_c4)),
    };
    photograph(&mut canvas, MOUNT, &dusk);
    Ok(canvas)
}

fn gif(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0xae_3e_c9, Foot::Badge("GIF"))?;
    // Frames of a sequence, the latest in front.
    let (x0, y0, x1, y1) = (60.0, 86.0, 210.0, 176.0);
    for step in [2.0, 1.0] {
        let back = (
            x0 - 9.0 * step,
            y0 - 9.0 * step,
            x1 - 9.0 * step,
            y1 - 9.0 * step,
        );
        let mount = block(back.0, back.1, back.2, back.3, 6.0);
        raise(
            &mut canvas,
            &mount,
            Paint::vertical(rgb(0xf8_f0_fc), rgb(0xd0_bf_ff), back.1, back.3),
            Relief::of(3.0),
        );
    }
    photograph(&mut canvas, (x0, y0, x1, y1), &DAY);
    Ok(canvas)
}

fn svg(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0xf5_9f_00;
    let mut canvas = on_sheet(fonts, colour, Foot::Badge("SVG"))?;
    let (p0, p1, p2, p3) = ((52.0, 156.0), (78.0, 70.0), (166.0, 172.0), (204.0, 92.0));
    vector_handles(&mut canvas, [p0, p1, p2, p3]);
    let path = Mask::of(&stroke(&cubic(p0, p1, p2, p3), 11.0));
    raise(
        &mut canvas,
        &path,
        lit(rgb(colour), 70.0, 172.0),
        Relief::of(4.0),
    );
    vector_anchors(&mut canvas, [p0, p1, p2, p3]);
    Ok(canvas)
}

/// The handles of the cubic through `points`: a line from each end to its
/// control point.
fn vector_handles(canvas: &mut Canvas, [p0, p1, p2, p3]: [(f64, f64); 4]) {
    let lines = Mask::of(&stroke(&[p0, p1], 3.0)).union(&Mask::of(&stroke(&[p3, p2], 3.0)));
    canvas.paint(&lines, Paint::Solid(rgb(0x3b_43_52)), Relief::FLAT, 0.9);
}

/// The anchors of the cubic through `points`: squares at its ends, knobs at
/// its control points.
fn vector_anchors(canvas: &mut Canvas, [p0, p1, p2, p3]: [(f64, f64); 4]) {
    for (x, y) in [p1, p2] {
        let knob = disc(x, y, 7.5);
        canvas.paint(
            &knob.outline(1.6),
            Paint::Solid(rgb(0x3b_43_52)),
            Relief::FLAT,
            1.0,
        );
        canvas.paint(
            &knob,
            Paint::Solid(rgb(0xff_ff_ff)),
            Relief {
                width: 3.0,
                highlight: 0.2,
                shade: 0.3,
            },
            1.0,
        );
    }
    for (x, y) in [p0, p3] {
        let handle = block(x - 9.0, y - 9.0, x + 9.0, y + 9.0, 2.0);
        canvas.shadow(&handle, 1.0, 1.5, 1.5, 0.3);
        canvas.paint(
            &handle.outline(1.6),
            Paint::Solid(rgb(0x3b_43_52)),
            Relief::FLAT,
            1.0,
        );
        canvas.paint(
            &handle,
            Paint::vertical(rgb(0x5c_9d_ff), rgb(0x1c_4f_c4), y - 9.0, y + 9.0),
            Relief::of(3.0),
            1.0,
        );
    }
}

/// The desktop's sixteen colours, in their order.
const WIMP: [u32; 16] = [
    0xff_ff_ff, 0xdd_dd_dd, 0xbb_bb_bb, 0x99_99_99, 0x77_77_77, 0x55_55_55, 0x33_33_33, 0x00_00_00,
    0x00_44_99, 0xee_ee_00, 0x00_cc_00, 0xdd_00_00, 0xee_ee_bb, 0x55_88_00, 0xff_bb_00, 0x00_bb_ff,
];

/// Picture elements across a sprite's picture, and down it.
const CELLS: usize = 32;

/// A sprite's picture: each element's colour.
struct Picture([[u32; CELLS]; CELLS]);

impl Picture {
    /// Colour the elements in `columns` and `rows`.
    fn fill(&mut self, columns: RangeInclusive<usize>, rows: RangeInclusive<usize>, colour: u32) {
        for line in &mut self.0[rows] {
            for cell in &mut line[columns.clone()] {
                *cell = colour;
            }
        }
    }

    /// Colour each element whose centre lies within `radius` of `centre` with
    /// what `colour` answers for its offset from there.
    fn disc(&mut self, centre: (f64, f64), radius: f64, colour: impl Fn(f64, f64) -> u32) {
        for (row, line) in (0u32..).zip(self.0.iter_mut()) {
            for (column, cell) in (0u32..).zip(line.iter_mut()) {
                let dx = f64::from(column) + 0.5 - centre.0;
                let dy = f64::from(row) + 0.5 - centre.1;
                if dx * dx + dy * dy <= radius * radius {
                    *cell = colour(dx, dy);
                }
            }
        }
    }

    /// Each element with where it lies.
    fn cells(&self) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        (0u32..).zip(&self.0).flat_map(|(row, line)| {
            (0u32..)
                .zip(line)
                .map(move |(column, &colour)| (column, row, colour))
        })
    }
}

/// Where the far hills top out, column by column.
const HILLS: [usize; CELLS] = [
    18, 17, 17, 16, 16, 16, 17, 17, 18, 18, 19, 19, 19, 18, 18, 17, 17, 16, 16, 15, 15, 15, 16, 16,
    17, 17, 18, 18, 19, 19, 18, 18,
];

/// The picture a sprite file is shown by: a cottage by a tree under the sun,
/// in the desktop's colours where it has them.
fn cottage() -> Picture {
    let mut picture = Picture([[0xcf_ea_ff; CELLS]; CELLS]);
    picture.fill(0..=31, 0..=5, 0xa9_d6_fb);
    picture.disc((6.5, 5.5), 3.3, |dx, dy| {
        if dx * dx + dy * dy <= 5.0 {
            0xff_e0_66
        } else {
            WIMP[14]
        }
    });
    for (centre, radius) in [((21.0, 6.5), 2.2), ((24.5, 5.5), 2.9), ((28.0, 6.5), 2.2)] {
        picture.disc(centre, radius, |_, _| WIMP[0]);
    }
    picture.fill(19..=29, 7..=7, WIMP[0]);
    picture.fill(19..=29, 8..=8, 0xd7_e3_ef);
    for (column, &top) in HILLS.iter().enumerate() {
        picture.fill(column..=column, top..=top, 0xb2_e6_a8);
        picture.fill(column..=column, top + 1..=22, 0x8f_d6_94);
    }
    for row in 23..CELLS {
        for column in 0..CELLS {
            let speckled = (column * 7 + row * 3) % 11 == 0;
            let colour = if speckled || row >= 29 {
                0x2f_9e_44
            } else {
                0x40_c0_57
            };
            picture.fill(column..=column, row..=row, colour);
        }
    }
    picture.fill(15..=18, 7..=7, 0x5c_32_18);
    picture.fill(16..=17, 8..=12, 0x8d_4a_2b);
    for (step, row) in (9..=15).enumerate() {
        picture.fill(12 - step..=12, row..=row, WIMP[11]);
        picture.fill(13..=13 + step, row..=row, 0xb8_14_14);
    }
    picture.fill(5..=20, 15..=15, 0x8f_14_14);
    picture.fill(6..=19, 16..=22, WIMP[12]);
    picture.fill(6..=19, 16..=16, 0xd6_d6_a0);
    picture.fill(19..=19, 16..=22, 0xcd_cd_95);
    for left in [7, 15] {
        picture.fill(left..=left + 3, 17..=20, WIMP[0]);
        picture.fill(left + 1..=left + 2, 18..=19, WIMP[15]);
        picture.fill(left + 1..=left + 1, 18..=18, 0xe7_f5_ff);
        picture.fill(left..=left + 3, 21..=21, 0xad_b5_bd);
    }
    picture.fill(11..=13, 18..=22, 0x8d_4a_2b);
    picture.fill(11..=11, 18..=22, 0xa9_64_3a);
    picture.fill(13..=13, 20..=20, 0xff_d4_3b);
    picture.fill(11..=13, 23..=23, 0xce_d4_da);
    for (step, row) in (24..CELLS).enumerate() {
        let half = step / 2;
        picture.fill(11 - half..=13 + half, row..=row, 0xd9_b0_70);
        picture.fill(13 + half..=13 + half, row..=row, 0xb9_8f_4f);
    }
    picture.fill(24..=25, 18..=23, 0x7a_4b_2a);
    picture.fill(25..=25, 18..=23, 0x5c_36_18);
    picture.disc((25.0, 13.5), 4.7, |dx, dy| {
        if dx + dy > 2.5 {
            0x23_7a_35
        } else if dx + dy < -3.0 {
            0x69_db_7c
        } else {
            0x37_b2_4d
        }
    });
    for (column, row) in [(3, 25), (5, 28), (20, 25), (29, 27), (27, 30)] {
        picture.fill(column..=column, row..=row, 0xf7_83_ac);
    }
    for (column, row) in [(2, 29), (22, 29), (18, 27), (30, 24)] {
        picture.fill(column..=column, row..=row, 0xff_d4_3b);
    }
    picture
}

/// The paintbrush laid across the picture, in picture elements: wet tip,
/// bristles, ferrule and handle along a diagonal from `TIP`, outlined, each
/// element with its colour. It runs past the picture's corner.
fn paintbrush() -> Vec<(u32, u32, u32)> {
    const TIP: (f64, f64) = (20.5, 24.5);
    // Half-width across the brush, and its three colours (lit, body,
    // shaded), at distance `t` along it from the tip.
    let part = |t: f64| -> Option<(f64, [u32; 3])> {
        Some(match t {
            t if t < -0.3 => return None,
            t if t <= 2.0 => (0.7 + 0.45 * t, [0xf7_83_ac, 0xe6_49_80, 0xa6_1e_4d]),
            t if t <= 5.2 => (1.6, [0x8c_66_50, 0x6b_4a_36, 0x4a_30_20]),
            t if t <= 7.6 => (2.0, [0xf1_f3_f5, 0xc3_ca_d5, 0x86_8e_96]),
            t if t <= 8.2 => (2.0, [0xc3_ca_d5, 0x86_8e_96, 0x5c_63_6e]),
            t if t <= 19.3 => (1.3, [0xe8_b0_70, 0xd0_8c_45, 0x9a_5d_24]),
            t if t <= 20.0 => (1.0, [0xe8_b0_70, 0xd0_8c_45, 0x9a_5d_24]),
            _ => return None,
        })
    };
    let mut cells = Vec::new();
    for row in 0u32..44 {
        for column in 0u32..44 {
            let (x, y) = (
                f64::from(column) + 0.5 - TIP.0,
                f64::from(row) + 0.5 - TIP.1,
            );
            let (along, across) = ((x + y) / SQRT_2, (x - y) / SQRT_2);
            let inside = part(along).filter(|&(half, _)| across.abs() <= half);
            // The side up and to the right turns toward the light.
            let colour = if let Some((half, [lit, body, shaded])) = inside {
                if across > half * 0.4 {
                    lit
                } else if across < -half * 0.4 {
                    shaded
                } else {
                    body
                }
            } else {
                let reach = part(along.clamp(-0.3, 20.0)).map_or(0.0, |(half, _)| half);
                if (-1.3..=21.0).contains(&along) && across.abs() <= reach + 1.0 {
                    0x21_25_29
                } else {
                    continue;
                }
            };
            cells.push((column, row, colour));
        }
    }
    cells
}

/// Paint each `(column, row, colour)` picture element as a square `cell`
/// units across from `origin`, `inset` short of its neighbours, one pass per
/// colour.
fn paint_cells(
    canvas: &mut Canvas,
    origin: (f64, f64),
    cell: f64,
    inset: f64,
    cells: impl IntoIterator<Item = (u32, u32, u32)>,
) {
    let mut by_colour: BTreeMap<u32, Vec<Contour>> = BTreeMap::new();
    for (column, row, colour) in cells {
        let (x, y) = (
            origin.0 + f64::from(column) * cell,
            origin.1 + f64::from(row) * cell,
        );
        by_colour.entry(colour).or_default().push(rect(
            x + inset,
            y + inset,
            x + cell - inset,
            y + cell - inset,
        ));
    }
    for (colour, squares) in by_colour {
        canvas.paint(
            &Mask::of(&squares),
            Paint::Solid(rgb(colour)),
            Relief::FLAT,
            1.0,
        );
    }
}

fn sprite(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0xe6_49_80, Foot::Bare)?;
    let cell = 4.0;
    let origin = (MIDDLE - 64.0, 66.0);
    let side = cell * 32.0;
    let frame = block(
        origin.0 - 1.0,
        origin.1 - 1.0,
        origin.0 + side + 1.0,
        origin.1 + side + 1.0,
        2.0,
    );
    canvas.shadow(&frame, 2.0, 3.5, 3.0, 0.35);
    canvas.paint(
        &frame.outline(1.6),
        Paint::Solid(rgb(0x3b_43_52)),
        Relief::FLAT,
        1.0,
    );
    canvas.paint(&frame, Paint::Solid(rgb(0x7f_a8_cc)), Relief::FLAT, 1.0);
    // A hairline left between elements reads as the editor's grid.
    paint_cells(&mut canvas, origin, cell, 0.25, cottage().cells());
    let brush = paintbrush();
    let silhouette: Vec<Contour> = brush
        .iter()
        .map(|&(column, row, _)| {
            let (x, y) = (
                origin.0 + f64::from(column) * cell,
                origin.1 + f64::from(row) * cell,
            );
            rect(x, y, x + cell, y + cell)
        })
        .collect();
    canvas.shadow(&Mask::of(&silhouette), 2.0, 3.5, 3.0, 0.35);
    paint_cells(&mut canvas, origin, cell, 0.0, brush);
    Ok(canvas)
}

fn basic(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0xfa_b0_05, Foot::Lines)?;
    let (top, bottom) = (72.0, 136.0);
    let banner = block(MIDDLE - 82.0, top, MIDDLE + 82.0, bottom, 14.0);
    raise(
        &mut canvas,
        &banner,
        Paint::vertical(rgb(0x2a_6f_d6), rgb(0x0b_3a_85), top, bottom),
        Relief::of(7.0),
    );
    let word = fonts.set(Typeface::Sans(800.0), "BASIC", 46.0, 1.0, (MIDDLE, 104.0))?;
    canvas.shadow(&word.mask, 0.0, 2.0, 1.5, 0.45);
    canvas.paint(
        &word.mask,
        Paint::vertical(rgb(0xff_f2_7a), rgb(0xf2_b7_05), word.ink.1, word.ink.3),
        Relief {
            width: 3.0,
            highlight: 0.5,
            shade: 0.35,
        },
        1.0,
    );
    Ok(canvas)
}

/// A desktop window from `(x0, y0)` to `(x1, y1)`: its frame, a title bar
/// `bar` units tall in `title` with its furniture, and a work area in `work`.
fn window(
    canvas: &mut Canvas,
    (x0, y0, x1, y1): (f64, f64, f64, f64),
    bar: f64,
    title: (u32, u32),
    work: (u32, u32),
) {
    let frame = block(x0, y0, x1, y1, 7.0);
    canvas.shadow(&frame, 2.0, 4.0, 4.0, 0.35);
    canvas.paint(&frame, Paint::Solid(rgb(0x2f_36_42)), Relief::FLAT, 1.0);
    let pane = block(x0 + 3.0, y0 + 3.0, x1 - 3.0, y1 - 3.0, 5.0);
    canvas.paint(
        &pane,
        Paint::vertical(rgb(work.0), rgb(work.1), y0, y1),
        Relief::FLAT,
        1.0,
    );
    let heading = block(x0 + 3.0, y0 + 3.0, x1 - 3.0, y0 + 3.0 + bar, 5.0)
        .union(&block(x0 + 3.0, y0 + 12.0, x1 - 3.0, y0 + 3.0 + bar, 0.0))
        .within(&pane);
    canvas.paint(
        &heading,
        Paint::vertical(rgb(title.0), rgb(title.1), y0, y0 + bar),
        Relief::FLAT,
        1.0,
    );
    let rule = block(x0 + 3.0, y0 + 2.0 + bar, x1 - 3.0, y0 + 4.0 + bar, 0.0);
    canvas.paint(&rule, Paint::Solid(rgb(0x2f_36_42)), Relief::FLAT, 0.8);
    for x in [x0 + 8.0, x0 + 24.0, x1 - 22.0] {
        let tool = block(x, y0 + 7.0, x + 14.0, y0 + bar - 1.0, 2.5);
        canvas.paint(
            &tool,
            Paint::Solid(rgb(0xcf_d5_dd)),
            Relief {
                width: 2.5,
                highlight: 0.5,
                shade: 0.45,
            },
            1.0,
        );
    }
}

/// What a RISC OS program's page is named by, at its head.
#[derive(Copy, Clone)]
enum Badge {
    /// The command line's prompt: a script of its commands.
    Star,
    /// A word.
    Word(&'static str),
}

/// Where the sail's pole stands, the height of its peak, and how wide its
/// white edging is.
const POLE: f64 = 206.0;
const PEAK: f64 = 82.0;
const EDGING: f64 = 7.0;

/// How far down the sail its red pennant reaches.
const PENNANT: f64 = 70.0;

/// `sin(60°)`, for the prompt's arms.
const SIN_60: f64 = 0.866_025_403_784_438_6;

/// A program for Acorn's RISC OS: a page of text with the Archimedes A in
/// its background — a yellow sail rising from the page's foot to a pole
/// beneath the fold, edged in white, its peak a red pennant — and the
/// program's kind at its head.
fn riscos_program(fonts: &Fonts, colour: u32, badge: Badge) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let page = sheet::body();
    // Along each slope `x + y` is a constant; the edging stands outside the
    // sail's own.
    let slope = POLE + PEAK;
    let edge = slope - EDGING * SQRT_2;
    let outer = POLE + EDGING;
    let edged =
        polygon(&[(edge - 256.0, 256.0), (outer, edge - outer), (outer, 256.0)]).within(&page);
    canvas.paint(
        &edged.outline(1.6).within(&page),
        Paint::Solid(rgb(0x1f_2a_44)),
        Relief::FLAT,
        1.0,
    );
    canvas.paint(
        &edged,
        Paint::vertical(rgb(0xff_ff_ff), rgb(0xe9_ec_ef), PEAK, 256.0),
        Relief {
            width: 3.0,
            highlight: 0.3,
            shade: 0.3,
        },
        1.0,
    );
    let sail = polygon(&[(slope - 256.0, 256.0), (POLE, PEAK), (POLE, 256.0)]).within(&page);
    canvas.paint(
        &sail,
        Paint::vertical(rgb(0xff_f5_9d), rgb(0xf2_c2_00), PEAK, 256.0),
        Relief::of(6.0),
        1.0,
    );
    let pennant = polygon(&[
        (POLE, PEAK),
        (POLE, PEAK + PENNANT),
        (POLE - PENNANT, PEAK + PENNANT),
    ]);
    canvas.paint(
        &pennant,
        Paint::vertical(rgb(0xff_6b_6b), rgb(0xc9_2a_2a), PEAK, PEAK + PENNANT),
        Relief::of(4.0),
        1.0,
    );
    let left = sheet::LEFT + 18.0;
    let label = match badge {
        Badge::Star => {
            let (cx, cy, reach) = (left + 20.0, 50.0, 20.0);
            let arm = |dx: f64, dy: f64| {
                Mask::of(&stroke(&[(cx - dx, cy - dy), (cx + dx, cy + dy)], 7.0))
            };
            arm(reach, 0.0)
                .union(&arm(reach * 0.5, reach * SIN_60))
                .union(&arm(reach * 0.5, -reach * SIN_60))
        }
        Badge::Word(word) => {
            // Set no wider than the head leaves before the fold.
            let room = sheet::RIGHT - sheet::FOLD - 18.0 - left;
            let measured = fonts.set(Typeface::Sans(800.0), word, 44.0, 1.5, (0.0, 0.0))?;
            let size = 44.0 * (room / (measured.ink.2 - measured.ink.0)).min(1.0);
            let set = fonts.set(Typeface::Sans(800.0), word, size, 1.5, (0.0, 0.0))?;
            let width = set.ink.2 - set.ink.0;
            fonts
                .set(
                    Typeface::Sans(800.0),
                    word,
                    size,
                    1.5,
                    (left + width / 2.0, 50.0),
                )?
                .mask
        }
    };
    canvas.shadow(&label, 0.8, 1.6, 1.2, 0.3);
    canvas.paint(
        &label,
        Paint::vertical(rgb(0x3d_8b_fd), rgb(0x0b_3a_85), 30.0, 70.0),
        Relief {
            width: 3.0,
            highlight: 0.35,
            shade: 0.3,
        },
        1.0,
    );
    // The text stops short of the sail's edge, the lines' ends falling back
    // with its slope.
    for (y, share) in [
        (88.0, 1.0),
        (105.0, 0.82),
        (122.0, 1.0),
        (139.0, 0.7),
        (156.0, 0.95),
        (173.0, 0.8),
        (190.0, 1.0),
    ] {
        let room = edge - y - 14.0 - left;
        if room > 20.0 {
            text_line(&mut canvas, left, y, room * share, rgb(INK));
        }
    }
    Ok(canvas)
}

fn absolute(fonts: &Fonts) -> Result<Canvas, String> {
    riscos_program(fonts, 0x0c_a6_78, Badge::Word("APP"))
}

fn utility(fonts: &Fonts) -> Result<Canvas, String> {
    riscos_program(fonts, 0xe6_77_00, Badge::Word("UTIL"))
}

fn module(fonts: &Fonts) -> Result<Canvas, String> {
    riscos_program(fonts, 0x84_5e_f7, Badge::Word("MOD"))
}

fn obey(fonts: &Fonts) -> Result<Canvas, String> {
    riscos_program(fonts, 0x37_b2_4d, Badge::Star)
}

fn drawing(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x10_98_ad, Foot::Bare)?;
    let triangle = polygon(&[(56.0, 200.0), (104.0, 104.0), (152.0, 200.0)]);
    raise(
        &mut canvas,
        &triangle,
        Paint::linear(
            (70.0, 104.0),
            (140.0, 200.0),
            rgb(0xff_d4_3b),
            rgb(0xe8_59_0c),
        ),
        Relief::of(6.0),
    );
    let ball = disc(150.0, 168.0, 32.0);
    raise(
        &mut canvas,
        &ball,
        Paint::linear(
            (126.0, 138.0),
            (176.0, 198.0),
            rgb(0x74_c0_fc),
            rgb(0x18_64_ab),
        ),
        Relief::of(7.0),
    );
    let points = [(52.0, 98.0), (92.0, 40.0), (198.0, 180.0), (204.0, 104.0)];
    vector_handles(&mut canvas, points);
    let path = Mask::of(&stroke(
        &cubic(points[0], points[1], points[2], points[3]),
        12.0,
    ));
    raise(
        &mut canvas,
        &path,
        Paint::vertical(rgb(0xff_6b_6b), rgb(0xc9_2a_2a), 40.0, 170.0),
        Relief::of(4.0),
    );
    vector_anchors(&mut canvas, points);
    Ok(canvas)
}

fn template(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x33_9a_f0, Foot::Bare)?;
    let (x0, y0, x1, y1) = (44.0, 70.0, 212.0, 200.0);
    window(
        &mut canvas,
        (x0, y0, x1, y1),
        23.0,
        (0xfb_ef_b0, 0xe6_c9_5a),
        (0xee_f1_f5, 0xd6_dc_e4),
    );
    canvas.paint(
        &block(MIDDLE - 36.0, y0 + 9.0, MIDDLE + 36.0, y0 + 15.0, 3.0),
        Paint::Solid(rgb(0x6b_5a_1e)),
        Relief::FLAT,
        0.55,
    );
    let field = block(x0 + 18.0, y0 + 42.0, x1 - 18.0, y0 + 64.0, 3.0);
    canvas.paint(
        &field.outline(1.4),
        Paint::Solid(rgb(0x5b_64_74)),
        Relief::FLAT,
        1.0,
    );
    canvas.paint(
        &field,
        Paint::vertical(rgb(0xff_ff_ff), rgb(0xf1_f4_f8), y0 + 42.0, y0 + 64.0),
        Relief::FLAT,
        1.0,
    );
    text_line(&mut canvas, x0 + 26.0, y0 + 53.0, 62.0, rgb(0x9a_a3_b2));
    let (top, bottom) = (y0 + 84.0, y0 + 114.0);
    let cancel = block(x0 + 18.0, top, MIDDLE - 6.0, bottom, 9.0);
    raise(
        &mut canvas,
        &cancel,
        Paint::vertical(rgb(0xf5_f7_fa), rgb(0xc3_ca_d4), top, bottom),
        Relief::of(4.0),
    );
    let confirm = block(MIDDLE + 6.0, top, x1 - 18.0, bottom, 9.0);
    raise(
        &mut canvas,
        &confirm,
        Paint::vertical(rgb(0x5e_a2_ff), rgb(0x1d_58_c9), top, bottom),
        Relief::of(4.0),
    );
    Ok(canvas)
}

fn font(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x4c_6e_f5, Foot::Lines)?;
    let set = fonts.set(
        Typeface::Serif(700.0),
        "Aa",
        108.0,
        2.0,
        (MIDDLE - 2.0, 120.0),
    )?;
    raise(
        &mut canvas,
        &set.mask,
        Paint::vertical(rgb(0x4a_55_6e), rgb(0x14_19_22), set.ink.1, set.ink.3),
        Relief {
            width: 4.0,
            highlight: 0.45,
            shade: 0.3,
        },
    );
    Ok(canvas)
}

fn postscript(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x09_92_68;
    let mut canvas = on_sheet(fonts, colour, Foot::Lines)?;
    let set = fonts.set(
        Typeface::Sans(800.0),
        "PS",
        100.0,
        2.0,
        (MIDDLE - 2.0, 120.0),
    )?;
    raise(
        &mut canvas,
        &set.mask,
        lit(rgb(colour), set.ink.1, set.ink.3),
        Relief::of(5.0),
    );
    Ok(canvas)
}

fn printout(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x49_50_57, Foot::Bare)?;
    let paper_in = block(MIDDLE - 42.0, 66.0, MIDDLE + 42.0, 124.0, 3.0);
    raise(
        &mut canvas,
        &paper_in,
        Paint::vertical(rgb(0xff_ff_ff), rgb(0xe7_eb_f0), 66.0, 124.0),
        Relief::of(3.0),
    );
    let (top, bottom) = (108.0, 178.0);
    let body = block(MIDDLE - 76.0, top, MIDDLE + 76.0, bottom, 12.0);
    raise(
        &mut canvas,
        &body,
        Paint::vertical(rgb(0x6f_7a_8c), rgb(0x2b_31_3b), top, bottom),
        Relief::of(7.0),
    );
    let slot = block(MIDDLE - 56.0, 146.0, MIDDLE + 56.0, 154.0, 4.0);
    canvas.paint(&slot, Paint::Solid(rgb(0x14_18_1e)), Relief::FLAT, 0.9);
    let lamp = disc(MIDDLE + 56.0, 126.0, 5.0);
    canvas.paint(
        &lamp,
        Paint::Solid(rgb(0x51_cf_66)),
        Relief {
            width: 3.0,
            highlight: 0.6,
            shade: 0.3,
        },
        1.0,
    );
    let (out_top, out_bottom) = (150.0, 214.0);
    let paper_out = block(MIDDLE - 48.0, out_top, MIDDLE + 48.0, out_bottom, 3.0);
    raise(
        &mut canvas,
        &paper_out,
        Paint::vertical(rgb(0xff_ff_ff), rgb(0xe4_e8_ee), out_top, out_bottom),
        Relief::of(3.0),
    );
    for (y, width) in [(170.0, 72.0), (183.0, 72.0), (196.0, 48.0)] {
        text_line(&mut canvas, MIDDLE - 36.0, y, width, rgb(0x9a_a3_b2));
    }
    Ok(canvas)
}

fn palette(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0xff_bb_00, Foot::Bare)?;
    let (side, gap) = (28.0, 6.0);
    let span = 4.0 * side + 3.0 * gap;
    let (x0, y0) = (MIDDLE - span / 2.0, 82.0);
    let tray = block(x0 - 9.0, y0 - 9.0, x0 + span + 9.0, y0 + span + 9.0, 11.0);
    raise(
        &mut canvas,
        &tray,
        Paint::vertical(rgb(0x5b_64_74), rgb(0x2b_31_3b), y0, y0 + span),
        Relief::of(5.0),
    );
    for (index, &colour) in (0u32..).zip(&WIMP) {
        let x = x0 + f64::from(index % 4) * (side + gap);
        let y = y0 + f64::from(index / 4) * (side + gap);
        let swatch = block(x, y, x + side, y + side, 5.0);
        canvas.paint(
            &swatch,
            Paint::Solid(rgb(colour)),
            Relief {
                width: 4.0,
                highlight: 0.35,
                shade: 0.35,
            },
            1.0,
        );
    }
    Ok(canvas)
}

// A photograph must clear the plate beneath it.
const _: () = assert!(MOUNT.3 <= FOOT_TOP);
