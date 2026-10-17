//! How a sown field is drilled: its rows, its tramlines, and a ploughed
//! field's furrow slices, drawn from the bytes a land's vertices carry for
//! what grows there and the way its rows run (`tairix_land::grown`).

use core::f64::consts::{PI, TAU};

use tairix_countryside::usage::Crop;
use tairix_land::grown::rows_way;
use tairix_mix::{mix32, unit};
use tairix_noise::{cell, fbm2};
use tairix_util::mathf;

use crate::ground::coverage;

/// How far apart a crop's rows are drilled.
pub(crate) const fn row_spacing(crop: Crop) -> f64 {
    match crop {
        Crop::Wheat | Crop::Barley | Crop::Oats | Crop::Ley => 0.125,
        Crop::Rapeseed => 0.25,
        Crop::Maize => 0.75,
    }
}

/// The widths a sprayer's boom spans, which a field's tramlines lie apart
/// by; how far apart a tramline's two wheel tracks run; and how wide each.
const BOOMS: [f64; 4] = [12.0, 18.0, 24.0, 24.0];
const GAUGE: f64 = 1.8;
const TRACK: f64 = 0.45;
/// How far either side of its row's line a seed falls.
const SCATTER: f64 = 0.012;

/// How a sown field is drilled: the unit way across its rows, how far apart
/// they lie and where one lies, and how far apart its tramlines run and
/// where one does. Drawn from its growth's and rows' bytes, so every place
/// carrying them agrees; two fields alike in both are drilled alike.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Drill {
    normal: (f64, f64),
    spacing: f64,
    phase: f64,
    tramlines: (f64, f64),
    key: u32,
}

/// How broad a furrow slice a plough turns.
pub(crate) const FURROW: f64 = 0.35;

/// Where a place lies among a ploughed field's furrow slices: which slice,
/// and how far across it from the furrow before it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Furrow {
    pub(crate) slice: u32,
    pub(crate) within: f64,
}

impl Furrow {
    /// How high the slice stands there, `0.0` in its furrow to `1.0` at its
    /// crest, which lies nearer its landside face; and how steeply it rises
    /// across, in its own height a slice's breadth.
    pub(crate) fn rise(&self) -> (f64, f64) {
        let (sin, cos) = (mathf::sin(TAU * self.within), mathf::cos(TAU * self.within));
        let height = (0.5 - 0.5 * cos + 0.3 * sin + SLICE_LOW) / SLICE_SPAN;
        let slope = PI * (sin + 0.6 * cos) / SLICE_SPAN;
        (height, slope)
    }
}

/// How far below its furrow's own level, and over how much height, a
/// slice's profile runs.
const SLICE_LOW: f64 = 0.083;
const SLICE_SPAN: f64 = 1.166;

impl Drill {
    /// How a field keeping `(grown, rows)` is drilled, its rows `spacing`
    /// apart.
    pub(crate) fn of(spacing: f64, (grown, rows): (u8, u8)) -> Self {
        let (x, z) = rows_way(rows);
        let key = mix32((u32::from(grown) << 8 | u32::from(rows)) ^ 0x7d1a_93c5);
        let boom = BOOMS[(key % 4) as usize];
        Self {
            normal: (z, -x),
            spacing,
            phase: spacing * unit(mix32(key ^ 1)),
            tramlines: (boom, boom * unit(mix32(key ^ 2))),
            key,
        }
    }

    /// Where `(x, z)` lies among the furrow slices of the field ploughed
    /// along its rows: their lines wander a hand's breadth over a few
    /// strides.
    pub(crate) fn furrow(&self, (x, z): (f64, f64)) -> Furrow {
        let (across, along) = (self.across((x, z)), self.along((x, z)));
        let wander = 0.22 * fbm2(along / 3.5, across / 3.5, self.key ^ 0x2b7, (2, 0.5, 2.0));
        let (slice, within) = cell(across / FURROW + wander);
        Furrow { slice, within }
    }

    /// The key its field's own draws are taken under.
    pub(crate) const fn key(&self) -> u32 {
        self.key
    }

    /// How far apart its rows lie.
    pub(crate) const fn spacing(&self) -> f64 {
        self.spacing
    }

    /// How far across the rows `(x, z)` lies, and how far along them.
    pub(crate) fn across(&self, (x, z): (f64, f64)) -> f64 {
        x * self.normal.0 + z * self.normal.1
    }

    pub(crate) fn along(&self, (x, z): (f64, f64)) -> f64 {
        z * self.normal.0 - x * self.normal.1
    }

    /// The place `across` its rows and `along` them.
    pub(crate) fn place(&self, (across, along): (f64, f64)) -> (f64, f64) {
        let (x, z) = self.normal;
        (across * x - along * z, across * z + along * x)
    }

    /// How many rows across from the one it counts from `(x, z)` lies.
    pub(crate) fn rows_from(&self, at: (f64, f64)) -> f64 {
        (self.across(at) - self.phase) / self.spacing
    }

    /// How far across the rows its `row`th row runs.
    pub(crate) fn row(&self, row: f64) -> f64 {
        self.phase + row * self.spacing
    }

    /// The unit way across its rows, in x and z.
    pub(crate) const fn normal(&self) -> (f64, f64) {
        self.normal
    }

    /// `(x, z)` moved `pull` of the way onto the line of its nearest row,
    /// scattered there by `draw`, a unit draw of the seed's own.
    pub(crate) fn sown(&self, (x, z): (f64, f64), pull: f64, draw: f64) -> (f64, f64) {
        let offset = self.across((x, z)) - self.phase;
        let row = mathf::round(offset / self.spacing) * self.spacing;
        let moved = pull * (row - offset + SCATTER * (2.0 * draw - 1.0));
        (x + moved * self.normal.0, z + moved * self.normal.1)
    }

    /// How much of a footprint `footprint` across about `(x, z)` a tramline's
    /// wheel tracks cover, where nothing grows.
    pub(crate) fn tracked(&self, (x, z): (f64, f64), footprint: f64) -> f64 {
        let (boom, phase) = self.tramlines;
        let offset = self.across((x, z)) - phase;
        let within = offset - mathf::round(offset / boom) * boom;
        coverage((within.abs() - 0.5 * GAUGE).abs(), 0.5 * TRACK, footprint)
    }

    /// The share of seeds drawn evenly over a square `side` across and then
    /// moved `pull` of the way onto their rows that stay in the square; the
    /// rest land in the squares about it, which a square drawing its own
    /// seeds must make up for.
    pub(crate) fn kept(&self, side: f64, pull: f64) -> f64 {
        let (a, b) = (self.normal.0.abs(), self.normal.1.abs());
        let side = side.max(1e-9);
        // A seed moves across its rows by an even draw within `reach`; it
        // stays as often as the square moved that far still overlaps itself.
        let reach = 0.5 * pull * self.spacing;
        if reach <= 1e-12 {
            return 1.0;
        }
        let most = reach.min(side / a.max(b).max(1e-9));
        let overlap = most - (a + b) * most * most / (2.0 * side)
            + a * b * most * most * most / (3.0 * side * side);
        (overlap / reach).clamp(1e-3, 1.0)
    }
}
