//! Host tests for the places / devices rail's input routing.
//!
//! The rail is driven with the same `WindowEvent`s the desktop session
//! delivers, over an in-memory directory source, so every decision the running
//! app makes — which press lands on which row, where the keyboard focus goes,
//! and what a refused place does — is exercised without a kernel.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::BlkDeviceClass;
use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode, PointerButtonCode};
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_browse::render::{sidebar_index_at, sidebar_view, toolbar_command_at};
use tairix_browse::{Browser, Places, SidebarView, ToolbarBand, ToolbarCommand, Volume};
use tairix_controls::damage;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_theme::Theme;

use tairix_window::{pointer_point, Repaint};

use super::{
    apply_event, follow_pointer, is_refresh_request, press_point, refresh_places, track_hover,
};
use crate::chrome::Chrome;
use crate::test_fs::{browser, FakeFs};

/// The window the rail is laid out in for these tests.
const WINDOW: Rect = Rect::new(0, 0, 480, 480);

/// The chrome these tests drive: both bands shown, the layout the rail's
/// own routing is measured in.
const BAND: ToolbarBand = ToolbarBand::Shown;

/// A window showing the rail below the toolbar band.
const SHOWN: Chrome = Chrome {
    rail: true,
    toolbar: BAND,
};

/// The window this app is given, addressed by every synthesised event.
const WINDOW_ID: u64 = 7;

/// The user's home directory components.
fn home() -> Vec<String> {
    vec!["Users".to_string(), "ann".to_string()]
}

/// The rail the tests drive: the user's places plus one mounted volume.
fn places() -> Places {
    Places::new(
        &home(),
        &[Volume {
            label: "Backup".to_string(),
            target: "/Storage/Backup".to_string(),
            medium: Some(BlkDeviceClass::Rotational),
        }],
    )
}

/// The centre of the rail row at `index`.
fn row_centre(places: &Places, index: usize) -> Point {
    let view = rail(places);
    let rect = view
        .row_rect(index)
        .expect("every row of this rail is drawn in a 480px-tall window");
    Point::new(
        rect.origin.x + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.origin.y + i32::try_from(rect.height / 2).unwrap_or(0),
    )
}

/// The rail's geometry in this test's window.
fn rail(places: &Places) -> SidebarView {
    sidebar_view(WINDOW, Scale::ONE, &Theme::dark(), Some(places), BAND).expect("the rail has rows")
}

/// A primary press at `point`.
fn press(point: Point) -> WindowEvent {
    WindowEvent::Pointer {
        window_id: WINDOW_ID,
        x: u32::try_from(point.x).unwrap_or(0),
        y: u32::try_from(point.y).unwrap_or(0),
        action: PointerAction::Pressed(PointerButtonCode::Primary),
        modifiers: Modifiers::default(),
    }
}

/// A pointer motion to `point`.
fn motion(point: Point) -> WindowEvent {
    WindowEvent::Pointer {
        window_id: WINDOW_ID,
        x: u32::try_from(point.x).unwrap_or(0),
        y: u32::try_from(point.y).unwrap_or(0),
        action: PointerAction::Moved,
        modifiers: Modifiers::default(),
    }
}

/// A key press with no modifiers held.
fn key(key: KeyValue) -> WindowEvent {
    WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Pressed {
            key,
            modifiers: Modifiers::default(),
        },
    }
}

/// A named key press with no modifiers held.
fn named(code: NamedKeyCode) -> WindowEvent {
    key(KeyValue::Named(code))
}

/// A key press with `modifiers` held.
fn chord(key: KeyValue, modifiers: Modifiers) -> WindowEvent {
    WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Pressed { key, modifiers },
    }
}

/// Only `Alt` held.
const ALT: Modifiers = Modifiers {
    shift: false,
    ctrl: false,
    alt: true,
    meta: false,
};

/// Route `event` to the rail with this test's window and theme, discarding
/// what it reported.
fn route(
    browser: &mut Browser<FakeFs>,
    places: &mut Places,
    event: &WindowEvent,
) -> Option<super::SidebarOutcome> {
    routed(browser, places, event).0
}

