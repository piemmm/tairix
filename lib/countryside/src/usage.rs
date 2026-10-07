//! What each field is used for: drawn for it alone from a region's mix of
//! uses, weighed by its ground — arable on dry, level ground near the farm,
//! pasture on slopes and wet ground, orchards on warm, dry ground near the
//! farm, vineyards on warm, dry slopes, woodlots and land left to grow over
//! on steep or wet ground far from it — and the one kind of bale a field cut
//! for hay or straw is baled in.

use crate::field::Field;
use crate::key::{Key, Stage};
use crate::plane::Point;

/// What a field is sown with.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Crop {
    /// Wheat.
    Wheat,
    /// Barley.
    Barley,
    /// Oats.
    Oats,
    /// Maize, stood tall in rows.
    Maize,
    /// Oilseed rape.
    Rapeseed,
    /// Grass sown for a few years among the crops.
    Ley,
}

/// The one kind of bale or stack a field cut for hay or straw is left in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Bale {
    /// Small oblong bales, a man's lift.
    Small,
    /// Big round bales, a tractor's.
    Round,
    /// Big oblong bales, stacked.
    Square,
    /// Sheaves stood together to dry, or hay piled in cocks.
    Stook,
}

/// What a field is used for.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Use {
    /// Ploughed and sown.
    Arable(Crop),
    /// Grazed.
    Pasture,
    /// Grass grown for hay.
    Meadow,
    /// Fruit trees in rows.
    Orchard,
    /// Vines on trellised rows.
    Vineyard,
    /// Trees grown for their wood.
    Woodlot,
    /// Land left to grow over.
    Overgrown,
}

/// How much a region holds of each use, weighed against each other: what
/// its consumer's climate, soils and custom make of its farming.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Mix {
    /// Arable's weight.
    pub arable: f64,
    /// Grazing's.
    pub pasture: f64,
    /// Hay meadow's.
    pub meadow: f64,
    /// Orchards'.
    pub orchard: f64,
    /// Vineyards'.
    pub vineyard: f64,
    /// Woodlots'.
    pub woodlot: f64,
    /// Land left to grow over's.
    pub overgrown: f64,
}

/// What a field is used for, and how a cut one is baled.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Usage {
    /// Its use.
    pub used: Use,
    /// The bale or stack it is left in once cut; `None` where it is never
    /// cut for hay or straw.
    pub bale: Option<Bale>,
}

/// Each crop's share of the arable fields.
const CROPS: [(Crop, f64); 6] = [
    (Crop::Wheat, 0.3),
    (Crop::Barley, 0.25),
    (Crop::Oats, 0.12),
    (Crop::Maize, 0.1),
    (Crop::Rapeseed, 0.1),
    (Crop::Ley, 0.13),
];

/// Every kind of bale, each as likely as another.
const BALES: [Bale; 4] = [Bale::Small, Bale::Round, Bale::Square, Bale::Stook];

/// What `field` is used for in a region of `mix`, its holding's `farm` where
/// it has one, and `warm` the unit way the noon sun stands toward.
#[must_use]
pub fn usage(key: Key, mix: &Mix, field: &Field, (farm, warm): (Option<Point>, Point)) -> Usage {
    let place = (
        i64::from(field.id.holding.i) * 4096 + i64::from(field.id.index),
        i64::from(field.id.holding.j),
    );
    let mut draws = key.draws(Stage::Use, place);
    let level = (1.0 - field.slope / 0.12).clamp(0.0, 1.0);
    let dry = 1.0 - field.wet;
    let near = farm.map_or(0.0, |farm| {
        (1.0 - (field.middle - farm).length() / 700.0).clamp(0.0, 1.0)
    });
    let sunny = (field.slope.min(0.3) * 4.0) * field.fall.dot(warm).max(0.0);
    let weights = [
        (
            Use::Arable(Crop::Wheat),
            mix.arable * level * dry * (0.4 + 0.6 * near),
        ),
        (
            Use::Pasture,
            mix.pasture * (0.5 + field.slope.min(0.5) + field.wet),
        ),
        (Use::Meadow, mix.meadow * level * (0.4 + field.wet.min(0.6))),
        (Use::Orchard, mix.orchard * near * (0.3 + sunny) * dry),
        (Use::Vineyard, mix.vineyard * sunny * dry),
        (
            Use::Woodlot,
            mix.woodlot * (0.2 + field.slope.min(0.6) * 2.0) * (1.0 - near),
        ),
        (
            Use::Overgrown,
            mix.overgrown * (0.2 + field.slope.min(0.6) + field.wet) * (1.0 - near),
        ),
    ];
    let mut used = draws.pick(&weights).unwrap_or(Use::Pasture);
    if let Use::Arable(_) = used {
        used = Use::Arable(draws.pick(&CROPS).unwrap_or(Crop::Wheat));
    }
    let baled = match used {
        Use::Meadow | Use::Arable(Crop::Wheat | Crop::Barley | Crop::Oats | Crop::Ley) => true,
        Use::Arable(Crop::Maize | Crop::Rapeseed)
        | Use::Pasture
        | Use::Orchard
        | Use::Vineyard
        | Use::Woodlot
        | Use::Overgrown => false,
    };
    let bale = baled.then(|| BALES[draws.below(BALES.len())]);
    Usage { used, bale }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
