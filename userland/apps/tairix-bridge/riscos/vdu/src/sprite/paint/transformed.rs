//! SpriteExtend's transformed plots, `OS_SpriteOp` 55 and 56, as RISC OS 5
//! draws them (held to it by the recorded corpus):
//!
//! - a destination row is drawn where its centre line crosses the image of
//!   the source rectangle, and a pixel where its centre lies strictly right
//!   of the image's left boundary on that line and at or left of its right
//!   one; rows likewise, strictly above the lowest corner and at or below the
//!   highest;
//! - each row's pixels sample the source on its centre line at a grid of
//!   whole pixels anchored to the image of the source rectangle's top left,
//!   that point's sub-pixel position carried into every column; the row's
//!   own offset from that point is exact, each column's step a 16.16 value
//!   rounded to the nearest;
//! - a sample's source column is its coordinate rounded down, and its row
//!   is counted down from the rectangle's top and rounded down likewise,
//!   both held to the rectangle.
//!
//! The parallelogram form maps the rectangle's first corner to its first
//! point, the whole sprite's first corner being its top left. The matrix
//! form is the parallelogram Draw makes of the rectangle, held to the sprite:
//! SpriteExtend has `Draw_TransformPath` move its first corner and the two
//! beside it in place, so a claimant of `DrawV` sees them and may move them,
//! Draw's refusal of a matrix that overflows is the plot's, and with Draw
//! gone the plot fails as its call does. Both forms' points are from the
//! graphics origin, and no matrix at all leaves the corners where they are.
//! Every product is exact or refused: a parallelogram too extreme to carry
//! draws nothing.

use tairix_drawing::riscos::format::{END, MOVE};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::output::workspace;
use tairix_riscos_kernel::swi::{draw, X};
use tairix_util::divisor::gcd;

use super::super::format;
use super::super::header::Sprite;
use super::colours::{converted_first, mapped, Conversion, Converted};
use super::scaled::{paint, plot_block};
use super::source::{Checked, Source};
use super::{Destination, Mapping, PlotFlags};
use crate::changed::Rect;
use crate::state;

/// R3's bit making R6 a destination parallelogram rather than a matrix.
const PARALLELOGRAM: u32 = 1;
/// R3's bit making R4 a source rectangle.
const SOURCE_RECTANGLE: u32 = 2;
/// The fraction bits of a column step, as RISC OS 5 carries it.
const STEP_BITS: u32 = 16;
/// The fraction bits a row's offset is carried to: enough that rounding
/// it down never moves the step grid's whole part.
const ROW_BITS: u32 = 32;
/// The words of the path SpriteExtend hands Draw: three moves and the end.
const CORNER_PATH_WORDS: usize = 10;

/// `OS_SpriteOp` 56: the sprite through R6's matrix or parallelogram, the
/// part of it R4 names where R3 asks, turned into the destination's pixels
/// by R7's table.
pub(crate) fn put_transformed(
    os: &mut Os<'_>,
    sprite: &Sprite,
    form: u32,
    rectangle: u32,
    flags: PlotFlags,
    shape: u32,
    table: u32,
) -> Result<(), Fail> {
    let checked = Source::check(os, sprite)?;
    let destination = Destination::now(os)?;
    let (mut source, converted, destination) =
        converted_first(os, &checked, &destination, flags, table)?;
    let handed = Handed {
        row: handed_row(&checked, form),
        action: flags.0,
        table: table_in_use(&converted, &checked, &destination, table),
    };
    let transform = Transform::read(os, &checked, &destination, form, rectangle, shape, handed)?;
    let Some(mut transform) = transform.filter(|_| converted.draws()) else {
        return Ok(());
    };
    if !mapped(&checked.pixels, flags) {
        checked.read_rows(os, &mut source, transform.source_rows())?;
    }
    let bounds = transform.bounds;
    paint(
        os,
        &source,
        &destination,
        &converted,
        flags,
        bounds,
        &mut transform,
    )
}

/// `OS_SpriteOp` 55: the background colour plotted through R6's matrix or
/// parallelogram under the sprite's mask, solid where it has none.
pub(crate) fn plot_mask_transformed(
    os: &mut Os<'_>,
    sprite: &Sprite,
    form: u32,
    rectangle: u32,
    shape: u32,
) -> Result<(), Fail> {
    let checked = Source::check(os, sprite)?;
    let destination = Destination::now(os)?;
    let handed = Handed {
        row: handed_row(&checked, form),
        action: PlotFlags::MASKED,
        table: 0,
    };
    let Some(mut transform) =
        Transform::read(os, &checked, &destination, form, rectangle, shape, handed)?
    else {
        return Ok(());
    };
    let source = checked.read(os, transform.source_rows())?;
    let bounds = transform.bounds;
    let solid = |(x, row): (u32, u32)| source.alpha(x, row) != 0;
    plot_block(
        os,
        &destination,
        workspace::BACKGROUND,
        bounds,
        &mut transform,
        solid,
    )
}

