//! The floor: a grid of glowing lines streaming towards the viewer and fading
//! into the haze along the horizon, with the sun's light rippling on it.
//!
//! Every line is a train of pulses in the floor's own coordinates, and each
//! pixel takes the share of its footprint they cover — across the frame's
//! exposure too, for the lines the flight carries towards the viewer — so the
//! grid is exact at every distance, and where its lines crowd too finely to be
//! drawn their contrast gives way to their mean before they could shimmer.
//! Each line glows about its core, its glow falling away with the distance
//! across it, and where two cross their light adds.
//!
//! The reflection is the sun mirrored in the floor and drawn out towards the
//! viewer: a column that sways, broken into bands by ripples running with the
//! grid, and brightest along its core by the horizon.

use core::f64::consts::TAU;
use core::ops::Range;

use tairix_inline::ArrayVec;
use tairix_parallel::JobRunner;
use tairix_raster::Pixel;
use tairix_util::mathf::{self, Phasor};
use tairix_wm::Surface;

use super::sky::disc_colour;
use super::{
    column, dither_biases, faded, fill_dithered, index, paint_rows, swept, Moment, Rgb, View,
    CAMERA,
};

/// The floor's own dark, the haze along the horizon, and how far the haze
/// falls off by a factor of `e`, in screen heights.
const FLOOR: Rgb = Rgb::new(1.0, 3.0, 9.0);
const HAZE: Rgb = Rgb::new(50.0, 76.0, 134.0);
const HAZE_FALL: f64 = 0.04;

/// A line's core and glow at full strength, what a crossing of two full
/// cores adds, and the depth over which the lines fade by a factor of `e`,
/// in cells.
const LINE: Rgb = Rgb::new(58.0, 108.0, 235.0);
const LINE_GLOW: Rgb = Rgb::new(20.0, 44.0, 120.0);
const CROSSING: Rgb = Rgb::new(90.0, 110.0, 130.0);
const LINE_FOG: f64 = 22.0;

/// A line's core width far off, in logical pixels, and how much wider it is
/// one cell ahead; how far its glow reaches either side, in cores; and the
/// most of its period a core may fill.
const LINE_WIDTH: f64 = 1.2;
const LINE_NEAR: f64 = 3.2;
const GLOW_REACH: f64 = 2.4;
const MOST_DUTY: f64 = 0.5;

/// The spacing in pixels at which a pattern has given all its contrast to
/// its mean, and at which it keeps all of it.
const FINEST: f64 = 2.5;
const CLEAR: f64 = 6.0;

/// Columns a row is shaded in at a time, the light the lines lend them
/// gathered first; the most lines, and runs of columns to shade, one such
/// piece keeps track of before it shades the whole piece instead.
const PIECE: usize = 256;
const PIECE_LINES: usize = 64;
const PIECE_RUNS: usize = 72;

/// How far the reflection is drawn out towards the viewer: a row mirrors the
/// sky this many times nearer the horizon than it lies below it.
const STRETCH: f64 = 2.4;

/// The reflection's width, as a share of the mirrored disc's, and its soft
/// edge, as a share of its half-width.
const REFLECTION_WIDTH: f64 = 0.82;
const REFLECTION_SOFT: f64 = 0.3;

/// How much of the disc's light the reflection keeps by the horizon and close
/// by, and how far it falls between them, in screen heights: grazing light
/// reflects the more.
const FAR_SHINE: f64 = 1.0;
const NEAR_SHINE: f64 = 0.52;
const SHINE_FALL: f64 = 0.14;

/// The haze the reflection takes by the horizon, at most.
const REFLECTION_HAZE: Rgb = Rgb::new(236.0, 158.0, 122.0);
const REFLECTION_HAZE_SHARE: f64 = 0.45;

