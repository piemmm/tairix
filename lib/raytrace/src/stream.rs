//! A stream's surface over its stones: how the water running down a stony
//! channel rises and falls over and about each stone in it, and where it
//! breaks into foam.
//!
//! The stretch of stream the eye looks over is laid straight along its
//! course, and the steady flow down it is answered linearly: potential flow
//! over a finite depth with gravity and surface tension (Lamb, §§ 246-247;
//! Wehausen and Laitone, 1960). A stone beneath the surface is a rise in the
//! bed; a stone through it parts the water about it and leaves a wake, as a
//! Rankine half-body as broad as the stone at the waterline does. Each wave
//! number of the stones' bed and sources answers alone, so the whole answer
//! is two Fourier transforms of a grid along the stream and back: over a
//! stone the water dips where the flow is slow and humps where it is fast,
//! the lee waves whose crests keep pace with the stream stand behind it,
//! fanned in a V and shorter the slower it runs, and before a stone through
//! the surface the water piles up while behind it the wake falls away.
//! Turbulence damps the shorter waves sooner, and a fictitious damping lets
//! waves run only the way the flow carries them (Rayleigh's).
//!
//! The channel deepens and shallows along the stretch as well as across it,
//! its water slow in its pools and fast down its riffles, so the answer is
//! worked out at four depths and three speeds, and each place takes the
//! answers about its own depth and speed; water slower than the slowest
//! answers as that does scaled by the square of its speed, as the linear
//! answer itself falls away with it.
//!
//! The answer is linear, so a stone as tall as the water is deep would draw
//! a surface no water stands to: no place rises higher than the water's
//! velocity head lets it, nor falls nearer the bed than a little above it,
//! and a wave standing steeper than water can breaks. Fast water churns as
//! it nears critical, and where it lands below a ledge it is white; that
//! foam, with the foam a stone through the surface sheds, is carried down
//! the stream as it spreads and bursts.
//!
//! Every unit of the work is a few rows of the grid a core, so a caller
//! answering a frame stops after any of them.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::channel::{Section, GRAVITY};
use crate::fourier::{self, frequency, transposed, Complex, Fourier};
use crate::noise::{noise2, smoothstep};
use crate::vector::{byte, real, share, single, tanh, whole};

/// Water's surface tension over its density, in metres and seconds: past
/// about 1.7 cm surface tension holds a wave more than gravity.
const TENSION: f64 = 7.3e-5;

/// The fictitious damping that keeps the answer to the waves a stream
/// carries away from what stands in it, in each second, and the eddies'
/// viscosity that damps the shorter waves sooner, in square metres a second:
/// of the order a shallow stony stream's turbulence holds (Elder, 1959), so
/// a train of standing waves dies within a few of its lengths.
const DAMPING: f64 = 0.05;
const EDDIES: f64 = 1.0e-3;

/// The most of the water's depth a stone beneath the surface is taken to
/// raise the bed by in the linear answer, which holds only for rises small
/// against the depth.
const LINEAR: f64 = 0.6;

/// How broad the wake a stone through the surface leaves, against its own
/// breadth at the waterline.
const WAKE: f64 = 1.0;

/// The depths and the speeds the answer is worked out at, and the slowest
/// speed against the fastest.
const DEPTHS: usize = 4;
const SPEEDS: usize = 3;
const LAYERS: usize = DEPTHS * SPEEDS;
const SLOWEST: f64 = 0.3;

/// The slopes from which a wave begins to break and at which it is all
/// foam, and how long foam lasts on the water before it bursts: about a
/// second, so it whitens only the water an obstruction stirs.
const BREAKING: (f64, f64) = (0.45, 0.85);
const FOAM_LIFE: f64 = 0.8;

/// The speeds from which a stone through the surface sheds foam into its
/// wake and at which it sheds the most it does.
const SHEDDING: (f64, f64) = (0.5, 1.4);
const MOST_SHED: f64 = 0.7;

/// How much more steeply water poured just above a place than it runs there,
/// where it lands at the foot of a ledge, from which it begins to whiten and
/// at which it is all white; how far below the tongue it lands, a jet a metre
/// a second fast falling a few decimetres before it strikes the pool; and the
/// streaks it breaks in, how long down the stream and how broad across it,
/// clear water showing between them until it is all white.
const POURING: (f64, f64) = (0.25, 0.6);
const THROW: f64 = 0.4;
const STREAKS: (f64, f64) = (0.6, 0.05);
const STREAK_SEED: u32 = 0x5e7a;

/// How high fast water churns, as a share of its depth; the shares of
/// critical speed it begins to churn at and churns fully at; how long its
/// boils run along the stream and across it, as shares of its depth, and
/// the shortest a grid of the stream's holds.
const CHURN: f64 = 0.06;
const CHURNING: (f64, f64) = (0.35, 0.8);
const CHURN_LENGTHS: (f64, f64) = (2.0, 1.2);
const SHORTEST_BOIL: f64 = 0.08;
const CHURN_SEED: u32 = 0x6c1b;

/// How shallow water runs where its surface drapes over the gravel beneath
/// it rather than lying level above it, how high it then heaves as a share
/// of its depth, and how long the gravel's heaves run.
const DRAPED: f64 = 0.08;
const DRAPE: f64 = 0.3;
const GRAVEL: f64 = 0.07;
const DRAPE_SEED: u32 = 0x3d4f;

/// How near the bed the surface may fall, as a share of the depth.
const FLOOR: f64 = 0.85;

/// Rows of the grid a core works in a unit.
const ROWS: usize = 16;

/// A stretch of stream, as its course runs: where it begins along the
/// course and how long it is, how far either side of the course it reaches,
/// and its channel's sections along it, `spacing` apart from where it
/// begins.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stretch {
    pub(crate) from: f64,
    pub(crate) length: f64,
    pub(crate) half: f64,
    pub(crate) spacing: f64,
    pub(crate) sections: Vec<Section>,
}

impl Stretch {
    /// Whether water runs down it: some of it holds water, and it spans some
    /// length and breadth.
    pub(crate) fn flows(&self) -> bool {
        self.length > 0.0
            && self.half > 0.0
            && self
                .sections
                .iter()
                .any(|section| section.depth(section.thalweg) > 0.0)
    }

