//! What a folder holds, and the picture of a folder drawn from it: the
//! folder's back, a fan of cards sampled from its entries rising out of its
//! mouth, and its front over them (`plans/FILES-INTERACTION.md` FI11, FI18).

use alloc::sync::Arc;
use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use core::mem::{size_of, size_of_val};

use tairix_geometry::Rect;
use tairix_raster::{Affine, Color, Region, Surface};

use crate::artwork::glyph_mask;
use crate::glyph::IconKind;
use crate::thumbnail::{Thumbnail, MAX_THUMBNAIL_BYTES};

/// One card a folder's picture fans out of its mouth.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum SampleCard {
    /// A member drawn as its kind.
    Kind(IconKind),
    /// A picture file drawn as its own content. Its kind is the card it draws
    /// while the picture is produced, and when it will not decode.
    Picture(IconKind, Thumbnail),
}

impl SampleCard {
    /// The member's kind.
    #[must_use]
    pub const fn kind(&self) -> IconKind {
        match self {
            Self::Kind(kind) | Self::Picture(kind, _) => *kind,
        }
    }

    /// The member's own picture, when the card shows one.
    #[must_use]
    pub const fn picture(&self) -> Option<&Thumbnail> {
        match self {
            Self::Kind(_) => None,
            Self::Picture(_, thumbnail) => Some(thumbnail),
        }
    }
}

/// One card as a sample draws it: the member's kind, and its own picture when
/// the card prints one.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct DrawnCard<'a> {
    kind: IconKind,
    picture: Option<&'a Thumbnail>,
}

impl<'a> DrawnCard<'a> {
    /// The member's kind.
    #[must_use]
    pub const fn kind(self) -> IconKind {
        self.kind
    }

    /// The member's own picture, when the card prints one.
    #[must_use]
    pub const fn picture(self) -> Option<&'a Thumbnail> {
        self.picture
    }
}

/// What a folder holds, as its picture draws it: up to three cards, the one
/// drawn at the front first. A folder holding only folders and files of no
/// family samples nothing.
///
/// The cards are shared, so the listing entry, every cache key naming the
/// folder, and its [`kinds_only`](Self::kinds_only) view hold one copy, and
/// equality is over the cards as drawn: two samples drawing the same cards
/// are one picture.
#[derive(Clone, Debug, Default)]
pub struct FolderSample {
    cards: Arc<[SampleCard]>,
    as_kinds: bool,
}

impl FolderSample {
    /// The most cards a sample holds.
    pub const MOST: usize = 3;

    /// The largest member a card prints as its own picture: the most a
    /// thumbnail may read, shared between the cards, so drawing a folder reads
    /// no more than drawing one picture does. A larger member is its kind.
    pub const MAX_CARD_BYTES: u64 = MAX_THUMBNAIL_BYTES / Self::MOST as u64;

    /// A sample of `cards`, the front card first; those past
    /// [`MOST`](Self::MOST) are dropped.
    #[must_use]
    pub fn new(cards: impl IntoIterator<Item = SampleCard>) -> Self {
        Self {
            cards: cards
                .into_iter()
                .take(Self::MOST)
                .map(|card| match card {
                    SampleCard::Picture(kind, thumbnail)
                        if thumbnail.stamp.size > Self::MAX_CARD_BYTES =>
                    {
                        SampleCard::Kind(kind)
                    }
                    card => card,
                })
                .collect(),
            as_kinds: false,
        }
    }

    /// The cards as drawn, the front one first.
    #[must_use]
    pub fn cards(&self) -> impl DoubleEndedIterator<Item = DrawnCard<'_>> {
        self.cards.iter().map(move |card| DrawnCard {
            kind: card.kind(),
            picture: card.picture().filter(|_| !self.as_kinds),
        })
    }

    /// How many cards the sample holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Whether the sample holds no card.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Whether a card shows a member's own picture: drawing it reads member
    /// files, which is thumbnail-class work.
    #[must_use]
    pub fn has_pictures(&self) -> bool {
        self.cards().any(|card| card.picture().is_some())
    }

    /// This sample with every picture card drawn as its kind: what the folder
    /// shows while its pictures are produced.
    #[must_use]
    pub fn kinds_only(&self) -> Self {
        Self {
            cards: Arc::clone(&self.cards),
            as_kinds: true,
        }
    }

    /// The bytes the sample holds on the heap — its shared cards and the
    /// member paths they name — for a cache keyed by it to charge.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let counts = 2 * size_of::<usize>();
        let paths: usize = self
            .cards
            .iter()
            .filter_map(SampleCard::picture)
            .map(|thumbnail| thumbnail.path.capacity())
            .sum();
        counts + size_of_val(&*self.cards) + paths
    }
}

impl PartialEq for FolderSample {
    fn eq(&self, other: &Self) -> bool {
        self.cards().eq(other.cards())
    }
}

impl Eq for FolderSample {}

impl Hash for FolderSample {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.len());
        for card in self.cards() {
            card.hash(state);
        }
    }
}

impl PartialOrd for FolderSample {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FolderSample {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cards().cmp(other.cards())
    }
}

