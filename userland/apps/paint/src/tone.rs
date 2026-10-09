//! The tone and colour adjustments' settings, and the colour maps they make:
//! levels, curves, white balance, colour balance and hue by range.
//!
//! Levels, curves and white balance map each channel alone, so each is three
//! tables of 256 levels; colour balance and hue by range read the whole colour.

use tairix_colour::{
    chromaticity_of, linear_of, linear_to_srgb, srgb_to_linear, uv_of, white_of, Fraction, Hsl,
    Hue, Illuminant, Rgb, Xyz,
};
use tairix_raster::Color;
use tairix_util::mathf;

/// A channel's map: the level each of the 256 becomes.
pub type Table = [u8; 256];

/// The identity table.
pub const IDENTITY_TABLE: Table = {
    let mut table = [0u8; 256];
    let mut level: u8 = 0;
    loop {
        table[level as usize] = level;
        if level == u8::MAX {
            break table;
        }
        level += 1;
    }
};

/// `v`, a level worked out in full precision, to the nearest of the 256.
#[must_use]
pub fn to_level(v: f64) -> u8 {
    u8::try_from(mathf::round_i32(mathf::clamp(v, 0.0, 255.0)).clamp(0, 255)).unwrap_or(u8::MAX)
}

/// A channel a levels or curves adjustment is set for. The composite's map
/// follows each colour channel's own.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Channel {
    /// The three together.
    #[default]
    Composite,
    /// Red alone.
    Red,
    /// Green alone.
    Green,
    /// Blue alone.
    Blue,
}

impl Channel {
    /// Every channel, the composite first.
    pub const ALL: [Self; 4] = [Self::Composite, Self::Red, Self::Green, Self::Blue];

    /// What a list of channels calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Composite => "RGB",
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Blue => "Blue",
        }
    }

    /// Where it sits in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Composite => 0,
            Self::Red => 1,
            Self::Green => 2,
            Self::Blue => 3,
        }
    }
}

/// One channel's levels: the input levels taken as black and white, how the
/// levels between bend, and the output levels black and white become.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ChannelLevels {
    /// The input level that becomes `out_black`.
    pub black: u8,
    /// The input level that becomes `out_white`; always above `black`.
    pub white: u8,
    /// The gamma, in hundredths: above 100 lifts the levels between.
    pub gamma: u16,
    /// What black becomes.
    pub out_black: u8,
    /// What white becomes; below `out_black` turns the channel over.
    pub out_white: u8,
}

impl ChannelLevels {
    /// Levels that change nothing.
    pub const IDENTITY: Self = Self {
        black: 0,
        white: 255,
        gamma: 100,
        out_black: 0,
        out_white: 255,
    };

    /// The least and most gamma, in hundredths.
    pub const GAMMA: (u16, u16) = (10, 999);

    /// What input level `level` becomes, unrounded.
    #[must_use]
    pub fn map(&self, level: f64) -> f64 {
        let span = f64::from(self.white) - f64::from(self.black);
        let t = mathf::clamp((level - f64::from(self.black)) / span, 0.0, 1.0);
        let bent = if t <= 0.0 {
            0.0
        } else {
            mathf::exp(mathf::ln(t) * 100.0 / f64::from(self.gamma.max(1)))
        };
        f64::from(self.out_black) + (f64::from(self.out_white) - f64::from(self.out_black)) * bent
    }

    /// The input level of the grey point: where the levels between black and
    /// white reach half way.
    #[must_use]
    pub fn grey(&self) -> f64 {
        let span = f64::from(self.white) - f64::from(self.black);
        f64::from(self.black) + span * mathf::exp(mathf::ln(0.5) * f64::from(self.gamma) / 100.0)
    }

    /// Stand the grey point at input level `level`, held strictly between
    /// black and white, so the gamma is `ln p / ln ½` of the fraction `p` of
    /// the way it stands.
    pub fn set_grey(&mut self, level: f64) {
        let span = f64::from(self.white) - f64::from(self.black);
        let p = mathf::clamp((level - f64::from(self.black)) / span, 1e-4, 1.0 - 1e-4);
        self.set_gamma(mathf::ln(p) / mathf::ln(0.5) * 100.0);
    }

