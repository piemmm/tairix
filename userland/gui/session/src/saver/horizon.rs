//! The retro horizon: a flight over a glowing wireframe grid towards a banded
//! sun setting between two wireframe mountain ranges, its light rippling on
//! the floor beneath it.
//!
//! The sky, the sun and the mountains stand so far off that the flight never
//! moves them, so they are painted once and kept. A frame repaints only what
//! moves: the floor, where the grid streams towards the viewer as the flight
//! sways and the ripples run with it, and the sun's lower part, where its
//! bands sink towards the horizon. Under reduced motion the scene holds still
//! and nothing is drawn after the first frame.

mod floor;
mod mountains;
mod sky;

use core::f64::consts::TAU;
use core::ops::{Add, Mul, Range, Sub};

use tairix_inline::ArrayVec;
use tairix_parallel::JobRunner;
use tairix_raster::{DitherRow, Pixel, RowBand};
use tairix_util::mathf;
use tairix_wallpaper::HorizonOptions;
use tairix_wm::{Compositor, Rect, Region, Scale, Surface, WindowId};

use super::{seconds, seed_from, MAX_STEP_FRAMES, SAVER_FRAME_NS};
use floor::Floor;
use mountains::Mountains;
use sky::Sky;

/// How far down the screen the horizon lies, in thousandths of its height.
const HORIZON: u64 = 575;

/// Pixels a unit of lateral offset spans at unit depth, per pixel of the
/// screen's height.
const FOCAL: f64 = 1.0;

/// The camera's height over the floor, in grid cells.
const CAMERA: f64 = 1.0;

/// The sun's radius as a share of the screen's height, and the most of the
/// screen's width its disc may span.
const SUN_RADIUS: f64 = 0.245;
const SUN_WIDEST: f64 = 0.6;

/// How far above the horizon the sun's centre stands, in radii: the lowest
/// part of the disc has already set.
const SUN_LIFT: f64 = 0.66;

/// Grid cells the flight crosses a second at its own pace.
const FLIGHT_SPEED: f64 = 0.8;

/// The share of a frame's interval its exposure lasts: a moving line is
/// drawn across the ground it covered in that time, so the flight reads as
/// continuous at the pace frames are drawn.
const SHUTTER: f64 = 0.5;

/// How far the flight sways either side of the grid's centre line, in cells,
/// and how long one sway takes, in seconds.
const SWAY: f64 = 0.3;
const SWAY_PERIOD: f64 = 53.0;

/// The most bands a paint splits its rows into, and the fewest pixels one is
/// worth handing to another core.
const MAX_BANDS: usize = 64;
const MIN_BAND_PIXELS: usize = 32_768;

/// The retro horizon screensaver.
pub(super) struct Horizon {
    view: View,
    sky: Sky,
    mountains: Mountains,
    floor: Floor,
    /// When the last frame was drawn, and the seconds the flight has run by
    /// it; `None` when the flight holds still.
    flying: Option<(u64, f64)>,
    /// Grid cells the flight crosses a second.
    speed: f64,
    due_ns: u64,
    damage: Region,
}

impl Horizon {
    /// The scene for a `size` screen at `scale` as `options` describe it,
    /// first drawn at `now_ns`, and still when `calm`; `None` for an empty
    /// screen or when the heap will not give it.
    pub(super) fn new(
        size: (u32, u32),
        scale: Scale,
        (calm, options): (bool, HorizonOptions),
        now_ns: u64,
    ) -> Option<Self> {
        let view = View::new(size, scale)?;
        let mountains = Mountains::new(&view, seed_from(now_ns))?;
        let sky = Sky::new(view, &mountains)?;
        Some(Self {
            view,
            sky,
            mountains,
            floor: Floor::new(view),
            flying: (!calm).then_some((now_ns, 0.0)),
            speed: FLIGHT_SPEED * f64::from(options.speed.percent()) / 100.0,
            due_ns: if calm {
                u64::MAX
            } else {
                now_ns.saturating_add(SAVER_FRAME_NS)
            },
            damage: Region::new(),
        })
    }

    /// When the next frame is due; never, while the scene holds still.
    pub(super) const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Draw the whole scene onto `surface` as it stands when the flight
    /// begins, spreading the work across `runner`.
    pub(super) fn paint(&self, surface: &mut Surface, runner: &dyn JobRunner) {
        let moment = Moment::default();
        self.sky.paint(surface, runner, moment);
        self.mountains.paint(surface, runner);
        self.sky.paint_bands(surface, moment);
        self.floor.paint(surface, runner, moment);
    }

