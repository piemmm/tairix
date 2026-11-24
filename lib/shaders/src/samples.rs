//! A known input for every pinned pass, so a device can be held to the
//! software device on the same work (`plans/GPU.md` GP5.4), built by the
//! pass's description type. A pass laying over a surface lays over a `WIDE` ×
//! `HIGH` one of premultiplied pixels, mostly within the rectangle `RECT`.

use alloc::vec::Vec;

use tairix_raster::{
    Caster, Color, Coverage, DropShadow, FillRule, Pixel, Region, ResamplePlan, Ring, RingInk,
    RoundRect, ScanScratch, SUBPIXEL,
};
use tairix_util::fallible;

use crate::chrome::body::{Layout, Strip};
use crate::{chrome, compose, frost, groups, indirect, paint, Refused, Sample};

const WIDE: u32 = 24;
const HIGH: u32 = 20;

/// `(left, top, width, height)`.
const RECT: (u32, u32, u32, u32) = (3, 2, 17, 13);

fn words(count: u32, word: impl Fn(u32) -> u32) -> Result<Vec<u32>, Refused> {
    fallible::collected(count as usize, (0..count).map(word)).ok_or(Refused::OutOfMemory)
}

fn copied(words: &[u32]) -> Result<Vec<u32>, Refused> {
    fallible::collected(words.len(), words.iter().copied()).ok_or(Refused::OutOfMemory)
}

fn zeros(count: u32) -> Result<Vec<u32>, Refused> {
    words(count, |_| 0)
}

/// Premultiplied pixels, red in each word's low byte, no two neighbours
/// alike.
fn pixels(count: u32) -> Result<Vec<u32>, Refused> {
    words(count, |at| {
        let alpha = at.wrapping_mul(37).wrapping_add(11) % 256;
        let channel = |salt: u32| (at.wrapping_mul(salt).wrapping_add(5) % 256) * alpha / 255;
        channel(13) | channel(29) << 8 | channel(53) << 16 | alpha << 24
    })
}

/// A plane of weights, one a word in its low byte.
fn weights(count: u32) -> Result<Vec<u32>, Refused> {
    words(count, |at| at.wrapping_mul(71).wrapping_add(3) % 256)
}

fn signed(value: u32) -> Result<i32, Refused> {
    i32::try_from(value).map_err(|_| Refused::Extent)
}

fn sample<const N: usize>(
    buffers: [Vec<u32>; N],
    push: &[u32],
    groups: [u32; 3],
) -> Result<Sample, Refused> {
    Ok(Sample {
        buffers: fallible::collected(N, buffers.into_iter()).ok_or(Refused::OutOfMemory)?,
        push: copied(push)?,
        groups,
    })
}

/// The paint passes' push over `RECT`.
const fn painted(dithered: bool) -> paint::Push {
    let (left, top, width, height) = RECT;
    paint::Push {
        stride: WIDE,
        left,
        top,
        width,
        height,
        dithered,
    }
}

/// The chrome passes' push over the whole surface.
const FRAME: chrome::Push = chrome::Push {
    stride: WIDE,
    left: 0,
    top: 0,
    width: WIDE,
    height: HIGH,
    dithered: true,
};

pub(crate) fn frost_reduce() -> Result<Sample, Refused> {
    let (left, top, width, height) = RECT;
    let push = frost::reduce::Push {
        stride: WIDE,
        left,
        top,
        width,
        height,
    };
    let (across, down) = push.cells();
    sample(
        [pixels(WIDE * HIGH)?, zeros(across * down)?],
        &push.words(),
        groups(across, down),
    )
}

pub(crate) fn frost_blur() -> Result<Sample, Refused> {
    let (across, down) = (9, 7);
    let push = frost::blur::Push {
        across,
        down,
        reach: 3,
        vertical: true,
    };
    sample(
        [pixels(across * down)?, zeros(across * down)?],
        &push.words(),
        groups(across, down),
    )
}

pub(crate) fn frost_spread() -> Result<Sample, Refused> {
    let (left, top, width, height) = RECT;
    let push = frost::spread::Push {
        stride: WIDE,
        left,
        top,
        width,
        height,
    };
    sample(
        [
            pixels(width.div_ceil(2) * height.div_ceil(2))?,
            pixels(WIDE * HIGH)?,
        ],
        &push.words(),
        groups(width, height),
    )
}