    /// Set the gamma, in hundredths, held to [`GAMMA`](Self::GAMMA).
    pub fn set_gamma(&mut self, hundredths: f64) {
        let (least, most) = Self::GAMMA;
        let rounded = mathf::round_i32(hundredths).clamp(i32::from(least), i32::from(most));
        self.gamma = u16::try_from(rounded).unwrap_or(least);
    }

    /// Set the input black, kept below white.
    pub fn set_black(&mut self, level: u8) {
        self.black = level.min(self.white.saturating_sub(1));
    }

    /// Set the input white, kept above black.
    pub fn set_white(&mut self, level: u8) {
        self.white = level.max(self.black.saturating_add(1));
    }
}

/// Levels for the composite and each colour channel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Levels {
    /// Each channel's levels, in [`Channel::ALL`]'s order.
    pub channels: [ChannelLevels; 4],
}

impl Levels {
    /// Levels that change nothing.
    pub const IDENTITY: Self = Self {
        channels: [ChannelLevels::IDENTITY; 4],
    };

    /// The levels of `channel`.
    #[must_use]
    pub const fn of(&self, channel: Channel) -> &ChannelLevels {
        &self.channels[channel.index()]
    }

    /// The levels of `channel`, to change.
    pub fn of_mut(&mut self, channel: Channel) -> &mut ChannelLevels {
        &mut self.channels[channel.index()]
    }

    /// Each colour channel's table: its own levels, then the composite's.
    #[must_use]
    pub fn tables(&self) -> [Table; 3] {
        let composite = self.of(Channel::Composite);
        [Channel::Red, Channel::Green, Channel::Blue].map(|channel| {
            let own = self.of(channel);
            let mut table = [0u8; 256];
            for (input, slot) in (0u8..=255).zip(table.iter_mut()) {
                *slot = to_level(composite.map(own.map(f64::from(input))));
            }
            table
        })
    }

    /// Take the picked colour as black: each colour channel's input black
    /// becomes that channel of it.
    pub fn black_point(&mut self, colour: Rgb) {
        for (channel, level) in Self::colours(colour) {
            self.of_mut(channel).set_black(level);
        }
    }

    /// Take the picked colour as white.
    pub fn white_point(&mut self, colour: Rgb) {
        for (channel, level) in Self::colours(colour) {
            self.of_mut(channel).set_white(level);
        }
    }

    /// Take the picked colour as a neutral grey: each colour channel bends so
    /// that its level of the colour lands where the colour's own luma does.
    /// A channel at or past its black or white cannot be bent so, and is left.
    pub fn grey_point(&mut self, colour: Rgb) {
        let target = f64::from(Color::rgb(colour.r, colour.g, colour.b).luma());
        for (channel, level) in Self::colours(colour) {
            let levels = self.of_mut(channel);
            let span = f64::from(levels.white) - f64::from(levels.black);
            let at = (f64::from(level) - f64::from(levels.black)) / span;
            let goal = (target - f64::from(levels.black)) / span;
            let inside = |t: f64| t > 0.0 && t < 1.0;
            if inside(at) && inside(goal) {
                levels.set_gamma(mathf::ln(at) / mathf::ln(goal) * 100.0);
            }
        }
    }

    /// Stretch each colour channel's input levels over what it holds, setting
    /// aside `clip` of its weight at each end — 0.001 of it is 0.1% — from
    /// `histogram`'s red, green and blue counts.
    pub fn auto(&mut self, histogram: &[[u64; 256]], clip: f64) {
        for (channel, counts) in [Channel::Red, Channel::Green, Channel::Blue]
            .into_iter()
            .zip(histogram)
        {
            if let Some((low, high)) = clipped_span(counts, clip) {
                let levels = self.of_mut(channel);
                levels.black = low.min(254);
                levels.white = high.max(levels.black + 1);
            }
        }
    }

