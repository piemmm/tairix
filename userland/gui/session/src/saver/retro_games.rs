//! The retro games: a flight over a glowing wireframe grid towards a banded
//! sun setting between two wireframe mountain ranges, its light rippling on
//! the floor beneath it, where wireframe craft now and then come on to play
//! out retro arcade games: a spaceship passing, a flying saucer darting about,
//! tanks trading fire, riders walling each other in ([`cast`]).
//!
//! The sky, the sun and the mountains stand so far off that the flight never
//! moves them, so they are painted once and kept. A frame repaints only what
//! moves: the floor, where the grid streams towards the viewer as the flight
//! sways and the ripples run with it; the sun's lower part, where its bands
//! sink towards the horizon; and wherever a craft over the sky stood last
//! frame or stands now, where the sky and the mountains are laid back first.
//! The craft are drawn last of all, over everything. Under reduced motion the
//! scene holds still, nothing comes on, and nothing is drawn after the first
//! frame.

mod blast;
mod cast;
mod floor;
mod mountains;
mod riders;
mod saucer;
mod ship;
mod sky;
mod tanks;
mod under;
mod wire;

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};
use core::ops::{Add, Mul, Range, Sub};

use tairix_parallel::JobRunner;
use tairix_raster::{DitherRow, Pixel, RowBand, ScanScratch, SUBPIXEL};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::{fallible, mathf};
use tairix_wallpaper::RetroGamesOptions;
use tairix_wm::{Color, Compositor, Rect, Region, Scale, Surface, WindowId};

use super::seed_from;
use cast::Cast;
use floor::Floor;
use mountains::Mountains;
use sky::Sky;
use tairix_theme::motion::{seconds, SceneClock};
use under::Under;
use wire::{Camera, Display, Models, Stage};

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

/// The fewest pixels a band is worth handing to another core.
const MIN_BAND_PIXELS: usize = 32_768;

/// The parts and polygons a frame's craft have room for before they grow.
const STAGE_PARTS: usize = 1_024;
const DISPLAY_SHAPES: usize = 4_096;

/// What sets the craft's stream apart from the mountains' though both are
/// seeded from the one start instant.
const CAST_STREAM: u64 = 0xC0DE_CA57_5EED_0B0E;

/// The retro games screensaver.
pub(super) struct RetroGames {
    view: View,
    sky: Sky,
    mountains: Mountains,
    floor: Floor,
    cast: Cast,
    models: Models,
    stage: Stage,
    display: Display,
    /// The boxes the craft reach above the horizon this frame, and what lies
    /// under those they reached as last drawn.
    showing: Vec<Rect>,
    under: Under,
    /// A scan converter's scratch for each band a paint is split into.
    scratch: Vec<ScanScratch>,
    /// When the next frame is due, and how far the flight has run.
    clock: SceneClock,
    /// Grid cells the flight crosses a second.
    speed: f64,
    damage: Region,
}

impl RetroGames {
    /// The scene for a `size` screen at `scale` as `options` describe it,
    /// first drawn at `now_ns`, and still when `calm`; `None` for an empty
    /// screen or when the heap will not give it.
    pub(super) fn new(
        size: (u32, u32),
        scale: Scale,
        (calm, options): (bool, RetroGamesOptions),
        now_ns: u64,
    ) -> Option<Self> {
        let view = View::new(size, scale)?;
        let scatter = seed_from(now_ns);
        let (scratch, mut showing) = (Vec::new(), Vec::new());
        if !fallible::reserve(&mut showing, STAGE_PARTS) {
            return None;
        }
        let mountains = Mountains::new(&view, scatter)?;
        let sky = Sky::new(view, &mountains)?;
        let speed = FLIGHT_SPEED * f64::from(options.speed.percent()) / 100.0;
        Some(Self {
            view,
            sky,
            mountains,
            floor: Floor::new(view),
            cast: Cast::new(scatter ^ CAST_STREAM, speed, calm)?,
            models: Models::new()?,
            stage: Stage::new(&view, STAGE_PARTS)?,
            display: Display::new(&view, DISPLAY_SHAPES)?,
            showing,
            under: Under::new(),
            scratch,
            clock: SceneClock::new(now_ns, calm),
            speed,
            damage: Region::new(),
        })
    }

