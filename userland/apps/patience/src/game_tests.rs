//! Game tests: a deal, a move, a win and a lost game each leave the game, its
//! records and its requests as they should, and a game with nothing moving
//! owes the loop no wake at all.

use alloc::vec::Vec;

use super::*;
use crate::fixture::{plain, resumed, rng, settle, setup};
use crate::motion::GLOW_NS;
use crate::score::Scoring;
use crate::solver::Solver;
use crate::variant::Draw;

const KLONDIKE: Variant = Variant::Klondike(Draw::One);

/// The first move the rules allow onto a column.
fn a_move(table: &Table) -> Action {
    let mut actions = Vec::new();
    table.actions(&mut actions);
    actions
        .into_iter()
        .find(|action| matches!(action, Action::Move { .. }))
        .expect("a move")
}

/// FreeCell's first deal, played to one move short of the end.
fn nearly_won() -> (Play, Action) {
    let rules = Scoring::Standard.rules(Variant::FreeCell);
    let number = DealNumber::new(Variant::FreeCell, 1).expect("a deal");
    let (mut play, _) = Play::deal(rules, Scoring::Standard, number);
    let Verdict::Won(mut line) = Solver::new().solve(play.table(), HINT_BUDGET) else {
        panic!("deal 1 is won");
    };
    let last = line.pop().expect("a line");
    for action in line {
        assert!(play.act(action).is_some());
    }
    (play, last)
}

#[test]
fn a_new_game_deals_at_once_and_then_owes_no_wake() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    assert!(game.table().is_some());
    assert!(game.deadline_ns(0).is_some(), "the deal is animated");
    let rested = settle(&mut game, 0);
    assert_eq!(
        game.deadline_ns(rested),
        None,
        "no move yet, so not even the clock runs"
    );
    assert!(matches!(
        game.outbox().question,
        Some(Question::Position { .. })
    ));
}

#[test]
fn a_winnable_deal_waits_for_the_search_and_the_next_is_found_ahead() {
    let prefs = Preferences {
        winnable: true,
        ..plain(KLONDIKE)
    };
    let mut game = Game::new(setup(prefs, None), 0, &mut rng());
    assert!(game.table().is_none());
    assert_eq!(game.note.map(|(note, _)| note), Some(Note::Shuffling));
    let search = game.outbox().deal.take().expect("a search");
    let number = DealNumber::new(KLONDIKE, 77).expect("a deal");
    let found = DealFound {
        rules: search.rules,
        number,
        proven: true,
    };
    game.dealt(found, 0, &mut rng(), &mut Region::new());
    assert_eq!(game.play().map(Play::number), Some(number));
    assert!(game.proven && game.note.is_none());
    let ahead = game
        .outbox()
        .deal
        .take()
        .expect("the next is looked for at once");
    let next = DealNumber::new(KLONDIKE, 78).expect("a deal");
    game.dealt(
        DealFound {
            rules: ahead.rules,
            number: next,
            proven: true,
        },
        0,
        &mut rng(),
        &mut Region::new(),
    );
    assert_eq!(
        game.play().map(Play::number),
        Some(number),
        "a deal found ahead waits its turn"
    );
    game.new_game(KLONDIKE, None, 0, &mut rng(), &mut Region::new());
    assert_eq!(
        game.play().map(Play::number),
        Some(next),
        "and is dealt at once"
    );
}

#[test]
fn a_move_repaints_asks_the_solver_and_keeps_the_game() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    let now = settle(&mut game, 0);
    game.outbox().question = None;
    let action = a_move(game.table().expect("a table"));
    let mut damage = Region::new();
    assert!(game.act(action, Pace::Slide, &[], now, &mut damage));
    assert!(!damage.is_empty());
    let Some(Question::Position { table, .. }) = game.outbox().question.take() else {
        panic!("the solver is asked about the new position");
    };
    assert_eq!(Some(table.fingerprint()), game.fingerprint());
    let due = now + AUTOSAVE_NS;
    assert_eq!(game.autosave_ns, Some(due), "kept once the moves stop");
    assert!(game.deadline_ns(now).is_some_and(|wake| wake <= due));
    let _ = game.tick(due, &mut Region::new());
    assert_eq!(game.autosave_ns, None);
    let job = game.store_job().expect("a write");
    assert!(job.saved.is_some_and(|saved| saved != Saved::default()));
}

