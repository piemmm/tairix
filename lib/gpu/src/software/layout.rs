//! Where an image's texels lie in its memory on the software device: mip
//! level by mip level, each layer a box of rows, each texel its samples in
//! turn.

use tairix_inline::ArrayVec;

use crate::image::{Extent3d, ImageDesc};
use crate::Error;

/// What every layer of every level starts on, so a texel of up to sixteen
/// bytes never straddles a word it need not.
const LAYER_ALIGNMENT: u64 = 16;

/// What every row starts on: whole words, so work on separate rows or on
/// separate runs of whole words of one never shares a word.
const ROW_ALIGNMENT: u64 = 4;

/// The most mip levels an image has: the software device's widest size halves
/// to one in this many.
const MAX_LEVELS: usize = super::MAX_IMAGE_2D.ilog2() as usize + 1;
const _: () = assert!(super::MAX_IMAGE_3D <= super::MAX_IMAGE_2D);

/// One mip level: where its first layer starts, the bytes from one layer,
/// slice and row to the next, and its size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Level {
    pub(crate) base: u64,
    pub(crate) layer: u64,
    pub(crate) slice: u64,
    pub(crate) row: u64,
    pub(crate) extent: Extent3d,
}

/// An image's texel layout: what a texel of each sample takes, each level's
/// placing, and the bytes the whole image spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImageLayout {
    pub(crate) texel: u64,
    pub(crate) samples: u64,
    pub(crate) levels: ArrayVec<Level, MAX_LEVELS>,
    pub(crate) size: u64,
}

impl ImageLayout {
    /// The layout of an image of `desc`, which its check has admitted.
    pub(crate) fn of(desc: &ImageDesc) -> Result<Self, Error> {
        let texel = u64::from(desc.format.texel_bytes());
        let samples = u64::from(desc.samples);
        let layers = u64::from(desc.array_layers);
        let mut levels = ArrayVec::new();
        let mut base = 0u64;
        for level in 0..desc.mip_levels {
            let extent = desc.level_extent(level);
            let row = u64::from(extent.width)
                .checked_mul(texel * samples)
                .and_then(|bytes| bytes.checked_next_multiple_of(ROW_ALIGNMENT))
                .ok_or(Error::TooLarge)?;
            let slice = row
                .checked_mul(u64::from(extent.height))
                .ok_or(Error::TooLarge)?;
            let layer = slice
                .checked_mul(u64::from(extent.depth))
                .and_then(|bytes| bytes.checked_next_multiple_of(LAYER_ALIGNMENT))
                .ok_or(Error::TooLarge)?;
            levels
                .try_push(Level {
                    base,
                    layer,
                    slice,
                    row,
                    extent,
                })
                .map_err(|_| Error::TooLarge)?;
            base = layer
                .checked_mul(layers)
                .and_then(|bytes| base.checked_add(bytes))
                .ok_or(Error::TooLarge)?;
        }
        Ok(Self {
            texel,
            samples,
            levels,
            size: base,
        })
    }

    /// What an image's offset into its allocation is a multiple of.
    pub(crate) const fn alignment() -> u64 {
        LAYER_ALIGNMENT
    }

    /// The byte at which sample `sample` of texel `(x, y, z)` of layer
    /// `layer` of level `level` starts, from the image's start; the caller
    /// keeps each within what the image holds.
    pub(crate) fn texel_offset(&self, level: u32, layer: u32, at: [u32; 3], sample: u32) -> u64 {
        let Some(placed) = self.levels.get(level as usize) else {
            return self.size;
        };
        placed.base
            + u64::from(layer) * placed.layer
            + u64::from(at[2]) * placed.slice
            + u64::from(at[1]) * placed.row
            + (u64::from(at[0]) * self.samples + u64::from(sample)) * self.texel
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
