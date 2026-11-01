//! Texel formats: what a texel holds, and what a format may be used for.

use tairix_spirv::enumerant::ImageFormat;
use tairix_spirv::Component;

tairix_util::flags! {
    /// What a format may be used for, as a device states it
    /// ([`DeviceInfo::formats`](crate::DeviceInfo::formats)).
    pub struct FormatFeatures(u32) {
        /// Sampled, through a sampled image or a uniform texel buffer.
        const SAMPLED = 1;
        /// Sampled with linear filtering.
        const FILTERED = 1 << 1;
        /// Read and written by texel, through a storage image or a storage
        /// texel buffer.
        const STORAGE = 1 << 2;
        /// Rendered to as a colour attachment.
        const COLOR_ATTACHMENT = 1 << 3;
        /// Rendered to as a depth or stencil attachment.
        const DEPTH_STENCIL = 1 << 4;
        /// Made with four samples.
        const MULTISAMPLED = 1 << 5;
        /// Read through a uniform texel buffer.
        const UNIFORM_TEXEL_BUFFER = 1 << 6;
        /// Read and written through a storage texel buffer.
        const STORAGE_TEXEL_BUFFER = 1 << 7;
    }
}

tairix_util::flags! {
    /// The planes a format's texels hold.
    pub struct Aspects(u32) {
        /// Colour.
        const COLOR = 1;
        /// Depth.
        const DEPTH = 1 << 1;
        /// Stencil.
        const STENCIL = 1 << 2;
    }
}

impl Aspects {
    /// Depth and stencil.
    pub const DEPTH_STENCIL: Self = Self::DEPTH.with(Self::STENCIL);
}

/// How a format's texels are encoded, as the software backend reads them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Encoding {
    Unorm8,
    Snorm8,
    Uint8,
    Sint8,
    Srgb8,
    Unorm16,
    Snorm16,
    Uint16,
    Sint16,
    Float16,
    Uint32,
    Sint32,
    Float32,
    Rgb10A2Unorm,
    Rgb10A2Uint,
    Rg11B10Float,
    Rgb9E5Float,
    Depth16,
    Depth32,
    Stencil8,
    Depth32Stencil8,
}

/// A texel format.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Format {
    /// One 8-bit unsigned normalised channel.
    R8Unorm,
    /// One 8-bit signed normalised channel.
    R8Snorm,
    /// One 8-bit unsigned integer channel.
    R8Uint,
    /// One 8-bit signed integer channel.
    R8Sint,
    /// One 16-bit unsigned normalised channel.
    R16Unorm,
    /// One 16-bit signed normalised channel.
    R16Snorm,
    /// One 16-bit unsigned integer channel.
    R16Uint,
    /// One 16-bit signed integer channel.
    R16Sint,
    /// One 16-bit float channel.
    R16Float,
    /// Two 8-bit unsigned normalised channels.
    Rg8Unorm,
    /// Two 8-bit signed normalised channels.
    Rg8Snorm,
    /// Two 8-bit unsigned integer channels.
    Rg8Uint,
    /// Two 8-bit signed integer channels.
    Rg8Sint,
    /// One 32-bit unsigned integer channel.
    R32Uint,
    /// One 32-bit signed integer channel.
    R32Sint,
    /// One 32-bit float channel.
    R32Float,
    /// Two 16-bit unsigned normalised channels.
    Rg16Unorm,
    /// Two 16-bit signed normalised channels.
    Rg16Snorm,
    /// Two 16-bit unsigned integer channels.
    Rg16Uint,
    /// Two 16-bit signed integer channels.
    Rg16Sint,
    /// Two 16-bit float channels.
    Rg16Float,
    /// Four 8-bit unsigned normalised channels.
    Rgba8Unorm,
    /// Four 8-bit sRGB-encoded channels, alpha linear.
    Rgba8UnormSrgb,
    /// Four 8-bit signed normalised channels.
    Rgba8Snorm,
    /// Four 8-bit unsigned integer channels.
    Rgba8Uint,
    /// Four 8-bit signed integer channels.
    Rgba8Sint,
    /// Four 8-bit unsigned normalised channels, blue first.
    Bgra8Unorm,
    /// Four 8-bit sRGB-encoded channels, blue first, alpha linear.
    Bgra8UnormSrgb,
    /// Three 10-bit and one 2-bit unsigned normalised channel.
    Rgb10A2Unorm,
    /// Three 10-bit and one 2-bit unsigned integer channel.
    Rgb10A2Uint,
    /// Two 11-bit and one 10-bit unsigned float channel.
    Rg11B10Float,
    /// Three 9-bit mantissas sharing a 5-bit exponent.
    Rgb9E5Float,
    /// Two 32-bit unsigned integer channels.
    Rg32Uint,
    /// Two 32-bit signed integer channels.
    Rg32Sint,
    /// Two 32-bit float channels.
    Rg32Float,
    /// Four 16-bit unsigned normalised channels.
    Rgba16Unorm,
    /// Four 16-bit signed normalised channels.
    Rgba16Snorm,
    /// Four 16-bit unsigned integer channels.
    Rgba16Uint,
    /// Four 16-bit signed integer channels.
    Rgba16Sint,
    /// Four 16-bit float channels.
    Rgba16Float,
    /// Four 32-bit unsigned integer channels.
    Rgba32Uint,
    /// Four 32-bit signed integer channels.
    Rgba32Sint,
    /// Four 32-bit float channels.
    Rgba32Float,
    /// 16-bit unsigned normalised depth.
    Depth16Unorm,
    /// 32-bit float depth.
    Depth32Float,
    /// 8-bit unsigned integer stencil.
    Stencil8,
    /// 32-bit float depth and 8-bit unsigned integer stencil.
    Depth32FloatStencil8,
    /// Depth of at least 24 bits, of the device's choosing, and 8-bit unsigned
    /// integer stencil: the depth-stencil format every device has.
    Depth24PlusStencil8,
}