#[test]
fn the_clock_runs_from_the_first_move_and_wakes_on_each_second() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    let now = settle(&mut game, 0);
    let action = a_move(game.table().expect("a table"));
    assert!(game.act(action, Pace::Slide, &[], now, &mut Region::new()));
    let rested = settle(&mut game, now);
    let wake = game
        .deadline_ns(rested)
        .expect("the clock owes its next second");
    assert!(wake <= now + SECOND_NS);
    assert_eq!(game.secs(now + 3 * SECOND_NS + 1), 3);
    game.set_visible(false, now + 3 * SECOND_NS);
    assert_eq!(
        game.secs(now + 90 * SECOND_NS),
        3,
        "a hidden game's clock stands"
    );
    assert!(
        game.store_job().is_some_and(|job| job.saved.is_some()),
        "and the game is kept"
    );
    game.set_visible(true, now + 90 * SECOND_NS);
    assert_eq!(game.secs(now + 92 * SECOND_NS), 5);
}

#[test]
fn dealing_again_concedes_a_started_game_but_not_an_unstarted_one() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    game.new_game(KLONDIKE, None, 0, &mut rng(), &mut Region::new());
    assert_eq!(game.stats().record(KLONDIKE).played, 0);
    let action = a_move(game.table().expect("a table"));
    assert!(game.act(action, Pace::Slide, &[], 0, &mut Region::new()));
    game.new_game(KLONDIKE, None, SECOND_NS, &mut rng(), &mut Region::new());
    let record = game.stats().record(KLONDIKE);
    assert_eq!((record.played, record.won, record.streak), (1, 0, -1));
}

#[test]
fn undo_and_redo_go_back_and_forth_over_a_move() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    let before = game.fingerprint();
    let action = a_move(game.table().expect("a table"));
    assert!(game.act(action, Pace::Slide, &[], 0, &mut Region::new()));
    let after = game.fingerprint();
    assert_ne!(before, after);
    assert!(game.offers().has(Offer::Undo) && !game.offers().has(Offer::Redo));
    assert!(game.undo(0, &mut Region::new()));
    assert_eq!(game.fingerprint(), before);
    assert!(game.offers().has(Offer::Redo));
    assert!(game.redo(0, &mut Region::new()));
    assert_eq!(game.fingerprint(), after);
    assert!(
        !game.redo(0, &mut Region::new()),
        "nothing more to make again"
    );
}

#[test]
fn a_known_line_answers_a_hint_and_a_stale_answer_changes_nothing() {
    let mut game = Game::new(setup(plain(Variant::FreeCell), None), 0, &mut rng());
    let now = settle(&mut game, 0);
    let fingerprint = game.fingerprint().expect("a table");
    let line = alloc::vec![a_move(game.table().expect("a table"))];
    let stale = Finding::Position {
        fingerprint: fingerprint ^ 1,
        verdict: Verdict::Lost,
    };
    game.answered(stale, now, &mut Region::new());
    assert!(game.lost.is_none() && game.known.is_none());
    game.answered(
        Finding::Position {
            fingerprint,
            verdict: Verdict::Won(line),
        },
        now,
        &mut Region::new(),
    );
    game.hint(now, &mut Region::new());
    assert!(
        game.motion.lit(now + GLOW_NS / 4).is_some(),
        "the hint is lit"
    );
}