    /// The sections either side of `along`, and how far it lies from the
    /// first toward the second; held to the stretch's ends.
    fn about(&self, along: f64) -> Option<(&Section, &Section, f64)> {
        let at = ((along - self.from) / self.spacing.max(1e-9)).max(0.0);
        let last = self.sections.len().checked_sub(1)?;
        let below = whole(mathf::floor(at)).min(last);
        let above = (below + 1).min(last);
        Some((
            self.sections.get(below)?,
            self.sections.get(above)?,
            (at - real(below)).clamp(0.0, 1.0),
        ))
    }

    /// How deep the water stands and how fast it runs at `along` and
    /// `across` the course.
    pub(crate) fn place(&self, along: f64, across: f64) -> (f64, f64) {
        let Some((one, other, t)) = self.about(along) else {
            return (0.0, 0.0);
        };
        let blend = |a: f64, b: f64| a + (b - a) * t;
        let ((depth, speed), (deeper, faster)) = (one.water_at(across), other.water_at(across));
        (blend(depth, deeper), blend(speed, faster))
    }

    /// How steeply the water's surface falls at `along`.
    fn slope(&self, along: f64) -> f64 {
        self.about(along).map_or(0.0, |(one, other, t)| {
            one.slope + (other.slope - one.slope) * t
        })
    }

    /// How much of its bed is a ledge's bare rock at `along`, smooth where
    /// gravel is not.
    fn ledge(&self, along: f64) -> f64 {
        self.about(along).map_or(0.0, |(one, other, t)| {
            one.ledge + (other.ledge - one.ledge) * t
        })
    }
}

/// A stone in the stretch: where it lies along and across the course, how
/// far it reaches along the stream and across it from there, and how high
/// its top stands above the bed beneath it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Stone {
    pub(crate) at: (f64, f64),
    pub(crate) reach: (f64, f64),
    pub(crate) top: f64,
}

/// The surface's answer to a stretch's stones, once solved: how far the
/// surface stands above or below its level, and how much foam lies on it,
/// at each point of a grid along the stretch.
#[derive(Debug)]
pub(crate) struct Flow {
    /// Where the grid begins along the course, and how far either side of
    /// it it reaches.
    from: f64,
    half: f64,
    /// Points along the stretch and across it, and how far apart.
    size: (usize, usize),
    cell: f64,
    rise: Vec<f32>,
    foam: Vec<u8>,
}

impl Flow {
    /// How far the surface stands above its level at `along` and `across`
    /// the course, and the share of it foam covers; nothing outside the
    /// stretch.
    pub(crate) fn at(&self, along: f64, across: f64) -> (f64, f64) {
        let (columns, rows) = self.size;
        let u = (along - self.from) / self.cell;
        let v = (across + self.half) / self.cell;
        if !(u >= 0.0 && v >= 0.0 && u < real(columns - 1) && v < real(rows - 1)) {
            return (0.0, 0.0);
        }
        let (column, row) = (mathf::floor(u), mathf::floor(v));
        let (across_cell, down) = (u - column, v - row);
        let (column, row) = (whole(column), whole(row));
        let at = |c: usize, r: usize| {
            let index = r * columns + c;
            (
                self.rise.get(index).map_or(0.0, |&rise| f64::from(rise)),
                self.foam
                    .get(index)
                    .map_or(0.0, |&foam| f64::from(foam) / 255.0),
            )
        };
        let corners = [
            at(column, row),
            at(column + 1, row),
            at(column, row + 1),
            at(column + 1, row + 1),
        ];
        let blend = |pick: fn((f64, f64)) -> f64| {
            let top = pick(corners[0]) + (pick(corners[1]) - pick(corners[0])) * across_cell;
            let bottom = pick(corners[2]) + (pick(corners[3]) - pick(corners[2])) * across_cell;
            top + (bottom - top) * down
        };
        (blend(|corner| corner.0), blend(|corner| corner.1))
    }
}

/// What a pass over the surface makes of a point on the stretch: from the
/// rise there, where it lies along and across the course, how deep and fast
/// its water runs, and how far it is tapered to nothing.
type Adjust = dyn Fn(&Stretch, f64, (f64, f64), (f64, f64), f64) -> f64 + Sync;

/// A stone as the grid takes it: where it stands in the order the stones
/// were laid, the first and last rows it reaches, the cells its rise in the
/// bed covers, and, where it stands through the surface, the cells its
/// source covers and the source itself.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Laid {
    stone: Stone,
    order: u32,
    rows: (usize, usize),
    bed: Option<Cells>,
    source: Option<(Cells, Source)>,
}

/// The columns and rows of the grid a footprint covers, both ends taken.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Cells {
    columns: (usize, usize),
    rows: (usize, usize),
}

impl Cells {
    fn holds(&self, row: usize) -> bool {
        (self.rows.0..=self.rows.1).contains(&row)
    }
}

/// The source of the water a stone through the surface parts: where it
/// stands, how far it spreads, and how strong it is at its middle for each
/// metre a second the stream runs.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Source {
    at: (f64, f64),
    spread: f64,
    density: f64,
}

impl Laid {
    /// Lay its share into row `row` of the grid, `line`, whose water stands
    /// as `places` has it, the grid's points `cell` apart from `origin`.
    fn row(
        &self,
        (row, line, places): (usize, &mut [Complex], &[[f32; 2]]),
        (origin, cell): ((f64, f64), f64),
    ) {
        let along = |column: usize| origin.0 + real(column) * cell;
        let across = origin.1 + real(row) * cell;
        if let Some(bed) = self.bed.filter(|bed| bed.holds(row)) {
            let Stone { at, reach, top } = self.stone;
            let v = (across - at.1) / reach.1;
            for column in bed.columns.0..=bed.columns.1 {
                let u = (along(column) - at.0) / reach.0;
                let inside = 1.0 - u * u - v * v;
                let (Some(value), Some(&[depth, _])) = (line.get_mut(column), places.get(column))
                else {
                    continue;
                };
                if inside > 0.0 {
                    let rise = (top * mathf::sqrt(inside)).min(LINEAR * f64::from(depth));
                    value.re = value.re.max(rise);
                }
            }
        }
        if let Some((cells, source)) = self.source.filter(|(cells, _)| cells.holds(row)) {
            let dn = across - source.at.1;
            let spread = 2.0 * source.spread * source.spread;
            for column in cells.columns.0..=cells.columns.1 {
                let ds = along(column) - source.at.0;
                if let Some(value) = line.get_mut(column) {
                    value.im += source.density * mathf::exp(-(ds * ds + dn * dn) / spread);
                }
            }
        }
    }
}

