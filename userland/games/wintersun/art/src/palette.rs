//! The `WinterSun` palette.
//!
//! Every colour the ground is drawn from is here, and nowhere else. A
//! literal `Color::rgb(…)` anywhere else in the game's art is a second
//! palette to keep in step with this one.
//!
//! # Why a ramp and not a colour
//!
//! `WinterSun` is lit by a low sun, so a surface is never one colour: the
//! slope facing the sun is warm, the slope away from it is cold, and the
//! mid tone is what the material is between them. A palette of flat
//! colours cannot express that without every consumer inventing its own
//! darkening, so the unit here is a [`Ramp`] — shadow, mid, light — and a
//! material's grain, a slope's shading and a particle's tint all sample
//! the one ramp rather than deriving a tint apiece.
//!
//! The ground set spans the climate the world does, from glacier ice to red
//! desert. A ramp is the ground's own tonal range; the warm and cold of the
//! low sun across a slope is the client's shading, applied over every ramp
//! alike, so an ice field and a laterite plain read as one light on two
//! grounds. Nothing is saturated: the ground is the stage, not the actor.

use tairix_raster::color::Color;

/// A surface's three tones under a low sun.
///
/// `shadow` is the tone away from the light, `light` the tone into it, and
/// `mid` what the material reads as overall — not the average of the
/// other two, because a physical surface darkens faster than it brightens.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ramp {
    /// The tone facing away from the sun.
    pub shadow: Color,
    /// The tone the material reads as.
    pub mid: Color,
    /// The tone facing into the sun.
    pub light: Color,
}

impl Ramp {
    /// A ramp from its three opaque tones.
    #[must_use]
    pub const fn new(shadow: (u8, u8, u8), mid: (u8, u8, u8), light: (u8, u8, u8)) -> Self {
        Self {
            shadow: Color::rgb(shadow.0, shadow.1, shadow.2),
            mid: Color::rgb(mid.0, mid.1, mid.2),
            light: Color::rgb(light.0, light.1, light.2),
        }
    }

    /// The ramp at `t`, where `0` is [`shadow`](Self::shadow), `128` is
    /// [`mid`](Self::mid) and `255` is [`light`](Self::light).
    ///
    /// Two straight lines rather than one, so the mid tone is hit exactly
    /// and the darker half can fall away faster than the lighter half
    /// rises.
    #[must_use]
    pub fn sample(&self, t: u8) -> Color {
        if t < MID_STOP {
            lerp(self.shadow, self.mid, scale_to_byte(t, MID_STOP))
        } else {
            lerp(
                self.mid,
                self.light,
                scale_to_byte(t - MID_STOP, u8::MAX - MID_STOP),
            )
        }
    }
}

/// Where [`Ramp::mid`] sits on the `0..=255` sample axis.
const MID_STOP: u8 = 128;

/// `numerator / denominator` as a `0..=255` fraction, saturating at the top
/// and reading a zero denominator as a full one.
fn scale_to_byte(numerator: u8, denominator: u8) -> u8 {
    if denominator == 0 {
        return u8::MAX;
    }
    let scaled = u32::from(numerator) * u32::from(u8::MAX) / u32::from(denominator);
    u8::try_from(scaled.min(u32::from(u8::MAX))).unwrap_or(u8::MAX)
}

/// `a` toward `b` by `t`/255, per channel, rounded to nearest.
#[must_use]
pub fn lerp(a: Color, b: Color, t: u8) -> Color {
    Color::rgba(
        lerp_channel(a.r, b.r, t),
        lerp_channel(a.g, b.g, t),
        lerp_channel(a.b, b.b, t),
        lerp_channel(a.a, b.a, t),
    )
}

/// One channel of [`lerp`].
fn lerp_channel(a: u8, b: u8, t: u8) -> u8 {
    let (a, b, t) = (u32::from(a), u32::from(b), u32::from(t));
    let total = u32::from(u8::MAX);
    let mixed = a * (total - t) + b * t + total / 2;
    u8::try_from(mixed / total).unwrap_or(u8::MAX)
}