    fn colours(colour: Rgb) -> [(Channel, u8); 3] {
        [
            (Channel::Red, colour.r),
            (Channel::Green, colour.g),
            (Channel::Blue, colour.b),
        ]
    }
}

/// The lowest and highest levels `counts` holds once `clip` of its weight is
/// set aside at each end, or `None` where it holds none.
#[must_use]
pub fn clipped_span(counts: &[u64; 256], clip: f64) -> Option<(u8, u8)> {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    // The weight set aside at each end, held below the whole so a level is
    // always found.
    let aside = mathf::fmin(
        mathf::fmax(clip, 0.0) * u64_f64(total),
        u64_f64(total) - 1.0,
    );
    let low = first_past(counts.iter().enumerate(), aside)?;
    let high = first_past(counts.iter().enumerate().rev(), aside)?;
    Some((low, high.max(low)))
}

/// The first level, in the order `levels` runs, at which the weight seen
/// passes `aside`.
fn first_past<'a>(levels: impl Iterator<Item = (usize, &'a u64)>, aside: f64) -> Option<u8> {
    let mut seen = 0u64;
    levels
        .map(|(level, count)| {
            seen += count;
            (level, seen)
        })
        .find(|&(_, seen)| u64_f64(seen) > aside)
        .and_then(|(level, _)| u8::try_from(level).ok())
}

