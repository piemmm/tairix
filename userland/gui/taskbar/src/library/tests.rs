//! The launcher: what it lists, how it is searched and walked, how it moves,
//! what it asks the session for, and what it repaints.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::window_ipc::AppMenuItemId;
use tairix_controls::damage::Repaint;
use tairix_controls::ChainChild;
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_proglib::{
    BundlePath, Catalog, DisplayName, Edit, EntryId, LibraryCategory, LibraryEntry, Shelf,
};
use tairix_raster::Surface;
use tairix_theme::{Accessibility, Contrast, Motion, Theme};

use super::motion::{CASCADE_RISE, CASCADE_STAGGER_MS, RISE, SINK, SLIDE};
use super::{ItemAt, LibraryFocus, LibraryPage, LibraryPopup, SectionKind, HINT};
use crate::input::{TaskbarInput, TaskbarResponse};
use crate::menu::{EntryRow, MenuSubject};
use crate::render::TaskbarRenderer;
use crate::taskbar::{Taskbar, TaskbarConfig};
use crate::tests::{
    bottom_bar, catalog, centre_of, dark_with_contrast, identity, office_and_games, open_library,
    owes, painted_library, press_at, press_key, test_icon_cache, NOW_NS, SETTLED_NS,
};

const MS: u64 = 1_000_000;

fn id(stem: &str) -> EntryId {
    EntryId::new(&format!("os.tairix.{stem}")).expect("id")
}

/// The bundle directories of the fixture's programs `stems`, as the session
/// hands over what runs.
fn bundles(stems: &[&str]) -> BTreeSet<String> {
    stems
        .iter()
        .map(|stem| format!("/System/Applications/{stem}.app"))
        .collect()
}

/// `count` programs `p0`, `p1`, … named `Program 00`, `Program 01`, … in one
/// folder, so they list in the order they are numbered.
fn numbered(count: usize) -> Catalog {
    let stems: Vec<(String, String)> = (0..count)
        .map(|n| (format!("p{n}"), format!("Program {n:02}")))
        .collect();
    let triples: Vec<(&str, &str, LibraryCategory)> = stems
        .iter()
        .map(|(stem, name)| (stem.as_str(), name.as_str(), LibraryCategory::Accessories))
        .collect();
    catalog(&triples)
}

/// A shelf keeping `favourites` in that order and `frequent` most used first.
fn shelf(favourites: &[&str], frequent: &[&str]) -> Shelf {
    let mut weights = BTreeMap::new();
    for (rank, stem) in frequent.iter().enumerate() {
        let rank = u32::try_from(rank).expect("fits");
        weights.insert(id(stem), 10_000 - rank * 100);
    }
    Shelf {
        favourites: favourites.iter().map(|stem| id(stem)).collect(),
        frequent: frequent.iter().map(|stem| id(stem)).collect(),
        weights,
    }
}

/// A bottom bar on a 1000×800 screen listing `catalog`, settled.
fn bar_over(catalog: Catalog) -> Taskbar {
    let mut bar = bottom_bar();
    bar.set_library_catalog(catalog);
    let _ = bar.take_repaint();
    bar
}

/// The bar's theme with every movement reduced to an immediate change, as
/// the desktop's accessibility setting asks.
fn still() -> Theme {
    let base = Theme::dark().floating();
    let axes = Accessibility {
        contrast: base.contrast(),
        density: base.density(),
        motion: Motion::Reduced,
    };
    base.with_axes(axes)
}

/// What the details strip says over the bar's own layout.
fn details(bar: &Taskbar) -> String {
    bar.library().details(&bar.library_layout(Scale::ONE))
}

fn names(popup: &LibraryPopup) -> Vec<Vec<&str>> {
    popup
        .sections()
        .iter()
        .map(|section| {
            section
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect()
        })
        .collect()
}

fn kinds(popup: &LibraryPopup) -> Vec<SectionKind> {
    popup
        .sections()
        .iter()
        .map(|section| section.kind)
        .collect()
}

fn tabs(popup: &LibraryPopup) -> Vec<(&str, Option<&str>)> {
    popup
        .sidebar()
        .tabs()
        .iter()
        .map(|tab| (tab.label(), tab.reading()))
        .collect()
}

const fn at(section: usize, item: usize) -> ItemAt {
    ItemAt { section, item }
}

fn key_with(
    input: &mut TaskbarInput,
    bar: &mut Taskbar,
    key: Key,
    modifiers: Modifiers,
) -> TaskbarResponse {
    input.handle(
        InputEvent::KeyPressed { key, modifiers },
        bar,
        Scale::ONE,
        NOW_NS,
    )
}

fn named(input: &mut TaskbarInput, bar: &mut Taskbar, key: NamedKey) -> TaskbarResponse {
    press_key(input, bar, Key::Named(key))
}

fn type_text(input: &mut TaskbarInput, bar: &mut Taskbar, text: &str) {
    for ch in text.chars() {
        press_key(input, bar, Key::Char(ch));
    }
}

fn move_to(input: &mut TaskbarInput, bar: &mut Taskbar, point: Point) -> TaskbarResponse {
    input.handle(
        InputEvent::PointerMoved { to: point },
        bar,
        Scale::ONE,
        NOW_NS,
    )
}

fn release(input: &mut TaskbarInput, bar: &mut Taskbar) -> TaskbarResponse {
    input.handle(
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        bar,
        Scale::ONE,
        NOW_NS,
    )
}

/// A primary press and release at `point`, answering the release.
fn click(input: &mut TaskbarInput, bar: &mut Taskbar, point: Point) -> TaskbarResponse {
    let pressed = press_at(input, bar, point.x, point.y);
    assert_eq!(
        pressed,
        TaskbarResponse::Ignored,
        "a press alone decides nothing"
    );
    release(input, bar)
}

fn secondary_at(input: &mut TaskbarInput, bar: &mut Taskbar, point: Point) -> TaskbarResponse {
    move_to(input, bar, point);
    input.handle(
        InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        },
        bar,
        Scale::ONE,
        NOW_NS,
    )
}

/// The centre of the picture of the tile at `item` — a point on the tile
/// whatever its name.
fn picture_of(bar: &Taskbar, item: ItemAt) -> Point {
    let layout = bar.library_layout(Scale::ONE);
    let cell = layout.item_rect(item).expect("the tile is shown");
    centre_of(
        layout
            .tile
            .core_rect(cell, Scale::ONE, bar.theme())
            .expect("the tile has a picture"),
    )
}

/// Rest the pointer on the picture of the tile at `item`.
fn hover_tile(input: &mut TaskbarInput, bar: &mut Taskbar, item: ItemAt) -> TaskbarResponse {
    let point = picture_of(bar, item);
    move_to(input, bar, point)
}

fn tab_centre(bar: &Taskbar, index: usize) -> Point {
    let layout = bar.library_layout(Scale::ONE);
    let strip = layout.sidebar.expect("a sidebar");
    centre_of(
        bar.library()
            .sidebar()
            .tab_rect(index, strip, Scale::ONE, bar.theme())
            .expect("the tab is laid out"),
    )
}

fn settle(bar: &mut Taskbar) {
    bar.advance_library(NOW_NS + SETTLED_NS, Scale::ONE);
    let _ = bar.take_repaint();
}

fn renderer() -> TaskbarRenderer {
    TaskbarRenderer::new(test_icon_cache())
}

/// The surface-local pixels of `rects`, a surface standing at `origin`.
fn local(rects: &[Rect], origin: Point) -> Vec<Rect> {
    rects
        .iter()
        .map(|rect| {
            Rect::new(
                rect.left() - origin.x,
                rect.top() - origin.y,
                rect.width,
                rect.height,
            )
        })
        .collect()
}

/// How many pixels differ between `before` and `after`, and whether every one
/// of them lies inside `allowed`.
fn changed_within(before: &Surface, after: &Surface, allowed: &[Rect]) -> (usize, bool) {
    assert_eq!(
        (before.width(), before.height()),
        (after.width(), after.height())
    );
    let width = usize::try_from(before.width()).expect("fits");
    let mut changed = 0;
    let mut contained = true;
    for (index, (a, b)) in before.pixels().iter().zip(after.pixels()).enumerate() {
        if a == b {
            continue;
        }
        changed += 1;
        let point = Point::new(
            i32::try_from(index % width).expect("fits"),
            i32::try_from(index / width).expect("fits"),
        );
        contained &= allowed.iter().any(|rect| rect.contains(point));
    }
    (changed, contained)
}

// ---- what is listed ----------------------------------------------------