/// Route `event` to the rail, answering the outcome and the rectangles it
/// reported. A pointer event puts the pointer where it says, as the window
/// records it before routing; any other event is routed with no pointer known.
fn routed(
    browser: &mut Browser<FakeFs>,
    places: &mut Places,
    event: &WindowEvent,
) -> (Option<super::SidebarOutcome>, Region) {
    let pointer = match event {
        WindowEvent::Pointer { x, y, .. } => Some(pointer_point(*x, *y)),
        _ => None,
    };
    routed_at(browser, places, pointer, event)
}

/// Route `event` to the rail with the window's pointer last at `pointer`.
fn routed_at(
    browser: &mut Browser<FakeFs>,
    places: &mut Places,
    pointer: Option<Point>,
    event: &WindowEvent,
) -> (Option<super::SidebarOutcome>, Region) {
    let mut damage = damage::sink();
    let outcome = apply_event(
        browser,
        places,
        Scale::ONE,
        &Theme::dark(),
        WINDOW,
        BAND,
        pointer,
        event,
        &mut damage,
    );
    (outcome, damage)
}

/// Track the rail's hover for `event`, answering whether it moved and the
/// rectangles it reported.
fn hovered(places: &mut Places, event: &WindowEvent) -> (bool, Region) {
    let mut damage = damage::sink();
    let moved = track_hover(
        places,
        Scale::ONE,
        &Theme::dark(),
        WINDOW,
        BAND,
        event,
        &mut damage,
    );
    (moved, damage)
}

#[test]
fn a_press_on_a_row_focuses_the_rail_and_navigates_to_that_place() {
    let mut browser = browser();
    let mut places = places();
    let apps = places
        .index_of(&["Apps".to_string()])
        .expect("the application root is a fixed row");

    let on_apps = press(row_centre(&places, apps));
    let outcome = route(&mut browser, &mut places, &on_apps).expect("the rail owns its own press");

    assert_eq!(
        outcome.repaint,
        Repaint::Whole,
        "the listing, the toolbar's enable states, and the rail's own mark all moved"
    );
    assert_eq!(outcome.refused, None);
    assert_eq!(browser.components(), ["Apps".to_string()]);
    assert!(places.is_focused());
    assert_eq!(places.cursor(), apps);
    // The rail shows where the browser now is.
    assert_eq!(places.index_of(browser.components()), Some(apps));
}

#[test]
fn a_press_beyond_the_rail_is_left_to_the_view_below() {
    let mut browser = browser();
    let mut places = places();
    // Level with a drawn row, one pixel past the rail's trailing edge.
    let just_outside = Point::new(
        i32::try_from(rail(&places).width()).unwrap_or(0),
        row_centre(&places, 0).y,
    );

    assert!(route(&mut browser, &mut places, &press(just_outside)).is_none());
    // Nothing about the rail moved, and the browser stayed put.
    assert!(!places.is_focused());
    assert!(browser.components().is_empty());
}

#[test]
fn tab_moves_the_focus_between_the_rail_and_the_view_in_both_directions() {
    let mut browser = browser();
    let mut places = places();

    // Into the rail…
    let outcome = route(&mut browser, &mut places, &named(NamedKeyCode::Tab));
    assert_eq!(outcome, Some(super::SidebarOutcome::reported(true)));
    assert!(places.is_focused());

    // …and back out again, from the same key.
    let outcome = route(&mut browser, &mut places, &named(NamedKeyCode::Tab));
    assert_eq!(outcome, Some(super::SidebarOutcome::reported(true)));
    assert!(!places.is_focused());

    // Escape hands the focus back too, and only while the rail holds it.
    assert!(route(&mut browser, &mut places, &named(NamedKeyCode::Escape)).is_none());
    places.set_focused(true);
    let outcome = route(&mut browser, &mut places, &named(NamedKeyCode::Escape));
    assert_eq!(outcome, Some(super::SidebarOutcome::reported(true)));
    assert!(!places.is_focused());
}

