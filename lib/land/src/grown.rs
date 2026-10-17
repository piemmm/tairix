//! What a farmed land grows where, as a land's vertices carry it: a byte
//! naming its field's use, and for an arable field its crop and the stage the
//! season has it at; and a byte for the way its drilled rows run. A category,
//! read at the nearest vertex and never blended between them.

use core::f64::consts::PI;

use tairix_countryside::field::Field;
use tairix_countryside::layout::Parcel;
use tairix_countryside::usage::{Crop, Usage, Use};
use tairix_mix::{mix32, unit};
use tairix_util::mathf;

use crate::Season;

/// What grows at a place of a farmed land.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Grown {
    /// Nothing farmed: a way, a yard, a verge, water or the land beyond.
    #[default]
    Wild,
    /// Grazed.
    Grazed,
    /// Grass grown for hay, standing.
    Mown,
    /// Grass grown for hay, cut and baled.
    Hayed,
    /// Fruit trees in rows.
    Orchard,
    /// Vines on trellised rows.
    Vineyard,
    /// Trees grown for their wood.
    Woodlot,
    /// Left to grow over.
    Overgrown,
    /// An arable field: its crop, and how far the season has it.
    Sown(Crop, Stage),
}

/// How far a sown field has come.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stage {
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

/// The crops a farmed land sows, in the order their codes run.
pub const CROPS: [Crop; 6] = [
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
pub const CODES: usize = 64;

const _: () = assert!(
    SOWN as usize + STAGE_SPAN as usize * CROPS.len() <= CODES
        && STAGES.len() <= STAGE_SPAN as usize
        && UNSOWN.len() <= SOWN as usize
);

impl Grown {
    /// Its byte, as a grid carries it.
    #[must_use]
    pub fn code(self) -> u8 {
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
    #[must_use]
    pub fn of(code: u8) -> Self {
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
    #[must_use]
    pub const fn tilled(self) -> bool {
        matches!(self, Self::Sown(..))
    }

    /// Whether its soil lies bare between its plants: a tilled field's but a
    /// ley's, which stands as a sward of sown grass.
    #[must_use]
    pub const fn bare(self) -> bool {
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
#[must_use]
pub fn sown(parcel: &Parcel, season: Season, seed: u32) -> (Grown, f64) {
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
#[must_use]
pub fn rows_of(field: &Field) -> u8 {
    rows_code(mathf::atan2(field.along.x, field.along.y))
}

/// What a field used as `usage` grows in `season`, `draw` a unit draw of its
/// own deciding how far its crop has come where the season leaves a choice:
/// a cereal's winter sowing taller in spring than the spring sowing beside
/// it, a summer's harvest begun on some fields, an autumn's on to ploughing
/// and drilling.
#[must_use]
pub fn grown(usage: Usage, season: Season, draw: f64) -> Grown {
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
pub fn growths(season: Season) -> impl Iterator<Item = Grown> {
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
pub fn stages(crop: Crop, season: Season) -> impl Iterator<Item = Stage> {
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

/// The byte the way a field's rows run is kept in: its heading, in 255ths of
/// a half turn, 255 the same way as nought, rows running either way along
/// it.
#[must_use]
pub fn rows_code(heading: f64) -> u8 {
    let turns = heading / PI;
    let half = turns - mathf::floor(turns);
    u8::try_from(mathf::round_i32(255.0 * half)).unwrap_or(0)
}

/// The unit way the rows `code` keeps run, in x and z.
#[must_use]
pub fn rows_way(code: u8) -> (f64, f64) {
    let heading = PI * f64::from(code) / 255.0;
    (mathf::sin(heading), mathf::cos(heading))
}

#[cfg(test)]
#[path = "grown_tests.rs"]
mod tests;