/// The ripples that break the reflection into bands: waves along the floor's
/// depth whose crests slant across it, so where they cross the breaks are as
/// irregular as water's.
const RIPPLES: [Ripple; 3] = [
    Ripple {
        wave: Wave {
            amplitude: 0.45,
            wavelength: 0.5,
            drift: 0.12,
            phase: 0.0,
        },
        slant: 0.35,
    },
    Ripple {
        wave: Wave {
            amplitude: 0.32,
            wavelength: 0.26,
            drift: -0.2,
            phase: 2.1,
        },
        slant: -0.8,
    },
    Ripple {
        wave: Wave {
            amplitude: 0.23,
            wavelength: 0.13,
            drift: 0.3,
            phase: 4.0,
        },
        slant: 1.9,
    },
];

/// The ripples' finest wavelengths along the floor and across it, in cells:
/// what decides how finely they crowd towards the horizon.
const SHORTEST_RIPPLE: f64 = 0.13;
const NARROWEST_RIPPLE: f64 = 1.0 / 1.9;

/// Where a ripple breaks the reflection, how softly, and how much of the
/// column a break leaves lit on average.
const BREAK: f64 = -0.1;
const BREAK_SOFT: f64 = 0.22;
const UNBROKEN: f64 = 0.6;

/// How the column sways, as a share of its half-width, and how far from the
/// horizon the sway has grown to its full, in screen heights.
const WOBBLE: [Wave; 2] = [
    Wave {
        amplitude: 0.07,
        wavelength: 0.9,
        drift: -0.15,
        phase: 1.7,
    },
    Wave {
        amplitude: 0.04,
        wavelength: 0.35,
        drift: 0.25,
        phase: 0.3,
    },
];
const WOBBLE_GROWTH: f64 = 0.15;

/// How the column's width wavers, as a share of it.
const SPREAD: Wave = Wave {
    amplitude: 0.1,
    wavelength: 0.41,
    drift: 0.18,
    phase: 2.6,
};

/// How narrow the column is by the horizon, as a share of its width close by,
/// and how far below the horizon it widens, in screen heights.
const NARROWEST: f64 = 0.35;
const NARROWING: f64 = 0.1;

/// How far the ripples fray the column's edges, as a share of its half-width
/// a unit of their height.
const FRAYING: f64 = 0.14;

/// The glint along the column's core: its strength, its width as a share of
/// the half-width, and how far it falls off, in screen heights.
const GLINT: f64 = 0.55;
const GLINT_WIDTH: f64 = 0.3;
const GLINT_FALL: f64 = 0.1;

/// A wave travelling along the floor:
/// `amplitude · sin(τ·(at − drift·t) / wavelength + phase)`, for `at` in
/// cells and `t` in seconds.
#[derive(Copy, Clone, Debug)]
struct Wave {
    amplitude: f64,
    wavelength: f64,
    drift: f64,
    phase: f64,
}

impl Wave {
    fn at(self, at: f64, time: f64) -> f64 {
        self.amplitude * mathf::sin(self.angle(at, time))
    }

    /// Where the wave's phase stands at `at` cells, `time` seconds on.
    fn angle(self, at: f64, time: f64) -> f64 {
        TAU * (at - self.drift * time) / self.wavelength + self.phase
    }
}

/// A ripple: a wave along the floor's depth whose crests turn `slant` times a
/// cell across the floor.
#[derive(Copy, Clone, Debug)]
struct Ripple {
    wave: Wave,
    slant: f64,
}

/// The floor.
pub(super) struct Floor {
    view: View,
}

