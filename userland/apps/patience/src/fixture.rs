//! Test fixtures: cards written the way card players write them, positions
//! laid out from that writing, and games set up to play on them.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_geometry::{Rect, Region, Scale};
use tairix_rng::NonCryptoRng;

use crate::card::{Face, Rank, Slot, Suit};
use crate::game::{Game, Setup};
use crate::motion::FRAME_NS;
use crate::play::Play;
use crate::prefs::Preferences;
use crate::stats::Stats;
use crate::table::Table;
use crate::variant::{Rules, Variant};

/// The window the games here are laid out in: the one the game opens.
pub(crate) const CLIENT: Rect = Rect::new(0, 0, 900, 680);

const RANKS: &[u8; 13] = b"A23456789TJQK";
const SUITS: &[u8; 4] = b"CDHS";

/// `face` as written: `"TD"` for the ten of diamonds.
pub(crate) fn spell(face: Face) -> String {
    let rank = RANKS[usize::from(face.rank.get() - 1)];
    let suit = SUITS[face.suit.index()];
    String::from_utf8(alloc::vec![rank, suit]).expect("ascii")
}

/// The face `text` writes, as `"QH"`.
pub(crate) fn face(text: &str) -> Face {
    let bytes = text.as_bytes();
    let rank = RANKS.iter().position(|&r| r == bytes[0]).expect("a rank");
    let suit = SUITS.iter().position(|&s| s == bytes[1]).expect("a suit");
    Face {
        suit: Suit::ALL[suit],
        rank: Rank::new(u8::try_from(rank + 1).expect("small")).expect("a rank"),
    }
}

/// A position of `variant` laid out from text: each pile its number and its
/// cards bottom first, `#` marking one face down — `"#KS QH JC"`. Each card
/// written is another card of the deck showing that face.
pub(crate) fn position(variant: Variant, piles: &[(usize, &str)]) -> Table {
    let deck = variant.deck();
    let mut laid = alloc::vec![Vec::new(); variant.shape().piles()];
    let mut used = alloc::vec![false; deck.count()];
    for &(pile, text) in piles {
        for token in text.split_whitespace() {
            let (up, written) = token
                .strip_prefix('#')
                .map_or((true, token), |card| (false, card));
            let wanted = face(written);
            let card = deck
                .cards()
                .find(|&card| !used[card.index()] && deck.face(card) == wanted)
                .expect("the deck holds another of that card");
            used[card.index()] = true;
            laid[pile].push(Slot::new(card, up));
        }
    }
    Table::laid(Rules::of(variant), laid)
}

/// The generator every game here draws from.
pub(crate) fn rng() -> NonCryptoRng {
    NonCryptoRng::seed_from_u64(0x5EED)
}

/// Preferences that deal at once, sending nothing home unasked.
pub(crate) fn plain(variant: Variant) -> Preferences {
    Preferences {
        variant,
        winnable: false,
        auto_home: false,
        ..Preferences::default()
    }
}

/// A game's start in [`CLIENT`], resuming `resume`.
pub(crate) fn setup(prefs: Preferences, resume: Option<(Play, u32)>) -> Setup {
    Setup {
        prefs,
        stats: Stats::default(),
        resume,
        client: CLIENT,
        scale: Scale::ONE,
        reduced: false,
    }
}

/// A game resumed at `play`.
pub(crate) fn resumed(prefs: Preferences, play: Play) -> Game {
    Game::new(setup(prefs, Some((play, 0))), 0, &mut rng())
}

/// Run everything moving on to its end, answering when that was.
pub(crate) fn settle(game: &mut Game, from_ns: u64) -> u64 {
    let mut now = from_ns;
    loop {
        let moving = game.motion.deadline_ns(now);
        let gliding = game
            .drag
            .as_ref()
            .and_then(|drag| drag.glide)
            .filter(|glide| glide.moving(now));
        let Some(next) = moving.or(gliding.map(|_| now + FRAME_NS)) else {
            return now;
        };
        now = next;
        let _ = game.tick(now, &mut Region::new());
        assert!(
            now - from_ns < 120_000_000_000,
            "everything moving comes to rest"
        );
    }
}
