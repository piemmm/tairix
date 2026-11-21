//! Bubble Universe, a TAIRiX screensaver: a disc of light woven from two
//! hundred curves of the Bubble Universe map, each two hundred points long,
//! rings and spirals flowing through it as its time runs.
//!
//! Every point is drawn as a soft dot whose light adds to its neighbours', so
//! where the curves crowd the light gathers and whitens, and a halo and a wide
//! haze glow about it; the sum is toned, dithered and laid opaque. A frame is
//! exposed over half its own time at four moments, so the curves flow at
//! thirty frames a second rather than jumping. A slow drift of the map's other
//! phase keeps the picture from repeating, the whole turns once in seven
//! minutes, and the bubble wanders a little about the screen so no pixel holds
//! one light for long. Under reduced motion each picture is still, and another
//! takes its place, elsewhere, each minute.
//!
//! The points are traced a gang of eight curves to the lanes of a vector at a
//! time, across every core the saver is granted; they are sorted by the
//! strips of rows they light, and each band of rows draws and tones its own
//! strips, so the work follows the bubble rather than the screen and a frame
//! repaints only the square about it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod glow;
mod light;
mod orbit;
mod palette;
mod tone;

use alloc::vec::Vec;
use core::array::from_fn;
use core::f64::consts::TAU;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::Errno;
use tairix_geometry::{Rect, Region};
use tairix_parallel::{for_each_drawn, JobRunner, Pool};
use tairix_raster::Surface;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_saver::{erase, Frame, Host, NextFrame, Scene, Setup};
use tairix_theme::motion::{seconds, SceneClock};
use tairix_util::{fallible, mathf};

use glow::{Glow, GLOW_CELLS};
use light::{Bins, Dot, Light, Placed, Spot, SUB};
use orbit::{Curves, Gang, Lanes, Moment, CURVES, GANGS, GANG_POINTS, LANES, REACH, STEPS};
use palette::Palette;
use tone::{Tone, LIGHT_ONE};

/// The bubble's radius, as a share of the screen's shorter side.
const BUBBLE: f64 = 0.44;

/// The bubble's radius on a screen 1080 rows tall, where a dot covers a pixel
/// either way: the size every other is drawn to look like.
const REFERENCE_RADIUS: f64 = BUBBLE * 1080.0;

/// The light a point's dot sheds in all on that screen, counted in pixels lit
/// at its colour's full strength.
const POINT_LIGHT: f64 = 2.0;

/// How strongly the tone curve shows light: `1 − e^(−EXPOSURE · light)`.
const EXPOSURE: f64 = 1.0;

/// The moments a frame is exposed at, and the share of its time they span.
const EXPOSURES: usize = 4;
const SHUTTER: f64 = 0.5;

/// The spots one moment traces.
const MOMENT_SPOTS: usize = CURVES * STEPS;

/// The map's time, its drift and the whole picture's turn, in radians a
/// second at normal pace.
const TIME_RATE: f64 = 0.3;
const DRIFT_RATE: f64 = TAU / 150.0;
const TURN_RATE: f64 = TAU / 420.0;

/// The seconds the bubble takes to wander across and back, and down and back,
/// and how far it goes either way, in its radii: against burn-in, too slowly
/// to see.
const WANDER_S: (f64, f64) = (173.0, 131.0);
const WANDER: f64 = 0.08;

/// The share of the screen the bubble keeps clear of each edge as it wanders.
const MARGIN: f64 = 0.02;

/// Glow cells across the bubble's radius.
const GLOW_CELLS_PER_RADIUS: f64 = 60.0;

/// Under reduced motion: how long each still picture is held, and how far on
/// in the picture's own seconds the next is set.
const STILL_NS: u64 = 60_000_000_000;
const STILL_STRIDE_S: f64 = 41.0;

/// The Bubble Universe screensaver.
pub struct BubbleUniverse {
    universe: Universe,
    /// The cores a frame is traced and drawn across.
    runner: Pool,
}

impl BubbleUniverse {
    /// The universe `setup` describes.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] or [`Errno::OutOfRange`] for an option the host
    /// did not send as the manifest declares it, [`Errno::OutOfMemory`] when
    /// the heap will not hold the picture.
    pub fn new(setup: &Setup<'_>) -> Result<Self, Errno> {
        let runner = setup.pool();
        Ok(Self {
            universe: Universe::new(setup, &runner)?,
            runner,
        })
    }
}