    /// Fly on to `now_ns` and draw the frame, if one is due.
    pub(super) fn advance(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) {
        if now_ns < self.due_ns {
            return;
        }
        let Some((last_ns, flown)) = self.flying else {
            self.due_ns = u64::MAX;
            return;
        };
        let step = now_ns
            .saturating_sub(last_ns)
            .min(MAX_STEP_FRAMES * SAVER_FRAME_NS);
        let time = flown + seconds(step);
        self.flying = Some((now_ns, time));
        self.due_ns = now_ns.saturating_add(SAVER_FRAME_NS);
        let moment = Moment::at(time, self.speed);
        let size = (self.view.width, self.view.height);
        self.damage.clear();
        self.damage.add(self.view.floor());
        self.damage.add(self.sky.zone());
        let runner = compositor.job_runner();
        let kept = compositor.keeps_content(wm, size);
        let Self {
            sky,
            mountains,
            floor,
            damage,
            ..
        } = self;
        let _ = compositor.repaint_window(wm, size, damage, |surface, _| {
            if !kept {
                sky.paint(surface, runner, moment);
                mountains.paint(surface, runner);
            }
            sky.paint_bands(surface, moment);
            floor.paint(surface, runner, moment);
        });
    }
}

/// Where the scene stands on a screen.
#[derive(Copy, Clone, Debug)]
struct View {
    width: u32,
    height: u32,
    /// The first floor row: the horizon runs along its top edge.
    horizon: u32,
    /// The column the grid's lines run towards.
    centre: f64,
    /// Pixels a unit of lateral offset spans at unit depth.
    focal: f64,
    /// The sun's centre row and its radius, in pixels.
    sun: (f64, f64),
    /// Physical pixels per logical one.
    pixel: f64,
}

impl View {
    /// The view of a `(width, height)` screen at `scale`, or `None` for one
    /// with no floor or no sky.
    fn new((width, height): (u32, u32), scale: Scale) -> Option<Self> {
        let horizon = u32::try_from(u64::from(height) * HORIZON / 1000).ok()?;
        if width == 0 || horizon == 0 || horizon >= height {
            return None;
        }
        let tall = f64::from(height);
        let radius = mathf::fmin(SUN_RADIUS * tall, SUN_WIDEST * f64::from(width) / 2.0);
        Some(Self {
            width,
            height,
            horizon,
            centre: f64::from(width) / 2.0,
            focal: FOCAL * tall,
            sun: (f64::from(horizon) - SUN_LIFT * radius, radius),
            pixel: f64::from(scale.scale_length(1_000)) / 1_000.0,
        })
    }

    /// The floor's rows.
    const fn floor_rows(&self) -> Range<u32> {
        self.horizon..self.height
    }

    /// The floor's rectangle.
    fn floor(&self) -> Rect {
        Rect::new(
            0,
            i32::try_from(self.horizon).unwrap_or(i32::MAX),
            self.width,
            self.height - self.horizon,
        )
    }

    /// How far ahead the floor lies `drop` pixels below the horizon, in
    /// cells; the map is its own inverse, so it is also how far below the
    /// horizon the floor `drop` cells ahead lies, in pixels.
    fn depth(&self, drop: f64) -> f64 {
        CAMERA * self.focal / drop
    }
}

/// The scene as it stands at one instant of the flight.
#[derive(Copy, Clone, Debug, Default)]
struct Moment {
    /// Seconds the flight has run.
    time: f64,
    /// Cells flown, and flown over the frame's exposure.
    flown: f64,
    travel: f64,
    /// How far the flight has swayed off the grid's centre line, in cells.
    sway: f64,
}

impl Moment {
    /// The flight `time` seconds in, at `speed` cells a second.
    fn at(time: f64, speed: f64) -> Self {
        Self {
            time,
            flown: speed * time,
            travel: speed * seconds(SAVER_FRAME_NS) * SHUTTER,
            sway: SWAY * mathf::sin(TAU * time / SWAY_PERIOD),
        }
    }
}

/// Light in display levels, each channel counted in `0.0..=255.0` and able to
/// add up past it; only a pixel is clamped.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

impl Rgb {
    const fn new(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b }
    }

    /// This light moved `share` of the way towards `to`.
    fn mix(self, to: Self, share: f64) -> Self {
        self + (to - self) * share
    }

    /// The opaque pixel this light rounds to at `bias`, a share of a level.
    fn pixel(self, bias: f64) -> Pixel {
        Pixel {
            r: level(self.r + bias),
            g: level(self.g + bias),
            b: level(self.b + bias),
            a: u8::MAX,
        }
    }
}

impl Add for Rgb {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.r + other.r, self.g + other.g, self.b + other.b)
    }
}

impl Sub for Rgb {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.r - other.r, self.g - other.g, self.b - other.b)
    }
}

impl Mul<f64> for Rgb {
    type Output = Self;

    fn mul(self, k: f64) -> Self {
        Self::new(self.r * k, self.g * k, self.b * k)
    }
}

/// A channel's value rounded down to the level it reaches.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into 0.0..=255.0 first, and a NaN clamps to 0.0"
)]
fn level(value: f64) -> u8 {
    mathf::clamp(value, 0.0, 255.0) as u8
}

/// A pixel coordinate held to `0..=most` and rounded to the nearest whole
/// column or row.
fn column(at: f64, most: u32) -> u32 {
    u32::try_from(mathf::round_i32(mathf::clamp(at, 0.0, f64::from(most)))).unwrap_or(0)
}