// What the software device states of each format: everything the format's
// encoding can mean.

/// A colour format that samples, filters, renders and multisamples, and is
/// read through a texel buffer.
const FLOAT_COLOR: FormatFeatures = FormatFeatures::SAMPLED
    .with(FormatFeatures::FILTERED)
    .with(FormatFeatures::COLOR_ATTACHMENT)
    .with(FormatFeatures::MULTISAMPLED)
    .with(FormatFeatures::UNIFORM_TEXEL_BUFFER);

/// [`FLOAT_COLOR`], and read and written by texel.
const STORED_FLOAT: FormatFeatures = FLOAT_COLOR
    .with(FormatFeatures::STORAGE)
    .with(FormatFeatures::STORAGE_TEXEL_BUFFER);

/// A signed normalised format: filtered and stored, but no attachment.
const SNORM: FormatFeatures = FormatFeatures::SAMPLED
    .with(FormatFeatures::FILTERED)
    .with(FormatFeatures::STORAGE)
    .with(FormatFeatures::UNIFORM_TEXEL_BUFFER)
    .with(FormatFeatures::STORAGE_TEXEL_BUFFER);

/// An integer colour format: sampled unfiltered, stored, and rendered and
/// multisampled without blending or resolve.
const INTEGER: FormatFeatures = FormatFeatures::SAMPLED
    .with(FormatFeatures::STORAGE)
    .with(FormatFeatures::COLOR_ATTACHMENT)
    .with(FormatFeatures::MULTISAMPLED)
    .with(FormatFeatures::UNIFORM_TEXEL_BUFFER)
    .with(FormatFeatures::STORAGE_TEXEL_BUFFER);

/// A depth or stencil format.
const DEPTH: FormatFeatures = FormatFeatures::SAMPLED
    .with(FormatFeatures::DEPTH_STENCIL)
    .with(FormatFeatures::MULTISAMPLED);

/// One format's description.
struct Info {
    encoding: Encoding,
    channels: u8,
    /// Channels stored blue first.
    reversed: bool,
    component: Component,
    aspects: Aspects,
    features: FormatFeatures,
    storage: ImageFormat,
}

const fn colour(
    encoding: Encoding,
    channels: u8,
    component: Component,
    features: FormatFeatures,
    storage: ImageFormat,
) -> Info {
    Info {
        encoding,
        channels,
        reversed: false,
        component,
        aspects: Aspects::COLOR,
        features,
        storage,
    }
}