#[test]
fn the_arrows_walk_the_rail_and_enter_navigates_to_the_cursor() {
    let mut browser = browser();
    let mut places = places();
    places.set_focused(true);

    // Down to UserFiles (Home, Desktop, UserFiles).
    assert_eq!(
        route(&mut browser, &mut places, &named(NamedKeyCode::Down)),
        Some(super::SidebarOutcome::reported(true))
    );
    assert_eq!(
        route(&mut browser, &mut places, &named(NamedKeyCode::Down)),
        Some(super::SidebarOutcome::reported(true))
    );
    assert_eq!(places.cursor(), 2);

    let outcome = route(&mut browser, &mut places, &named(NamedKeyCode::Enter));
    assert_eq!(
        outcome.map(|o| o.repaint),
        Some(Repaint::Whole),
        "a move replaces the listing, which no rail report describes"
    );
    assert_eq!(
        browser.components(),
        [
            "Users".to_string(),
            "ann".to_string(),
            "UserFiles".to_string()
        ]
    );

    // Up walks back and clamps at the first row rather than wrapping.
    route(&mut browser, &mut places, &named(NamedKeyCode::Up));
    route(&mut browser, &mut places, &named(NamedKeyCode::Up));
    assert_eq!(places.cursor(), 0);
    assert_eq!(
        route(&mut browser, &mut places, &named(NamedKeyCode::Up)),
        Some(super::SidebarOutcome::QUIET)
    );

    // The last row is reachable and clamps at the other end.
    for _ in 0..places.len() + 2 {
        route(&mut browser, &mut places, &named(NamedKeyCode::Down));
    }
    assert_eq!(places.cursor(), places.len() - 1);
    let outcome = route(&mut browser, &mut places, &named(NamedKeyCode::Enter));
    assert_eq!(outcome.map(|o| o.repaint), Some(Repaint::Whole));
    assert_eq!(
        browser.components(),
        ["Storage".to_string(), "Backup".to_string()]
    );
}

#[test]
fn the_rail_takes_only_the_keys_it_owns_and_only_while_it_is_focused() {
    let mut browser = browser();
    let mut places = places();

    // Unfocused, its arrows belong to the listing.
    assert!(route(&mut browser, &mut places, &named(NamedKeyCode::Down)).is_none());
    assert!(route(&mut browser, &mut places, &named(NamedKeyCode::Enter)).is_none());

    // Focused, a key it has no use for is swallowed rather than reaching the
    // listing behind it.
    places.set_focused(true);
    assert_eq!(
        route(&mut browser, &mut places, &key(KeyValue::Char('x'))),
        Some(super::SidebarOutcome::QUIET)
    );
    // A key *release* is never the rail's, focused or not.
    let release = WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Released {
            key: KeyValue::Named(NamedKeyCode::Down),
            modifiers: Modifiers::default(),
        },
    };
    assert!(route(&mut browser, &mut places, &release).is_none());
    // And a `Ctrl+Tab` is a shortcut for the window, not a focus move.
    let ctrl_tab = WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Pressed {
            key: KeyValue::Named(NamedKeyCode::Tab),
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        },
    };
    assert_eq!(
        route(&mut browser, &mut places, &ctrl_tab),
        Some(super::SidebarOutcome::QUIET)
    );
    assert!(places.is_focused());
}

#[test]
fn a_place_that_cannot_be_listed_reports_and_leaves_the_browser_where_it_was() {
    let mut browser = browser();
    let mut places = places();
    let desktop = places
        .index_of(&[
            "Users".to_string(),
            "ann".to_string(),
            "Desktop".to_string(),
        ])
        .expect("Desktop is a fixed row");
    let before = browser.components().to_vec();
    let on_desktop = press(row_centre(&places, desktop));

    let outcome =
        route(&mut browser, &mut places, &on_desktop).expect("the rail owns its own press");

    // Stated, not silent — and naming the place the user clicked.
    assert_eq!(outcome.refused.as_deref(), Some("could not open Desktop"));
    assert_eq!(outcome.repaint, Repaint::Whole);
    // The browser did not move, and the row now reads disabled.
    assert_eq!(browser.components(), before.as_slice());
    assert!(!places.rows()[desktop].is_available());

    // A disabled row does not act: activating it again refuses nothing,
    // repaints nothing, and still leaves the browser alone.
    places.set_cursor(desktop);
    places.set_focused(true);
    let again = route(&mut browser, &mut places, &named(NamedKeyCode::Enter));
    assert_eq!(again, Some(super::SidebarOutcome::QUIET));
    assert_eq!(browser.components(), before.as_slice());
}