#[test]
fn home_lists_every_program_by_name_and_leaves_empty_sections_out() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let popup = bar.library();
    assert_eq!(kinds(popup), [SectionKind::All]);
    assert_eq!(names(popup), [["Calc", "Chess", "Write"]]);
    assert_eq!(popup.focus(), LibraryFocus::Search);
    assert_eq!(popup.selection(), None);
    assert_eq!(
        tabs(popup),
        [("Home", None), ("Office", Some("2")), ("Games", Some("1"))]
    );
    assert_eq!(popup.sidebar().tabs()[1].group(), Some("Folders"));
    assert_eq!(popup.placeholder(), None);
    assert_eq!(details(&bar), HINT);
}

#[test]
fn favourites_come_first_then_one_row_of_the_frequently_used() {
    let mut bar = bar_over(numbered(10));
    bar.set_library_shelf(shelf(
        &["p3", "p1", "ghost"],
        &["p5", "p6", "p7", "p8", "p9", "p0", "p2"],
    ));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let popup = bar.library();
    assert_eq!(
        kinds(popup),
        [
            SectionKind::Favourites,
            SectionKind::Frequent,
            SectionKind::All
        ]
    );
    assert_eq!(
        names(popup)[0],
        ["Program 03", "Program 01"],
        "kept in the account's order, and a favourite no longer catalogued is not shown"
    );
    assert_eq!(names(popup)[2].len(), 10, "everything is still listed");
    let layout = bar.library_layout(Scale::ONE);
    assert!(layout.columns < 7, "the fixture outnumbers a row");
    assert_eq!(
        layout.sections[1].count, layout.columns,
        "the frequently used take one row, as wide as the pane"
    );
}

#[test]
fn a_folder_tab_lists_its_folder_and_home_comes_back() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);

    let office = tab_centre(&bar, 1);
    assert_eq!(
        click(&mut input, &mut bar, office),
        TaskbarResponse::Ignored
    );
    let popup = bar.library();
    assert_eq!(popup.page(), LibraryPage::Folder(LibraryCategory::Office));
    assert_eq!(kinds(popup), [SectionKind::Folder(LibraryCategory::Office)]);
    assert_eq!(names(popup), [["Calc", "Write"]]);
    assert_eq!(popup.focus(), LibraryFocus::Search, "typing still searches");

    let home = tab_centre(&bar, 0);
    click(&mut input, &mut bar, home);
    assert_eq!(bar.library().page(), LibraryPage::Home);
}

#[test]
fn a_route_reaches_an_entry_in_two_clicks_whatever_the_history() {
    let mut bar = bottom_bar();
    let route = bar
        .library_route_to(&id("calc"), Scale::ONE)
        .expect("calc is catalogued and the sidebar is shown");
    bar.set_library_shelf(shelf(&["chess"], &["write"]));
    assert_eq!(
        bar.library_route_to(&id("calc"), Scale::ONE),
        Some(route),
        "the account's own pages never move the route"
    );
    assert_eq!(bar.library_route_to(&id("absent"), Scale::ONE), None);

    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    click(&mut input, &mut bar, centre_of(route.folder));
    assert_eq!(
        bar.library().page(),
        LibraryPage::Folder(LibraryCategory::Office)
    );
    assert_eq!(
        click(&mut input, &mut bar, centre_of(route.picture)),
        TaskbarResponse::LibraryLaunch { entry: id("calc") }
    );
}

#[test]
fn a_narrow_screen_drops_the_sidebar_and_the_keyboard_skips_it() {
    let mut bar = Taskbar::new(
        TaskbarConfig::bottom_bar(420, 800),
        &Theme::dark().floating(),
    );
    bar.set_library_catalog(office_and_games());
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let layout = bar.library_layout(Scale::ONE);
    assert_eq!(layout.sidebar, None);
    assert!(layout.panel.width <= 420);
    assert_eq!(
        names(bar.library()),
        [["Calc", "Chess", "Write"]],
        "Home lists everything"
    );
    assert_eq!(bar.library_route_to(&id("calc"), Scale::ONE), None);

    named(&mut input, &mut bar, NamedKey::Tab);
    assert_eq!(bar.library().focus(), LibraryFocus::Content);
    named(&mut input, &mut bar, NamedKey::Tab);
    assert_eq!(bar.library().focus(), LibraryFocus::Search);
}

#[test]
fn a_degenerate_screen_lays_out_and_paints_without_panicking() {
    for (width, height) in [(1, 1), (40, 40), (u32::MAX / 2, 30)] {
        let mut bar = Taskbar::new(
            TaskbarConfig::bottom_bar(width, height),
            &Theme::dark().floating(),
        );
        bar.set_library_catalog(numbered(30));
        let mut input = TaskbarInput::new();
        bar.toggle_library(NOW_NS, Scale::ONE);
        let layout = bar.library_layout(Scale::ONE);
        assert!(!layout.panel.is_empty(), "the panel always has a pixel");
        for key in [
            NamedKey::Down,
            NamedKey::End,
            NamedKey::PageUp,
            NamedKey::Tab,
        ] {
            named(&mut input, &mut bar, key);
        }
        if layout.panel.width <= 4096 && layout.panel.height <= 4096 {
            let _ = painted_library(&mut renderer(), &bar, Scale::ONE);
        }
    }
}

// ---- searching ---------------------------------------------------------

#[test]
fn typing_searches_and_enter_opens_the_best_match() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "wr");
    let popup = bar.library();
    assert_eq!(popup.search_text(), "wr");
    assert_eq!(kinds(popup), [SectionKind::Results(None)]);
    assert_eq!(names(popup), [["Write"]]);
    let emphasis = &popup.sections()[0].items[0].emphasis;
    assert_eq!(emphasis.len(), 1);
    assert_eq!(
        emphasis[0],
        0..2,
        "the letters typed are the ones emphasised"
    );
    assert_eq!(popup.selection(), Some(at(0, 0)), "the best match is ready");
    assert_eq!(details(&bar), "Office");
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::LibraryLaunch { entry: id("write") }
    );
    assert!(!bar.library().is_open(), "launching closes the launcher");
}

#[test]
fn a_search_counts_by_folder_and_a_folder_narrows_it() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "c");
    assert_eq!(names(bar.library()), [["Calc", "Chess"]]);
    assert_eq!(
        tabs(bar.library()),
        [
            ("All", Some("2")),
            ("Office", Some("1")),
            ("Games", Some("1"))
        ]
    );

    let games = tab_centre(&bar, 2);
    click(&mut input, &mut bar, games);
    let popup = bar.library();
    assert_eq!(popup.scope(), Some(LibraryCategory::Games));
    assert_eq!(
        kinds(popup),
        [SectionKind::Results(Some(LibraryCategory::Games))]
    );
    assert_eq!(names(popup), [["Chess"]]);
    assert_eq!(popup.selection(), Some(at(0, 0)));
    assert_eq!(
        popup.page(),
        LibraryPage::Home,
        "narrowing a search turns no page"
    );

    type_text(&mut input, &mut bar, "x");
    let popup = bar.library();
    assert!(popup.sections().is_empty());
    assert_eq!(popup.selection(), None);
    assert_eq!(
        popup.placeholder().as_deref(),
        Some("No matching programs in Games")
    );
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::Ignored,
        "nothing found opens nothing"
    );
}

#[test]
fn placeholders_tell_an_empty_library_from_a_fruitless_search() {
    let mut empty = Taskbar::new(
        TaskbarConfig::bottom_bar(1000, 800),
        &Theme::dark().floating(),
    );
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut empty);
    assert_eq!(
        empty.library().placeholder().as_deref(),
        Some("No programs are catalogued")
    );

    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "zzz");
    assert_eq!(
        bar.library().placeholder().as_deref(),
        Some("No matching programs")
    );
}

#[test]
fn escape_clears_the_query_first_and_dismisses_after() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "c");
    named(&mut input, &mut bar, NamedKey::Down);
    assert_eq!(bar.library().focus(), LibraryFocus::Content);

    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Escape),
        TaskbarResponse::Ignored
    );
    let popup = bar.library();
    assert!(popup.is_open());
    assert_eq!(popup.search_text(), "");
    assert_eq!(
        popup.focus(),
        LibraryFocus::Search,
        "the keyboard returns to the search"
    );
    assert_eq!(kinds(popup), [SectionKind::All]);
    assert_eq!(popup.scope(), None);

    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Escape),
        TaskbarResponse::LibraryDismissed
    );
    assert!(!bar.library().is_open());
}

#[test]
fn reopening_starts_afresh() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "c");
    named(&mut input, &mut bar, NamedKey::Tab);
    named(&mut input, &mut bar, NamedKey::Escape);
    bar.toggle_library(NOW_NS, Scale::ONE);
    assert!(!bar.library().is_open());
    bar.toggle_library(NOW_NS, Scale::ONE);
    let popup = bar.library();
    assert_eq!(popup.search_text(), "");
    assert_eq!(popup.page(), LibraryPage::Home);
    assert_eq!(popup.focus(), LibraryFocus::Search);
    assert_eq!(popup.current(), None);
    assert_eq!(bar.library_layout(Scale::ONE).offset, 0);
}