impl Format {
    /// Every format, in declaration order.
    pub const ALL: [Self; 48] = [
        Self::R8Unorm,
        Self::R8Snorm,
        Self::R8Uint,
        Self::R8Sint,
        Self::R16Unorm,
        Self::R16Snorm,
        Self::R16Uint,
        Self::R16Sint,
        Self::R16Float,
        Self::Rg8Unorm,
        Self::Rg8Snorm,
        Self::Rg8Uint,
        Self::Rg8Sint,
        Self::R32Uint,
        Self::R32Sint,
        Self::R32Float,
        Self::Rg16Unorm,
        Self::Rg16Snorm,
        Self::Rg16Uint,
        Self::Rg16Sint,
        Self::Rg16Float,
        Self::Rgba8Unorm,
        Self::Rgba8UnormSrgb,
        Self::Rgba8Snorm,
        Self::Rgba8Uint,
        Self::Rgba8Sint,
        Self::Bgra8Unorm,
        Self::Bgra8UnormSrgb,
        Self::Rgb10A2Unorm,
        Self::Rgb10A2Uint,
        Self::Rg11B10Float,
        Self::Rgb9E5Float,
        Self::Rg32Uint,
        Self::Rg32Sint,
        Self::Rg32Float,
        Self::Rgba16Unorm,
        Self::Rgba16Snorm,
        Self::Rgba16Uint,
        Self::Rgba16Sint,
        Self::Rgba16Float,
        Self::Rgba32Uint,
        Self::Rgba32Sint,
        Self::Rgba32Float,
        Self::Depth16Unorm,
        Self::Depth32Float,
        Self::Stencil8,
        Self::Depth32FloatStencil8,
        Self::Depth24PlusStencil8,
    ];

