//! Preference store tests over the shared fake app-data service.

use tairix_appconf::Keys;
use tairix_appdata::fake::FakeService;
use tairix_appdata::Settings;

use super::*;
use crate::card::SpiderSuits;

fn holding(text: &str) -> FakeService {
    FakeService::for_word("patience").with_store(text)
}

#[test]
fn a_new_player_plays_draw_one_with_winnable_deals_and_cards_sent_home() {
    let prefs = Preferences::default();
    assert_eq!(prefs.variant, Variant::Klondike(Draw::One));
    assert!(prefs.winnable && prefs.auto_home && !prefs.four_colours);
    let mut host = FakeService::for_word("patience");
    let (loaded, refused) = Preferences::load(&Settings::open_without_defaults(&mut host));
    assert_eq!((loaded, refused.len()), (prefs, 0));
}

#[test]
fn every_choice_survives_a_round_trip() {
    let mut host = FakeService::for_word("patience");
    let chosen = Preferences {
        variant: Variant::Spider(SpiderSuits::Two),
        scoring: Scoring::Cumulative,
        winnable: false,
        auto_home: false,
        four_colours: true,
        back: Back::Graphite,
        baize: Baize::Desktop,
    };
    let mut store = Settings::open_without_defaults(&mut host);
    tairix_appdata::save(&chosen, Keys::ALL, &mut store).expect("commits");
    drop(store);
    let (loaded, refused) = Preferences::load(&Settings::open_without_defaults(&mut host));
    assert!(refused.is_empty());
    assert_eq!(loaded, chosen);
}

#[test]
fn a_value_it_does_not_know_is_refused_and_named_and_costs_only_itself() {
    let mut host = holding("game = pyramid\ncards.back = crimson\nplay.auto-home = maybe\n");
    let (loaded, refused) = Preferences::load(&Settings::open_without_defaults(&mut host));
    assert_eq!(refused, [Pref::Variant, Pref::AutoHome]);
    assert_eq!(loaded.back, Back::Crimson);
    assert_eq!(loaded.variant, Preferences::default().variant);
}

#[test]
fn one_choice_written_leaves_the_others_as_stored() {
    let mut host = holding("cards.back = emerald\ntable.baize = claret\n");
    let mut store = Settings::open_without_defaults(&mut host);
    let chosen = Preferences {
        four_colours: true,
        ..Preferences::default()
    };
    tairix_appdata::save(&chosen, Keys::of(Pref::FourColours), &mut store).expect("commits");
    drop(store);
    let document = host.committed();
    assert_eq!(document.get("cards.four-colour"), Some("true"));
    assert_eq!(document.get("cards.back"), Some("emerald"));
    assert_eq!(document.get("table.baize"), Some("claret"));
}

#[test]
fn every_back_and_table_is_spelled_once() {
    for back in Back::ALL {
        let mut prefs = Preferences::default();
        assert!(prefs.read(Pref::Back, back.name()));
        assert_eq!(prefs.back, back);
    }
    for baize in Baize::ALL {
        let mut prefs = Preferences::default();
        assert!(prefs.read(Pref::Baize, baize.name()));
        assert_eq!(prefs.baize, baize);
    }
}
