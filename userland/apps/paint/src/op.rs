//! The operations the Adjustments and Effects panes offer, and what each is
//! set to.
//!
//! An adjustment maps each colour on its own ([`tone`](crate::tone)), so on a
//! palette picture it maps the palette; an effect makes each pixel from its
//! neighbourhood ([`effect`](crate::effect)), which only a colour picture's
//! pixels can take. Either is worked out a block of tiles at a time
//! ([`tiled`](crate::tiled)) and held to the selection, as much of each pixel
//! as it chooses.

use alloc::vec::Vec;
use core::fmt;

use tairix_abi::window_ipc::AppMenuSymbol;
use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::OutOfMemory;
use crate::effect::{
    Effect, Noise, MOST_BLUR, MOST_EMBOSS, MOST_MEDIAN, MOST_RELIEF, MOST_SHARPEN,
};
use crate::tone::{
    brightness_table, posterise_table, ChannelMixer, ColourBalance, Curves, HueRanges, Levels,
    Table, ToneMap, WhiteBalance, INVERT_TABLE,
};

/// Which pane offers an operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Family {
    /// The operations that map each colour on its own.
    Adjustments,
    /// The operations that make each pixel from its neighbours, which only a
    /// colour picture can take.
    Effects,
}

impl Family {
    /// Both, the Adjustments first.
    pub const ALL: [Self; 2] = [Self::Adjustments, Self::Effects];

    /// What the pane's band calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Adjustments => "Adjustments",
            Self::Effects => "Effects",
        }
    }

    /// What the pane's selector is captioned.
    #[must_use]
    pub const fn caption(self) -> &'static str {
        match self {
            Self::Adjustments => "Adjustment",
            Self::Effects => "Effect",
        }
    }

    /// Its operations as the selector lists them, at their starting settings.
    #[must_use]
    pub const fn ops(self) -> &'static [Op] {
        match self {
            Self::Adjustments => &Op::ADJUSTMENTS,
            Self::Effects => &Op::EFFECTS,
        }
    }

    /// The operation its pane opens on.
    #[must_use]
    pub const fn first(self) -> Op {
        match self {
            Self::Adjustments => Op::ADJUSTMENTS[0],
            Self::Effects => Op::EFFECTS[0],
        }
    }
}

/// One number an operation is set by: what its slider says and holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    /// What its slider is labelled.
    pub label: &'static str,
    /// The least it holds.
    pub least: i32,
    /// The most it holds.
    pub most: i32,
}

impl Parameter {
    /// Where `value` lies along the slider, in thousandths.
    #[must_use]
    pub fn permille_of(&self, value: i32) -> u16 {
        let span = i64::from(self.most - self.least).max(1);
        let along = i64::from(value.clamp(self.least, self.most) - self.least);
        u16::try_from(along * 1000 / span).unwrap_or(1000)
    }

    /// The value `permille` thousandths along the slider, to the nearest.
    #[must_use]
    pub fn value_of(&self, permille: u16) -> i32 {
        let span = i64::from(self.most - self.least);
        let along = (i64::from(permille.min(1000)) * span + 500) / 1000;
        i32::try_from(i64::from(self.least) + along).unwrap_or(self.least)
    }

    /// The slider's steps, in thousandths: a line moves one, a page ten.
    #[must_use]
    pub fn steps(&self) -> (u16, u16) {
        let span = u16::try_from(self.most - self.least).unwrap_or(1).max(1);
        let line = 1000u16.div_ceil(span);
        (line, line.saturating_mul(10).min(1000))
    }
}