// ---- the keyboard ------------------------------------------------------

#[test]
fn enter_opens_nothing_the_user_has_not_moved_onto() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let elevated = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };
    let menu_key = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::Ignored
    );
    assert_eq!(
        key_with(&mut input, &mut bar, Key::Named(NamedKey::Enter), elevated),
        TaskbarResponse::Ignored
    );
    assert_eq!(
        key_with(
            &mut input,
            &mut bar,
            Key::Named(NamedKey::Function { number: 10 }),
            menu_key
        ),
        TaskbarResponse::Ignored
    );

    // Onto a program and back up to the search: the cursor is kept but no
    // longer drawn, so it is no longer what Enter means.
    named(&mut input, &mut bar, NamedKey::Down);
    named(&mut input, &mut bar, NamedKey::Right);
    named(&mut input, &mut bar, NamedKey::Up);
    let popup = bar.library();
    assert_eq!(popup.focus(), LibraryFocus::Search);
    assert_eq!(popup.current(), Some(at(0, 1)));
    assert_eq!(popup.selection(), None);
    assert_eq!(details(&bar), HINT);
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::Ignored
    );
    assert!(bar.library().is_open());

    named(&mut input, &mut bar, NamedKey::Down);
    assert_eq!(
        bar.library().selection(),
        Some(at(0, 1)),
        "and Down goes back to where it was"
    );
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::LibraryLaunch { entry: id("chess") }
    );
}

#[test]
fn arrows_walk_the_programs_across_sections() {
    let mut bar = bar_over(numbered(10));
    bar.set_library_shelf(shelf(&["p3", "p1"], &["p5", "p6", "p7", "p8", "p9"]));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let walk = |input: &mut TaskbarInput, bar: &mut Taskbar, key| {
        named(input, bar, key);
        bar.library().current()
    };

    assert_eq!(walk(&mut input, &mut bar, NamedKey::Down), Some(at(0, 0)));
    assert_eq!(
        walk(&mut input, &mut bar, NamedKey::Left),
        Some(at(0, 0)),
        "nothing before the first"
    );
    assert_eq!(walk(&mut input, &mut bar, NamedKey::Right), Some(at(0, 1)));
    assert_eq!(
        walk(&mut input, &mut bar, NamedKey::Right),
        Some(at(1, 0)),
        "right from a section's last runs on into the next"
    );
    assert_eq!(
        walk(&mut input, &mut bar, NamedKey::Down),
        Some(at(2, 0)),
        "down crosses into the section below, in the same column"
    );
    assert_eq!(walk(&mut input, &mut bar, NamedKey::Up), Some(at(1, 0)));
    assert_eq!(walk(&mut input, &mut bar, NamedKey::Up), Some(at(0, 0)));
    let columns = bar.library_layout(Scale::ONE).columns;
    assert_eq!(walk(&mut input, &mut bar, NamedKey::End), Some(at(2, 9)));
    assert_eq!(walk(&mut input, &mut bar, NamedKey::Home), Some(at(0, 0)));
    named(&mut input, &mut bar, NamedKey::Down);
    let row_end = at(1, columns.min(5) - 1);
    for _ in 1..columns.min(5) {
        named(&mut input, &mut bar, NamedKey::Right);
    }
    assert_eq!(bar.library().current(), Some(row_end));
    assert_eq!(
        walk(&mut input, &mut bar, NamedKey::Right),
        Some(at(2, 0)),
        "the frequently used row ends where it is drawn"
    );

    named(&mut input, &mut bar, NamedKey::Home);
    assert_eq!(walk(&mut input, &mut bar, NamedKey::Up), Some(at(0, 0)));
    assert_eq!(
        bar.library().focus(),
        LibraryFocus::Search,
        "up from the top returns to the search"
    );
}

#[test]
fn the_keyboard_scrolls_what_it_moves_onto_into_view() {
    let mut bar = bar_over(numbered(60));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let layout = bar.library_layout(Scale::ONE);
    assert!(
        layout.scrollbar.is_some(),
        "sixty programs overflow the pane"
    );

    named(&mut input, &mut bar, NamedKey::Down);
    named(&mut input, &mut bar, NamedKey::End);
    let last = at(0, 59);
    assert_eq!(bar.library().current(), Some(last));
    let layout = bar.library_layout(Scale::ONE);
    assert!(layout.offset > 0);
    let laid = layout.laid(last).expect("laid");
    let shown = layout.item_rect(last).expect("shown");
    assert_eq!(
        (shown.width, shown.height),
        (laid.width, laid.height),
        "wholly in view"
    );

    named(&mut input, &mut bar, NamedKey::Home);
    let layout = bar.library_layout(Scale::ONE);
    assert_eq!(
        layout.offset, 0,
        "the first program's heading comes back into view too"
    );

    named(&mut input, &mut bar, NamedKey::PageDown);
    let after = bar.library().current().expect("a cursor");
    let layout = bar.library_layout(Scale::ONE);
    let moved =
        layout.laid(after).expect("laid").top() - layout.laid(at(0, 0)).expect("laid").top();
    let viewport = i32::try_from(layout.viewport.height).expect("fits");
    let row = i32::try_from(layout.laid(at(0, 0)).expect("laid").height).expect("fits");
    assert!(
        (moved - viewport).abs() <= row,
        "a page down moves about a viewport: moved {moved}, viewport {viewport}"
    );
    assert_eq!(
        layout.laid(after).expect("laid").left(),
        layout.laid(at(0, 0)).expect("laid").left(),
        "in the same column"
    );
}

#[test]
fn tab_cycles_the_search_the_sidebar_and_the_content() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let back = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let focus_after = |input: &mut TaskbarInput, bar: &mut Taskbar, modifiers| {
        key_with(input, bar, Key::Named(NamedKey::Tab), modifiers);
        bar.library().focus()
    };
    let forward = Modifiers::default();
    assert_eq!(
        focus_after(&mut input, &mut bar, forward),
        LibraryFocus::Sidebar
    );
    assert_eq!(
        focus_after(&mut input, &mut bar, forward),
        LibraryFocus::Content
    );
    assert_eq!(
        bar.library().selection(),
        Some(at(0, 0)),
        "arriving in the content puts the cursor down"
    );
    assert_eq!(
        focus_after(&mut input, &mut bar, forward),
        LibraryFocus::Search
    );
    assert_eq!(
        focus_after(&mut input, &mut bar, back),
        LibraryFocus::Content
    );
    assert_eq!(
        focus_after(&mut input, &mut bar, back),
        LibraryFocus::Sidebar
    );
    assert_eq!(
        focus_after(&mut input, &mut bar, back),
        LibraryFocus::Search
    );

    // With nothing listed there is no content to move into.
    type_text(&mut input, &mut bar, "zzz");
    assert_eq!(
        focus_after(&mut input, &mut bar, forward),
        LibraryFocus::Sidebar
    );
    assert_eq!(
        focus_after(&mut input, &mut bar, forward),
        LibraryFocus::Search
    );
}

#[test]
fn the_sidebar_takes_the_arrows_and_hands_typing_to_the_search() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    named(&mut input, &mut bar, NamedKey::Tab);
    let page_after = |input: &mut TaskbarInput, bar: &mut Taskbar, key| {
        named(input, bar, key);
        bar.library().page()
    };
    let office = LibraryPage::Folder(LibraryCategory::Office);
    let games = LibraryPage::Folder(LibraryCategory::Games);
    assert_eq!(page_after(&mut input, &mut bar, NamedKey::Down), office);
    assert_eq!(page_after(&mut input, &mut bar, NamedKey::Down), games);
    assert_eq!(
        page_after(&mut input, &mut bar, NamedKey::Home),
        LibraryPage::Home
    );
    assert_eq!(page_after(&mut input, &mut bar, NamedKey::End), games);
    assert_eq!(bar.library().focus(), LibraryFocus::Sidebar);

    named(&mut input, &mut bar, NamedKey::Right);
    let popup = bar.library();
    assert_eq!(popup.focus(), LibraryFocus::Content);
    assert_eq!(names(popup), [["Chess"]]);
    assert_eq!(popup.selection(), Some(at(0, 0)));

    named(&mut input, &mut bar, NamedKey::Tab);
    named(&mut input, &mut bar, NamedKey::Tab);
    assert_eq!(bar.library().focus(), LibraryFocus::Sidebar);
    press_key(&mut input, &mut bar, Key::Char('w'));
    let popup = bar.library();
    assert_eq!(popup.focus(), LibraryFocus::Search);
    assert_eq!(popup.search_text(), "w");
}

