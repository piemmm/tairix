//! The time of day and the weather an outdoor scene is set under: the sun
//! or the moon, the air it shines through, the cloud, and the stars.
//!
//! The sky's colours are not chosen here: the atmosphere works them out from
//! where the sun stands and how hazy the air is, so noon is blue, sunset
//! orange and the dusk after it violet because the light's path through the
//! air makes them so.

use super::{direction, Dice, Stage};
use crate::atmosphere::{Air, Atmosphere, AERIAL_REACH};
use crate::body;
use crate::cloud::{Cloudbank, Deck, Matter};
use crate::scene::Exposure;
use crate::sky::{Dome, Sky};
use crate::stars::Starfield;
use crate::vector::Vec3;

/// The hours a scene can be set at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Hour {
    Noon,
    /// Morning or afternoon: the sun well up, off to one side.
    Day,
    /// The hour before sunset or after sunrise.
    Golden,
    Sunset,
    /// After sunset, the sun below the horizon and its glow still on it.
    Dusk,
    /// By the moon.
    Night,
}

/// The cloud a scene can be set under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Cover {
    Clear,
    /// Heaps of fair-weather cumulus, blue between them.
    Fair,
    /// A broken layer, more cloud than sky.
    Broken,
    /// Grey, the sun a pale disc if it shows at all.
    Overcast,
    /// High streaks of ice.
    Cirrus,
    /// Heaps grown into towers, some a few kilometres tall.
    Towering,
    /// A mackerel sky: a field of small, high billows in rows.
    Mackerel,
}

/// What a setting's weather may be, each hour and cover weighed by how often
/// it comes, how hazy its air is, and the ground it lies on.
pub(super) struct Climate {
    pub(super) hours: &'static [(Hour, u32)],
    pub(super) covers: &'static [(Cover, u32)],
    /// Haze, as a multiple of a clear day's, at its least and most.
    pub(super) haze: (f64, f64),
    /// How far above the sea the scene's level lies, in metres.
    pub(super) base: f64,
    /// The land's reflectance, which lights the air from below.
    pub(super) albedo: f64,
}

/// A scene's weather, as it lights the scene.
pub(super) struct Outdoors {
    pub(super) sky: Sky,
    pub(super) exposure: Exposure,
    pub(super) hour: Hour,
}

/// One of `choices`, drawn by weight.
fn weighted<T: Copy>(dice: &mut Dice, choices: &[(T, u32)]) -> Option<T> {
    let total: u32 = choices.iter().map(|(_, weight)| weight).sum();
    let mut draw = dice.count(0, total.max(1) - 1);
    for &(choice, weight) in choices {
        if draw < weight {
            return Some(choice);
        }
        draw -= weight;
    }
    choices.first().map(|(choice, _)| *choice)
}

/// The weather of `climate` over a scene seen toward `facing`, its sun or
/// moon added to `stage`; the atmosphere's tables are built later, once the
/// eye it is seen from stands.
///
/// The sun stands where it truly is: whether its light reaches the scene,
/// lifted by the air near the horizon, the air says once its tables are
/// built. By night it is the full moon's light that fills the air.
pub(super) fn outdoors(
    stage: &mut Stage,
    dice: &mut Dice,
    climate: &Climate,
    facing: f64,
) -> Option<Outdoors> {
    let hour = weighted(dice, climate.hours)?;
    let cover = weighted(dice, climate.covers)?;
    // A low sun is best ahead, behind the scene; a high one off to a side.
    let (elevation, turn) = match hour {
        Hour::Noon => (dice.range(48.0, 70.0), dice.range(-180.0, 180.0)),
        Hour::Day => (
            dice.range(22.0, 42.0),
            dice.sign() * dice.range(50.0, 140.0),
        ),
        Hour::Golden => (dice.range(5.0, 14.0), dice.sign() * dice.range(20.0, 120.0)),
        Hour::Sunset => (dice.range(0.6, 3.5), dice.range(-35.0, 35.0)),
        Hour::Dusk => (dice.range(-5.0, -1.5), dice.range(-40.0, 40.0)),
        Hour::Night => (dice.range(20.0, 50.0), dice.range(-150.0, 150.0)),
    };
    let toward = direction(facing, turn.to_radians(), elevation.to_radians());
    let night = hour == Hour::Night;
    let light = if night {
        body::full_moon(toward)
    } else {
        body::sun(toward)
    };
    let solar = light.irradiance();
    stage.light(light)?;
    let mut haze = dice.range(climate.haze.0, climate.haze.1)
        * match cover {
            Cover::Overcast => 2.5,
            Cover::Broken => 1.4,
            _ => 1.0,
        };
    if night {
        haze = haze.min(2.0);
    }
    let air = Air {
        sun: toward,
        solar,
        base: climate.base,
        haze,
        albedo: Vec3::splat(climate.albedo),
        eye: Vec3::ZERO,
    };
    let low = decks(dice, cover);
    let low = if low.iter().any(Option::is_some) {
        Some(Cloudbank::new(low, (0.0, 0.0), BANK_HALF, toward)?)
    } else {
        None
    };
    // Cirrus above everything now and then, and always under a cirrus sky.
    let streaks = cover == Cover::Cirrus || (cover != Cover::Overcast && dice.chance(0.3));
    let high = if streaks {
        let deck = cirrus(dice, cover == Cover::Cirrus);
        Some(Cloudbank::new(
            [Some(deck), None],
            (0.0, 0.0),
            HIGH_HALF,
            toward,
        )?)
    } else {
        None
    };
    let sky = Sky {
        dome: Dome::Air(Atmosphere::new(air)?),
        stars: Some(Starfield::new()),
        low,
        high,
    };
    // Where a photographer would set the scene's middle tone: bright at noon,
    // lower as the light goes so a sunset keeps its colour and night its dark.
    let key = match hour {
        Hour::Noon | Hour::Day => 0.17,
        Hour::Golden => 0.15,
        Hour::Sunset => 0.13,
        Hour::Dusk => 0.1,
        Hour::Night => 0.055,
    };
    Some(Outdoors {
        sky,
        exposure: Exposure::Metered { key },
        hour,
    })
}