#[test]
fn pressing_the_row_already_marked_changes_and_reports_nothing() {
    let mut browser = browser();
    let mut places = places();
    let apps = places
        .index_of(&["Apps".to_string()])
        .expect("the application root is a fixed row");
    let on_apps = press(row_centre(&places, apps));
    route(&mut browser, &mut places, &on_apps);

    // The focus, the cursor, and the browser are all already there, so the
    // second press moves no pixel and owes no present.
    let (outcome, damage) = routed(&mut browser, &mut places, &on_apps);
    assert_eq!(outcome, Some(super::SidebarOutcome::QUIET));
    assert!(damage.is_empty());
    assert_eq!(browser.components(), ["Apps".to_string()]);
}

#[test]
fn a_press_that_only_moves_the_mark_reports_the_rows_it_moved_between() {
    let mut browser = browser();
    let mut places = places();
    let apps = places
        .index_of(&["Apps".to_string()])
        .expect("the application root is a fixed row");
    let on_apps = press(row_centre(&places, apps));
    route(&mut browser, &mut places, &on_apps);
    // Put the cursor elsewhere without navigating, so the next press on the
    // shown row moves the mark and nothing else.
    places.set_cursor(0);
    let view = rail(&places);
    let (first, marked) = (
        view.row_rect(0).expect("row 0"),
        view.row_rect(apps).expect("the Apps row"),
    );

    let (outcome, damage) = routed(&mut browser, &mut places, &on_apps);

    assert_eq!(outcome.map(|o| o.repaint), Some(Repaint::Reported));
    let mut want = damage::sink();
    want.add(first);
    want.add(marked);
    assert_eq!(damage.rects(), want.rects());
}

#[test]
fn the_hover_highlight_follows_the_pointer_and_clears_off_the_rail() {
    let mut places = places();
    let width = rail(&places).width();
    let second = rail(&places).row_rect(1).expect("the rail's second row");

    let over_second = motion(row_centre(&places, 1));
    let (moved, damage) = hovered(&mut places, &over_second);
    assert!(moved);
    assert_eq!(places.hovered(), Some(1));
    assert_eq!(
        damage.rects(),
        &[second],
        "entering a row from nowhere marks that row and nothing else"
    );

    // The same row again is not a change, so it owes no repaint.
    let (moved, damage) = hovered(&mut places, &over_second);
    assert!(!moved);
    assert!(damage.is_empty());

    // Leaving the rail clears the highlight, marking the row it left.
    let outside = Point::new(i32::try_from(width).unwrap_or(0) + 20, 4);
    let (moved, damage) = hovered(&mut places, &motion(outside));
    assert!(moved);
    assert_eq!(places.hovered(), None);
    assert_eq!(damage.rects(), &[second]);

    // A press is not a motion: the highlight is only ever moved by one.
    let on_first = press(row_centre(&places, 0));
    let (moved, damage) = hovered(&mut places, &on_first);
    assert!(!moved);
    assert!(damage.is_empty());
}

#[test]
fn a_hover_that_leaves_one_row_for_the_next_reports_both() {
    let mut places = places();
    let view = rail(&places);
    let (first, second) = (
        view.row_rect(0).expect("row 0"),
        view.row_rect(1).expect("row 1"),
    );
    let (onto_first, onto_second) = (
        motion(row_centre(&places, 0)),
        motion(row_centre(&places, 1)),
    );
    let (moved, _) = hovered(&mut places, &onto_first);
    assert!(moved);

    let (moved, damage) = hovered(&mut places, &onto_second);

    assert!(moved);
    let mut want = damage::sink();
    want.add(first);
    want.add(second);
    assert_eq!(damage.rects(), want.rects());
}