#[test]
fn typing_in_the_content_carries_on_in_the_search() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "c");
    named(&mut input, &mut bar, NamedKey::Down);
    assert_eq!(bar.library().focus(), LibraryFocus::Content);
    // A space carries on a query being typed rather than opening.
    press_key(&mut input, &mut bar, Key::Char(' '));
    type_text(&mut input, &mut bar, "h");
    let popup = bar.library();
    assert_eq!(popup.focus(), LibraryFocus::Search);
    assert_eq!(popup.search_text(), "c h");
    assert_eq!(names(popup), [["Chess"]]);
}

#[test]
fn ctrl_shift_enter_runs_the_selection_as_an_administrator() {
    let mut bar = bottom_bar();
    bar.set_elevation_available(true);
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "ch");
    let elevated = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(
        key_with(&mut input, &mut bar, Key::Named(NamedKey::Enter), elevated),
        TaskbarResponse::LibraryLaunchElevated { entry: id("chess") }
    );
    assert!(!bar.library().is_open());
}

#[test]
fn shift_f10_opens_the_selections_menu_at_its_tile() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    named(&mut input, &mut bar, NamedKey::Down);
    let tile = bar
        .library_layout(Scale::ONE)
        .item_rect(at(0, 0))
        .expect("shown");
    let menu_key = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let TaskbarResponse::OpenMenu(request) = key_with(
        &mut input,
        &mut bar,
        Key::Named(NamedKey::Function { number: 10 }),
        menu_key,
    ) else {
        panic!("Shift+F10 on a program asks for its menu");
    };
    assert_eq!(request.subject, MenuSubject::Entry { entry: id("calc") });
    assert_eq!(request.placement.anchor, tile);
    assert!(
        bar.library().is_open(),
        "the launcher stays behind its menu"
    );
}

// ---- the pointer -------------------------------------------------------

#[test]
fn a_click_opens_a_program_and_a_press_slid_off_it_does_not() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let chess = picture_of(&bar, at(0, 1));
    assert_eq!(
        press_at(&mut input, &mut bar, chess.x, chess.y),
        TaskbarResponse::Ignored
    );
    assert_eq!(bar.library().pressed(), Some(at(0, 1)));
    assert_eq!(
        release(&mut input, &mut bar),
        TaskbarResponse::LibraryLaunch { entry: id("chess") }
    );
    assert!(!bar.library().is_open());

    open_library(&mut input, &mut bar);
    press_at(&mut input, &mut bar, chess.x, chess.y);
    let write = picture_of(&bar, at(0, 2));
    move_to(&mut input, &mut bar, write);
    assert_eq!(release(&mut input, &mut bar), TaskbarResponse::Ignored);
    let popup = bar.library();
    assert!(popup.is_open());
    assert_eq!(popup.pressed(), None);
}

#[test]
fn a_press_beyond_the_panel_dismisses_and_on_its_ground_does_not() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let layout = bar.library_layout(Scale::ONE);
    let footer = centre_of(layout.footer);
    assert_eq!(
        press_at(&mut input, &mut bar, footer.x, footer.y),
        TaskbarResponse::Ignored
    );
    assert!(bar.library().is_open());
    assert!(!layout.panel.contains(Point::new(999, 5)));
    assert_eq!(
        press_at(&mut input, &mut bar, 999, 5),
        TaskbarResponse::LibraryDismissed
    );
    assert!(!bar.library().is_open());
}

#[test]
fn a_secondary_press_on_a_program_asks_for_its_menu() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let write = picture_of(&bar, at(0, 2));
    let TaskbarResponse::OpenMenu(request) = secondary_at(&mut input, &mut bar, write) else {
        panic!("a secondary press on a program asks for its menu");
    };
    assert_eq!(request.subject, MenuSubject::Entry { entry: id("write") });
    assert_eq!(request.model.title(), "Write");
    assert!(bar.library().is_open());

    let footer = centre_of(bar.library_layout(Scale::ONE).footer);
    assert_eq!(
        secondary_at(&mut input, &mut bar, footer),
        TaskbarResponse::Ignored
    );
    assert_eq!(
        secondary_at(&mut input, &mut bar, Point::new(999, 5)),
        TaskbarResponse::LibraryDismissed
    );
}

#[test]
fn a_tile_is_its_picture_and_its_name_never_the_ground_beside_them() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let cell = bar
        .library_layout(Scale::ONE)
        .item_rect(at(0, 0))
        .expect("shown");
    move_to(
        &mut input,
        &mut bar,
        Point::new(cell.left() + 1, cell.top() + 1),
    );
    assert_eq!(bar.library().hover(), None);
    assert_eq!(
        press_at(&mut input, &mut bar, cell.left() + 1, cell.top() + 1),
        TaskbarResponse::Ignored
    );
    assert_eq!(bar.library().pressed(), None);
    release(&mut input, &mut bar);
    hover_tile(&mut input, &mut bar, at(0, 0));
    assert_eq!(bar.library().hover(), Some(at(0, 0)));
}

#[test]
fn the_wheel_scrolls_a_long_library_and_the_hover_follows() {
    let mut bar = bar_over(numbered(60));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let point = picture_of(&bar, at(0, 0));
    move_to(&mut input, &mut bar, point);
    assert_eq!(bar.library().hover(), Some(at(0, 0)));
    input.handle(
        InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT * 4,
        },
        &mut bar,
        Scale::ONE,
        NOW_NS,
    );
    let layout = bar.library_layout(Scale::ONE);
    assert!(layout.offset > 0, "the content scrolled");
    assert_ne!(
        bar.library().hover(),
        Some(at(0, 0)),
        "the first program moved out from under a pointer that did not move"
    );
}

// ---- what it says ------------------------------------------------------

#[test]
fn the_details_strip_says_what_a_program_is_for() {
    let mut bar = bottom_bar();
    bar.set_library_facts(BTreeMap::from([(id("write"), identity("Write"))]));
    assert!(bar.set_library_running(bundles(&["write"]), Scale::ONE));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    hover_tile(&mut input, &mut bar, at(0, 2));
    assert_eq!(
        details(&bar),
        "Does one thing well  \u{b7}  Office  \u{b7}  Running"
    );
    hover_tile(&mut input, &mut bar, at(0, 0));
    assert_eq!(details(&bar), "Office");
    let footer = centre_of(bar.library_layout(Scale::ONE).footer);
    move_to(&mut input, &mut bar, footer);
    assert_eq!(details(&bar), HINT);
}

#[test]
fn a_purpose_finds_a_program_its_name_does_not() {
    let mut bar = bottom_bar();
    let mut purpose = identity("Write");
    purpose.purpose = Some(String::from("Compose letters"));
    bar.set_library_facts(BTreeMap::from([(id("write"), purpose)]));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "letters");
    let popup = bar.library();
    assert_eq!(names(popup), [["Write"]]);
    assert!(
        popup.sections()[0].items[0].emphasis.is_empty(),
        "no letter of the name matched"
    );
}

// ---- what it repaints --------------------------------------------------

#[test]
fn a_hover_repaints_only_the_programs_it_moves_between_and_the_strip() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let layout = bar.library_layout(Scale::ONE);
    let calc = layout.item_rect(at(0, 0)).expect("shown");
    let chess = layout.item_rect(at(0, 1)).expect("shown");

    hover_tile(&mut input, &mut bar, at(0, 0));
    let latched = bar.take_repaint();
    assert_eq!(latched.library, owes(layout.panel, &[calc, layout.footer]));
    assert!(latched.bar.is_clean());

    hover_tile(&mut input, &mut bar, at(0, 1));
    assert_eq!(
        bar.take_repaint().library,
        owes(layout.panel, &[calc, chess, layout.footer])
    );

    hover_tile(&mut input, &mut bar, at(0, 1));
    assert!(
        bar.take_repaint().library.is_clean(),
        "resting still repaints nothing"
    );
}

#[test]
fn what_a_hover_repaints_is_every_pixel_it_changes() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let mut renderer = renderer();
    let before = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    hover_tile(&mut input, &mut bar, at(0, 1));
    let Repaint::Parts(owed) = bar.take_repaint().library else {
        panic!("a hover owes parts, never the whole launcher");
    };
    let after = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    let (changed, contained) = changed_within(&before, &after, owed.rects());
    assert!(changed > 0, "the hover draws");
    assert!(contained, "and draws nothing it did not report");
}

#[test]
fn a_query_repaints_the_content_and_the_sidebar_not_the_chrome() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let layout = bar.library_layout(Scale::ONE);
    press_key(&mut input, &mut bar, Key::Char('c'));
    let Repaint::Parts(owed) = bar.take_repaint().library else {
        panic!("a query owes parts, never the whole launcher");
    };
    let chrome = local(&[layout.panel], layout.panel.origin)[0];
    let title_band = Rect::new(chrome.left(), chrome.top(), chrome.width, 4);
    assert!(
        owed.rects()
            .iter()
            .all(|rect| rect.intersection(&title_band).is_empty()),
        "the panel's own chrome is not repainted for a keystroke"
    );
}