/// How far the cloud bank spreads either way of the eye: far enough that
/// its edge lies in the haze at the horizon.
const BANK_HALF: f64 = 28_000.0;
/// How far the high bank of cirrus spreads: as far as the air between it
/// and the eye is tabulated, past which the haze takes it.
const HIGH_HALF: f64 = AERIAL_REACH * 1000.0;

/// A deck of `form`, its heading about `heading`.
fn deck(dice: &mut Dice, form: &Form, heading: f64) -> Deck {
    Deck {
        base: dice.range(form.base.0, form.base.1),
        base_spread: dice.range(80.0, 260.0),
        depth: form.depth,
        cover: dice.range(form.cover.0, form.cover.1),
        heap: form.heap,
        scale: dice.range(form.scale.0, form.scale.1),
        stretch: dice.range(form.stretch.0, form.stretch.1),
        heading: heading + dice.range(-0.3, 0.3),
        thickness: dice.range(form.thickness.0, form.thickness.1),
        billow: dice.range(form.billow.0, form.billow.1),
        fibre: dice.range(form.medium.fibre.0, form.medium.fibre.1),
        matter: form.medium.matter,
        seed: dice.seed(),
    }
}

/// A deck of cirrus: a sky of it where `whole`, else a few streaks.
fn cirrus(dice: &mut Dice, whole: bool) -> Deck {
    let heading = dice.range(0.0, core::f64::consts::TAU);
    deck(dice, if whole { &CIRRUS } else { &STREAKS }, heading)
}

/// The decks of cloud `cover` stacks: a low one of heaps or sheets, and a
/// middle one of smaller billows.
fn decks(dice: &mut Dice, cover: Cover) -> [Option<Deck>; 2] {
    let heading = dice.range(0.0, core::f64::consts::TAU);
    let mut deck = |form: Form| deck(dice, &form, heading);
    match cover {
        Cover::Clear | Cover::Cirrus => [None, None],
        Cover::Fair => [Some(deck(CUMULUS)), None],
        Cover::Broken => [Some(deck(STRATOCUMULUS)), Some(deck(ALTOCUMULUS))],
        Cover::Overcast => [Some(deck(STRATUS)), Some(deck(ALTOSTRATUS))],
        Cover::Towering => [Some(deck(CONGESTUS)), None],
        Cover::Mackerel => [Some(deck(HUMILIS)), Some(deck(MACKEREL))],
    }
}

/// A kind of cloud, as ranges its decks are drawn from.
#[derive(Copy, Clone, Debug)]
struct Form {
    base: (f64, f64),
    depth: (f64, f64),
    cover: (f64, f64),
    heap: f64,
    scale: (f64, f64),
    stretch: (f64, f64),
    billow: (f64, f64),
    thickness: (f64, f64),
    medium: Medium,
}

/// What a kind of cloud is made of, and how drawn out its billows are.
#[derive(Copy, Clone, Debug)]
struct Medium {
    matter: Matter,
    fibre: (f64, f64),
}