/// R4, R5 and R7 as SpriteExtend hands them to Draw with the corners, which
/// a claimant of `DrawV` sees: the sprite's row in words, or in bits where a
/// source rectangle is named, the plot's action, and the table it plots
/// through.
#[derive(Clone, Copy, Debug)]
struct Handed {
    row: u32,
    action: u32,
    table: u32,
}

/// The sprite's row as R4 hands it to Draw for R3 `form`.
fn handed_row(checked: &Checked<'_>, form: u32) -> u32 {
    let words = checked.row_words();
    if form & SOURCE_RECTANGLE == 0 {
        words
    } else {
        words.wrapping_mul(u32::BITS)
    }
}

/// The table at `table` as the plot uses it: none where it maps a sprite of
/// the destination's depth to itself.
fn table_in_use(
    converted: &Converted,
    checked: &Checked<'_>,
    destination: &Destination,
    table: u32,
) -> u32 {
    let identity = checked.pixels.log2_bpp == destination.format.log2_bpp()
        && matches!(&converted.conversion, Conversion::Bytes(bytes)
            if bytes.iter().enumerate().all(|(i, &byte)| usize::from(byte) == i));
    if identity {
        0
    } else {
        table
    }
}

/// `corners` each held to a sprite of `size` pixels, their order kept.
fn held(corners: [i128; 4], size: (i128, i128)) -> [i128; 4] {
    let [x0, y0, x1, y1] = corners;
    [
        x0.clamp(0, size.0),
        y0.clamp(0, size.1),
        x1.clamp(0, size.0),
        y1.clamp(0, size.1),
    ]
}

/// The path SpriteExtend hands Draw: moves to the rectangle `corners`' first
/// corner, the one along its row, and the one along its column, each in
/// 1/256 OS units of a sprite at `eig`, carried in a word as RISC OS's are.
fn corner_path(corners: [i128; 4], eig: (u32, u32)) -> [u32; CORNER_PATH_WORDS] {
    let unit = |pixels: i128, eig: u32| u32::try_from(pixels).unwrap_or(0).wrapping_shl(8 + eig);
    let [x0, y0, x1, y1] = corners;
    let (x0, y0, x1, y1) = (
        unit(x0, eig.0),
        unit(y0, eig.1),
        unit(x1, eig.0),
        unit(y1, eig.1),
    );
    [MOVE, x0, y0, MOVE, x1, y0, MOVE, x0, y1, END]
}

/// The parallelogram the matrix at `matrix` makes of the rectangle
/// `corners`, its first point and the two beside it, as Draw moves the
/// corners in place below SpriteExtend's stack.
fn drawn_corners(
    os: &mut Os<'_>,
    corners: [i128; 4],
    eig: (u32, u32),
    matrix: u32,
    handed: Handed,
) -> Result<[i128; 8], Fail> {
    let mut path = [0u8; 4 * CORNER_PATH_WORDS];
    for (bytes, word) in path
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(corner_path(corners, eig))
    {
        *bytes = word.to_le_bytes();
    }
    os.below_supervisor_stack(len32(path.len()), |os, at| {
        os.write_bytes(at, &path)?;
        let mut regs = Regs::default();
        regs.r[..8].copy_from_slice(&[
            at,
            0,
            matrix,
            0,
            handed.row,
            handed.action,
            matrix,
            handed.table,
        ]);
        os.perform(X | draw::TRANSFORM_PATH, &mut regs)?;
        let mut point = |n: u32| -> Result<[i128; 2], Fail> {
            let x = os.read_word(at.wrapping_add(12 * n + 4))?.cast_signed();
            let y = os.read_word(at.wrapping_add(12 * n + 8))?.cast_signed();
            Ok([i128::from(x), i128::from(y)])
        };
        let [[x0, y0], [x1, y1], [x3, y3]] = [point(0)?, point(1)?, point(2)?];
        Ok([x0, y0, x1, y1, 0, 0, x3, y3])
    })
}