/// A scrollbar that goes widens the content into the gap it stood beside, so
/// that gap is owed as well as the two rectangles either side of it.
#[test]
fn a_scrollbar_that_goes_repaints_the_gap_it_stood_beside() {
    let mut bar = bar_over(numbered(60));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let before = bar.library_layout(Scale::ONE);
    let scrollbar = before.scrollbar.expect("sixty programs overflow");
    let gap = Rect::new(
        before.viewport.right(),
        before.viewport.top(),
        u32::try_from(scrollbar.left() - before.viewport.right()).expect("a gap"),
        before.viewport.height,
    );
    assert!(!gap.is_empty());

    press_key(&mut input, &mut bar, Key::Char('9'));
    assert_eq!(names(bar.library())[0].len(), 6, "six names hold a nine");
    assert!(
        bar.library_layout(Scale::ONE).scrollbar.is_none(),
        "and they fit"
    );
    let Repaint::Parts(owed) = bar.take_repaint().library else {
        panic!("a keystroke owes parts of the launcher, never the whole");
    };
    assert!(
        owed.rects().len() > 1,
        "the account is exact, not a bounding box that would cover the gap anyway"
    );
    let gap = local(&[gap], before.panel.origin)[0];
    assert!(
        tairix_controls::testkit::covers(&owed, gap),
        "the content now draws across the gap the scrollbar stood beside"
    );
}

/// A page turned from the sidebar repaints the content and the two tabs the
/// selection moved between, and leaves the rest of the sidebar alone.
#[test]
fn a_page_turn_repaints_the_content_and_the_two_tabs_alone() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let layout = bar.library_layout(Scale::ONE);
    let strip = layout.sidebar.expect("a sidebar");
    let tab = |index| {
        bar.library()
            .sidebar()
            .tab_rect(index, strip, Scale::ONE, bar.theme())
            .expect("a tab")
    };
    let (home, office, games) = (tab(0), tab(1), tab(2));
    let point = tab_centre(&bar, 2);
    click(&mut input, &mut bar, point);
    let Repaint::Parts(owed) = bar.take_repaint().library else {
        panic!("a page turn owes parts of the launcher, never the whole");
    };
    let shown = local(&[layout.content_column(), home, games], layout.panel.origin);
    for rect in &shown {
        assert!(
            tairix_controls::testkit::covers(&owed, *rect),
            "{rect:?} is owed"
        );
    }
    let office = local(&[office], layout.panel.origin)[0];
    let middle = Rect::new(
        office.left(),
        office.top() + 2,
        office.width,
        office.height - 4,
    );
    assert!(
        owed.rects()
            .iter()
            .all(|rect| rect.intersection(&middle).is_empty()),
        "the tab the selection never touched is left alone"
    );
}

#[test]
fn artwork_repaints_only_the_programs_showing_it() {
    let mut bar = bottom_bar();
    bar.set_library_shelf(shelf(&["write"], &[]));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let layout = bar.library_layout(Scale::ONE);
    let wants = bar.catalog_icon_wants(Scale::ONE);
    assert_eq!(
        wants.len(),
        3,
        "one want per program, though Write is shown twice"
    );
    let write = wants
        .iter()
        .find(|want| want.entry == id("write"))
        .expect("write wants its icon");
    let picture = Surface::new(write.side, write.side).expect("fits");

    bar.set_library_artwork(&id("write"), write.side, &layout, Some(picture.clone()));
    assert_eq!(
        bar.take_repaint().library,
        owes(
            layout.panel,
            &[
                layout.item_rect(at(0, 0)).expect("the favourite"),
                layout.item_rect(at(1, 2)).expect("the listing"),
            ]
        ),
        "both places Write is shown, nothing else"
    );
    bar.set_library_artwork(&id("write"), write.side, &layout, Some(picture.clone()));
    assert!(
        bar.take_repaint().library.is_clean(),
        "the same picture changes nothing"
    );
    assert!(bar.library().artwork(&id("write")).is_some());

    bar.set_library_artwork(&id("calc"), write.side + 1, &layout, Some(picture));
    assert!(
        bar.library().artwork(&id("write")).is_none(),
        "a picture for another side retires every picture made for the old one"
    );
}

#[test]
fn only_shown_programs_want_their_icons() {
    let mut bar = bar_over(numbered(60));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let wants = bar.catalog_icon_wants(Scale::ONE);
    let layout = bar.library_layout(Scale::ONE);
    let shown = layout.shown().count();
    assert!(shown < 60);
    assert_eq!(wants.len(), shown);
    assert!(wants.iter().all(|want| want.side > 0));
}

// ---- motion ------------------------------------------------------------

#[test]
fn the_window_rises_into_place_and_fades_in() {
    let mut bar = bottom_bar();
    bar.toggle_library(NOW_NS, Scale::ONE);
    let rise = i32::try_from(RISE).expect("fits");
    assert_eq!(
        bar.library().placement(),
        (rise, 0),
        "it starts below its place, unseen"
    );
    let refresh = RefreshRate::ASSUMED;
    assert!(bar.library_next_frame_in(NOW_NS, refresh).is_some());

    assert!(bar.advance_library(NOW_NS + 40 * MS, Scale::ONE));
    let (part, strength) = bar.library().placement();
    assert!(0 < part && part < rise, "part-way up: {part}");
    assert!(0 < strength && strength < u8::MAX);

    assert!(bar.advance_library(NOW_NS + SETTLED_NS, Scale::ONE));
    assert_eq!(bar.library().placement(), (0, u8::MAX));
    assert_eq!(
        bar.library_next_frame_in(NOW_NS + SETTLED_NS, refresh),
        None
    );
    assert!(
        !bar.advance_library(NOW_NS + SETTLED_NS + MS, Scale::ONE),
        "a settled window asks for no frame"
    );
}

#[test]
fn reduced_motion_lands_every_movement_at_once() {
    let mut bar = bottom_bar();
    bar.apply_theme(&still());
    let refresh = RefreshRate::ASSUMED;
    bar.toggle_library(NOW_NS, Scale::ONE);
    let popup = bar.library();
    assert_eq!(popup.placement(), (0, u8::MAX));
    assert_eq!(popup.entrance_frame(0), None);
    assert_eq!(bar.library_next_frame_in(NOW_NS, refresh), None);

    let mut input = TaskbarInput::new();
    named(&mut input, &mut bar, NamedKey::Down);
    named(&mut input, &mut bar, NamedKey::Right);
    assert_eq!(
        bar.library_next_frame_in(NOW_NS, refresh),
        None,
        "the mark moves at once"
    );
    let mut announced = false;
    bar.report_library_shown(|| announced = true);
    assert!(announced, "a window with nothing to rise is shown at once");

    bar.toggle_library(NOW_NS, Scale::ONE);
    assert!(!bar.library().is_departing(), "and gone at once");
}

#[test]
fn closing_sinks_the_window_away_and_reopening_resumes_where_it_stands() {
    let mut bar = bottom_bar();
    bar.toggle_library(NOW_NS, Scale::ONE);
    settle(&mut bar);
    let closed_at = NOW_NS + SETTLED_NS;
    bar.toggle_library(closed_at, Scale::ONE);
    assert!(bar.library().is_departing());
    assert!(bar.advance_library(closed_at + 30 * MS, Scale::ONE));
    let (sunk, faded) = bar.library().placement();
    let depth = i32::try_from(SINK).expect("fits");
    assert!(0 < sunk && sunk < depth, "part-way down: {sunk}");
    assert!(0 < faded && faded < u8::MAX);

    bar.toggle_library(closed_at + 30 * MS, Scale::ONE);
    assert!(!bar.library().is_departing());
    assert_eq!(
        bar.library().placement(),
        (sunk, faded),
        "reopening carries on from what is on screen, never a jump"
    );

    bar.toggle_library(closed_at + 40 * MS, Scale::ONE);
    let _ = bar.take_repaint();
    assert!(bar.advance_library(closed_at + SETTLED_NS, Scale::ONE));
    assert!(!bar.library().is_departing());
    assert!(
        !bar.take_repaint().library.is_clean(),
        "the window's removal is owed once it has gone"
    );
    assert!(!bar.advance_library(closed_at + SETTLED_NS + MS, Scale::ONE));
}