pub(crate) fn frost_mix() -> Result<Sample, Refused> {
    let (left, top, width, height) = RECT;
    let push = frost::mix::Push {
        stride: WIDE,
        left,
        top,
        width,
        height,
        blur_stride: width,
        blur_left: 0,
        blur_top: 0,
        coverage_stride: width,
        dithered: true,
    };
    // Coverage is a byte a pixel, four to a word, so any word holds four.
    let coverage = words((width * height).div_ceil(4), |at| {
        at.wrapping_mul(0x9e37_79b9)
    })?;
    sample(
        [pixels(WIDE * HIGH)?, pixels(width * height)?, coverage],
        &push.words(),
        groups(width, height),
    )
}

pub(crate) fn over() -> Result<Sample, Refused> {
    let (left, top, width, height) = RECT;
    let push = compose::over::Push {
        stride: WIDE,
        left,
        top,
        source_stride: 20,
        source_left: 1,
        source_top: 2,
        width,
        height,
        factor: 200,
        dithered: true,
    };
    sample(
        [pixels(WIDE * HIGH)?, pixels(20 * 16)?],
        &push.words(),
        groups(width, height),
    )
}

pub(crate) fn copy() -> Result<Sample, Refused> {
    let (left, top, width, height) = RECT;
    let push = compose::copy::Push {
        stride: WIDE,
        left,
        top,
        source_stride: 20,
        source_left: 2,
        source_top: 1,
        width,
        height,
    };
    sample(
        [pixels(WIDE * HIGH)?, pixels(20 * 16)?],
        &push.words(),
        groups(width, height),
    )
}

pub(crate) fn fill() -> Result<Sample, Refused> {
    let (left, top, width, height) = RECT;
    let push = compose::fill::Push {
        stride: WIDE,
        left,
        top,
        width,
        height,
        pixel: 0x8040_2010,
    };
    sample([pixels(WIDE * HIGH)?], &push.words(), groups(width, height))
}

pub(crate) fn encode() -> Result<Sample, Refused> {
    let push = compose::encode::Push {
        stride: WIDE,
        left: 0,
        top: 0,
        source_stride: WIDE,
        source_left: 0,
        source_top: 0,
        width: WIDE,
        height: HIGH,
        // Blue, green, red, alpha: a BGRA display's order.
        order: u32::from_le_bytes([2, 1, 0, 3]),
    };
    sample(
        [zeros(WIDE * HIGH)?, pixels(WIDE * HIGH)?],
        &push.words(),
        groups(WIDE, HIGH),
    )
}

pub(crate) fn resample() -> Result<Sample, Refused> {
    let window = Region {
        x: 2,
        y: 1,
        width: 19,
        height: 15,
    };
    let source = (12, 9);
    let plan = ResamplePlan::new(source, (WIDE, HIGH), window).map_err(|_| Refused::Extent)?;
    let push = compose::resample::Push::new(&plan, window, (WIDE, source.0))?;
    sample(
        [
            pixels(WIDE * HIGH)?,
            pixels(source.0 * source.1)?,
            compose::resample::taps(&plan)?,
        ],
        &push.words(),
        groups(window.width, window.height),
    )
}

pub(crate) fn body() -> Result<Sample, Refused> {
    // A 20 × 17 window: a 16 × 12 client inside a top band, a bottom band and
    // two side strips, its pixels drawn short of its extent and cut round.
    let strip = |x, y, width, height, base| Strip {
        x,
        y,
        width,
        height,
        base,
        stride: width,
    };
    let layout = Layout {
        origin: (2, 1),
        opacity: 230,
        client: strip(2, 3, 16, 12, 0),
        drawn: (14, 11),
        backdrop: Some(Pixel {
            r: 40,
            g: 50,
            b: 60,
            a: 200,
        }),
        cut: Some((
            RoundRect {
                width: 18,
                height: 15,
                radius: 3,
            },
            (1, 2),
        )),
        furniture: [
            strip(0, 0, 20, 3, 0),
            strip(0, 15, 20, 2, 60),
            strip(0, 3, 2, 12, 100),
            strip(18, 3, 2, 12, 124),
        ],
    };
    sample(
        [
            pixels(WIDE * HIGH)?,
            pixels(16 * 12)?,
            pixels(148)?,
            copied(&layout.words()?)?,
        ],
        &FRAME.words(),
        groups(WIDE, HIGH),
    )
}

pub(crate) fn shadow() -> Result<Sample, Refused> {
    let silhouette = RoundRect {
        width: 14,
        height: 11,
        radius: 4,
    };
    let level = |reach, alpha| {
        let mut level = DropShadow::new(reach, Color::rgba(8, 10, 14, alpha));
        level.ensure_tile(silhouette.corner_reach());
        level
    };
    let tables = chrome::shadow::Tables::new(&level(6, 160), &level(3, 96))?;
    let caster = Caster {
        left: 4,
        top: 3,
        right: 18,
        bottom: 14,
        silhouette: Some(silhouette),
        opacity: 220,
    };
    sample(
        [pixels(WIDE * HIGH)?, copied(tables.words())?],
        &tables.push(&FRAME, &caster, 128)?,
        groups(WIDE, HIGH),
    )
}