/// The lines running towards the horizon, as one floor row crosses them.
///
/// The line `offset` cells right of the flight runs down the row as a sheared
/// band, `offset · top` pixels right of the centre at the row's top edge and
/// `offset · bottom` at its bottom. A pixel takes the share of it the band's
/// core covers averaged down the row, so a line shallow enough to cross many
/// columns within one row is drawn as the unbroken stroke it is rather than a
/// dash in each; the glow about the core is soft enough to be taken at the
/// pixel's centre. Where lines crowd too finely across their own direction to
/// be drawn, their contrast gives way to their mean.
#[derive(Copy, Clone, Debug)]
struct Crossing {
    centre: f64,
    sway: f64,
    /// Pixels a cell of offset carries a line across at the row's top edge
    /// and midway, and the cells a pixel spans at the top and bottom edges.
    top: f64,
    middle: f64,
    per_top: f64,
    per_bottom: f64,
    /// A line's core, and how far its glow reaches either side of it, in
    /// pixels across it.
    core: f64,
    glow: f64,
    /// How far along the row a line's glow can reach from its centre, in
    /// cells at the row's top edge.
    reach: f64,
    /// How far either side of the centre the lines keep all their contrast,
    /// and from how far they keep none, in pixels.
    clear: f64,
    faint: f64,
}

/// One line as it crosses a row.
#[derive(Clone, Debug)]
struct Shape {
    /// Where its centre stands at the row's top edge, right of the centre
    /// column, and how far it moves across by the bottom, in pixels.
    from: f64,
    shear: f64,
    /// Its core, in pixels along the row, and the inverse of how far along
    /// the row its glow reaches.
    core: f64,
    per_glow: f64,
    /// The columns its core and its glow reach.
    cored: Range<u32>,
    glowing: Range<u32>,
}

/// What one floor row is lit by.
#[derive(Copy, Clone, Debug)]
struct Row {
    /// How far below the horizon the row's centre lies, in pixels, and how
    /// far ahead the floor there is, in cells.
    drop: f64,
    depth: f64,
    /// What every pixel of the row takes: the floor, the haze, and the lines
    /// across it.
    light: Rgb,
    /// The light a line running towards the horizon lends where its core
    /// wholly covers a pixel, crossings of the lines across the row included,
    /// and where its glow does.
    core_light: Rgb,
    glow_light: Rgb,
    /// The lines running towards the horizon; `None` when they are only
    /// their mean here, which `light` already holds.
    towards: Option<Crossing>,
}

/// The sun's reflection across one floor row.
#[derive(Clone, Debug)]
struct Reflection {
    /// The columns it reaches.
    columns: Range<u32>,
    /// Its centre, half-width and soft edge, in pixels, and how far from its
    /// centre it is lit whole however the ripples fray its edges.
    centre: f64,
    half: f64,
    soft: f64,
    inside: f64,
    /// Its light at full strength, and the share of it the row keeps.
    light: Rgb,
    shine: f64,
    /// The ripples at the next column, each turning column by column, and
    /// how much of their contrast the breaks they make keep.
    ripples: [Phasor; 3],
    contrast: f64,
    /// The glint's strength and half-width, in pixels.
    glint: f64,
    glint_width: f64,
}

/// The runs of columns one piece of a row shades, and the lines it gathered
/// the light of.
struct Piece {
    columns: Range<u32>,
    runs: ArrayVec<Range<u32>, PIECE_RUNS>,
    /// Whether the runs could not all be kept, so the whole piece is shaded.
    whole: bool,
}

impl Floor {
    pub(super) const fn new(view: View) -> Self {
        Self { view }
    }

    /// Paint every floor row as the flight stands at `moment`, spread across
    /// `runner`.
    pub(super) fn paint(&self, surface: &mut Surface, runner: &dyn JobRunner, moment: Moment) {
        paint_rows(surface, self.view.floor_rows(), runner, &|y, span| {
            self.row(y, span, moment);
        });
    }

    /// Paint floor row `y`, whole: the row's own light, then piece by piece
    /// whatever takes more than it.
    fn row(&self, y: u32, span: &mut [Pixel], moment: Moment) {
        let view = &self.view;
        let row = Row::new(view, y, moment);
        let biases = dither_biases(y);
        fill_dithered(span, 0, row.light, &biases);
        let mut reflection = Reflection::new(view, &row, moment);
        let lit = reflection
            .as_ref()
            .map_or(0..0, |reflection| reflection.columns.clone());
        let reached = if row.towards.is_some() {
            0..view.width
        } else {
            lit.clone()
        };
        let mut lines = [(0.0, 0.0); PIECE];
        let mut start = reached.start;
        while start < reached.end {
            let end = start
                .saturating_add(u32::try_from(PIECE).unwrap_or(u32::MAX))
                .min(reached.end);
            let piece = Piece::gathered(&row, start..end, &lit, &mut lines);
            piece.shade(&row, reflection.as_mut(), &lines, (span, &biases));
            start = end;
        }
    }
}