#[test]
fn a_departure_finished_early_lands_at_its_end_and_owes_the_removal() {
    let mut bar = bottom_bar();
    bar.finish_library_departure();
    assert!(
        bar.take_repaint().library.is_clean(),
        "a closed launcher owes nothing"
    );

    bar.toggle_library(NOW_NS, Scale::ONE);
    settle(&mut bar);
    bar.finish_library_departure();
    assert!(bar.library().is_open(), "an open launcher is not leaving");
    assert!(bar.take_repaint().library.is_clean());

    bar.toggle_library(NOW_NS + SETTLED_NS, Scale::ONE);
    let _ = bar.take_repaint();
    assert!(bar.library().is_departing());
    bar.finish_library_departure();
    assert!(!bar.library().is_departing());
    assert!(
        !bar.take_repaint().library.is_clean(),
        "its removal is owed"
    );
    let depth = i32::try_from(SINK).expect("fits");
    assert_eq!(
        bar.library().placement(),
        (depth, 0),
        "at the end it was leaving for"
    );
    assert_eq!(
        bar.library_next_frame_in(NOW_NS + SETTLED_NS, RefreshRate::ASSUMED),
        None
    );
}

#[test]
fn the_first_page_cascades_in_one_program_after_another() {
    let mut bar = bar_over(numbered(6));
    bar.toggle_library(NOW_NS, Scale::ONE);
    let cascade = i32::try_from(CASCADE_RISE).expect("fits");
    let first = bar.library().entrance_frame(0).expect("an entrance");
    assert_eq!((first.strength, first.rise), (0, cascade));

    let stagger = u64::from(CASCADE_STAGGER_MS) * MS;
    bar.advance_library(NOW_NS + 3 * stagger, Scale::ONE);
    let popup = bar.library();
    let frames: Vec<u8> = (0..4)
        .map(|order| {
            popup
                .entrance_frame(order)
                .expect("still arriving")
                .strength
        })
        .collect();
    assert!(
        frames.windows(2).all(|pair| pair[0] > pair[1]),
        "each program sets off after the one before it: {frames:?}"
    );
    assert_eq!(frames[3], 0, "the fourth has only just set off");

    bar.advance_library(NOW_NS + SETTLED_NS, Scale::ONE);
    assert_eq!(
        bar.library().entrance_frame(0),
        None,
        "and the entrance ends"
    );
}

#[test]
fn a_page_slides_in_from_the_side_of_the_sidebar_it_lies_on() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let slide = i32::try_from(SLIDE).expect("fits");
    named(&mut input, &mut bar, NamedKey::Tab);
    named(&mut input, &mut bar, NamedKey::End);
    assert_eq!(
        bar.library().entrance_frame(0).map(|frame| frame.rise),
        Some(slide),
        "a page further down arrives from below"
    );
    named(&mut input, &mut bar, NamedKey::Home);
    assert_eq!(
        bar.library().entrance_frame(0).map(|frame| frame.rise),
        Some(-slide),
        "and one further up from above"
    );
}

#[test]
fn results_arrive_once_and_then_hold_still_while_typing() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    press_key(&mut input, &mut bar, Key::Char('c'));
    assert!(
        bar.library().entrance_frame(0).is_some(),
        "browsing turned into results"
    );
    press_key(&mut input, &mut bar, Key::Char('h'));
    assert_eq!(
        bar.library().entrance_frame(0),
        None,
        "a refined query never animates"
    );
}

#[test]
fn the_keyboards_mark_fades_across_and_repaints_only_its_two_tiles() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    named(&mut input, &mut bar, NamedKey::Down);
    settle(&mut bar);
    named(&mut input, &mut bar, NamedKey::Right);
    let popup = bar.library();
    assert_eq!(popup.mark_strength(SectionKind::All, &id("chess")), Some(0));
    assert_eq!(
        popup.mark_strength(SectionKind::All, &id("calc")),
        Some(u8::MAX)
    );
    assert_eq!(popup.mark_strength(SectionKind::All, &id("write")), None);
    let _ = bar.take_repaint();

    let layout = bar.library_layout(Scale::ONE);
    assert!(bar.advance_library(NOW_NS + 20 * MS, Scale::ONE));
    assert_eq!(
        bar.take_repaint().library,
        owes(
            layout.panel,
            &[
                layout.item_rect(at(0, 0)).expect("shown"),
                layout.item_rect(at(0, 1)).expect("shown"),
            ]
        )
    );
    let arriving = bar
        .library()
        .mark_strength(SectionKind::All, &id("chess"))
        .expect("still moving");
    assert!(0 < arriving && arriving < u8::MAX);

    bar.advance_library(NOW_NS + SETTLED_NS, Scale::ONE);
    assert_eq!(
        bar.library().mark_strength(SectionKind::All, &id("chess")),
        None
    );
}

#[test]
fn the_entrance_is_one_painted_layer_blended_into_place() {
    let mut bar = bottom_bar();
    bar.toggle_library(NOW_NS, Scale::ONE);
    let mut renderer = renderer();
    bar.advance_library(NOW_NS + 30 * MS, Scale::ONE);
    let entering = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    bar.advance_library(NOW_NS + SETTLED_NS, Scale::ONE);
    let arrived = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    let fresh = painted_library(&mut self::renderer(), &bar, Scale::ONE).expect("open");
    assert_ne!(
        entering.pixels(),
        arrived.pixels(),
        "a frame part-way in differs"
    );
    assert_eq!(
        arrived.pixels(),
        fresh.pixels(),
        "and the arrived frame is the content painted at rest"
    );
}

// ---- its menu ----------------------------------------------------------

/// The labels of the entry menu a secondary press on `item` asks for.
fn menu_labels(bar: &mut Taskbar, item: ItemAt) -> Vec<String> {
    let mut input = TaskbarInput::new();
    if !bar.library().is_open() {
        open_library(&mut input, bar);
    }
    let point = picture_of(bar, item);
    let TaskbarResponse::OpenMenu(request) = secondary_at(&mut input, bar, point) else {
        panic!("a program offers a menu");
    };
    request
        .model
        .rows()
        .iter()
        .map(|row| String::from(row.drawn().label()))
        .collect()
}

#[test]
fn an_entry_menu_offers_what_the_account_can_do_with_the_program() {
    let mut bar = bottom_bar();
    assert_eq!(
        menu_labels(&mut bar, at(0, 0)),
        ["Open", "Add to Favourites", "Create Desktop Shortcut"]
    );

    bar.set_elevation_available(true);
    // Calc has been used; a favourite is never also among the frequent.
    let mut kept = shelf(&["calc", "chess", "write"], &[]);
    kept.weights.insert(id("calc"), 1_000);
    bar.set_library_shelf(kept);
    // Favourites: Calc, Chess, Write; the listing below them.
    assert_eq!(
        menu_labels(&mut bar, at(0, 1)),
        [
            "Open",
            "Run as Administrator\u{2026}",
            "Remove from Favourites",
            "Move Earlier",
            "Move Later",
            "Create Desktop Shortcut",
        ]
    );
    assert_eq!(
        menu_labels(&mut bar, at(0, 0)),
        [
            "Open",
            "Run as Administrator\u{2026}",
            "Remove from Favourites",
            "Move Later",
            "Remove from Frequently Used",
            "Clear Frequently Used",
            "Create Desktop Shortcut",
        ],
        "the first favourite has nowhere earlier to go"
    );
}

#[test]
fn an_entry_menu_closes_with_the_programs_own_information() {
    let mut bar = bottom_bar();
    bar.set_library_facts(BTreeMap::from([(id("calc"), identity("Calc"))]));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    let point = picture_of(&bar, at(0, 0));
    let TaskbarResponse::OpenMenu(request) = secondary_at(&mut input, &mut bar, point) else {
        panic!("a program offers a menu");
    };
    let last = request.model.rows().last().expect("rows");
    assert!(matches!(last.child(), ChainChild::Info(_)));
    let point = picture_of(&bar, at(0, 1));
    let TaskbarResponse::OpenMenu(request) = secondary_at(&mut input, &mut bar, point) else {
        panic!("a program offers a menu");
    };
    assert!(
        request
            .model
            .rows()
            .iter()
            .all(|row| !matches!(row.child(), ChainChild::Info(_))),
        "a program whose manifest was not read states nothing about itself"
    );
}

