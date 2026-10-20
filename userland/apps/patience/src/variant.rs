//! The games: which is played, the rules it is played by, and where its piles
//! lie.
//!
//! Every pile of a table has a fixed number for the whole game, assigned by
//! role in the order a [`Shape`] states, so a move names its piles by number
//! and a saved line of play stays meaningful.

use crate::card::{Deck, SpiderSuits};

/// How many cards Klondike turns from the stock at a time.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Draw {
    /// One card, the easier game.
    One,
    /// Three, only the top of which is in play.
    Three,
}

impl Draw {
    /// How many cards one draw turns.
    #[must_use]
    pub const fn count(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Three => 3,
        }
    }
}

/// A game of patience.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Variant {
    /// Seven columns, a stock, and four foundations built up by suit.
    Klondike(Draw),
    /// Eight open columns, four cells and four foundations.
    FreeCell,
    /// Ten columns from two packs, cleared one complete suit run at a time.
    Spider(SpiderSuits),
}

impl Variant {
    /// Every variant, in the order the game's menu lists them.
    pub const ALL: [Self; 6] = [
        Self::Klondike(Draw::One),
        Self::Klondike(Draw::Three),
        Self::FreeCell,
        Self::Spider(SpiderSuits::One),
        Self::Spider(SpiderSuits::Two),
        Self::Spider(SpiderSuits::Four),
    ];

    /// The variant's position in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Klondike(Draw::One) => 0,
            Self::Klondike(Draw::Three) => 1,
            Self::FreeCell => 2,
            Self::Spider(SpiderSuits::One) => 3,
            Self::Spider(SpiderSuits::Two) => 4,
            Self::Spider(SpiderSuits::Four) => 5,
        }
    }

    /// Its spelling in the game's store.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Klondike(Draw::One) => "klondike-1",
            Self::Klondike(Draw::Three) => "klondike-3",
            Self::FreeCell => "freecell",
            Self::Spider(SpiderSuits::One) => "spider-1",
            Self::Spider(SpiderSuits::Two) => "spider-2",
            Self::Spider(SpiderSuits::Four) => "spider-4",
        }
    }

    /// The variant `name` spells.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|variant| variant.name() == name)
    }

    /// What it is called, and how it is played: the menu's and the window's
    /// words for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Klondike(Draw::One) => "Klondike, draw one",
            Self::Klondike(Draw::Three) => "Klondike, draw three",
            Self::FreeCell => "FreeCell",
            Self::Spider(SpiderSuits::One) => "Spider, one suit",
            Self::Spider(SpiderSuits::Two) => "Spider, two suits",
            Self::Spider(SpiderSuits::Four) => "Spider, four suits",
        }
    }

    /// The cards it is dealt from.
    #[must_use]
    pub const fn deck(self) -> Deck {
        match self {
            Self::Klondike(_) | Self::FreeCell => Deck::Standard,
            Self::Spider(suits) => Deck::Spider(suits),
        }
    }

    /// Where its piles lie.
    #[must_use]
    pub const fn shape(self) -> Shape {
        match self {
            Self::Klondike(_) => Shape {
                stock: Some(PileId(0)),
                waste: Some(PileId(1)),
                cells: Piles::EMPTY,
                foundations: Piles::new(2, 4),
                tableau: Piles::new(6, 7),
            },
            Self::FreeCell => Shape {
                stock: None,
                waste: None,
                cells: Piles::new(0, 4),
                foundations: Piles::new(4, 4),
                tableau: Piles::new(8, 8),
            },
            Self::Spider(_) => Shape {
                stock: Some(PileId(0)),
                waste: None,
                cells: Piles::EMPTY,
                foundations: Piles::new(1, 8),
                tableau: Piles::new(9, 10),
            },
        }
    }
}

/// A pile, by its number in its table.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PileId(u8);

impl PileId {
    /// The most piles any table holds: Spider's nineteen.
    pub const MOST: usize = 19;