impl Piece {
    /// The columns `columns` of `row` must shade, the lines' light across
    /// them gathered into `lines`; `lit` is where the reflection lies.
    fn gathered(
        row: &Row,
        columns: Range<u32>,
        lit: &Range<u32>,
        lines: &mut [(f64, f64); PIECE],
    ) -> Self {
        let mut piece = Self {
            columns: columns.clone(),
            runs: ArrayVec::new(),
            whole: false,
        };
        piece.run(lit.start.max(columns.start)..lit.end.min(columns.end));
        let mut shapes: ArrayVec<Shape, PIECE_LINES> = ArrayVec::new();
        let mut kept = true;
        if let Some(towards) = &row.towards {
            for outer in towards.crowded(columns.clone()) {
                piece.run(outer);
            }
            for shape in towards.shapes(columns.clone()) {
                piece.run(shape.glowing.clone());
                kept &= shapes.try_push(shape).is_ok();
            }
        }
        piece.merge();
        for run in piece.shaded() {
            if let Some(cleared) = lines.get_mut(piece.slots(run)) {
                cleared.fill((0.0, 0.0));
            }
        }
        if let Some(towards) = &row.towards {
            if kept {
                for shape in &shapes {
                    towards.lend(shape, &columns, lines);
                }
            } else {
                for shape in towards.shapes(columns.clone()) {
                    towards.lend(&shape, &columns, lines);
                }
            }
        }
        piece
    }

    /// Note `run` as columns to shade, or give up keeping runs apart.
    fn run(&mut self, run: Range<u32>) {
        if !run.is_empty() && !self.whole && self.runs.try_push(run).is_err() {
            self.whole = true;
        }
    }

    /// Order the runs and join those that meet.
    fn merge(&mut self) {
        if self.whole {
            return;
        }
        self.runs.sort_unstable_by_key(|run| run.start);
        let mut joined: ArrayVec<Range<u32>, PIECE_RUNS> = ArrayVec::new();
        for run in self.runs.iter().cloned() {
            match joined.last_mut() {
                Some(last) if run.start <= last.end => last.end = last.end.max(run.end),
                _ => {
                    let _ = joined.try_push(run);
                }
            }
        }
        self.runs = joined;
    }

    /// The runs to shade, in order.
    fn shaded(&self) -> &[Range<u32>] {
        if self.whole {
            core::slice::from_ref(&self.columns)
        } else {
            &self.runs
        }
    }

    /// Where `run` lies among the piece's gathered lines.
    fn slots(&self, run: &Range<u32>) -> Range<usize> {
        let slot = |x: u32| index(x.saturating_sub(self.columns.start));
        slot(run.start)..slot(run.end)
    }

    /// Shade the piece's runs of `row` onto `span`, rounded at `biases`,
    /// from the lines' light gathered in `lines` and the sun's `reflection`.
    fn shade(
        &self,
        row: &Row,
        mut reflection: Option<&mut Reflection>,
        lines: &[(f64, f64); PIECE],
        (span, biases): (&mut [Pixel], &[f64; 8]),
    ) {
        for run in self.shaded() {
            let (Some(pixels), Some(gathered)) = (
                span.get_mut(index(run.start)..index(run.end)),
                lines.get(self.slots(run)),
            ) else {
                continue;
            };
            for ((x, pixel), &gathered) in run.clone().zip(pixels).zip(gathered) {
                let mut light = row.light;
                if let Some(towards) = &row.towards {
                    let (core, glow) = towards.faded(x, gathered);
                    if core > 0.0 || glow > 0.0 {
                        light = light + row.core_light * core + row.glow_light * glow;
                    }
                }
                if let Some(reflection) = reflection.as_deref_mut() {
                    if reflection.columns.contains(&x) {
                        light = light + reflection.next(x);
                    }
                }
                *pixel = light.pixel(biases[index(x & 7)]);
            }
        }
    }
}