/// A count as a float: exact to 2^53, past which a level's share is no longer
/// told apart by a unit.
fn u64_f64(count: u64) -> f64 {
    let high = u32::try_from(count >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(count & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// The most points a curve holds.
pub const MOST_POINTS: usize = 16;

/// A tone curve: points through which the curve runs as a monotone cubic
/// (Fritsch and Carlson, "Monotone Piecewise Cubic Interpolation", SIAM J.
/// Numer. Anal. 1980), flat beyond its first and last.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Curve {
    /// The points, inputs rising; slots past `count` are zero.
    points: [(u8, u8); MOST_POINTS],
    count: u8,
}

impl Curve {
    /// The curve that changes nothing.
    pub const IDENTITY: Self = {
        let mut points = [(0, 0); MOST_POINTS];
        points[1] = (255, 255);
        Self { points, count: 2 }
    };

    /// The points, inputs rising.
    #[must_use]
    pub fn points(&self) -> &[(u8, u8)] {
        &self.points[..usize::from(self.count)]
    }

    /// Whether it maps every level to itself.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.table() == IDENTITY_TABLE
    }

    /// Add a point at `at`, answering where it landed; `None` where the curve
    /// is full or a point already stands at that input.
    pub fn add(&mut self, at: (u8, u8)) -> Option<usize> {
        let count = usize::from(self.count);
        if count == MOST_POINTS || self.points().iter().any(|point| point.0 == at.0) {
            return None;
        }
        let index = self.points().partition_point(|point| point.0 < at.0);
        self.points.copy_within(index..count, index + 1);
        self.points[index] = at;
        self.count += 1;
        Some(index)
    }

    /// Move point `index` to `to`, its input held strictly between its
    /// neighbours'; answers where it stands.
    pub fn set(&mut self, index: usize, to: (u8, u8)) -> Option<(u8, u8)> {
        let count = usize::from(self.count);
        if index >= count {
            return None;
        }
        let least = index
            .checked_sub(1)
            .map_or(0, |before| self.points[before].0.saturating_add(1));
        let most = self
            .points()
            .get(index + 1)
            .map_or(255, |after| after.0.saturating_sub(1));
        let placed = (to.0.clamp(least, most.max(least)), to.1);
        self.points[index] = placed;
        Some(placed)
    }

    /// Take point `index` away; a curve keeps two.
    pub fn remove(&mut self, index: usize) -> bool {
        let count = usize::from(self.count);
        if count <= 2 || index >= count {
            return false;
        }
        self.points.copy_within(index + 1..count, index);
        self.points[count - 1] = (0, 0);
        self.count -= 1;
        true
    }

    /// The tangent at each point: the mean of the secants either side, flat
    /// where they turn, and held within the region that keeps each span
    /// monotone.
    fn tangents(&self) -> [f64; MOST_POINTS] {
        let points = self.points();
        let mut secants = [0.0; MOST_POINTS];
        for (slot, pair) in secants.iter_mut().zip(points.windows(2)) {
            let (x0, y0) = (f64::from(pair[0].0), f64::from(pair[0].1));
            let (x1, y1) = (f64::from(pair[1].0), f64::from(pair[1].1));
            *slot = (y1 - y0) / (x1 - x0);
        }
        let spans = points.len().saturating_sub(1);
        let mut tangents = [0.0; MOST_POINTS];
        for (index, tangent) in tangents.iter_mut().enumerate().take(points.len()) {
            *tangent = if index == 0 {
                secants[0]
            } else if index == spans {
                secants[spans - 1]
            } else {
                let (before, after) = (secants[index - 1], secants[index]);
                if before * after <= 0.0 {
                    0.0
                } else {
                    before.midpoint(after)
                }
            };
        }
        for span in 0..spans {
            let secant = secants[span];
            if secant == 0.0 {
                tangents[span] = 0.0;
                tangents[span + 1] = 0.0;
                continue;
            }
            let alpha = tangents[span] / secant;
            let beta = tangents[span + 1] / secant;
            let reach = alpha * alpha + beta * beta;
            if reach > 9.0 {
                let tau = 3.0 / mathf::sqrt(reach);
                tangents[span] = tau * alpha * secant;
                tangents[span + 1] = tau * beta * secant;
            }
        }
        tangents
    }

    /// What input level `x` becomes, unrounded and held to the levels.
    #[must_use]
    pub fn value_at(&self, x: f64) -> f64 {
        self.value_with(x, &self.tangents())
    }

    fn value_with(&self, x: f64, tangents: &[f64; MOST_POINTS]) -> f64 {
        let points = self.points();
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return x;
        };
        if x <= f64::from(first.0) {
            return f64::from(first.1);
        }
        if x >= f64::from(last.0) {
            return f64::from(last.1);
        }
        let span = points
            .partition_point(|point| f64::from(point.0) <= x)
            .saturating_sub(1)
            .min(points.len() - 2);
        let (x0, y0) = (f64::from(points[span].0), f64::from(points[span].1));
        let (x1, y1) = (f64::from(points[span + 1].0), f64::from(points[span + 1].1));
        let h = x1 - x0;
        let t = (x - x0) / h;
        let (t2, t3) = (t * t, t * t * t);
        let value = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
            + (t3 - 2.0 * t2 + t) * h * tangents[span]
            + (-2.0 * t3 + 3.0 * t2) * y1
            + (t3 - t2) * h * tangents[span + 1];
        mathf::clamp(value, 0.0, 255.0)
    }

    /// The level each of the 256 becomes.
    #[must_use]
    pub fn table(&self) -> Table {
        let tangents = self.tangents();
        let mut table = [0u8; 256];
        for (input, slot) in (0u8..=255).zip(table.iter_mut()) {
            *slot = to_level(self.value_with(f64::from(input), &tangents));
        }
        table
    }
}

/// Curves for the composite and each colour channel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Curves {
    /// Each channel's curve, in [`Channel::ALL`]'s order.
    pub channels: [Curve; 4],
}

impl Curves {
    /// Curves that change nothing.
    pub const IDENTITY: Self = Self {
        channels: [Curve::IDENTITY; 4],
    };

    /// The curve of `channel`.
    #[must_use]
    pub const fn of(&self, channel: Channel) -> &Curve {
        &self.channels[channel.index()]
    }

    /// The curve of `channel`, to change.
    pub fn of_mut(&mut self, channel: Channel) -> &mut Curve {
        &mut self.channels[channel.index()]
    }

