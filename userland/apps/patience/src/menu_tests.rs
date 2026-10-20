//! Menu tests: every row a menu carries names a command, the two menus agree,
//! nothing the builder was given is dropped, and a typed deal number is read
//! as a person types one.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{AppMenuMark, AppMenuRowView};

use super::*;
use crate::stats::{Outcome, Tally};

/// Each chooseable row's id, enabled state and mark, in order.
fn items(menu: &AppMenu) -> Vec<(u16, bool, AppMenuMark)> {
    menu.rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.id.get(), item.enabled, item.mark)),
            _ => None,
        })
        .collect()
}

fn entry(menu: &AppMenu) -> Option<(u16, String)> {
    menu.rows().find_map(|(row, _)| match row {
        AppMenuRowView::Item(item) => item
            .entry
            .map(|entry| (entry.id.get(), String::from(entry.initial))),
        _ => None,
    })
}

const ALL_OFFERED: [Offer; 4] = [Offer::Undo, Offer::Redo, Offer::Rewind, Offer::Finish];

fn offering(offers: &[Offer]) -> Offers {
    offers
        .iter()
        .fold(Offers::default(), |all, &offer| all.with(offer, true))
}

#[test]
fn every_row_id_names_one_command() {
    let mut seen = Vec::new();
    for id in NEW_GAME..DEAL_ENTRY {
        let command = Command::of_row(id).expect("a command");
        assert!(!seen.contains(&command), "{command:?} named twice");
        seen.push(command);
    }
    for id in [0, QUIT_ROW, DEAL_ENTRY, DEAL_ROW, DEAL_ROW + 1, u16::MAX] {
        assert_eq!(Command::of_row(id), None, "{id}");
    }
}

#[test]
fn every_row_of_both_menus_is_carried() {
    let prefs = Preferences::default();
    let bar = menu(&prefs, offering(&ALL_OFFERED), None);
    let table = menu(&prefs, offering(&ALL_OFFERED), Some(42));
    let commands = usize::from(DEAL_ENTRY - NEW_GAME);
    // The bar adds Quit; the table adds the row carrying the deal's field.
    assert_eq!(items(&bar).len(), commands + 1);
    assert_eq!(items(&table).len(), commands + 1);
    assert!(bar
        .rows()
        .any(|(row, _)| matches!(row, AppMenuRowView::Info)));
    assert!(!table
        .rows()
        .any(|(row, _)| matches!(row, AppMenuRowView::Info)));
    assert!(items(&bar).iter().any(|&(id, ..)| id == QUIT_ROW));
    assert!(!items(&table).iter().any(|&(id, ..)| id == QUIT_ROW));
    let chooseable = |menu: &AppMenu| -> Vec<u16> {
        items(menu)
            .into_iter()
            .map(|(id, ..)| id)
            .filter(|&id| Command::of_row(id).is_some())
            .collect()
    };
    assert_eq!(
        chooseable(&bar),
        chooseable(&table),
        "the two menus offer the same commands"
    );
}

#[test]
fn the_table_menu_carries_the_deal_in_play() {
    let menu = menu(&Preferences::default(), Offers::default(), Some(31_337));
    assert_eq!(entry(&menu), Some((DEAL_ENTRY, String::from("31337"))));
    let bar = super::menu(&Preferences::default(), Offers::default(), None);
    assert_eq!(entry(&bar), None);
}

#[test]
fn rows_the_game_does_not_allow_now_are_disabled() {
    let prefs = Preferences::default();
    let enabled = |menu: &AppMenu, id: u16| {
        items(menu)
            .iter()
            .find(|&&(row, ..)| row == id)
            .map(|&(_, on, _)| on)
    };
    let none = menu(&prefs, Offers::default(), None);
    for id in [UNDO, REDO, REWIND, FINISH] {
        assert_eq!(enabled(&none, id), Some(false), "{id}");
    }
    assert_eq!(enabled(&none, NEW_GAME), Some(true));
    for (offer, id) in ALL_OFFERED.into_iter().zip([UNDO, REDO, REWIND, FINISH]) {
        let one = menu(&prefs, offering(&[offer]), None);
        assert_eq!(enabled(&one, id), Some(true), "{offer:?}");
        let others = [UNDO, REDO, REWIND, FINISH]
            .into_iter()
            .filter(|&other| other != id);
        for other in others {
            assert_eq!(enabled(&one, other), Some(false), "{offer:?} alone");
        }
    }
}