#[test]
fn a_focus_flip_marks_the_whole_rail_because_every_row_redraws() {
    let mut browser = browser();
    let mut places = places();
    let rail_rect = rail(&places).rail_rect();

    let (outcome, damage) = routed(&mut browser, &mut places, &named(NamedKeyCode::Tab));

    assert_eq!(outcome.map(|o| o.repaint), Some(Repaint::Reported));
    assert!(places.is_focused());
    assert_eq!(
        damage.rects(),
        &[rail_rect],
        "a rail that holds the keyboard draws every row as a focus-field member"
    );
}

#[test]
fn walking_the_focused_cursor_marks_only_the_two_rows_it_moved_between() {
    let mut browser = browser();
    let mut places = places();
    places.set_focused(true);
    let view = rail(&places);
    let (first, second) = (
        view.row_rect(0).expect("row 0"),
        view.row_rect(1).expect("row 1"),
    );

    let (outcome, damage) = routed(&mut browser, &mut places, &named(NamedKeyCode::Down));

    assert_eq!(outcome.map(|o| o.repaint), Some(Repaint::Reported));
    let mut want = damage::sink();
    want.add(first);
    want.add(second);
    assert_eq!(damage.rects(), want.rects());
}

#[test]
fn a_cursor_that_cannot_move_reports_nothing() {
    let mut browser = browser();
    let mut places = places();
    places.set_focused(true);

    let (outcome, damage) = routed(&mut browser, &mut places, &named(NamedKeyCode::Up));

    assert_eq!(outcome, Some(super::SidebarOutcome::QUIET));
    assert!(damage.is_empty());
}

#[test]
fn the_refresh_gesture_is_f5_or_the_toolbars_refresh_command() {
    let browser = browser();
    let places = places();
    let theme = Theme::dark();

    assert!(is_refresh_request(
        &browser,
        Scale::ONE,
        &theme,
        WINDOW,
        BAND,
        &named(NamedKeyCode::F5)
    ));
    assert!(!is_refresh_request(
        &browser,
        Scale::ONE,
        &theme,
        WINDOW,
        BAND,
        &named(NamedKeyCode::Enter)
    ));

    // A press on the toolbar's Refresh control is the same request. The
    // toolbar is window chrome, so the control's position is read from the
    // whole window — the band it was drawn across — and the test cannot drift
    // from the painted chrome.
    let mut refresh = None;
    let mut other = None;
    let right = WINDOW.origin.x + i32::try_from(WINDOW.width).unwrap_or(0);
    let bottom = WINDOW.origin.y + i32::try_from(WINDOW.height).unwrap_or(0);
    for y in WINDOW.origin.y..bottom {
        for x in WINDOW.origin.x..right {
            let point = Point::new(x, y);
            match toolbar_command_at(&browser, Scale::ONE, &theme, WINDOW, BAND, point) {
                Some(ToolbarCommand::Refresh) if refresh.is_none() => refresh = Some(point),
                Some(command) if command != ToolbarCommand::Refresh && other.is_none() => {
                    other = Some(point);
                }
                _ => {}
            }
        }
        if refresh.is_some() && other.is_some() {
            break;
        }
    }
    let refresh = refresh.expect("the toolbar carries a Refresh command");
    let other = other.expect("the toolbar carries more than one enabled command");
    // The two routers are disjoint: the toolbar owns its band outright, so the
    // press that refreshes is never also a press on a place.
    assert_eq!(
        sidebar_index_at(WINDOW, Scale::ONE, &theme, Some(&places), BAND, refresh),
        None
    );
    assert!(is_refresh_request(
        &browser,
        Scale::ONE,
        &theme,
        WINDOW,
        BAND,
        &press(refresh)
    ));
    assert!(!is_refresh_request(
        &browser,
        Scale::ONE,
        &theme,
        WINDOW,
        BAND,
        &press(other)
    ));
    // A motion over Refresh is not a request; only a press is.
    assert!(!is_refresh_request(
        &browser,
        Scale::ONE,
        &theme,
        WINDOW,
        BAND,
        &motion(refresh)
    ));
}