    const fn info(self) -> Info {
        use Component::{Float, Sint, Uint};
        use Encoding as E;
        use ImageFormat as I;
        match self {
            Self::R8Unorm => colour(E::Unorm8, 1, Float, STORED_FLOAT, I::R8),
            Self::R8Snorm => colour(E::Snorm8, 1, Float, SNORM, I::R8_SNORM),
            Self::R8Uint => colour(E::Uint8, 1, Uint, INTEGER, I::R8UI),
            Self::R8Sint => colour(E::Sint8, 1, Sint, INTEGER, I::R8I),
            Self::R16Unorm => colour(E::Unorm16, 1, Float, STORED_FLOAT, I::R16),
            Self::R16Snorm => colour(E::Snorm16, 1, Float, SNORM, I::R16_SNORM),
            Self::R16Uint => colour(E::Uint16, 1, Uint, INTEGER, I::R16UI),
            Self::R16Sint => colour(E::Sint16, 1, Sint, INTEGER, I::R16I),
            Self::R16Float => colour(E::Float16, 1, Float, STORED_FLOAT, I::R16F),
            Self::Rg8Unorm => colour(E::Unorm8, 2, Float, STORED_FLOAT, I::RG8),
            Self::Rg8Snorm => colour(E::Snorm8, 2, Float, SNORM, I::RG8_SNORM),
            Self::Rg8Uint => colour(E::Uint8, 2, Uint, INTEGER, I::RG8UI),
            Self::Rg8Sint => colour(E::Sint8, 2, Sint, INTEGER, I::RG8I),
            Self::R32Uint => colour(E::Uint32, 1, Uint, INTEGER, I::R32UI),
            Self::R32Sint => colour(E::Sint32, 1, Sint, INTEGER, I::R32I),
            Self::R32Float => colour(E::Float32, 1, Float, STORED_FLOAT, I::R32F),
            Self::Rg16Unorm => colour(E::Unorm16, 2, Float, STORED_FLOAT, I::RG16),
            Self::Rg16Snorm => colour(E::Snorm16, 2, Float, SNORM, I::RG16_SNORM),
            Self::Rg16Uint => colour(E::Uint16, 2, Uint, INTEGER, I::RG16UI),
            Self::Rg16Sint => colour(E::Sint16, 2, Sint, INTEGER, I::RG16I),
            Self::Rg16Float => colour(E::Float16, 2, Float, STORED_FLOAT, I::RG16F),
            Self::Rgba8Unorm => colour(E::Unorm8, 4, Float, STORED_FLOAT, I::RGBA8),
            Self::Rgba8UnormSrgb => colour(E::Srgb8, 4, Float, FLOAT_COLOR, I::UNKNOWN),
            Self::Rgba8Snorm => colour(E::Snorm8, 4, Float, SNORM, I::RGBA8_SNORM),
            Self::Rgba8Uint => colour(E::Uint8, 4, Uint, INTEGER, I::RGBA8UI),
            Self::Rgba8Sint => colour(E::Sint8, 4, Sint, INTEGER, I::RGBA8I),
            Self::Bgra8Unorm => Info {
                reversed: true,
                ..colour(E::Unorm8, 4, Float, FLOAT_COLOR, I::UNKNOWN)
            },
            Self::Bgra8UnormSrgb => Info {
                reversed: true,
                ..colour(E::Srgb8, 4, Float, FLOAT_COLOR, I::UNKNOWN)
            },
            Self::Rgb10A2Unorm => colour(E::Rgb10A2Unorm, 4, Float, STORED_FLOAT, I::RGB10_A2),
            Self::Rgb10A2Uint => colour(E::Rgb10A2Uint, 4, Uint, INTEGER, I::RGB10A2UI),
            Self::Rg11B10Float => {
                colour(E::Rg11B10Float, 3, Float, STORED_FLOAT, I::R11F_G11F_B10F)
            }
            Self::Rgb9E5Float => colour(
                E::Rgb9E5Float,
                3,
                Float,
                FormatFeatures::SAMPLED
                    .with(FormatFeatures::FILTERED)
                    .with(FormatFeatures::UNIFORM_TEXEL_BUFFER),
                I::UNKNOWN,
            ),
            Self::Rg32Uint => colour(E::Uint32, 2, Uint, INTEGER, I::RG32UI),
            Self::Rg32Sint => colour(E::Sint32, 2, Sint, INTEGER, I::RG32I),
            Self::Rg32Float => colour(E::Float32, 2, Float, STORED_FLOAT, I::RG32F),
            Self::Rgba16Unorm => colour(E::Unorm16, 4, Float, STORED_FLOAT, I::RGBA16),
            Self::Rgba16Snorm => colour(E::Snorm16, 4, Float, SNORM, I::RGBA16_SNORM),
            Self::Rgba16Uint => colour(E::Uint16, 4, Uint, INTEGER, I::RGBA16UI),
            Self::Rgba16Sint => colour(E::Sint16, 4, Sint, INTEGER, I::RGBA16I),
            Self::Rgba16Float => colour(E::Float16, 4, Float, STORED_FLOAT, I::RGBA16F),
            Self::Rgba32Uint => colour(E::Uint32, 4, Uint, INTEGER, I::RGBA32UI),
            Self::Rgba32Sint => colour(E::Sint32, 4, Sint, INTEGER, I::RGBA32I),
            Self::Rgba32Float => colour(E::Float32, 4, Float, STORED_FLOAT, I::RGBA32F),
            Self::Depth16Unorm => Info {
                aspects: Aspects::DEPTH,
                ..colour(E::Depth16, 1, Float, DEPTH, I::UNKNOWN)
            },
            Self::Depth32Float => Info {
                aspects: Aspects::DEPTH,
                ..colour(E::Depth32, 1, Float, DEPTH, I::UNKNOWN)
            },
            Self::Stencil8 => Info {
                aspects: Aspects::STENCIL,
                ..colour(E::Stencil8, 1, Uint, DEPTH, I::UNKNOWN)
            },
            Self::Depth32FloatStencil8 | Self::Depth24PlusStencil8 => Info {
                aspects: Aspects::DEPTH_STENCIL,
                ..colour(E::Depth32Stencil8, 1, Float, DEPTH, I::UNKNOWN)
            },
        }
    }

    /// Its position in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// What the software device states it can be used for: everything its
    /// encoding can mean.
    pub(crate) const fn software_features(self) -> FormatFeatures {
        self.info().features
    }

