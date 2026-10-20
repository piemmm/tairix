//! Cards: what is printed on one, and the physical card a deal moves about.
//!
//! A [`Face`] is a suit and a rank. A [`CardId`] is one card of the deck a
//! game is dealt from: Spider's two decks hold two of every face, and the
//! animation must follow each card rather than each face, so the two are
//! distinct types. A [`Slot`] is a card where it lies, face up or down.

/// One of the four suits, in the order Microsoft's FreeCell numbering deals
/// them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Suit {
    /// ♣
    Clubs,
    /// ♦
    Diamonds,
    /// ♥
    Hearts,
    /// ♠
    Spades,
}

impl Suit {
    /// Every suit, in deal order.
    pub const ALL: [Self; 4] = [Self::Clubs, Self::Diamonds, Self::Hearts, Self::Spades];

    /// The suit's colour.
    #[must_use]
    pub const fn colour(self) -> Colour {
        match self {
            Self::Diamonds | Self::Hearts => Colour::Red,
            Self::Clubs | Self::Spades => Colour::Black,
        }
    }

    /// The suit's position in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The colour a tableau alternates.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Colour {
    /// Hearts and diamonds.
    Red,
    /// Clubs and spades.
    Black,
}

/// A rank, ace low: `1..=13`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Rank(u8);

impl Rank {
    /// The lowest rank.
    pub const ACE: Self = Self(1);
    /// The highest rank.
    pub const KING: Self = Self(13);
    /// How many ranks a suit holds.
    pub const COUNT: u8 = 13;

    /// `value` as a rank, or `None` outside `1..=13`.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= 1 && value <= Self::COUNT {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The rank's value, `1..=13`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Whether `self` is exactly one below `above`.
    #[must_use]
    pub const fn is_below(self, above: Self) -> bool {
        self.0 + 1 == above.0
    }
}

/// What is printed on a card.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Face {
    /// Its suit.
    pub suit: Suit,
    /// Its rank.
    pub rank: Rank,
}

impl Face {
    /// The face's colour.
    #[must_use]
    pub const fn colour(self) -> Colour {
        self.suit.colour()
    }

    /// Whether `self` may lie on `below` in a tableau built down in
    /// alternating colours.
    #[must_use]
    pub fn alternates_on(self, below: Self) -> bool {
        self.rank.is_below(below.rank) && self.colour() != below.colour()
    }

    /// Whether `self` may lie on `below` in a tableau built down regardless
    /// of suit.
    #[must_use]
    pub const fn descends_on(self, below: Self) -> bool {
        self.rank.is_below(below.rank)
    }

    /// Whether `self` continues a run of one suit down from `below`.
    #[must_use]
    pub fn follows_suit_on(self, below: Self) -> bool {
        self.rank.is_below(below.rank) && self.suit == below.suit
    }

    /// Whether `self` goes onto a foundation whose top card is `top`.
    #[must_use]
    pub fn builds_on(self, top: Option<Self>) -> bool {
        match top {
            None => self.rank == Rank::ACE,
            Some(top) => top.suit == self.suit && top.rank.is_below(self.rank),
        }
    }
}

/// One card of the deck a game is dealt from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CardId(u8);

impl CardId {
    /// The most cards any deck holds: Spider's two packs.
    pub const MOST: usize = 104;

    /// The card numbered `index`, or `None` past [`Self::MOST`].
    #[must_use]
    pub fn new(index: usize) -> Option<Self> {
        if index >= Self::MOST {
            return None;
        }
        u8::try_from(index).ok().map(Self)
    }

    /// The card's number within its deck.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A card where it lies: which card, and whether it is face up, in a byte.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Slot(u8);

impl Slot {
    const UP: u8 = 0x80;

    /// `card`, face up when `up`.
    #[must_use]
    pub const fn new(card: CardId, up: bool) -> Self {
        Self(card.0 | if up { Self::UP } else { 0 })
    }

    /// Which card.
    #[must_use]
    pub const fn card(self) -> CardId {
        CardId(self.0 & !Self::UP)
    }

    /// Whether it lies face up.
    #[must_use]
    pub const fn up(self) -> bool {
        self.0 & Self::UP != 0
    }

    /// The same card, turned to `up`.
    #[must_use]
    pub const fn turned(self, up: bool) -> Self {
        Self::new(self.card(), up)
    }

    /// The slot as one byte, for hashing a position.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self.0
    }
}

/// How many suits a Spider deal mixes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SpiderSuits {
    /// Every card a spade.
    One,
    /// Spades and hearts.
    Two,
    /// All four suits.
    Four,
}

impl SpiderSuits {
    /// How many suits.
    #[must_use]
    pub const fn count(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
        }
    }
}

/// The cards a game is dealt from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Deck {
    /// One pack of fifty-two.
    Standard,
    /// Two packs' worth, a hundred and four cards, mixing as many suits as
    /// named.
    Spider(SpiderSuits),
}

impl Deck {
    /// How many cards the deck holds.
    #[must_use]
    pub const fn count(self) -> usize {
        match self {
            Self::Standard => 52,
            Self::Spider(_) => CardId::MOST,
        }
    }

    /// What `card` shows.
    ///
    /// Every face appears equally often: a Spider deck of two suits holds four
    /// of each spade and heart, of one suit eight of each spade.
    #[must_use]
    pub fn face(self, card: CardId) -> Face {
        let index = card.index();
        let rank = Rank(u8::try_from(index % 13).unwrap_or(0) + 1);
        let set = index / 13;
        let suit = match self {
            Self::Standard | Self::Spider(SpiderSuits::Four) => Suit::ALL[set % 4],
            Self::Spider(SpiderSuits::One) => Suit::Spades,
            Self::Spider(SpiderSuits::Two) if set.is_multiple_of(2) => Suit::Spades,
            Self::Spider(SpiderSuits::Two) => Suit::Hearts,
        };
        Face { suit, rank }
    }

    /// Every card of the deck, in number order.
    pub fn cards(self) -> impl Iterator<Item = CardId> {
        (0..self.count()).filter_map(CardId::new)
    }

    /// The card of a standard deck showing `face`.
    #[must_use]
    pub const fn standard(face: Face) -> CardId {
        CardId((face.suit as u8) * 13 + face.rank.0 - 1)
    }
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