#[test]
fn the_chosen_settings_are_marked() {
    let prefs = Preferences {
        variant: Variant::FreeCell,
        winnable: false,
        four_colours: true,
        ..Preferences::default()
    };
    let rows = items(&menu(&prefs, Offers::default(), None));
    let mark = |id: u16| {
        rows.iter()
            .find(|&&(row, ..)| row == id)
            .map(|&(.., mark)| mark)
    };
    let chosen = |id: u16| mark(id).is_some_and(|mark| mark != AppMenuMark::None);
    let freecell = FIRST_VARIANT + u16::try_from(Variant::FreeCell.index()).expect("small");
    assert!(chosen(freecell));
    assert!(!chosen(FIRST_VARIANT), "one game chosen at a time");
    assert!(!chosen(WINNABLE));
    assert!(chosen(FOUR_COLOURS));
    assert!(chosen(AUTO_HOME));
}

#[test]
fn an_offer_is_made_and_withdrawn_alone() {
    let offers = Offers::default()
        .with(Offer::Redo, true)
        .with(Offer::Finish, true);
    assert!(offers.has(Offer::Redo) && offers.has(Offer::Finish));
    assert!(!offers.has(Offer::Undo) && !offers.has(Offer::Rewind));
    let withdrawn = offers.with(Offer::Redo, false);
    assert!(!withdrawn.has(Offer::Redo) && withdrawn.has(Offer::Finish));
}

#[test]
fn a_deal_number_is_read_as_typed() {
    let klondike = Variant::Klondike(crate::variant::Draw::One);
    let read =
        |variant: Variant, text: &str| deal_number(variant, text).map(crate::deal::DealNumber::get);
    assert_eq!(read(klondike, "42"), Some(42));
    assert_eq!(read(klondike, " #1,234,567 "), Some(1_234_567));
    assert_eq!(read(klondike, "1 000"), Some(1000));
    assert_eq!(read(klondike, ""), None);
    assert_eq!(read(klondike, "  "), None);
    assert_eq!(read(klondike, "0"), None);
    assert_eq!(read(klondike, "12a"), None);
    assert_eq!(read(klondike, "-5"), None);
    assert_eq!(
        read(klondike, "99999999999999"),
        None,
        "an overflowing number is refused"
    );
    assert_eq!(read(Variant::FreeCell, "1000000"), Some(1_000_000));
    assert_eq!(read(Variant::FreeCell, "1000001"), None);
}

#[test]
fn durations_and_scores_read_as_a_clock_and_money() {
    assert_eq!(clock(0), "0:00");
    assert_eq!(clock(59), "0:59");
    assert_eq!(clock(61), "1:01");
    assert_eq!(clock(3600), "1:00:00");
    assert_eq!(clock(3 * 3600 + 7 * 60 + 9), "3:07:09");
    assert_eq!(score_text(Score::Points(120)), "120 points");
    assert_eq!(score_text(Score::Dollars(52)), "$52");
    assert_eq!(score_text(Score::Dollars(-52)), "\u{2212}$52");
}

#[test]
fn every_alert_can_be_put_up() {
    let mut stats = Stats::default();
    for variant in Variant::ALL {
        assert!(statistics(variant, &stats).is_some(), "{variant:?}");
        assert!(forget(variant).is_some(), "{variant:?}");
        assert!(
            won(variant, 125, 98, Some(Score::Points(640)), &stats).is_some(),
            "{variant:?}"
        );
    }
    for won_it in [true, false, true, true] {
        let _ = stats.apply(Tally::Add(Outcome {
            variant: Variant::FreeCell,
            won: won_it,
            secs: 300,
            moves: 120,
            score: None,
            banked: None,
        }));
    }
    assert!(statistics(Variant::FreeCell, &stats).is_some());
    let lines = record_lines(Variant::FreeCell, stats.record(Variant::FreeCell), None);
    assert!(lines.starts_with("Played 4 \u{b7} won 3 (75%)"), "{lines}");
    assert!(lines.contains("Current run: 2 wins"), "{lines}");
    assert!(lines.contains("Fastest win: 5:00"), "{lines}");
}