/// The pixel a picture of `size` pixels, held at the standard screen's
/// resolution, is plotted from and its scale factors, where R2's matrix or
/// parallelogram (the picture's top left its first point) only scales it.
pub(crate) fn scaling_of(
    os: &mut Os<'_>,
    size: (u32, u32),
    parallelogram: bool,
    shape: u32,
) -> Result<Option<(crate::raster::Point, super::Scale)>, Fail> {
    let mut words = [0i32; 8];
    let count = if parallelogram { 8 } else { 6 };
    for (i, word) in (0u32..).zip(words.iter_mut().take(count)) {
        *word = os.read_word(shape.wrapping_add(4 * i))?.cast_signed();
    }
    let eig = state::with(os, |vdu, _| {
        Ok((vdu.format().x_eig(), vdu.format().y_eig()))
    })?;
    let pixel = (256u32 << eig.0, 256u32 << eig.1);
    // A matrix scales OS units, and a picture's pixel is the standard screen's.
    let picture = 256u32 << format::STANDARD_EIG;
    let positive = |v: i32| u32::try_from(v).ok().filter(|&v| v > 0);
    let found = if parallelogram {
        let [x0, y0, x1, y1, _, _, x3, y3] = words;
        let across = x1.checked_sub(x0).and_then(positive);
        let down = y0.checked_sub(y3).and_then(positive);
        match (across, down) {
            (Some(across), Some(down)) if y1 == y0 && x3 == x0 => super::Scale::of(
                (across, down),
                (
                    size.0.saturating_mul(pixel.0),
                    size.1.saturating_mul(pixel.1),
                ),
            )
            .map(|scale| ((x3, y3), scale)),
            _ => None,
        }
    } else {
        let [x_across, y_across, x_down, y_down, shift_x, shift_y, _, _] = words;
        match (positive(x_across), positive(y_down)) {
            (Some(across), Some(down)) if y_across == 0 && x_down == 0 => super::Scale::of(
                (across, down),
                ((pixel.0 << 16) / picture, (pixel.1 << 16) / picture),
            )
            .map(|scale| ((shift_x, shift_y), scale)),
            _ => None,
        }
    };
    let Some(((x, y), scale)) = found else {
        return Ok(None);
    };
    let at = super::at(os, (x >> 8).cast_unsigned(), (y >> 8).cast_unsigned())?;
    Ok(Some((at, scale)))
}

/// `n / d` rounded down, `d` nonzero.
fn floor_div(n: i128, d: i128) -> Option<i128> {
    let (n, d) = if d < 0 {
        (n.checked_neg()?, d.checked_neg()?)
    } else {
        (n, d)
    };
    n.checked_div_euclid(d)
}

/// The greatest common divisor of the magnitudes of `values`, at least one.
fn common_factor(values: &[i128]) -> Option<i128> {
    let factor = values
        .iter()
        .fold(0, |factor, value| gcd(factor, value.unsigned_abs()));
    i128::try_from(factor.max(1)).ok()
}

/// `value` carried to `bits` fraction bits; none where that does not fit.
fn fixed(value: i128, bits: u32) -> Option<i128> {
    value.checked_mul(1 << bits)
}

/// `n / d` rounded to the nearest, halves up.
fn nearest_div(n: i128, d: i128) -> Option<i128> {
    let (n, d) = if d < 0 {
        (n.checked_neg()?, d.checked_neg()?)
    } else {
        (n, d)
    };
    floor_div(n.checked_mul(2)?.checked_add(d)?, d.checked_mul(2)?)
}

/// A coordinate of the inverse map, `base + (x * (X - Xb) + y * (Y - Yb)) /
/// den`, `den` positive, from a destination point in 1/256 OS units.
#[derive(Clone, Copy, Debug)]
struct Inverse {
    base: i128,
    x: i128,
    y: i128,
    den: i128,
}

impl Inverse {
    /// The coordinate in lowest terms, its denominator positive.
    fn new(base: i128, x: i128, y: i128, den: i128) -> Option<Self> {
        if den == 0 {
            return None;
        }
        let common = common_factor(&[x, y, den])? * den.signum();
        Some(Self {
            base,
            x: x / common,
            y: y / common,
            den: den / common,
        })
    }
}

/// A coordinate of the destination in 1/256 OS units, `n / d`, `d`
/// positive.
#[derive(Clone, Copy, Debug)]
struct Exact {
    n: i128,
    d: i128,
}

