//! Where everything is drawn, in physical pixels.
//!
//! Every length is authored in logical pixels at the desktop's reference
//! density, or as a proportion of the card, and converted through the one
//! shared scale. The card's size is derived from the window rather than
//! fixed, so a larger window shows larger cards, between a floor where an
//! index stops being legible and a ceiling past which every further pixel is
//! table.
//!
//! A column's fan is derived from its cards too: a column that would run off
//! the bottom of the table closes up, face-down cards first, so every card of
//! every column stays in reach however long it grows.

use alloc::vec::Vec;

use tairix_abi::window_ipc::WindowSizing;
use tairix_geometry::{to_i32, Point, Rect, Scale};

use crate::card::CardId;
use crate::table::Table;
use crate::variant::{Draw, PileId, Role, Variant};

/// The space around the table's cards, in logical pixels.
const MARGIN: u32 = 18;
/// The status line's height, in logical pixels.
pub const STATUS_HEIGHT: u32 = 30;
/// The narrowest card drawn, in logical pixels: below it an index is no
/// longer legible.
const CARD_MIN: u32 = 46;
/// The widest card drawn, in logical pixels.
const CARD_MAX: u32 = 150;
/// A card's height per thousand of its width: the poker card's 2½ × 3½.
const ASPECT: u32 = 1400;
/// The gap between two columns, per thousand of the card's width.
const COLUMN_GAP: u32 = 130;
/// The gap between the top row and the columns, per thousand of the height.
const ROW_GAP: u32 = 180;
/// How far a face-down card shows below the one it covers, per thousand of
/// the card's height, and the least a long column closes it to.
const DOWN_STEP: (u32, u32) = (110, 45);
/// The same for a face-up card: enough to show its whole index, and at the
/// least its rank.
const UP_STEP: (u32, u32) = (270, 145);
/// How far each of the waste's top three cards shows past the one under it
/// when drawing three, per thousand of the card's width.
const WASTE_STEP: u32 = 240;
/// How far each remaining deal of Spider's stock shows past the one under
/// it, per thousand of the card's width.
const DEAL_STEP: u32 = 140;
/// How far each card's corner index runs down the card, per thousand of its
/// height: the part a fanned card must keep in view.
pub const INDEX_REACH: u32 = 230;
/// The height a layout plans for, in card heights per thousand: the top row,
/// the gap under it, and a column of seven face-up cards.
const ROOM: u64 = 2000 + ROW_GAP as u64 + 6 * UP_STEP.0 as u64;
/// Cards a stacked pile shows as one step of thickness.
const LAYER: usize = 8;
/// Most steps of thickness a stacked pile shows.
const LAYERS: usize = 3;

/// The window the game opens at, in logical pixels.
const OPEN_SIZE: (u32, u32) = (900, 680);
/// The smallest window the game can be dragged to, in logical pixels: wide
/// enough for Spider's ten columns of the narrowest card.
const MIN_SIZE: (u32, u32) = (600, 460);

/// Where one card is drawn when at rest, and which way up.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Placement {
    /// The card's top-left corner.
    pub at: Point,
    /// Whether it shows its face.
    pub up: bool,
}

/// How far each card of one column shows below the one it covers.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Fan {
    /// Below a face-down card.
    pub down: u32,
    /// Below a face-up card.
    pub up: u32,
}

/// Where each part of the window is drawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    /// The whole window.
    pub client: Rect,
    /// The table the cards lie on.
    pub felt: Rect,
    /// The status line along the bottom.
    pub status: Rect,
    /// A card's width.
    pub card_width: u32,
    /// A card's height.
    pub card_height: u32,
    /// A card's corner radius.
    pub radius: u32,
    /// One pixel of thickness at this density.
    pub hairline: u32,
    variant: Variant,
    origins: [Point; PileId::MOST],
    natural: Fan,
    least: Fan,
    waste_step: u32,
    deal_step: u32,
    /// The lowest a column's cards may reach.
    floor: i32,
}