/// One operation and what it is set to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Op {
    /// Brightness and contrast, each `-100..=100`.
    BrightnessContrast {
        /// Lighter or darker.
        brightness: i32,
        /// Further from or nearer to mid grey.
        contrast: i32,
    },
    /// Each channel's input and output levels.
    Levels(Levels),
    /// Each channel's tone curve.
    Curves(Curves),
    /// Hue, saturation and lightness for every colour and for each range.
    HueSaturation(HueRanges),
    /// Colours moved toward red, green or blue in each band of tones.
    ColourBalance(ColourBalance),
    /// The light the picture was lit by, corrected to daylight.
    WhiteBalance(WhiteBalance),
    /// Each colour its grey.
    Desaturate,
    /// Every colour its opposite, opacity kept.
    Invert,
    /// Each channel held to so many levels.
    Posterise {
        /// The levels each channel keeps.
        levels: i32,
    },
    /// Black below a level of brightness, white from it.
    Threshold {
        /// The brightness from which a pixel is white.
        level: i32,
    },
    /// Each channel weighed from all three.
    ChannelMixer(ChannelMixer),
    /// A blur near a Gaussian.
    GaussianBlur {
        /// Its standard deviation, in pixels.
        radius: i32,
    },
    /// Each pixel pushed away from its blurred neighbourhood.
    UnsharpMask {
        /// How far, in percent.
        amount: i32,
        /// The blur's standard deviation, in pixels.
        radius: i32,
        /// The least difference from the blur, in levels, that is pushed.
        threshold: i32,
    },
    /// Each channel the median of the square about it.
    Median {
        /// How far the square reaches either side, in pixels.
        radius: i32,
    },
    /// Speckle, the same wherever the picture is read.
    AddNoise(Noise),
    /// The edges between colours lit, all else dark.
    FindEdges,
    /// A grey relief, lit from one side.
    Emboss {
        /// Where the light comes from, in degrees anticlockwise from the right.
        angle: i32,
        /// How far ahead of and behind each pixel it is read, in pixels.
        height: i32,
        /// How strong, in percent.
        amount: i32,
    },
    /// Squares of one colour, each the mean of what it covers.
    Pixelate {
        /// How wide each square is.
        cell: i32,
    },
}

impl Op {
    /// The Adjustments pane's operations at their starting settings, as its
    /// selector lists them.
    pub const ADJUSTMENTS: [Self; 11] = [
        Self::BrightnessContrast {
            brightness: 0,
            contrast: 0,
        },
        Self::Levels(Levels::IDENTITY),
        Self::Curves(Curves::IDENTITY),
        Self::HueSaturation(HueRanges::IDENTITY),
        Self::ColourBalance(ColourBalance::NEUTRAL),
        Self::WhiteBalance(WhiteBalance::NEUTRAL),
        Self::Desaturate,
        Self::Invert,
        Self::Posterise { levels: 4 },
        Self::Threshold { level: 128 },
        Self::ChannelMixer(ChannelMixer::IDENTITY),
    ];

    /// The Effects pane's operations at their starting settings, as its
    /// selector lists them.
    pub const EFFECTS: [Self; 7] = [
        Self::GaussianBlur { radius: 2 },
        Self::UnsharpMask {
            amount: 100,
            radius: 2,
            threshold: 0,
        },
        Self::Median { radius: 1 },
        Self::AddNoise(Noise::START),
        Self::FindEdges,
        Self::Emboss {
            angle: 135,
            height: 1,
            amount: 100,
        },
        Self::Pixelate { cell: 8 },
    ];

    /// How many operations the two panes offer between them.
    pub const COUNT: usize = Self::ADJUSTMENTS.len() + Self::EFFECTS.len();

    /// Every operation at its starting settings, the Adjustments pane's first:
    /// what the Adjust menu lists, [`COUNT`](Self::COUNT) of them.
    pub fn every() -> impl Iterator<Item = Self> {
        Family::ALL
            .into_iter()
            .flat_map(|family| family.ops().iter().copied())
    }