/// Values spaced evenly in their logarithm from the least to the most: the
/// depths or the speeds the answer is worked out at.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Ladder {
    least: f64,
    /// The least's logarithm, the span of them all, and how many rungs.
    ln_least: f64,
    ln_span: f64,
    rungs: usize,
}

impl Ladder {
    /// `rungs` rungs from `least` to `most`.
    fn new(least: f64, most: f64, rungs: usize) -> Self {
        let least = least.max(1e-9);
        let ln_least = mathf::ln(least);
        Self {
            least,
            ln_least,
            ln_span: (mathf::ln(most.max(least)) - ln_least).max(0.0),
            rungs: rungs.max(1),
        }
    }

    /// The value of rung `rung`.
    fn value(&self, rung: usize) -> f64 {
        let t = share(rung, self.rungs - 1);
        mathf::exp(self.ln_least + t * self.ln_span)
    }

    /// Where `value` lies on the ladder: the rung at or below it and how far
    /// toward the next, held to its ends.
    fn place(&self, value: f64) -> (usize, f64) {
        let top = real(self.rungs - 1);
        if self.ln_span <= 0.0 || value <= self.least {
            return (0, 0.0);
        }
        let at = (top * (mathf::ln(value) - self.ln_least) / self.ln_span).clamp(0.0, top);
        let below = mathf::floor(at).min(top - 1.0).max(0.0);
        (whole(below), at - below)
    }

    /// The weight rung `rung`'s answer takes where the value lies at
    /// `place`.
    fn weight((below, t): (usize, f64), rung: usize) -> f64 {
        if rung == below {
            1.0 - t
        } else if rung == below + 1 {
            t
        } else {
            0.0
        }
    }
}

/// How far a solving has come.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// Measuring how deep and fast the water runs at each point, from `row`.
    Measuring {
        row: usize,
    },
    /// Laying the stones into the bed and the sources, from `row`.
    Laying {
        row: usize,
    },
    /// Transforming the grid's rows along the stream from `row`.
    Along {
        row: usize,
    },
    /// Turning the grid on its side from `row`, then transforming those rows.
    Turning {
        row: usize,
    },
    Across {
        row: usize,
    },
    /// Answering the layers of `pair`, the `pair`th two of them at once,
    /// from `row` of the turned grid, then turning it back and transforming
    /// it back along the stream into the answer.
    Answering {
        pair: usize,
        row: usize,
    },
    TurningBack {
        pair: usize,
        row: usize,
    },
    Returning {
        pair: usize,
        row: usize,
    },
    /// Holding the answer to what water can stand, from `row`.
    Holding {
        row: usize,
    },
    /// Finding where it breaks and carrying the foam down the stream, from
    /// `row`.
    Foaming {
        row: usize,
    },
    /// Roughening the surface where fast water churns and shallow water
    /// drapes over its gravel, from `row`.
    Roughening {
        row: usize,
    },
    Done,
}

/// A stretch's flow being solved a few rows of its grid at a time.
#[derive(Debug)]
pub(crate) struct Solving {
    stretch: Stretch,
    stones: Vec<Stone>,
    size: (usize, usize),
    cell: f64,
    along: Fourier,
    across: Fourier,
    /// The bed's rises and the stones' sources as the real and imaginary
    /// parts of one grid, rows across the stream; the same turned, rows
    /// along it, which becomes their transform; and a turned grid of two
    /// layers' answers at once.
    grid: Vec<Complex>,
    turned: Vec<Complex>,
    answer: Vec<Complex>,
    /// How deep the water stands and how fast it runs at each point, and
    /// the deepest and the fastest measured so far.
    places: Vec<[f32; 2]>,
    measured: (f64, f64),
    /// The stones as the grid takes them, once the water is measured.
    laid: Vec<Laid>,
    /// The depths and the speeds answered, the answer blended between them,
    /// and the foam on it.
    depths: Ladder,
    speeds: Ladder,
    rise: Vec<f32>,
    foam: Vec<u8>,
    /// How white the water breaks where it lands below a ledge, down each
    /// column of the grid.
    pouring: Vec<f64>,
    /// The foam each stone through the surface sheds: where across the
    /// course it sheds it, how far along, and how broad the wake it sheds it
    /// into.
    shed: Vec<Shed>,
    phase: Phase,
}

/// The wake of foam a stone through the surface sheds behind it.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Shed {
    across: f64,
    along: f64,
    half: f64,
    /// How thick the foam it sheds, `0.0..=1.0`.
    share: f64,
}

impl Solving {
    /// The flow over `stones` down `stretch`, on a grid of `cells` apart and
    /// at most `most` points along and across it; `None` for a stretch with
    /// no water or when the heap will not hold the work.
    pub(crate) fn new(
        stretch: Stretch,
        stones: Vec<Stone>,
        (cell, most): (f64, (usize, usize)),
    ) -> Option<Self> {
        if !stretch.flows() || cell <= 0.0 {
            return None;
        }
        // As many points as the span wants, a power of two no more than
        // `most`.
        let fit = |span: f64, most: usize| {
            let wanted = usize::try_from(mathf::round_i32(mathf::ceil(span / cell)).max(2)).ok()?;
            Some(wanted.min(1 << most.max(2).ilog2()).next_power_of_two())
        };
        let (columns, rows) = (
            fit(stretch.length, most.0)?,
            fit(2.0 * stretch.half, most.1)?,
        );
        // As fine as the grid allows, its points spanning the whole stretch.
        let cell = (stretch.length / real(columns)).max(2.0 * stretch.half / real(rows));
        let points = columns.checked_mul(rows)?;
        Some(Self {
            stretch,
            stones,
            size: (columns, rows),
            cell,
            along: Fourier::new(columns)?,
            across: Fourier::new(rows)?,
            grid: fallible::filled(points, Complex::ZERO)?,
            turned: fallible::filled(points, Complex::ZERO)?,
            answer: fallible::filled(points, Complex::ZERO)?,
            places: fallible::filled(points, [0.0f32; 2])?,
            measured: (0.0, 0.0),
            laid: Vec::new(),
            depths: Ladder::new(1.0, 1.0, DEPTHS),
            speeds: Ladder::new(1.0, 1.0, SPEEDS),
            rise: fallible::filled(points, 0.0f32)?,
            foam: Vec::new(),
            pouring: Vec::new(),
            shed: Vec::new(),
            phase: Phase::Measuring { row: 0 },
        })
    }