/// A column as an index into a row's pixels or its dither biases.
fn index(column: u32) -> usize {
    usize::try_from(column).unwrap_or(usize::MAX)
}

/// The ordered dither's rounding bias for each of surface row `y`'s eight
/// column phases, as a share of a level.
fn dither_biases(y: u32) -> [f64; 8] {
    let row = DitherRow::at(y);
    core::array::from_fn(|x| f64::from(row.bias(u32::try_from(x).unwrap_or(0))) / 256.0)
}

/// Fill `span`, whose first pixel is surface column `first`, with `light`
/// rounded at `biases`.
fn fill_dithered(span: &mut [Pixel], first: u32, light: Rgb, biases: &[f64; 8]) {
    let offset = index(first & 7);
    let tile: [Pixel; 8] = core::array::from_fn(|at| light.pixel(biases[(offset + at) & 7]));
    let (whole, rest) = span.as_chunks_mut::<8>();
    for chunk in whole {
        *chunk = tile;
    }
    rest.copy_from_slice(&tile[..rest.len()]);
}

/// How much of `from..to` a train of pulses `width` wide covers, as a share
/// of it: one pulse centred on each integer, `width` a share of their unit
/// spacing. The exact area, so a pulse far narrower than the interval counts
/// for just the part it fills.
fn covered(from: f64, to: f64, width: f64) -> f64 {
    let span = to - from;
    if span <= 0.0 {
        return 0.0;
    }
    let base = mathf::floor(from);
    (cumulative(to - base, width) - cumulative(from - base, width)) / span
}

/// [`covered`], averaged over the train moving on `travel` in the time it is
/// seen: `from..to` as it stood `travel` ago through as it stands now.
fn swept(from: f64, to: f64, width: f64, travel: f64) -> f64 {
    let span = to - from;
    if travel <= span * 1e-6 {
        return covered(from, to, width);
    }
    if span <= 0.0 {
        return 0.0;
    }
    let base = mathf::floor(from - travel);
    let q = |at: f64| second_cumulative(at - base, width);
    (q(to) - q(to - travel) - q(from) + q(from - travel)) / (span * travel)
}

/// How much of `0..at` the pulses cover, for `at` at or above zero.
fn cumulative(at: f64, width: f64) -> f64 {
    let whole = mathf::floor(at);
    let part = at - whole;
    let half = width / 2.0;
    whole * width + mathf::fmin(part, half) + mathf::fmax(part - (1.0 - half), 0.0)
}

/// The integral of [`cumulative`] over `0..at`, for `at` at or above zero.
fn second_cumulative(at: f64, width: f64) -> f64 {
    let whole = mathf::floor(at);
    let part = at - whole;
    let half = width / 2.0;
    let rising = if part <= half {
        part * part / 2.0
    } else {
        half * half / 2.0 + half * (part - half)
    };
    let closing = mathf::fmax(part - (1.0 - half), 0.0);
    width * whole * whole / 2.0 + whole * width * part + rising + closing * closing / 2.0
}

/// `value` moved from `mean` by `contrast` of its distance: a pattern whose
/// contrast gives way to its mean as it crowds too finely to be drawn.
fn faded(value: f64, mean: f64, contrast: f64) -> f64 {
    mean + (value - mean) * contrast
}

/// Paint rows `rows` of `surface`, whole, with `row`, spread across `runner`:
/// each row's index and its pixels.
fn paint_rows(
    surface: &mut Surface,
    rows: Range<u32>,
    runner: &dyn JobRunner,
    row: &(dyn Fn(u32, &mut [Pixel]) + Sync),
) {
    let width = surface.width();
    paint_bands(surface, rows, runner, &|band| {
        for y in band.rows() {
            if let Some((_, span)) = band.row_span_mut(y, 0, width) {
                row(y, span);
            }
        }
    });
}

/// Paint rows `rows` of `surface` with `paint`, a band of whole rows at a
/// time, spread across `runner`.
fn paint_bands(
    surface: &mut Surface,
    rows: Range<u32>,
    runner: &dyn JobRunner,
    paint: &(dyn Fn(&mut RowBand<'_>) + Sync),
) {
    let count = usize::try_from(rows.end.saturating_sub(rows.start)).unwrap_or(0);
    let wide = usize::try_from(surface.width()).unwrap_or(1).max(1);
    let pieces =
        tairix_parallel::bands(runner, count, MIN_BAND_PIXELS.div_ceil(wide)).clamp(1, MAX_BANDS);
    let per_band = u32::try_from(count.div_ceil(pieces))
        .unwrap_or(u32::MAX)
        .max(1);
    let mut bands: ArrayVec<RowBand<'_>, MAX_BANDS> = ArrayVec::new();
    for band in surface.row_bands_mut(rows, per_band) {
        // The split never makes more bands than it was asked for; one that
        // somehow did is still painted, on this thread.
        if let Err(refused) = bands.try_push(band) {
            paint(&mut refused.into_value());
        }
    }
    tairix_parallel::for_each(runner, &mut bands, paint);
}

#[cfg(test)]
#[path = "horizon_tests.rs"]
mod tests;