    /// What its selector calls it.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::BrightnessContrast { .. } => "Brightness & contrast",
            Self::Levels(_) => "Levels",
            Self::Curves(_) => "Curves",
            Self::HueSaturation(_) => "Hue & saturation",
            Self::ColourBalance(_) => "Colour balance",
            Self::WhiteBalance(_) => "White balance",
            Self::Desaturate => "Desaturate",
            Self::Invert => "Invert",
            Self::Posterise { .. } => "Posterise",
            Self::Threshold { .. } => "Threshold",
            Self::ChannelMixer(_) => "Channel mixer",
            Self::GaussianBlur { .. } => "Gaussian blur",
            Self::UnsharpMask { .. } => "Unsharp mask",
            Self::Median { .. } => "Median",
            Self::AddNoise(_) => "Add noise",
            Self::FindEdges => "Find edges",
            Self::Emboss { .. } => "Emboss",
            Self::Pixelate { .. } => "Pixelate",
        }
    }

    /// The function the Adjust menu's row opening it names.
    #[must_use]
    pub const fn symbol(&self) -> AppMenuSymbol {
        match self {
            Self::BrightnessContrast { .. } => AppMenuSymbol::Brightness,
            Self::Levels(_) => AppMenuSymbol::Levels,
            Self::Curves(_) => AppMenuSymbol::Curves,
            Self::HueSaturation(_) => AppMenuSymbol::HueSaturation,
            Self::ColourBalance(_) => AppMenuSymbol::ColourBalance,
            Self::WhiteBalance(_) => AppMenuSymbol::WhiteBalance,
            Self::Desaturate => AppMenuSymbol::Desaturate,
            Self::Invert => AppMenuSymbol::InvertColours,
            Self::Posterise { .. } => AppMenuSymbol::Posterise,
            Self::Threshold { .. } => AppMenuSymbol::Threshold,
            Self::ChannelMixer(_) => AppMenuSymbol::ChannelMixer,
            Self::GaussianBlur { .. } => AppMenuSymbol::Blur,
            Self::UnsharpMask { .. } => AppMenuSymbol::Sharpen,
            Self::Median { .. } => AppMenuSymbol::Median,
            Self::AddNoise(_) => AppMenuSymbol::Noise,
            Self::FindEdges => AppMenuSymbol::FindEdges,
            Self::Emboss { .. } => AppMenuSymbol::Emboss,
            Self::Pixelate { .. } => AppMenuSymbol::Pixelate,
        }
    }

    /// Which pane offers it.
    #[must_use]
    pub const fn family(&self) -> Family {
        match self {
            Self::BrightnessContrast { .. }
            | Self::Levels(_)
            | Self::Curves(_)
            | Self::HueSaturation(_)
            | Self::ColourBalance(_)
            | Self::WhiteBalance(_)
            | Self::Desaturate
            | Self::Invert
            | Self::Posterise { .. }
            | Self::Threshold { .. }
            | Self::ChannelMixer(_) => Family::Adjustments,
            Self::GaussianBlur { .. }
            | Self::UnsharpMask { .. }
            | Self::Median { .. }
            | Self::AddNoise(_)
            | Self::FindEdges
            | Self::Emboss { .. }
            | Self::Pixelate { .. } => Family::Effects,
        }
    }

    /// Whether `other` is the same operation, however each is set.
    #[must_use]
    pub fn same_kind(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }

    /// Where it stands in its family's list.
    #[must_use]
    pub fn place(&self) -> usize {
        self.family()
            .ops()
            .iter()
            .position(|op| op.same_kind(self))
            .unwrap_or(0)
    }

    /// Whether it needs a colour picture's pixels: every effect does, as a
    /// palette picture's pixels are one entry each.
    #[must_use]
    pub const fn needs_colour(&self) -> bool {
        matches!(self.family(), Family::Effects)
    }

    /// Whether, as set, it changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        match self {
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => *brightness == 0 && *contrast == 0,
            Self::Levels(levels) => *levels == Levels::IDENTITY,
            Self::Curves(curves) => curves.is_identity(),
            Self::HueSaturation(ranges) => ranges.is_identity(),
            Self::ColourBalance(balance) => balance.is_identity(),
            Self::WhiteBalance(balance) => *balance == WhiteBalance::NEUTRAL,
            Self::ChannelMixer(mixer) => mixer.is_identity(),
            Self::GaussianBlur { radius } | Self::Median { radius } => *radius <= 0,
            Self::UnsharpMask { amount, .. } => *amount <= 0,
            Self::AddNoise(noise) => noise.amount <= 0,
            Self::Pixelate { cell } => *cell <= 1,
            Self::Desaturate
            | Self::Invert
            | Self::Posterise { .. }
            | Self::Threshold { .. }
            | Self::FindEdges
            | Self::Emboss { .. } => false,
        }
    }

    /// The numbers it is set by, in its sliders' order; an operation set by
    /// channel, range, band or output has settings of its own beside these.
    #[must_use]
    pub const fn parameters(&self) -> &'static [Parameter] {
        const fn p(label: &'static str, least: i32, most: i32) -> Parameter {
            Parameter { label, least, most }
        }
        const BRIGHTNESS: [Parameter; 2] = [p("Brightness", -100, 100), p("Contrast", -100, 100)];
        const POSTERISE: [Parameter; 1] = [p("Levels", 2, 64)];
        const THRESHOLD: [Parameter; 1] = [p("Level", 0, 255)];
        const GAUSSIAN: [Parameter; 1] = [p("Radius", 0, MOST_BLUR)];
        const UNSHARP: [Parameter; 3] = [
            p("Amount", 0, MOST_SHARPEN),
            p("Radius", 1, MOST_BLUR),
            p("Threshold", 0, 255),
        ];
        const MEDIAN: [Parameter; 1] = [p("Radius", 0, MOST_MEDIAN)];
        const NOISE: [Parameter; 1] = [p("Amount", 0, 100)];
        const EMBOSS: [Parameter; 3] = [
            p("Angle", -180, 180),
            p("Height", 1, MOST_EMBOSS),
            p("Amount", 1, MOST_RELIEF),
        ];
        const PIXELATE: [Parameter; 1] = [p("Size", 1, 128)];
        match self {
            Self::BrightnessContrast { .. } => &BRIGHTNESS,
            Self::Posterise { .. } => &POSTERISE,
            Self::Threshold { .. } => &THRESHOLD,
            Self::GaussianBlur { .. } => &GAUSSIAN,
            Self::UnsharpMask { .. } => &UNSHARP,
            Self::Median { .. } => &MEDIAN,
            Self::AddNoise(_) => &NOISE,
            Self::Emboss { .. } => &EMBOSS,
            Self::Pixelate { .. } => &PIXELATE,
            Self::Levels(_)
            | Self::Curves(_)
            | Self::HueSaturation(_)
            | Self::ColourBalance(_)
            | Self::WhiteBalance(_)
            | Self::Desaturate
            | Self::Invert
            | Self::ChannelMixer(_)
            | Self::FindEdges => &[],
        }
    }

    /// The value of number `index`.
    #[must_use]
    pub const fn value(&self, index: usize) -> i32 {
        const fn nth(values: &[i32], index: usize) -> i32 {
            if index < values.len() {
                values[index]
            } else {
                0
            }
        }
        match *self {
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => nth(&[brightness, contrast], index),
            Self::Posterise { levels: v }
            | Self::Threshold { level: v }
            | Self::GaussianBlur { radius: v }
            | Self::Median { radius: v }
            | Self::Pixelate { cell: v } => nth(&[v], index),
            Self::AddNoise(noise) => nth(&[noise.amount], index),
            Self::UnsharpMask {
                amount,
                radius,
                threshold,
            } => nth(&[amount, radius, threshold], index),
            Self::Emboss {
                angle,
                height,
                amount,
            } => nth(&[angle, height, amount], index),
            Self::Levels(_)
            | Self::Curves(_)
            | Self::HueSaturation(_)
            | Self::ColourBalance(_)
            | Self::WhiteBalance(_)
            | Self::Desaturate
            | Self::Invert
            | Self::ChannelMixer(_)
            | Self::FindEdges => 0,
        }
    }

    /// Set number `index` to `value`, held to its bounds.
    pub fn set(&mut self, index: usize, value: i32) {
        fn put<const N: usize>(slots: [&mut i32; N], index: usize, value: i32) {
            if let Some(slot) = slots.into_iter().nth(index) {
                *slot = value;
            }
        }
        let Some(parameter) = self.parameters().get(index) else {
            return;
        };
        let v = value.clamp(parameter.least, parameter.most);
        match self {
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => put([brightness, contrast], index, v),
            Self::Posterise { levels: slot }
            | Self::Threshold { level: slot }
            | Self::GaussianBlur { radius: slot }
            | Self::Median { radius: slot }
            | Self::Pixelate { cell: slot } => put([slot], index, v),
            Self::AddNoise(noise) => put([&mut noise.amount], index, v),
            Self::UnsharpMask {
                amount,
                radius,
                threshold,
            } => put([amount, radius, threshold], index, v),
            Self::Emboss {
                angle,
                height,
                amount,
            } => put([angle, height, amount], index, v),
            Self::Levels(_)
            | Self::Curves(_)
            | Self::HueSaturation(_)
            | Self::ColourBalance(_)
            | Self::WhiteBalance(_)
            | Self::Desaturate
            | Self::Invert
            | Self::ChannelMixer(_)
            | Self::FindEdges => {}
        }
    }

    /// What it is worked through, made once for a run: an adjustment's map, or
    /// an effect's settings and what is made of them.
    #[must_use]
    pub fn work(&self) -> Work {
        let one = |table: Table| Work::Tone(ToneMap::Tables([table; 3]));
        match *self {
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => one(brightness_table(brightness, contrast)),
            Self::Levels(levels) => Work::Tone(ToneMap::Tables(levels.tables())),
            Self::Curves(curves) => Work::Tone(ToneMap::Tables(curves.tables())),
            Self::HueSaturation(ranges) => Work::Tone(ToneMap::Hue(ranges)),
            Self::ColourBalance(balance) => Work::Tone(ToneMap::balance(balance)),
            Self::WhiteBalance(balance) => Work::Tone(ToneMap::Tables(balance.tables())),
            Self::Desaturate => Work::Tone(ToneMap::Grey),
            Self::Invert => one(INVERT_TABLE),
            Self::Posterise { levels } => one(posterise_table(levels)),
            Self::Threshold { level } => Work::Tone(ToneMap::Threshold(level)),
            Self::ChannelMixer(mixer) => Work::Tone(ToneMap::Mixer(mixer)),
            Self::GaussianBlur { radius } => Work::Effect(Effect::gaussian(radius)),
            Self::UnsharpMask {
                amount,
                radius,
                threshold,
            } => Work::Effect(Effect::unsharp(amount, radius, threshold)),
            Self::Median { radius } => Work::Effect(Effect::median(radius)),
            Self::AddNoise(noise) => Work::Effect(Effect::Noise(noise)),
            Self::FindEdges => Work::Effect(Effect::Edges),
            Self::Emboss {
                angle,
                height,
                amount,
            } => Work::Effect(Effect::emboss(angle, height, amount)),
            Self::Pixelate { cell } => Work::Effect(Effect::pixelate(cell)),
        }
    }

    /// `palette` mapped as this adjustment maps colours; `None` for an effect,
    /// which a palette picture cannot take, or where the room cannot be had.
    #[must_use]
    pub fn mapped_palette(&self, palette: &[Rgba8]) -> Option<Vec<Rgba8>> {
        let Work::Tone(map) = self.work() else {
            return None;
        };
        fallible::collected(palette.len(), palette.iter().map(|&colour| map.map(colour)))
    }
}

/// What an operation is worked through, made once for a run and lent to every
/// block of it.
#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "made once for a run and lent to every block; boxing the tables would allocate for nothing"
)]
pub enum Work {
    /// Each colour mapped on its own.
    Tone(ToneMap),
    /// Each pixel made from its neighbourhood.
    Effect(Effect),
}

/// Why an operation's run stopped short of its answer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Halted {
    /// What it was working out is no longer wanted: a setting or the picture
    /// moved.
    Abandoned,
    /// There was not the memory for it.
    OutOfMemory,
    /// It runs over a colour picture's pixels, which a palette picture, one
    /// entry a pixel, does not hold.
    NeedsColour,
}

impl From<OutOfMemory> for Halted {
    fn from(_: OutOfMemory) -> Self {
        Self::OutOfMemory
    }
}

impl fmt::Display for Halted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Abandoned => "it was asked again before it was done",
            Self::OutOfMemory => "there is not enough memory",
            Self::NeedsColour => "it needs a colour picture",
        })
    }
}

#[cfg(test)]
#[path = "op_tests.rs"]
mod tests;