    /// When the next frame is due; never, while the scene holds still.
    pub(super) fn due_ns(&self) -> u64 {
        self.clock.due_ns().unwrap_or(u64::MAX)
    }

    /// Draw the whole scene onto `surface` as it stands when the flight
    /// begins, spreading the work across `runner`.
    pub(super) fn paint(&mut self, surface: &mut Surface, runner: &dyn JobRunner) {
        grow_scratch(&mut self.scratch, runner);
        let moment = Moment::default();
        self.stage_craft(moment);
        self.paint_backdrop(surface, runner, moment);
        self.under.keep(surface, &self.showing);
        draw_craft(surface, runner, &self.display, &mut self.scratch);
    }

    /// Fly on to `now_ns` and draw the frame, if one is due.
    pub(super) fn advance(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) {
        if !self.clock.frame_due(now_ns) {
            return;
        }
        let time = self.clock.advance(now_ns);
        let moment = Moment::at(time, self.speed);
        self.stage_craft(moment);
        self.damage.clear();
        self.damage.add(self.view.floor());
        self.damage.add(self.sky.zone());
        // What lies under the craft as last drawn is laid back by copying it,
        // unless the heap would not give it room: then the whole sky is
        // painted again.
        let lay_back = self.under.is_whole();
        if lay_back {
            for rect in self.under.boxes().iter().chain(&self.showing) {
                self.damage.add(*rect);
            }
        } else {
            self.damage.add(self.view.sky());
        }
        let size = (self.view.width, self.view.height);
        let runner = compositor.job_runner();
        grow_scratch(&mut self.scratch, runner);
        let kept = compositor.keeps_content(wm, size) && lay_back;
        let Self {
            sky,
            mountains,
            floor,
            display,
            showing,
            under,
            scratch,
            damage,
            ..
        } = self;
        let _ = compositor.repaint_window(wm, size, damage, |surface, _| {
            if kept {
                under.lay_back(surface);
            } else {
                sky.paint(surface, runner, moment);
                mountains.paint(surface, runner, scratch);
            }
            sky.paint_bands(surface, moment);
            floor.paint(surface, runner, moment);
            under.keep(surface, showing);
            draw_craft(surface, runner, display, scratch);
        });
    }

    /// Play the craft on to `moment` and draw them into the display.
    fn stage_craft(&mut self, moment: Moment) {
        let camera = Camera::at(&self.view, moment);
        self.cast.advance(moment, &camera, &self.models);
        self.stage.reset(camera);
        self.cast.stage(moment, &mut self.stage);
        self.stage
            .draw(&self.models, &mut self.display, &mut self.showing);
    }

    /// Paint every pixel of the scene but the craft as it stands at
    /// `moment`.
    fn paint_backdrop(&mut self, surface: &mut Surface, runner: &dyn JobRunner, moment: Moment) {
        self.sky.paint(surface, runner, moment);
        self.mountains.paint(surface, runner, &mut self.scratch);
        self.sky.paint_bands(surface, moment);
        self.floor.paint(surface, runner, moment);
    }
}

