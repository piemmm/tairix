//! The models a colour picker's fields and sliders show a colour in, and the
//! views it is picked on.

use tairix_colour::{Cmyk, Fraction, Hsl, Hsv, Hue, Lab, Lch, Rgb};
use tairix_raster::Color;
use tairix_util::mathf;

/// How a colour picker picks: the surface it is dragged on.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum PickerView {
    /// Saturation across and value up a square at the colour's hue, the hue
    /// on a strip beside it.
    #[default]
    Square,
    /// The hue round a ring, and saturation and value in a triangle within
    /// it.
    Wheel,
    /// A slider for each channel of the model shown, each drawn as that
    /// channel sweeps with the rest held.
    Sliders,
}

impl PickerView {
    /// Every view, in the order a list of them reads.
    pub const ALL: [Self; 3] = [Self::Square, Self::Wheel, Self::Sliders];

    /// What a list of views calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Square => "Square",
            Self::Wheel => "Wheel",
            Self::Sliders => "Sliders",
        }
    }
}

/// One channel of a model: what its field is captioned and holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ChannelSpec {
    /// The caption beside its field.
    pub label: &'static str,
    /// The unit after it.
    pub unit: &'static str,
    /// The least it holds, in its smallest place.
    pub least: i32,
    /// The most it holds, in its smallest place.
    pub most: i32,
    /// What Page Up and Page Down step it by, in its smallest place.
    pub page: i32,
    /// The decimal places it is spelled with: a CIE value is held to tenths,
    /// as whole ones move a colour near sRGB's corners by several levels.
    pub places: u8,
}

const fn spec(
    label: &'static str,
    unit: &'static str,
    least: i32,
    most: i32,
    page: i32,
) -> ChannelSpec {
    ChannelSpec {
        label,
        unit,
        least,
        most,
        page,
        places: 0,
    }
}

/// A channel held to tenths.
const fn tenths(label: &'static str, unit: &'static str, least: i32, most: i32) -> ChannelSpec {
    ChannelSpec {
        label,
        unit,
        least: least * 10,
        most: most * 10,
        page: 100,
        places: 1,
    }
}

const RGB: [ChannelSpec; 3] = [
    spec("R", "", 0, 255, 16),
    spec("G", "", 0, 255, 16),
    spec("B", "", 0, 255, 16),
];
const HSV: [ChannelSpec; 3] = [
    spec("H", "°", 0, 359, 15),
    spec("S", "%", 0, 100, 10),
    spec("V", "%", 0, 100, 10),
];
const HSL: [ChannelSpec; 3] = [
    spec("H", "°", 0, 359, 15),
    spec("S", "%", 0, 100, 10),
    spec("L", "%", 0, 100, 10),
];
const CMYK: [ChannelSpec; 4] = [
    spec("C", "%", 0, 100, 10),
    spec("M", "%", 0, 100, 10),
    spec("Y", "%", 0, 100, 10),
    spec("K", "%", 0, 100, 10),
];
const LAB: [ChannelSpec; 3] = [
    tenths("L", "", 0, 100),
    tenths("a", "", -128, 127),
    tenths("b", "", -128, 127),
];
const LCH: [ChannelSpec; 3] = [
    tenths("L", "", 0, 100),
    tenths("C", "", 0, 150),
    ChannelSpec {
        page: 150,
        ..tenths("h", "°", 0, 360)
    },
];
const GREY: [ChannelSpec; 1] = [spec("K", "%", 0, 100, 10)];

/// The most channels a model has.
pub const MOST_CHANNELS: usize = 4;

/// The model a picker's fields and sliders show a colour in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ColourModel {
    /// Red, green and blue, `0..=255`.
    #[default]
    Rgb,
    /// Hue, saturation and value.
    Hsv,
    /// Hue, saturation and lightness.
    Hsl,
    /// Device cyan, magenta, yellow and black, in percent of full coverage.
    Cmyk,
    /// CIE L\*a\*b\* under D65.
    Lab,
    /// CIE LCh(ab): L\*a\*b\* by lightness, chroma and hue angle.
    Lch,
    /// A grey, as the percent of black it lays down.
    Grey,
}

impl ColourModel {
    /// Every model, in the order a list of them reads.
    pub const ALL: [Self; 7] = [
        Self::Rgb,
        Self::Hsv,
        Self::Hsl,
        Self::Cmyk,
        Self::Lab,
        Self::Lch,
        Self::Grey,
    ];

