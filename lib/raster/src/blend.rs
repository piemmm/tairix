//! The blend stage: how a fragment's colour is combined with the colour held,
//! as a draw states it for each attachment, and the logic operations a draw
//! may apply instead.
//!
//! This is the 3D pipeline's blend, the equations a device evaluates: single
//! precision over four channels, red to alpha. A normalised attachment holds
//! every value and factor to its range before the equation; a float one holds
//! none. The desktop's own composite is [`Pixel::over`](crate::Pixel::over),
//! its premultiplied eight-bit arithmetic and dither, which no draw changes.

/// What a term of a blend equation is scaled by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BlendFactor {
    /// Nought.
    Zero,
    /// One.
    One,
    /// The source's channel.
    SourceColor,
    /// One less the source's channel.
    OneMinusSourceColor,
    /// The held colour's channel.
    DestinationColor,
    /// One less the held colour's channel.
    OneMinusDestinationColor,
    /// The source's alpha.
    SourceAlpha,
    /// One less the source's alpha.
    OneMinusSourceAlpha,
    /// The held colour's alpha.
    DestinationAlpha,
    /// One less the held colour's alpha.
    OneMinusDestinationAlpha,
    /// The blend constant's channel.
    ConstantColor,
    /// One less the blend constant's channel.
    OneMinusConstantColor,
    /// The blend constant's alpha.
    ConstantAlpha,
    /// One less the blend constant's alpha.
    OneMinusConstantAlpha,
    /// The lesser of the source's alpha and one less the held alpha, for a
    /// colour channel; one for alpha.
    SourceAlphaSaturate,
    /// The second source's channel.
    Source1Color,
    /// One less the second source's channel.
    OneMinusSource1Color,
    /// The second source's alpha.
    Source1Alpha,
    /// One less the second source's alpha.
    OneMinusSource1Alpha,
}

impl BlendFactor {
    /// Whether it reads the second source a dual-source blend writes.
    #[must_use]
    pub const fn reads_second_source(self) -> bool {
        matches!(
            self,
            Self::Source1Color
                | Self::OneMinusSource1Color
                | Self::Source1Alpha
                | Self::OneMinusSource1Alpha
        )
    }
}

/// How a blend equation's two scaled terms combine.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BlendOp {
    /// Source plus destination.
    Add,
    /// Source less destination.
    Subtract,
    /// Destination less source.
    ReverseSubtract,
    /// The lesser of the two, unscaled.
    Min,
    /// The greater of the two, unscaled.
    Max,
}

/// The range an attachment holds a blend's values and factors to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BlendRange {
    /// Unsigned normalised: `0..=1`.
    Unsigned,
    /// Signed normalised: `-1..=1`.
    Signed,
    /// Float: unheld.
    Unbounded,
}

impl BlendRange {
    fn hold(self, value: f32) -> f32 {
        let (low, high) = match self {
            Self::Unsigned => (0.0, 1.0),
            Self::Signed => (-1.0, 1.0),
            Self::Unbounded => return value,
        };
        if value < low {
            low
        } else if value > high {
            high
        } else {
            value
        }
    }
}

/// A blend's two equations: for the colour channels and for alpha.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Blend {
    /// What the source's colour is scaled by.
    pub source_color: BlendFactor,
    /// What the held colour is scaled by.
    pub destination_color: BlendFactor,
    /// How the colour terms combine.
    pub color: BlendOp,
    /// What the source's alpha is scaled by.
    pub source_alpha: BlendFactor,
    /// What the held alpha is scaled by.
    pub destination_alpha: BlendFactor,
    /// How the alpha terms combine.
    pub alpha: BlendOp,
}

/// What a blend reads, each four channels red to alpha.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BlendInputs {
    /// The fragment's colour.
    pub source: [f32; 4],
    /// The second colour a dual-source blend writes.
    pub source1: [f32; 4],
    /// The colour held.
    pub destination: [f32; 4],
    /// The draw's blend constant.
    pub constant: [f32; 4],
}

impl Blend {
    /// Premultiplied *over*: the source plus what of the held colour its
    /// alpha leaves.
    pub const OVER: Self = Self {
        source_color: BlendFactor::One,
        destination_color: BlendFactor::OneMinusSourceAlpha,
        color: BlendOp::Add,
        source_alpha: BlendFactor::One,
        destination_alpha: BlendFactor::OneMinusSourceAlpha,
        alpha: BlendOp::Add,
    };

    /// Whether either equation reads the second source.
    #[must_use]
    pub const fn reads_second_source(&self) -> bool {
        self.source_color.reads_second_source()
            || self.destination_color.reads_second_source()
            || self.source_alpha.reads_second_source()
            || self.destination_alpha.reads_second_source()
    }

