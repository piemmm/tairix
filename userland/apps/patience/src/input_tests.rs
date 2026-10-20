//! Input tests: a click sends cards where they are best placed, a drag lands
//! where it is dropped or goes back, and the keyboard can play a whole move
//! the pointer can.

use alloc::string::String;

use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};

use super::*;
use crate::deal::DealNumber;
use crate::fixture::{plain, position, resumed, rng, settle};
use crate::game::Alerting;
use crate::score::Scoring;

const FREECELL: Variant = Variant::FreeCell;

fn pile(index: usize) -> PileId {
    PileId::new(index).expect("a pile")
}

/// A game resumed at `variant`'s position laid out as `piles`.
fn game_at(variant: Variant, piles: &[(usize, &str)]) -> Game {
    let table = position(variant, piles);
    let play = Play::laid(
        table,
        Scoring::Standard,
        DealNumber::new(variant, 1).expect("a deal"),
    );
    resumed(plain(variant), play)
}

/// Where the top card of `pile` is drawn, at its middle.
fn top_of(game: &Game, pile: PileId) -> Point {
    let table = game.table().expect("a table");
    let card = table.top(pile).expect("a card").card();
    game.layout
        .card_at(game.placements[card.index()].at)
        .center()
}

fn cards(game: &Game, pile: PileId) -> String {
    let table = game.table().expect("a table");
    let faces: Vec<String> = table
        .pile(pile)
        .iter()
        .map(|slot| crate::fixture::spell(table.face(slot.card())))
        .collect();
    faces.join(" ")
}

fn point(game: &mut Game, to: Point, now: u64) {
    let _ = game.on_pointer(&InputEvent::PointerMoved { to }, now, &mut Region::new());
}

fn press(game: &mut Game, now: u64) -> bool {
    let event = InputEvent::PointerPressed {
        button: PointerButton::Primary,
    };
    game.on_pointer(&event, now, &mut Region::new())
}

fn release(game: &mut Game, now: u64) -> bool {
    let event = InputEvent::PointerReleased {
        button: PointerButton::Primary,
    };
    game.on_pointer(&event, now, &mut Region::new())
}

fn click(game: &mut Game, at: Point, now: u64) -> bool {
    point(game, at, now);
    press(game, now) | release(game, now)
}

/// Drag what is under `from` to `to` in a few steps, and let go.
fn drag(game: &mut Game, from: Point, to: Point, now: u64) -> bool {
    point(game, from, now);
    let _ = press(game, now);
    for step in 1..=4 {
        let x = from.x + (to.x - from.x) * step / 4;
        let y = from.y + (to.y - from.y) * step / 4;
        point(game, Point::new(x, y), now);
    }
    release(game, now)
}

fn key(game: &mut Game, key: Key) -> bool {
    game.on_key(key, Modifiers::default(), 0, &mut rng(), &mut Region::new())
}

fn ctrl(game: &mut Game, letter: char) -> bool {
    let modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    game.on_key(
        Key::Char(letter),
        modifiers,
        0,
        &mut rng(),
        &mut Region::new(),
    )
}

/// A FreeCell table one king short of won.
fn one_short() -> Game {
    let run = |suit: char, last: usize| -> String {
        "A23456789TJQK"
            .chars()
            .take(last)
            .map(|rank| alloc::format!("{rank}{suit}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (clubs, diamonds, hearts, spades) =
        (run('C', 13), run('D', 13), run('H', 13), run('S', 12));
    game_at(
        FREECELL,
        &[
            (4, &clubs),
            (5, &diamonds),
            (6, &hearts),
            (7, &spades),
            (8, "KS"),
        ],
    )
}

#[test]
fn a_click_sends_a_card_home_first() {
    let mut game = game_at(FREECELL, &[(8, "KS AH"), (9, "QD")]);
    let at = top_of(&game, pile(8));
    assert!(click(&mut game, at, 0));
    assert_eq!(cards(&game, pile(4)), "AH");
    assert_eq!(cards(&game, pile(8)), "KS");
}

#[test]
fn a_click_sends_a_card_onto_a_column_before_a_free_cell() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "5C QH")]);
    let at = top_of(&game, pile(9));
    assert!(click(&mut game, at, 0));
    assert_eq!(cards(&game, pile(8)), "KS QH");
    let at = top_of(&game, pile(9));
    assert!(click(&mut game, at, 0));
    assert_eq!(
        cards(&game, pile(0)),
        "5C",
        "with no column to take it, a free cell does"
    );
}