    /// How far the solving has come, as a share of its work.
    pub(crate) fn done(&self) -> f64 {
        let (columns, rows) = self.size;
        let pairs = LAYERS / 2;
        // Measuring, laying, the forward transform's three passes, each
        // pair's three, and the holding, foaming and roughening, weighed
        // alike.
        let passes = real(5 + 3 * pairs + 3);
        let (pass, within) = match self.phase {
            Phase::Measuring { row } => (0, share(row, rows)),
            Phase::Laying { row } => (1, share(row, rows)),
            Phase::Along { row } => (2, share(row, rows)),
            Phase::Turning { row } => (3, share(row, columns)),
            Phase::Across { row } => (4, share(row, columns)),
            Phase::Answering { pair, row } => (5 + 3 * pair, share(row, columns)),
            Phase::TurningBack { pair, row } => (6 + 3 * pair, share(row, rows)),
            Phase::Returning { pair, row } => (7 + 3 * pair, share(row, rows)),
            Phase::Holding { row } => (5 + 3 * pairs, share(row, rows)),
            Phase::Foaming { row } => (6 + 3 * pairs, share(row, rows)),
            Phase::Roughening { row } => (7 + 3 * pairs, share(row, rows)),
            Phase::Done => return 1.0,
        };
        (real(pass) + within) / passes
    }

    /// Solve the next unit across `runner`: whether the flow is solved, or
    /// `None` when the heap will not hold the work.
    pub(crate) fn step(&mut self, runner: &dyn JobRunner) -> Option<bool> {
        self.phase = match self.phase {
            Phase::Measuring { .. }
            | Phase::Laying { .. }
            | Phase::Along { .. }
            | Phase::Turning { .. }
            | Phase::Across { .. } => self.forward(runner)?,
            Phase::Done => Phase::Done,
            _ => self.backward(runner)?,
        };
        Some(self.phase == Phase::Done)
    }

    /// The next unit of measuring the water, laying the stones and
    /// transforming them, and the phase it leads to.
    fn forward(&mut self, runner: &dyn JobRunner) -> Option<Phase> {
        let (columns, rows) = self.size;
        let unit = ROWS * runner.width().max(1);
        Some(match self.phase {
            Phase::Measuring { row } => {
                let end = (row + unit).min(rows);
                self.measure_rows(row..end, runner)?;
                if end < rows {
                    Phase::Measuring { row: end }
                } else {
                    self.ladders();
                    self.lay_out()?;
                    Phase::Laying { row: 0 }
                }
            }
            Phase::Laying { row } => {
                let end = (row + unit).min(rows);
                self.lay_rows(row..end, runner)?;
                if end < rows {
                    Phase::Laying { row: end }
                } else {
                    self.laid = Vec::new();
                    Phase::Along { row: 0 }
                }
            }
            Phase::Along { row } => {
                let end = (row + unit).min(rows);
                let span = self.grid.get_mut(row * columns..end * columns)?;
                fourier::rows(&self.along, span, (ROWS, false), runner)?;
                if end < rows {
                    Phase::Along { row: end }
                } else {
                    Phase::Turning { row: 0 }
                }
            }
            Phase::Turning { row } => {
                let end = (row + unit).min(columns);
                let to = self.turned.get_mut(row * rows..end * rows)?;
                transposed(&self.grid, (rows, columns), to, (row, ROWS), runner)?;
                if end < columns {
                    Phase::Turning { row: end }
                } else {
                    Phase::Across { row: 0 }
                }
            }
            Phase::Across { row } => {
                let end = (row + unit).min(columns);
                let span = self.turned.get_mut(row * rows..end * rows)?;
                fourier::rows(&self.across, span, (ROWS, false), runner)?;
                if end < columns {
                    Phase::Across { row: end }
                } else {
                    Phase::Answering { pair: 0, row: 0 }
                }
            }
            other => other,
        })
    }

    /// The next unit of answering each pair of layers, turning the answer
    /// back and holding it to what water stands, and the phase it leads to.
    fn backward(&mut self, runner: &dyn JobRunner) -> Option<Phase> {
        let (columns, rows) = self.size;
        let unit = ROWS * runner.width().max(1);
        Some(match self.phase {
            Phase::Answering { pair, row } => {
                let end = (row + unit).min(columns);
                self.answer_rows(pair, row..end, runner)?;
                if end < columns {
                    Phase::Answering { pair, row: end }
                } else {
                    Phase::TurningBack { pair, row: 0 }
                }
            }
            Phase::TurningBack { pair, row } => {
                let end = (row + unit).min(rows);
                let to = self.grid.get_mut(row * columns..end * columns)?;
                transposed(&self.answer, (columns, rows), to, (row, ROWS), runner)?;
                if end < rows {
                    Phase::TurningBack { pair, row: end }
                } else {
                    Phase::Returning { pair, row: 0 }
                }
            }
            Phase::Returning { pair, row } => {
                let end = (row + unit).min(rows);
                self.return_rows(pair, row..end, runner)?;
                if end < rows {
                    Phase::Returning { pair, row: end }
                } else if 2 * (pair + 1) < LAYERS {
                    Phase::Answering {
                        pair: pair + 1,
                        row: 0,
                    }
                } else {
                    self.release();
                    Phase::Holding { row: 0 }
                }
            }
            Phase::Holding { row } => {
                let end = (row + unit).min(rows);
                self.hold_rows(row..end, runner)?;
                if end < rows {
                    Phase::Holding { row: end }
                } else {
                    self.foam = self.shed_foam()?;
                    self.pouring = self.poured()?;
                    Phase::Foaming { row: 0 }
                }
            }
            Phase::Foaming { row } => {
                let end = (row + unit).min(rows);
                self.foam_rows(row..end, runner)?;
                if end < rows {
                    Phase::Foaming { row: end }
                } else {
                    Phase::Roughening { row: 0 }
                }
            }
            Phase::Roughening { row } => {
                let end = (row + unit).min(rows);
                self.roughen_rows(row..end, runner)?;
                if end < rows {
                    Phase::Roughening { row: end }
                } else {
                    Phase::Done
                }
            }
            other => other,
        })
    }