    /// Each colour channel's table: its own curve, then the composite's.
    #[must_use]
    pub fn tables(&self) -> [Table; 3] {
        let composite = self.of(Channel::Composite);
        let tangents = composite.tangents();
        [Channel::Red, Channel::Green, Channel::Blue].map(|channel| {
            let own = self.of(channel);
            let own_tangents = own.tangents();
            let mut table = [0u8; 256];
            for (input, slot) in (0u8..=255).zip(table.iter_mut()) {
                let through = own.value_with(f64::from(input), &own_tangents);
                *slot = to_level(composite.value_with(through, &tangents));
            }
            table
        })
    }

    /// Whether every curve maps every level to itself.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.channels.iter().all(Curve::is_identity)
    }
}

/// White balance: the temperature and tint the picture's light is taken to
/// have, corrected to daylight.
///
/// The temperature and tint are read from daylight: a light is daylight's
/// white, D65 — sRGB's own — moved across the CIE 1960 uv plane as far as the
/// named temperature and tint lie from 6500 K on the Planckian locus. So the
/// neutral settings change nothing, and a light the picker reads from a
/// colour is corrected to a true sRGB grey.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WhiteBalance {
    /// The light's correlated colour temperature, in kelvin.
    pub kelvin: u16,
    /// Its tint: positive turns the picture toward magenta, negative toward
    /// green.
    pub tint: i16,
}

impl WhiteBalance {
    /// The balance that changes nothing.
    pub const NEUTRAL: Self = Self {
        kelvin: 6500,
        tint: 0,
    };

    /// The least and most temperature, in kelvin.
    pub const KELVIN: (i32, i32) = (2000, 12000);

    /// The furthest the tint reaches either way.
    pub const TINT: i32 = 100;

    /// Duv per unit of tint: the whole reach is a strong cast, beyond which
    /// a picture is not balanced but recoloured.
    const DUV_PER_TINT: f64 = 0.0003;

    /// The point of the locus these settings name. A greener light is
    /// corrected toward magenta, so tint and Duv share their sign.
    fn illuminant(self) -> Illuminant {
        Illuminant::new(
            f64::from(self.kelvin),
            f64::from(self.tint) * Self::DUV_PER_TINT,
        )
    }

    /// How far these settings move a light from daylight across the uv
    /// plane.
    fn offset(self) -> (f64, f64) {
        let (u, v) = self.illuminant().uv();
        let (u0, v0) = Self::NEUTRAL.illuminant().uv();
        (u - u0, v - v0)
    }

    /// Daylight's white in the uv plane.
    fn daylight() -> (f64, f64) {
        Xyz::D65
            .chromaticity()
            .and_then(uv_of)
            .unwrap_or((0.197_8, 0.312_2))
    }

    /// The white of the light these settings take the picture to be lit by,
    /// as linear sRGB at luminance one.
    fn light(self) -> [f64; 3] {
        let (du, dv) = self.offset();
        let (u, v) = Self::daylight();
        white_of(chromaticity_of((u + du, v + dv)))
    }

    /// What each linear channel is multiplied by: the von Kries ratio of
    /// daylight's white — one in every channel — to this light's, scaled so
    /// that a grey keeps its luminance.
    #[must_use]
    pub fn gains(self) -> [f64; 3] {
        if self == Self::NEUTRAL {
            return [1.0; 3];
        }
        let light = self.light();
        let ratio = [0, 1, 2].map(|c| 1.0 / mathf::fmax(light[c], 1e-6));
        // sRGB's luminance weights (IEC 61966-2-1).
        let luminance = 0.212_672_9 * ratio[0] + 0.715_152_2 * ratio[1] + 0.072_175_0 * ratio[2];
        ratio.map(|gain| gain / mathf::fmax(luminance, 1e-6))
    }

