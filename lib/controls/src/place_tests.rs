//! The place chooser: what a query shows, how the highlight walks, what a
//! click and a double-click answer, and what each change reports.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::damage;
use crate::place::{PlaceAction, PlaceChooser, PlaceRow, PressTiming, NO_MATCH};
use crate::state::ControlState;
use crate::testkit::high_contrast;

const W: u32 = 360;
const H: u32 = 320;

fn bounds() -> Rect {
    Rect::new(0, 0, W, H)
}

fn row(name: &str, region: &str, key: &str) -> PlaceRow {
    PlaceRow {
        name: name.to_string(),
        region: region.to_string(),
        reading: "14:02 · UTC+01:00".to_string(),
        key: key.to_string(),
    }
}

/// Places in name order, as an owner hands them over.
pub(crate) fn chooser() -> PlaceChooser {
    let rows = [
        ("Auckland", "New Zealand", "Pacific/Auckland"),
        ("Barcelona", "Spain", "Europe/Madrid"),
        ("Berlin", "Germany", "Europe/Berlin"),
        ("Curaçao", "Curaçao", "America/Curacao"),
        ("Kolkata", "India", "Asia/Kolkata"),
        ("London", "United Kingdom", "Europe/London"),
        ("Londonderry", "United Kingdom", "Europe/London"),
        ("New York", "United States, Eastern", "America/New_York"),
        ("Paris", "France", "Europe/Paris"),
        ("São Paulo", "Brazil", "America/Sao_Paulo"),
        ("Tokyo", "Japan", "Asia/Tokyo"),
        ("Zürich", "Switzerland", "Europe/Zurich"),
    ]
    .into_iter()
    .map(|(name, region, key)| row(name, region, key))
    .collect();
    PlaceChooser::new(rows).with_placeholder("Find a city")
}

fn names(chooser: &PlaceChooser) -> Vec<&str> {
    chooser
        .shown()
        .iter()
        .map(|&row| chooser.rows()[row].name.as_str())
        .collect()
}

fn focused() -> PlaceChooser {
    let mut chooser = chooser();
    chooser.set_focused(true);
    chooser
}

fn key(chooser: &mut PlaceChooser, key: Key) -> Option<PlaceAction> {
    let theme = Theme::dark();
    chooser.on_key(
        key,
        Modifiers::default(),
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage::sink(),
    )
}

fn named(chooser: &mut PlaceChooser, named: NamedKey) -> Option<PlaceAction> {
    key(chooser, Key::Named(named))
}

fn typed(chooser: &mut PlaceChooser, text: &str) -> Option<PlaceAction> {
    let mut last = None;
    for ch in text.chars() {
        last = key(chooser, Key::Char(ch));
    }
    last
}

const INTERVAL: Duration64 = Duration64::from_millis(400);

fn at(ms: u64) -> PressTiming {
    PressTiming {
        now_ns: ms * 1_000_000,
        double_click: INTERVAL,
    }
}

fn pointer(chooser: &mut PlaceChooser, event: &InputEvent, ms: u64) -> Option<PlaceAction> {
    let theme = Theme::dark();
    chooser.on_pointer(
        event,
        at(ms),
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage::sink(),
    )
}

/// The window point at the middle of the `index`th row shown, unscrolled.
fn row_point(index: u32) -> Point {
    let theme = Theme::dark();
    let height = PlaceChooser::row_height(Scale::ONE, &theme);
    let top = PlaceChooser::height_for(0, Scale::ONE, &theme);
    Point::new(
        40,
        i32::try_from(top + height * index + height / 2).expect("fits"),
    )
}

fn press_at(chooser: &mut PlaceChooser, point: Point, ms: u64) -> Option<PlaceAction> {
    pointer(chooser, &InputEvent::PointerMoved { to: point }, ms);
    let answer = pointer(
        chooser,
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        ms,
    );
    pointer(
        chooser,
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        ms,
    );
    answer
}

#[test]
fn an_empty_query_shows_every_place_in_the_owners_order() {
    let chooser = chooser();
    assert_eq!(chooser.shown().len(), chooser.rows().len());
    assert_eq!(names(&chooser)[0], "Auckland");
    assert_eq!(chooser.highlighted(), None);
}

#[test]
fn a_query_matches_without_case_or_accent_and_names_that_begin_with_it_lead() {
    let mut chooser = focused();
    assert_eq!(typed(&mut chooser, "LON"), Some(PlaceAction::Searched));
    assert_eq!(names(&chooser), ["London", "Londonderry", "Barcelona"]);
    let mut chooser = focused();
    typed(&mut chooser, "sao");
    assert_eq!(names(&chooser), ["São Paulo"]);
    let mut chooser = focused();
    typed(&mut chooser, "zur");
    assert_eq!(names(&chooser), ["Zürich"]);
    let mut chooser = focused();
    typed(&mut chooser, "curacao");
    assert_eq!(names(&chooser), ["Curaçao"]);
}