#[test]
fn a_refresh_re_reads_the_volumes_and_keeps_the_users_place_in_the_rail() {
    let mut places = places();
    places.set_focused(true);
    places.set_cursor(2);

    refresh_places(
        &mut places,
        &home(),
        &[
            Volume {
                label: "Backup".to_string(),
                target: "/Storage/Backup".to_string(),
                medium: Some(BlkDeviceClass::Rotational),
            },
            // A stick the user just plugged in: it appears without a poll.
            Volume {
                label: "Stick".to_string(),
                target: "/Storage/Stick".to_string(),
                medium: Some(BlkDeviceClass::Removable),
            },
        ],
    );

    assert!(places.is_focused());
    assert_eq!(places.cursor(), 2);
    assert_eq!(places.len(), 7);
    assert_eq!(places.rows()[6].label(), "Stick");

    // A cursor the shorter rail no longer has is left at the first row rather
    // than pointing past the end.
    places.set_cursor(6);
    refresh_places(&mut places, &home(), &[]);
    assert_eq!(places.len(), 5);
    assert_eq!(places.cursor(), 0);
}

#[test]
fn only_a_primary_press_resolves_to_a_point() {
    assert_eq!(
        press_point(PointerAction::Pressed(PointerButtonCode::Primary), 3, 9),
        Some(Point::new(3, 9))
    );
    assert_eq!(
        press_point(PointerAction::Pressed(PointerButtonCode::Secondary), 3, 9),
        None
    );
    assert_eq!(press_point(PointerAction::Moved, 3, 9), None);
    assert_eq!(
        press_point(PointerAction::Released(PointerButtonCode::Primary), 3, 9),
        None
    );
    // A coordinate no window can hold is clamped rather than wrapped into a
    // negative position that would hit-test somewhere else entirely.
    assert_eq!(
        press_point(
            PointerAction::Pressed(PointerButtonCode::Primary),
            u32::MAX,
            0
        ),
        Some(Point::new(i32::MAX, 0))
    );
}

// --- A rail longer than its window scrolls -------------------------------

/// Thirty mounted volumes: far more rows than the test window holds.
fn crowded_volumes() -> Vec<Volume> {
    (0..30)
        .map(|at| Volume {
            label: alloc::format!("Disk {at:02}"),
            target: alloc::format!("/Storage/d{at:02}"),
            medium: None,
        })
        .collect()
}

/// A rail that outgrows the test window.
fn crowded() -> Places {
    Places::new(&home(), &crowded_volumes())
}

/// A wheel turn of `detents` toward the end.
fn wheel(detents: i32) -> WindowEvent {
    WindowEvent::Scrolled {
        window_id: WINDOW_ID,
        x: 0,
        y: 0,
        dx: 0,
        dy: detents * tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT,
        modifiers: tairix_abi::input::Modifiers::default(),
    }
}

/// Whether `damage` covers every pixel of `rect`.
fn covers(damage: &Region, rect: Rect) -> bool {
    let mut uncovered = Region::new();
    uncovered.add(rect);
    for covered in damage.rects() {
        uncovered.subtract(*covered);
    }
    uncovered.is_empty()
}

/// The rail row the window shows under `at`.
fn row_under(places: &Places, at: Point) -> Option<usize> {
    sidebar_index_at(WINDOW, Scale::ONE, &Theme::dark(), Some(places), BAND, at)
}

/// A wheel over the rail scrolls the rail, never the listing beside it, and
/// the row lit is the one the turn brought under the still pointer. A wheel
/// anywhere else is not the rail's.
#[test]
fn a_wheel_over_a_crowded_rail_scrolls_the_rail_and_the_hover_follows_its_rows() {
    let mut browser = browser();
    let mut places = crowded();
    assert!(
        rail(&places).bar_rect().is_some(),
        "the premise: it outgrows the window"
    );
    let at = row_centre(&places, 1);
    hovered(&mut places, &motion(at));
    assert_eq!(places.hovered(), Some(1));
    let listing = browser.scroll_offset();

    let (outcome, damage) = routed_at(&mut browser, &mut places, Some(at), &wheel(3));

    assert_eq!(
        outcome.map(|outcome| outcome.repaint),
        Some(Repaint::Reported)
    );
    assert!(places.scroll().offset() > 0, "the rail scrolled");
    assert_eq!(browser.scroll_offset(), listing, "and the listing did not");
    let view = rail(&places);
    assert!(
        covers(&damage, view.rows_area()),
        "the rows slid unreported"
    );
    assert!(
        covers(&damage, view.bar_rect().expect("a bar")),
        "the thumb moved unreported"
    );
    let now_under = row_under(&places, at);
    assert!(
        now_under.is_some_and(|row| row != 1),
        "the premise: another row is under it"
    );
    assert_eq!(
        places.hovered(),
        now_under,
        "the hover stayed on the row carried away"
    );

    let listing = Point::new(400, 200);
    hovered(&mut places, &motion(listing));
    assert!(
        routed_at(&mut browser, &mut places, Some(listing), &wheel(1))
            .0
            .is_none(),
        "a wheel over the listing is the listing's"
    );
}