impl Exact {
    /// `n / d` in lowest terms.
    fn new(n: i128, d: i128) -> Option<Self> {
        if d == 0 {
            return None;
        }
        let common = common_factor(&[n, d])? * d.signum();
        Some(Self {
            n: n / common,
            d: d / common,
        })
    }
}

/// A transform's forward map: the destination per source pixel, over a
/// shared denominator, from source corner `base` to destination `origin`.
struct Forward {
    jacobian: [i128; 4],
    den: i128,
    origin: (i128, i128),
    base: (i128, i128),
}

impl Forward {
    /// The map the parallelogram `points` gives the rectangle `corners`.
    fn of(corners: [i128; 4], points: [i128; 8]) -> Option<Self> {
        let [x0, y0, x1, y1] = corners;
        let [px0, py0, px1, py1, _, _, px3, py3] = points;
        let (dx, dy) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
        Some(Self {
            jacobian: [
                px1.checked_sub(px0)?.checked_mul(dy)?,
                px3.checked_sub(px0)?.checked_mul(dx)?,
                py1.checked_sub(py0)?.checked_mul(dy)?,
                py3.checked_sub(py0)?.checked_mul(dx)?,
            ],
            den: dx.checked_mul(dy)?,
            origin: (px0, py0),
            base: (x0, y0),
        })
    }

    /// The inverse maps, a destination point to its source column and row;
    /// none where the map is singular.
    fn inverse(&self) -> Option<(Inverse, Inverse)> {
        let [xu, xv, yu, yv] = self.jacobian;
        let den = self.den;
        let det = xu.checked_mul(yv)?.checked_sub(xv.checked_mul(yu)?)?;
        if det == 0 || den == 0 {
            return None;
        }
        Some((
            Inverse::new(
                self.base.0,
                yv.checked_mul(den)?,
                xv.checked_mul(den)?.checked_neg()?,
                det,
            )?,
            Inverse::new(
                self.base.1,
                yu.checked_mul(den)?.checked_neg()?,
                xu.checked_mul(den)?,
                det,
            )?,
        ))
    }

    /// Where source point `(su, sv)` lands.
    fn image(&self, su: i128, sv: i128) -> Option<(Exact, Exact)> {
        let [xu, xv, yu, yv] = self.jacobian;
        let (du, dv) = (su.checked_sub(self.base.0)?, sv.checked_sub(self.base.1)?);
        let along = |o: i128, ju: i128, jv: i128| -> Option<Exact> {
            let n = o
                .checked_mul(self.den)?
                .checked_add(ju.checked_mul(du)?)?
                .checked_add(jv.checked_mul(dv)?)?;
            Exact::new(n, self.den)
        };
        Some((along(self.origin.0, xu, xv)?, along(self.origin.1, yu, yv)?))
    }
}

/// The destination pixels whose centres lie within the extremes of
/// `images`, a rectangle's corners mapped, held to `window`.
fn covered(images: &[(Exact, Exact); 4], pixel: (i128, i128), window: Rect) -> Option<Rect> {
    let extreme = |pick: fn(&(Exact, Exact)) -> Exact, lowest: bool| -> Option<Exact> {
        let mut best = pick(&images[0]);
        for image in &images[1..] {
            let candidate = pick(image);
            let below = candidate.n.checked_mul(best.d)? < best.n.checked_mul(candidate.d)?;
            if below == lowest {
                best = candidate;
            }
        }
        Some(best)
    };
    let (rows_from, rows_to) =
        centres_between(extreme(|i| i.1, true)?, extreme(|i| i.1, false)?, pixel.1)?;
    let (columns_from, columns_to) =
        centres_between(extreme(|i| i.0, true)?, extreme(|i| i.0, false)?, pixel.0)?;
    let clip = |low: i128, high: i128, w_low: i32, w_high: i32| {
        let low = i32::try_from(low.max(i128::from(w_low))).ok()?;
        let high = i32::try_from(high.min(i128::from(w_high))).ok()?;
        (low <= high).then_some((low, high))
    };
    let (bottom, top) = clip(rows_from, rows_to, window.bottom, window.top)?;
    let (left, right) = clip(columns_from, columns_to, window.left, window.right)?;
    Some(Rect {
        left,
        bottom,
        right,
        top,
    })
}

/// The first and last pixel whose centres, `index * size + size / 2`, lie
/// strictly above `low` and at or below `high`.
fn centres_between(low: Exact, high: Exact, size: i128) -> Option<(i128, i128)> {
    let first = floor_div(
        low.n.checked_sub(low.d.checked_mul(size / 2)?)?,
        low.d.checked_mul(size)?,
    )? + 1;
    let last = floor_div(
        high.n.checked_sub(high.d.checked_mul(size / 2)?)?,
        high.d.checked_mul(size)?,
    )?;
    Some((first, last))
}