/// The side, in pixels, below which a folder's cards would be too small to
/// read, so its picture is the plain filled folder instead.
pub(crate) const MIN_COMPOSITE_SIDE: u32 = 32;

/// The design grid the folder's back and front are authored on.
const DESIGN: u32 = 256;

/// Where each card stands for a sample of one, two and three, on the
/// [`DESIGN`] grid as `(left, top, side, turn)`, drawn first to last: the last
/// drawn, at the front, is the sample's first card. Each card is turned `turn`
/// degrees clockwise about the middle of its foot, so the cards rise out of the
/// mouth as a spread, and every foot is below the front pocket's top, so the
/// cards stand in the folder.
const SLOTS: [&[(u32, u32, u32, f64)]; FolderSample::MOST] = [
    &[(78, 30, 100, -3.0)],
    &[(110, 40, 92, 9.0), (54, 44, 92, -7.0)],
    &[(130, 50, 84, 13.0), (42, 52, 84, -11.0), (86, 34, 84, 1.0)],
];

/// One card's place in a picture: where its square's top-left corner lies, its
/// side, and the turn it is drawn at.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct CardPlace {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) side: u32,
    pub(crate) turn: f64,
}

impl CardPlace {
    /// The transform from a card's own pixels to the picture's.
    pub(crate) fn transform(self) -> Affine {
        let (x, y, side) = (f64::from(self.x), f64::from(self.y), f64::from(self.side));
        Affine::translate(x, y).then(Affine::rotate_degrees_about(
            self.turn,
            x + side / 2.0,
            y + side,
        ))
    }
}

/// Where `sample`'s cards stand in a picture `side` pixels square, each with
/// its card, drawn first to last.
pub(crate) fn card_slots(
    sample: &FolderSample,
    side: u32,
) -> impl Iterator<Item = (CardPlace, DrawnCard<'_>)> {
    let scale = move |design: u32| u64::from(design) * u64::from(side) / u64::from(DESIGN);
    let slots = sample
        .len()
        .checked_sub(1)
        .and_then(|index| SLOTS.get(index))
        .copied()
        .unwrap_or(&[]);
    slots
        .iter()
        .zip(sample.cards().rev())
        .map(move |(&(x, y, card, turn), sampled)| {
            let pixels = |design| i32::try_from(scale(design)).unwrap_or(i32::MAX);
            let place = CardPlace {
                x: pixels(x),
                y: pixels(y),
                side: u32::try_from(scale(card)).unwrap_or(0),
                turn,
            };
            (place, sampled)
        })
}

/// The paper a card is drawn on, its edge, and the ink a glyph is drawn in:
/// fixed, so a folder's picture is the same in every theme.
const PAPER: Color = Color::rgb(0xFF, 0xFF, 0xFF);
const PAPER_EDGE: Color = Color::rgb(0xCF, 0xE2, 0xFF);
pub(crate) const INK: Color = Color::rgb(0x33, 0x47, 0x5B);

/// The edge around a photograph's print.
const PRINT_EDGE: Color = Color::rgb(0xB4, 0xBE, 0xC8);

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

/// A photograph printed on a card `side` pixels square: the part of `picture`
/// at `frame` laid on white with a border all round and a fixed-ink edge, the
/// print as wide and tall as the picture is, centred in the square.
pub(crate) fn print_card(picture: &Surface, frame: Rect, side: u32) -> Option<Surface> {
    let (x, y) = frame.surface_origin()?;
    let source = Region {
        x,
        y,
        width: frame.width,
        height: frame.height,
    };
    let border = (side / 14).max(1);
    let edge = (side / 64).max(1);
    let room = side.checked_sub(2 * (border + edge))?;
    let (width, height) = fit_within(frame.width, frame.height, room)?;
    let print_width = width + 2 * (border + edge);
    let print_height = height + 2 * (border + edge);
    let (left, top) = ((side - print_width) / 2, (side - print_height) / 2);
    let mut card = Surface::new(side, side)?;
    card.fill_rect(left, top, print_width, print_height, PRINT_EDGE);
    card.fill_rect(
        left + edge,
        top + edge,
        print_width - 2 * edge,
        print_height - 2 * edge,
        PAPER,
    );
    let scaled = picture.resampled(source, width, height).ok()?;
    let inset = |at: u32| i32::try_from(at + edge + border).unwrap_or(0);
    card.blit(inset(left), inset(top), &scaled);
    Some(card)
}

/// The largest `width` × `height` with the aspect of `(width, height)` that
/// fits a `room` pixels square, each side at least one pixel.
fn fit_within(width: u32, height: u32, room: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || room == 0 {
        return None;
    }
    let (wide, high) = (u64::from(width), u64::from(height));
    let room64 = u64::from(room);
    let (fit_w, fit_h) = if wide >= high {
        (room64, (high * room64 / wide).max(1))
    } else {
        ((wide * room64 / high).max(1), room64)
    };
    Some((u32::try_from(fit_w).ok()?, u32::try_from(fit_h).ok()?))
}

#[cfg(test)]
#[path = "folder_tests.rs"]
mod tests;
