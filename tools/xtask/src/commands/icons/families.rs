//! The masters of the archive, RISC OS data and media families
//! (`plans/ICONS.md` I20): each a sheet of the one family style with an emblem
//! of what the file is, the Acorn mark left to the Acorn programs.

use super::canvas::{rgb, scaled, Canvas, Mask, Paint, Relief};
use super::masters::{block, disc, lit, on_sheet, photograph, polygon, raise, text_line, DAY, INK};
use super::sheet::{Foot, MIDDLE};
use super::text::{Fonts, Typeface};

/// The fold every archive shares: its format is its badge.
const ARCHIVE: u32 = 0xb0_74_2a;

/// A sheet held by a zip down its middle, the slider pulled part way: what
/// every archive shows, labelled by `foot`.
fn zipped(fonts: &Fonts, foot: Foot<'_>) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, ARCHIVE, foot)?;
    let bottom = if matches!(foot, Foot::Badge(_)) {
        168.0
    } else {
        214.0
    };
    let (top, half) = (30.0, 15.0);
    let tape = block(MIDDLE - half, top, MIDDLE + half, bottom, 6.0);
    raise(
        &mut canvas,
        &tape,
        Paint::vertical(
            scaled(rgb(ARCHIVE), 0.85),
            scaled(rgb(ARCHIVE), 0.6),
            top,
            bottom,
        ),
        Relief::of(3.0),
    );
    let metal = Paint::vertical(rgb(0xf1_f3_f5), rgb(0xad_b5_bd), top, bottom);
    let teeth = Relief {
        width: 2.0,
        highlight: 0.5,
        shade: 0.4,
    };
    // Open above the slider, the teeth meet below it.
    let slider = top + (bottom - top) * 0.42;
    let mut y = top + 6.0;
    let mut left = true;
    while y + 7.0 < bottom - 6.0 {
        let open = if y < slider { 5.0 } else { 0.0 };
        let (x0, x1) = if left {
            (MIDDLE - 12.0 - open, MIDDLE + 2.0 - open)
        } else {
            (MIDDLE - 2.0 + open, MIDDLE + 12.0 + open)
        };
        canvas.paint(&block(x0, y, x1, y + 6.0, 2.0), metal, teeth, 1.0);
        y += 8.0;
        left = !left;
    }
    let body = block(
        MIDDLE - 13.0,
        slider - 6.0,
        MIDDLE + 13.0,
        slider + 18.0,
        6.0,
    );
    raise(&mut canvas, &body, metal, Relief::of(4.0));
    let pull = block(
        MIDDLE - 7.0,
        slider + 14.0,
        MIDDLE + 7.0,
        slider + 44.0,
        6.0,
    )
    .minus(&block(
        MIDDLE - 3.0,
        slider + 30.0,
        MIDDLE + 3.0,
        slider + 38.0,
        3.0,
    ));
    raise(&mut canvas, &pull, metal, Relief::of(3.0));
    Ok(canvas)
}

pub fn archive(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Bare)
}

pub fn zip(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("ZIP"))
}

pub fn tar(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("TAR"))
}

pub fn gzip(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("GZ"))
}

pub fn bzip2(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("BZ2"))
}

pub fn xz(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("XZ"))
}

pub fn zstd(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("ZST"))
}

pub fn seven_zip(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("7Z"))
}

pub fn rar(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("RAR"))
}

pub fn jar(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("JAR"))
}

pub fn spark(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("SPK"))
}

pub fn arcfs(fonts: &Fonts) -> Result<Canvas, String> {
    zipped(fonts, Foot::Badge("ARC"))
}

