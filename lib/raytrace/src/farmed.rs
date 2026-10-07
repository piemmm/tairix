//! What a farmed land grows where, as its grids carry it: a byte at each
//! vertex naming its field's use, and for an arable field its crop and the
//! stage the season has it at; and a byte for the way its drilled rows run.
//! A category, read at the nearest vertex and never blended between them.

use core::f64::consts::{PI, TAU};

use tairix_countryside::field::Field;
use tairix_countryside::layout::Parcel;
use tairix_countryside::usage::{Crop, Usage, Use};
use tairix_util::mathf;

use crate::ground::coverage;
use crate::noise::{cell, fbm2};
use crate::sample::{mix32, unit};
use crate::tree::Season;

/// What grows at a place of a farmed land.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Grown {
    /// Nothing farmed: a way, a yard, a verge, water or the land beyond.
    #[default]
    Wild,
    /// Grazed.
    Grazed,
    /// Grass grown for hay, standing.
    Mown,
    /// Grass grown for hay, cut and baled.
    Hayed,
    Orchard,
    Vineyard,
    Woodlot,
    /// Left to grow over.
    Overgrown,
    /// An arable field: its crop, and how far the season has it.
    Sown(Crop, Stage),
}

/// How far a sown field has come.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Drilled into a fine seed bed, nothing showing yet.
    Drilled,
    /// Its young shoots up in their rows.
    Shooting,
    /// Grown green to its height.
    Green,
    /// In flower.
    Flowering,
    /// Ripe, ready to cut.
    Ripe,
    /// Cut, its stubble left.
    Stubble,
    /// Ploughed.
    Ploughed,
}

pub(crate) const CROPS: [Crop; 6] = [
    Crop::Wheat,
    Crop::Barley,
    Crop::Oats,
    Crop::Maize,
    Crop::Rapeseed,
    Crop::Ley,
];
const STAGES: [Stage; 7] = [
    Stage::Drilled,
    Stage::Shooting,
    Stage::Green,
    Stage::Flowering,
    Stage::Ripe,
    Stage::Stubble,
    Stage::Ploughed,
];
const UNSOWN: [Grown; 8] = [
    Grown::Wild,
    Grown::Grazed,
    Grown::Mown,
    Grown::Hayed,
    Grown::Orchard,
    Grown::Vineyard,
    Grown::Woodlot,
    Grown::Overgrown,
];

/// The first code a sown field takes, and how many each crop's stages span.
const SOWN: u8 = 16;
const STAGE_SPAN: u8 = 8;
/// Past the last code any growth takes, so a table by growth need hold no
/// more.
pub(crate) const CODES: usize = 64;

const _: () = assert!(
    SOWN as usize + STAGE_SPAN as usize * CROPS.len() <= CODES
        && STAGES.len() <= STAGE_SPAN as usize
        && UNSOWN.len() <= SOWN as usize
);

impl Grown {
    /// Its byte, as a grid carries it.
    pub(crate) fn code(self) -> u8 {
        let index = |of: usize| u8::try_from(of).unwrap_or(0);
        match self {
            Self::Sown(crop, stage) => {
                let crop = CROPS.iter().position(|&kind| kind == crop).map_or(0, index);
                let stage = STAGES
                    .iter()
                    .position(|&kind| kind == stage)
                    .map_or(0, index);
                SOWN + STAGE_SPAN * crop + stage
            }
            unsown => UNSOWN
                .iter()
                .position(|&kind| kind == unsown)
                .map_or(0, index),
        }
    }

    /// What `code` names: nothing farmed for a byte that names nothing.
    pub(crate) fn of(code: u8) -> Self {
        if code < SOWN {
            return UNSOWN.get(usize::from(code)).copied().unwrap_or_default();
        }
        let (crop, stage) = ((code - SOWN) / STAGE_SPAN, (code - SOWN) % STAGE_SPAN);
        match (CROPS.get(usize::from(crop)), STAGES.get(usize::from(stage))) {
            (Some(&crop), Some(&stage)) => Self::Sown(crop, stage),
            _ => Self::Wild,
        }
    }

    /// Whether the ground is tilled: an arable field's, which grows nothing
    /// wild.
    pub(crate) const fn tilled(self) -> bool {
        matches!(self, Self::Sown(..))
    }

    /// Whether its soil lies bare between its plants: a tilled field's but a
    /// ley's, which stands as a sward of sown grass.
    pub(crate) const fn bare(self) -> bool {
        matches!(self, Self::Sown(crop, _) if !matches!(crop, Crop::Ley))
    }
}

/// The fewest and most metres of grass a tilled field leaves along its edge:
/// the foot of its hedge, wall or fence is never drilled, and some farms
/// leave a broad margin.
const MARGIN: (f64, f64) = (0.4, 2.5);

/// What `parcel` grows in `season`, its draws its own under `seed`; and how
/// broad a margin of grass it leaves along its edge, nought where it is not
/// tilled.
pub(crate) fn sown(parcel: &Parcel, season: Season, seed: u32) -> (Grown, f64) {
    let id = parcel.field.id;
    let key = mix32(
        id.index ^ mix32(id.holding.j.cast_unsigned() ^ mix32(id.holding.i.cast_unsigned() ^ seed)),
    );
    let grown = grown(parcel.usage, season, unit(key));
    let margin = if grown.tilled() {
        MARGIN.0 + (MARGIN.1 - MARGIN.0) * unit(mix32(key ^ 0x3a))
    } else {
        0.0
    };
    (grown, margin)
}