/// The wheel goes where the window says its pointer is, not where the pointer
/// last moved over the rail: a window that hid its rail went on recording the
/// pointer, and a rail shown again must not claim a wheel turned elsewhere.
#[test]
fn a_wheel_follows_the_windows_pointer_not_the_rails_last_motion() {
    let mut browser = browser();
    let mut places = crowded();
    let on_rail = motion(row_centre(&places, 1));
    hovered(&mut places, &on_rail);

    let elsewhere = Point::new(400, 200);
    let (outcome, damage) = routed_at(&mut browser, &mut places, Some(elsewhere), &wheel(1));

    assert!(outcome.is_none(), "the wheel is the listing's");
    assert_eq!(places.scroll().offset(), 0, "and the rail did not move");
    assert!(damage.is_empty());
}

/// A press on the rail's bar is the bar's: it steps the rail and neither
/// navigates nor moves the rail's cursor.
#[test]
fn a_press_on_the_rails_bar_steps_it_and_navigates_nowhere() {
    let mut browser = browser();
    let mut places = crowded();
    let bar = rail(&places).bar_rect().expect("a bar");
    let end = Point::new(
        bar.origin.x + i32::try_from(bar.width / 2).unwrap_or(0),
        bar.bottom() - 2,
    );
    hovered(&mut places, &motion(end));
    let location = browser.components().to_vec();
    let cursor = places.cursor();

    assert!(route(&mut browser, &mut places, &press(end)).is_some());

    assert!(
        places.scroll().offset() > 0,
        "the end button stepped the rail"
    );
    assert_eq!(browser.components(), location, "and navigated nowhere");
    assert_eq!(places.cursor(), cursor, "nor moved the cursor");
}

/// Once scrolled, a press lands on the row it was aimed at, past the fold the
/// window started with.
#[test]
fn a_press_where_a_scrolled_rail_shows_its_last_row_lands_on_it() {
    let mut browser = browser();
    let mut places = crowded();
    let first = row_centre(&places, 0);
    hovered(&mut places, &motion(first));
    let _ = routed_at(&mut browser, &mut places, Some(first), &wheel(400));
    let last = places.len() - 1;
    let shown = rail(&places)
        .shown_row_rect(last)
        .expect("the wheel brought the last row into view");
    let at = Point::new(
        shown.origin.x + 4,
        shown.origin.y + i32::try_from(shown.height / 2).unwrap_or(0),
    );

    let _ = route(&mut browser, &mut places, &press(at));

    assert_eq!(
        places.cursor(),
        last,
        "the press landed on the row it was aimed at"
    );
}

/// Walking the focused cursor past the fold scrolls the rail to it, so the
/// keyboard reaches every row the pointer can.
#[test]
fn walking_the_rail_cursor_past_the_fold_scrolls_the_rail_to_it() {
    let mut browser = browser();
    let mut places = crowded();
    let _ = route(&mut browser, &mut places, &named(NamedKeyCode::Tab));
    assert!(places.is_focused());
    for _ in 0..places.len() {
        let _ = route(&mut browser, &mut places, &named(NamedKeyCode::Down));
    }
    let last = places.len() - 1;
    assert_eq!(places.cursor(), last);
    let view = rail(&places);
    let whole = view.row_rect(last).expect("the last row");
    let shown = view.shown_row_rect(last).expect("revealed");
    assert_eq!(shown.height, whole.height, "revealed whole");
}