    /// The flow solved; `None` before it is.
    pub(crate) fn finish(self) -> Option<Flow> {
        (self.phase == Phase::Done).then_some(Flow {
            from: self.stretch.from,
            half: self.stretch.half,
            size: self.size,
            cell: self.cell,
            rise: self.rise,
            foam: self.foam,
        })
    }

    /// Rows `rows` of how deep the water stands and how fast it runs at
    /// each point, across `runner`.
    fn measure_rows(
        &mut self,
        rows: core::ops::Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let (columns, _) = self.size;
        let (stretch, cell) = (&self.stretch, self.cell);
        let first = rows.start;
        let span = self
            .places
            .get_mut(rows.start * columns..rows.end * columns)?;
        let most = |one: (f64, f64), other: (f64, f64)| (one.0.max(other.0), one.1.max(other.1));
        let measured = band::fold(
            runner,
            span,
            (0, ROWS * columns),
            (0.0, 0.0),
            &|band, values| {
                let mut measured = (0.0f64, 0.0f64);
                for (offset, line) in values.chunks_mut(columns).enumerate() {
                    let across = real(first + band * ROWS + offset) * cell - stretch.half;
                    for (column, place) in line.iter_mut().enumerate() {
                        let (depth, speed) =
                            stretch.place(stretch.from + real(column) * cell, across);
                        *place = [single(depth), single(speed)];
                        let [depth, speed] = place.map(f64::from);
                        measured = most(measured, (depth, speed));
                    }
                }
                measured
            },
            most,
        );
        self.measured = most(self.measured, measured);
        Some(())
    }

    /// The depths and the speeds the answer is worked out at, spanning those
    /// the water was measured at.
    fn ladders(&mut self) {
        let (deepest, fastest) = self.measured;
        let shallowest = (0.15 * deepest).max(0.02).min(deepest);
        self.depths = Ladder::new(shallowest, deepest, DEPTHS);
        let fastest = fastest.max(1e-3);
        self.speeds = Ladder::new(SLOWEST * fastest, fastest, SPEEDS);
    }

    /// Every stone as the grid takes it, ordered by the first row it
    /// reaches, and the wake each through the surface sheds; `None` when the
    /// heap will not hold them.
    fn lay_out(&mut self) -> Option<()> {
        let stones = core::mem::take(&mut self.stones);
        let mut laid = Vec::new();
        laid.try_reserve_exact(stones.len()).ok()?;
        for (order, stone) in (0u32..).zip(stones) {
            let (taken, shed) = self.taken(stone, order);
            if let Some(shed) = shed {
                self.shed.try_reserve(1).ok()?;
                self.shed.push(shed);
            }
            laid.extend(taken);
        }
        laid.sort_unstable_by_key(|laid| (laid.rows.0, laid.order));
        self.laid = laid;
        Some(())
    }

    /// How the grid takes `stone`, the `order`th laid: the rise it makes in
    /// the bed, as much of it as the linear answer holds, and, where it
    /// stands through the surface, the source of the water it parts about
    /// it and the wake it sheds; nothing where it covers no point.
    fn taken(&self, stone: Stone, order: u32) -> (Option<Laid>, Option<Shed>) {
        let (columns, rows) = self.size;
        let (depth, speed) = self.stretch.place(stone.at.0, stone.at.1);
        let reach = (stone.reach.0.max(1e-3), stone.reach.1.max(1e-3));
        let stone = Stone { reach, ..stone };
        let span = |centre: f64, reach: f64, first: f64, count: usize| {
            let low = mathf::floor((centre - reach - first) / self.cell).max(0.0);
            let high = mathf::ceil((centre + reach - first) / self.cell).min(real(count - 1));
            (low <= high).then(|| (whole(low), whole(high)))
        };
        let cells = |at: (f64, f64), reach: (f64, f64)| {
            Some(Cells {
                columns: span(at.0, reach.0, self.stretch.from, columns)?,
                rows: span(at.1, reach.1, -self.stretch.half, rows)?,
            })
        };
        let bed = cells(stone.at, reach);
        let (source, shed) = if depth > 0.0 && stone.top > depth {
            // Its breadth at the waterline, and the half-body as broad, its
            // nose at the stone's upstream face.
            let waterline = mathf::sqrt(1.0 - (depth / stone.top) * (depth / stone.top));
            let (nose, half) = (reach.0 * waterline, reach.1 * waterline);
            let at = (stone.at.0 - nose + half / PI, stone.at.1);
            let spread = (0.5 * half).max(self.cell);
            let source = cells(at, (3.0 * spread, 3.0 * spread)).map(|cells| {
                let density = 2.0 * WAKE * half / (TAU * spread * spread);
                (
                    cells,
                    Source {
                        at,
                        spread,
                        density,
                    },
                )
            });
            let shed = Shed {
                across: stone.at.1,
                along: stone.at.0 + nose,
                half,
                // Water sheds white behind a stone only once it runs briskly.
                share: MOST_SHED * smoothstep(SHEDDING.0, SHEDDING.1, speed),
            };
            (source, Some(shed))
        } else {
            (None, None)
        };
        let reached = bed
            .iter()
            .chain(source.iter().map(|(cells, _)| cells))
            .map(|cells| cells.rows)
            .reduce(|one, other| (one.0.min(other.0), one.1.max(other.1)));
        let laid = reached.map(|rows| Laid {
            stone,
            order,
            rows,
            bed,
            source,
        });
        (laid, shed)
    }