#[test]
fn each_entry_row_asks_for_its_own_response_and_only_outward_ones_close() {
    let entry = id("calc");
    let subject = MenuSubject::Entry {
        entry: entry.clone(),
    };
    let expected = [
        (
            EntryRow::Open,
            TaskbarResponse::LibraryLaunch {
                entry: entry.clone(),
            },
            true,
        ),
        (
            EntryRow::RunAsAdministrator,
            TaskbarResponse::LibraryLaunchElevated {
                entry: entry.clone(),
            },
            true,
        ),
        (
            EntryRow::Favourite,
            TaskbarResponse::LibraryEdit(Edit::Favourite(entry.clone())),
            false,
        ),
        (
            EntryRow::Unfavourite,
            TaskbarResponse::LibraryEdit(Edit::Unfavourite(entry.clone())),
            false,
        ),
        (
            EntryRow::MoveEarlier,
            TaskbarResponse::LibraryEdit(Edit::MoveEarlier(entry.clone())),
            false,
        ),
        (
            EntryRow::MoveLater,
            TaskbarResponse::LibraryEdit(Edit::MoveLater(entry.clone())),
            false,
        ),
        (
            EntryRow::Forget,
            TaskbarResponse::LibraryEdit(Edit::Forget(entry.clone())),
            false,
        ),
        (
            EntryRow::ForgetAll,
            TaskbarResponse::LibraryEdit(Edit::ForgetAll),
            false,
        ),
        (
            EntryRow::Shortcut,
            TaskbarResponse::CreateDesktopShortcut {
                entry: entry.clone(),
            },
            true,
        ),
    ];
    let rows = expected.len();
    for (index, (row, response, closes)) in expected.into_iter().enumerate() {
        let mut bar = bottom_bar();
        bar.toggle_library(NOW_NS, Scale::ONE);
        let item = AppMenuItemId::for_index(index).expect("an id");
        assert_eq!(
            bar.menu_chosen(&subject, item, NOW_NS, Scale::ONE),
            Some(response),
            "{row:?}"
        );
        assert_eq!(!bar.library().is_open(), closes, "{row:?}");
    }
    let past = AppMenuItemId::for_index(rows).expect("an id");
    let mut bar = bottom_bar();
    assert_eq!(
        bar.menu_chosen(&subject, past, NOW_NS, Scale::ONE),
        None,
        "fail closed"
    );
}

// ---- how it looks ------------------------------------------------------

#[test]
fn a_running_program_wears_its_live_mark_on_its_tile_alone() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let mut renderer = renderer();
    let idle = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    let layout = bar.library_layout(Scale::ONE);
    let tile = layout.item_rect(at(0, 0)).expect("shown");
    assert!(bar.set_library_running(bundles(&["calc"]), Scale::ONE));
    assert_eq!(
        bar.take_repaint().library,
        owes(layout.panel, &[tile]),
        "a program starting owes its own tile, nothing else"
    );
    assert!(
        !bar.set_library_running(bundles(&["calc"]), Scale::ONE),
        "the same set again changes nothing"
    );
    assert!(bar.take_repaint().library.is_clean());
    let running = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    let calc = local(&[tile], layout.panel.origin);
    let (changed, contained) = changed_within(&idle, &running, &calc);
    assert!(changed > 0, "the mark draws");
    assert!(contained, "on Calc's tile alone");
}

#[test]
fn a_program_starting_under_the_pointer_owes_its_tile_and_the_details_strip() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    hover_tile(&mut input, &mut bar, at(0, 0));
    let _ = bar.take_repaint();
    let layout = bar.library_layout(Scale::ONE);
    let tile = layout.item_rect(at(0, 0)).expect("shown");
    assert!(bar.set_library_running(bundles(&["calc"]), Scale::ONE));
    assert_eq!(
        bar.take_repaint().library,
        owes(layout.panel, &[tile, layout.footer])
    );
    assert_eq!(details(&bar), "Office  \u{b7}  Running");
}

#[test]
fn a_program_the_catalog_does_not_list_starting_moves_no_mark() {
    let mut bar = bottom_bar();
    assert!(!bar.set_library_running(bundles(&["draw"]), Scale::ONE));
    assert!(bar.take_repaint().library.is_clean());
}

#[test]
fn a_catalog_handed_later_marks_the_programs_already_running() {
    let mut bar = bottom_bar();
    assert!(!bar.set_library_running(bundles(&["draw"]), Scale::ONE));
    bar.set_library_catalog(catalog(&[("draw", "Draw", LibraryCategory::Graphics)]));
    assert!(bar.library().is_running(&id("draw")));
    bar.set_library_catalog(office_and_games());
    assert!(
        !bar.library().is_running(&id("draw")),
        "an entry gone is no longer marked"
    );
}

#[test]
fn every_entry_starting_a_running_bundle_is_marked() {
    let mut twice = office_and_games();
    twice
        .insert(LibraryEntry::new(
            id("calc.sheets"),
            DisplayName::new("Sheets").expect("name"),
            BundlePath::new("/System/Applications/calc.app").expect("bundle"),
            LibraryCategory::Office,
            None,
        ))
        .expect("fits");
    let mut bar = bar_over(twice);
    assert!(bar.set_library_running(bundles(&["calc"]), Scale::ONE));
    let popup = bar.library();
    assert!(popup.is_running(&id("calc")) && popup.is_running(&id("calc.sheets")));
    assert!(!popup.is_running(&id("write")));
}

#[test]
fn a_searchs_rows_emphasise_what_matched() {
    let paint_query = |query: &str| {
        let mut bar = bottom_bar();
        let mut input = TaskbarInput::new();
        open_library(&mut input, &mut bar);
        type_text(&mut input, &mut bar, query);
        settle(&mut bar);
        let layout = bar.library_layout(Scale::ONE);
        let parts = local(
            &[layout.search, layout.item_rect(at(0, 0)).expect("shown")],
            layout.panel.origin,
        );
        (
            painted_library(&mut renderer(), &bar, Scale::ONE).expect("open"),
            parts,
        )
    };
    let (one, parts) = paint_query("w");
    let (two, _) = paint_query("wr");
    let (_, only_the_query) = changed_within(&one, &two, &parts[..1]);
    assert!(
        !only_the_query,
        "a longer match emphasises more of the name"
    );
    let (_, contained) = changed_within(&one, &two, &parts);
    assert!(contained, "and only in the row it is drawn in");
}

#[test]
fn every_contrast_paints_the_launcher() {
    for contrast in [Contrast::Normal, Contrast::High, Contrast::Monochrome] {
        let mut bar = bottom_bar();
        bar.apply_theme(&dark_with_contrast(contrast));
        let mut input = TaskbarInput::new();
        open_library(&mut input, &mut bar);
        type_text(&mut input, &mut bar, "c");
        named(&mut input, &mut bar, NamedKey::Down);
        settle(&mut bar);
        let surface = painted_library(&mut renderer(), &bar, Scale::ONE).expect("open");
        let first = surface.pixels()[0];
        assert!(
            surface.pixels().iter().any(|pixel| *pixel != first),
            "{contrast:?} draws something"
        );
    }
}

#[test]
fn a_launcher_leaving_the_screen_keeps_the_pixels_it_had() {
    let mut bar = bottom_bar();
    bar.toggle_library(NOW_NS, Scale::ONE);
    settle(&mut bar);
    let mut renderer = renderer();
    let mut shown = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    let kept = shown.clone();
    bar.toggle_library(NOW_NS + SETTLED_NS, Scale::ONE);
    assert!(bar.library().is_departing());
    let whole = Rect::new(0, 0, shown.width(), shown.height());
    renderer.paint_library(&bar, Scale::ONE, &mut shown, &[whole]);
    assert_eq!(shown.pixels(), kept.pixels());
}

// ---- what a change repaints when it cuts another short -------------------

/// Whether `owed` repaints every pixel of the screen rectangle `rect` of a
/// surface laid out at `panel`.
fn covers(owed: &Repaint, panel: Rect, rect: Rect) -> bool {
    match owed {
        Repaint::Whole => true,
        Repaint::Parts(region) => {
            let mut left = tairix_geometry::Region::new();
            left.add(local(&[rect], panel.origin)[0]);
            for owed in region.rects() {
                left.subtract(*owed);
            }
            left.is_empty()
        }
    }
}

/// Press `key` at `now_ns`.
fn named_at(input: &mut TaskbarInput, bar: &mut Taskbar, key: NamedKey, now_ns: u64) {
    input.handle(
        InputEvent::KeyPressed {
            key: Key::Named(key),
            modifiers: Modifiers::default(),
        },
        bar,
        Scale::ONE,
        now_ns,
    );
}

/// Turn the wheel `detents` toward the content's end over the pointer.
fn wheel(input: &mut TaskbarInput, bar: &mut Taskbar, detents: i32) {
    input.handle(
        InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT * detents,
        },
        bar,
        Scale::ONE,
        NOW_NS,
    );
}

#[test]
fn a_key_that_ends_the_cascade_repaints_the_content_it_cut_short() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    bar.advance_library(NOW_NS + 30 * MS, Scale::ONE);
    assert!(
        bar.library().entrance_frame(0).is_some(),
        "the cascade is under way"
    );
    let _ = bar.take_repaint();
    named_at(&mut input, &mut bar, NamedKey::Down, NOW_NS + 30 * MS);
    assert_eq!(bar.library().entrance_frame(0), None, "the key ends it");
    let layout = bar.library_layout(Scale::ONE);
    assert!(covers(
        &bar.take_repaint().library,
        layout.panel,
        layout.viewport
    ));
}

