//! Deals: every game is one numbered shuffle, so any deal can be played again,
//! shared, or asked for by number.
//!
//! FreeCell follows Microsoft's numbering exactly — deal 1 is the deal its
//! FreeCell dealt as game 1, and deal 11982 is the famously unwinnable one —
//! because players know those deals by number. Klondike and Spider have no
//! such canon, so each number seeds this game's own generator for that
//! variant.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};

use crate::card::{CardId, Deck, Face, Rank, Suit};
use crate::variant::Variant;

/// The most FreeCell deals Microsoft's numbering offers.
pub const FREECELL_DEALS: u32 = 1_000_000;

/// The most deals any other variant offers: nine digits, which a player can
/// still read out and type.
pub const OTHER_DEALS: u32 = 999_999_999;

/// One deal of one variant, by its number.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DealNumber(u32);

impl DealNumber {
    /// The highest number `variant` deals.
    #[must_use]
    pub const fn most(variant: Variant) -> u32 {
        match variant {
            Variant::FreeCell => FREECELL_DEALS,
            Variant::Klondike(_) | Variant::Spider(_) => OTHER_DEALS,
        }
    }

    /// Deal `number` of `variant`, or `None` outside `1..=most`.
    #[must_use]
    pub const fn new(variant: Variant, number: u32) -> Option<Self> {
        if number >= 1 && number <= Self::most(variant) {
            Some(Self(number))
        } else {
            None
        }
    }

    /// A deal of `variant` drawn uniformly from all it offers.
    pub fn random(variant: Variant, rng: &mut dyn RandU64) -> Self {
        let pick = rng.next_below(u64::from(Self::most(variant)));
        Self(u32::try_from(pick).map_or(1, |pick| pick + 1))
    }

    /// The deal after this one, wrapping to the first.
    #[must_use]
    pub const fn next(self, variant: Variant) -> Self {
        if self.0 >= Self::most(variant) {
            Self(1)
        } else {
            Self(self.0 + 1)
        }
    }

    /// The deal's number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// The cards of deal `number` of `variant`, in the order they are dealt.
#[must_use]
pub fn shuffled(variant: Variant, number: DealNumber) -> Vec<CardId> {
    match variant {
        Variant::FreeCell => microsoft(number.0),
        Variant::Klondike(_) | Variant::Spider(_) => seeded(variant, number.0),
    }
}

/// Microsoft's FreeCell deal: its C library's linear congruential generator
/// seeded with the deal number, drawing each card from a pack ordered ace to
/// king with the suits clubs, diamonds, hearts, spades within each rank.
fn microsoft(number: u32) -> Vec<CardId> {
    let mut state = number;
    let mut pack: Vec<u8> = (0..52).collect();
    let mut dealt = Vec::with_capacity(pack.len());
    while !pack.is_empty() {
        state = state.wrapping_mul(214_013).wrapping_add(2_531_011) & 0x7fff_ffff;
        let draw = usize::try_from(state >> 16).unwrap_or(0) % pack.len();
        let card = pack.swap_remove(draw);
        dealt.push(Deck::standard(Face {
            suit: Suit::ALL[usize::from(card % 4)],
            rank: Rank::new(card / 4 + 1).unwrap_or(Rank::ACE),
        }));
    }
    dealt
}

/// A uniform shuffle of `variant`'s deck seeded by the deal number, each
/// variant's numbers its own.
fn seeded(variant: Variant, number: u32) -> Vec<CardId> {
    let salt = u64::try_from(variant.index()).unwrap_or(0) << 32;
    let mut rng = NonCryptoRng::seed_from_u64(salt | u64::from(number));
    let mut cards: Vec<CardId> = variant.deck().cards().collect();
    for last in (1..cards.len()).rev() {
        let bound = u64::try_from(last + 1).unwrap_or(u64::MAX);
        let pick = usize::try_from(rng.next_below(bound)).unwrap_or(last);
        cards.swap(last, pick);
    }
    cards
}

#[cfg(test)]
#[path = "deal_tests.rs"]
mod tests;