    /// Lay the stones into rows `rows` of the grid across `runner`, each row
    /// taking every stone that reaches it in the stones' order, so the grid
    /// comes out the same however its rows are divided.
    fn lay_rows(&mut self, rows: core::ops::Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        let (columns, _) = self.size;
        let (laid, places) = (&self.laid, &self.places);
        let placing = ((self.stretch.from, -self.stretch.half), self.cell);
        let first = rows.start;
        let span = self
            .grid
            .get_mut(rows.start * columns..rows.end * columns)?;
        band::for_each(runner, span, (0, ROWS * columns), &|band, values| {
            let top = first + band * ROWS;
            let bottom = top + values.len() / columns;
            let reaching = laid.partition_point(|laid| laid.rows.0 < bottom);
            for stone in laid.get(..reaching).unwrap_or(&[]) {
                if stone.rows.1 < top {
                    continue;
                }
                for (row, line) in (top..).zip(values.chunks_mut(columns)) {
                    let measured = places
                        .get(row * columns..(row + 1) * columns)
                        .unwrap_or(&[]);
                    stone.row((row, line, measured), placing);
                }
            }
        });
        Some(())
    }

    /// The depth and the speed layer `layer` is answered at.
    fn layer(&self, layer: usize) -> (f64, f64) {
        (
            self.depths.value(layer / SPEEDS),
            self.speeds.value(layer % SPEEDS),
        )
    }

    /// Rows `rows` of the turned answer for the `pair`th two layers: each
    /// wave number's answer to the bed and the sources it carries, the two
    /// layers as the real and imaginary parts, transformed back across the
    /// stream.
    fn answer_rows(
        &mut self,
        pair: usize,
        rows: core::ops::Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let (columns, across) = self.size;
        let (length, breadth) = (real(columns) * self.cell, real(across) * self.cell);
        let layers = (self.layer(2 * pair), self.layer(2 * pair + 1));
        // Layers answered at one depth share its wave numbers' depth terms.
        let shared = 2 * pair / SPEEDS == (2 * pair + 1) / SPEEDS;
        let (spectrum, back) = (&self.turned, &self.across);
        let first = rows.start;
        let span = self
            .answer
            .get_mut(rows.start * across..rows.end * across)?;
        let failed = band::fold(
            runner,
            span,
            (0, ROWS * across),
            false,
            &|band, values| {
                values.chunks_mut(across).enumerate().any(|(offset, line)| {
                    let row = first + band * ROWS + offset;
                    let mirrored = (columns - row) % columns;
                    let k_along = TAU * frequency(row, columns) / length;
                    for (column, value) in line.iter_mut().enumerate() {
                        let k_across = TAU * frequency(column, across) / breadth;
                        let here = spectrum
                            .get(row * across + column)
                            .copied()
                            .unwrap_or_default();
                        // The bed and the sources are real, so each wave
                        // number's pair is its mirror's conjugate.
                        let there = spectrum
                            .get(mirrored * across + (across - column) % across)
                            .copied()
                            .unwrap_or_default()
                            .conj();
                        let bed = (here + there).scale(0.5);
                        let sources =
                            Complex::new(here.im - there.im, there.re - here.re).scale(0.5);
                        let k = mathf::hypot(k_along, k_across);
                        let first = Deep::at(k, layers.0 .0);
                        let second = if shared {
                            first
                        } else {
                            Deep::at(k, layers.1 .0)
                        };
                        let respond = |deep: Option<Deep>, speed: f64| {
                            deep.map_or(Complex::ZERO, |deep| {
                                let (to_bed, to_sources) = deep.answer(k_along, speed);
                                to_bed * bed + to_sources * sources
                            })
                        };
                        let (one, other) =
                            (respond(first, layers.0 .1), respond(second, layers.1 .1));
                        *value = one + Complex::new(-other.im, other.re);
                    }
                    back.inverse(line).is_none()
                })
            },
            |one, other| one || other,
        );
        (!failed).then_some(())
    }

    /// Rows `rows` of the grid, turned back from the answer, transformed back
    /// along the stream and blended into the answer by the depth and the
    /// speed at each point.
    fn return_rows(
        &mut self,
        pair: usize,
        rows: core::ops::Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let (columns, _) = self.size;
        let points = rows.start * columns..rows.end * columns;
        fourier::rows(
            &self.along,
            self.grid.get_mut(points.clone())?,
            (ROWS, true),
            runner,
        )?;
        let (depths, speeds) = (self.depths, self.speeds);
        let slowest = speeds.value(0);
        let (grid, places) = (&self.grid, &self.places);
        let first = points.start;
        let failed = band::fold(
            runner,
            self.rise.get_mut(points)?,
            (0, ROWS * columns),
            false,
            &|band, out| {
                let from = first + band * ROWS * columns;
                let span = from..from + out.len();
                let (Some(line), Some(places)) = (grid.get(span.clone()), places.get(span)) else {
                    return true;
                };
                for ((rise, value), &[depth, speed]) in out.iter_mut().zip(line).zip(places) {
                    let (depth, speed) = (f64::from(depth), f64::from(speed));
                    let (by_depth, by_speed) = (depths.place(depth), speeds.place(speed));
                    let slower = if speed < slowest {
                        let ratio = speed / slowest;
                        ratio * ratio
                    } else {
                        1.0
                    };
                    let weight = |layer: usize| {
                        Ladder::weight(by_depth, layer / SPEEDS)
                            * Ladder::weight(by_speed, layer % SPEEDS)
                            * slower
                    };
                    *rise += single(weight(2 * pair) * value.re + weight(2 * pair + 1) * value.im);
                }
                false
            },
            |one, other| one || other,
        );
        (!failed).then_some(())
    }

    /// Let the transforms' grids go once every layer is answered.
    fn release(&mut self) {
        self.grid = Vec::new();
        self.turned = Vec::new();
        self.answer = Vec::new();
    }