    /// Each colour channel's table.
    #[must_use]
    pub fn tables(self) -> [Table; 3] {
        if self == Self::NEUTRAL {
            return [IDENTITY_TABLE; 3];
        }
        self.gains().map(|gain| {
            let mut table = [0u8; 256];
            for (input, slot) in (0u8..=255).zip(table.iter_mut()) {
                let linear = srgb_to_linear(f64::from(input) / 255.0) * gain;
                *slot = to_level(linear_to_srgb(mathf::clamp(linear, 0.0, 1.0)) * 255.0);
            }
            table
        })
    }

    /// What a mid grey becomes: the colour a track shows these settings as.
    #[must_use]
    pub fn cast(self) -> Rgb {
        let gains = self.gains();
        let grey = srgb_to_linear(0.5);
        let [r, g, b] = [0, 1, 2]
            .map(|c| to_level(linear_to_srgb(mathf::clamp(grey * gains[c], 0.0, 1.0)) * 255.0));
        Rgb::new(r, g, b)
    }

    /// The balance that makes `colour` grey, held to the settings' reach;
    /// `None` for black, which has no colour to read.
    #[must_use]
    pub fn neutralising(colour: Rgb) -> Option<Self> {
        Self::neutralising_linear(linear_of(colour))
    }

    /// As [`neutralising`](Self::neutralising), for linear light.
    #[must_use]
    pub fn neutralising_linear(linear: [f64; 3]) -> Option<Self> {
        let (u, v) = uv_of(Xyz::from_linear(linear).chromaticity()?)?;
        let (u_day, v_day) = Self::daylight();
        let (u0, v0) = Self::NEUTRAL.illuminant().uv();
        let light = Illuminant::of_uv((u - u_day + u0, v - v_day + v0));
        let (least, most) = Self::KELVIN;
        let kelvin = mathf::round_i32(light.kelvin).clamp(least, most);
        let tint = mathf::round_i32(light.duv / Self::DUV_PER_TINT).clamp(-Self::TINT, Self::TINT);
        Some(Self {
            kelvin: u16::try_from(kelvin).unwrap_or(Self::NEUTRAL.kelvin),
            tint: i16::try_from(tint).unwrap_or(0),
        })
    }
}

/// A band of tones colour balance is set for.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Tones {
    /// The dark tones.
    Shadows,
    /// The middle tones.
    #[default]
    Midtones,
    /// The light tones.
    Highlights,
}

impl Tones {
    /// Every band, darkest first.
    pub const ALL: [Self; 3] = [Self::Shadows, Self::Midtones, Self::Highlights];

    /// What a list of bands calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Shadows => "Shadows",
            Self::Midtones => "Midtones",
            Self::Highlights => "Highlights",
        }
    }

    /// Where it sits in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Shadows => 0,
            Self::Midtones => 1,
            Self::Highlights => 2,
        }
    }
}

/// Colour balance: for each band of tones, how far toward red, green and
/// blue — away from cyan, magenta and yellow — its colours are moved.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ColourBalance {
    /// Each band's cyan–red, magenta–green and yellow–blue, each
    /// `-100..=100`, in [`Tones::ALL`]'s order.
    pub tones: [[i8; 3]; 3],
    /// Whether each colour keeps the lightness it had.
    pub keep_luminosity: bool,
}

impl ColourBalance {
    /// The balance that changes nothing.
    pub const NEUTRAL: Self = Self {
        tones: [[0; 3]; 3],
        keep_luminosity: true,
    };

    /// The three axes, as a list of them reads: what each end of its slider
    /// names.
    pub const AXES: [(&'static str, &'static str); 3] =
        [("Cyan", "Red"), ("Magenta", "Green"), ("Yellow", "Blue")];