impl Row {
    /// What floor row `y` of `view` is lit by as the flight stands at
    /// `moment`.
    fn new(view: &View, y: u32, moment: Moment) -> Self {
        let tall = f64::from(view.height);
        let top = f64::from(y - view.horizon);
        let depth = view.depth(top + 0.5);
        let fog = mathf::exp(-depth / LINE_FOG);
        let thickness = view.pixel * (LINE_WIDTH + LINE_NEAR / depth);
        // Rows a cell of depth spans here, and the depths the row's own edges
        // stand at.
        let rows_per_cell = CAMERA * view.focal / (depth * depth);
        let near = view.depth(top + 1.0) + moment.flown;
        let far = if top > 0.0 {
            view.depth(top) + moment.flown
        } else {
            f64::INFINITY
        };
        let contrast = contrast(rows_per_cell);
        let core = mathf::fmin(thickness / rows_per_cell, MOST_DUTY);
        let across = if far.is_finite() {
            faded(swept(near, far, core, moment.travel), core, contrast)
        } else {
            core
        };
        let reach = GLOW_REACH * thickness;
        let glowing = glow_across(view, top + 0.5, moment.flown, reach);
        let across_glow = faded(glowing, reach / rows_per_cell, contrast);
        let towards = Crossing::new(view, top, moment.sway, thickness);
        let haze = HAZE * mathf::exp(-top / (HAZE_FALL * tall));
        let mut light = FLOOR + haze + LINE * (fog * across) + LINE_GLOW * (fog * across_glow);
        // Lines crowded past drawing even at the centre, as they always are on
        // the row the horizon runs along, are their mean alone, the same across
        // the whole row.
        let (core_light, glow_light) = (LINE * fog + CROSSING * (fog * across), LINE_GLOW * fog);
        let (clearest, core, glow) = towards.crowding(0.0);
        let towards = if top > 0.0 && clearest > 0.0 {
            Some(towards)
        } else {
            light = light + core_light * core + glow_light * glow;
            None
        };
        Self {
            drop: top + 0.5,
            depth,
            light,
            core_light,
            glow_light,
            towards,
        }
    }
}

impl Crossing {
    /// The lines seen from the row `top` pixels below the horizon on `view`,
    /// the flight swayed `sway` cells off the centre line, each line's core
    /// `core` pixels across it.
    fn new(view: &View, top: f64, sway: f64, core: f64) -> Self {
        let glow = core * GLOW_REACH;
        let widest = mathf::fmax(view.centre, f64::from(view.width) - view.centre);
        let middle = (top + 0.5) / CAMERA;
        // How far from the centre lines stand `apart` pixels apart across
        // their own direction.
        let apart_at = |apart: f64| {
            let ratio = middle / apart;
            if ratio <= 1.0 {
                0.0
            } else {
                middle * CAMERA * mathf::sqrt(ratio * ratio - 1.0)
            }
        };
        let shear = widest / (middle * CAMERA);
        Self {
            centre: view.centre,
            sway,
            top: top / CAMERA,
            middle,
            per_top: CAMERA / top,
            per_bottom: CAMERA / (top + 1.0),
            core,
            glow,
            reach: (glow * mathf::sqrt(1.0 + shear * shear) + 1.0) * CAMERA / top,
            clear: apart_at(CLEAR),
            faint: apart_at(FINEST),
        }
    }