pub(crate) fn plate() -> Result<Sample, Refused> {
    let (x, y, width, height) = RECT;
    let plate = paint::plate::Plate {
        x,
        y,
        width,
        height,
        radius: 5,
        color: Color::rgba(200, 120, 40, 180),
        replace: false,
    };
    sample(
        [pixels(WIDE * HIGH)?],
        &plate.push(&painted(true))?,
        groups(width, height),
    )
}

pub(crate) fn wash() -> Result<Sample, Refused> {
    let (x, y, width, height) = RECT;
    let wash = paint::wash::Wash {
        x,
        y,
        width,
        height,
        top: Color::rgba(250, 200, 10, 255),
        bottom: Color::rgba(10, 40, 220, 120),
        planes: [
            Some(paint::wash::Plane {
                base: 0,
                stride: width,
            }),
            None,
        ],
    };
    sample(
        [pixels(WIDE * HIGH)?, weights(width * height)?],
        &wash.push(&painted(true))?,
        groups(width, height),
    )
}

pub(crate) fn ring() -> Result<Sample, Refused> {
    let (x, y, width, height) = RECT;
    let placed = paint::ring::Placed {
        x,
        y,
        width,
        height,
        ring: Ring {
            top_radius: 5,
            bottom_radius: 3,
            thickness: 2,
        },
        laid: paint::ring::Laid::Washed(RingInk::Bevel {
            light: Color::rgba(250, 250, 250, 200),
            shade: Color::rgba(20, 20, 30, 160),
        }),
    };
    sample(
        [pixels(WIDE * HIGH)?],
        &placed.push(&painted(true))?,
        groups(width, height),
    )
}

pub(crate) fn coverage() -> Result<Sample, Refused> {
    let unit = SUBPIXEL;
    let triangle = [
        (3 * unit, 2 * unit),
        (21 * unit + 3, 6 * unit),
        (9 * unit, 18 * unit + 5),
    ];
    let mut scratch = ScanScratch::new();
    let rows = scratch
        .coverage_rows(
            &[triangle],
            SUBPIXEL.unsigned_abs(),
            FillRule::EvenOdd,
            Coverage::Area,
        )
        .ok_or(Refused::Extent)?;
    let bounds = rows.bounds((0, 0, WIDE, HIGH)).ok_or(Refused::Extent)?;
    let shape = paint::coverage::Shape::new(&rows, bounds)?;
    let (_, _, width, height) = shape.window();
    sample(
        [zeros(width * height)?, copied(shape.words())?],
        &shape.push(),
        groups(width, height),
    )
}

pub(crate) fn mask() -> Result<Sample, Refused> {
    let (x, y, width, height) = RECT;
    let mask = paint::mask::Mask {
        x: signed(x)?,
        y: signed(y)?,
        width,
        height,
        base: 0,
        stride: width,
        ink: Color::rgba(30, 220, 90, 230),
    };
    sample(
        [pixels(WIDE * HIGH)?, weights(width * height)?],
        &mask.push(&painted(false))?,
        groups(width, height),
    )
}

pub(crate) fn picture() -> Result<Sample, Refused> {
    let (x, y, width, height) = RECT;
    let picture = paint::picture::Picture {
        x: signed(x)?,
        y: signed(y)?,
        width,
        height,
        base: 0,
        stride: width,
        laid: paint::picture::Laid::Rounded(4),
    };
    sample(
        [pixels(WIDE * HIGH)?, pixels(width * height)?],
        &picture.push(&painted(false))?,
        groups(width, height),
    )
}

pub(crate) fn indirect_counts() -> Result<Sample, Refused> {
    use indirect::counts;

    // Two slots, the second past the most along its first axis.
    let held = [5, 6, 7, 0, 65, 1, 1, 0];
    let push = counts::Push {
        slot: 1,
        most: [64, 64, 64],
    };
    sample([copied(&held)?, zeros(2)?], &push.words(), [1, 1, 1])
}

pub(crate) fn indirect_records() -> Result<Sample, Refused> {
    use indirect::records;

    // Two indexed records after a word not theirs, the second naming a first
    // instance: it alone is zeroed.
    let held = [9, 3, 1, 0, 0, 0, 6, 2, 4, 1, 7];
    let push = records::Push {
        slot: 1,
        first: 1,
        count: 2,
        indexed: true,
    };
    sample([copied(&held)?, zeros(2)?], &push.words(), [1, 1, 1])
}
