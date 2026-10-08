//! What a folder holds, and the picture of a folder drawn from it: the
//! folder's back, a card per kind sampled from its entries standing in its
//! mouth, and its front over them (`plans/FILES-INTERACTION.md` FI11).

use tairix_raster::{Color, Surface};

use crate::artwork::glyph_mask;
use crate::glyph::IconKind;

/// What a folder holds, as its picture draws it: up to three kinds, one for
/// each of the most frequent families of file in it, the most frequent first.
/// A folder holding only folders and files of no family samples nothing.
#[derive(Copy, Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct FolderSample {
    kinds: [Option<IconKind>; Self::MOST],
}

impl FolderSample {
    /// The most kinds a sample holds.
    pub const MOST: usize = 3;

    /// A sample of `kinds`, the most frequent family's first; those past
    /// [`MOST`](Self::MOST) are dropped.
    #[must_use]
    pub fn new(kinds: impl IntoIterator<Item = IconKind>) -> Self {
        let mut sample = Self::default();
        for (slot, kind) in sample.kinds.iter_mut().zip(kinds) {
            *slot = Some(kind);
        }
        sample
    }

    /// The sampled kinds, the most frequent family's first.
    #[must_use]
    pub fn kinds(self) -> impl DoubleEndedIterator<Item = IconKind> {
        self.kinds.into_iter().flatten()
    }

    /// How many kinds the sample holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.iter().flatten().count()
    }

    /// Whether the sample holds no kind.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.kinds[0].is_none()
    }
}

/// The side, in pixels, below which a folder's cards would be too small to
/// read, so its picture is the plain filled folder instead.
pub(crate) const MIN_COMPOSITE_SIDE: u32 = 32;

/// The design grid the folder's back and front are authored on.
const DESIGN: u32 = 256;

/// Where each card stands for a sample of one, two and three, on the
/// [`DESIGN`] grid as `(left, top, side)`, drawn first to last: the last
/// drawn, at the front, is the sample's most frequent family. Every card's
/// foot is below the front pocket's top, so the cards stand in the folder.
const SLOTS: [&[(u32, u32, u32)]; FolderSample::MOST] = [
    &[(78, 30, 100)],
    &[(114, 38, 92), (50, 46, 92)],
    &[(132, 50, 84), (40, 50, 84), (86, 34, 84)],
];

/// Where `sample`'s cards stand in a picture `side` pixels square, as
/// `(left, top, side)` in pixels, each with its kind, drawn first to last.
pub(crate) fn card_slots(
    sample: FolderSample,
    side: u32,
) -> impl Iterator<Item = ((i32, i32, u32), IconKind)> {
    let scale = move |design: u32| u64::from(design) * u64::from(side) / u64::from(DESIGN);
    let slots = sample
        .len()
        .checked_sub(1)
        .and_then(|index| SLOTS.get(index))
        .copied()
        .unwrap_or(&[]);
    slots
        .iter()
        .zip(sample.kinds().rev())
        .map(move |(&(x, y, card), kind)| {
            let pixels = |design| i32::try_from(scale(design)).unwrap_or(i32::MAX);
            let card = u32::try_from(scale(card)).unwrap_or(0);
            ((pixels(x), pixels(y), card), kind)
        })
}

/// The paper a card without artwork of its own is drawn on, its edge, and the
/// ink its glyph is drawn in: fixed, so a folder's picture is the same in
/// every theme.
const PAPER: Color = Color::rgb(0xFF, 0xFF, 0xFF);
const PAPER_EDGE: Color = Color::rgb(0xCF, 0xE2, 0xFF);
pub(crate) const INK: Color = Color::rgb(0x33, 0x47, 0x5B);

/// `kind`'s glyph on a sheet of paper, `side` pixels square.
pub(crate) fn paper_card(kind: IconKind, side: u32) -> Option<Surface> {
    let mut card = Surface::new(side, side)?;
    let inset = side / 8;
    let width = side.checked_sub(2 * inset)?;
    let edge = (side / 48).max(1);
    let radius = side / 12;
    card.fill_round_rect(inset, 0, width, side, radius, PAPER_EDGE);
    card.fill_round_rect(
        inset + edge,
        edge,
        width.checked_sub(2 * edge)?,
        side.checked_sub(2 * edge)?,
        radius.saturating_sub(edge),
        PAPER,
    );
    let glyph_side = side * 5 / 8;
    let glyph = glyph_mask(kind, glyph_side)?;
    let at = i32::try_from((side - glyph_side) / 2).unwrap_or(0);
    card.blit_tinted(at, at, &glyph, INK);
    Some(card)
}

#[cfg(test)]
#[path = "folder_tests.rs"]
mod tests;