#[test]
fn a_query_finds_a_place_by_its_region_and_by_its_unseen_key() {
    let mut chooser = focused();
    typed(&mut chooser, "kingdom");
    assert_eq!(names(&chooser), ["London", "Londonderry"]);
    let mut chooser = focused();
    typed(&mut chooser, "asia/");
    assert_eq!(names(&chooser), ["Kolkata", "Tokyo"]);
}

#[test]
fn the_first_match_is_highlighted_and_a_highlight_that_still_shows_is_kept() {
    let mut chooser = focused();
    typed(&mut chooser, "l");
    assert_eq!(chooser.highlighted(), chooser.shown().first().copied());
    named(&mut chooser, NamedKey::Down);
    let kept = chooser.highlighted();
    typed(&mut chooser, "o");
    assert_eq!(chooser.highlighted(), kept, "Londonderry still matches");
}

#[test]
fn a_query_that_matches_nothing_still_edits() {
    let mut chooser = focused();
    typed(&mut chooser, "qqq");
    assert!(chooser.shown().is_empty());
    assert_eq!(chooser.highlighted(), None);
    assert_eq!(named(&mut chooser, NamedKey::Down), None);
    assert_eq!(named(&mut chooser, NamedKey::Enter), None);
    assert_eq!(
        named(&mut chooser, NamedKey::Backspace),
        Some(PlaceAction::Searched)
    );
    assert_eq!(chooser.query(), "qq");
}

#[test]
fn the_arrows_walk_the_highlight_and_stop_at_either_end() {
    let mut chooser = focused();
    assert_eq!(
        named(&mut chooser, NamedKey::Down),
        Some(PlaceAction::Moved { row: 0 })
    );
    assert_eq!(
        named(&mut chooser, NamedKey::Down),
        Some(PlaceAction::Moved { row: 1 })
    );
    assert_eq!(
        named(&mut chooser, NamedKey::Up),
        Some(PlaceAction::Moved { row: 0 })
    );
    assert_eq!(named(&mut chooser, NamedKey::Up), None, "the top stops");
    let last = chooser.rows().len() - 1;
    for _ in 0..chooser.rows().len() {
        named(&mut chooser, NamedKey::PageDown);
    }
    assert_eq!(chooser.highlighted(), Some(last));
    assert_eq!(
        named(&mut chooser, NamedKey::Down),
        None,
        "the bottom stops"
    );
    assert!(matches!(
        named(&mut chooser, NamedKey::PageUp),
        Some(PlaceAction::Moved { .. })
    ));
}

#[test]
fn enter_chooses_the_highlight_and_escape_clears_before_it_cancels() {
    let mut chooser = focused();
    typed(&mut chooser, "par");
    assert_eq!(names(&chooser), ["Paris"]);
    assert_eq!(
        named(&mut chooser, NamedKey::Enter),
        Some(PlaceAction::Chose { row: 8 })
    );
    assert_eq!(
        named(&mut chooser, NamedKey::Escape),
        Some(PlaceAction::Searched)
    );
    assert_eq!(chooser.query(), "");
    assert_eq!(chooser.shown().len(), chooser.rows().len());
    assert_eq!(
        named(&mut chooser, NamedKey::Escape),
        Some(PlaceAction::Cancelled)
    );
    assert_eq!(
        named(&mut chooser, NamedKey::Tab),
        None,
        "the owner moves on"
    );
}

#[test]
fn a_chooser_without_the_keyboard_or_disabled_takes_no_keys() {
    let mut chooser = chooser();
    assert_eq!(named(&mut chooser, NamedKey::Down), None);
    let mut chooser = focused();
    chooser.set_state(ControlState::disabled());
    chooser.set_focused(true);
    assert_eq!(named(&mut chooser, NamedKey::Down), None);
    assert_eq!(press_at(&mut chooser, row_point(2), 0), None);
}