impl Layout {
    /// Lay `variant`'s table out inside `client` at `scale`.
    #[must_use]
    pub fn resolve(client: Rect, variant: Variant, scale: Scale) -> Self {
        let status_height = scale.scale_length(STATUS_HEIGHT).min(client.height);
        let felt = Rect::new(
            client.left(),
            client.top(),
            client.width,
            client.height.saturating_sub(status_height),
        );
        let status = Rect::new(client.left(), felt.bottom(), client.width, status_height);
        let margin = scale.scale_length(MARGIN);
        let shape = variant.shape();
        let columns = u32::try_from(shape.tableau.len()).unwrap_or(1).max(1);

        let usable_width = felt.width.saturating_sub(margin * 2);
        let usable_height = felt.height.saturating_sub(margin * 2);
        let by_width = usable_width * 1000 / (columns * 1000 + (columns - 1) * COLUMN_GAP);
        let by_height =
            u32::try_from(u64::from(usable_height) * 1_000_000 / (ROOM * u64::from(ASPECT)))
                .unwrap_or(u32::MAX);
        let least = scale.scale_length(CARD_MIN).max(1);
        let most = scale.scale_length(CARD_MAX).max(least);
        let card_width = by_width.min(by_height).clamp(least, most);
        let card_height = card_width * ASPECT / 1000;
        let gap = card_width * COLUMN_GAP / 1000;

        let block = columns * card_width + (columns - 1) * gap;
        let left = felt.left() + to_i32(felt.width.saturating_sub(block) / 2);
        let top = felt.top() + to_i32(margin);
        let tableau_top = top + to_i32(card_height + card_height * ROW_GAP / 1000);
        let column_x =
            |index: usize| left + to_i32(u32::try_from(index).unwrap_or(0) * (card_width + gap));

        let mut origins = [Point::ORIGIN; PileId::MOST];
        let mut place = |pile: Option<PileId>, at: Point| {
            if let Some(slot) = pile.and_then(|pile| origins.get_mut(pile.index())) {
                *slot = at;
            }
        };
        for (index, column) in shape.tableau.iter().enumerate() {
            place(Some(column), Point::new(column_x(index), tableau_top));
        }
        let top_row: &[(Option<PileId>, usize)] = &match variant {
            Variant::Klondike(_) => [
                (shape.stock, 0),
                (shape.waste, 1),
                (shape.foundations.nth(0), 3),
                (shape.foundations.nth(1), 4),
                (shape.foundations.nth(2), 5),
                (shape.foundations.nth(3), 6),
                (None, 0),
                (None, 0),
                (None, 0),
            ],
            Variant::FreeCell => [
                (shape.cells.nth(0), 0),
                (shape.cells.nth(1), 1),
                (shape.cells.nth(2), 2),
                (shape.cells.nth(3), 3),
                (shape.foundations.nth(0), 4),
                (shape.foundations.nth(1), 5),
                (shape.foundations.nth(2), 6),
                (shape.foundations.nth(3), 7),
                (None, 0),
            ],
            Variant::Spider(_) => [
                (shape.stock, 0),
                (shape.foundations.nth(0), 2),
                (shape.foundations.nth(1), 3),
                (shape.foundations.nth(2), 4),
                (shape.foundations.nth(3), 5),
                (shape.foundations.nth(4), 6),
                (shape.foundations.nth(5), 7),
                (shape.foundations.nth(6), 8),
                (shape.foundations.nth(7), 9),
            ],
        };
        for &(pile, column) in top_row {
            place(pile, Point::new(column_x(column), top));
        }

        Self {
            client,
            felt,
            status,
            card_width,
            card_height,
            radius: (card_width * 7 / 100).max(1),
            hairline: scale.scale_length(1).max(1),
            variant,
            origins,
            natural: Fan {
                down: card_height * DOWN_STEP.0 / 1000,
                up: card_height * UP_STEP.0 / 1000,
            },
            least: Fan {
                down: (card_height * DOWN_STEP.1 / 1000).max(1),
                up: (card_height * UP_STEP.1 / 1000).max(1),
            },
            waste_step: card_width * WASTE_STEP / 1000,
            deal_step: card_width * DEAL_STEP / 1000,
            floor: felt.bottom() - to_i32(margin),
        }
    }

