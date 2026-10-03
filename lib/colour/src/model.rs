//! Hue, saturation, value and lightness: the colour circle a picker and CSS
//! `hsl()` describe a colour by.
//!
//! The coordinates are fixed point, fine enough that every 8-bit colour comes
//! back from them unchanged: a [`Hue`] counts 393216ths of a turn, a
//! [`Fraction`] 65535ths, and each conversion rounds once, at the end.

use tairix_util::mathf;

use crate::rgb::Rgb;

/// `Fraction::ALL`'s raw value, widened for the conversions' products.
const UNIT: u64 = 0xFFFF;

/// `Hue::SEXTANT`, widened for the conversions' products.
const SIXTH: u64 = 1 << 16;

/// A proportion from none to all in 65535ths: a saturation, a value or a
/// lightness.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Fraction(u16);

impl Fraction {
    /// None at all.
    pub const NONE: Self = Self(0);

    /// All of it.
    pub const ALL: Self = Self(u16::MAX);

    /// The fraction `raw` 65535ths.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// The fraction in 65535ths.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// An 8-bit level as a fraction, exactly: `byte` 255ths.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        Self(u16::from_le_bytes([byte, byte]))
    }

    /// The nearest 8-bit level.
    #[must_use]
    pub fn byte(self) -> u8 {
        u8::try_from((u32::from(self.0) + 128) / 257).unwrap_or(u8::MAX)
    }

    /// `percent` hundredths, rounded to the nearest step; above 100 is all.
    #[must_use]
    pub fn from_percent(percent: u32) -> Self {
        Self::ratio(percent.min(100), 100)
    }

    /// The nearest whole percentage, `0..=100`.
    #[must_use]
    pub fn percent(self) -> u32 {
        (u32::from(self.0) * 100 + 0x7FFF) / 0xFFFF
    }

    /// `value` held to `0.0..=1.0` and rounded to the nearest step; `NaN` is
    /// none.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        let raw = mathf::round_i32(mathf::clamp(value, 0.0, 1.0) * f64::from(u16::MAX));
        Self(u16::try_from(raw).unwrap_or(0))
    }

    /// `part / whole` to the nearest step, for `part <= whole` and a non-zero
    /// `whole`.
    fn ratio(part: u32, whole: u32) -> Self {
        let raw = (u64::from(part) * UNIT + u64::from(whole) / 2) / u64::from(whole.max(1));
        Self(u16::try_from(raw).unwrap_or(u16::MAX))
    }
}

/// A hue: an angle round the colour circle from red, in 393216ths of a turn,
/// 65536 to each sixth, so the primaries and secondaries are exact.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Hue(u32);

impl Hue {
    /// A sixth of a turn: red to yellow.
    pub const SEXTANT: u32 = 1 << 16;

    /// A whole turn.
    pub const TURN: u32 = 6 * Self::SEXTANT;

    /// Red, where the circle starts.
    pub const RED: Self = Self(0);

    /// The hue `steps` 393216ths of a turn from red, wrapped onto the circle.
    #[must_use]
    pub const fn from_steps(steps: u32) -> Self {
        Self(steps % Self::TURN)
    }

    /// The hue in 393216ths of a turn from red, `0..TURN`.
    #[must_use]
    pub const fn steps(self) -> u32 {
        self.0
    }

    /// The hue `degrees` round from red, to the nearest step; whole turns
    /// wrap.
    #[must_use]
    pub fn from_degrees(degrees: u32) -> Self {
        let steps = (u64::from(degrees % 360) * u64::from(Self::TURN) + 180) / 360;
        Self::from_steps(u32::try_from(steps).unwrap_or(0))
    }

    /// The nearest whole degree, `0..=359`: a hue just short of a turn is red
    /// again.
    #[must_use]
    pub fn degrees(self) -> u32 {
        let turn = u64::from(Self::TURN);
        let degrees = (u64::from(self.0) * 360 + turn / 2) / turn % 360;
        u32::try_from(degrees).unwrap_or(0)
    }

    /// An angle of any size or sign, in degrees, wrapped onto the circle — a
    /// CSS `<hue>`; one that is not finite is red.
    #[must_use]
    pub fn from_degrees_f64(degrees: f64) -> Self {
        if !degrees.is_finite() {
            return Self::RED;
        }
        let turns = degrees / 360.0;
        let part = turns - mathf::floor(turns);
        let steps = mathf::round_i32(part * f64::from(Self::TURN));
        Self::from_steps(u32::try_from(steps).unwrap_or(0))
    }

    /// The hue of `rgb`; `None` for a grey, which has none.
    #[must_use]
    pub fn of(rgb: Rgb) -> Option<Self> {
        let [r, g, b] = rgb.to_array().map(i64::from);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let chroma = max - min;
        if chroma == 0 {
            return None;
        }
        let (sixths, rise) = if max == r {
            (0, g - b)
        } else if max == g {
            (2, b - r)
        } else {
            (4, r - g)
        };
        let sextant = i64::from(Self::SEXTANT);
        let steps = sixths * sextant + nearest(rise * sextant, chroma);
        let steps = steps.rem_euclid(i64::from(Self::TURN));
        Some(Self(u32::try_from(steps).unwrap_or(0)))
    }

    /// Which sixth of the circle the hue lies in, `0..=5`, and how far
    /// through it, in 65536ths.
    fn split(self) -> (u32, u64) {
        (self.0 / Self::SEXTANT, u64::from(self.0 % Self::SEXTANT))
    }
}