/// A refresh keeps where the rail was scrolled, as it keeps the cursor.
#[test]
fn a_refresh_keeps_where_the_rail_was_scrolled() {
    let mut places = crowded();
    places.scroll_mut().set_offset(40);
    refresh_places(&mut places, &home(), &crowded_volumes());
    assert_eq!(places.scroll().offset(), 40);
}

// --- The window's own keys, and the whole repaint -------------------------

/// A focused rail lets the window's accelerators through: they act on the
/// window, and with the toolbar hidden they are the only way to its commands.
#[test]
fn a_focused_rail_lets_the_windows_accelerators_through() {
    let mut browser = browser();
    let mut places = places();
    places.set_focused(true);
    places.set_cursor(2);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };

    for accelerator in [
        named(NamedKeyCode::F5),
        chord(KeyValue::Named(NamedKeyCode::Left), ALT),
        chord(KeyValue::Named(NamedKeyCode::Right), ALT),
        chord(KeyValue::Named(NamedKeyCode::Up), ALT),
        chord(KeyValue::Char('N'), ctrl_shift),
    ] {
        assert!(
            route(&mut browser, &mut places, &accelerator).is_none(),
            "{accelerator:?} is the window's"
        );
    }
    assert_eq!(places.cursor(), 2, "Alt+Up climbs the window, not the rail");
    assert!(places.is_focused());
}

/// Alt+Enter asks for the selection's Properties: on a focused rail it
/// navigates nowhere, where a plain Enter would.
#[test]
fn alt_enter_on_the_focused_rail_navigates_nowhere() {
    let mut browser = browser();
    let mut places = places();
    let apps = places
        .index_of(&["Apps".to_string()])
        .expect("the application root is a fixed row");
    places.set_focused(true);
    places.set_cursor(apps);

    let outcome = route(
        &mut browser,
        &mut places,
        &chord(KeyValue::Named(NamedKeyCode::Enter), ALT),
    );

    assert_eq!(outcome, Some(super::SidebarOutcome::QUIET));
    assert!(browser.components().is_empty(), "the browser stayed put");
}

/// The rows a rebuild moves under a still pointer are lit again by the whole
/// repaint that follows it: the row now under the pointer, not the one that
/// was.
#[test]
fn a_rebuilt_rail_lights_the_row_now_under_a_still_pointer() {
    let mut places = places();
    let backup = places.len() - 1;
    let at = row_centre(&places, backup);
    hovered(&mut places, &motion(at));
    assert_eq!(places.hovered(), Some(backup));

    // A volume sorting ahead of Backup takes the row the pointer rests on.
    refresh_places(
        &mut places,
        &home(),
        &[
            Volume {
                label: "Archive".to_string(),
                target: "/Storage/Archive".to_string(),
                medium: None,
            },
            Volume {
                label: "Backup".to_string(),
                target: "/Storage/Backup".to_string(),
                medium: Some(BlkDeviceClass::Rotational),
            },
        ],
    );
    follow_pointer(
        &mut places,
        (WINDOW, Scale::ONE, &Theme::dark()),
        SHOWN,
        Some(at),
    );

    let now = row_under(&places, at).expect("a row is still under the pointer");
    assert_eq!(
        places.rows()[now].label(),
        "Archive",
        "the premise: the rows moved"
    );
    assert_eq!(places.hovered(), Some(now));
}

/// A window that hides its rail lights none of its rows, so showing it again
/// never draws a row the pointer left while it was hidden.
#[test]
fn a_hidden_rail_lights_no_row() {
    let mut places = places();
    let at = row_centre(&places, 1);
    hovered(&mut places, &motion(at));

    let hidden = Chrome {
        rail: false,
        ..SHOWN
    };
    follow_pointer(
        &mut places,
        (WINDOW, Scale::ONE, &Theme::dark()),
        hidden,
        Some(at),
    );
    assert_eq!(places.hovered(), None);

    // And a window whose pointer has not been over it lights nothing either.
    follow_pointer(
        &mut places,
        (WINDOW, Scale::ONE, &Theme::dark()),
        SHOWN,
        None,
    );
    assert_eq!(places.hovered(), None);
}