impl Scene for BubbleUniverse {
    /// Carry the universe on to the frame's instant and draw it about the
    /// bubble, clearing what the last frame painted beyond it.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        _host: &mut Host<'_>,
    ) -> NextFrame {
        self.universe
            .frame(surface, damage, frame.now_ns, &self.runner)
    }
}

/// The picture and everything a frame of it is drawn with.
struct Universe {
    curves: Curves,
    /// Every point's light, gang by gang in the order a gang traces them.
    colours: Vec<[u16; 3]>,
    /// The spots of every moment of a frame, gang by gang.
    spots: Vec<Spot>,
    light: Light,
    pace: Pace,
    course: Course,
    /// What the last frame painted, cleared where this one no longer reaches.
    painted: Rect,
    uncovered: Region,
    refresh: RefreshRate,
}

impl Universe {
    /// The universe `setup` describes, drawn in as many bands as `runner`
    /// takes.
    fn new(setup: &Setup<'_>, runner: &dyn JobRunner) -> Result<Self, Errno> {
        let palette =
            Palette::from_word(setup.choice("colours")?.as_str()).ok_or(Errno::OutOfRange)?;
        let pace = f64::from(setup.pace("speed")?) / 100.0;
        let glowing = setup.switch("glow")?;
        let start = setup.start;
        let screen = Rect::new(0, 0, start.region.width(), start.region.height());
        let radius = BUBBLE * f64::from(screen.width.min(screen.height));
        let scale = radius / REFERENCE_RADIUS;
        let dot = Dot::new(u32::try_from(mathf::round_i32(scale).max(1)).unwrap_or(1));
        let cell =
            u32::try_from(mathf::round_i32(radius / GLOW_CELLS_PER_RADIUS).max(2)).unwrap_or(2);
        let course = Course::new(
            (screen, radius),
            (dot, glowing.then_some(cell)),
            pace,
            setup.seed,
        );
        let side = course.side;
        // The glow is gathered from one moment's points, standing for every
        // moment's.
        let glow = if glowing {
            let energy = dot.energy() * u64::try_from(EXPOSURES).unwrap_or(1);
            Some(Glow::new((side, side), cell, energy, runner).ok_or(Errno::OutOfMemory)?)
        } else {
            None
        };
        let tone = Tone::new(EXPOSURE).ok_or(Errno::OutOfMemory)?;
        let bins = Bins::new(EXPOSURES * GANGS, GANG_POINTS, screen.height, dot)
            .ok_or(Errno::OutOfMemory)?;
        let light =
            Light::new((side, side), bins, (tone, glow), runner).ok_or(Errno::OutOfMemory)?;
        // A dot sheds the light its screen's size is worth, however its
        // radius was rounded, shared across the frame's moments.
        let rho = f64::from(dot.radius());
        let peak = POINT_LIGHT * scale * scale / (rho * rho * rho * rho * mathf::real(EXPOSURES));
        Ok(Self {
            curves: Curves::new(),
            colours: colours(palette, peak).ok_or(Errno::OutOfMemory)?,
            spots: fallible::filled(EXPOSURES * MOMENT_SPOTS, Spot::default())
                .ok_or(Errno::OutOfMemory)?,
            light,
            pace: Pace::new(start.now_ns, start.reduced_motion, start.refresh),
            course,
            painted: Rect::EMPTY,
            uncovered: Region::new(),
            refresh: start.refresh,
        })
    }

    /// Draw the frame at `now_ns` into `surface` across `runner`, adding what
    /// it changed to `damage`; answers when the next is wanted.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        now_ns: u64,
        runner: &dyn JobRunner,
    ) -> NextFrame {
        let Some(seconds) = self.pace.advance(now_ns, self.refresh) else {
            return self.pace.next();
        };
        let instant = self.course.at(seconds);
        let shutter = self.pace.shutter_s(self.refresh) * self.course.pace;
        let placed = self.course.placed(instant.centre);
        self.trace(&instant, shutter, placed.painted, runner);
        let moment = self.spots.get(..MOMENT_SPOTS).unwrap_or(&[]);
        self.light.paint(moment, &placed, surface, runner);
        damage.add(placed.painted);
        if self.painted != placed.painted {
            self.uncovered.clear();
            self.uncovered.add(self.painted);
            self.uncovered.subtract(placed.painted);
            for &rect in self.uncovered.rects() {
                erase(surface, rect);
                damage.add(rect);
            }
            self.painted = placed.painted;
        }
        self.pace.next()
    }

    /// Trace every curve at each of the frame's moments, spread over
    /// `shutter` seconds of motion about `instant`, into the frame's spots,
    /// and sort each job's by the strips of the `painted` rows they light,
    /// across `runner`.
    fn trace(&mut self, instant: &Instant, shutter: f64, painted: Rect, runner: &dyn JobRunner) {
        let moments: [Moment; EXPOSURES] = from_fn(|exposure| {
            let offset = (mathf::real(exposure) + 0.5) / mathf::real(EXPOSURES) - 0.5;
            Moment::at(instant.time + TIME_RATE * shutter * offset, instant.drift)
        });
        let projection = Projection::new(instant, self.course.radius / REACH);
        let (curves, colours) = (&self.curves, self.colours.as_chunks::<GANG_POINTS>().0);
        let jobs = moments.iter().flat_map(|moment| {
            curves
                .gangs()
                .iter()
                .zip(colours)
                .map(move |(gang, colours)| (moment, gang, colours))
        });
        let traced = self.spots.as_chunks_mut::<GANG_POINTS>().0.iter_mut();
        for_each_drawn(
            runner,
            traced.zip(self.light.shares(painted)).zip(jobs),
            &|((spots, mut share), (moment, gang, colours))| {
                trace(gang, moment, colours, spots, &projection);
                share.sort(spots);
            },
        );
    }
}