    /// Whether it changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.tones == Self::NEUTRAL.tones
    }

    /// How far each colour channel moves at each lightness, in levels.
    ///
    /// The bands' weights are GIMP's colour-balance masks: shadows fade out and
    /// highlights fade in over ramps a quarter wide about a third and two
    /// thirds of the way, midtones between, and the three always sum to the
    /// same, so one move in every band is that move everywhere.
    #[must_use]
    pub fn shifts(&self) -> [[i16; 256]; 3] {
        const RAMP: f64 = 0.25;
        const KNEE: f64 = 0.333;
        const SCALE: f64 = 0.7;
        let mut shifts = [[0i16; 256]; 3];
        for level in 0..256u16 {
            let l = f64::from(level) / 255.0;
            let shadows = mathf::clamp((l - KNEE) / -RAMP + 0.5, 0.0, 1.0) * SCALE;
            let midtones = mathf::clamp((l - KNEE) / RAMP + 0.5, 0.0, 1.0)
                * mathf::clamp((l + KNEE - 1.0) / -RAMP + 0.5, 0.0, 1.0)
                * SCALE;
            let highlights = mathf::clamp((l + KNEE - 1.0) / RAMP + 0.5, 0.0, 1.0) * SCALE;
            let weights = [shadows, midtones, highlights];
            for (axis, channel) in shifts.iter_mut().enumerate() {
                let moved: f64 = weights
                    .iter()
                    .zip(&self.tones)
                    .map(|(weight, band)| weight * f64::from(band[axis]) / 100.0)
                    .sum();
                channel[usize::from(level)] =
                    i16::try_from(mathf::round_i32(moved * 255.0)).unwrap_or(0);
            }
        }
        shifts
    }

    /// What `colour` becomes, its channels moved by `shifts` (from
    /// [`shifts`](Self::shifts)) at its lightness.
    #[must_use]
    pub fn map(&self, colour: Rgb, shifts: &[[i16; 256]; 3]) -> Rgb {
        let lightest = colour.r.max(colour.g).max(colour.b);
        let darkest = colour.r.min(colour.g).min(colour.b);
        let lightness = usize::from(
            u8::try_from((u16::from(lightest) + u16::from(darkest)).div_ceil(2)).unwrap_or(u8::MAX),
        );
        let moved = |level: u8, channel: usize| {
            u8::try_from((i16::from(level) + shifts[channel][lightness]).clamp(0, 255))
                .unwrap_or(u8::MAX)
        };
        let balanced = Rgb::new(moved(colour.r, 0), moved(colour.g, 1), moved(colour.b, 2));
        if !self.keep_luminosity {
            return balanced;
        }
        let before = Hsl::from_rgb(colour, Hsl::default());
        let after = Hsl::from_rgb(balanced, Hsl::default());
        Hsl::new(after.hue, after.saturation, before.lightness).to_rgb()
    }
}

/// A range of hues hue and saturation is set for: all of them, or one of the
/// six colours either side of which its setting fades out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum HueRange {
    /// Every colour.
    #[default]
    Master,
    /// About red.
    Reds,
    /// About yellow.
    Yellows,
    /// About green.
    Greens,
    /// About cyan.
    Cyans,
    /// About blue.
    Blues,
    /// About magenta.
    Magentas,
}

impl HueRange {
    /// Every range, the master first and then round the colour circle from
    /// red.
    pub const ALL: [Self; 7] = [
        Self::Master,
        Self::Reds,
        Self::Yellows,
        Self::Greens,
        Self::Cyans,
        Self::Blues,
        Self::Magentas,
    ];

    /// What a list of ranges calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Master => "Master",
            Self::Reds => "Reds",
            Self::Yellows => "Yellows",
            Self::Greens => "Greens",
            Self::Cyans => "Cyans",
            Self::Blues => "Blues",
            Self::Magentas => "Magentas",
        }
    }

    /// Where it sits in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Master => 0,
            Self::Reds => 1,
            Self::Yellows => 2,
            Self::Greens => 3,
            Self::Cyans => 4,
            Self::Blues => 5,
            Self::Magentas => 6,
        }
    }

    /// The hue a colour range is centred on, in degrees; `None` for the
    /// master.
    #[must_use]
    pub const fn centre(self) -> Option<u16> {
        match self {
            Self::Master => None,
            Self::Reds => Some(0),
            Self::Yellows => Some(60),
            Self::Greens => Some(120),
            Self::Cyans => Some(180),
            Self::Blues => Some(240),
            Self::Magentas => Some(300),
        }
    }
}

