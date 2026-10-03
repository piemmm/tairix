//! What a tool puts down, and finding the palette entry nearest a colour.

use alloc::vec::Vec;

use tairix_image::Rgba8;

use crate::canvas::{Kind, Sample};

/// Opaque white: what a picture that cannot be cleared shows where it is
/// erased to nothing.
pub const WHITE: Rgba8 = [255, 255, 255, 255];

/// Opaque black.
pub const BLACK: Rgba8 = [0, 0, 0, 255];

/// What a tool puts down.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ink {
    /// A palette entry, opaque.
    Index(u8),
    /// A colour, which may be translucent.
    Colour(Rgba8),
    /// Nothing: the pixel made wholly transparent.
    Clear,
}

impl Ink {
    /// The colour this ink shows as on a canvas of `kind`.
    #[must_use]
    pub fn shown(self, kind: &Kind) -> Rgba8 {
        match self {
            Self::Index(index) => kind
                .palette()
                .and_then(|palette| palette.get(usize::from(index)).copied())
                .unwrap_or(BLACK),
            Self::Colour(colour) => colour,
            Self::Clear => [0; 4],
        }
    }

    /// This ink, chosen on a canvas of kind `from`, as the nearest a canvas
    /// of kind `to` can put down.
    ///
    /// A picture that cannot be cleared takes the entry nearest white where
    /// the ink was nothing, which is what an erased paper shows.
    #[must_use]
    pub fn adapted(self, from: &Kind, to: &Kind) -> Self {
        let colour = self.shown(from);
        match to {
            Kind::Rgba => match self {
                Self::Clear => Self::Clear,
                _ => Self::Colour(colour),
            },
            Kind::Indexed {
                palette, masked, ..
            } => match self {
                Self::Clear if *masked => Self::Clear,
                Self::Clear => Self::Index(nearest(palette, WHITE)),
                // An index names an entry, which a palette edit or a mask
                // added keeps; only a change of depth renumbers them.
                Self::Index(index)
                    if from.depth() == to.depth() && usize::from(index) < palette.len() =>
                {
                    Self::Index(index)
                }
                _ if *masked && colour[3] < 128 => Self::Clear,
                _ => Self::Index(nearest(palette, colour)),
            },
        }
    }

    /// The ink a picked pixel stands for on a canvas of `kind`.
    #[must_use]
    pub const fn of_sample(sample: Sample, kind: &Kind) -> Self {
        match sample {
            Sample::Index(_, 0) if kind.masked() => Self::Clear,
            Sample::Index(index, _) => Self::Index(index),
            Sample::Rgba(colour) => Self::of_colour(colour),
        }
    }

    /// The ink laying `colour` on a colour picture: clear where it has no
    /// alpha, since a colour laid over with none leaves the pixel as it was.
    #[must_use]
    pub const fn of_colour(colour: Rgba8) -> Self {
        if colour[3] == 0 {
            Self::Clear
        } else {
            Self::Colour(colour)
        }
    }
}

/// The squared distance between two colours, alpha weighted as a channel.
fn distance(a: Rgba8, b: Rgba8) -> u32 {
    a.iter()
        .zip(b)
        .map(|(&x, y)| {
            let d = u32::from(x.abs_diff(y));
            d * d
        })
        .sum()
}

/// The index of the entry of `palette` nearest `colour`, the lowest on a
/// tie; `0` for an empty palette.
#[must_use]
pub fn nearest(palette: &[Rgba8], colour: Rgba8) -> u8 {
    let mut best = (u32::MAX, 0usize);
    for (index, entry) in palette.iter().enumerate() {
        let d = distance(*entry, colour);
        if d < best.0 {
            best = (d, index);
        }
    }
    u8::try_from(best.1).unwrap_or(0)
}

/// A palette arranged for many nearest-entry searches: sorted by green, the
/// channel the eye weighs most, so a search walks outward from the colour's
/// own green and stops once green alone is further than the best found.
///
/// It answers exactly as [`nearest`] does, lowest index on a tie.
#[derive(Clone, Debug)]
pub struct Nearest {
    /// `(green, index)` for every entry, ordered by green then index.
    order: Vec<(u8, u8)>,
    palette: Vec<Rgba8>,
    /// The last colour asked about and its answer: runs of one colour are
    /// what pictures are made of.
    last: Option<(Rgba8, u8)>,
}

impl Nearest {
    /// A search over `palette`, which must hold at most 256 entries; `None`
    /// when the allocator refuses the room.
    #[must_use]
    pub fn new(palette: &[Rgba8]) -> Option<Self> {
        let count = palette.len().min(256);
        let mut order = tairix_util::fallible::collected(
            count,
            palette
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry[1], u8::try_from(index).unwrap_or(u8::MAX))),
        )?;
        order.sort_unstable();
        let palette = tairix_util::fallible::collected(count, palette.iter().copied())?;
        Some(Self {
            order,
            palette,
            last: None,
        })
    }

    /// The index nearest `colour`.
    pub fn find(&mut self, colour: Rgba8) -> u8 {
        if let Some((seen, index)) = self.last {
            if seen == colour {
                return index;
            }
        }
        let start = self.order.partition_point(|&(green, _)| green < colour[1]);
        let mut best = (u32::MAX, u8::MAX);
        let consider = |index: u8, best: &mut (u32, u8)| {
            let d = distance(self.palette[usize::from(index)], colour);
            if d < best.0 || (d == best.0 && index < best.1) {
                *best = (d, index);
            }
        };
        let (mut up, mut down) = (start, start);
        loop {
            let above = self.order.get(up).copied();
            let below = down
                .checked_sub(1)
                .and_then(|at| self.order.get(at).copied());
            let gap = |green: u8| {
                let d = u32::from(green.abs_diff(colour[1]));
                d * d
            };
            let mut moved = false;
            if let Some((green, index)) = above {
                if gap(green) <= best.0 {
                    consider(index, &mut best);
                    up += 1;
                    moved = true;
                }
            }
            if let Some((green, index)) = below {
                if gap(green) <= best.0 {
                    consider(index, &mut best);
                    down -= 1;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        let index = if self.palette.is_empty() { 0 } else { best.1 };
        self.last = Some((colour, index));
        index
    }
}

#[cfg(test)]
#[path = "colour_tests.rs"]
mod tests;