    /// What a list of models calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Hsv => "HSV",
            Self::Hsl => "HSL",
            Self::Cmyk => "CMYK",
            Self::Lab => "Lab",
            Self::Lch => "LCh",
            Self::Grey => "Grey",
        }
    }

    /// Its channels, in its fields' order.
    #[must_use]
    pub const fn channels(self) -> &'static [ChannelSpec] {
        match self {
            Self::Rgb => &RGB,
            Self::Hsv => &HSV,
            Self::Hsl => &HSL,
            Self::Cmyk => &CMYK,
            Self::Lab => &LAB,
            Self::Lch => &LCH,
            Self::Grey => &GREY,
        }
    }

    /// Whether a colour it names may lie outside sRGB, and be clipped to it.
    #[must_use]
    pub const fn reaches_past_srgb(self) -> bool {
        matches!(self, Self::Lab | Self::Lch)
    }

    /// The values `rgb` takes, the hue a grey has none of kept from `near`.
    #[must_use]
    pub fn values(self, rgb: Rgb, near: [i32; MOST_CHANNELS]) -> [i32; MOST_CHANNELS] {
        let percent = |fraction: Fraction| i32::try_from(fraction.percent()).unwrap_or(0);
        let degrees = |hue: Hue| i32::try_from(hue.degrees()).unwrap_or(0);
        let near_hue = Hue::from_degrees(u32::try_from(near[0].rem_euclid(360)).unwrap_or(0));
        match self {
            Self::Rgb => [i32::from(rgb.r), i32::from(rgb.g), i32::from(rgb.b), 0],
            Self::Hsv => {
                let hsv = Hsv::from_rgb(rgb, Hsv::new(near_hue, Fraction::NONE, Fraction::NONE));
                [
                    degrees(hsv.hue),
                    percent(hsv.saturation),
                    percent(hsv.value),
                    0,
                ]
            }
            Self::Hsl => {
                let hsl = Hsl::from_rgb(rgb, Hsl::new(near_hue, Fraction::NONE, Fraction::NONE));
                [
                    degrees(hsl.hue),
                    percent(hsl.saturation),
                    percent(hsl.lightness),
                    0,
                ]
            }
            Self::Cmyk => {
                let inks = Cmyk::from_rgb(rgb);
                [
                    percent(inks.cyan),
                    percent(inks.magenta),
                    percent(inks.yellow),
                    percent(inks.key),
                ]
            }
            Self::Lab => {
                let lab = Lab::from_rgb(rgb);
                [
                    tenths_of(lab.l).clamp(0, 1000),
                    tenths_of(lab.a).clamp(-1280, 1270),
                    tenths_of(lab.b).clamp(-1280, 1270),
                    0,
                ]
            }
            Self::Lch => {
                let lch = Lch::from_rgb(rgb);
                let chroma = tenths_of(lch.c).clamp(0, 1500);
                // A grey's hue angle is noise: keep the one shown.
                let hue = if chroma == 0 {
                    near[2]
                } else {
                    tenths_of(lch.h).rem_euclid(3600)
                };
                [tenths_of(lch.l).clamp(0, 1000), chroma, hue, 0]
            }
            Self::Grey => {
                let grey = Color::rgb(rgb.r, rgb.g, rgb.b).luma();
                [100 - (i32::from(grey) * 100 + 127) / 255, 0, 0, 0]
            }
        }
    }

    /// The colour `values` name, and whether it lay outside sRGB and was
    /// clipped to it.
    #[must_use]
    pub fn colour(self, values: [i32; MOST_CHANNELS]) -> (Rgb, bool) {
        let fraction = |percent: i32| {
            Fraction::from_percent(u32::try_from(percent.clamp(0, 100)).unwrap_or(0))
        };
        let hue =
            |degrees: i32| Hue::from_degrees(u32::try_from(degrees.rem_euclid(360)).unwrap_or(0));
        let byte = |level: i32| u8::try_from(level.clamp(0, 255)).unwrap_or(0);
        match self {
            Self::Rgb => (
                Rgb::new(byte(values[0]), byte(values[1]), byte(values[2])),
                false,
            ),
            Self::Hsv => (
                Hsv::new(hue(values[0]), fraction(values[1]), fraction(values[2])).to_rgb(),
                false,
            ),
            Self::Hsl => (
                Hsl::new(hue(values[0]), fraction(values[1]), fraction(values[2])).to_rgb(),
                false,
            ),
            Self::Cmyk => (
                Cmyk::new(
                    fraction(values[0]),
                    fraction(values[1]),
                    fraction(values[2]),
                    fraction(values[3]),
                )
                .to_rgb(),
                false,
            ),
            Self::Lab => {
                let landed = Lab {
                    l: f64::from(values[0]) / 10.0,
                    a: f64::from(values[1]) / 10.0,
                    b: f64::from(values[2]) / 10.0,
                }
                .to_rgb();
                (landed.rgb, landed.clipped)
            }
            Self::Lch => {
                let landed = Lch {
                    l: f64::from(values[0]) / 10.0,
                    c: f64::from(values[1]) / 10.0,
                    h: f64::from(values[2]) / 10.0,
                }
                .to_rgb();
                (landed.rgb, landed.clipped)
            }
            Self::Grey => {
                let level = byte(((100 - values[0].clamp(0, 100)) * 255 + 50) / 100);
                (Rgb::new(level, level, level), false)
            }
        }
    }
}

/// `value` to the nearest tenth, as a whole number of tenths.
fn tenths_of(value: f64) -> i32 {
    mathf::round_i32(value * 10.0)
}

#[cfg(test)]
#[path = "colour_model_tests.rs"]
mod tests;