    /// Rows `rows` of the answer held to what water can stand, and tapered
    /// to nothing at the stretch's ends and where the water thins to its
    /// edge; across `runner`.
    fn hold_rows(&mut self, rows: core::ops::Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        self.each_rise(rows, runner, &|_, rise, _, (depth, speed), taper| {
            taper * held(rise, head(speed), FLOOR * depth)
        })
    }

    /// Rows `rows` of the held answer roughened where fast water churns and
    /// shallow water drapes over its gravel — texture the water's own
    /// breaking never sees — tapered as the answer is and held again; across
    /// `runner`.
    fn roughen_rows(
        &mut self,
        rows: core::ops::Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        self.each_rise(rows, runner, &|stretch, rise, at, (depth, speed), taper| {
            // A ledge's bare rock is smooth; only gravel shows through.
            let rough = churn(at, (depth, speed)) + (1.0 - stretch.ledge(at.0)) * draped(at, depth);
            held(rise + taper * rough, head(speed), FLOOR * depth)
        })
    }

    /// Rows `rows` of the surface, each point set to what `adjust` makes of
    /// it on the stretch: as it stands, where it lies along and across the
    /// course, how deep and fast its water runs, and how far it is tapered
    /// to nothing at the stretch's ends and where the water thins to its
    /// edge; across `runner`.
    fn each_rise(
        &mut self,
        rows: core::ops::Range<usize>,
        runner: &dyn JobRunner,
        adjust: &Adjust,
    ) -> Option<()> {
        let (columns, _) = self.size;
        let (cell, half, length, from) = (
            self.cell,
            self.stretch.half,
            self.stretch.length,
            self.stretch.from,
        );
        let (stretch, places) = (&self.stretch, &self.places);
        let first = rows.start;
        let span = self
            .rise
            .get_mut(rows.start * columns..rows.end * columns)?;
        band::for_each(runner, span, (0, ROWS * columns), &|band, values| {
            for (offset, line) in values.chunks_mut(columns).enumerate() {
                let row = first + band * ROWS + offset;
                let across = real(row) * cell - half;
                let measured = places
                    .get(row * columns..(row + 1) * columns)
                    .unwrap_or(&[]);
                for ((column, rise), &[depth, speed]) in line.iter_mut().enumerate().zip(measured) {
                    let (depth, speed) = (f64::from(depth), f64::from(speed));
                    let along = real(column) * cell;
                    let taper = smoothstep(0.0, 0.1 * length, along)
                        * smoothstep(0.0, 0.1 * length, length - along)
                        * smoothstep(0.0, 0.01, depth);
                    let at = (from + along, across);
                    *rise = single(adjust(stretch, f64::from(*rise), at, (depth, speed), taper));
                }
            }
        });
        Some(())
    }

    /// The foam grid, each point holding the foam a stone through the
    /// surface sheds there, the source its carrying down the stream starts
    /// from.
    fn shed_foam(&self) -> Option<Vec<u8>> {
        let (columns, rows) = self.size;
        let mut foam = fallible::filled(columns * rows, 0u8)?;
        let (cell, half, from) = (self.cell, self.stretch.half, self.stretch.from);
        for wake in &self.shed {
            let column = mathf::round((wake.along - from) / cell);
            if !(0.0..real(columns)).contains(&column) {
                continue;
            }
            let share = byte(wake.share);
            let low = mathf::floor((wake.across - wake.half + half) / cell).max(0.0);
            let high = mathf::ceil((wake.across + wake.half + half) / cell).min(real(rows - 1));
            if low > high {
                continue;
            }
            for row in whole(low)..=whole(high) {
                let across = real(row) * cell - half;
                if (across - wake.across).abs() >= wake.half {
                    continue;
                }
                if let Some(slot) = foam.get_mut(row * columns + whole(column)) {
                    *slot = (*slot).max(share);
                }
            }
        }
        Some(foam)
    }

    /// How white the water breaks down each column of the grid where it lands
    /// below a ledge: by how much more steeply it poured within a throw above
    /// than it runs there, so it pours glassy down the ledge's tongue and
    /// breaks at its foot; `None` when the heap will not hold the columns.
    fn poured(&self) -> Option<Vec<f64>> {
        let (columns, _) = self.size;
        let (cell, from) = (self.cell, self.stretch.from);
        let mut slopes = Vec::new();
        slopes.try_reserve_exact(columns).ok()?;
        slopes.extend((0..columns).map(|column| self.stretch.slope(from + real(column) * cell)));
        let throw = whole(THROW / cell).max(1);
        let mut pouring = Vec::new();
        pouring.try_reserve_exact(columns).ok()?;
        pouring.extend((0..columns).map(|column| {
            let above = slopes
                .get(column.saturating_sub(throw)..=column)
                .unwrap_or(&[])
                .iter()
                .fold(0.0f64, |steepest, &slope| steepest.max(slope));
            let here = slopes.get(column).copied().unwrap_or(0.0);
            smoothstep(POURING.0, POURING.1, above - here)
        }));
        Some(pouring)
    }

