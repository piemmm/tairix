//! A picture kept at every halving of its size, so it is drawn smaller without
//! the shimmer one bilinear sample of the whole picture makes: past half a
//! picture pixel to a destination pixel, a sample skips pixels it should
//! weigh.
//!
//! Each level is the one above it reduced to half its size, rounded up, by the
//! shared area-average filter ([`Surface::resampled`]), premultiplied; the
//! smallest is one pixel. Together the levels below the picture take a third
//! of its own memory again.

use core::fmt;

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::resample::Region;
use crate::surface::Surface;

/// A picture and its halvings, largest first.
pub struct Pyramid {
    base: Surface,
    /// Each halving in turn, the last a pixel.
    below: Vec<Surface>,
}

impl Pyramid {
    /// `picture` with every halving beneath it, or `None` where memory would
    /// not hold them.
    #[must_use]
    pub fn new(picture: Surface) -> Option<Self> {
        let mut below: Vec<Surface> = Vec::new();
        let (mut width, mut height) = (picture.width(), picture.height());
        while width > 1 || height > 1 {
            let above = below.last().unwrap_or(&picture);
            let whole = Region {
                x: 0,
                y: 0,
                width,
                height,
            };
            (width, height) = (width.div_ceil(2), height.div_ceil(2));
            let level = above.resampled(whole, width, height).ok()?;
            fallible::push(&mut below, level).then_some(())?;
        }
        Some(Self {
            base: picture,
            below,
        })
    }

    /// The picture itself.
    #[must_use]
    pub const fn base(&self) -> &Surface {
        &self.base
    }

    /// How many levels there are, the picture's own the first.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.below.len() + 1
    }

    /// The level to draw the picture from at `scale` destination pixels to a
    /// picture pixel: the smallest whose pixels are at least half a
    /// destination pixel, so bilinear samples a destination pixel apart weigh
    /// every pixel of it.
    #[must_use]
    pub fn level_for(&self, scale: f64) -> &Surface {
        let mut level = &self.base;
        let mut reach = scale;
        for halved in &self.below {
            if reach >= 0.5 {
                break;
            }
            reach *= 2.0;
            level = halved;
        }
        level
    }
}

impl fmt::Debug for Pyramid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pyramid")
            .field("size", &(self.base.width(), self.base.height()))
            .field("depth", &self.depth())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "pyramid_tests.rs"]
mod tests;