#[test]
fn a_mark_cut_short_repaints_the_tile_it_left_and_hands_on_its_strength() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    named(&mut input, &mut bar, NamedKey::Down);
    settle(&mut bar);
    named_at(&mut input, &mut bar, NamedKey::Right, NOW_NS + SETTLED_NS);
    let midway_ns = NOW_NS + SETTLED_NS + 50 * MS;
    bar.advance_library(midway_ns, Scale::ONE);
    let midway = bar
        .library()
        .mark_strength(SectionKind::All, &id("chess"))
        .expect("still moving onto Chess");
    assert!(0 < midway && midway < u8::MAX);
    let _ = bar.take_repaint();

    named_at(&mut input, &mut bar, NamedKey::Right, midway_ns);
    let layout = bar.library_layout(Scale::ONE);
    let calc = layout.item_rect(at(0, 0)).expect("shown");
    assert!(
        covers(&bar.take_repaint().library, layout.panel, calc),
        "the tile the cut-short mark was leaving redraws at rest"
    );
    let popup = bar.library();
    assert_eq!(popup.mark_strength(SectionKind::All, &id("calc")), None);
    assert_eq!(
        popup.mark_strength(SectionKind::All, &id("chess")),
        Some(midway),
        "Chess fades from the strength it was drawn at, never from full"
    );
}

#[test]
fn the_scrollbar_repaints_as_the_keyboard_moves_into_and_out_of_the_content() {
    let mut bar = bar_over(numbered(40));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    let layout = bar.library_layout(Scale::ONE);
    let scrollbar = layout.scrollbar.expect("the content overflows");
    named(&mut input, &mut bar, NamedKey::Down);
    assert!(covers(&bar.take_repaint().library, layout.panel, scrollbar));
    named(&mut input, &mut bar, NamedKey::Up);
    assert_eq!(bar.library().focus(), LibraryFocus::Search);
    assert!(covers(&bar.take_repaint().library, layout.panel, scrollbar));
}

#[test]
fn the_scrollbar_is_drawn_to_the_contents_own_range_before_any_event() {
    let mut bar = bar_over(numbered(40));
    bar.toggle_library(NOW_NS, Scale::ONE);
    let layout = bar.library_layout(Scale::ONE);
    let range = bar.library().scrollbar_for(&layout).model().range();
    assert_eq!(
        (range.content_extent(), range.viewport_extent()),
        (
            u64::from(layout.content_height),
            u64::from(layout.viewport.height)
        )
    );
}

#[test]
fn the_keyboard_stays_on_its_program_when_the_shelf_reorders_it() {
    let mut bar = bar_over(numbered(10));
    bar.set_library_shelf(shelf(&["p1", "p2"], &[]));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    named(&mut input, &mut bar, NamedKey::Down);
    named(&mut input, &mut bar, NamedKey::Right);
    assert_eq!(bar.library().current(), Some(at(0, 1)));
    bar.set_library_shelf(shelf(&["p2", "p1"], &[]));
    assert_eq!(
        bar.library().current(),
        Some(at(0, 0)),
        "the cursor follows Program 02 to its new place"
    );
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::LibraryLaunch { entry: id("p2") }
    );
}

#[test]
fn a_program_gone_from_its_section_takes_the_cursor_with_it() {
    let mut bar = bar_over(numbered(10));
    bar.set_library_shelf(shelf(&["p1", "p2"], &[]));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    named(&mut input, &mut bar, NamedKey::Down);
    named(&mut input, &mut bar, NamedKey::Right);
    bar.set_library_shelf(shelf(&["p1"], &[]));
    assert_eq!(
        bar.library().current(),
        None,
        "nothing else is chosen in its place"
    );
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::Ignored
    );
}

#[test]
fn ctrl_shift_enter_asks_for_nothing_where_no_broker_could_answer() {
    let mut bar = bottom_bar();
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    type_text(&mut input, &mut bar, "wr");
    let elevated = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(
        key_with(&mut input, &mut bar, Key::Named(NamedKey::Enter), elevated),
        TaskbarResponse::Ignored
    );
    assert!(bar.library().is_open(), "the launcher stays as it was");
    bar.set_elevation_available(true);
    assert_eq!(
        key_with(&mut input, &mut bar, Key::Named(NamedKey::Enter), elevated),
        TaskbarResponse::LibraryLaunchElevated { entry: id("write") }
    );
}

#[test]
fn a_key_on_a_selection_scrolled_out_of_sight_brings_it_back_before_acting() {
    let mut bar = bar_over(numbered(60));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    named(&mut input, &mut bar, NamedKey::Down);
    hover_tile(&mut input, &mut bar, at(0, 1));
    wheel(&mut input, &mut bar, 6);
    let search = centre_of(bar.library_layout(Scale::ONE).search);
    move_to(&mut input, &mut bar, search);
    assert_eq!(
        bar.library_layout(Scale::ONE).item_rect(at(0, 0)),
        None,
        "the cursor is scrolled out of sight"
    );
    assert_eq!(bar.library().focus(), LibraryFocus::Content);
    assert_eq!(
        details(&bar),
        HINT,
        "the strip names no program it cannot show"
    );
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::Ignored,
        "the first Enter brings it back"
    );
    assert!(bar.library_layout(Scale::ONE).item_rect(at(0, 0)).is_some());
    assert_eq!(
        named(&mut input, &mut bar, NamedKey::Enter),
        TaskbarResponse::LibraryLaunch { entry: id("p0") }
    );
}

#[test]
fn the_layout_visits_only_the_rows_in_view_and_finds_each_program_by_its_place() {
    let mut bar = bar_over(numbered(100));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    wheel(&mut input, &mut bar, 3);
    let layout = bar.library_layout(Scale::ONE);
    assert!(layout.offset > 0);
    let shown: Vec<(ItemAt, Rect)> = layout.shown().collect();
    let tile = shown.first().expect("a program in view").1;
    let rows = layout.viewport.height / tile.height + 2;
    assert!(
        shown.len() <= usize::try_from(rows).expect("fits") * layout.columns,
        "{} visited for {rows} rows",
        shown.len()
    );
    for (at, rect) in &shown {
        assert_eq!(
            layout.cell_at(centre_of(*rect)),
            Some((*at, *rect)),
            "{at:?}"
        );
        assert_eq!(layout.laid(*at), Some(*rect));
    }
    let (first, _) = shown[0];
    let gap = Point::new(
        layout.laid(first).expect("laid").right(),
        centre_of(layout.laid(first).expect("laid")).y,
    );
    assert_eq!(
        layout.cell_at(gap),
        None,
        "the gap beside a tile is no program"
    );
}

#[test]
fn a_part_repainted_alone_lays_exactly_what_the_whole_paint_laid_there() {
    let mut bar = bar_over(numbered(40));
    let mut input = TaskbarInput::new();
    open_library(&mut input, &mut bar);
    settle(&mut bar);
    hover_tile(&mut input, &mut bar, at(0, 2));
    wheel(&mut input, &mut bar, 1);
    let layout = bar.library_layout(Scale::ONE);
    assert!(
        layout.offset > 0,
        "scrolled, so content and screen space differ"
    );
    let (_, tile) = layout.shown().next().expect("a program in view");
    let parts = [
        layout.view().to_window(tile).expect("on screen"),
        layout.footer,
        layout.search,
        layout.scrollbar.expect("the content overflows"),
        Rect::new(
            layout.viewport.left() - 12,
            layout.viewport.top() + 5,
            60,
            80,
        ),
    ];
    let mut renderer = renderer();
    let whole = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    for part in parts {
        let mut again = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
        renderer.paint_library(
            &bar,
            Scale::ONE,
            &mut again,
            &local(&[part], layout.panel.origin),
        );
        assert!(again.pixels() == whole.pixels(), "{part:?}");
    }
}

#[test]
fn a_part_of_the_cascade_repainted_alone_is_the_whole_frame_there() {
    let mut bar = bottom_bar();
    bar.toggle_library(NOW_NS, Scale::ONE);
    bar.advance_library(NOW_NS + 30 * MS, Scale::ONE);
    assert!(bar.library().entrance_frame(0).is_some());
    let layout = bar.library_layout(Scale::ONE);
    let heading = layout.sections[0].heading;
    let parts = [
        layout.item_rect(at(0, 1)).expect("shown"),
        layout.view().to_window(heading).expect("on screen"),
        layout.viewport,
    ];
    let mut renderer = renderer();
    let whole = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
    for part in parts {
        let mut again = painted_library(&mut renderer, &bar, Scale::ONE).expect("open");
        renderer.paint_library(
            &bar,
            Scale::ONE,
            &mut again,
            &local(&[part], layout.panel.origin),
        );
        assert!(again.pixels() == whole.pixels(), "{part:?}");
    }
}