/// Trace `gang` at `moment` into `spots`, each lit as `colours` says, where
/// `projection` puts it on the screen.
fn trace(
    gang: &Gang,
    moment: &Moment,
    colours: &[[u16; 3]; GANG_POINTS],
    spots: &mut [Spot; GANG_POINTS],
    projection: &Projection,
) {
    let mut steps = spots
        .as_chunks_mut::<LANES>()
        .0
        .iter_mut()
        .zip(colours.as_chunks::<LANES>().0);
    gang.trace(moment, |u, v| {
        if let Some((spots, colours)) = steps.next() {
            projection.place(u, v, spots, colours);
        }
    });
}

/// Where a point of the map lands on the screen: turned, scaled and centred.
struct Projection {
    centre: (f32, f32),
    /// The turn and scale as a matrix, by rows.
    turn: [f32; 4],
}

impl Projection {
    /// The projection at `instant`, `scale` pixels to the map's unit.
    fn new(instant: &Instant, scale: f64) -> Self {
        let (sin, cos) = (mathf::sin(instant.turn), mathf::cos(instant.turn));
        let in_fractions = |pixels: f64| mathf::single(pixels * f64::from(SUB));
        Self {
            centre: (
                in_fractions(instant.centre.0),
                in_fractions(instant.centre.1),
            ),
            turn: [cos, -sin, sin, cos].map(|entry| in_fractions(entry * scale)),
        }
    }

    /// Place each lane's point `(u, v)` into `spots`, lit as `colours` says.
    #[inline]
    fn place(&self, u: &Lanes, v: &Lanes, spots: &mut [Spot; LANES], colours: &[[u16; 3]; LANES]) {
        let [xu, xv, yu, yv] = self.turn;
        for (((spot, &light), &u), &v) in spots.iter_mut().zip(colours).zip(u).zip(v) {
            *spot = Spot {
                x: fixed(self.centre.0 + xu * u + xv * v),
                y: fixed(self.centre.1 + yu * u + yv * v),
                light,
            };
        }
    }
}

/// A position already in 256ths of a pixel as a whole number of them: the
/// nearest, for any position on the screen.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a position on the screen is far inside i32, and a float cast \
              saturates rather than wrapping should one ever not be"
)]
#[inline]
fn fixed(at: f32) -> i32 {
    (at + 0.5) as i32
}

/// Every point's light in the palette, its peak `peak` of a light, gang by
/// gang in the order a gang traces them; `None` when the heap will not hold
/// the table.
fn colours(palette: Palette, peak: f64) -> Option<Vec<[u16; 3]>> {
    let mut table = fallible::filled(CURVES * STEPS, [0u16; 3])?;
    palette.each(|curve, step, light| {
        let index = (curve / LANES) * GANG_POINTS + step * LANES + curve % LANES;
        if let Some(entry) = table.get_mut(index) {
            *entry = light.map(|channel| {
                let lit = mathf::round_i32(channel * peak * f64::from(LIGHT_ONE));
                u16::try_from(lit.clamp(0, i32::from(u16::MAX))).unwrap_or(u16::MAX)
            });
        }
    });
    Some(table)
}

/// When the picture moves on: every frame, or a still each minute.
enum Pace {
    Moving(SceneClock),
    Still { due_ns: u64, shown: u32 },
}