    /// How far down one row a line `rel` pixels right of the centre moves
    /// across it, in pixels.
    fn shear(&self, rel: f64) -> f64 {
        rel / (self.middle * CAMERA)
    }

    /// Where lines stand `rel` pixels right of the centre: how much of their
    /// contrast they keep, and the shares of a pixel their cores and glows
    /// cover on average.
    fn crowding(&self, rel: f64) -> (f64, f64, f64) {
        let shear = self.shear(rel);
        let apart = self.middle / mathf::sqrt(1.0 + shear * shear);
        (contrast(apart), self.core / apart, self.glow / apart)
    }

    /// The light the lines lend column `x`, their cores and glows covering
    /// `gathered` of it: that, faded towards their mean where they crowd, and
    /// their mean alone past drawing.
    fn faded(&self, x: u32, gathered: (f64, f64)) -> (f64, f64) {
        let rel = f64::from(x) + 0.5 - self.centre;
        if mathf::fabs(rel) <= self.clear {
            return gathered;
        }
        let (contrast, core, glow) = self.crowding(rel);
        (
            faded(gathered.0, core, contrast),
            faded(gathered.1, glow, contrast),
        )
    }

    /// The columns of `columns` where the lines crowd enough to give any of
    /// their contrast to their mean: every one of them takes light of the
    /// lines, however far from one it lies.
    fn crowded(&self, columns: Range<u32>) -> [Range<u32>; 2] {
        let most = columns.end;
        let at = |rel: f64| column(self.centre + rel, most).max(columns.start);
        [columns.start..at(-self.clear), at(self.clear)..columns.end]
    }

    /// The lines whose light reaches `columns` where they keep any contrast,
    /// from the left, each as it crosses the row.
    fn shapes(&self, columns: Range<u32>) -> impl Iterator<Item = Shape> + '_ {
        let most = columns.end;
        let near = column(self.centre - self.faint, most).max(columns.start)
            ..column(self.centre + self.faint, most).max(columns.start);
        let (first, last) = if near.is_empty() {
            (1, 0)
        } else {
            let edge = |x: u32| f64::from(x) + 0.5 - self.centre;
            let (low, _) = self.bounds(edge(near.start));
            let (_, high) = self.bounds(edge(near.end - 1));
            (
                mathf::round_i32(mathf::ceil(low - 0.5)),
                mathf::round_i32(mathf::floor(high - 0.5)),
            )
        };
        (first..=last)
            .map(move |line| self.shape(line, &near))
            .filter(|shape| !shape.glowing.is_empty())
    }

    /// Line `line` as it crosses the row, its reach held to `columns`.
    fn shape(&self, line: i32, columns: &Range<u32>) -> Shape {
        let offset = f64::from(line) + 0.5 - self.sway;
        let shear = offset / CAMERA;
        let stretch = mathf::sqrt(1.0 + shear * shear);
        let (from, core) = (offset * self.top, self.core * stretch);
        let (left, right) = if shear < 0.0 {
            (from + shear, from)
        } else {
            (from, from + shear)
        };
        let glow = self.glow * stretch;
        let held = |from: f64, to: f64| {
            let most = columns.end;
            column(mathf::floor(self.centre + from), most).max(columns.start)
                ..column(mathf::ceil(self.centre + to), most).max(columns.start)
        };
        Shape {
            from,
            shear,
            core,
            per_glow: 1.0 / glow,
            cored: held(left - core / 2.0 - 0.5, right + core / 2.0 + 0.5),
            glowing: held(
                mathf::fmin(left - core / 2.0, from + shear / 2.0 - glow) - 0.5,
                mathf::fmax(right + core / 2.0, from + shear / 2.0 + glow) + 0.5,
            ),
        }
    }

    /// Add the light `shape` lends the columns of `piece` into `lines`.
    fn lend(&self, shape: &Shape, piece: &Range<u32>, lines: &mut [(f64, f64); PIECE]) {
        let slot = |x: u32| index(x.saturating_sub(piece.start));
        let rel = |x: u32| f64::from(x) + 0.5 - self.centre;
        // The glow is soft enough to be taken at each pixel's centre.
        let glowing = shape.glowing.start.max(piece.start)..shape.glowing.end.min(piece.end);
        let mut apart = shape.from + shape.shear / 2.0 - rel(glowing.start);
        if let Some(glows) = lines.get_mut(slot(glowing.start)..slot(glowing.end)) {
            for glow in glows {
                glow.1 += mathf::fmax(1.0 - mathf::fabs(apart) * shape.per_glow, 0.0);
                apart -= 1.0;
            }
        }
        let cored = shape.cored.start.max(piece.start)..shape.cored.end.min(piece.end);
        let mut from = shape.from - rel(cored.start);
        if let Some(cores) = lines.get_mut(slot(cored.start)..slot(cored.end)) {
            for core in cores {
                core.0 += swept_box(from, shape.shear, shape.core);
                from -= 1.0;
            }
        }
    }

    /// The span of lines, in cells across the floor, whose glows can reach
    /// the pixel `rel` pixels right of the centre at some depth down the row.
    fn bounds(&self, rel: f64) -> (f64, f64) {
        let (at_top, at_bottom) = (
            self.sway + rel * self.per_top,
            self.sway + rel * self.per_bottom,
        );
        (
            mathf::fmin(at_top, at_bottom) - self.reach,
            mathf::fmax(at_top, at_bottom) + self.reach,
        )
    }
}