    /// The variant laid out.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        self.variant
    }

    /// Where `pile`'s first card is drawn.
    #[must_use]
    pub fn origin(&self, pile: PileId) -> Point {
        self.origins.get(pile.index()).copied().unwrap_or_default()
    }

    /// A card's width and height.
    #[must_use]
    pub const fn card_size(&self) -> (u32, u32) {
        (self.card_width, self.card_height)
    }

    /// A card's rectangle drawn at `at`.
    #[must_use]
    pub const fn card_at(&self, at: Point) -> Rect {
        Rect::new(at.x, at.y, self.card_width, self.card_height)
    }

    /// How far `column`'s cards show below one another, closed up as far as
    /// it takes to keep the column on the table.
    #[must_use]
    pub fn fan(&self, table: &Table, column: PileId) -> Fan {
        let cards = table.pile(column);
        let Some((_, below)) = cards.split_last() else {
            return self.natural;
        };
        let downs = u32::try_from(below.iter().filter(|slot| !slot.up()).count()).unwrap_or(0);
        let ups = u32::try_from(below.len()).unwrap_or(0) - downs;
        let room = u32::try_from(self.floor - self.origin(column).y)
            .unwrap_or(0)
            .saturating_sub(self.card_height);
        let needs = |fan: Fan| downs * fan.down + ups * fan.up;
        if needs(self.natural) <= room {
            return self.natural;
        }
        let down = if needs(Fan {
            down: self.least.down,
            up: self.natural.up,
        }) <= room
        {
            // Close the face-down cards only as far as it takes.
            let spare = room.saturating_sub(ups * self.natural.up);
            (spare / downs.max(1)).clamp(self.least.down, self.natural.down)
        } else {
            self.least.down
        };
        let up = room
            .saturating_sub(downs * down)
            .checked_div(ups)
            .map_or(self.natural.up, |up| {
                up.clamp(self.least.up, self.natural.up)
            });
        Fan { down, up }
    }

    /// Where every card of `table` rests, by card number, into `out`.
    pub fn placements(&self, table: &Table, out: &mut Vec<Placement>) {
        out.clear();
        out.resize(table.deck().count(), Placement::default());
        for pile in table.shape().all() {
            let fan = self.fan(table, pile);
            let cards = table.pile(pile);
            for (depth, slot) in cards.iter().enumerate() {
                if let Some(placement) = out.get_mut(slot.card().index()) {
                    *placement = Placement {
                        at: self.place(table, pile, depth, fan),
                        up: slot.up(),
                    };
                }
            }
        }
    }

    /// Where the card `depth` up `pile` rests, given the column's `fan`.
    #[must_use]
    pub fn place(&self, table: &Table, pile: PileId, depth: usize, fan: Fan) -> Point {
        let origin = self.origin(pile);
        let cards = table.pile(pile);
        match table.role(pile) {
            Some(Role::Tableau) => {
                let offset: u32 = cards
                    .iter()
                    .take(depth)
                    .map(|slot| if slot.up() { fan.up } else { fan.down })
                    .sum();
                Point::new(origin.x, origin.y + to_i32(offset))
            }
            Some(Role::Waste) => {
                let shown = self.waste_fanned(cards.len());
                let first = cards.len() - shown;
                let step = u32::try_from(depth.saturating_sub(first)).unwrap_or(0);
                Point::new(
                    origin.x
                        + if depth >= first {
                            to_i32(step * self.waste_step)
                        } else {
                            0
                        },
                    origin.y,
                )
            }
            Some(Role::Stock) if matches!(self.variant, Variant::Spider(_)) => {
                let deal = u32::try_from(depth / 10).unwrap_or(0);
                Point::new(origin.x + to_i32(deal * self.deal_step), origin.y)
            }
            Some(Role::Stock | Role::Foundation | Role::Cell) | None => {
                let layer = u32::try_from((depth / LAYER).min(LAYERS)).unwrap_or(0) * self.hairline;
                Point::new(origin.x + to_i32(layer), origin.y + to_i32(layer))
            }
        }
    }

    /// How many of a waste of `len` cards are fanned out: the top three
    /// drawing three, none drawing one.
    const fn waste_fanned(&self, len: usize) -> usize {
        let fanned = match self.variant {
            Variant::Klondike(Draw::Three) => 3,
            Variant::Klondike(Draw::One) | Variant::FreeCell | Variant::Spider(_) => 1,
        };
        if len < fanned {
            len
        } else {
            fanned
        }
    }

    /// The area `pile`'s cards cover, or its empty place: every layout here
    /// moves away from the first place as a pile grows, so the first place
    /// and the last card span it.
    #[must_use]
    pub fn pile_area(&self, table: &Table, pile: PileId) -> Rect {
        let base = self.card_at(self.origin(pile));
        let fan = self.fan(table, pile);
        let len = table.pile(pile).len();
        len.checked_sub(1)
            .map(|depth| self.card_at(self.place(table, pile, depth, fan)))
            .map_or(base, |last| base.union(&last))
    }

    /// Where a unit dropped on `pile` would land: the place after its top
    /// card, or its first place.
    #[must_use]
    pub fn landing(&self, table: &Table, pile: PileId) -> Rect {
        let cards = table.pile(pile);
        if table.role(pile) != Some(Role::Tableau) || cards.is_empty() {
            return self.card_at(self.origin(pile));
        }
        let fan = self.fan(table, pile);
        let top = self.place(table, pile, cards.len() - 1, fan);
        let step = cards
            .last()
            .map_or(0, |slot| if slot.up() { fan.up } else { fan.down });
        self.card_at(Point::new(top.x, top.y + to_i32(step)))
    }

    /// The card or place under `point`: its pile, and the depth of the card,
    /// or `None` for an empty place.
    #[must_use]
    pub fn hit(&self, table: &Table, point: Point) -> Option<(PileId, Option<usize>)> {
        for pile in table.shape().all() {
            let fan = self.fan(table, pile);
            let cards = table.pile(pile);
            for depth in (0..cards.len()).rev() {
                if self
                    .card_at(self.place(table, pile, depth, fan))
                    .contains(point)
                {
                    return Some((pile, Some(depth)));
                }
            }
            if cards.is_empty() && self.card_at(self.origin(pile)).contains(point) {
                return Some((pile, None));
            }
        }
        None
    }

    /// The card number at `depth` of `pile`.
    #[must_use]
    pub fn card(table: &Table, pile: PileId, depth: usize) -> Option<CardId> {
        table.pile(pile).get(depth).map(|slot| slot.card())
    }
}