#[test]
fn a_hint_asked_before_the_answer_is_shown_when_it_comes() {
    let mut game = Game::new(setup(plain(Variant::FreeCell), None), 0, &mut rng());
    let now = settle(&mut game, 0);
    game.hint(now, &mut Region::new());
    assert_eq!(game.note.map(|(note, _)| note), Some(Note::Thinking));
    assert!(matches!(
        game.outbox().question,
        Some(Question::Position {
            budget: HINT_BUDGET,
            ..
        })
    ));
    let fingerprint = game.fingerprint().expect("a table");
    let line = alloc::vec![a_move(game.table().expect("a table"))];
    game.answered(
        Finding::Position {
            fingerprint,
            verdict: Verdict::Won(line),
        },
        now,
        &mut Region::new(),
    );
    assert!(game.note.is_none());
    assert!(game.motion.lit(now + GLOW_NS / 4).is_some());
}

#[test]
fn a_lost_game_offers_the_way_back_to_the_last_winnable_position() {
    let mut game = Game::new(setup(plain(Variant::FreeCell), None), 0, &mut rng());
    for _ in 0..3 {
        let action = a_move(game.table().expect("a table"));
        assert!(game.act(action, Pace::Slide, &[], 0, &mut Region::new()));
    }
    let fingerprint = game.fingerprint().expect("a table");
    game.answered(
        Finding::Position {
            fingerprint,
            verdict: Verdict::Lost,
        },
        0,
        &mut Region::new(),
    );
    assert!(game.offers().has(Offer::Rewind));
    assert_eq!(game.note.map(|(note, _)| note), Some(Note::Lost));
    game.rewind(0, &mut Region::new());
    let Some(Question::Rewind { line, winnable, .. }) = game.outbox().question.take() else {
        panic!("the rewind is asked for");
    };
    assert_eq!((line.len(), winnable), (3, false));
    game.answered(
        Finding::Rewind {
            fingerprint,
            keep: Some(1),
        },
        0,
        &mut Region::new(),
    );
    assert_eq!(game.play().map(Play::moves), Some(1));
    assert!(!game.offers().has(Offer::Rewind));
}

#[test]
fn winning_counts_the_game_and_ends_on_the_alert() {
    for reduced in [false, true] {
        let (play, last) = nearly_won();
        let mut game = resumed(plain(Variant::FreeCell), play);
        game.set_reduced_motion(reduced, &mut Region::new());
        assert!(game.act(last, Pace::Slide, &[], 0, &mut Region::new()));
        let record = game.stats().record(Variant::FreeCell);
        assert_eq!((record.played, record.won), (1, 1));
        assert!(
            game.store_job()
                .is_some_and(|job| job.saved == Some(Saved::default())),
            "a won game is not kept"
        );
        assert_eq!(
            game.motion.cascade().is_some(),
            !reduced,
            "reduced motion celebrates nothing"
        );
        if !reduced {
            assert!(
                game.outbox().alert.is_none(),
                "the alert waits for the victory"
            );
            let ended = settle(&mut game, 0);
            assert!(game.motion.cascade().is_none());
            assert_eq!(
                game.deadline_ns(ended),
                None,
                "a won game's clock has stopped"
            );
        }
        let (alert, _) = game.outbox().alert.take().expect("the won alert");
        assert_eq!(alert, Alerting::Won);
        assert!(
            !game.undo(0, &mut Region::new()),
            "a won game cannot be taken back"
        );
    }
}

#[test]
fn the_won_alert_deals_again() {
    let (play, last) = nearly_won();
    let mut game = resumed(plain(Variant::FreeCell), play);
    game.set_reduced_motion(true, &mut Region::new());
    assert!(game.act(last, Pace::Slide, &[], 0, &mut Region::new()));
    let number = game.play().map(Play::number);
    game.alert_answered(Alerting::Won, Some(0), 0, &mut rng(), &mut Region::new());
    assert_ne!(game.play().map(Play::number), number);
    assert_eq!(game.play().map(Play::moves), Some(0));
}