/// RISC OS Data: a page of bytes, two hexadecimal digits each, in columns.
pub fn data(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x5c_7c_fa;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let rows: [[&str; 4]; 7] = [
        ["7F", "45", "4C", "46"],
        ["02", "01", "01", "00"],
        ["4F", "2A", "00", "7C"],
        ["93", "1B", "FF", "08"],
        ["E5", "60", "3D", "A1"],
        ["00", "00", "17", "C2"],
        ["A0", "5E", "C4", "31"],
    ];
    let columns = [MIDDLE - 64.0, MIDDLE - 22.0, MIDDLE + 20.0, MIDDLE + 62.0];
    for (row, values) in (0u32..).zip(&rows) {
        let y = 84.0 + f64::from(row) * 20.0;
        for (column, (&x, value)) in columns.iter().zip(values).enumerate() {
            let ink = if column % 2 == 0 {
                rgb(INK)
            } else {
                scaled(rgb(colour), 0.95)
            };
            let set = fonts.set(Typeface::Sans(700.0), value, 22.0, 1.0, (x, y))?;
            canvas.paint(
                &set.mask,
                Paint::Solid(ink),
                Relief {
                    width: 1.5,
                    highlight: 0.25,
                    shade: 0.35,
                },
                1.0,
            );
        }
    }
    Ok(canvas)
}

/// RISC OS Command: a command line, its `*` prompt and its cursor, above the
/// lines it types.
pub fn command(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0xf5_9f_00;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let (x0, y0, x1, y1) = (MIDDLE - 76.0, 74.0, MIDDLE + 76.0, 150.0);
    let screen = block(x0, y0, x1, y1, 10.0);
    raise(
        &mut canvas,
        &screen,
        Paint::vertical(rgb(0x34_3a_40), rgb(0x16_19_1d), y0, y1),
        Relief::of(5.0),
    );
    let prompt = fonts.set(Typeface::Sans(800.0), "*", 64.0, 0.0, (x0 + 30.0, 114.0))?;
    canvas.paint(
        &prompt.mask,
        lit(rgb(0xff_d4_3b), prompt.ink.1, prompt.ink.3),
        Relief::of(2.0),
        1.0,
    );
    let cursor = block(x0 + 56.0, 112.0, x0 + 78.0, 122.0, 2.0);
    canvas.paint(&cursor, Paint::Solid(rgb(0xff_e0_66)), Relief::FLAT, 1.0);
    for (y, width) in [(172.0, 150.0), (189.0, 120.0), (206.0, 138.0)] {
        text_line(&mut canvas, MIDDLE - 75.0, y, width, rgb(INK));
    }
    Ok(canvas)
}

/// RISC OS Desktop: a desktop in small, a window on its backdrop and the icon
/// bar along its foot.
pub fn desktop(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x22_8b_e6;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let (x0, y0, x1, y1) = (MIDDLE - 80.0, 74.0, MIDDLE + 80.0, 206.0);
    let screen = block(x0, y0, x1, y1, 8.0);
    raise(
        &mut canvas,
        &screen,
        Paint::vertical(rgb(0xa5_d8_ff), rgb(0x4d_ab_f7), y0, y1),
        Relief::of(4.0),
    );
    let window = |canvas: &mut Canvas, (left, top, right, bottom): (f64, f64, f64, f64)| {
        let frame = block(left, top, right, bottom, 3.0);
        raise(
            canvas,
            &frame,
            Paint::vertical(rgb(0xff_ff_ff), rgb(0xe9_ec_ef), top, bottom),
            Relief::of(2.0),
        );
        let band = block(left, top, right, top + 11.0, 3.0);
        canvas.paint(&band, Paint::Solid(rgb(0x49_50_57)), Relief::FLAT, 1.0);
    };
    window(&mut canvas, (x0 + 14.0, y0 + 14.0, x0 + 96.0, y0 + 82.0));
    window(&mut canvas, (x0 + 62.0, y0 + 40.0, x1 - 14.0, y1 - 34.0));
    let bar = block(x0 + 2.0, y1 - 24.0, x1 - 2.0, y1 - 2.0, 6.0);
    canvas.paint(
        &bar,
        Paint::vertical(rgb(0xde_e2_e6), rgb(0xad_b5_bd), y1 - 24.0, y1),
        Relief::of(2.0),
        1.0,
    );
    for (x, tint) in [
        (x0 + 18.0, 0x4d_ab_f7),
        (x0 + 38.0, 0xff_d4_3b),
        (x1 - 38.0, 0x51_cf_66),
        (x1 - 18.0, 0xff_6b_6b),
    ] {
        canvas.paint(
            &disc(x, y1 - 13.0, 6.0),
            Paint::Solid(rgb(tint)),
            Relief::of(2.0),
            1.0,
        );
    }
    Ok(canvas)
}