#[test]
fn a_click_highlights_and_a_second_within_the_interval_chooses() {
    let mut chooser = chooser();
    assert_eq!(
        press_at(&mut chooser, row_point(2), 0),
        Some(PlaceAction::Moved { row: 2 })
    );
    assert_eq!(
        press_at(&mut chooser, row_point(2), 100),
        Some(PlaceAction::Chose { row: 2 })
    );
    // Past the interval a press on the same row is a fresh single.
    assert_eq!(press_at(&mut chooser, row_point(2), 1000), None);
    // Two presses on different rows are two singles.
    assert_eq!(
        press_at(&mut chooser, row_point(3), 1100),
        Some(PlaceAction::Moved { row: 3 })
    );
    assert_eq!(
        press_at(&mut chooser, row_point(4), 1200),
        Some(PlaceAction::Moved { row: 4 })
    );
}

#[test]
fn a_press_in_the_search_field_breaks_a_pair() {
    let mut chooser = chooser();
    press_at(&mut chooser, row_point(2), 0);
    assert_eq!(press_at(&mut chooser, Point::new(20, 5), 50), None);
    assert_eq!(
        press_at(&mut chooser, row_point(2), 100),
        None,
        "the row was already highlighted, and the press is a single"
    );
}

#[test]
fn the_wheel_scrolls_the_list_and_a_highlight_is_brought_into_view() {
    let theme = Theme::dark();
    let last = chooser().rows().len() - 1;
    let mut chooser = chooser().with_highlight(last);
    assert!(
        PlaceChooser::height_for(u32::try_from(last + 1).expect("few"), Scale::ONE, &theme) > H,
        "the list must outgrow the bounds for this to test scrolling"
    );
    let mut surface = Surface::new(W, H).expect("a surface");
    chooser.render(&mut surface, bounds(), Scale::ONE, &theme);
    // Settled at the first layout: the last row stands at the list's foot.
    let foot = Point::new(40, i32::try_from(H - 2).expect("fits"));
    pointer(&mut chooser, &InputEvent::PointerMoved { to: foot }, 0);
    assert_eq!(press_at(&mut chooser, foot, 0), None, "already highlighted");
    assert_eq!(
        press_at(&mut chooser, foot, 100),
        Some(PlaceAction::Chose { row: last }),
        "the row at the foot is the highlighted last one"
    );
    pointer(
        &mut chooser,
        &InputEvent::PointerMoved { to: row_point(0) },
        0,
    );
    let top = chooser.shown()[0];
    pointer(
        &mut chooser,
        &InputEvent::PointerScrolled { dx: 0, dy: -10_000 },
        0,
    );
    // Scrolled back to the top, the row under the pointer is the first.
    assert_eq!(
        press_at(&mut chooser, row_point(0), 1000),
        Some(PlaceAction::Moved { row: top })
    );
}