#[test]
fn forgetting_a_record_asks_first() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    let action = a_move(game.table().expect("a table"));
    assert!(game.act(action, Pace::Slide, &[], 0, &mut Region::new()));
    game.new_game(KLONDIKE, None, 0, &mut rng(), &mut Region::new());
    assert_eq!(game.stats().record(KLONDIKE).played, 1);
    game.show_statistics();
    let (alert, _) = game.outbox().alert.take().expect("the statistics");
    assert_eq!(alert, Alerting::Statistics);
    game.alert_answered(
        Alerting::Statistics,
        Some(1),
        0,
        &mut rng(),
        &mut Region::new(),
    );
    let (alert, _) = game.outbox().alert.take().expect("the question");
    assert_eq!(alert, Alerting::Forget);
    game.alert_answered(Alerting::Forget, None, 0, &mut rng(), &mut Region::new());
    assert_eq!(
        game.stats().record(KLONDIKE).played,
        1,
        "dismissing keeps it"
    );
    game.alert_answered(Alerting::Forget, Some(0), 0, &mut rng(), &mut Region::new());
    assert_eq!(game.stats().record(KLONDIKE).played, 0);
}

#[test]
fn a_resumed_game_stands_where_it_was_left_with_its_clock() {
    let rules = Scoring::Standard.rules(KLONDIKE);
    let number = DealNumber::new(KLONDIKE, 9).expect("a deal");
    let (mut play, _) = Play::deal(rules, Scoring::Standard, number);
    let action = a_move(play.table());
    assert!(play.act(action).is_some());
    let fingerprint = play.table().fingerprint();
    let game = Game::new(setup(plain(KLONDIKE), Some((play, 125))), 0, &mut rng());
    assert_eq!(game.fingerprint(), Some(fingerprint));
    assert_eq!(game.secs(2 * SECOND_NS), 127);
    assert_eq!(
        game.deadline_ns(0).map(|due| due <= SECOND_NS),
        Some(true),
        "nothing animates a resume"
    );
    assert!(game.motion.airborne().is_empty());
}

#[test]
fn a_new_layout_repaints_everything_and_lets_go_of_the_hand() {
    let mut game = Game::new(setup(plain(KLONDIKE), None), 0, &mut rng());
    game.drag = Some(Drag {
        pile: game
            .layout
            .variant()
            .shape()
            .tableau
            .nth(0)
            .expect("a column"),
        depth: 0,
        grab: Point::ORIGIN,
        held: Vec::new(),
        target: None,
        glide: None,
    });
    let larger = Rect::new(0, 0, 1400, 1000);
    let mut damage = Region::new();
    game.relayout(larger, Scale::ONE, &mut damage);
    assert_eq!(game.layout.client, larger);
    assert!(damage.intersects(larger) && damage.bounds() == larger);
    assert!(game.drag.is_none());
    assert!(game.motion.airborne().is_empty(), "a resize is not a move");
}

#[test]
fn turning_auto_home_on_sends_what_is_safe_home() {
    let table =
        crate::fixture::position(Variant::FreeCell, &[(8, "KS AH"), (9, "QD 2H"), (10, "AS")]);
    let play = Play::laid(
        table,
        Scoring::Standard,
        DealNumber::new(Variant::FreeCell, 1).expect("a deal"),
    );
    let mut game = resumed(plain(Variant::FreeCell), play);
    let prefs = Preferences {
        auto_home: true,
        ..*game.prefs()
    };
    game.choose(prefs, 0, &mut Region::new());
    assert_eq!(
        game.table().map(Table::home),
        Some(3),
        "both aces and the two of hearts"
    );
    assert!(game.outbox().menu_moved);
}

#[test]
fn the_status_line_says_the_game_the_deal_and_the_readings() {
    let mut game = Game::new(setup(plain(Variant::FreeCell), None), 0, &mut rng());
    game.refresh_status(0);
    let number = game.play().map(|play| play.number().get()).expect("a deal");
    assert_eq!(
        game.status.game,
        alloc::format!("FreeCell \u{b7} deal {number}")
    );
    assert_eq!(game.status.readings, "0 moves   0:00");
    game.proven = true;
    game.refresh_status(0);
    assert!(
        game.status.game.ends_with('\u{2713}'),
        "a proven deal is marked"
    );
}