/// One transformed plot's geometry, worked a row at a time.
struct Transform {
    u: Inverse,
    v: Inverse,
    /// The destination point the inverse is taken from.
    origin: (i128, i128),
    /// The given rectangle's top left, in sprite pixels from the bottom left.
    corner: (i128, i128),
    /// The source rectangle within the sprite.
    left: i128,
    bottom: i128,
    right: i128,
    top: i128,
    /// The sprite's rows.
    height: u32,
    /// The destination's pixel, in 1/256 OS units.
    pixel: (i128, i128),
    /// The image of the rectangle's top left.
    reference: (Exact, Exact),
    /// The destination column the image of the rectangle's top left lies in.
    anchor: i128,
    /// Each column's step in the source column and in the row from the top.
    step: (i128, i128),
    /// The pixels the plot can reach.
    bounds: Rect,
    /// The row under way: its next column's source coordinates, carried to
    /// `ROW_BITS` fraction bits.
    at: (i128, i128),
}

impl Transform {
    /// The geometry R3, R4 and R6 give, if it draws anything: R6's
    /// parallelogram, or the one Draw makes of the rectangle through R6's
    /// matrix, `handed` its other registers.
    fn read(
        os: &mut Os<'_>,
        checked: &Checked<'_>,
        destination: &Destination,
        form: u32,
        rectangle: u32,
        shape: u32,
        handed: Handed,
    ) -> Result<Option<Self>, Fail> {
        let size = (i128::from(checked.width), i128::from(checked.height));
        let given = if form & SOURCE_RECTANGLE == 0 {
            [0, size.1, size.0, 0]
        } else {
            let mut words = [0i128; 4];
            for (i, word) in (0u32..).zip(words.iter_mut()) {
                *word = i128::from(os.read_word(rectangle.wrapping_add(4 * i))?.cast_signed());
            }
            words
        };
        let (corners, mut points) = if form & PARALLELOGRAM == 0 {
            let corners = held(given, size);
            let points = drawn_corners(os, corners, checked.pixels.shown.eig, shape, handed)?;
            (corners, points)
        } else {
            let mut points = [0i128; 8];
            for (i, point) in (0u32..).zip(points.iter_mut()) {
                *point = i128::from(os.read_word(shape.wrapping_add(4 * i))?.cast_signed());
            }
            (given, points)
        };
        let origin = state::with(os, |vdu, _| Ok(vdu.graphics.origin))?;
        for (i, point) in points.iter_mut().enumerate() {
            let along = if i % 2 == 0 { origin.0 } else { origin.1 };
            *point += i128::from(along) << 8;
        }
        let format = destination.format;
        let pixel = (256i128 << format.x_eig(), 256i128 << format.y_eig());
        Ok(Self::of(corners, points, size, pixel, destination.window))
    }

    /// The rows, from the top, of the sprite it reads: the source
    /// rectangle's.
    fn source_rows(&self) -> core::ops::Range<u32> {
        let height = i128::from(self.height);
        let row = |r: i128| u32::try_from(r).unwrap_or(0);
        row(height - self.top)..row(height - self.bottom)
    }

    /// The geometry of the rectangle `corners` of a sprite of `size` pixels
    /// mapped onto the parallelogram `points`, drawn on `pixel`-sized pixels
    /// within `window`.
    fn of(
        corners: [i128; 4],
        points: [i128; 8],
        size: (i128, i128),
        pixel: (i128, i128),
        window: Rect,
    ) -> Option<Self> {
        let [x0, y0, x1, y1] = corners;
        let (left, right) = (x0.min(x1).max(0), x0.max(x1).min(size.0));
        let (bottom, top) = (y0.min(y1).max(0), y0.max(y1).min(size.1));
        if left >= right || bottom >= top {
            return None;
        }
        let forward = Forward::of(corners, points)?;
        let (u, v) = forward.inverse()?;
        let images = [
            forward.image(left, bottom)?,
            forward.image(right, bottom)?,
            forward.image(right, top)?,
            forward.image(left, top)?,
        ];
        // The given rectangle's top left, a corner of the parallelogram even
        // where the sprite clips the rectangle.
        let corner = (x0.min(x1), y0.max(y1));
        let reference = forward.image(corner.0, corner.1)?;
        let bounds = covered(&images, pixel, window)?;
        let step = |inverse: &Inverse| {
            let step = nearest_div(
                fixed(pixel.0.checked_mul(inverse.x)?, STEP_BITS)?,
                inverse.den,
            )?;
            fixed(step, ROW_BITS - STEP_BITS)
        };
        let step = (step(&u)?, step(&v)?.checked_neg()?);
        let anchor = floor_div(reference.0.n, reference.0.d.checked_mul(pixel.0)?)?;
        Some(Self {
            u,
            v,
            origin: forward.origin,
            corner,
            left,
            bottom,
            right,
            top,
            height: u32::try_from(size.1).ok()?,
            pixel,
            reference,
            anchor,
            step,
            bounds,
            at: (0, 0),
        })
    }