/// The window the game asks the window manager for: the client size it opens
/// at and the range a user may resize it within, in physical pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WindowGeometry {
    /// The client width it opens at.
    pub width: u32,
    /// The client height it opens at.
    pub height: u32,
    /// The range a resize is held to.
    pub sizing: WindowSizing,
}

impl WindowGeometry {
    /// The window at `scale` on a display of `screen` physical pixels: the
    /// opening size capped to the display, the floor following it down, and
    /// no ceiling — a larger window is a larger table.
    #[must_use]
    pub fn resolve(scale: Scale, screen: Rect) -> Self {
        let fit = |wanted: u32, available: u32| {
            if available == 0 {
                wanted.max(1)
            } else {
                wanted.min(available).max(1)
            }
        };
        let width = fit(scale.scale_length(OPEN_SIZE.0), screen.width);
        let height = fit(scale.scale_length(OPEN_SIZE.1), screen.height);
        Self {
            width,
            height,
            sizing: WindowSizing::Resizable {
                min_width_px: scale.scale_length(MIN_SIZE.0).min(width),
                min_height_px: scale.scale_length(MIN_SIZE.1).min(height),
                max_width_px: 0,
                max_height_px: 0,
            },
        }
    }

    /// The client rectangle a window of this geometry opens with.
    #[must_use]
    pub const fn client(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