/// Draw the craft's display over `surface`, a band of rows at a time across
/// `runner`.
fn draw_craft(
    surface: &mut Surface,
    runner: &dyn JobRunner,
    display: &Display,
    scratch: &mut [ScanScratch],
) {
    let rows = display.rows();
    if rows.is_empty() {
        return;
    }
    draw_bands(surface, rows, runner, scratch, &|band, scratch| {
        let rows = band.rows();
        display.replay(band, &rows, scratch);
    });
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

    /// The sky's rectangle: every row above the horizon.
    const fn sky(&self) -> Rect {
        Rect::new(0, 0, self.width, self.horizon)
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
            travel: speed * seconds(SceneClock::FRAME_NS) * SHUTTER,
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

/// `light` as an opaque colour.
fn opaque(light: Rgb) -> Color {
    translucent(light, 1.0)
}

/// `light` as a colour `alpha` opaque.
fn translucent(light: Rgb, alpha: f64) -> Color {
    let pixel = light.pixel(0.5);
    let alpha = u8::try_from(mathf::round_i32(mathf::clamp(alpha, 0.0, 1.0) * 255.0)).unwrap_or(0);
    Color::rgba(pixel.r, pixel.g, pixel.b, alpha)
}

/// A point in pixels in the scan converter's sub-pixel units.
fn sub((x, y): (f64, f64)) -> (i32, i32) {
    let unit = f64::from(SUBPIXEL);
    (mathf::round_i32(x * unit), mathf::round_i32(y * unit))
}

/// The quad a line `half` pixels either side of the segment `from`–`to`
/// fills, in sub-pixels; `None` for a segment with no length.
fn band(from: (f64, f64), to: (f64, f64), half: f64) -> Option<[(i32, i32); 4]> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = mathf::hypot(dx, dy);
    if length <= f64::EPSILON {
        return None;
    }
    let (ox, oy) = (-dy / length * half, dx / length * half);
    Some([
        sub((from.0 + ox, from.1 + oy)),
        sub((from.0 - ox, from.1 - oy)),
        sub((to.0 - ox, to.1 - oy)),
        sub((to.0 + ox, to.1 + oy)),
    ])
}

/// A number drawn evenly from `least..most`.
fn between(rng: &mut NonCryptoRng, (least, most): (f64, f64)) -> f64 {
    least + (most - least) * rng.next_f64()
}

/// One of `count` choices, drawn evenly; the first when there is none.
fn pick(rng: &mut NonCryptoRng, count: usize) -> usize {
    let bound = u64::try_from(count).unwrap_or(u64::MAX);
    usize::try_from(rng.next_below(bound)).unwrap_or(0)
}

/// `angle` brought within half a turn either way of nought.
fn wrap(angle: f64) -> f64 {
    angle - TAU * mathf::floor((angle + PI) / TAU)
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
    paint_bands(
        surface,
        rows,
        runner,
        usize::MAX,
        |band| band,
        &|band: &mut RowBand<'_>| {
            for y in band.rows() {
                if let Some((_, span)) = band.row_span_mut(y, 0, width) {
                    row(y, span);
                }
            }
        },
    );
}

/// Give `scratch` a scan converter for each band `runner` splits a paint
/// into, once: a refusal keeps what it holds, and the paint splits no finer.
fn grow_scratch(scratch: &mut Vec<ScanScratch>, runner: &dyn JobRunner) {
    let wanted = tairix_parallel::bands(runner, usize::MAX, 1);
    if let Some(more) = wanted.checked_sub(scratch.len()) {
        if fallible::reserve(scratch, more) {
            scratch.resize_with(wanted, ScanScratch::new);
        }
    }
}

/// Paint rows `rows` of `surface` with `paint` a band of whole rows at a time
/// across `runner`, each band with a scratch of `scratch`'s own.
fn draw_bands(
    surface: &mut Surface,
    rows: Range<u32>,
    runner: &dyn JobRunner,
    scratch: &mut [ScanScratch],
    paint: &(dyn Fn(&mut RowBand<'_>, &mut ScanScratch) + Sync),
) {
    let most = scratch.len().max(1);
    let mut spare = scratch.iter_mut();
    paint_bands(
        surface,
        rows,
        runner,
        most,
        |band| (band, spare.next()),
        &|(band, scratch): &mut (RowBand<'_>, Option<&mut ScanScratch>)| match scratch {
            Some(scratch) => paint(band, scratch),
            None => paint(band, &mut ScanScratch::new()),
        },
    );
}

/// Paint rows `rows` of `surface` in at most `most` bands of whole rows,
/// spread across `runner`: `ready` makes each band what `paint` is handed.
fn paint_bands<'s, T: Send>(
    surface: &'s mut Surface,
    rows: Range<u32>,
    runner: &dyn JobRunner,
    most: usize,
    ready: impl FnMut(RowBand<'s>) -> T,
    paint: &(dyn Fn(&mut T) + Sync),
) {
    let count = usize::try_from(rows.end.saturating_sub(rows.start)).unwrap_or(0);
    let wide = usize::try_from(surface.width()).unwrap_or(1).max(1);
    let pieces = tairix_parallel::bands(runner, count, MIN_BAND_PIXELS.div_ceil(wide))
        .min(most)
        .max(1);
    let per_band = tairix_raster::band_rows(count, pieces);
    tairix_parallel::for_each_drawn(
        runner,
        surface.row_bands_mut(rows, per_band).map(ready),
        &|mut piece| paint(&mut piece),
    );
}

#[cfg(test)]
#[path = "retro_games_tests.rs"]
mod tests;