    /// The planes its texels hold.
    #[must_use]
    pub const fn aspects(self) -> Aspects {
        self.info().aspects
    }

    /// How many channels a texel of its colour or of one aspect holds.
    #[must_use]
    pub const fn channels(self) -> u32 {
        self.info().channels as u32
    }

    /// What its channels read as in a shader: for a depth-stencil format, its
    /// depth.
    #[must_use]
    pub const fn component(self) -> Component {
        self.info().component
    }

    /// Whether its colour channels are sRGB-encoded.
    #[must_use]
    pub const fn is_srgb(self) -> bool {
        matches!(self.info().encoding, Encoding::Srgb8)
    }

    /// The storage format a module declares for it, if one names it.
    #[must_use]
    pub fn storage_format(self) -> Option<ImageFormat> {
        let storage = self.info().storage;
        (storage != ImageFormat::UNKNOWN).then_some(storage)
    }

    /// The format an sRGB format is the encoding of, or the sRGB encoding of
    /// a format: what a view may read it as besides itself.
    #[must_use]
    pub const fn srgb_twin(self) -> Option<Self> {
        match self {
            Self::Rgba8Unorm => Some(Self::Rgba8UnormSrgb),
            Self::Rgba8UnormSrgb => Some(Self::Rgba8Unorm),
            Self::Bgra8Unorm => Some(Self::Bgra8UnormSrgb),
            Self::Bgra8UnormSrgb => Some(Self::Bgra8Unorm),
            _ => None,
        }
    }

    /// The bytes one texel of `aspect` takes in a buffer a copy reads or
    /// writes: its colour, its depth or its stencil. `None` for an aspect it
    /// does not hold, for more than one aspect at once, and for the depth of
    /// [`Self::Depth24PlusStencil8`], whose bits are the device's own.
    #[must_use]
    pub const fn copy_bytes(self, aspect: Aspects) -> Option<u32> {
        let info = self.info();
        if !info.aspects.contains(aspect) {
            return None;
        }
        let device_chosen = matches!(self, Self::Depth24PlusStencil8);
        let bytes = match (info.encoding, aspect.bits()) {
            (Encoding::Depth32Stencil8, bits) if bits == Aspects::DEPTH.bits() => {
                if device_chosen {
                    return None;
                }
                4
            }
            (Encoding::Depth32Stencil8, bits) if bits == Aspects::STENCIL.bits() => 1,
            (Encoding::Depth32Stencil8, _) => return None,
            (encoding, _) => stored_bytes(encoding, info.channels as u32),
        };
        Some(bytes)
    }

    pub(crate) const fn encoding(self) -> Encoding {
        self.info().encoding
    }

    /// Whether its channels are stored blue first.
    pub(crate) const fn reversed(self) -> bool {
        self.info().reversed
    }

    /// The bytes one texel takes in an image, every aspect included.
    pub(crate) const fn texel_bytes(self) -> u32 {
        let info = self.info();
        match info.encoding {
            Encoding::Depth32Stencil8 => 8,
            encoding => stored_bytes(encoding, info.channels as u32),
        }
    }
}

/// The bytes `channels` channels of `encoding` take.
const fn stored_bytes(encoding: Encoding, channels: u32) -> u32 {
    let per_channel = match encoding {
        Encoding::Unorm8
        | Encoding::Snorm8
        | Encoding::Uint8
        | Encoding::Sint8
        | Encoding::Srgb8
        | Encoding::Stencil8 => 1,
        Encoding::Unorm16
        | Encoding::Snorm16
        | Encoding::Uint16
        | Encoding::Sint16
        | Encoding::Float16
        | Encoding::Depth16 => 2,
        Encoding::Uint32 | Encoding::Sint32 | Encoding::Float32 | Encoding::Depth32 => 4,
        Encoding::Rgb10A2Unorm
        | Encoding::Rgb10A2Uint
        | Encoding::Rg11B10Float
        | Encoding::Rgb9E5Float => return 4,
        Encoding::Depth32Stencil8 => return 8,
    };
    per_channel * channels
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