/// The share of a pixel a band `width` pixels wide covers, averaged over its
/// centre moving from `from` to `from + shear` pixels right of the pixel's
/// own: the exact area a sheared band leaves in the pixel.
fn swept_box(from: f64, shear: f64, width: f64) -> f64 {
    if mathf::fabs(shear) < 1e-6 {
        return box_overlap(from + shear / 2.0, width);
    }
    (box_integral(from + shear, width) - box_integral(from, width)) / shear
}

/// How much of a pixel a band `width` pixels wide covers, its centre `at`
/// pixels right of the pixel's.
fn box_overlap(at: f64, width: f64) -> f64 {
    let half = width / 2.0;
    mathf::fmax(
        mathf::fmin(0.5, at + half) - mathf::fmax(-0.5, at - half),
        0.0,
    )
}

/// The integral of [`box_overlap`] over every centre left of `at`.
fn box_integral(at: f64, width: f64) -> f64 {
    let outer = f64::midpoint(1.0, width);
    let inner = mathf::fabs(1.0 - width) / 2.0;
    let plateau = mathf::fmin(1.0, width);
    if at <= -outer {
        0.0
    } else if at <= -inner {
        (at + outer) * (at + outer) / 2.0
    } else if at <= inner {
        plateau * plateau / 2.0 + plateau * (at + inner)
    } else if at < outer {
        width - (outer - at) * (outer - at) / 2.0
    } else {
        width
    }
}

