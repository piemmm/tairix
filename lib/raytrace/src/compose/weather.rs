//! The time of day and the weather an outdoor scene is set under: the sky,
//! the sun or the moon, the cloud, and the haze.

use super::{direction, rgb, sun, Dice, Stage};
use crate::scene::Fog;
use crate::sky::{CloudLook, Clouds, Glow, Sky};
use crate::terrain::Cloudscape;
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
}

/// What a setting's weather may be, each hour and cover weighed by how often
/// it comes, and how hazy its air is.
pub(super) struct Climate {
    pub(super) hours: &'static [(Hour, u32)],
    pub(super) covers: &'static [(Cover, u32)],
    /// Haze per metre, at its least and most.
    pub(super) haze: (f64, f64),
}

/// A scene's weather, as it lights the scene.
pub(super) struct Outdoors {
    pub(super) sky: Sky,
    pub(super) fog: Option<Fog>,
    pub(super) exposure: f64,
    pub(super) bounce: Vec3,
    /// Roughly how much light falls on the scene, relative to a clear day:
    /// what a medium's own glow is scaled by, so deep water darkens at night.
    pub(super) daylight: f64,
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
/// moon added to `stage`.
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
        Hour::Golden => (dice.range(6.0, 16.0), dice.sign() * dice.range(20.0, 120.0)),
        Hour::Sunset => (dice.range(0.8, 5.0), dice.range(-35.0, 35.0)),
        Hour::Dusk => (dice.range(-6.0, -2.0), dice.range(-40.0, 40.0)),
        Hour::Night => (dice.range(20.0, 50.0), dice.range(-150.0, 150.0)),
    };
    let toward = direction(facing, turn.to_radians(), elevation.to_radians());
    let (mut light, mut sky, mut exposure) = daylight_of(hour, toward, dice);
    if cover == Cover::Overcast {
        // The whole sky goes to the cloud's grey, and the sun into it.
        sky.zenith = sky
            .zenith
            .lerp(Vec3::splat(sky.zenith.max_element() * 0.8 + 0.12), 0.8);
        sky.horizon = sky
            .horizon
            .lerp(Vec3::splat(sky.horizon.max_element() * 0.7 + 0.1), 0.7);
        light = light.map(|sun| Luminary {
            colour: sun.colour.lerp(Vec3::ONE, 0.5),
            irradiance: sun.irradiance * 0.3,
            disc: sun.disc * 3.0,
        });
        exposure *= 1.35;
    }
    if let Some(luminary) = light {
        stage.light(sun(
            toward,
            luminary.disc,
            luminary.colour,
            luminary.irradiance,
        ))?;
    }
    let lit = light.map_or(Vec3::ZERO, |luminary| luminary.colour * luminary.irradiance);
    sky.clouds = match layer_form(dice, cover) {
        Some(form) => Some(cloud_layer(
            stage,
            dice,
            &form,
            (cover, hour),
            (toward, lit),
            &sky,
        )?),
        None => None,
    };
    let haze = dice.range(climate.haze.0, climate.haze.1)
        * match cover {
            Cover::Overcast => 2.0,
            Cover::Broken => 1.3,
            _ => 1.0,
        };
    let ambient = sky.ambient();
    let overhead = lit * toward.y.max(0.0);
    let bounce = ambient * 0.2 + overhead * 0.012;
    let daylight = ambient.max_element() + 0.3 * overhead.max_element();
    Some(Outdoors {
        sky,
        fog: Some(Fog { density: haze }),
        exposure,
        bounce,
        daylight,
        hour,
    })
}

/// The sun, or the moon.
#[derive(Copy, Clone, Debug)]
struct Luminary {
    colour: Vec3,
    irradiance: f64,
    /// Its disc's radius, in degrees.
    disc: f64,
}

/// The sun or moon at `hour` toward `toward`, if either is up; the sky it
/// lights; and the exposure it wants.
fn daylight_of(hour: Hour, toward: Vec3, dice: &mut Dice) -> (Option<Luminary>, Sky, f64) {
    match hour {
        Hour::Noon => (
            Some(Luminary {
                colour: rgb(0xFF_F8_EC),
                irradiance: 4.2,
                disc: 0.6,
            }),
            clear_sky(rgb(0x24_58_B8), rgb(0xB0_CC_EC), None),
            0.72,
        ),
        Hour::Day => (
            Some(Luminary {
                colour: rgb(0xFF_F0_DC),
                irradiance: 3.8,
                disc: 0.7,
            }),
            clear_sky(
                rgb(0x30_62_B4),
                rgb(0xC4_D4_E8),
                Some((toward, rgb(0xFF_F0_D8) * 1.1, 0.25)),
            ),
            0.78,
        ),
        Hour::Golden => (
            Some(Luminary {
                colour: rgb(0xFF_C8_88),
                irradiance: 3.0,
                disc: 0.9,
            }),
            clear_sky(
                rgb(0x3A_5C_A8),
                rgb(0xF0_CC_A8),
                Some((toward, rgb(0xFF_C0_80) * 1.4, 0.7)),
            ),
            0.9,
        ),
        Hour::Sunset => (
            Some(Luminary {
                colour: rgb(0xFF_94_50),
                irradiance: 2.2,
                disc: 1.0,
            }),
            clear_sky(
                rgb(0x26_2C_68),
                rgb(0xFF_A0_68),
                Some((toward, rgb(0xFF_8C_48) * 2.0, 2.0)),
            ),
            1.0,
        ),
        Hour::Dusk => (
            None,
            clear_sky(
                rgb(0x0C_18_40),
                rgb(0x6A_58_7A),
                Some((toward, rgb(0xFF_84_58) * 0.9, 2.6)),
            ),
            2.6,
        ),
        Hour::Night => (
            Some(Luminary {
                colour: rgb(0xB8_C8_FF),
                irradiance: 0.32,
                disc: 1.2,
            }),
            {
                let mut night = clear_sky(
                    rgb(0x03_04_0C),
                    rgb(0x10_16_28),
                    Some((toward, rgb(0x30_3C_60), 0.0)),
                );
                night.stars = dice.range(0.4, 0.9);
                night
            },
            2.8,
        ),
    }
}

