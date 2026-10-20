//! Card-art tests: every face and back draws at the card's size, the faces a
//! player must tell apart differ, and a tile once drawn is kept until the size
//! or the colours change.

use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_theme::Theme;

use super::*;
use crate::card::{Deck, Face, Rank, Suit};
use crate::prefs::{Back, Preferences};

const WIDTH: u32 = 96;
const HEIGHT: u32 = 134;

fn palette(prefs: Preferences) -> Palette {
    Palette::resolve(&Theme::dark(), &prefs)
}

fn art(prefs: Preferences) -> CardArt {
    CardArt::new(WIDTH, HEIGHT, 7, 1, palette(prefs))
}

fn face(suit: Suit, rank: u8) -> Face {
    Face {
        suit,
        rank: Rank::new(rank).expect("a rank"),
    }
}

fn pixels(tile: Option<&Surface>) -> Vec<Pixel> {
    tile.expect("a tile").pixels().to_vec()
}

#[test]
fn the_index_is_set_in_the_serif_face() {
    assert_ne!(INDEX_FACE, FamilyKey::MONO);
    assert_eq!(Some(INDEX_FACE), FamilyKey::new("noto-serif").ok());
}

#[test]
fn every_face_draws_a_whole_card() {
    let mut art = art(Preferences::default());
    for card in Deck::Standard.cards() {
        let face = Deck::Standard.face(card);
        let tile = art.face(face).expect("a tile");
        assert_eq!((tile.width(), tile.height()), (WIDTH, HEIGHT));
        let centre = tile.get(WIDTH / 2, HEIGHT / 2).expect("a pixel");
        assert_ne!(
            centre,
            Pixel::TRANSPARENT,
            "{face:?} is drawn through its middle"
        );
        // The rounded corner is cut away.
        assert_eq!(tile.get(0, 0), Some(Pixel::TRANSPARENT), "{face:?}");
    }
    let back = art.back().expect("a back");
    assert_eq!((back.width(), back.height()), (WIDTH, HEIGHT));
}

#[test]
fn every_face_differs_from_every_other() {
    let mut art = art(Preferences::default());
    let drawn: Vec<Vec<Pixel>> = Deck::Standard
        .cards()
        .map(|card| pixels(art.face(Deck::Standard.face(card))))
        .collect();
    for (index, tile) in drawn.iter().enumerate() {
        for other in &drawn[index + 1..] {
            assert_ne!(tile, other);
        }
    }
    let back = pixels(art.back());
    assert!(drawn.iter().all(|tile| *tile != back));
}

#[test]
fn a_tile_is_kept_until_the_size_or_colours_change() {
    let prefs = Preferences::default();
    let mut art = art(prefs);
    let first = pixels(art.face(face(Suit::Hearts, 12)));
    assert_eq!(pixels(art.face(face(Suit::Hearts, 12))), first);
    art.retune(WIDTH, HEIGHT, 7, 1, palette(prefs));
    assert!(
        art.faces.iter().any(Option::is_some),
        "the same look keeps its tiles"
    );
    art.retune(WIDTH + 10, HEIGHT + 14, 7, 1, palette(prefs));
    assert!(art.faces.iter().all(Option::is_none));
    let larger = art.face(face(Suit::Hearts, 12)).expect("a tile");
    assert_eq!((larger.width(), larger.height()), (WIDTH + 10, HEIGHT + 14));
    art.trim();
    assert!(art.faces.iter().all(Option::is_none) && art.back.is_none());
}

#[test]
fn four_colours_repaint_only_clubs_and_diamonds() {
    let two = Preferences::default();
    let four = Preferences {
        four_colours: true,
        ..two
    };
    let (mut plain, mut coloured) = (art(two), art(four));
    for suit in Suit::ALL {
        let a = pixels(plain.face(face(suit, 7)));
        let b = pixels(coloured.face(face(suit, 7)));
        match suit {
            Suit::Clubs | Suit::Diamonds => assert_ne!(a, b, "{suit:?}"),
            Suit::Hearts | Suit::Spades => assert_eq!(a, b, "{suit:?}"),
        }
    }
}

#[test]
fn every_back_is_its_own() {
    let backs: Vec<Vec<Pixel>> = Back::ALL
        .into_iter()
        .map(|back| {
            let prefs = Preferences {
                back,
                ..Preferences::default()
            };
            pixels(art(prefs).back())
        })
        .collect();
    for (index, back) in backs.iter().enumerate() {
        for other in &backs[index + 1..] {
            assert_ne!(back, other);
        }
    }
}

#[test]
fn a_lifted_card_casts_a_longer_shadow() {
    let mut art = art(Preferences::default());
    let reach =
        |shadow: &Shadow| shadow.offset.1 + i32::try_from(shadow.mask.height()).expect("small");
    let resting = reach(art.shadow(false).expect("a shadow"));
    let lifted = reach(art.shadow(true).expect("a shadow"));
    assert!(
        lifted > resting,
        "a lifted card's shadow falls further below it"
    );
}

#[test]
fn every_suit_has_a_mask_and_its_outline_stays_on_the_grid() {
    for suit in Suit::ALL {
        let contours = suit_contours(suit);
        assert!(!contours.is_empty());
        for &(x, y) in contours.iter().flatten() {
            assert!(
                (0..=1000).contains(&x) && (0..=1000).contains(&y),
                "{suit:?}"
            );
        }
        for side in [6, 17, 64] {
            let mask = suit_mask(suit, side).expect("a mask");
            assert_eq!((mask.width(), mask.height()), (side, side));
            assert!(mask
                .pixels()
                .iter()
                .any(|&pixel| pixel != Pixel::TRANSPARENT));
        }
    }
}

#[test]
fn every_rank_has_its_index() {
    let texts: Vec<&str> = (1..=13)
        .map(|rank| rank_text(Rank::new(rank).expect("a rank")))
        .collect();
    assert_eq!(
        texts,
        ["A", "2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K"]
    );
}

#[test]
fn the_baize_is_shaded_once_per_size() {
    let mut art = art(Preferences::default());
    let shaded = core::cell::Cell::new(0);
    let shade = |surface: &mut Surface| {
        shaded.set(shaded.get() + 1);
        surface.fill_round_rect(
            0,
            0,
            surface.width(),
            surface.height(),
            0,
            Color::rgb(1, 2, 3),
        );
    };
    assert!(art.baize(300, 200, shade).is_some());
    assert!(art.baize(300, 200, shade).is_some());
    assert_eq!(shaded.get(), 1, "kept");
    let wider = art
        .baize(320, 200, shade)
        .map(|baize| (baize.width(), baize.height()));
    assert_eq!(wider, Some((320, 200)));
    assert_eq!(shaded.get(), 2, "a new size shades it again");
    art.trim();
    assert!(art.baize.is_none());
}