#[test]
fn a_card_with_nowhere_to_go_hops_where_it_is() {
    let mut game = game_at(
        FREECELL,
        &[
            (0, "2C"),
            (1, "2D"),
            (2, "3C"),
            (3, "3D"),
            (8, "KS"),
            (9, "KH"),
        ],
    );
    let before = game.table().map(Table::fingerprint);
    let at = top_of(&game, pile(9));
    assert!(click(&mut game, at, 0));
    assert_eq!(game.table().map(Table::fingerprint), before);
    let king = game
        .table()
        .and_then(|table| table.top(pile(9)))
        .expect("a card")
        .card();
    assert!(game.motion.flying(king), "it hops");
    let rested = settle(&mut game, 0);
    assert_eq!(
        game.motion.shown(king, rested).at,
        game.placements[king.index()].at
    );
}

#[test]
fn a_dragged_card_lands_on_the_place_it_is_dropped_on() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    let from = top_of(&game, pile(9));
    let table = game.table().expect("a table");
    let to = game.layout.landing(table, pile(8)).center();
    assert!(drag(&mut game, from, to, 0));
    assert_eq!(cards(&game, pile(8)), "KS QH");
    assert!(game.drag.is_none());
}

#[test]
fn a_card_dropped_where_nothing_takes_it_goes_back() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    let before = game.table().map(Table::fingerprint);
    let from = top_of(&game, pile(9));
    let nowhere = Point::new(
        game.layout.felt.right() - 10,
        game.layout.felt.bottom() - 10,
    );
    assert!(drag(&mut game, from, nowhere, 0));
    assert_eq!(game.table().map(Table::fingerprint), before);
    let queen = game
        .table()
        .and_then(|table| table.top(pile(9)))
        .expect("a card")
        .card();
    assert!(game.motion.flying(queen), "it flies back");
}

#[test]
fn a_drag_the_desktop_takes_away_goes_back() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    let before = game.table().map(Table::fingerprint);
    let from = top_of(&game, pile(9));
    point(&mut game, from, 0);
    let _ = press(&mut game, 0);
    point(&mut game, Point::new(from.x - 100, from.y - 60), 0);
    assert!(game.drag.is_some());
    assert!(game.on_pointer(&InputEvent::PointerCancelled, 0, &mut Region::new()));
    assert!(game.drag.is_none() && game.press.is_none());
    assert_eq!(game.table().map(Table::fingerprint), before);
}

#[test]
fn a_press_that_does_not_travel_is_a_click_not_a_drag() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    let at = top_of(&game, pile(9));
    point(&mut game, at, 0);
    let _ = press(&mut game, 0);
    point(&mut game, Point::new(at.x + 1, at.y + 1), 0);
    assert!(
        game.drag.is_none(),
        "within the slop the press stays a press"
    );
    assert!(release(&mut game, 0));
    assert_eq!(cards(&game, pile(8)), "KS QH");
}

#[test]
fn a_click_on_the_stock_turns_it() {
    let variant = Variant::Klondike(crate::variant::Draw::One);
    let mut game = Game::new(crate::fixture::setup(plain(variant), None), 0, &mut rng());
    let stock = variant.shape().stock.expect("a stock");
    let waste = variant.shape().waste.expect("a waste");
    let at = top_of(&game, stock);
    assert!(click(&mut game, at, 0));
    assert_eq!(game.table().map(|table| table.pile(waste).len()), Some(1));
}

#[test]
fn a_stock_that_may_not_deal_says_why() {
    let variant = Variant::Spider(crate::card::SpiderSuits::One);
    let mut game = game_at(variant, &[(0, "AS 2S 3S 4S 5S 6S 7S 8S 9S TS"), (9, "KS")]);
    let before = game.table().map(Table::fingerprint);
    let at = top_of(&game, pile(0));
    assert!(click(&mut game, at, 0));
    assert_eq!(game.table().map(Table::fingerprint), before);
    assert_eq!(game.note.map(|(note, _)| note), Some(Note::DealBlocked));
}