/// The byte the way `field`'s rows run is kept in: along its length.
pub(crate) fn rows_of(field: &Field) -> u8 {
    rows_code(mathf::atan2(field.along.x, field.along.y))
}

/// What a field used as `usage` grows in `season`, `draw` a unit draw of its
/// own deciding how far its crop has come where the season leaves a choice:
/// a cereal's winter sowing taller in spring than the spring sowing beside
/// it, a summer's harvest begun on some fields, an autumn's on to ploughing
/// and drilling.
pub(crate) fn grown(usage: Usage, season: Season, draw: f64) -> Grown {
    match usage.used {
        Use::Pasture => Grown::Grazed,
        Use::Meadow if hay_cut(season) && usage.bale.is_some() && draw < 0.45 => Grown::Hayed,
        Use::Meadow => Grown::Mown,
        Use::Orchard => Grown::Orchard,
        Use::Vineyard => Grown::Vineyard,
        Use::Woodlot => Grown::Woodlot,
        Use::Overgrown => Grown::Overgrown,
        Use::Arable(crop) => Grown::Sown(crop, stage(crop, season, draw)),
    }
}

/// Whether `season` is when hay is made.
const fn hay_cut(season: Season) -> bool {
    matches!(season, Season::Summer)
}

/// Every growth a sward grows in place of its wild grass where a farmed
/// land's fields stand at it in `season`: each crop at each stage the
/// season holds, and a meadow cut for hay.
pub(crate) fn growths(season: Season) -> impl Iterator<Item = Grown> {
    let hayed = hay_cut(season).then_some(Grown::Hayed);
    CROPS
        .into_iter()
        .flat_map(move |crop| stages(crop, season).map(move |stage| Grown::Sown(crop, stage)))
        .chain(hayed)
}

/// How far `crop` has come in `season`, `draw` deciding between the stages
/// the season holds.
fn stage(crop: Crop, season: Season, draw: f64) -> Stage {
    let choices = choices(crop, season);
    choices
        .iter()
        .find(|&&(share, _)| draw < share)
        .map_or(choices[2].1, |&(_, stage)| stage)
}

/// The stages `crop` may stand at in `season`.
pub(crate) fn stages(crop: Crop, season: Season) -> impl Iterator<Item = Stage> {
    let choices = choices(crop, season);
    (0..choices.len()).filter_map(move |index| {
        let stage = choices[index].1;
        (!choices[..index]
            .iter()
            .any(|&(_, earlier)| earlier == stage))
        .then_some(stage)
    })
}

/// The stages a crop may stand at in a season, each with the share of its
/// fields standing at it or one before it.
type Choices = [(f64, Stage); 3];

const fn only(stage: Stage) -> Choices {
    [(1.0, stage); 3]
}

const fn either(share: f64, (first, second): (Stage, Stage)) -> Choices {
    [(share, first), (1.0, second), (1.0, second)]
}

/// Cereals stand green in spring, the winter sowing taller than the spring
/// sowing beside it; are ripe in summer, some fields already cut; in autumn
/// are cut, ploughed or drilled again; and in winter stand as young shoots
/// or lie ploughed. Maize is drilled late, stands green through summer and
/// is cut in autumn; rape flowers in spring and is in pod in summer.
const fn choices(crop: Crop, season: Season) -> Choices {
    match (crop, season) {
        (Crop::Wheat | Crop::Barley | Crop::Oats, Season::Spring) => {
            either(0.6, (Stage::Green, Stage::Shooting))
        }
        (Crop::Wheat | Crop::Barley | Crop::Oats, Season::Summer) => {
            either(0.65, (Stage::Ripe, Stage::Stubble))
        }
        (Crop::Wheat | Crop::Barley | Crop::Oats, Season::Autumn { .. }) => [
            (0.4, Stage::Stubble),
            (0.7, Stage::Ploughed),
            (1.0, Stage::Shooting),
        ],
        (Crop::Wheat | Crop::Barley | Crop::Oats, Season::Winter) => {
            either(0.55, (Stage::Shooting, Stage::Ploughed))
        }
        (Crop::Maize, Season::Spring) => only(Stage::Drilled),
        (Crop::Maize, Season::Autumn { .. }) | (Crop::Rapeseed, Season::Summer) => {
            either(0.5, (Stage::Ripe, Stage::Stubble))
        }
        (Crop::Maize, Season::Winter) => only(Stage::Stubble),
        (Crop::Rapeseed, Season::Spring) => only(Stage::Flowering),
        (Crop::Rapeseed, Season::Autumn { .. }) => only(Stage::Shooting),
        (Crop::Maize, Season::Summer)
        | (Crop::Rapeseed | Crop::Ley, Season::Winter)
        | (Crop::Ley, Season::Spring | Season::Autumn { .. }) => only(Stage::Green),
        (Crop::Ley, Season::Summer) => either(0.4, (Stage::Stubble, Stage::Green)),
    }
}

/// The byte the way a field's rows run is kept in: its heading, in 256ths of
/// a half turn, rows running either way along it.
pub(crate) fn rows_code(heading: f64) -> u8 {
    let turns = heading / PI;
    let half = turns - mathf::floor(turns);
    u8::try_from(mathf::round_i32(255.0 * half)).unwrap_or(0)
}

/// The unit way the rows `code` keeps run, in x and z.
pub(crate) fn rows_way(code: u8) -> (f64, f64) {
    let heading = PI * f64::from(code) / 255.0;
    (mathf::sin(heading), mathf::cos(heading))
}

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

#[cfg(test)]
#[path = "farmed_tests.rs"]
mod tests;