    /// The columns the centre line `y_line` covers, as `first..=last`, those
    /// strictly right of the image's left boundary and at or left of its right.
    fn span(&self, y_line: i128) -> Option<(i128, i128)> {
        let mut first = i128::MIN;
        let mut last = i128::MAX;
        for (inverse, low, high) in [
            (&self.u, self.left, self.right),
            (&self.v, self.bottom, self.top),
        ] {
            let lean = inverse.y.checked_mul(y_line.checked_sub(self.origin.1)?)?;
            let at_x = inverse.x.checked_mul(self.origin.0)?;
            for (bound, at_least) in [(low, true), (high, false)] {
                // x * X >= c, or <= c where `at_least` is false.
                let c = bound
                    .checked_sub(inverse.base)?
                    .checked_mul(inverse.den)?
                    .checked_add(at_x)?
                    .checked_sub(lean)?;
                let (a, c) = if at_least {
                    (inverse.x, c)
                } else {
                    (inverse.x.checked_neg()?, c.checked_neg()?)
                };
                if a == 0 {
                    if c > 0 {
                        return None;
                    }
                    continue;
                }
                let half = self.pixel.0 / 2;
                let edge = floor_div(
                    c.checked_sub(a.checked_mul(half)?)?,
                    a.checked_mul(self.pixel.0)?,
                )?;
                if a > 0 {
                    first = first.max(edge + 1);
                } else {
                    last = last.min(edge);
                }
            }
        }
        (first <= last).then_some((first, last))
    }

    /// The source column and row down from the given rectangle's top at the
    /// reference column on centre line `y_line`, to `ROW_BITS` fraction bits.
    fn row_start(&self, y_line: i128) -> Option<(i128, i128)> {
        let (_, reference_y) = self.reference;
        let rise = y_line
            .checked_mul(reference_y.d)?
            .checked_sub(reference_y.n)?;
        let along = |inverse: &Inverse| {
            floor_div(
                fixed(inverse.y.checked_mul(rise)?, ROW_BITS)?,
                inverse.den.checked_mul(reference_y.d)?,
            )
        };
        let u = fixed(self.corner.0, ROW_BITS)?.checked_add(along(&self.u)?)?;
        let w = along(&self.v)?.checked_neg()?;
        Some((u, w))
    }
}

impl Mapping for Transform {
    fn row(&mut self, y: i32, left: i32, right: i32) -> Option<(i32, i32)> {
        let y_line = i128::from(y)
            .checked_mul(self.pixel.1)?
            .checked_add(self.pixel.1 / 2)?;
        let (first, last) = self.span(y_line)?;
        let first = i32::try_from(first.max(i128::from(left))).ok()?;
        let last = i32::try_from(last.min(i128::from(right))).ok()?;
        if first > last {
            return None;
        }
        let columns = i128::from(first).checked_sub(self.anchor)?;
        let (u, w) = self.row_start(y_line)?;
        self.at = (
            u.checked_add(self.step.0.checked_mul(columns)?)?,
            w.checked_add(self.step.1.checked_mul(columns)?)?,
        );
        Some((first, last))
    }

    fn next(&mut self) -> (u32, u32) {
        let height = i128::from(self.height);
        let column = (self.at.0 >> ROW_BITS).clamp(self.left, self.right - 1);
        let row = (height - self.corner.1 + (self.at.1 >> ROW_BITS))
            .clamp(height - self.top, height - 1 - self.bottom);
        self.at.0 = self.at.0.saturating_add(self.step.0);
        self.at.1 = self.at.1.saturating_add(self.step.1);
        (
            u32::try_from(column).unwrap_or(0),
            u32::try_from(row).unwrap_or(0),
        )
    }
}

#[cfg(test)]
mod tests;