#[test]
fn a_secondary_press_asks_for_the_table_menu() {
    let mut game = game_at(FREECELL, &[(8, "KS")]);
    let at = Point::new(300, 500);
    point(&mut game, at, 0);
    let event = InputEvent::PointerPressed {
        button: PointerButton::Secondary,
    };
    let _ = game.on_pointer(&event, 0, &mut Region::new());
    assert_eq!(game.outbox().context, Some(at));
}

#[test]
fn the_keyboard_picks_up_carries_and_puts_down() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    assert!(
        key(&mut game, Key::Named(NamedKey::Right)),
        "the first arrow shows the cursor"
    );
    assert_eq!(game.cursor.map(|cursor| cursor.pile), Some(pile(8)));
    assert!(key(&mut game, Key::Named(NamedKey::Right)));
    assert_eq!(game.cursor.map(|cursor| cursor.pile), Some(pile(9)));
    assert!(key(&mut game, Key::Char(' ')));
    assert!(
        game.drag.as_ref().is_some_and(|drag| drag.glide.is_some()),
        "the hand is the keyboard's"
    );
    assert!(key(&mut game, Key::Named(NamedKey::Left)));
    assert_eq!(
        game.drag.as_ref().and_then(|drag| drag.target),
        Some(pile(8)),
        "the king takes it"
    );
    let _ = settle(&mut game, 0);
    let hover = game
        .drag
        .as_ref()
        .and_then(|drag| drag.held.first())
        .map(|&(_, at)| at);
    let landing = game.layout.landing(game.table().expect("a table"), pile(8));
    assert!(
        hover.is_some_and(|at| at.y < landing.top()),
        "it hovers above where it would land"
    );
    assert!(key(&mut game, Key::Named(NamedKey::Enter)));
    assert_eq!(cards(&game, pile(8)), "KS QH");
    assert!(game.drag.is_none());
}

#[test]
fn escape_puts_the_hand_back_and_then_hides_the_cursor() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    let before = game.table().map(Table::fingerprint);
    let _ = key(&mut game, Key::Named(NamedKey::Right));
    assert!(key(&mut game, Key::Char(' ')));
    assert!(key(&mut game, Key::Named(NamedKey::Escape)));
    assert!(game.drag.is_none() && game.cursor.is_some());
    assert_eq!(game.table().map(Table::fingerprint), before);
    assert!(key(&mut game, Key::Named(NamedKey::Escape)));
    assert!(game.cursor.is_none());
}

#[test]
fn the_cursor_climbs_only_over_the_run_that_moves() {
    let mut game = game_at(FREECELL, &[(8, "2D KS QH JC")]);
    let _ = key(&mut game, Key::Named(NamedKey::Down));
    assert_eq!(
        game.cursor,
        Some(Cursor {
            pile: pile(8),
            depth: Some(3)
        })
    );
    assert!(key(&mut game, Key::Named(NamedKey::Up)));
    assert!(key(&mut game, Key::Named(NamedKey::Up)));
    assert_eq!(
        game.cursor,
        Some(Cursor {
            pile: pile(8),
            depth: Some(1)
        })
    );
    assert!(
        key(&mut game, Key::Named(NamedKey::Up)),
        "past the run is the top row"
    );
    let on = game.cursor.map(|cursor| cursor.pile).expect("a cursor");
    assert!(!FREECELL.shape().tableau.contains(on));
    assert!(key(&mut game, Key::Named(NamedKey::Down)));
    assert_eq!(game.cursor.map(|cursor| cursor.pile), Some(pile(8)));
}

#[test]
fn enter_sends_the_cards_at_the_cursor_to_their_best_place() {
    let mut game = game_at(FREECELL, &[(8, "KS AH")]);
    let _ = key(&mut game, Key::Named(NamedKey::Down));
    assert!(key(&mut game, Key::Named(NamedKey::Enter)));
    assert_eq!(cards(&game, pile(4)), "AH");
    assert_eq!(
        game.cursor,
        Some(Cursor {
            pile: pile(8),
            depth: Some(0)
        }),
        "the cursor follows the column"
    );
}