impl Pace {
    fn new(now_ns: u64, still: bool, refresh: RefreshRate) -> Self {
        if still {
            Self::Still {
                due_ns: now_ns,
                shown: 0,
            }
        } else {
            Self::Moving(SceneClock::new(now_ns, false, refresh))
        }
    }

    /// The seconds of motion the frame at `now_ns` shows, or `None` when it
    /// shows nothing new.
    fn advance(&mut self, now_ns: u64, refresh: RefreshRate) -> Option<f64> {
        match self {
            Self::Moving(clock) => Some(clock.advance(now_ns, refresh)),
            Self::Still { due_ns, shown } => {
                if now_ns < *due_ns {
                    return None;
                }
                let at = f64::from(*shown) * STILL_STRIDE_S;
                *shown = shown.saturating_add(1);
                *due_ns = now_ns.saturating_add(STILL_NS);
                Some(at)
            }
        }
    }

    /// When the next frame is wanted.
    fn next(&self) -> NextFrame {
        match self {
            Self::Moving(clock) => clock.due_ns().map_or(NextFrame::Idle, NextFrame::At),
            Self::Still { due_ns, .. } => NextFrame::At(*due_ns),
        }
    }

    /// The seconds of motion one frame's exposure spans.
    fn shutter_s(&self, refresh: RefreshRate) -> f64 {
        let frame = match self {
            Self::Moving(clock) => clock.frame_ns(),
            Self::Still { .. } => SceneClock::frame_on(refresh),
        };
        SHUTTER * seconds(frame)
    }
}

/// Where the universe stands at an instant.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Instant {
    /// The map's time and drift, and the whole picture's turn, in radians.
    time: f64,
    drift: f64,
    turn: f64,
    /// The bubble's centre on the screen, in pixels.
    centre: (f64, f64),
}

/// How the universe moves: the pace it is shown at, the phases a seed set it
/// off from, and where it sits on the screen and how far it wanders.
struct Course {
    pace: f64,
    /// The map's time, its drift, the turn, and the wander across and down.
    phases: [f64; 5],
    centre: (f64, f64),
    wander: (f64, f64),
    /// The bubble's radius, in pixels.
    radius: f64,
    screen: Rect,
    /// The side of the square a frame paints about the bubble's centre.
    side: u32,
}

impl Course {
    /// The course of a bubble `radius` pixels across `screen`, drawn as
    /// `dot`s with a glow of `cell`-pixel cells if it glows, shown at `pace`
    /// times normal and set off from phases `seed` chooses.
    fn new(
        (screen, radius): (Rect, f64),
        (dot, cell): (Dot, Option<u32>),
        pace: f64,
        seed: u64,
    ) -> Self {
        let (width, height) = (f64::from(screen.width), f64::from(screen.height));
        let room = |extent: f64| {
            mathf::clamp(
                extent / 2.0 - radius - MARGIN * extent,
                0.0,
                WANDER * radius,
            )
        };
        let reach =
            f64::from(dot.radius() + 1) + cell.map_or(0.0, |cell| f64::from(GLOW_CELLS * cell));
        let mut rng = NonCryptoRng::seed_from_u64(seed);
        Self {
            pace,
            phases: from_fn(|_| rng.next_f64() * TAU),
            centre: (width / 2.0, height / 2.0),
            wander: (room(width), room(height)),
            radius,
            screen,
            side: u32::try_from(mathf::round_i32(2.0 * mathf::ceil(radius + reach))).unwrap_or(0),
        }
    }

    /// Where the universe stands `seconds` into its motion.
    fn at(&self, seconds: f64) -> Instant {
        let s = seconds * self.pace;
        let [time, drift, turn, across, down] = self.phases;
        let wander = |period: f64, phase: f64| mathf::sin(TAU * s / period + phase);
        Instant {
            time: time + TIME_RATE * s,
            drift: drift + DRIFT_RATE * s,
            turn: turn + TURN_RATE * s,
            centre: (
                self.centre.0 + self.wander.0 * wander(WANDER_S.0, across),
                self.centre.1 + self.wander.1 * wander(WANDER_S.1, down),
            ),
        }
    }

    /// The square a frame centred at `centre` paints, and its part on the
    /// screen.
    fn placed(&self, centre: (f64, f64)) -> Placed {
        let half = f64::from(self.side) / 2.0;
        let corner = |at: f64| mathf::round_i32(mathf::floor(at - half));
        let frame = Rect::new(corner(centre.0), corner(centre.1), self.side, self.side);
        Placed {
            frame,
            painted: frame.intersection(&self.screen),
        }
    }
}

#[cfg(test)]
mod tests;