/// Open water, at depth.
pub const WATER: Ramp = Ramp::new((10, 24, 38), (22, 48, 72), (58, 96, 124));
/// Glacier and sheet ice: blue in shadow, near-white into the sun.
pub const ICE: Ramp = Ramp::new((122, 150, 176), (190, 210, 228), (236, 245, 252));
/// Lying snow, which is the brightest thing in the realm.
pub const SNOW: Ramp = Ramp::new((150, 164, 186), (220, 228, 240), (252, 253, 255));
/// Grey-green lichen over stones.
pub const LICHEN: Ramp = Ramp::new((54, 60, 46), (118, 130, 82), (178, 186, 132));
/// Deep, damp moss.
pub const MOSS: Ramp = Ramp::new((24, 40, 26), (66, 98, 48), (122, 150, 80));
/// Rust-brown conifer needles.
pub const NEEDLE_LITTER: Ramp = Ramp::new((36, 26, 24), (96, 66, 44), (156, 116, 76));
/// Fallen broadleaves, ochre and brown.
pub const LEAF_LITTER: Ramp = Ramp::new((48, 34, 24), (124, 88, 48), (190, 146, 86));
/// Dark forest earth.
pub const FOREST_LOAM: Ramp = Ramp::new((26, 22, 22), (70, 54, 42), (120, 98, 74));
/// Rainforest floor: dark olive under a closed canopy.
pub const RAINFOREST_FLOOR: Ramp = Ramp::new((18, 26, 16), (56, 66, 34), (104, 116, 62));
/// Short, grazed grass.
pub const SHORT_GRASS: Ramp = Ramp::new((36, 52, 32), (94, 122, 62), (156, 180, 104));
/// Lush grass by water.
pub const LUSH_GRASS: Ramp = Ramp::new((24, 52, 24), (62, 120, 44), (122, 180, 80));
/// Dry, bleached grass: straw under a hard sun.
pub const DRY_GRASS: Ramp = Ramp::new((82, 70, 44), (170, 150, 92), (224, 208, 150));
/// Tall grass and reed, olive and seeding.
pub const TALL_GRASS: Ramp = Ramp::new((38, 42, 16), (114, 118, 54), (178, 176, 100));
/// Flowering meadow, lighter and yellower than pasture.
pub const MEADOW: Ramp = Ramp::new((46, 58, 34), (132, 144, 76), (200, 204, 124));
/// Heather and ling, purple-brown.
pub const HEATH: Ramp = Ramp::new((44, 32, 44), (108, 74, 92), (164, 124, 138));
/// Peat, near black.
pub const PEAT: Ramp = Ramp::new((20, 16, 18), (58, 44, 36), (100, 82, 64));
/// Grey-brown mud.
pub const MUD: Ramp = Ramp::new((40, 36, 34), (92, 78, 60), (144, 128, 102));
/// Pale sand ground from shell and lime.
pub const WHITE_SAND: Ramp = Ramp::new((140, 136, 128), (214, 206, 186), (246, 242, 228));
/// Golden quartz sand.
pub const GOLDEN_SAND: Ramp = Ramp::new((108, 90, 62), (196, 168, 116), (238, 216, 170));
/// Iron-red desert sand.
pub const RED_SAND: Ramp = Ramp::new((86, 46, 34), (178, 104, 62), (226, 162, 112));
/// Black volcanic sand.
pub const BLACK_SAND: Ramp = Ramp::new((20, 20, 26), (58, 56, 60), (110, 106, 108));
/// Wind-heaped dune sand, lighter than the flat sand between.
pub const DUNE_SAND: Ramp = Ramp::new((124, 100, 68), (216, 186, 128), (250, 230, 184));
/// Gravel.
pub const GRAVEL: Ramp = Ramp::new((64, 62, 58), (126, 120, 110), (184, 178, 166));
/// Rounded beach shingle, paler than gravel.
pub const SHINGLE: Ramp = Ramp::new((86, 86, 88), (160, 156, 150), (216, 212, 204));
/// Scree: shattered rock, cool grey.
pub const SCREE: Ramp = Ramp::new((52, 54, 58), (108, 108, 110), (170, 170, 170));
/// Cracked clay crust, pale tan.
pub const CLAY_CRUST: Ramp = Ramp::new((104, 80, 68), (186, 150, 130), (232, 208, 188));
/// A dried salt pan: the palest ground there is.
pub const SALT_PAN: Ramp = Ramp::new((162, 162, 160), (228, 224, 214), (252, 250, 244));
/// Brick-red laterite.
pub const LATERITE: Ramp = Ramp::new((66, 30, 22), (154, 74, 42), (206, 128, 84));
/// Volcanic ash.
pub const ASH: Ramp = Ramp::new((24, 22, 24), (70, 66, 66), (124, 118, 114));
/// Fresh lava, near black and glassy.
pub const COOLED_LAVA: Ramp = Ramp::new((10, 10, 14), (38, 34, 36), (92, 84, 82));
/// Crystalline shield rock: pink-grey gneiss.
pub const SHIELD_ROCK: Ramp = Ramp::new((72, 66, 68), (144, 128, 120), (206, 192, 182));
/// Granite, light and speckled.
pub const GRANITE: Ramp = Ramp::new((74, 72, 74), (152, 144, 138), (214, 208, 200));
/// Basalt, dark and cold.
pub const BASALT: Ramp = Ramp::new((28, 30, 38), (74, 78, 86), (132, 134, 138));
/// Limestone, pale cream-grey.
pub const LIMESTONE: Ramp = Ramp::new((104, 104, 104), (186, 180, 164), (234, 230, 214));
/// Sandstone, tan to orange.
pub const SANDSTONE: Ramp = Ramp::new((88, 58, 42), (180, 132, 90), (228, 186, 136));
/// Shale, dark blue-grey.
pub const SHALE: Ramp = Ramp::new((38, 40, 48), (92, 90, 98), (150, 146, 150));
/// Chalk, near white.
pub const CHALK: Ramp = Ramp::new((150, 150, 148), (222, 220, 208), (250, 250, 242));
/// Schist and gneiss, silvery green-grey.
pub const SCHIST: Ramp = Ramp::new((46, 60, 56), (98, 124, 112), (164, 184, 170));
/// Ground the world was torn through: the one place the palette is allowed
/// a hue that is not in the landscape.
pub const RIFT_GROUND: Ramp = Ramp::new((34, 20, 44), (72, 44, 86), (132, 96, 148));

/// Rain and sleet: near-colourless, and read by their streak rather than
/// their hue.
pub const RAIN: Ramp = Ramp::new((96, 108, 124), (150, 164, 182), (206, 216, 230));
/// Falling snow and hail.
pub const SNOWFALL: Ramp = Ramp::new((176, 186, 202), (226, 232, 242), (255, 255, 255));
/// Embers and sparks.
pub const EMBER: Ramp = Ramp::new((112, 30, 8), (206, 92, 22), (255, 196, 96));
/// Smoke, which is lit from one side like everything else.
pub const SMOKE: Ramp = Ramp::new((26, 26, 30), (72, 72, 78), (138, 140, 146));
/// Kicked-up dust and ash.
pub const DUST: Ramp = Ramp::new((70, 64, 54), (124, 114, 96), (180, 170, 148));
/// Splashed water.
pub const SPLASH: Ramp = Ramp::new((44, 72, 92), (96, 134, 156), (176, 206, 222));
/// Blown leaves and needles.
pub const LEAF: Ramp = Ramp::new((42, 48, 26), (94, 92, 44), (156, 142, 74));

#[cfg(test)]
mod tests;