/// A colour by hue, saturation and value: the coordinates a colour picker's
/// field and hue strip are drawn in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Hsv {
    /// The hue.
    pub hue: Hue,
    /// How far from grey toward the pure hue.
    pub saturation: Fraction,
    /// How far from black: the brightest channel.
    pub value: Fraction,
}

impl Hsv {
    /// The colour with these coordinates.
    #[must_use]
    pub const fn new(hue: Hue, saturation: Fraction, value: Fraction) -> Self {
        Self {
            hue,
            saturation,
            value,
        }
    }

    /// `rgb`'s coordinates. A grey has no hue and black no saturation either,
    /// so those come from `near`: a colour dragged to black keeps the hue and
    /// saturation it had.
    #[must_use]
    pub fn from_rgb(rgb: Rgb, near: Self) -> Self {
        let (max, min) = extremes(rgb);
        if max == 0 {
            return Self {
                value: Fraction::NONE,
                ..near
            };
        }
        Self {
            hue: Hue::of(rgb).unwrap_or(near.hue),
            saturation: Fraction::ratio(u32::from(max - min), u32::from(max)),
            value: Fraction::from_byte(max),
        }
    }

    /// The 8-bit colour these coordinates name, rounded to nearest.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        let value = u64::from(self.value.0);
        let saturation = u64::from(self.saturation.0);
        let (sextant, along) = self.hue.split();
        let whole = UNIT * SIXTH;
        let level = |share: u64| nearest_byte(value * share);
        arrange(
            sextant,
            Levels {
                max: level(whole),
                min: level((UNIT - saturation) * SIXTH),
                rising: level(whole - saturation * (SIXTH - along)),
                falling: level(whole - saturation * along),
            },
        )
    }
}

/// A colour by hue, saturation and lightness: CSS's `hsl()` coordinates.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Hsl {
    /// The hue.
    pub hue: Hue,
    /// How far from grey toward the pure hue, at this lightness.
    pub saturation: Fraction,
    /// How far from black to white: the mean of the brightest and darkest
    /// channels.
    pub lightness: Fraction,
}

impl Hsl {
    /// The colour with these coordinates.
    #[must_use]
    pub const fn new(hue: Hue, saturation: Fraction, lightness: Fraction) -> Self {
        Self {
            hue,
            saturation,
            lightness,
        }
    }

    /// `rgb`'s coordinates. A grey has no hue, and black and white no
    /// saturation either, so those come from `near`.
    #[must_use]
    pub fn from_rgb(rgb: Rgb, near: Self) -> Self {
        let (max, min) = extremes(rgb);
        let sum = u32::from(max) + u32::from(min);
        let reach = sum.min(510 - sum);
        Self {
            hue: Hue::of(rgb).unwrap_or(near.hue),
            saturation: if reach == 0 {
                near.saturation
            } else {
                Fraction::ratio(u32::from(max - min), reach)
            },
            lightness: Fraction::ratio(sum, 510),
        }
    }

    /// The 8-bit colour these coordinates name, rounded to nearest: CSS
    /// Color 4's conversion, exactly.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        let lightness = u64::from(self.lightness.0);
        let saturation = u64::from(self.saturation.0);
        let (sextant, along) = self.hue.split();
        let spread = saturation * lightness.min(UNIT - lightness);
        let floor = lightness * UNIT - spread;
        arrange(
            sextant,
            Levels {
                max: nearest_byte((floor + 2 * spread) * SIXTH),
                min: nearest_byte(floor * SIXTH),
                rising: nearest_byte(floor * SIXTH + 2 * spread * along),
                falling: nearest_byte(floor * SIXTH + 2 * spread * (SIXTH - along)),
            },
        )
    }
}

/// A colour's four channel levels: its brightest and darkest, and the middle
/// channel as it rises into a sextant or falls through it.
#[derive(Copy, Clone)]
struct Levels {
    max: u8,
    min: u8,
    rising: u8,
    falling: u8,
}

/// The colour whose hue lies in `sextant`, from its levels.
fn arrange(sextant: u32, levels: Levels) -> Rgb {
    let Levels {
        max,
        min,
        rising,
        falling,
    } = levels;
    match sextant {
        0 => Rgb::new(max, rising, min),
        1 => Rgb::new(falling, max, min),
        2 => Rgb::new(min, max, rising),
        3 => Rgb::new(min, falling, max),
        4 => Rgb::new(rising, min, max),
        _ => Rgb::new(max, min, falling),
    }
}

/// `rgb`'s brightest and darkest channels.
fn extremes(rgb: Rgb) -> (u8, u8) {
    let Rgb { r, g, b } = rgb;
    (r.max(g).max(b), r.min(g).min(b))
}

/// The 8-bit level nearest `share / (UNIT² · SIXTH)` of full.
fn nearest_byte(share: u64) -> u8 {
    const WHOLE: u64 = UNIT * UNIT * SIXTH;
    u8::try_from((share * 255 + WHOLE / 2) / WHOLE).unwrap_or(u8::MAX)
}

/// `n / d` to the nearest integer, halves away from zero, for `d > 0`.
fn nearest(n: i64, d: i64) -> i64 {
    let half = d / 2;
    if n >= 0 {
        (n + half) / d
    } else {
        -((half - n) / d)
    }
}