#[test]
fn keys_name_the_commands() {
    let mut game = game_at(FREECELL, &[(8, "KS AH")]);
    let before = game.table().map(Table::fingerprint);
    let at = top_of(&game, pile(8));
    assert!(click(&mut game, at, 0));
    assert!(ctrl(&mut game, 'z'));
    assert_eq!(game.table().map(Table::fingerprint), before);
    assert!(ctrl(&mut game, 'y'));
    assert_ne!(game.table().map(Table::fingerprint), before);
    assert!(key(&mut game, Key::Named(NamedKey::Backspace)));
    assert_eq!(game.table().map(Table::fingerprint), before);
    assert!(key(&mut game, Key::Char('4')));
    assert_eq!(
        game.play().map(Play::variant),
        Some(Variant::Spider(crate::card::SpiderSuits::One))
    );
    assert_eq!(
        game.prefs().variant,
        Variant::Spider(crate::card::SpiderSuits::One)
    );
    assert!(!key(&mut game, Key::Char('9')), "no seventh game");
    assert!(!ctrl(&mut game, 'n'), "a modified letter is not the letter");
}

#[test]
fn a_typed_deal_number_deals_that_deal() {
    let mut game = game_at(FREECELL, &[(8, "KS")]);
    assert!(game.deal_entered("11,982", 0, &mut rng(), &mut Region::new()));
    assert_eq!(game.play().map(|play| play.number().get()), Some(11_982));
    assert!(!game.deal_entered("no", 0, &mut rng(), &mut Region::new()));
    assert_eq!(game.play().map(|play| play.number().get()), Some(11_982));
}

#[test]
fn the_pointer_hand_is_not_the_keyboard_s() {
    let mut game = game_at(FREECELL, &[(8, "KS"), (9, "QH")]);
    let before = game.table().map(Table::fingerprint);
    let from = top_of(&game, pile(9));
    point(&mut game, from, 0);
    let _ = press(&mut game, 0);
    point(&mut game, Point::new(from.x - 80, from.y), 0);
    assert!(!key(&mut game, Key::Char(' ')));
    assert!(!ctrl(&mut game, 'z'));
    assert!(game.drag.is_some());
    assert!(key(&mut game, Key::Named(NamedKey::Escape)));
    assert!(game.drag.is_none());
    assert_eq!(game.table().map(Table::fingerprint), before);
}

#[test]
fn any_press_ends_the_victory_and_says_the_game_is_won() {
    let mut game = one_short();
    let at = top_of(&game, pile(8));
    assert!(click(&mut game, at, 0));
    assert!(game.table().is_some_and(Table::won));
    assert!(game.motion.cascade().is_some());
    assert!(game.outbox().alert.is_none());
    assert!(press(&mut game, 0));
    assert!(game.motion.cascade().is_none());
    let (alert, _) = game.outbox().alert.take().expect("the won alert");
    assert_eq!(alert, Alerting::Won);
}

#[test]
fn the_best_place_for_cards() {
    let table = position(
        FREECELL,
        &[(8, "KS AH"), (9, "QD"), (10, "KD 5C"), (11, "TH 9S")],
    );
    assert_eq!(
        best_target(&table, pile(8), 1),
        Some(pile(4)),
        "an ace goes home"
    );
    assert_eq!(
        best_target(&table, pile(10), 1),
        Some(pile(12)),
        "an empty column before a cell"
    );
    assert_eq!(
        best_target(&table, pile(11), 2),
        None,
        "a whole column is not moved to an empty one"
    );
    let alone = position(FREECELL, &[(8, "KS")]);
    assert_eq!(
        best_target(&alone, pile(8), 1),
        Some(pile(0)),
        "a lone column's card is not moved to an empty one"
    );

    let spider = Variant::Spider(crate::card::SpiderSuits::Two);
    let table = position(spider, &[(9, "8H"), (10, "8S"), (11, "7S")]);
    assert_eq!(
        best_target(&table, pile(11), 1),
        Some(pile(10)),
        "a run of one suit first"
    );
    assert!(movable_from(&table, pile(11), 0));
    assert!(!movable_from(&table, pile(11), 1));
}

#[test]
fn a_click_never_takes_a_card_off_home() {
    let variant = Variant::Klondike(crate::variant::Draw::One);
    let mut game = game_at(variant, &[(2, "AS"), (6, "KH")]);
    let before = game.table().map(Table::fingerprint);
    let at = top_of(&game, pile(2));
    assert!(click(&mut game, at, 0), "the ace hops");
    assert_eq!(
        game.table().map(Table::fingerprint),
        before,
        "and stays home"
    );
    let table = game.table().expect("a table");
    assert!(
        table.allows(Action::Move {
            from: pile(2),
            to: pile(3),
            count: 1
        }),
        "though the rules would allow it"
    );
}