#[test]
fn a_reading_reports_only_a_change_of_a_row_that_shows() {
    let theme = Theme::dark();
    let mut chooser = focused();
    let mut damage = damage::sink();
    chooser.set_reading(
        0,
        "14:02 · UTC+01:00",
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert!(damage.is_empty(), "the same reading draws the same pixels");
    chooser.set_reading(
        0,
        "15:02 · UTC+02:00",
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert!(!damage.is_empty());
    typed(&mut chooser, "tokyo");
    let mut damage = damage::sink();
    chooser.set_reading(
        0,
        "16:02 · UTC+03:00",
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert!(damage.is_empty(), "Auckland is not shown");
    assert_eq!(chooser.rows()[0].reading, "16:02 · UTC+03:00");
}

#[test]
fn a_hover_reports_the_rows_it_moves_between_and_nothing_else() {
    let mut chooser = chooser();
    let theme = Theme::dark();
    let mut damage = damage::sink();
    chooser.on_pointer(
        &InputEvent::PointerMoved { to: row_point(1) },
        at(0),
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    let height = PlaceChooser::row_height(Scale::ONE, &theme);
    let rects = damage.rects();
    assert!(!rects.is_empty());
    assert!(rects.iter().all(|rect| rect.height <= height), "{rects:?}");
}

#[test]
fn an_empty_list_says_so_and_every_theme_draws_a_list() {
    for theme in [Theme::dark(), Theme::light(), high_contrast()] {
        let mut chooser = focused();
        let mut blank = Surface::new(W, H).expect("a surface");
        chooser.render(&mut blank, bounds(), Scale::ONE, &theme);
        typed(&mut chooser, "qqqq");
        let mut empty = Surface::new(W, H).expect("a surface");
        chooser.render(&mut empty, bounds(), Scale::ONE, &theme);
        assert_ne!(
            blank.pixels(),
            empty.pixels(),
            "{NO_MATCH} differs from a list"
        );
    }
}

#[test]
fn a_tiny_or_zero_bound_draws_without_fault() {
    let theme = Theme::dark();
    let chooser = chooser().with_highlight(3);
    for bounds in [
        Rect::new(0, 0, 0, 0),
        Rect::new(0, 0, 3, 3),
        Rect::new(0, 0, W, 10),
    ] {
        let mut surface = Surface::new(W, H).expect("a surface");
        chooser.render(&mut surface, bounds, Scale::ONE, &theme);
    }
}

#[test]
fn a_paste_searches() {
    let theme = Theme::dark();
    let mut chooser = focused();
    assert_eq!(
        chooser.insert_text("berl", bounds(), Scale::ONE, &theme, &mut damage::sink()),
        Some(PlaceAction::Searched)
    );
    assert_eq!(names(&chooser), ["Berlin"]);
    let _: String = chooser.query().into();
}

// --- The place dialog ------------------------------------------------------

use crate::place::{PlaceDialog, PlaceOutcome};

const DW: u32 = 420;

fn dialog() -> PlaceDialog {
    PlaceDialog::new("Choose a time zone", chooser())
}

fn dialog_bounds(dialog: &PlaceDialog) -> Rect {
    let theme = Theme::dark();
    Rect::new(0, 0, DW, dialog.height_for(6, DW, Scale::ONE, &theme))
}

fn dialog_key(dialog: &mut PlaceDialog, named: NamedKey) -> Option<PlaceOutcome> {
    let theme = Theme::dark();
    let bounds = dialog_bounds(dialog);
    dialog.on_key(
        Key::Named(named),
        Modifiers::default(),
        bounds,
        Scale::ONE,
        &theme,
        &mut damage::sink(),
    )
}

fn dialog_click(dialog: &mut PlaceDialog, point: Point, ms: u64) -> Option<PlaceOutcome> {
    let theme = Theme::dark();
    let bounds = dialog_bounds(dialog);
    let mut last = None;
    for event in [
        InputEvent::PointerMoved { to: point },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        if let Some(outcome) = dialog.on_pointer(
            &event,
            at(ms),
            bounds,
            Scale::ONE,
            &theme,
            &mut damage::sink(),
        ) {
            last = Some(outcome);
        }
    }
    last
}

/// The centre of the dialog's action `index`.
fn action(dialog: &PlaceDialog, index: usize) -> Point {
    let theme = Theme::dark();
    let rect = crate::decision::Dialog::new("Choose a time zone")
        .with_actions(alloc::vec![
            crate::button::Button::labelled("Cancel"),
            crate::button::Button::labelled("Choose"),
        ])
        .action_rects(dialog_bounds(dialog), Scale::ONE, &theme)[index];
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).expect("fits"),
        rect.top() + i32::try_from(rect.height / 2).expect("fits"),
    )
}

#[test]
fn choose_is_offered_only_while_a_place_is_highlighted() {
    let mut dialog = dialog();
    assert_eq!(
        {
            let point = action(&dialog, 1);
            dialog_click(&mut dialog, point, 0)
        },
        None,
        "nothing is highlighted, so Choose answers nothing"
    );
    assert_eq!(dialog_key(&mut dialog, NamedKey::Down), None);
    assert_eq!(dialog.chooser().highlighted(), Some(0));
    assert_eq!(
        {
            let point = action(&dialog, 1);
            dialog_click(&mut dialog, point, 1000)
        },
        Some(PlaceOutcome::Chose { row: 0 })
    );
}

#[test]
fn enter_chooses_escape_cancels_and_cancel_cancels() {
    let mut dialog = dialog();
    dialog_key(&mut dialog, NamedKey::Down);
    dialog_key(&mut dialog, NamedKey::Down);
    assert_eq!(
        dialog_key(&mut dialog, NamedKey::Enter),
        Some(PlaceOutcome::Chose { row: 1 })
    );
    assert_eq!(
        dialog_key(&mut dialog, NamedKey::Escape),
        Some(PlaceOutcome::Cancelled)
    );
    assert_eq!(
        {
            let point = action(&dialog, 0);
            dialog_click(&mut dialog, point, 0)
        },
        Some(PlaceOutcome::Cancelled)
    );
}

#[test]
fn a_dialog_opened_on_the_current_place_offers_choose_at_once() {
    let theme = Theme::dark();
    let mut dialog = PlaceDialog::new("Choose a time zone", chooser().with_highlight(5));
    let mut surface = Surface::new(DW, 400).expect("a surface");
    let bounds = dialog_bounds(&dialog);
    dialog.render(&mut surface, bounds, Scale::ONE, &theme);
    assert_eq!(
        {
            let point = action(&dialog, 1);
            dialog_click(&mut dialog, point, 0)
        },
        Some(PlaceOutcome::Chose { row: 5 })
    );
}