impl Reflection {
    /// The sun's reflection across `row` of `view` as the flight stands at
    /// `moment`, or `None` for a row the mirrored disc does not reach.
    fn new(view: &View, row: &Row, moment: Moment) -> Option<Self> {
        let tall = f64::from(view.height);
        let (sun, radius) = view.sun;
        let mirrored = f64::from(view.horizon) - row.drop / STRETCH;
        let across = mirrored - sun;
        if mathf::fabs(across) >= radius {
            return None;
        }
        let at = row.depth + moment.flown;
        // A glitter path narrows towards the horizon: there a facet must tilt
        // further to throw the sun's light aside.
        let narrowed =
            NARROWEST + (1.0 - NARROWEST) * (1.0 - mathf::exp(-row.drop / (NARROWING * tall)));
        let grown = mathf::smoothstep(row.drop / (WOBBLE_GROWTH * tall));
        let spread = 1.0 + SPREAD.at(at, moment.time) * grown;
        let half =
            REFLECTION_WIDTH * narrowed * spread * mathf::sqrt(radius * radius - across * across);
        let wobble: f64 = WOBBLE.iter().map(|wave| wave.at(at, moment.time)).sum();
        let centre = view.centre + half * wobble * grown;
        let step = row.depth / view.focal;
        let finest = mathf::fmin(
            SHORTEST_RIPPLE * CAMERA * view.focal / (row.depth * row.depth),
            NARROWEST_RIPPLE / step,
        );
        let columns = column(mathf::floor(centre - half), view.width)
            ..column(mathf::ceil(centre + half), view.width);
        // The ripples run across the floor, which sways beneath the column.
        let lateral = moment.sway + (f64::from(columns.start) + 0.5 - view.centre) * step;
        let ripples = RIPPLES.map(|ripple| {
            let across = TAU * ripple.slant;
            Phasor::new(
                ripple.wave.amplitude,
                ripple.wave.angle(at, moment.time) + across * lateral,
                across * step,
            )
        });
        let hazed = REFLECTION_HAZE_SHARE * mathf::exp(-row.drop / (HAZE_FALL * tall));
        Some(Self {
            columns,
            centre,
            half,
            soft: REFLECTION_SOFT * half,
            inside: half * (1.0 - FRAYING - REFLECTION_SOFT),
            light: disc_colour(view, mirrored).mix(REFLECTION_HAZE, hazed),
            shine: NEAR_SHINE
                + (FAR_SHINE - NEAR_SHINE) * mathf::exp(-row.drop / (SHINE_FALL * tall)),
            ripples,
            contrast: contrast(finest),
            glint: GLINT * mathf::exp(-row.drop / (GLINT_FALL * tall)),
            glint_width: GLINT_WIDTH * half,
        })
    }

    /// The light the reflection lends column `x`, the column after the one it
    /// was last asked for, its ripples turning on to the next.
    fn next(&mut self, x: u32) -> Rgb {
        let height = self.ripples.iter().map(Phasor::value).sum();
        self.ripples.iter_mut().for_each(Phasor::advance);
        self.light * self.strength(x, height)
    }

    /// The share of its full light the reflection shows at column `x`, where
    /// the ripples stand `height` high.
    fn strength(&self, x: u32, height: f64) -> f64 {
        let apart = mathf::fabs(f64::from(x) + 0.5 - self.centre);
        let edge = if apart < self.inside {
            1.0
        } else {
            mathf::smoothstep((self.half * (1.0 + FRAYING * height) - apart) / self.soft)
        };
        let unbroken = mathf::smoothstep((height - BREAK) / BREAK_SOFT + 0.5);
        let glinting = 1.0 - apart / self.glint_width;
        let glint = if glinting > 0.0 {
            self.glint * glinting * glinting
        } else {
            0.0
        };
        edge * (faded(unbroken, UNBROKEN, self.contrast) * self.shine + glint)
    }
}

/// How much of their glow the lines across the floor lend the row whose centre
/// lies `drop` pixels below the horizon on `view`, `flown` cells into the
/// flight: each glows `reach` pixels either side of it, falling away evenly.
fn glow_across(view: &View, drop: f64, flown: f64, reach: f64) -> f64 {
    let at = view.depth(drop) + flown;
    let nearest = mathf::round_i32(mathf::floor(at));
    (nearest.saturating_sub(1)..=nearest.saturating_add(2)).fold(0.0, |glow, line| {
        let depth = f64::from(line) - flown;
        if depth <= 0.0 {
            return glow;
        }
        let apart = mathf::fabs(view.depth(depth) - drop);
        glow + mathf::fmax(1.0 - apart / reach, 0.0)
    })
}

/// How much of its contrast a pattern keeps where its lines stand `spacing`
/// pixels apart.
fn contrast(spacing: f64) -> f64 {
    mathf::smoothstep((spacing - FINEST) / (CLEAR - FINEST))
}

#[cfg(test)]
#[path = "floor_tests.rs"]
mod tests;