    /// The colour a fragment leaves over the attachment: both equations over
    /// `inputs`, every value and factor held to `range` first.
    #[must_use]
    pub fn apply(&self, range: BlendRange, inputs: BlendInputs) -> [f32; 4] {
        let held = BlendInputs {
            source: inputs.source.map(|value| range.hold(value)),
            source1: inputs.source1.map(|value| range.hold(value)),
            destination: inputs.destination.map(|value| range.hold(value)),
            constant: inputs.constant.map(|value| range.hold(value)),
        };
        core::array::from_fn(|channel| {
            let (source, destination, op) = if channel == 3 {
                (self.source_alpha, self.destination_alpha, self.alpha)
            } else {
                (self.source_color, self.destination_color, self.color)
            };
            let s = held.source[channel];
            let d = held.destination[channel];
            let scaled = |factor| range.hold(factor_value(factor, channel, &held));
            match op {
                BlendOp::Add => s * scaled(source) + d * scaled(destination),
                BlendOp::Subtract => s * scaled(source) - d * scaled(destination),
                BlendOp::ReverseSubtract => d * scaled(destination) - s * scaled(source),
                BlendOp::Min => {
                    if d < s {
                        d
                    } else {
                        s
                    }
                }
                BlendOp::Max => {
                    if d > s {
                        d
                    } else {
                        s
                    }
                }
            }
        })
    }
}

/// `factor`'s value for `channel` (alpha is `3`) over `inputs`.
fn factor_value(factor: BlendFactor, channel: usize, inputs: &BlendInputs) -> f32 {
    let (s, s1, d, c) = (
        inputs.source,
        inputs.source1,
        inputs.destination,
        inputs.constant,
    );
    match factor {
        BlendFactor::Zero => 0.0,
        BlendFactor::One => 1.0,
        BlendFactor::SourceColor => s[channel],
        BlendFactor::OneMinusSourceColor => 1.0 - s[channel],
        BlendFactor::DestinationColor => d[channel],
        BlendFactor::OneMinusDestinationColor => 1.0 - d[channel],
        BlendFactor::SourceAlpha => s[3],
        BlendFactor::OneMinusSourceAlpha => 1.0 - s[3],
        BlendFactor::DestinationAlpha => d[3],
        BlendFactor::OneMinusDestinationAlpha => 1.0 - d[3],
        BlendFactor::ConstantColor => c[channel],
        BlendFactor::OneMinusConstantColor => 1.0 - c[channel],
        BlendFactor::ConstantAlpha => c[3],
        BlendFactor::OneMinusConstantAlpha => 1.0 - c[3],
        BlendFactor::SourceAlphaSaturate => {
            if channel == 3 {
                1.0
            } else {
                let left = 1.0 - d[3];
                if s[3] < left {
                    s[3]
                } else {
                    left
                }
            }
        }
        BlendFactor::Source1Color => s1[channel],
        BlendFactor::OneMinusSource1Color => 1.0 - s1[channel],
        BlendFactor::Source1Alpha => s1[3],
        BlendFactor::OneMinusSource1Alpha => 1.0 - s1[3],
    }
}

/// A logic operation over an attachment's stored bits, in place of a blend.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LogicOp {
    /// Nought.
    Clear,
    /// Source and destination.
    And,
    /// Source and not destination.
    AndReverse,
    /// Source.
    Copy,
    /// Not source and destination.
    AndInverted,
    /// Destination.
    NoOp,
    /// Source exclusive-or destination.
    Xor,
    /// Source or destination.
    Or,
    /// Not either.
    Nor,
    /// Not their exclusive-or.
    Equivalent,
    /// Not destination.
    Invert,
    /// Source or not destination.
    OrReverse,
    /// Not source.
    CopyInverted,
    /// Not source or destination.
    OrInverted,
    /// Not both.
    Nand,
    /// Every bit set.
    Set,
}

impl LogicOp {
    /// The bits left where `source` meets `destination`.
    #[must_use]
    pub const fn apply(self, source: u32, destination: u32) -> u32 {
        let (s, d) = (source, destination);
        match self {
            Self::Clear => 0,
            Self::And => s & d,
            Self::AndReverse => s & !d,
            Self::Copy => s,
            Self::AndInverted => !s & d,
            Self::NoOp => d,
            Self::Xor => s ^ d,
            Self::Or => s | d,
            Self::Nor => !(s | d),
            Self::Equivalent => !(s ^ d),
            Self::Invert => !d,
            Self::OrReverse => s | !d,
            Self::CopyInverted => !s,
            Self::OrInverted => !s | d,
            Self::Nand => !(s & d),
            Self::Set => u32::MAX,
        }
    }
}

#[cfg(test)]
#[path = "blend_tests.rs"]
mod tests;