/// How far one range of hues is turned, saturated and lightened.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct HueShift {
    /// Degrees round the colour circle, `-180..=180`.
    pub hue: i16,
    /// Toward the pure hue or toward grey, `-100..=100`.
    pub saturation: i8,
    /// Toward white or toward black, `-100..=100`.
    pub lightness: i8,
}

impl HueShift {
    /// No shift.
    pub const NONE: Self = Self {
        hue: 0,
        saturation: 0,
        lightness: 0,
    };
}

/// Hue and saturation for the master and each colour range.
///
/// A colour takes the master's shift and, of the two colour ranges its hue
/// lies between, each one's in proportion to how near it lies to that range's
/// centre, so a range's setting fades to nothing a range away. A range's
/// lightness is weighed by how saturated the colour is: a grey has no hue to
/// lie in a range by, and keeps its lightness.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct HueRanges {
    /// Each range's shift, in [`HueRange::ALL`]'s order.
    pub shifts: [HueShift; 7],
}

impl HueRanges {
    /// Hue and saturation that change nothing.
    pub const IDENTITY: Self = Self {
        shifts: [HueShift::NONE; 7],
    };

    /// The shift of `range`.
    #[must_use]
    pub const fn of(&self, range: HueRange) -> &HueShift {
        &self.shifts[range.index()]
    }

    /// The shift of `range`, to change.
    pub fn of_mut(&mut self, range: HueRange) -> &mut HueShift {
        &mut self.shifts[range.index()]
    }

    /// Whether it changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// The hue, saturation and lightness a colour at `hue` degrees and
    /// `saturation` (`0.0..=1.0`) is moved by.
    #[must_use]
    pub fn shift_at(&self, hue: f64, saturation: f64) -> (f64, f64, f64) {
        let sextant = mathf::clamp(hue, 0.0, 360.0) / 60.0;
        let below = mathf::floor(sextant);
        let toward = sextant - below;
        let first = usize::try_from(mathf::round_i32(below).rem_euclid(6)).unwrap_or(0);
        let ranges = &self.shifts[1..];
        let (a, b) = (ranges[first], ranges[(first + 1) % 6]);
        let blend = |of: fn(&HueShift) -> f64| of(&a) * (1.0 - toward) + of(&b) * toward;
        let master = self.shifts[0];
        (
            f64::from(master.hue) + blend(|shift| f64::from(shift.hue)),
            mathf::clamp(
                f64::from(master.saturation) + blend(|shift| f64::from(shift.saturation)),
                -100.0,
                100.0,
            ),
            mathf::clamp(
                f64::from(master.lightness)
                    + saturation * blend(|shift| f64::from(shift.lightness)),
                -100.0,
                100.0,
            ),
        )
    }

    /// What `colour` becomes.
    #[must_use]
    pub fn map(&self, colour: Rgb) -> Rgb {
        let hsl = Hsl::from_rgb(colour, Hsl::default());
        let degrees = f64::from(hsl.hue.steps()) * 360.0 / f64::from(Hue::TURN);
        let saturation = f64::from(hsl.saturation.raw()) / 65535.0;
        let (turn, saturate, lighten) = self.shift_at(degrees, saturation);
        let saturation = mathf::clamp(saturation * (1.0 + saturate / 100.0), 0.0, 1.0);
        let lightness = f64::from(hsl.lightness.raw()) / 65535.0;
        let lightness = if lighten >= 0.0 {
            lightness + (1.0 - lightness) * lighten / 100.0
        } else {
            lightness * (1.0 + lighten / 100.0)
        };
        Hsl::new(
            Hue::from_degrees_f64(degrees + turn),
            Fraction::from_f64(saturation),
            Fraction::from_f64(lightness),
        )
        .to_rgb()
    }
}

#[cfg(test)]
#[path = "tone_tests.rs"]
mod tests;