/// RISC OS Squash: a page of text pressed into half its width between two
/// jaws.
pub fn squash(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0xe8_59_0c;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let jaw = |canvas: &mut Canvas, outer: f64, inner: f64| {
        let (left, right) = (outer.min(inner), outer.max(inner));
        let plate = block(left, 102.0, right, 214.0, 6.0);
        raise(
            canvas,
            &plate,
            Paint::vertical(rgb(0x86_8e_96), rgb(0x34_3a_40), 102.0, 214.0),
            Relief::of(4.0),
        );
    };
    jaw(&mut canvas, MIDDLE - 84.0, MIDDLE - 62.0);
    jaw(&mut canvas, MIDDLE + 62.0, MIDDLE + 84.0);
    let press = |towards: f64, from: f64| polygon(&[(from, 72.0), (towards, 84.0), (from, 96.0)]);
    for arrow in [
        press(MIDDLE - 66.0, MIDDLE - 94.0),
        press(MIDDLE + 66.0, MIDDLE + 94.0),
    ] {
        raise(
            &mut canvas,
            &arrow,
            lit(rgb(colour), 72.0, 96.0),
            Relief::of(3.0),
        );
    }
    let mut lines = Mask::empty();
    for (row, width) in (0u32..).zip([92.0, 86.0, 94.0, 70.0, 90.0, 82.0, 94.0, 60.0]) {
        let y = 114.0 + f64::from(row) * 13.5;
        lines = lines.union(&block(
            MIDDLE - 47.0,
            y - 3.0,
            MIDDLE - 47.0 + width,
            y + 3.0,
            3.0,
        ));
    }
    canvas.paint(
        &lines,
        Paint::Solid(rgb(INK)),
        Relief {
            width: 2.0,
            highlight: 0.3,
            shade: 0.45,
        },
        1.0,
    );
    Ok(canvas)
}

/// Sound: a waveform's bars about the page's middle.
pub fn audio(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x70_48_e8;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let heights = [
        18.0, 34.0, 58.0, 86.0, 62.0, 104.0, 128.0, 96.0, 70.0, 112.0, 80.0, 46.0, 66.0, 30.0,
    ];
    let (centre, pitch, width) = (128.0, 11.0, 7.0);
    let last = f64::from(u32::try_from(heights.len() - 1).unwrap_or(0));
    let start = MIDDLE - pitch * last / 2.0;
    let mut bars = Mask::empty();
    for (index, height) in (0u32..).zip(heights) {
        let x = start + f64::from(index) * pitch;
        bars = bars.union(&block(
            x - width / 2.0,
            centre - height / 2.0,
            x + width / 2.0,
            centre + height / 2.0,
            width / 2.0,
        ));
    }
    raise(
        &mut canvas,
        &bars,
        lit(rgb(colour), centre - 64.0, centre + 64.0),
        Relief::of(3.0),
    );
    Ok(canvas)
}