    /// The pile numbered `index`, or `None` past [`Self::MOST`].
    #[must_use]
    pub fn new(index: usize) -> Option<Self> {
        if index >= Self::MOST {
            return None;
        }
        u8::try_from(index).ok().map(Self)
    }

    /// The pile's number.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a pile is for.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// The face-down cards still to be turned or dealt.
    Stock,
    /// Klondike's turned cards, the top one in play.
    Waste,
    /// A pile built up by suit from the ace: home.
    Foundation,
    /// FreeCell's holding place for one card.
    Cell,
    /// A column of the layout.
    Tableau,
}

/// A run of consecutively numbered piles sharing a role.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Piles {
    first: u8,
    count: u8,
}

impl Piles {
    /// No piles at all.
    pub const EMPTY: Self = Self { first: 0, count: 0 };

    const fn new(first: u8, count: u8) -> Self {
        Self { first, count }
    }

    /// How many piles.
    #[must_use]
    pub const fn len(self) -> usize {
        self.count as usize
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    /// Whether `pile` is one of them.
    #[must_use]
    pub const fn contains(self, pile: PileId) -> bool {
        pile.0 >= self.first && pile.0 - self.first < self.count
    }

    /// The `n`th of them, counting from zero.
    #[must_use]
    pub fn nth(self, n: usize) -> Option<PileId> {
        let n = u8::try_from(n).ok().filter(|&n| n < self.count)?;
        Some(PileId(self.first + n))
    }

    /// Where `pile` falls among them, counting from zero.
    #[must_use]
    pub const fn position(self, pile: PileId) -> Option<usize> {
        if self.contains(pile) {
            Some((pile.0 - self.first) as usize)
        } else {
            None
        }
    }

    /// Every one of them, in order.
    pub fn iter(self) -> impl DoubleEndedIterator<Item = PileId> + ExactSizeIterator + Clone {
        (self.first..self.first + self.count).map(PileId)
    }
}

/// Where a variant's piles lie, by role.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Shape {
    /// The stock, where the variant has one.
    pub stock: Option<PileId>,
    /// The waste, where the variant has one.
    pub waste: Option<PileId>,
    /// The cells.
    pub cells: Piles,
    /// The foundations.
    pub foundations: Piles,
    /// The columns.
    pub tableau: Piles,
}

impl Shape {
    /// How many piles the table holds.
    #[must_use]
    pub const fn piles(self) -> usize {
        self.tableau.first as usize + self.tableau.len()
    }

    /// What `pile` is for.
    #[must_use]
    pub fn role(self, pile: PileId) -> Option<Role> {
        if self.stock == Some(pile) {
            Some(Role::Stock)
        } else if self.waste == Some(pile) {
            Some(Role::Waste)
        } else if self.cells.contains(pile) {
            Some(Role::Cell)
        } else if self.foundations.contains(pile) {
            Some(Role::Foundation)
        } else if self.tableau.contains(pile) {
            Some(Role::Tableau)
        } else {
            None
        }
    }

    /// Every pile of the table, in number order.
    pub fn all(self) -> impl Iterator<Item = PileId> {
        (0..self.piles()).filter_map(PileId::new)
    }
}

/// How often Klondike's waste may be turned back into the stock.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Recycles {
    /// As often as the player likes.
    Unlimited,
    /// No more than this many times.
    AtMost(u8),
}

impl Recycles {
    /// Whether a waste already turned back `done` times may be turned again.
    #[must_use]
    pub const fn allow(self, done: u8) -> bool {
        match self {
            Self::Unlimited => true,
            Self::AtMost(most) => done < most,
        }
    }
}

/// The rules one game is played by, fixed when it is dealt.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Rules {
    /// The game.
    pub variant: Variant,
    /// How often the waste may be turned back; unlimited outside Klondike,
    /// which has no waste.
    pub recycles: Recycles,
}

impl Rules {
    /// `variant` played without a limit on the waste.
    #[must_use]
    pub const fn of(variant: Variant) -> Self {
        Self {
            variant,
            recycles: Recycles::Unlimited,
        }
    }
}

#[cfg(test)]
#[path = "variant_tests.rs"]
mod tests;