/// A cloudless sky from `zenith` to `horizon`, with a glow about the sun
/// toward `glow.0` of colour `glow.1` hugging the horizon by `glow.2`.
fn clear_sky(zenith: Vec3, horizon: Vec3, glow: Option<(Vec3, Vec3, f64)>) -> Sky {
    Sky {
        zenith,
        horizon,
        ground: horizon * 0.35,
        glow: glow.map(|(toward, colour, hugs)| Glow {
            toward,
            colour,
            horizon: hugs,
        }),
        stars: 0.0,
        clouds: None,
    }
}

/// A cloud layer's shape: its altitude, the density cloud begins at and
/// the softness of its edges, its optical depth, and the scale and stretch
/// of its cover.
struct LayerForm {
    altitude: f64,
    threshold: f64,
    softness: f64,
    depth: f64,
    scale: f64,
    stretch: f64,
}

/// The layer `cover` draws, or `None` for a clear sky.
fn layer_form(dice: &mut Dice, cover: Cover) -> Option<LayerForm> {
    let (altitude, threshold, softness, depth, scale, stretch) = match cover {
        Cover::Clear => return None,
        Cover::Fair => (
            dice.range(1200.0, 2200.0),
            dice.range(0.16, 0.32),
            dice.range(0.12, 0.2),
            dice.range(3.5, 5.0),
            dice.range(700.0, 1400.0),
            dice.range(1.0, 1.3),
        ),
        Cover::Broken => (
            dice.range(900.0, 1600.0),
            dice.range(-0.06, 0.1),
            0.2,
            dice.range(3.0, 4.0),
            dice.range(500.0, 900.0),
            dice.range(1.0, 1.5),
        ),
        Cover::Overcast => (
            dice.range(700.0, 1300.0),
            -0.9,
            0.3,
            dice.range(2.5, 3.5),
            600.0,
            1.2,
        ),
        Cover::Cirrus => (
            dice.range(6000.0, 9000.0),
            dice.range(0.05, 0.2),
            0.3,
            dice.range(0.25, 0.5),
            dice.range(1500.0, 3000.0),
            dice.range(4.0, 8.0),
        ),
    };
    Some(LayerForm {
        altitude,
        threshold,
        softness,
        depth,
        scale,
        stretch,
    })
}

/// The cloud layer of `form` that `cover` makes at `hour`, lit by `lit` from
/// `toward`, its cover to be filled; `None` when the heap will not hold it.
fn cloud_layer(
    stage: &mut Stage,
    dice: &mut Dice,
    form: &LayerForm,
    (cover, hour): (Cover, Hour),
    (toward, lit): (Vec3, Vec3),
    sky: &Sky,
) -> Option<Clouds> {
    let &LayerForm {
        altitude,
        threshold,
        softness,
        depth,
        scale,
        stretch,
    } = form;
    let warm = matches!(hour, Hour::Golden | Hour::Sunset | Hour::Dusk);
    let shade = sky.zenith.lerp(sky.horizon, 0.6) * if warm { 1.1 } else { 0.95 };
    let look = CloudLook {
        altitude,
        threshold,
        softness,
        depth,
        shade: if cover == Cover::Overcast {
            shade * 1.2
        } else {
            shade
        },
        sunlight: lit * if warm { 0.42 } else { 0.32 },
        toward: if toward.y > 0.0 {
            toward
        } else {
            Vec3::new(toward.x, 0.05, toward.z).normalized()
        },
        detail: 0.18 * scale,
        seed: dice.seed(),
    };
    let heading = dice.range(0.0, core::f64::consts::TAU);
    stage.clouds(Cloudscape {
        scale,
        stretch,
        heading,
        seed: look.seed ^ 0x5eed,
    })?;
    // The layer's grid spans far enough that it fades into the haze before
    // its edge.
    Clouds::new(384, 12.0 * altitude.max(2500.0), look)
}
