//! How an image is sampled: filtering, addressing, level of detail and
//! comparison.

use crate::info::{DeviceInfo, Features};
use crate::Error;

/// How texels are combined.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Filter {
    /// The nearest texel.
    Nearest,
    /// The texels about the point, weighted by distance.
    Linear,
}

/// What a coordinate outside the image addresses.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AddressMode {
    /// The image again, tiled.
    Repeat,
    /// The image again, mirrored at each edge.
    MirroredRepeat,
    /// The texel at the nearest edge.
    ClampToEdge,
    /// The image mirrored once about its origin, then its nearest edge.
    MirrorClampToEdge,
    /// The border colour.
    ClampToBorder,
}

/// What a coordinate past a clamp-to-border edge reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BorderColor {
    /// Every channel zero.
    TransparentBlack,
    /// Colour zero, alpha one.
    OpaqueBlack,
    /// Every channel one.
    OpaqueWhite,
}

pub use tairix_raster::CompareOp;

/// The furthest a level-of-detail bias moves the level sampled.
pub const MAX_LOD_BIAS: f32 = 16.0;

/// How a sampler is made.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SamplerDesc {
    /// The filter where the image is magnified.
    pub mag_filter: Filter,
    /// The filter where the image is minified.
    pub min_filter: Filter,
    /// How mip levels are combined.
    pub mipmap_filter: Filter,
    /// What a coordinate outside the image addresses, along each axis.
    pub address: [AddressMode; 3],
    /// Added to the level of detail computed or given.
    pub lod_bias: f32,
    /// The least level of detail sampled.
    pub lod_min: f32,
    /// The greatest level of detail sampled.
    pub lod_max: f32,
    /// The most texels along the longer axis of a sample's footprint
    /// combined: one for none.
    pub max_anisotropy: u32,
    /// The comparison a depth-reference sample makes; `None` for a sampler
    /// that reads texels.
    pub compare: Option<CompareOp>,
    /// What a clamp-to-border edge reads.
    pub border: BorderColor,
    /// Whether coordinates count texels rather than spanning the image as
    /// `0.0..=1.0`: then the filters agree, mip filtering is nearest, the
    /// level of detail is held at zero, addressing clamps, and there is no
    /// anisotropy or comparison.
    pub unnormalized: bool,
}

impl SamplerDesc {
    /// Linear filtering everywhere, clamped to the edge, every level, no
    /// anisotropy and no comparison.
    pub const LINEAR: Self = Self {
        mag_filter: Filter::Linear,
        min_filter: Filter::Linear,
        mipmap_filter: Filter::Linear,
        address: [AddressMode::ClampToEdge; 3],
        lod_bias: 0.0,
        lod_min: 0.0,
        lod_max: 1000.0,
        max_anisotropy: 1,
        compare: None,
        border: BorderColor::TransparentBlack,
        unnormalized: false,
    };

    /// Refuse a sampler `device` does not make: a level-of-detail clamp
    /// reversed, negative or not finite, a bias past [`MAX_LOD_BIAS`],
    /// anisotropy of none, past the device's or without linear filtering
    /// throughout, a feature the device does not state, or unnormalised
    /// coordinates outside their restrictions.
    pub(crate) fn check(&self, device: &DeviceInfo) -> Result<(), Error> {
        let limits = &device.limits;
        let finite =
            self.lod_min.is_finite() && self.lod_max.is_finite() && self.lod_bias.is_finite();
        if !finite || self.lod_min < 0.0 || self.lod_min > self.lod_max {
            return Err(Error::OutOfRange);
        }
        if self.lod_bias > MAX_LOD_BIAS || self.lod_bias < -MAX_LOD_BIAS {
            return Err(Error::OutOfRange);
        }
        if self.max_anisotropy == 0 || self.max_anisotropy > limits.max_sampler_anisotropy {
            return Err(Error::OutOfRange);
        }
        let linear = [self.mag_filter, self.min_filter, self.mipmap_filter]
            .iter()
            .all(|&f| f == Filter::Linear);
        if self.max_anisotropy > 1 && !linear {
            return Err(Error::Unsupported);
        }
        let needs = |feature: Features| device.features.contains(feature);
        if self.max_anisotropy > 1 && !needs(Features::SAMPLER_ANISOTROPY) {
            return Err(Error::Unsupported);
        }
        if self.address.contains(&AddressMode::MirrorClampToEdge)
            && !needs(Features::MIRROR_CLAMP_TO_EDGE)
        {
            return Err(Error::Unsupported);
        }
        if self.unnormalized {
            let clamped = self.address[..2]
                .iter()
                .all(|&a| matches!(a, AddressMode::ClampToEdge | AddressMode::ClampToBorder));
            let restricted = self.mag_filter == self.min_filter
                && self.mipmap_filter == Filter::Nearest
                && self.lod_min == 0.0
                && self.lod_max == 0.0
                && self.max_anisotropy == 1
                && self.compare.is_none()
                && clamped;
            if !restricted {
                return Err(Error::Unsupported);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "sampler_tests.rs"]
mod tests;