/// Water droplets, in billows as round as they are broad.
const DROPLETS: Medium = Medium {
    matter: Matter::Water,
    fibre: (1.0, 1.0),
};
/// Ice, drawn out into streaks along the wind.
const ICE: Medium = Medium {
    matter: Matter::Ice,
    fibre: (5.0, 10.0),
};
/// Fair-weather cumulus: heaps a kilometre or two across and nearly as tall.
const CUMULUS: Form = Form {
    base: (1100.0, 1600.0),
    depth: (600.0, 2200.0),
    cover: (0.28, 0.42),
    heap: 1.0,
    scale: (1800.0, 3400.0),
    stretch: (1.0, 1.3),
    billow: (300.0, 450.0),
    thickness: (0.05, 0.05),
    medium: DROPLETS,
};
/// Small, flat heaps under a mackerel sky.
const HUMILIS: Form = Form {
    base: (1300.0, 1800.0),
    depth: (250.0, 800.0),
    cover: (0.12, 0.2),
    heap: 0.8,
    scale: (1500.0, 2600.0),
    stretch: (1.0, 1.3),
    billow: (260.0, 380.0),
    thickness: (0.045, 0.045),
    medium: DROPLETS,
};
/// A broken sheet of lumpy cloud, more cloud than sky.
const STRATOCUMULUS: Form = Form {
    base: (900.0, 1400.0),
    depth: (300.0, 1000.0),
    cover: (0.58, 0.72),
    heap: 0.55,
    scale: (2500.0, 5000.0),
    stretch: (1.0, 1.6),
    billow: (380.0, 560.0),
    thickness: (0.04, 0.04),
    medium: DROPLETS,
};
/// Heaps grown into towers.
const CONGESTUS: Form = Form {
    base: (1200.0, 1600.0),
    depth: (900.0, 4800.0),
    cover: (0.3, 0.44),
    heap: 1.0,
    scale: (2200.0, 4200.0),
    stretch: (1.0, 1.3),
    billow: (420.0, 680.0),
    thickness: (0.05, 0.05),
    medium: DROPLETS,
};
/// A grey ceiling.
const STRATUS: Form = Form {
    base: (600.0, 1000.0),
    depth: (700.0, 1800.0),
    cover: (0.93, 1.0),
    heap: 0.3,
    scale: (3000.0, 6000.0),
    stretch: (1.0, 1.4),
    billow: (600.0, 900.0),
    thickness: (0.035, 0.035),
    medium: DROPLETS,
};
/// A middle layer of small billows.
const ALTOCUMULUS: Form = Form {
    base: (3800.0, 4800.0),
    depth: (150.0, 420.0),
    cover: (0.3, 0.45),
    heap: 0.2,
    scale: (2500.0, 4500.0),
    stretch: (2.0, 3.0),
    billow: (220.0, 320.0),
    thickness: (0.02, 0.02),
    medium: DROPLETS,
};
/// A thin grey middle sheet above an overcast.
const ALTOSTRATUS: Form = Form {
    base: (2800.0, 3600.0),
    depth: (300.0, 700.0),
    cover: (0.7, 0.85),
    heap: 0.15,
    scale: (3000.0, 6000.0),
    stretch: (1.5, 2.5),
    billow: (400.0, 600.0),
    thickness: (0.02, 0.02),
    medium: DROPLETS,
};
/// A mackerel sky: rows of small, high billows.
const MACKEREL: Form = Form {
    base: (3500.0, 5000.0),
    depth: (150.0, 350.0),
    cover: (0.5, 0.68),
    heap: 0.15,
    scale: (1500.0, 3000.0),
    stretch: (2.0, 4.0),
    billow: (150.0, 240.0),
    thickness: (0.025, 0.025),
    medium: DROPLETS,
};
/// A sky of cirrus: thin sheets and streaks of ice high above everything,
/// drawn out along the wind, a tenth to one and a half of optical depth
/// through.
const CIRRUS: Form = Form {
    base: (7000.0, 9500.0),
    depth: (400.0, 1500.0),
    cover: (0.3, 0.55),
    heap: 0.1,
    scale: (1500.0, 3000.0),
    stretch: (4.0, 8.0),
    billow: (180.0, 320.0),
    thickness: (4e-4, 1.5e-3),
    medium: ICE,
};
/// A few streaks of cirrus above other cloud.
const STREAKS: Form = Form {
    cover: (0.12, 0.3),
    ..CIRRUS
};