/// Video: a frame of film between its sprocket holes, a play mark on it.
pub fn video(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0xc2_25_5c;
    let mut canvas = on_sheet(fonts, colour, Foot::Bare)?;
    let (x0, y0, x1, y1) = (MIDDLE - 82.0, 70.0, MIDDLE + 82.0, 188.0);
    let mut film = block(x0, y0, x1, y1, 8.0);
    for row in [y0 + 6.0, y1 - 16.0] {
        let mut x = x0 + 10.0;
        while x + 12.0 < x1 - 6.0 {
            film = film.minus(&block(x, row, x + 12.0, row + 10.0, 2.5));
            x += 20.0;
        }
    }
    raise(
        &mut canvas,
        &film,
        Paint::vertical(rgb(0x34_3a_40), rgb(0x16_19_1d), y0, y1),
        Relief::of(4.0),
    );
    let frame = block(x0 + 8.0, y0 + 22.0, x1 - 8.0, y1 - 22.0, 3.0);
    canvas.paint(
        &frame,
        Paint::vertical(scaled(rgb(colour), 1.35), scaled(rgb(colour), 0.8), y0, y1),
        Relief::FLAT,
        1.0,
    );
    let play = polygon(&[
        (MIDDLE - 14.0, 108.0),
        (MIDDLE + 20.0, 129.0),
        (MIDDLE - 14.0, 150.0),
    ]);
    raise(
        &mut canvas,
        &play,
        Paint::Solid(rgb(0xff_ff_ff)),
        Relief::of(3.0),
    );
    Ok(canvas)
}

/// A picture of no format with a master of its own: a photograph, unlabelled.
pub fn image(fonts: &Fonts) -> Result<Canvas, String> {
    let mut canvas = on_sheet(fonts, 0x10_98_ad, Foot::Bare)?;
    photograph(&mut canvas, (46.0, 70.0, 210.0, 200.0), &DAY);
    Ok(canvas)
}

/// Comma-separated values: a table, its header row in the fold's colour.
pub fn csv(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x2b_8a_3e;
    let mut canvas = on_sheet(fonts, colour, Foot::Badge("CSV"))?;
    let (x0, y0, x1) = (MIDDLE - 78.0, 72.0, MIDDLE + 78.0);
    let (row, rows, columns) = (16.0, 6u32, 3u32);
    let y1 = y0 + row * f64::from(rows);
    let sheet = block(x0, y0, x1, y1, 5.0);
    raise(
        &mut canvas,
        &sheet,
        Paint::vertical(rgb(0xff_ff_ff), rgb(0xf1_f3_f5), y0, y1),
        Relief::of(3.0),
    );
    let header = block(x0, y0, x1, y0 + row, 5.0).within(&sheet);
    canvas.paint(&header, lit(rgb(colour), y0, y0 + row), Relief::FLAT, 1.0);
    let rule = Paint::Solid(rgb(0xce_d4_da));
    let width = (x1 - x0) / f64::from(columns);
    for column in 1..columns {
        let x = x0 + width * f64::from(column);
        canvas.paint(
            &block(x - 0.75, y0, x + 0.75, y1, 0.0),
            rule,
            Relief::FLAT,
            1.0,
        );
    }
    for line in 2..rows {
        let y = y0 + row * f64::from(line);
        canvas.paint(
            &block(x0, y - 0.75, x1, y + 0.75, 0.0),
            rule,
            Relief::FLAT,
            1.0,
        );
    }
    for line in 1..rows {
        for column in 0..columns {
            let left = x0 + width * f64::from(column) + 7.0;
            let length = width - 14.0 - f64::from((line + column) % 3) * 9.0;
            text_line(
                &mut canvas,
                left,
                y0 + row * (f64::from(line) + 0.5),
                length,
                rgb(INK),
            );
        }
    }
    Ok(canvas)
}

/// A makefile: its rules' text above its badge, `$@` the target it makes.
pub fn makefile(fonts: &Fonts) -> Result<Canvas, String> {
    let colour = 0x5f_3d_c4;
    let mut canvas = on_sheet(fonts, colour, Foot::Badge("MAKE"))?;
    let set = fonts.set(Typeface::Sans(800.0), "$@", 76.0, 4.0, (MIDDLE, 92.0))?;
    raise(
        &mut canvas,
        &set.mask,
        lit(rgb(colour), set.ink.1, set.ink.3),
        Relief::of(5.0),
    );
    for (y, width) in [(140.0, 150.0), (156.0, 118.0)] {
        text_line(&mut canvas, MIDDLE - 75.0, y, width, rgb(INK));
    }
    Ok(canvas)
}