    /// Rows `rows` of the foam: where the surface stands steeper than a wave
    /// can and breaks, where the water lands below a ledge, and where a stone
    /// through the surface sheds it, each carried down the stream as it
    /// bursts; across `runner`.
    fn foam_rows(&mut self, rows: core::ops::Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        let (columns, all) = self.size;
        let (cell, half, from) = (self.cell, self.stretch.half, self.stretch.from);
        let (rise, places, pouring) = (&self.rise, &self.places, &self.pouring);
        let first = rows.start;
        let span = self
            .foam
            .get_mut(rows.start * columns..rows.end * columns)?;
        band::for_each(runner, span, (0, ROWS * columns), &|band, values| {
            for (offset, line) in values.chunks_mut(columns).enumerate() {
                let row = first + band * ROWS + offset;
                let across = real(row) * cell - half;
                let height = |column: usize, row: usize| {
                    rise.get(row.min(all - 1) * columns + column.min(columns - 1))
                        .map_or(0.0, |&value| f64::from(value))
                };
                let mut carried = 0.0f64;
                for (column, slot) in line.iter_mut().enumerate() {
                    let [depth, speed] = places
                        .get(row * columns + column)
                        .copied()
                        .unwrap_or_default();
                    let speed = f64::from(speed);
                    let fade = if speed > 0.0 {
                        mathf::exp(-cell / (speed * FOAM_LIFE))
                    } else {
                        0.0
                    };
                    let (left, right) = (column.saturating_sub(1), column + 1);
                    let along_slope = (height(right, row) - height(left, row))
                        / (cell * real(right.min(columns - 1) - left).max(1.0));
                    let across_slope = (height(column, row + 1)
                        - height(column, row.saturating_sub(1)))
                        / (2.0 * cell);
                    let breaking = smoothstep(
                        BREAKING.0,
                        BREAKING.1,
                        mathf::hypot(along_slope, across_slope),
                    );
                    let along = from + real(column) * cell;
                    let shedding = f64::from(*slot) / 255.0;
                    let poured = if depth > 0.0 {
                        let pour = pouring.get(column).copied().unwrap_or(0.0);
                        let streak =
                            0.5 + 0.5 * noise2(along / STREAKS.0, across / STREAKS.1, STREAK_SEED);
                        pour * (streak + (1.0 - streak) * pour * pour)
                    } else {
                        0.0
                    };
                    carried = (carried * fade).max(breaking).max(shedding).max(poured);
                    *slot = byte(carried);
                }
            }
        });
        Some(())
    }
}

/// How far water `depth` deep running at `speed` churns its surface up or
/// down at `(along, across)`: where it runs near critical, in boils as big
/// as its depth drawn out down the stream, so its churning stands no
/// steeper however deep it runs; not at all where it runs slow.
fn churn((along, across): (f64, f64), (depth, speed): (f64, f64)) -> f64 {
    if depth <= 0.0 || speed <= 0.0 {
        return 0.0;
    }
    let froude = speed / mathf::sqrt(GRAVITY * depth);
    let churning = smoothstep(CHURNING.0, CHURNING.1, froude);
    if churning <= 0.0 {
        return 0.0;
    }
    let (long, broad) = (
        (CHURN_LENGTHS.0 * depth).max(SHORTEST_BOIL),
        (CHURN_LENGTHS.1 * depth).max(SHORTEST_BOIL),
    );
    let boils = (noise2(along / long, across / broad, CHURN_SEED)
        + 0.5 * noise2(2.0 * along / long, 2.0 * across / broad, CHURN_SEED ^ 1))
        / 1.5;
    CHURN * churning * depth * boils
}

/// How far water `depth` deep heaves up or down at `(along, across)` where
/// it runs so shallow over gravel that each stone beneath shows in its
/// surface; not at all where it is deep enough to lie level over them.
fn draped((along, across): (f64, f64), depth: f64) -> f64 {
    let shallow = 1.0 - smoothstep(0.5 * DRAPED, DRAPED, depth);
    if depth <= 0.0 || shallow <= 0.0 {
        return 0.0;
    }
    DRAPE * depth * shallow * noise2(along / GRAVEL, across / GRAVEL, DRAPE_SEED)
}

/// `rise` held between `-floor` and `head`, smoothly: the surface rises no
/// higher than the stream's velocity head lets it, nor falls nearer the bed
/// than `floor` below its level.
fn held(rise: f64, head: f64, floor: f64) -> f64 {
    let limit = |rise: f64, bound: f64| {
        if bound <= 0.0 {
            0.0
        } else {
            bound * tanh(rise / bound)
        }
    };
    if rise >= 0.0 {
        limit(rise, head)
    } else {
        -limit(-rise, floor)
    }
}

/// How high water running at `speed` would rise were it stopped: its
/// velocity head.
fn head(speed: f64) -> f64 {
    speed * speed / (2.0 * GRAVITY)
}

/// A wave number of magnitude `k` over water as deep as `tanh_kh` and
/// `sech_kh` have it, the part of its answer that holds at any speed.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Deep {
    k: f64,
    tanh_kh: f64,
    sech_kh: f64,
}

impl Deep {
    /// Wave number `k` over water `depth` deep; `None` for the mean or no
    /// water, which answer nothing.
    fn at(k: f64, depth: f64) -> Option<Self> {
        if k <= 0.0 || depth <= 0.0 {
            return None;
        }
        let kh = k * depth;
        let e = mathf::exp(-2.0 * kh);
        Some(Self {
            k,
            tanh_kh: (1.0 - e) / (1.0 + e),
            sech_kh: 2.0 * mathf::exp(-kh) / (1.0 + e),
        })
    }

    /// The surface's answer to a rise of the bed and to a source of water,
    /// the wave number running `along` the stream at `speed`.
    ///
    /// Steady linear potential flow at speed `U` over a depth `h`, gravity
    /// `g` and surface tension `T`: the surface's transform answers the
    /// bed's `b` and the sources' `m` as
    ///
    /// `η = −Ω² sech(Kh) b / D + i U Ω tanh(Kh) m / (K D)`,
    /// `D = (g + T K²) K tanh(Kh) − Ω²`, `Ω = U k_along − i ε(K)`,
    ///
    /// `m` a source's strength for each metre a second the stream runs, as
    /// a stone parts water in proportion to its speed; `ε` the damping that
    /// keeps the answer to the waves the flow carries off and that the
    /// eddies lend the short ones. A wave number that runs with the flow at
    /// the speed its own waves travel (`D` near nought) is the lee wave
    /// standing behind what raised it.
    fn answer(&self, along: f64, speed: f64) -> (Complex, Complex) {
        let Self {
            k,
            tanh_kh,
            sech_kh,
        } = *self;
        let omega = Complex::new(speed * along, -(DAMPING + 2.0 * EDDIES * k * k));
        let omega2 = omega * omega;
        let gravity = (GRAVITY + TENSION * k * k) * k * tanh_kh;
        let denominator = Complex::new(gravity, 0.0) - omega2;
        let to_bed = Complex::new(-omega2.re * sech_kh, -omega2.im * sech_kh).over(denominator);
        let lifted = Complex::new(-omega.im, omega.re).scale(speed * tanh_kh / k);
        (to_bed, lifted.over(denominator))
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
