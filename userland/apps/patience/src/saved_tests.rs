//! Kept-game tests.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_appconf::Keys;
use tairix_appdata::fake::FakeService;
use tairix_appdata::Settings;

use super::*;
use crate::table::Action;
use crate::variant::Draw;

fn played(variant: Variant, steps: usize) -> Play {
    let number = DealNumber::new(variant, 19).expect("a deal");
    let (mut play, _) = Play::deal(Scoring::Standard.rules(variant), Scoring::Standard, number);
    let mut actions = Vec::new();
    for _ in 0..steps {
        play.table().actions(&mut actions);
        let Some(&action) = actions.iter().find(|a| !matches!(a, Action::Recycle)) else {
            break;
        };
        play.act(action).expect("an offered action");
    }
    play
}

fn kept_through(host: &mut FakeService, saved: &Saved) -> Saved {
    let mut store = Settings::open_without_defaults(host);
    tairix_appdata::save(saved, Keys::ALL, &mut store).expect("commits");
    drop(store);
    let (loaded, refused) = Saved::load(&Settings::open_without_defaults(host));
    assert!(refused.is_empty(), "{refused:?}");
    loaded
}

#[test]
fn a_kept_game_resumes_exactly_where_it_was() {
    let mut host = FakeService::for_word("patience");
    let play = played(Variant::Klondike(Draw::Three), 40);
    let saved = Saved::of(&play, 125).expect("short enough to keep");
    let loaded = kept_through(&mut host, &saved);
    let (resumed, secs) = loaded.resume().expect("a game");
    assert_eq!(secs, 125);
    assert_eq!(resumed.table(), play.table());
    assert_eq!(resumed.moves(), play.moves());
}

#[test]
fn a_long_line_spans_several_keys_and_comes_back_whole() {
    let mut host = FakeService::for_word("patience");
    let play = played(Variant::FreeCell, 900);
    let saved = Saved::of(&play, 1).expect("short enough to keep");
    let loaded = kept_through(&mut host, &saved);
    assert!(
        host.committed().get("saved.line.1").is_some(),
        "more than one chunk"
    );
    let (resumed, _) = loaded.resume().expect("a game");
    assert_eq!(resumed.table(), play.table());
}

#[test]
fn forgetting_the_kept_game_removes_every_part_of_it() {
    let mut host = FakeService::for_word("patience");
    let play = played(Variant::FreeCell, 900);
    let _ = kept_through(&mut host, &Saved::of(&play, 1).expect("kept"));
    let forgotten = kept_through(&mut host, &Saved::default());
    assert!(!forgotten.holds());
    assert_eq!(host.committed().settings().count(), 0);
}

#[test]
fn a_line_missing_a_chunk_is_not_spliced_and_does_not_resume() {
    let line = String::from("8a1");
    let mut host = FakeService::for_word("patience").with_store(&alloc::format!(
        "saved.game = freecell\nsaved.scoring = standard\nsaved.deal = 1\n\
         saved.clock = 3\nsaved.line.1 = {line}\n"
    ));
    let (loaded, refused) = Saved::load(&Settings::open_without_defaults(&mut host));
    assert_eq!(refused, [Part::Line(1)]);
    let (resumed, _) = loaded.resume().expect("the empty line is still the deal");
    assert_eq!(resumed.moves(), 0);
}

#[test]
fn text_that_is_not_a_line_or_not_a_legal_one_is_refused() {
    let mut host = FakeService::for_word("patience")
        .with_store("saved.game = freecell\nsaved.deal = 1\nsaved.line.0 = 8A1\n");
    let (loaded, refused) = Saved::load(&Settings::open_without_defaults(&mut host));
    assert_eq!(
        refused,
        [Part::Line(0)],
        "upper case is not how a line is spelled"
    );
    assert!(loaded.resume().is_some());
    let mut illegal = FakeService::for_word("patience")
        .with_store("saved.game = freecell\nsaved.deal = 1\nsaved.line.0 = 0f1\n");
    let (loaded, _) = Saved::load(&Settings::open_without_defaults(&mut illegal));
    assert!(
        loaded.resume().is_none(),
        "a move the rules refuse is not a game"
    );
}

#[test]
fn nothing_kept_resumes_nothing() {
    assert!(Saved::default().resume().is_none());
    let mut host = FakeService::for_word("patience").with_store("saved.deal = 4\n");
    let (loaded, _) = Saved::load(&Settings::open_without_defaults(&mut host));
    assert!(loaded.resume().is_none(), "a deal of no game");
}
