//! Unit tests for the settings shell.
//!
//! These cover what the shell exists to get right: the frame's regions and
//! what a narrow window sheds, the strip's shape and the cursor reaching
//! every row of it, the search filtering the strip, the trail rewriting
//! itself, the category list a shed strip becomes, and the scroll a pane too
//! tall for its column gets.

use alloc::vec::Vec;

use tairix_abi::blkio::BlkDeviceClass;
use tairix_abi::desktop::{Appearance, Contrast, Density};
use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::sysinfo::{MountAvailability, MountRecord, MountVolumeState};
use tairix_controls::testkit::keystroke;
use tairix_controls::{ground_fill, plate_border, ChromeLayer, FieldGroup, WHEEL_STEP};
use tairix_font::install_test_transport;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{CursorSetId, Theme, ThemeRegistry};
use tairix_wallpaper::{DesktopSettings, SettingsKey};

use crate::form::{Composition, FormPlace, Setting};
use crate::frame::{resolve_frame, Actions, Overflow, CONTENT_FLOOR, SIDEBAR_WIDTH};
use crate::pictures::Chooser;
use crate::registry::{Category, Location, Pane, PaneContent, StripRow, CATEGORIES};
use crate::saver::SaverOption;
use crate::shell::{Shell, ShellOutcome};
use crate::test_support::{click, clicked, damage, opaque, theme, WIDE};
use crate::volumes::VolumeReading;
use tairix_controls::testkit::covers;

/// A window too narrow to seat the strip at all.
const NARROW: Rect = Rect::new(0, 0, 360, 640);

fn shell() -> Shell {
    Shell::new(DesktopSettings::default()).expect("the registry holds a category")
}

/// A shell showing a pane that states an absence, which is the body that
/// scrolls by pixels — the one a wrapped-prose column is measured against.
fn stating() -> Shell {
    let mut shell = shell();
    let mut sink = damage();
    assert!(
        shell.go_to_pane("sound", WIDE, Scale::ONE, &theme(), &mut sink),
        "the registry carries the pane that states the absent sound controls"
    );
    shell
}

/// The centre of strip row `index`, or `None` when the strip did not seat it.
fn row_point(shell: &Shell, index: usize, viewport: Rect, theme: &Theme) -> Option<Point> {
    let rect = shell.strip_row_rect(index, viewport, Scale::ONE, theme)?;
    Some(Point::new(
        rect.left() + to_i32(rect.width / 2),
        rect.top() + to_i32(rect.height / 2),
    ))
}

// --- The frame ----------------------------------------------------------

/// Whether `inner` lies wholly within `outer`.
fn within(inner: Rect, outer: Rect) -> bool {
    inner.left() >= outer.left()
        && inner.top() >= outer.top()
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

#[test]
fn a_wide_window_seats_every_region() {
    let theme = theme();
    let frame = resolve_frame(WIDE, Scale::ONE, &theme, Overflow::default(), Actions::None);
    let panel = frame.panel.expect("a panel");
    let search = frame.search.expect("a search field");
    let sidebar = frame.sidebar.expect("a strip");
    // The search field and the strip are both on the panel, the field above.
    assert!(within(search, panel), "{search:?} leaves {panel:?}");
    assert!(within(sidebar, panel), "{sidebar:?} leaves {panel:?}");
    assert!(search.bottom() <= sidebar.top());
    // The panel stands a gap in from the window, as a pane's plates stand in
    // from their column, and the columns do not overlap.
    let gap = Scale::ONE.scale_length(theme.metrics().control_gap).max(1);
    assert_eq!(panel.left(), WIDE.left() + to_i32(gap));
    assert_eq!(panel.top(), WIDE.top() + to_i32(gap));
    assert_eq!(panel.bottom(), WIDE.bottom() - to_i32(gap));
    assert_eq!(frame.breadcrumb.bottom(), frame.content.top());
    assert!(frame.content.left() >= panel.right());
    assert_eq!(frame.breadcrumb.left(), frame.content.left());
    assert!(frame.scrollbar.is_none(), "nothing to scroll");
}

/// The strip's rows span the panel's interior, so a row's wash reaches the
/// plate's edges; the search field keeps the plate's inset, as a group's
/// caption does.
#[test]
fn the_strip_spans_the_panel_and_the_field_keeps_its_inset() {
    let theme = theme();
    let frame = resolve_frame(WIDE, Scale::ONE, &theme, Overflow::default(), Actions::None);
    let panel = frame.panel.expect("a panel");
    let search = frame.search.expect("a search field");
    let sidebar = frame.sidebar.expect("a strip");
    let border = tairix_controls::plate_border(&theme, Scale::ONE);
    let pad = Scale::ONE
        .scale_length(theme.metrics().control_inset)
        .max(1);
    assert_eq!(sidebar.left(), panel.left() + to_i32(border));
    assert_eq!(sidebar.width, panel.width - 2 * border);
    assert_eq!(search.left(), panel.left() + to_i32(border + pad));
    assert_eq!(search.top(), panel.top() + to_i32(border + pad));

    // A strip too long for the panel gives up its trailing edge to its own
    // bar, inside the panel, and the pane column is untouched.
    let scrolling = resolve_frame(
        WIDE,
        Scale::ONE,
        &theme,
        Overflow {
            strip: true,
            pane: false,
        },
        Actions::None,
    );
    let bar = scrolling.strip_scrollbar.expect("a strip bar");
    let strip = scrolling.sidebar.expect("a strip");
    assert!(within(bar, panel));
    assert_eq!(bar.left(), strip.right());
    assert_eq!(bar.right(), sidebar.right());
    assert_eq!(scrolling.content, frame.content);
}

/// The sidebar's panel is a settings group's own plate rather than a
/// lookalike: its rim, and the ground its rounded corners let through, are
/// exactly what a group's plate paints over the same rectangle.
#[test]
fn the_sidebar_panel_is_a_groups_own_plate() {
    let theme = theme();
    let shell = shell();
    let drawn = rendered(&shell, &theme);
    let panel = shell
        .frame(WIDE, Scale::ONE, &theme)
        .panel
        .expect("a panel");
    let mut plate = Surface::new(WIDE.width, WIDE.height).expect("a surface");
    let ground = ground_fill(&theme, theme.palette().surface, ChromeLayer::Ground);
    plate.fill_rect(0, 0, WIDE.width, WIDE.height, Color::from(ground));
    assert!(FieldGroup::paint_plate(&mut plate, panel, Scale::ONE, &theme).is_some());
    // Nothing else is drawn within a rim's width of the panel's edge.
    let border = plate_border(&theme, Scale::ONE);
    let left = u32::try_from(panel.left()).expect("on the surface");
    let top = u32::try_from(panel.top()).expect("on the surface");
    let (right, bottom) = (left + panel.width, top + panel.height);
    let same = |x: u32, y: u32| assert_eq!(drawn.get(x, y), plate.get(x, y), "at ({x}, {y})");
    for y in (top..top + border).chain(bottom - border..bottom) {
        (left..right).for_each(|x| same(x, y));
    }
    for y in top + border..bottom - border {
        (left..left + border)
            .chain(right - border..right)
            .for_each(|x| same(x, y));
    }
}

#[test]
fn a_narrow_window_sheds_the_strip_and_keeps_the_pane() {
    let theme = theme();
    let frame = resolve_frame(
        NARROW,
        Scale::ONE,
        &theme,
        Overflow::default(),
        Actions::None,
    );
    assert!(frame.sidebar.is_none());
    assert!(frame.search.is_none(), "nothing left to filter");
    assert_eq!(frame.content.left(), NARROW.left());
    assert_eq!(frame.content.width, NARROW.width);
    assert_eq!(frame.breadcrumb.width, NARROW.width);
    assert!(frame.content.height > 0, "the pane survives");
}

#[test]
fn the_strip_is_shed_exactly_at_the_stated_floor() {
    let theme = theme();
    let gap = Scale::ONE.scale_length(theme.metrics().control_gap).max(1);
    let bar = Scale::ONE.scale_length(SIDEBAR_WIDTH).max(1);
    let floor = Scale::ONE.scale_length(CONTENT_FLOOR).max(1);
    let exact = bar + gap + floor;
    assert!(resolve_frame(
        Rect::new(0, 0, exact, 480),
        Scale::ONE,
        &theme,
        Overflow::default(),
        Actions::None
    )
    .sidebar
    .is_some());
    assert!(resolve_frame(
        Rect::new(0, 0, exact - 1, 480),
        Scale::ONE,
        &theme,
        Overflow::default(),
        Actions::None
    )
    .sidebar
    .is_none());
}

#[test]
fn a_pane_taller_than_its_column_gets_a_scrollbar_beside_it() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 120);
    let fits = resolve_frame(
        short,
        Scale::ONE,
        &theme,
        Overflow::default(),
        Actions::None,
    );
    assert!(fits.scrollbar.is_none());
    let scrolls = resolve_frame(
        short,
        Scale::ONE,
        &theme,
        Overflow {
            strip: false,
            pane: true,
        },
        Actions::None,
    );
    let bar = scrolls.scrollbar.expect("a bar");
    assert_eq!(bar.left(), scrolls.content.right());
    assert_eq!(bar.top(), scrolls.content.top());
    assert_eq!(bar.height, scrolls.content.height);
    assert!(
        scrolls.content.width < fits.content.width,
        "the bar took room"
    );
}

/// The shell lays out for itself: a pane too tall for a short column is
/// measured once and the bar follows from the scroll range, so the input path
/// never re-measures a wrapped statement.
#[test]
fn laying_out_a_short_window_raises_the_scrollbar_the_pane_needs() {
    let theme = theme();
    let short = Rect::new(0, 0, 420, 110);
    let mut shell = stating();
    // Before any layout there is no measured range, so no bar is claimed.
    assert!(shell.frame(short, Scale::ONE, &theme).scrollbar.is_none());

    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    assert!(
        frame.scrollbar.is_some(),
        "the statement does not fit a {}px column",
        frame.content.height
    );
    // A window tall enough for the same pane claims none.
    shell.lay_out(WIDE, Scale::ONE, &theme);
    assert!(shell.frame(WIDE, Scale::ONE, &theme).scrollbar.is_none());
}

/// A wheel detent over the pane column scrolls it, and the offset it lands on
/// is what the next paint draws from.
#[test]
fn a_wheel_detent_over_the_pane_scrolls_it_and_stops_at_the_ends() {
    let theme = theme();
    let short = Rect::new(0, 0, 420, 110);
    let mut shell = stating();
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    assert!(frame.scrollbar.is_some(), "the pane scrolls");

    let at = Point::new(
        frame.content.left() + to_i32(frame.content.width / 2),
        frame.content.top() + to_i32(frame.content.height / 2),
    );
    let mut sink = damage();
    shell.on_pointer(
        &InputEvent::PointerMoved { to: at },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert_eq!(shell.scroll_offset(), 0);
    assert!(shell
        .on_pointer(
            &InputEvent::PointerScrolled {
                dx: 0,
                dy: SCROLL_UNITS_PER_DETENT,
            },
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        )
        .changed());
    assert!(shell.scroll_offset() > 0, "the column moved");

    // Scrolling back past the top clamps rather than wrapping.
    for _ in 0..20 {
        shell.on_pointer(
            &InputEvent::PointerScrolled {
                dx: 0,
                dy: -SCROLL_UNITS_PER_DETENT,
            },
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    assert_eq!(shell.scroll_offset(), 0);
}

/// A turn smaller than a detent moves the column nothing yet and is not
/// lost: the rest of the detent completes the step.
#[test]
fn part_of_a_detent_over_the_pane_carries_into_the_next() {
    let theme = theme();
    let short = Rect::new(0, 0, 420, 110);
    let mut shell = stating();
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let mut sink = damage();
    shell.on_pointer(
        &InputEvent::PointerMoved {
            to: frame.content.center(),
        },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let half = InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT / 2,
    };
    shell.on_pointer(&half, short, Scale::ONE, &theme, &mut sink);
    let first = shell.scroll_offset();
    shell.on_pointer(&half, short, Scale::ONE, &theme, &mut sink);
    assert_eq!(
        shell.scroll_offset(),
        u64::from(WHEEL_STEP),
        "two halves are one detent, whatever the first moved ({first})"
    );
}

#[test]
fn a_viewport_with_no_room_for_the_band_yields_nothing() {
    let theme = theme();
    let frame = resolve_frame(
        Rect::new(0, 0, 900, 1),
        Scale::ONE,
        &theme,
        Overflow::default(),
        Actions::None,
    );
    assert!(frame.sidebar.is_none());
    assert_eq!(frame.content, Rect::EMPTY);
    assert_eq!(frame.breadcrumb, Rect::EMPTY);
}

#[test]
fn the_frame_holds_at_a_larger_density() {
    let theme = theme();
    let scale = Scale::from_percent(200).expect("a valid scale");
    let frame = resolve_frame(
        Rect::new(0, 0, 1600, 1200),
        scale,
        &theme,
        Overflow::default(),
        Actions::None,
    );
    let panel = frame.panel.expect("a panel");
    assert_eq!(panel.width, scale.scale_length(SIDEBAR_WIDTH).max(1));
    assert!(frame.sidebar.is_some_and(|strip| within(strip, panel)));
    assert!(frame.content.width > 0);
}

// --- The strip and the cursor -------------------------------------------

#[test]
fn the_surface_opens_on_the_first_category() {
    let opening = Location::opening().expect("an opening location");
    assert_eq!(shell().location(), opening);
}

#[test]
fn every_category_is_a_row_of_the_strip() {
    let shell = shell();
    let categories = shell
        .rows()
        .iter()
        .filter(|row| matches!(row, StripRow::Category(_)))
        .count();
    assert_eq!(categories, CATEGORIES.len());
}

#[test]
fn the_cursor_reaches_every_row_of_the_strip() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    // Put the cursor on the strip, then walk it to the end.
    let rows = shell.rows().len();
    assert!(rows > 1);
    for _ in 0..rows * 2 {
        shell.on_key(
            keystroke(Key::Named(NamedKey::Down)),
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    // Walking down and choosing each row in turn reaches every location the
    // strip offers, which is what "the cursor reaches every row" means. Every
    // list is opened first, so every pane has a row to choose; a category
    // that discloses its panes shows none of its own, so it is not pressed.
    open_every_list(&mut shell, WIDE, &theme);
    for index in 0..shell.rows().len() {
        let Some(expected) = shell.rows()[index].destination() else {
            continue;
        };
        let Some(at) = row_point(&shell, index, WIDE, &theme) else {
            continue;
        };
        click(&mut shell, at, WIDE, &theme);
        assert_eq!(shell.location(), expected, "row {index}");
    }
}

/// The index of `category`'s own strip row.
fn category_row(shell: &Shell, category: Category) -> usize {
    shell
        .rows()
        .iter()
        .position(|row| *row == StripRow::Category(category))
        .unwrap_or_else(|| panic!("the strip lists {category:?}"))
}

/// Press `category`'s own strip row.
fn press_category(shell: &mut Shell, category: Category, viewport: Rect, theme: &Theme) {
    let at = row_point(shell, category_row(shell, category), viewport, theme)
        .unwrap_or_else(|| panic!("the strip seats {category:?}"));
    click(shell, at, viewport, theme);
}

/// Whether the strip lists `category`'s panes.
fn lists_panes_of(shell: &Shell, category: Category) -> bool {
    shell
        .rows()
        .iter()
        .any(|row| matches!(row, StripRow::Pane(owner, _) if *owner == category))
}

/// Open every disclosing category's list by pressing its row.
fn open_every_list(shell: &mut Shell, viewport: Rect, theme: &Theme) {
    for row in CATEGORIES.iter().filter(|row| row.discloses()) {
        if !lists_panes_of(shell, row.category) {
            press_category(shell, row.category, viewport, theme);
        }
        assert!(lists_panes_of(shell, row.category), "{:?}", row.category);
    }
}

/// The report this behaviour answers: opening a second list of panes leaves
/// the first one open.
#[test]
fn opening_a_second_list_leaves_the_first_open() {
    let theme = theme();
    let mut shell = shell();
    assert!(
        lists_panes_of(&shell, Category::General),
        "it opens on General"
    );
    press_category(&mut shell, Category::Networking, WIDE, &theme);
    assert!(
        lists_panes_of(&shell, Category::Networking),
        "its panes are listed"
    );
    assert!(
        lists_panes_of(&shell, Category::General),
        "the first list closed when the second opened"
    );
}

/// A category that discloses its panes opens their list in place: it is not
/// itself a place, so the pane on show stays on show.
#[test]
fn choosing_a_closed_category_lists_its_panes_and_goes_nowhere() {
    let theme = theme();
    let mut shell = shell();
    let before = shell.location();
    press_category(&mut shell, Category::Networking, WIDE, &theme);
    assert_eq!(shell.location(), before);
    let at = category_row(&shell, Category::Networking);
    let panes = Category::Networking.row().expect("listed").panes;
    for (offset, pane) in panes.iter().enumerate() {
        assert_eq!(
            shell.rows()[at + 1 + offset],
            StripRow::Pane(Category::Networking, pane.pane),
            "each pane beneath its category"
        );
    }
}

/// Pressing an open category's row closes its list and nothing else, and a
/// pane on show whose row went with it is stood for by its category's row.
#[test]
fn choosing_an_open_category_closes_its_list_and_its_row_stands_for_the_pane() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    assert!(shell.go_to(Pane::Caching, WIDE, Scale::ONE, &theme, &mut sink));
    press_category(&mut shell, Category::Networking, WIDE, &theme);
    press_category(&mut shell, Category::General, WIDE, &theme);
    assert!(!lists_panes_of(&shell, Category::General), "General closed");
    assert!(
        lists_panes_of(&shell, Category::Networking),
        "Networking stayed"
    );
    assert_eq!(
        shell.location().pane,
        Pane::Caching,
        "the pane stays on show"
    );
    assert_eq!(
        shell.selected_row(),
        Some(category_row(&shell, Category::General)),
        "the category's row stands for its hidden pane"
    );
    // Opening it again lists the pane's own row, selected, and goes nowhere.
    press_category(&mut shell, Category::General, WIDE, &theme);
    assert_eq!(shell.location().pane, Pane::Caching);
    let selected = shell.selected_row().expect("a selected row");
    assert_eq!(
        shell.rows()[selected],
        StripRow::Pane(Category::General, Pane::Caching)
    );
}

/// Going to a pane lists its category's panes, so the pane on show always has
/// a row of its own to be selected on.
#[test]
fn going_to_a_pane_lists_its_category() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    assert!(!lists_panes_of(&shell, Category::Networking));
    assert!(shell.go_to_pane("dns", WIDE, Scale::ONE, &theme, &mut sink));
    let selected = shell.selected_row().expect("a selected row");
    assert_eq!(
        shell.rows()[selected],
        StripRow::Pane(Category::Networking, Pane::Dns)
    );
    assert!(
        lists_panes_of(&shell, Category::General),
        "General stayed open"
    );
}

/// Press `key` on whichever region holds the keyboard.
fn key(shell: &mut Shell, key: Key, theme: &Theme) -> ShellOutcome {
    shell.on_key(keystroke(key), WIDE, Scale::ONE, theme, &mut damage())
}

/// Put the keyboard in the search field.
fn focus_search(shell: &mut Shell, theme: &Theme) {
    let search = shell
        .frame(WIDE, Scale::ONE, theme)
        .search
        .expect("a search field");
    click(
        shell,
        Point::new(search.left() + 4, search.top() + to_i32(search.height / 2)),
        WIDE,
        theme,
    );
}

/// Put the keyboard in the search field and type `query` into it.
fn search_for(shell: &mut Shell, query: &str, theme: &Theme) {
    focus_search(shell, theme);
    for ch in query.chars() {
        key(shell, Key::Char(ch), theme);
    }
}

/// Press `named` with the strip holding the keyboard, its cursor on `row`,
/// answering what the press concluded.
fn key_on_row(shell: &mut Shell, row: usize, named: NamedKey, theme: &Theme) -> ShellOutcome {
    // Tab from the search field reaches the trail, then the strip.
    focus_search(shell, theme);
    for _ in 0..2 {
        key(shell, Key::Named(NamedKey::Tab), theme);
    }
    while shell.strip_cursor_for_test() != Some(row) {
        let step = match shell.strip_cursor_for_test() {
            Some(at) if at > row => NamedKey::Up,
            _ => NamedKey::Down,
        };
        key(shell, Key::Named(step), theme);
    }
    key(shell, Key::Named(named), theme)
}

/// The tree keys open and close a category's list from the keyboard, and the
/// cursor stays on the category's row while its list comes and goes.
#[test]
fn right_and_left_open_and_close_a_list_and_the_cursor_stays_put() {
    let theme = theme();
    let mut shell = shell();
    let networking = category_row(&shell, Category::Networking);
    key_on_row(&mut shell, networking, NamedKey::Right, &theme);
    assert!(lists_panes_of(&shell, Category::Networking));
    assert!(lists_panes_of(&shell, Category::General), "General stayed");
    let networking = category_row(&shell, Category::Networking);
    assert_eq!(shell.strip_cursor_for_test(), Some(networking));
    key_on_row(&mut shell, networking, NamedKey::Left, &theme);
    assert!(!lists_panes_of(&shell, Category::Networking));
    assert_eq!(
        shell.strip_cursor_for_test(),
        Some(category_row(&shell, Category::Networking))
    );
}

/// A tree key that asks nothing of the row it is pressed on — Left on a
/// closed category, Right on a pane — changes nothing, so asks for no frame.
#[test]
fn a_tree_key_that_asks_nothing_changes_nothing() {
    let theme = theme();
    let mut shell = shell();
    let networking = category_row(&shell, Category::Networking);
    assert!(!lists_panes_of(&shell, Category::Networking));
    assert_eq!(
        key_on_row(&mut shell, networking, NamedKey::Left, &theme),
        ShellOutcome::Idle
    );

    key_on_row(&mut shell, networking, NamedKey::Right, &theme);
    let pane = category_row(&shell, Category::Networking) + 1;
    let rows = shell.rows().to_vec();
    assert_eq!(
        key_on_row(&mut shell, pane, NamedKey::Right, &theme),
        ShellOutcome::Idle
    );
    assert_eq!(shell.rows(), rows.as_slice());
    assert_eq!(shell.strip_cursor_for_test(), Some(pane));
}

/// Enter on a category's row toggles its list exactly as a press does, and
/// leaves the cursor on the row rather than snapping it to the pane on show.
#[test]
fn enter_on_a_category_toggles_its_list_and_keeps_the_cursor() {
    let theme = theme();
    let mut shell = shell();
    let networking = category_row(&shell, Category::Networking);
    key_on_row(&mut shell, networking, NamedKey::Enter, &theme);
    assert!(lists_panes_of(&shell, Category::Networking));
    assert_eq!(
        shell.strip_cursor_for_test(),
        Some(category_row(&shell, Category::Networking))
    );
}

/// The strip sets each run of categories apart by half a row, and nowhere
/// else: rows of one run stack flush, a run's first row sits a break lower.
#[test]
fn the_strip_breaks_between_runs_of_categories_and_nowhere_else() {
    let theme = theme();
    let mut shell = shell();
    let tall = Rect::new(0, 0, WIDE.width, 2400);
    shell.lay_out(tall, Scale::ONE, &theme);
    let rows = shell.rows().to_vec();
    let mut above = None;
    let mut breaks = 0;
    for (index, strip_row) in rows.iter().enumerate() {
        let expected = match strip_row {
            StripRow::Category(category) => {
                let entry = category.row().expect("listed");
                let breaks_here = entry.breaks_from(above);
                above = Some(entry);
                breaks_here
            }
            StripRow::Pane(..) => false,
        };
        let Some(prior_index) = index.checked_sub(1) else {
            continue;
        };
        let prior = shell
            .strip_row_rect(prior_index, tall, Scale::ONE, &theme)
            .expect("seated");
        let row = shell
            .strip_row_rect(index, tall, Scale::ONE, &theme)
            .expect("seated");
        let gap = row.top() - prior.bottom();
        if expected {
            assert_eq!(gap, to_i32(prior.height / 2), "a break above row {index}");
            breaks += 1;
        } else {
            assert_eq!(gap, 0, "row {index} sits flush beneath the one above");
        }
    }
    assert!(breaks > 0, "the strip is grouped");
}

/// Every row of the shipped strip — the longest disclosed pane's included —
/// draws its label whole at the window's own size, in both themes: exactly as
/// the same row draws it with room to spare, across the span the label takes.
#[test]
fn every_strip_label_is_drawn_whole_at_the_reference_density() {
    install_test_transport();
    let viewport = Rect::new(0, 0, crate::frame::WIN_WIDTH, crate::frame::WIN_HEIGHT);
    for theme in [Theme::dark(), Theme::light()] {
        let mut shell = shell();
        shell.lay_out(viewport, Scale::ONE, &theme);
        open_every_list(&mut shell, viewport, &theme);
        let strip = shell.strip_for_test().clone();
        let width = shell
            .frame(viewport, Scale::ONE, &theme)
            .sidebar
            .expect("a strip")
            .width;
        let height = strip.measured_height(Scale::ONE, &theme);
        let roomy = width * 3;
        let draw = |w: u32| {
            let mut surface = Surface::new(w, height).expect("a surface");
            strip.render(
                &mut surface,
                Rect::new(0, 0, w, height),
                Scale::ONE,
                &theme,
                &mut NoArtwork,
            );
            surface
        };
        let (tight, spare) = (draw(width), draw(roomy));
        for index in 0..strip.len() {
            let row = strip
                .tab_area(index, Rect::new(0, 0, roomy, height), Scale::ONE, &theme)
                .expect("seated");
            let (top, bottom) = (
                u32::try_from(row.top()).expect("on the surface"),
                u32::try_from(row.bottom()).expect("on the surface"),
            );
            // Where the label ends with room to spare: the last column the row
            // paints anything but its own plate in, short of the trailing half
            // where the roomy row keeps its chevron.
            let plate = spare.get(roomy / 2, top + 1);
            let end = (0..roomy / 2)
                .rev()
                .find(|x| (top..bottom).any(|y| spare.get(*x, y) != plate))
                .expect("the row draws something");
            assert!(end < width, "row {index} needs {end} of {width} pixels");
            for y in top..bottom {
                for x in 0..=end {
                    assert_eq!(
                        tight.get(x, y),
                        spare.get(x, y),
                        "row {index} ({:?}) is cut at {x},{y}",
                        strip.tabs()[index].label()
                    );
                }
            }
        }
    }
}

/// A submitted search shows what it matched: the pane a category was reached
/// through, never the category's first pane.
#[test]
fn a_submitted_search_shows_the_pane_it_matched() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    let frame = shell.frame(WIDE, Scale::ONE, &theme);
    let search = frame.search.expect("a search field");
    click(
        &mut shell,
        Point::new(search.left() + 4, search.top() + to_i32(search.height / 2)),
        WIDE,
        &theme,
    );
    for ch in "caching".chars() {
        shell.on_key(
            keystroke(Key::Char(ch)),
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    shell.on_key(
        keystroke(Key::Named(NamedKey::Enter)),
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert_eq!(shell.location().pane, Pane::Caching);
}

/// While a search is in force a category lists its matches whatever is open,
/// so its row goes to the first of them rather than closing a list.
#[test]
fn a_category_row_in_a_search_goes_to_its_first_match() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    let frame = shell.frame(WIDE, Scale::ONE, &theme);
    let search = frame.search.expect("a search field");
    click(
        &mut shell,
        Point::new(search.left() + 4, search.top() + to_i32(search.height / 2)),
        WIDE,
        &theme,
    );
    for ch in "dns".chars() {
        shell.on_key(
            keystroke(Key::Char(ch)),
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    press_category(&mut shell, Category::Networking, WIDE, &theme);
    assert_eq!(shell.location().pane, Pane::Dns);
    assert!(lists_panes_of(&shell, Category::Networking));
}

/// A search lists a category's matches whatever is open, so a tree key that
/// would close its list changes nothing — and the list the reader had open
/// is still open once the search is gone.
#[test]
fn a_disclosure_does_nothing_while_a_search_is_in_force() {
    let theme = theme();
    let mut shell = shell();
    press_category(&mut shell, Category::Networking, WIDE, &theme);
    search_for(&mut shell, "dns", &theme);
    let matches = shell.rows().to_vec();
    let at = category_row(&shell, Category::Networking);

    assert_eq!(
        key_on_row(&mut shell, at, NamedKey::Left, &theme),
        ShellOutcome::Idle
    );
    assert_eq!(shell.rows(), matches.as_slice(), "the matches moved");

    focus_search(&mut shell, &theme);
    key(&mut shell, Key::Named(NamedKey::Escape), &theme);
    assert!(
        lists_panes_of(&shell, Category::Networking),
        "the search closed a list the reader had open"
    );
}

/// A change to the strip alone — a section disclosed, the search edited —
/// re-lays out the strip and leaves the pane as it was measured. The pane is
/// grown behind the shell's back, through a reading's own landing, so a
/// re-measure would show as the scrollbar its column then needs.
#[test]
fn a_strip_change_leaves_the_pane_as_it_was_measured() {
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Notifications,
        pane: Pane::Notifications,
    });
    shell.lay_out(WIDE, Scale::ONE, &theme);
    let scrollbar = |shell: &Shell| shell.frame(WIDE, Scale::ONE, &theme).scrollbar;
    assert!(scrollbar(&shell).is_none(), "the pane starts short");
    let many = (0..64)
        .map(|n| {
            tairix_abi::BundleId::new(&alloc::format!("com.example.app{n}"))
                .expect("a bounded identity")
        })
        .collect();
    shell.adopt_notify_sources(Some(many));

    press_category(&mut shell, Category::Networking, WIDE, &theme);
    assert!(
        lists_panes_of(&shell, Category::Networking),
        "the section opened"
    );
    assert!(
        scrollbar(&shell).is_none(),
        "a disclosure measured the pane"
    );
    search_for(&mut shell, "a", &theme);
    assert!(
        scrollbar(&shell).is_none(),
        "a search edit measured the pane"
    );

    shell.lay_out(WIDE, Scale::ONE, &theme);
    assert!(
        scrollbar(&shell).is_some(),
        "the grown pane never outgrew its column, so this proves nothing"
    );
}

#[test]
fn the_trail_names_where_the_surface_is() {
    let theme = theme();
    let mut shell = shell();
    // A disclosing category shows three crumbs; a single-pane one shows two,
    // because saying its name twice would read as two places.
    assert_eq!(
        shell.trail_labels(),
        alloc::vec!["Settings", "General", "About"]
    );
    let sound = shell
        .rows()
        .iter()
        .position(|row| matches!(row, StripRow::Category(Category::Sound)))
        .expect("the sound row");
    let at = row_point(&shell, sound, WIDE, &theme).expect("a row");
    click(&mut shell, at, WIDE, &theme);
    assert_eq!(shell.trail_labels(), alloc::vec!["Settings", "Sound"]);
}

// --- Search -------------------------------------------------------------

#[test]
fn a_query_filters_the_strip_to_what_it_reaches() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    let frame = shell.frame(WIDE, Scale::ONE, &theme);
    let search = frame.search.expect("a search field");
    click(
        &mut shell,
        Point::new(search.left() + 4, search.top() + to_i32(search.height / 2)),
        WIDE,
        &theme,
    );
    for ch in "sound".chars() {
        shell.on_key(
            keystroke(Key::Char(ch)),
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    assert_eq!(shell.rows(), &[StripRow::Category(Category::Sound)]);

    // Escape clears the query, and the strip is whole again.
    shell.on_key(
        keystroke(Key::Named(NamedKey::Escape)),
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert_eq!(
        shell
            .rows()
            .iter()
            .filter(|row| matches!(row, StripRow::Category(_)))
            .count(),
        CATEGORIES.len()
    );
}

// --- The shed strip's category list -------------------------------------

#[test]
fn the_leading_crumb_opens_the_category_list_only_once_the_strip_is_shed() {
    let theme = theme();
    let mut shell = shell();
    let wide = shell.frame(WIDE, Scale::ONE, &theme);
    click(
        &mut shell,
        Point::new(
            wide.breadcrumb.left() + 4,
            wide.breadcrumb.top() + to_i32(wide.breadcrumb.height / 2),
        ),
        WIDE,
        &theme,
    );
    assert!(
        !shell.category_list_open(),
        "the strip is on screen, so the list would be a second way to it"
    );

    let narrow = shell.frame(NARROW, Scale::ONE, &theme);
    click(
        &mut shell,
        Point::new(
            narrow.breadcrumb.left() + 4,
            narrow.breadcrumb.top() + to_i32(narrow.breadcrumb.height / 2),
        ),
        NARROW,
        &theme,
    );
    assert!(shell.category_list_open(), "the shed strip's way back");

    let mut sink = damage();
    shell.on_key(
        keystroke(Key::Named(NamedKey::Escape)),
        NARROW,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert!(!shell.category_list_open(), "escape dismisses it");
}

/// Opening the category list from the keyboard reports the plate it opens
/// into, which reaches above the pane's column, so the frame presented shows
/// the whole list.
#[test]
fn opening_the_category_list_reports_its_plate() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    for _ in 0..4 {
        if shell.category_list_open() {
            break;
        }
        let _ = shell.on_key(
            keystroke(Key::Named(NamedKey::Tab)),
            NARROW,
            Scale::ONE,
            &theme,
            &mut damage(),
        );
        sink = damage();
        let _ = shell.on_key(
            keystroke(Key::Named(NamedKey::Enter)),
            NARROW,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    let themes = ThemeRegistry::with_builtins();
    let list = shell
        .category_list_rect_for_test(
            NARROW,
            Scale::ONE,
            themes.grounds(crate::frame::WINDOW_GROUND).popups,
        )
        .expect("the shed strip's list is open");
    assert!(covers(&sink, list), "{list:?} opened unreported");
}

/// The category list stands over the window's own content, so it is drawn
/// solid even though the window is glass: laid down translucent, it would
/// show the desktop through the window instead of the pane beneath it.
#[test]
fn the_category_list_over_the_glass_window_is_solid() {
    let theme = theme();
    let mut shell = shell();
    let narrow = shell.frame(NARROW, Scale::ONE, &theme);
    click(
        &mut shell,
        Point::new(
            narrow.breadcrumb.left() + 4,
            narrow.breadcrumb.top() + to_i32(narrow.breadcrumb.height / 2),
        ),
        NARROW,
        &theme,
    );
    let themes = ThemeRegistry::with_builtins();
    let grounds = themes.grounds(crate::frame::WINDOW_GROUND);
    let mut surface = Surface::new(NARROW.width, NARROW.height).expect("a surface");
    shell.render(&mut surface, NARROW, Scale::ONE, grounds, &mut NoArtwork);
    let list = shell
        .category_list_rect_for_test(NARROW, Scale::ONE, grounds.popups)
        .expect("the shed strip's list is open");
    let clear = Scale::ONE.scale_length(theme.metrics().popup_corner_radius) + 1;
    let (left, top) = (
        u32::try_from(list.left()).expect("on the surface"),
        u32::try_from(list.top()).expect("on the surface"),
    );
    for y in top + clear..top + list.height - clear {
        for x in left + clear..left + list.width - clear {
            assert_eq!(
                surface.get(x, y).map(|pixel| pixel.a),
                Some(u8::MAX),
                "({x}, {y}) of the list lets the desktop through"
            );
        }
    }
}

// --- Painting -----------------------------------------------------------

#[test]
fn the_shell_draws_in_both_themes_and_at_both_densities() {
    install_test_transport();
    for theme in [Theme::dark(), Theme::light()] {
        for scale in [Scale::ONE, Scale::from_percent(200).expect("a valid scale")] {
            for viewport in [WIDE, NARROW] {
                let mut surface = Surface::new(viewport.width, viewport.height).expect("a surface");
                shell().render(
                    &mut surface,
                    viewport,
                    scale,
                    opaque(&theme),
                    &mut NoArtwork,
                );
                assert!(
                    surface.pixels().iter().any(|p| p.a > 0),
                    "the shell drew nothing"
                );
            }
        }
    }
}

#[test]
fn a_statement_pane_is_taller_in_a_narrower_column() {
    let theme = theme();
    let shell = shell();
    let wide = shell.frame(WIDE, Scale::ONE, &theme);
    let narrow = shell.frame(Rect::new(0, 0, 420, 640), Scale::ONE, &theme);
    let wide_h = shell.pane_height(wide.content.width, Scale::ONE, &theme);
    let narrow_h = shell.pane_height(narrow.content.width, Scale::ONE, &theme);
    assert!(wide_h > 0 && narrow_h > 0);
    assert!(
        narrow_h >= wide_h,
        "a narrower column wraps into more lines: {narrow_h} vs {wide_h}"
    );
}

#[test]
fn a_degenerate_viewport_draws_nothing_and_panics_at_nothing() {
    let theme = theme();
    let mut surface = Surface::new(4, 4).expect("a surface");
    shell().render(
        &mut surface,
        Rect::new(0, 0, 4, 4),
        Scale::ONE,
        opaque(&theme),
        &mut NoArtwork,
    );
}

// --- The strip is longer than a short column, and still walkable ---------

/// A window too short to seat every category scrolls the strip rather than
/// losing the rows past the fold: a row the reader cannot reach is a category
/// they cannot open.
#[test]
fn a_short_window_scrolls_the_category_strip() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 260);
    let mut shell = shell();
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let sidebar = frame.sidebar.expect("a strip");
    let bar = frame
        .strip_scrollbar
        .expect("a strip too long for the column gets its own bar");
    // The gutter is carved out of the strip's own column, so a long list
    // never narrows the pane beside it.
    assert_eq!(bar.left(), sidebar.right());
    let wide = shell.frame(WIDE, Scale::ONE, &theme).content;
    assert_eq!(
        (frame.content.left(), frame.content.width),
        (wide.left(), wide.width),
        "the strip's bar narrowed the pane"
    );
    assert!(
        shell.strip_for_test().measured_height(Scale::ONE, &theme) > sidebar.height,
        "this window is supposed to be too short for the whole list"
    );

    // Every row is reachable: revealing the last one brings it into the
    // column the reader is looking at.
    let last = shell.rows().len() - 1;
    assert!(
        shell
            .strip_row_rect(last, short, Scale::ONE, &theme)
            .is_none(),
        "the last row is past the fold to begin with"
    );
    shell.reveal_for_test(last, short, Scale::ONE, &theme);
    let row = shell
        .strip_row_rect(last, short, Scale::ONE, &theme)
        .expect("the last row is seated once revealed");
    assert!(
        row.top() >= sidebar.top() && row.bottom() <= sidebar.bottom(),
        "{row:?} is not inside {sidebar:?}"
    );
    assert!(shell.strip_offset_for_test() > 0, "the strip scrolled");

    // And back: the first row is reachable again.
    shell.reveal_for_test(0, short, Scale::ONE, &theme);
    assert_eq!(shell.strip_offset_for_test(), 0);
    assert!(shell.strip_row_rect(0, short, Scale::ONE, &theme).is_some());
}

/// Walking the cursor to the end of the strip scrolls it into view, so the
/// keyboard reaches a row a short window could not show.
#[test]
fn the_cursor_walks_past_the_fold() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 260);
    let mut shell = shell();
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let sidebar = frame.sidebar.expect("a strip");

    // Focus the strip by clicking its first row, then jump to the last.
    click(
        &mut shell,
        Point::new(
            sidebar.left() + to_i32(sidebar.width / 2),
            sidebar.top() + 2,
        ),
        short,
        &theme,
    );
    let mut sink = damage();
    shell.on_key(
        keystroke(Key::Named(NamedKey::End)),
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let cursor = shell
        .strip_cursor_for_test()
        .expect("a cursor on the strip");
    assert_eq!(cursor, shell.rows().len() - 1, "End reaches the last row");
    let row = shell
        .strip_row_rect(cursor, short, Scale::ONE, &theme)
        .expect("the cursor's row is on screen");
    assert!(
        row.top() >= sidebar.top() && row.bottom() <= sidebar.bottom(),
        "the cursor was left out of sight: {row:?} vs {sidebar:?}"
    );
}

/// Feed one key press with no modifiers, answering what it concluded.
fn press(
    shell: &mut Shell,
    key: Key,
    theme: &Theme,
    sink: &mut tairix_geometry::Region,
) -> ShellOutcome {
    shell.on_key(keystroke(key), WIDE, Scale::ONE, theme, sink)
}

/// Go to `location` and hand back the shell showing it.
fn shell_at(location: Location) -> Shell {
    let mut shell = shell();
    let theme = theme();
    let mut sink = damage();
    shell.go_to_for_test(location, WIDE, Scale::ONE, &theme, &mut sink);
    shell
}

#[test]
fn the_composed_panes_draw_a_form_rather_than_a_statement() {
    for (pane, groups) in [
        (Pane::Appearance, 2),
        (Pane::Accessibility, 4),
        (Pane::Wallpaper, 2),
        (Pane::Screensaver, 3),
    ] {
        let category = pane.locate().expect("a located pane").0;
        let shell = shell_at(Location { category, pane });
        let form = shell
            .form_for_test()
            .unwrap_or_else(|| panic!("{pane:?} composes no form"));
        assert_eq!(form.groups().len(), groups, "{pane:?}");
        // Composing a form means the statement renderer draws nothing, so
        // the column's height is the form's alone.
        assert!(shell.pane_height(600, Scale::ONE, &theme()) > 0);
    }
}

/// Every composed row is reserved exactly the height it draws. The groups of
/// a pane line up in one column, the widest any of them resolves, and a
/// description wraps into what that column leaves; a height measured in a
/// group's own narrower column would cut the description's last line.
#[test]
fn every_composed_row_is_reserved_what_it_draws_in_the_shared_column() {
    let theme = theme();
    let mut narrower = 0;
    for pane in CATEGORIES.iter().flat_map(|category| category.panes) {
        if !matches!(pane.content(), Some(PaneContent::Form(_))) {
            continue;
        }
        let category = pane.pane.locate().expect("a located pane").0;
        let shell = shell_at(Location {
            category,
            pane: pane.pane,
        });
        let form = shell.form_for_test().expect("a composed form");
        for width in [360, 520, 900] {
            let bounds = Rect::new(0, 0, width, 20_000);
            let place = FormPlace {
                bounds,
                viewport: bounds,
                scale: Scale::ONE,
                theme: &theme,
            };
            for (index, layout) in form.layouts_for_test(place) {
                let group = &form.groups()[index];
                let across = layout.bounds.width;
                if layout.column > group.slot_column(across, Scale::ONE, &theme) {
                    narrower += 1;
                }
                assert_eq!(
                    layout.bounds.height,
                    group.measured_height(across, layout.column, Scale::ONE, &theme),
                    "{}: group {index} at {width}px is not measured in its column",
                    pane.name
                );
                let span = group.row_text_span(across, layout.column, Scale::ONE, &theme);
                for (row, field) in group.rows().iter().enumerate() {
                    let rect = group
                        .row_rect(row, layout, Scale::ONE, &theme)
                        .expect("a tall column seats every row");
                    assert_eq!(
                        rect.height,
                        field.measured_height(span, Scale::ONE, &theme),
                        "{}: {:?} at {width}px",
                        pane.name,
                        field.label()
                    );
                }
            }
        }
    }
    assert!(
        narrower > 0,
        "no pane lays a group out in a column wider than its own, so this proves nothing"
    );
}

/// The Notifications pane opens asking which programs have notified, and
/// lists each once the desktop answers.
#[test]
fn the_notifications_pane_lists_the_sources_the_desktop_answers() {
    let mut shell = shell_at(Location {
        category: Category::Notifications,
        pane: Pane::Notifications,
    });
    assert!(shell.notify_sources_wanted(), "coming on show asks");
    let form = shell.form_for_test().expect("a composed form");
    assert_eq!(form.groups().len(), 2);

    let chat = tairix_abi::BundleId::new("com.example.chat").expect("a bounded identity");
    shell.adopt_notify_sources(Some(alloc::vec![chat]));
    assert!(!shell.notify_sources_wanted(), "the answer landed");
    let form = shell.form_for_test().expect("a composed form");
    let sources = &form.groups()[1];
    assert_eq!(sources.rows()[0].label(), "com.example.chat");
}

/// Choosing a source's level posts the notification keys alone, carrying
/// that source's new level.
#[test]
fn choosing_a_sources_level_posts_the_notification_keys_alone() {
    let mut shell = shell_at(Location {
        category: Category::Notifications,
        pane: Pane::Notifications,
    });
    let chat = tairix_abi::BundleId::new("com.example.chat").expect("a bounded identity");
    shell.adopt_notify_sources(Some(alloc::vec![chat]));
    let outcome = shell
        .form_mut_for_test()
        .expect("a composed form")
        .choose_for_test(1, 0, 2);
    let crate::FormOutcome::Apply(document) = outcome else {
        panic!("choosing a level posts a document, not {outcome:?}");
    };
    assert!(
        document.contains("notify.sources = com.example.chat:critical"),
        "{document}"
    );
    assert!(document.contains("notify.enabled = true"), "{document}");
    for key in SettingsKey::PINBOARD
        .into_iter()
        .chain(SettingsKey::APPEARANCE)
    {
        assert!(
            !document.contains(key.name()),
            "{} posted: {document}",
            key.name()
        );
    }
}

/// The desktop-wide switch is a row of its own, and turning it off says so.
#[test]
fn the_desktop_switch_posts_its_own_key() {
    let mut shell = shell_at(Location {
        category: Category::Notifications,
        pane: Pane::Notifications,
    });
    let outcome = shell
        .form_mut_for_test()
        .expect("a composed form")
        .choose_for_test(0, 0, 1);
    let crate::FormOutcome::Apply(document) = outcome else {
        panic!("the switch posts a document, not {outcome:?}");
    };
    assert!(document.contains("notify.enabled = false"), "{document}");
}

/// Each input pane posts its own keys alone, so the Keyboard pane cannot
/// reimpose a pointer value, nor the Mouse pane a repeat, nor the Trackpad
/// pane the mouse's speed.
#[test]
fn the_input_panes_post_only_their_own_keys() {
    for (pane, row, posted, withheld) in [
        (
            Pane::Mouse,
            0,
            "pointer.primary = right",
            &SettingsKey::KEYBOARD[..],
        ),
        (
            Pane::Keyboard,
            1,
            "key.repeat_rate = off",
            &SettingsKey::POINTER[..],
        ),
        (
            Pane::Trackpad,
            0,
            "touchpad.tap = ",
            &SettingsKey::POINTER[..],
        ),
    ] {
        let category = pane.locate().expect("a located pane").0;
        let mut shell = shell_at(Location { category, pane });
        let form = shell.form_mut_for_test().expect("a composed form");
        let chosen = usize::from(pane == Pane::Mouse);
        let crate::FormOutcome::Apply(document) = form.choose_for_test(0, row, chosen) else {
            panic!("{pane:?} posts a document");
        };
        assert!(document.contains(posted), "{pane:?}: {document}");
        for key in withheld {
            assert!(
                !document.contains(key.name()),
                "{pane:?} posted {key}: {document}"
            );
        }
    }
}

/// A double-click interval set off the ladder is offered under its own
/// value, so opening the pane changes nothing.
#[test]
fn an_off_ladder_interval_is_offered_as_itself() {
    let settings = tairix_wallpaper::DesktopSettings {
        double_click: tairix_abi::time::Duration64::from_millis(450),
        ..tairix_wallpaper::DesktopSettings::default()
    };
    let mut shell = Shell::new(settings).expect("a shell");
    let theme = theme();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Mouse,
            pane: Pane::Mouse,
        },
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let form = shell.form_for_test().expect("a composed form");
    let row = &form.groups()[0].rows()[2];
    let tairix_controls::FieldControl::Slider(slider) = row.control() else {
        panic!("the interval row is a slider");
    };
    // Seven stops from slow to fast, and the one set between 500 and 400 ms
    // standing as its own, fifth from the slow end.
    assert_eq!(slider.stop_value(7), Some(1000), "eight stops");
    assert_eq!(slider.stop_of(slider.value()), Some(4));
}

/// Leaving a value that was off the ladder takes its stop away at once, so
/// a second change made before the desktop answers the first is read
/// against the stops the row now shows.
#[test]
fn leaving_an_off_ladder_value_restates_the_row_it_was_a_stop_of() {
    let settings = tairix_wallpaper::DesktopSettings {
        double_click: tairix_abi::time::Duration64::from_millis(450),
        ..tairix_wallpaper::DesktopSettings::default()
    };
    let mut shell = Shell::new(settings).expect("a shell");
    let theme = theme();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Mouse,
            pane: Pane::Mouse,
        },
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let form = shell.form_mut_for_test().expect("a composed form");
    let slider = |form: &crate::form::Form| match form.groups()[0].rows()[2].control() {
        tairix_controls::FieldControl::Slider(slider) => slider.clone(),
        _ => panic!("the interval row is a slider"),
    };
    let at_300 = slider(form).stop_value(6).expect("a stop");
    let crate::FormOutcome::Apply(document) = form.settle_for_test(0, 2, at_300) else {
        panic!("the first settle posts a document");
    };
    assert!(
        document.contains("pointer.double_click_ms = 300"),
        "{document}"
    );
    assert_eq!(slider(form).stop_value(7), None, "the 450 ms stop is gone");
    let crate::FormOutcome::Apply(document) = form.settle_for_test(0, 2, 1000) else {
        panic!("the second settle posts a document");
    };
    assert!(
        document.contains("pointer.double_click_ms = 200"),
        "{document}"
    );
}

/// A double-click is set from slow to fast, the way a reader thinks of it,
/// and choosing the fast end writes the shortest interval.
#[test]
fn the_double_click_row_runs_from_slow_to_fast() {
    let mut shell = shell_at(Location {
        category: Category::Mouse,
        pane: Pane::Mouse,
    });
    let form = shell.form_mut_for_test().expect("a composed form");
    let tairix_controls::FieldControl::Slider(slider) = form.groups()[0].rows()[2].control() else {
        panic!("the interval row is a slider");
    };
    let fast = slider.stop_value(6).expect("the fast end");
    let crate::FormOutcome::Apply(document) = form.settle_for_test(0, 2, fast) else {
        panic!("settling the slider posts a document");
    };
    assert!(
        document.contains("pointer.double_click_ms = 200"),
        "{document}"
    );
    let crate::FormOutcome::Apply(document) = form.settle_for_test(0, 2, 0) else {
        panic!("settling the slider posts a document");
    };
    assert!(
        document.contains("pointer.double_click_ms = 1000"),
        "{document}"
    );
}

/// Every input setting measured in a unit is a slider named at both ends,
/// never a list of numbers.
#[test]
fn the_input_settings_read_in_words_at_either_end_of_a_slider() {
    for (pane, rows) in [
        (Pane::Mouse, &[1usize, 2][..]),
        (Pane::Keyboard, &[0, 1][..]),
    ] {
        let category = pane.locate().expect("a located pane").0;
        let shell = shell_at(Location { category, pane });
        let form = shell.form_for_test().expect("a composed form");
        for row in rows {
            assert!(
                matches!(
                    form.groups()[0].rows()[*row].control(),
                    tairix_controls::FieldControl::Slider(_)
                ),
                "{pane:?} row {row}"
            );
        }
    }
}

/// A live sample of a drag moves the slider and posts nothing; only where it
/// settles does.
#[test]
fn a_slider_writes_only_where_it_settles() {
    let mut shell = shell_at(Location {
        category: Category::Mouse,
        pane: Pane::Mouse,
    });
    let form = shell.form_mut_for_test().expect("a composed form");
    let outcome = form.act_for_test(
        0,
        2,
        tairix_controls::FieldAction::SetValue { permille: 500 },
    );
    assert!(
        !matches!(outcome, crate::FormOutcome::Apply(_)),
        "{outcome:?}"
    );
}

/// The energy-saving row reads in minutes and hours, offers switching off at
/// once, and keeps a wait set off its ladder as itself.
#[test]
fn the_display_off_row_counts_in_minutes_and_hours() {
    let settings = tairix_wallpaper::DesktopSettings {
        display_off_after: tairix_wallpaper::DisplayOffAfter::Minutes(90),
        ..tairix_wallpaper::DesktopSettings::default()
    };
    let mut shell = Shell::new(settings).expect("a shell");
    let theme = theme();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Screensaver,
            pane: Pane::Screensaver,
        },
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let form = shell.form_for_test().expect("a composed form");
    let row = &form.groups().last().expect("a group").rows()[0];
    assert_eq!(row.label(), Setting::DisplayOff.label());
    let tairix_controls::FieldControl::Combo(combo) = row.control() else {
        panic!("the display-off row is a choice");
    };
    assert_eq!(combo.selected_text(), Some("1 hour 30 minutes"));
    let choices = combo.choices();
    assert_eq!(choices[0], "Never");
    assert_eq!(choices[1], "With the screensaver");
    for label in ["1 minute", "30 minutes", "1 hour", "2 hours", "24 hours"] {
        assert!(choices.iter().any(|choice| choice == label), "{label}");
    }
}

/// Every kind the registry closes over is one a reader can choose, by its
/// picture.
#[test]
fn the_screensaver_chooser_offers_every_kind() {
    let shell = shell_at(Location {
        category: Category::Screensaver,
        pane: Pane::Screensaver,
    });
    let form = shell.form_for_test().expect("a form");
    let chooser = form.groups()[0]
        .pictures()
        .expect("the kind is chosen by its picture");
    let labels: alloc::vec::Vec<&str> = (0..chooser.len())
        .filter_map(|index| chooser.item(index))
        .map(tairix_controls::PictureItem::label)
        .collect();
    assert_eq!(
        labels,
        [
            "Black",
            "Dimmed desktop",
            "Slideshow",
            "Clock",
            "Minimal Clock",
            "Starfield",
            "Game of Life",
            "Ray Tracer",
            "Retro Games",
            "System Monitor"
        ]
    );
}

/// Choosing a display-off wait posts that key alone, beside no lock key.
#[test]
fn choosing_a_display_off_wait_posts_its_key() {
    let mut shell = shell_at(Location {
        category: Category::Screensaver,
        pane: Pane::Screensaver,
    });
    let form = shell.form_mut_for_test().expect("a composed form");
    let last = form.groups_len() - 1;
    let crate::FormOutcome::Apply(document) = form.choose_for_test(last, 0, 1) else {
        panic!("the row posts a document");
    };
    assert!(
        document.contains("screensaver.display_off_min = 0"),
        "{document}"
    );
    assert!(
        !document.contains(SettingsKey::LockAfter.name()),
        "{document}"
    );
}

/// The Keyboard pane states what it cannot offer rather than drawing
/// controls that would change nothing.
#[test]
fn the_keyboard_pane_states_its_absent_layouts_and_shortcuts() {
    let shell = shell_at(Location {
        category: Category::Keyboard,
        pane: Pane::Keyboard,
    });
    let form = shell.form_for_test().expect("a composed form");
    assert!(form.groups()[0]
        .footnote()
        .is_some_and(|note| note.contains("layout") && note.contains("shortcut")));
}

/// *Lock Now* is a command, not a setting: activating it asks for the
/// desktop's own lock and posts no document.
#[test]
fn lock_now_asks_for_the_lock_and_posts_nothing() {
    let mut shell = shell_at(Location {
        category: Category::LockScreen,
        pane: Pane::LockScreen,
    });
    let form = shell.form_mut_for_test().expect("a composed form");
    let outcome = form.activate_for_test(0, 1);
    assert_eq!(outcome, crate::FormOutcome::LockScreen);
}

/// A refused lock is stated on the row that asked, and a later success
/// takes the statement down again.
#[test]
fn a_refused_lock_is_stated_on_its_row() {
    let mut shell = shell_at(Location {
        category: Category::LockScreen,
        pane: Pane::LockScreen,
    });
    shell.adopt_lock_answer(Err(tairix_abi::Errno::NotSupported));
    let row = &shell.form_for_test().expect("a form").groups()[0].rows()[1];
    assert!(row
        .description()
        .is_some_and(|text| text.starts_with("The desktop would not lock the screen")));
    shell.adopt_lock_answer(Ok(()));
    let row = &shell.form_for_test().expect("a form").groups()[0].rows()[1];
    assert!(row
        .description()
        .is_some_and(|text| !text.contains("would not")));
}

/// A refused lock that lands while another pane is on show is stated once
/// the pane that asked is shown again.
#[test]
fn a_refused_lock_landing_on_another_pane_is_stated_when_its_pane_returns() {
    let mut shell = crate::test_support::showing("notifications");
    shell.adopt_lock_answer(Err(tairix_abi::Errno::NotSupported));
    let mut sink = damage();
    assert!(shell.go_to_pane("lock-screen", WIDE, Scale::ONE, &theme(), &mut sink));
    let row = &shell.form_for_test().expect("a form").groups()[0].rows()[1];
    assert!(row
        .description()
        .is_some_and(|text| text.starts_with("The desktop would not lock the screen")));
}

/// The two idle panes post their own keys alone.
#[test]
fn the_idle_panes_post_only_their_own_keys() {
    for (pane, posted, withheld) in [
        (
            Pane::Screensaver,
            "screensaver.after_min = never",
            &SettingsKey::LOCK[..],
        ),
        (
            Pane::LockScreen,
            "lock.after_min = never",
            &SettingsKey::SCREENSAVER[..],
        ),
    ] {
        let category = pane.locate().expect("a located pane").0;
        let mut shell = shell_at(Location { category, pane });
        let form = shell.form_mut_for_test().expect("a composed form");
        let crate::FormOutcome::Apply(document) = form.choose_for_test(0, 0, 0) else {
            panic!("{pane:?} posts a document");
        };
        assert!(document.contains(posted), "{pane:?}: {document}");
        for key in withheld {
            assert!(
                !document.contains(key.name()),
                "{pane:?} posted {key}: {document}"
            );
        }
    }
}

#[test]
fn a_pane_that_states_an_absence_composes_no_form() {
    let shell = shell_at(Location {
        category: Category::Bluetooth,
        pane: Pane::Bluetooth,
    });
    assert!(shell.form_for_test().is_none());
}

#[test]
fn choosing_a_value_posts_only_the_appearance_keys() {
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Appearance,
        pane: Pane::Appearance,
    });
    shell.focus_content_for_test(WIDE, Scale::ONE, &theme);
    let mut sink = damage();

    // Open the first row's choice list and take the second choice, which is
    // the appearance the desktop is not currently on.
    let outcome = press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    assert!(outcome.changed(), "the list did not open");
    let down = press(&mut shell, Key::Named(NamedKey::Down), &theme, &mut sink);
    assert!(down.changed());
    let chosen = press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);

    let document = chosen
        .document()
        .expect("choosing a value asks for a document");
    assert!(document.contains("appearance = dark"), "{document}");
    // The pinboard keys are the chooser's: posting them here would reimpose
    // whatever wallpaper this window happened to read at start-up.
    for key in SettingsKey::PINBOARD {
        assert!(
            !document.contains(key.name()),
            "{} posted: {document}",
            key.name()
        );
    }
}

/// Pressing where [`Shell::setting_rect`] and [`Shell::choice_rect`] say a
/// control is drawn opens that row's list and takes that choice: the two
/// answers are the rectangles the form hit-tests, not arithmetic beside it.
#[test]
fn a_press_on_the_reported_rectangles_opens_the_list_and_takes_the_choice() {
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Appearance,
        pane: Pane::Appearance,
    });
    assert!(
        shell.choice_rect(0, WIDE, Scale::ONE, &theme).is_none(),
        "no list is open to have a choice drawn"
    );
    let centre = |rect: Rect| {
        Point::new(
            rect.left() + to_i32(rect.width / 2),
            rect.top() + to_i32(rect.height / 2),
        )
    };
    let combo = shell
        .setting_rect(Setting::Appearance, WIDE, Scale::ONE, &theme)
        .expect("the pane draws the appearance row");
    click(&mut shell, centre(combo), WIDE, &theme);

    let dark = Appearance::ALL
        .iter()
        .position(|appearance| *appearance == Appearance::Dark)
        .expect("dark is offered");
    let choice = shell
        .choice_rect(dark, WIDE, Scale::ONE, &theme)
        .expect("the press opened the row's list");
    let mut sink = damage();
    let mut chosen = ShellOutcome::Idle;
    for event in [
        InputEvent::PointerMoved { to: centre(choice) },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        let outcome = shell.on_pointer(&event, WIDE, Scale::ONE, &theme, &mut sink);
        if outcome.document().is_some() {
            chosen = outcome;
        }
    }
    let document = chosen.document().expect("the choice asks for a document");
    assert!(document.contains("appearance = dark"), "{document}");
    assert!(
        shell.choice_rect(dark, WIDE, Scale::ONE, &theme).is_none(),
        "taking the choice closed the list"
    );
    assert!(
        shell
            .setting_rect(Setting::CursorSize, WIDE, Scale::ONE, &theme)
            .is_none(),
        "a setting only Accessibility shows is not drawn on Appearance"
    );
}

/// The window is titled with the pane on show, and follows every walk.
#[test]
fn the_window_is_titled_with_the_pane_on_show() {
    let theme = theme();
    let mut shell = shell();
    let title_of = |pane: Pane| pane.locate().expect("the pane is listed").1.title;
    assert_eq!(shell.title(), title_of(Pane::About));

    let storage = shell
        .rows()
        .iter()
        .position(|row| *row == StripRow::Category(Category::Storage))
        .expect("storage is a strip row");
    shell.lay_out(WIDE, Scale::ONE, &theme);
    shell.reveal_for_test(storage, WIDE, Scale::ONE, &theme);
    let at = row_point(&shell, storage, WIDE, &theme).expect("storage shows");
    click(&mut shell, at, WIDE, &theme);
    assert_eq!(shell.title(), title_of(Pane::Storage));
    assert_eq!(
        shell
            .selected_row()
            .and_then(|index| shell.rows().get(index).copied()),
        Some(StripRow::Category(Category::Storage)),
        "the row drawn selected is the one walked to"
    );
}

#[test]
fn the_rows_show_what_the_desktop_holds_not_the_defaults() {
    let settings = DesktopSettings {
        appearance: Appearance::Dark,
        contrast: Contrast::Monochrome,
        ..DesktopSettings::default()
    };
    let mut shell = Shell::new(settings.clone()).expect("a registry");
    let theme = theme();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Appearance,
            pane: Pane::Appearance,
        },
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().clone()),
        Some(settings)
    );
}

#[test]
fn adopting_the_desktops_answer_replaces_what_the_rows_show() {
    let mut shell = shell_at(Location {
        category: Category::Accessibility,
        pane: Pane::Accessibility,
    });
    let answered = DesktopSettings {
        density: Density::Compact,
        ..DesktopSettings::default()
    };
    shell.adopt_settings(answered.clone());
    assert_eq!(shell.settings_for_test(), &answered);
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().clone()),
        Some(answered)
    );
}

#[test]
fn a_refused_apply_reverts_the_row_to_what_the_store_holds() {
    // The session answers with what it still holds, so re-adopting after a
    // refusal must put the row back rather than leave the chosen value
    // standing.
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Appearance,
        pane: Pane::Appearance,
    });
    shell.focus_content_for_test(WIDE, Scale::ONE, &theme);
    let mut sink = damage();
    press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    press(&mut shell, Key::Named(NamedKey::Down), &theme, &mut sink);
    let chosen = press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    assert!(chosen.document().is_some());
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().appearance),
        Some(Appearance::Dark)
    );

    shell.adopt_settings(DesktopSettings::default());
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().appearance),
        Some(Appearance::Light)
    );
}

/// A choice refused before it reached the desktop puts the row back to what
/// the store last answered, with no store read to find out.
#[test]
fn a_choice_refused_before_it_was_asked_reverts_to_what_was_answered() {
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Appearance,
        pane: Pane::Appearance,
    });
    shell.focus_content_for_test(WIDE, Scale::ONE, &theme);
    let mut sink = damage();
    press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    press(&mut shell, Key::Named(NamedKey::Down), &theme, &mut sink);
    press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().appearance),
        Some(Appearance::Dark)
    );
    shell.revert_settings();
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().appearance),
        Some(Appearance::Light)
    );
}

/// The pointer pair is a real control now, so both rows must draw a choice
/// list rather than the statement that stood where they are.
#[test]
fn accessibility_offers_the_pointer_set_and_size_as_real_controls() {
    let shell = shell_at(Location {
        category: Category::Accessibility,
        pane: Pane::Accessibility,
    });
    let form = shell.form_for_test().expect("a form");
    for label in [Setting::CursorSet.label(), Setting::CursorSize.label()] {
        let row = form
            .groups()
            .iter()
            .flat_map(tairix_controls::FieldGroup::rows)
            .find(|row| row.label() == label)
            .unwrap_or_else(|| panic!("the {label} row"));
        assert!(matches!(
            row.control(),
            tairix_controls::FieldControl::Combo(_)
        ));
    }
}

/// The aids that help find the pointer are real controls on the pane, each
/// posting its own key, and shaking to find it is on before anyone asks.
#[test]
fn accessibility_offers_the_pointer_aids_and_each_posts_its_own_key() {
    let mut shell = shell_at(Location {
        category: Category::Accessibility,
        pane: Pane::Accessibility,
    });
    let form = shell.form_mut_for_test().expect("a form");
    let placed: alloc::vec::Vec<(usize, usize, &str)> = form
        .groups()
        .iter()
        .enumerate()
        .flat_map(|(group, held)| {
            held.rows()
                .iter()
                .enumerate()
                .map(move |(row, field)| (group, row, field.label()))
        })
        .collect();
    let at = |label: &str| {
        placed
            .iter()
            .find(|(_, _, held)| *held == label)
            .map_or_else(
                || panic!("the {label} row"),
                |(group, row, _)| (*group, *row),
            )
    };
    let rows = [
        (Setting::CursorShadow, 0, "cursor.shadow = true"),
        (Setting::CursorShake, 1, "cursor.shake = false"),
        (Setting::CursorLocate, 0, "cursor.locate = true"),
        (Setting::CursorTrail, 3, "cursor.trail = long"),
    ]
    .map(|(setting, index, posted)| (at(setting.label()), index, posted));
    let tairix_controls::FieldControl::Combo(shake) =
        form.groups()[rows[1].0 .0].rows()[rows[1].0 .1].control()
    else {
        panic!("the shake row is a choice");
    };
    assert_eq!(shake.selected_text(), Some("On"), "on by default");
    for ((group, row), index, posted) in rows {
        let crate::FormOutcome::Apply(document) = form.choose_for_test(group, row, index) else {
            panic!("{posted} is posted");
        };
        assert!(document.contains(posted), "{document}");
    }
}

/// Before the desktop answers, the pointer-set row still offers the
/// always-present built-in set: a list of nothing is a control that cannot
/// be used.
#[test]
fn the_pointer_set_row_offers_the_builtin_set_before_the_desktop_answers() {
    let shell = shell_at(Location {
        category: Category::Accessibility,
        pane: Pane::Accessibility,
    });
    let form = shell.form_for_test().expect("a form");
    let row = form
        .groups()
        .iter()
        .flat_map(tairix_controls::FieldGroup::rows)
        .find(|row| row.label() == Setting::CursorSet.label())
        .expect("the pointer-set row");
    let tairix_controls::FieldControl::Combo(combo) = row.control() else {
        panic!("the pointer-set row draws a choice list");
    };
    assert_eq!(combo.choices(), [CursorSetId::builtin().name()]);
}

/// The choice space is the desktop's to answer, and it reaches the row.
#[test]
fn an_answered_cursor_set_joins_the_pointer_set_row() {
    let mut shell = shell_at(Location {
        category: Category::Accessibility,
        pane: Pane::Accessibility,
    });
    let offered = CursorSetId::new("High Visibility").expect("a legal set name");
    shell.adopt_cursor_sets(alloc::vec![offered]);
    let form = shell.form_for_test().expect("a form");
    let row = form
        .groups()
        .iter()
        .flat_map(tairix_controls::FieldGroup::rows)
        .find(|row| row.label() == Setting::CursorSet.label())
        .expect("the pointer-set row");
    let tairix_controls::FieldControl::Combo(combo) = row.control() else {
        panic!("the pointer-set row draws a choice list");
    };
    assert_eq!(
        combo.choices(),
        [CursorSetId::builtin().name(), offered.name()]
    );
}

#[test]
fn both_composed_panes_offer_the_shared_settings_from_one_definition() {
    let appearance = Composition::Appearance.labels();
    let accessibility = Composition::Accessibility.labels();
    for shared in [
        Setting::Contrast.label(),
        Setting::Density.label(),
        Setting::Motion.label(),
        Setting::Scale.label(),
    ] {
        assert!(
            appearance.contains(&shared),
            "{shared} missing from Appearance"
        );
        assert!(
            accessibility.contains(&shared),
            "{shared} missing from Accessibility"
        );
    }
    // Light/dark is Appearance's alone; the pointer pair is
    // Accessibility's alone.
    assert!(appearance.contains(&Setting::Appearance.label()));
    assert!(!accessibility.contains(&Setting::Appearance.label()));
    assert!(accessibility.contains(&Setting::CursorSet.label()));
    assert!(accessibility.contains(&Setting::CursorSize.label()));
}

#[test]
fn a_composed_panes_settings_are_the_labels_its_rows_actually_draw() {
    // The search index is derived from the registry, so it must name
    // exactly what the composition draws — a term that reaches a row that
    // is not there is a search that lands nowhere.
    for (pane, composition) in [
        (Pane::Appearance, Composition::Appearance),
        (Pane::Accessibility, Composition::Accessibility),
        // A composition whose plates are discovered from a document only
        // an authenticated run answers contributes the subject a reader
        // searches for: no static index can name interfaces that do not
        // exist until someone asks.
        (Pane::Ethernet, Composition::Ethernet),
        (Pane::Dns, Composition::Dns),
        (Pane::Notifications, Composition::Notifications),
        (Pane::Mouse, Composition::Mouse),
        (Pane::Trackpad, Composition::Trackpad),
        (Pane::Keyboard, Composition::Keyboard),
        (Pane::Screensaver, Composition::Screensaver),
        (Pane::LockScreen, Composition::LockScreen),
    ] {
        let row = pane.locate().expect("a located pane").1;
        assert_eq!(row.settings, composition.labels().as_slice(), "{pane:?}");
    }
    // Storage's rows are discovered rather than declared, so its index is
    // the labels its cards carry rather than a composition's — one list,
    // read by the registry and drawn by the card.
    let storage = Pane::Storage.locate().expect("a located pane").1;
    assert_eq!(storage.settings, crate::volumes::VOLUME_FACTS);
}

/// A wheel detent moves the strip the one distance every view moves for a
/// detent, and repaints the bar whose thumb it moved as well as the rows.
#[test]
fn a_wheel_detent_over_the_strip_scrolls_it_the_wheel_step_and_repaints_its_bar() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 260);
    let mut shell = shell();
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let sidebar = frame.sidebar.expect("a strip");
    let bar = frame.strip_scrollbar.expect("the strip scrolls");
    let mut sink = damage();
    shell.on_pointer(
        &InputEvent::PointerMoved {
            to: sidebar.center(),
        },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let mut drew = damage();
    shell.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        short,
        Scale::ONE,
        &theme,
        &mut drew,
    );
    assert_eq!(shell.strip_offset_for_test(), u64::from(WHEEL_STEP));
    assert!(
        covers(&drew, bar),
        "the bar's thumb moved and was not repainted"
    );
    assert!(
        covers(&drew, sidebar),
        "the rows slid and were not repainted"
    );
}

#[test]
fn the_cursor_walks_from_one_group_into_the_next() {
    // A group clamps at its own ends, so without the form carrying the
    // cursor between groups every row below the first plate would be
    // unreachable from the keyboard.
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Appearance,
        pane: Pane::Appearance,
    });
    shell.focus_content_for_test(WIDE, Scale::ONE, &theme);
    let mut sink = damage();
    assert_eq!(shell.form_group_cursor_for_test(), Some((0, 0)));

    // The first group holds one row, so one Down must leave it.
    press(&mut shell, Key::Named(NamedKey::Down), &theme, &mut sink);
    assert_eq!(shell.form_group_cursor_for_test(), Some((1, 0)));

    // And Up comes back to the group above, landing on its last row.
    press(&mut shell, Key::Named(NamedKey::Up), &theme, &mut sink);
    assert_eq!(shell.form_group_cursor_for_test(), Some((0, 0)));
}

#[test]
fn walking_to_a_row_below_the_fold_scrolls_it_into_view() {
    // A short window cannot seat every group; a row the cursor reached but
    // the column does not show is a control the reader cannot use.
    let theme = theme();
    let short = Rect::new(0, 0, 900, 200);
    let mut shell = shell();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Accessibility,
            pane: Pane::Accessibility,
        },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    shell.lay_out(short, Scale::ONE, &theme);
    shell.focus_content_for_test(short, Scale::ONE, &theme);
    assert_eq!(shell.scroll_offset(), 0);

    // Walk to the very last row of the last group.
    for _ in 0..12 {
        shell.on_key(
            keystroke(Key::Named(NamedKey::Down)),
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    let (group, _) = shell
        .form_group_cursor_for_test()
        .expect("the cursor is on a row");
    let last = shell
        .form_for_test()
        .expect("a composed form")
        .groups()
        .len()
        - 1;
    assert_eq!(group, last, "the cursor did not reach the last group");
    assert!(
        shell.scroll_offset() > 0,
        "the column did not follow the cursor past the fold"
    );
    assert_cursor_row_shows_whole(&shell, short, &theme);
}

/// The keyboard cursor's row shows whole inside the pane's column.
fn assert_cursor_row_shows_whole(shell: &Shell, viewport: Rect, theme: &Theme) {
    let content = shell.frame(viewport, Scale::ONE, theme).content;
    let row = shell
        .cursor_row_rect_for_test(viewport, Scale::ONE, theme)
        .expect("the cursor's row shows");
    assert!(
        row.top() >= content.top() && row.bottom() <= content.bottom(),
        "{row:?} is cut by {content:?}"
    );
}

#[test]
fn walking_back_up_scrolls_a_row_above_the_fold_into_view() {
    // The mirror of the case above, and the one a narrowing cast hides: a
    // row scrolled off the *top* is above the frame, not at it.
    let theme = theme();
    let short = Rect::new(0, 0, 900, 200);
    let mut shell = shell();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Accessibility,
            pane: Pane::Accessibility,
        },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    shell.lay_out(short, Scale::ONE, &theme);
    shell.focus_content_for_test(short, Scale::ONE, &theme);
    for _ in 0..12 {
        shell.on_key(
            keystroke(Key::Named(NamedKey::Down)),
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    assert!(shell.scroll_offset() > 0, "the walk down did not scroll");

    for _ in 0..12 {
        shell.on_key(
            keystroke(Key::Named(NamedKey::Up)),
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    assert_eq!(shell.form_group_cursor_for_test(), Some((0, 0)));
    assert_eq!(
        shell.scroll_offset(),
        0,
        "the column did not follow the cursor back to the top, caption and all"
    );
    assert_cursor_row_shows_whole(&shell, short, &theme);
}

/// The system volume, as the mount table reports it.
fn volume(source: &[u8], target: &[u8], blocks: u64, free: u64) -> VolumeReading {
    let record = MountRecord::new(
        source,
        target,
        b"arxfs",
        MountFlags::READ_ONLY,
        MountVolumeState {
            usage: VolumeStats {
                block_size: 4096,
                total_blocks: blocks,
                free_blocks: free,
                avail_blocks: free,
                ..VolumeStats::default()
            },
            availability: MountAvailability::Available,
            medium: Some(BlkDeviceClass::SolidState),
        },
        [0; 16],
    )
    .expect("a well-formed record");
    VolumeReading::of(&record)
}

/// Where the storage pane lives.
const STORAGE: Location = Location {
    category: Category::Storage,
    pane: Pane::Storage,
};

#[test]
fn the_storage_pane_asks_for_the_mount_table_only_when_it_is_on_show() {
    // The walk is an IPC round trip, so the pane never makes one itself: it
    // says it wants one and draws whatever has already arrived. Asking for
    // it on every navigation would spend a round trip per pane.
    let mut shell = shell();
    assert!(
        !shell.volumes_wanted(),
        "a pane that lists no volume asked for the mount table"
    );

    let theme = theme();
    let mut sink = damage();
    shell.go_to_for_test(STORAGE, WIDE, Scale::ONE, &theme, &mut sink);
    assert!(shell.volumes_wanted(), "the pane on show asked for nothing");

    shell.adopt_volumes(alloc::vec![volume(b"arx0p2", b"/System", 1024, 768)]);
    assert!(
        !shell.volumes_wanted(),
        "an answered walk was asked for again"
    );

    // Leaving and coming back asks afresh, because unlike the picture store
    // the mount table moves.
    shell.go_to_for_test(
        Location {
            category: Category::Appearance,
            pane: Pane::Appearance,
        },
        WIDE,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert!(!shell.volumes_wanted());
    shell.go_to_for_test(STORAGE, WIDE, Scale::ONE, &theme, &mut sink);
    assert!(shell.volumes_wanted());
}

#[test]
fn the_storage_pane_opens_on_what_has_arrived_and_grows_when_the_rest_does() {
    let theme = theme();
    let mut shell = shell_at(STORAGE);
    // Nothing has been answered yet, so there is nothing to draw — and that
    // is a pane with no cards, never a fabricated one.
    let empty = shell
        .readings_for_test()
        .expect("the storage pane draws its volumes");
    assert!(empty.is_empty());
    let bare = shell.pane_height(600, Scale::ONE, &theme);

    shell.adopt_volumes(alloc::vec![
        volume(b"arx0p2", b"/System", 1024, 768),
        volume(b"arx0p3", b"/Users", 4096, 1024),
    ]);
    let readings = shell
        .readings_for_test()
        .expect("the storage pane draws its volumes");
    assert_eq!(readings.len(), 2);
    assert_eq!(readings.caption(0), Some("arx0p2"));
    assert_eq!(readings.caption(1), Some("arx0p3"));
    assert!(
        shell.pane_height(600, Scale::ONE, &theme) > bare,
        "the answered volumes did not grow the column"
    );
}

#[test]
fn the_storage_panes_column_scrolls_a_detent_by_the_wheel_step_through_cut_cards() {
    // The cards are laid out whole and shown through the column, so a detent
    // moves it the wheel step rather than a whole card, and the card the
    // column's top edge crosses is drawn cut rather than dropped.
    let theme = theme();
    let short = Rect::new(0, 0, 900, 300);
    let mut shell = shell();
    let mut sink = damage();
    shell.go_to_for_test(STORAGE, short, Scale::ONE, &theme, &mut sink);
    shell.adopt_volumes(alloc::vec![
        volume(b"a", b"/Storage/a", 8, 4),
        volume(b"b", b"/Storage/b", 8, 4),
        volume(b"c", b"/Storage/c", 8, 4),
    ]);
    shell.lay_out(short, Scale::ONE, &theme);

    let frame = shell.frame(short, Scale::ONE, &theme);
    assert!(frame.scrollbar.is_some(), "three cards do not fit 300px");
    let at = Point::new(
        frame.content.left() + to_i32(frame.content.width / 2),
        frame.content.top() + to_i32(frame.content.height / 2),
    );
    shell.on_pointer(
        &InputEvent::PointerMoved { to: at },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let mut drew = damage();
    shell.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        short,
        Scale::ONE,
        &theme,
        &mut drew,
    );
    assert_eq!(shell.scroll_offset(), u64::from(WHEEL_STEP));
    let bar = frame.scrollbar.expect("a bar");
    assert!(covers(&drew, bar), "the pane's bar was not repainted");
    assert!(covers(&drew, frame.content), "the cards slid unrepainted");

    // The first card now starts above the column and is drawn cut at its
    // top edge, not dropped: its plate reaches the column's first row.
    let mut surface = Surface::new(short.width, short.height).expect("a surface");
    shell.render(
        &mut surface,
        short,
        Scale::ONE,
        opaque(&theme),
        &mut NoArtwork,
    );
    let background = surface.get(
        u32::try_from(frame.content.left()).unwrap_or(0) + 1,
        u32::try_from(frame.content.top()).unwrap_or(0),
    );
    let top = u32::try_from(frame.content.top()).unwrap_or(0);
    let left = u32::try_from(frame.content.left()).unwrap_or(0);
    assert!(
        (left..left + frame.content.width).any(|x| surface.get(x, top) != background),
        "nothing of the cut card is drawn at the column's top edge"
    );
}

#[test]
fn the_storage_pane_offers_no_control_to_act_on() {
    // Read-only: mounting and unmounting are the file manager's and
    // `mount`'s, so the pane composes no settable and the pane column's
    // keyboard is its scrollbar's.
    let theme = theme();
    let short = Rect::new(0, 0, 900, 300);
    let mut shell = shell();
    let mut sink = damage();
    shell.go_to_for_test(STORAGE, short, Scale::ONE, &theme, &mut sink);
    shell.adopt_volumes(alloc::vec![
        volume(b"a", b"/Storage/a", 8, 4),
        volume(b"b", b"/Storage/b", 8, 4),
    ]);
    shell.lay_out(short, Scale::ONE, &theme);
    assert!(shell.form_for_test().is_none());

    shell.focus_content_for_test(short, Scale::ONE, &theme);
    // The column is on the focus ring because it scrolls, and what the
    // keyboard drives there is the scrollbar: `Down` moves the column and
    // nothing asks the desktop to change anything.
    let acted = shell.on_key(
        keystroke(Key::Named(NamedKey::Down)),
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert!(acted.changed());
    assert_eq!(acted.document(), None, "a read-only pane posted a document");
    assert_eq!(
        shell.scroll_offset(),
        u64::from(Scale::ONE.scale_length(theme.metrics().control_height)),
        "a line step is a control's height"
    );
}

#[test]
fn the_storage_pane_draws_in_both_themes_and_at_both_densities() {
    for theme in [Theme::dark(), Theme::light()] {
        for scale in [Scale::ONE, Scale::from_percent(200).expect("a valid scale")] {
            install_test_transport();
            let mut shell = shell_at(STORAGE);
            shell.adopt_volumes(alloc::vec![
                volume(b"arx0p2", b"/System", 1024, 768),
                // One that reports no capacity at all: it must render its
                // stated absence rather than a bar of nothing.
                VolumeReading::of(
                    &MountRecord::new(
                        b"",
                        b"/",
                        b"tairixfs",
                        MountFlags::READ_ONLY,
                        MountVolumeState {
                            usage: VolumeStats::default(),
                            availability: MountAvailability::Degraded,
                            medium: None,
                        },
                        [0; 16],
                    )
                    .expect("a well-formed record"),
                ),
            ]);
            shell.lay_out(WIDE, scale, &theme);
            let mut surface = Surface::new(WIDE.width, WIDE.height).expect("a surface");
            shell.render(&mut surface, WIDE, scale, opaque(&theme), &mut NoArtwork);
            assert!(
                surface.pixels().iter().any(|p| p.a > 0),
                "the storage pane drew nothing"
            );
        }
    }
}

// --- Pointer routing: leaving, presenting, and focus -------------------

/// A row the pointer moved off stopped looking hovered: the region a move
/// leaves is shown the move, not only the region it arrives in.
#[test]
fn a_strip_row_the_pointer_leaves_stops_looking_hovered() {
    let theme = theme();
    let mut shell = shell();
    shell.lay_out(WIDE, Scale::ONE, &theme);
    let rested = shell.strip_for_test().clone();
    let mut sink = damage();
    let row = row_point(&shell, 1, WIDE, &theme).expect("row 1 shows");
    let moved = |shell: &mut Shell, to, sink: &mut Region| {
        shell.on_pointer(
            &InputEvent::PointerMoved { to },
            WIDE,
            Scale::ONE,
            &theme,
            sink,
        )
    };
    moved(&mut shell, row, &mut sink);
    assert_ne!(shell.strip_for_test(), &rested, "the row hovered");
    let content = shell.frame(WIDE, Scale::ONE, &theme).content.center();
    moved(&mut shell, content, &mut sink);
    assert_eq!(
        shell.strip_for_test(),
        &rested,
        "the hover stuck on the strip"
    );
}

/// A round that repainted anything is presented: the caller presents only
/// what an outcome says changed, and a hover moving along the strip used to
/// report its damage beside an idle outcome that dropped it.
#[test]
fn a_hover_moving_along_the_strip_is_presented() {
    let theme = theme();
    let mut shell = shell();
    shell.lay_out(WIDE, Scale::ONE, &theme);
    let mut drew = damage();
    let acted = shell.on_pointer(
        &InputEvent::PointerMoved {
            to: row_point(&shell, 1, WIDE, &theme).expect("row 1 shows"),
        },
        WIDE,
        Scale::ONE,
        &theme,
        &mut drew,
    );
    assert!(!drew.is_empty(), "the row's hover look was reported");
    assert!(acted.changed(), "…and must be presented");
}

/// The keyboard cursor follows a press, never a hover: a reader typing in
/// the search field keeps typing there while the pointer crosses the strip
/// and the pane.
#[test]
fn a_hover_never_takes_the_keyboard_cursor() {
    let theme = theme();
    let mut shell = shell();
    shell.lay_out(WIDE, Scale::ONE, &theme);
    let frame = shell.frame(WIDE, Scale::ONE, &theme);
    click(
        &mut shell,
        frame.search.expect("a search field").center(),
        WIDE,
        &theme,
    );
    let mut sink = damage();
    let typed = |shell: &mut Shell, ch, sink: &mut Region| {
        shell.on_key(keystroke(Key::Char(ch)), WIDE, Scale::ONE, &theme, sink);
    };
    typed(&mut shell, 'w', &mut sink);
    for to in [
        row_point(&shell, 0, WIDE, &theme).expect("row 0 shows"),
        frame.content.center(),
    ] {
        shell.on_pointer(
            &InputEvent::PointerMoved { to },
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    typed(&mut shell, 'a', &mut sink);
    assert_eq!(
        shell.search_text_for_test(),
        "wa",
        "the cursor left the field"
    );
}

// --- The two columns scroll by pixels ----------------------------------

#[test]
fn walking_the_strip_to_its_end_repaints_the_strip_and_its_bar() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 260);
    let mut shell = shell();
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let bar = frame.strip_scrollbar.expect("the strip scrolls");
    let mut drew = damage();
    shell.on_key(
        keystroke(Key::Named(NamedKey::End)),
        short,
        Scale::ONE,
        &theme,
        &mut drew,
    );
    assert!(
        shell.strip_offset_for_test() > 0,
        "the strip followed the cursor"
    );
    assert!(
        covers(&drew, bar),
        "the bar's thumb moved and was not repainted"
    );
}

#[test]
fn a_wheel_over_either_bar_scrolls_the_column_it_belongs_to() {
    let theme = theme();
    let short = Rect::new(0, 0, 420, 110);
    let mut pane = stating();
    pane.lay_out(short, Scale::ONE, &theme);
    let frame = pane.frame(short, Scale::ONE, &theme);
    let mut sink = damage();
    let wheel = InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    };
    let bar = frame.scrollbar.expect("the pane scrolls");
    pane.on_pointer(
        &InputEvent::PointerMoved { to: bar.center() },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    pane.on_pointer(&wheel, short, Scale::ONE, &theme, &mut sink);
    assert!(pane.scroll_offset() > 0, "a wheel over the pane's bar");

    let tall_strip = Rect::new(0, 0, 900, 260);
    let mut shell = shell();
    shell.lay_out(tall_strip, Scale::ONE, &theme);
    let bar = shell
        .frame(tall_strip, Scale::ONE, &theme)
        .strip_scrollbar
        .expect("the strip scrolls");
    shell.on_pointer(
        &InputEvent::PointerMoved { to: bar.center() },
        tall_strip,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    shell.on_pointer(&wheel, tall_strip, Scale::ONE, &theme, &mut sink);
    assert!(
        shell.strip_offset_for_test() > 0,
        "a wheel over the strip's bar"
    );
}

#[test]
fn the_strips_bar_goes_once_the_window_is_tall_enough_for_every_row() {
    let theme = theme();
    let mut shell = shell();
    let short = Rect::new(0, 0, 900, 260);
    shell.lay_out(short, Scale::ONE, &theme);
    assert!(shell
        .frame(short, Scale::ONE, &theme)
        .strip_scrollbar
        .is_some());
    shell.reveal_for_test(shell.rows().len() - 1, short, Scale::ONE, &theme);
    assert!(shell.strip_offset_for_test() > 0);

    let tall = Rect::new(0, 0, 900, 4000);
    shell.lay_out(tall, Scale::ONE, &theme);
    assert!(
        shell
            .frame(tall, Scale::ONE, &theme)
            .strip_scrollbar
            .is_none(),
        "a strip that fits keeps no bar"
    );
    assert_eq!(
        shell.strip_offset_for_test(),
        0,
        "nor an offset past its end"
    );
}

#[test]
fn the_line_step_is_a_control_height_at_the_desktops_density() {
    let theme = theme();
    let double = Scale::from_percent(200).expect("a valid scale");
    let short = Rect::new(0, 0, 840, 220);
    let mut shell = stating();
    shell.lay_out(short, double, &theme);
    let frame = shell.frame(short, double, &theme);
    assert!(
        frame.scrollbar.is_some(),
        "the statement scrolls at this size"
    );
    let mut sink = damage();
    shell.focus_content_for_test(short, double, &theme);
    shell.on_key(
        keystroke(Key::Named(NamedKey::Down)),
        short,
        double,
        &theme,
        &mut sink,
    );
    assert_eq!(
        shell.scroll_offset(),
        u64::from(double.scale_length(theme.metrics().control_height)),
        "a line is a control's height at the scale the window is drawn at"
    );
}

/// A plate the column's edge crosses is drawn whole and cut there, and the
/// part of a control that shows still answers a press.
#[test]
fn a_part_scrolled_plate_is_drawn_cut_and_its_rows_still_answer() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 200);
    let mut shell = shell();
    let mut sink = damage();
    shell.go_to_for_test(
        Location {
            category: Category::Accessibility,
            pane: Pane::Accessibility,
        },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let bar = frame.scrollbar.expect("the pane scrolls at this size");
    // Drag nothing: scroll a few pixels at a time until a control is cut by
    // the column's top edge.
    let whole = shell
        .row_control_rect_for_test((0, 0), short, Scale::ONE, &theme)
        .expect("the first row shows");
    shell.on_pointer(
        &InputEvent::PointerMoved { to: bar.center() },
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    let mut cut = None;
    for _ in 0..40 {
        shell.on_pointer(
            &InputEvent::PointerScrolled { dx: 0, dy: 20 },
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
        if let Some(shown) = shell
            .row_control_rect_for_test((0, 0), short, Scale::ONE, &theme)
            .filter(|shown| shown.height < whole.height)
        {
            cut = Some(shown);
            break;
        }
    }
    let shown = cut.expect("some offset cuts the first control at the top edge");
    assert_eq!(shown.top(), frame.content.top(), "cut at the column's edge");

    let mut surface = Surface::new(short.width, short.height).expect("a surface");
    shell.render(
        &mut surface,
        short,
        Scale::ONE,
        opaque(&theme),
        &mut NoArtwork,
    );
    let row = u32::try_from(shown.top()).unwrap_or(0);
    let left = u32::try_from(shown.left()).unwrap_or(0);
    let background = surface.get(left, row);
    assert!(
        (left..left + shown.width).any(|x| surface.get(x, row + shown.height / 2) != background)
            || shown.height == 0,
        "nothing of the cut control is drawn"
    );

    let before = shell.settings_for_test().clone();
    click(&mut shell, shown.center(), short, &theme);
    assert!(
        shell
            .form_for_test()
            .is_some_and(crate::form::Form::is_listing)
            || shell.settings_for_test() != &before,
        "a press on the part that shows reached the control"
    );
}

// --- An open choice list stands over the rest of the window ------------

/// The pinboard group sits above the pictures, so its last row's list hangs
/// over them: it must be drawn above them, not beneath.
#[test]
fn an_open_choice_list_is_drawn_above_the_pictures_it_hangs_over() {
    let theme = theme();
    let mut shell = crate::test_support::showing("wallpaper");
    shell.adopt_catalog(
        (0..12)
            .map(|at| tairix_wallpaper::CatalogItem {
                category: alloc::string::String::from("TAIRiX"),
                file: alloc::format!("{at}.png"),
            })
            .collect(),
    );
    shell.lay_out(WIDE, Scale::ONE, &theme);
    let last_row = shell.form_for_test().expect("a form").groups()[0]
        .rows()
        .len()
        - 1;
    let field = shell
        .row_control_rect_for_test((0, last_row), WIDE, Scale::ONE, &theme)
        .expect("the last pinboard row shows");
    let closed = rendered(&shell, &theme);
    click(&mut shell, field.center(), WIDE, &theme);
    let choice = shell
        .choice_rect(0, WIDE, Scale::ONE, &theme)
        .expect("the list opened");
    let open = rendered(&shell, &theme);
    let x = u32::try_from(choice.center().x).unwrap_or(0);
    let y = u32::try_from(choice.center().y).unwrap_or(0);
    assert_ne!(
        closed.get(x, y),
        open.get(x, y),
        "the list is hidden beneath what it hangs over"
    );
}

/// The window drawn whole, for a test that asks what the reader sees.
fn rendered(shell: &Shell, theme: &Theme) -> Surface {
    let mut surface = Surface::new(WIDE.width, WIDE.height).expect("a surface");
    shell.render(
        &mut surface,
        WIDE,
        Scale::ONE,
        opaque(theme),
        &mut NoArtwork,
    );
    surface
}

// --- The pictures are reachable from the keyboard -----------------------

/// A wallpaper pane with `count` pictures, laid out for `viewport`: none of
/// them the picture in effect, which is listed last.
fn pictures(count: usize, viewport: Rect) -> Shell {
    let mut shell = shell();
    let mut sink = damage();
    assert!(shell.go_to_pane("wallpaper", viewport, Scale::ONE, &theme(), &mut sink));
    shell.adopt_catalog(
        (0..count)
            .map(|at| tairix_wallpaper::CatalogItem {
                category: alloc::string::String::from("TAIRiX"),
                file: alloc::format!("{at}.png"),
            })
            .collect(),
    );
    shell.lay_out(viewport, Scale::ONE, &theme());
    shell
}

/// The wallpaper chooser the pane on show draws.
fn chooser(shell: &Shell) -> &tairix_controls::PictureChoice {
    shell
        .form_for_test()
        .and_then(|form| form.groups().iter().find_map(FieldGroup::pictures))
        .expect("a chooser")
}

/// Every control on a pane is reachable without a pointer, the pictures
/// included: Down past the rows above steps onto the chooser, the arrows walk
/// it, Enter chooses, and Up from its first line steps back out.
#[test]
fn the_keyboard_walks_from_the_rows_onto_the_pictures_and_back() {
    let theme = theme();
    let mut shell = pictures(6, WIDE);
    shell.focus_content_for_test(WIDE, Scale::ONE, &theme);
    let mut sink = damage();
    let rows: usize = shell
        .form_for_test()
        .expect("a form")
        .groups()
        .iter()
        .map(|group| group.rows().len())
        .sum();
    for _ in 0..rows {
        press(&mut shell, Key::Named(NamedKey::Down), &theme, &mut sink);
    }
    assert!(
        chooser(&shell).is_focused(),
        "Down past the last row reached the pictures"
    );
    assert_eq!(shell.form_group_cursor_for_test(), Some((1, 0)));
    // The cursor starts on the picture in effect, listed last.
    assert_eq!(chooser(&shell).cursor(), chooser(&shell).len() - 1);

    press(&mut shell, Key::Named(NamedKey::Left), &theme, &mut sink);
    let chosen = chooser(&shell).cursor();
    let acted = press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    assert!(
        acted.document().is_some(),
        "Enter chose and posted the picture"
    );
    assert_eq!(chooser(&shell).selected(), Some(chosen));

    press(&mut shell, Key::Named(NamedKey::Home), &theme, &mut sink);
    press(&mut shell, Key::Named(NamedKey::Up), &theme, &mut sink);
    assert!(!chooser(&shell).is_focused());
    let above = shell.form_for_test().expect("a form").groups()[0]
        .rows()
        .len()
        - 1;
    assert_eq!(
        shell.form_group_cursor_for_test(),
        Some((0, above)),
        "Up from the first line lands on the row just above"
    );
}

#[test]
fn walking_the_pictures_scrolls_the_one_the_cursor_lands_on_into_view() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 420);
    let mut shell = pictures(60, short);
    let bar = shell
        .frame(short, Scale::ONE, &theme)
        .scrollbar
        .expect("sixty pictures overflow the column");
    shell.focus_content_for_test(short, Scale::ONE, &theme);
    let mut sink = damage();
    let rows: usize = shell
        .form_for_test()
        .expect("a form")
        .groups()
        .iter()
        .map(|group| group.rows().len())
        .sum();
    for _ in 0..rows {
        press_in(
            &mut shell,
            Key::Named(NamedKey::Down),
            short,
            &theme,
            &mut sink,
        );
    }
    let whole = {
        let one = tairix_controls::PictureChoice::new(
            tairix_controls::Aspect::WIDESCREEN,
            alloc::vec![tairix_controls::PictureSection::untitled(alloc::vec![
                tairix_controls::PictureItem::new("x", tairix_icon::IconKind::Image)
            ])],
        );
        one.measured_height(900, Scale::ONE, &theme)
    };
    let shown_whole = |shell: &Shell| {
        let cursor = chooser(shell).cursor();
        shell
            .picture_rect(Chooser::Wallpaper, cursor, short, Scale::ONE, &theme)
            .is_some_and(|rect| rect.height == whole)
    };
    // The picture in effect is listed last, so stepping onto the chooser
    // already scrolled to it.
    assert!(
        shell.scroll_offset() > 0,
        "the column followed the cursor in"
    );
    assert!(shown_whole(&shell));
    let mut offsets = alloc::vec::Vec::new();
    for key in [NamedKey::Home, NamedKey::End] {
        let mut drew = damage();
        press_in(&mut shell, Key::Named(key), short, &theme, &mut drew);
        assert!(shown_whole(&shell), "{key:?} left its picture in view");
        assert!(covers(&drew, bar), "{key:?} moved the thumb unrepainted");
        offsets.push(shell.scroll_offset());
    }
    assert!(
        offsets[0] < offsets[1],
        "Home and End scroll apart: {offsets:?}"
    );
}

/// Press `key` in a window of `viewport`.
fn press_in(shell: &mut Shell, key: Key, viewport: Rect, theme: &Theme, sink: &mut Region) {
    shell.on_key(keystroke(key), viewport, Scale::ONE, theme, sink);
}

// --- A still pointer follows the content that moves under it -----------

/// Move the pointer to `to` in a window of `viewport`.
fn point_at(shell: &mut Shell, to: Point, viewport: Rect, theme: &Theme, sink: &mut Region) {
    shell.on_pointer(
        &InputEvent::PointerMoved { to },
        viewport,
        Scale::ONE,
        theme,
        sink,
    );
}

/// Turn the wheel one detent toward the end, in a window of `viewport`.
fn detent(shell: &mut Shell, viewport: Rect, theme: &Theme, sink: &mut Region) {
    shell.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        viewport,
        Scale::ONE,
        theme,
        sink,
    );
}

/// Take the pointer to the location trail and back to `at`, so whatever it
/// lights at `at` is derived afresh from where the content now lies.
fn re_point(shell: &mut Shell, at: Point, viewport: Rect, theme: &Theme) {
    let mut sink = damage();
    let away = shell.frame(viewport, Scale::ONE, theme).breadcrumb.center();
    point_at(shell, away, viewport, theme, &mut sink);
    point_at(shell, at, viewport, theme, &mut sink);
}

/// The strip row the window draws under `at`.
fn strip_row_under(shell: &Shell, at: Point, viewport: Rect, theme: &Theme) -> Option<usize> {
    (0..shell.rows().len()).find(|&index| {
        shell
            .strip_row_rect(index, viewport, Scale::ONE, theme)
            .is_some_and(|rect| rect.contains(at))
    })
}

/// The form row whose control the window draws under `at`, as its group and
/// row.
fn form_control_under(
    shell: &Shell,
    at: Point,
    viewport: Rect,
    theme: &Theme,
) -> Option<(usize, usize)> {
    let form = shell.form_for_test()?;
    form.groups().iter().enumerate().find_map(|(group, plate)| {
        (0..plate.rows().len()).find_map(|row| {
            shell
                .row_control_rect_for_test((group, row), viewport, Scale::ONE, theme)
                .filter(|rect| rect.contains(at))
                .map(|_| (group, row))
        })
    })
}

/// A short window whose strip scrolls, laid out.
fn short_strip() -> (Shell, Rect) {
    let short = Rect::new(0, 0, 900, 260);
    let mut shell = shell();
    shell.lay_out(short, Scale::ONE, &theme());
    (shell, short)
}

/// A row the wheel carries out from under a still pointer gives up its hover
/// to the row the wheel brings under it, and the round reports the rows and
/// the bar it moved.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_strips_hover_to_the_row_now_under_it() {
    let theme = theme();
    let (mut shell, short) = short_strip();
    let frame = shell.frame(short, Scale::ONE, &theme);
    let (sidebar, bar) = (
        frame.sidebar.expect("a strip"),
        frame.strip_scrollbar.expect("the strip scrolls"),
    );
    let at = row_point(&shell, 1, short, &theme).expect("row 1 shows");
    let mut sink = damage();
    point_at(&mut shell, at, short, &theme, &mut sink);
    let stale = shell.strip_for_test().clone();

    let mut drew = damage();
    detent(&mut shell, short, &theme, &mut drew);
    assert_eq!(shell.strip_offset_for_test(), u64::from(WHEEL_STEP));
    let now_under = strip_row_under(&shell, at, short, &theme);
    assert!(
        now_under.is_some_and(|row| row != 1),
        "a detent brings another row under the pointer: {now_under:?}"
    );
    let (mut fresh, _) = short_strip();
    point_at(&mut fresh, at, short, &theme, &mut sink);
    detent(&mut fresh, short, &theme, &mut sink);
    re_point(&mut fresh, at, short, &theme);
    assert_ne!(
        fresh.strip_for_test(),
        &stale,
        "the premise: the lit row moves"
    );
    assert_eq!(
        shell.strip_for_test(),
        fresh.strip_for_test(),
        "the hover stayed on the row the wheel carried away"
    );
    assert!(covers(&drew, sidebar), "the rows slid unrepainted");
    assert!(covers(&drew, bar), "the thumb moved unrepainted");
}

/// The same for a form's controls under the pane's own wheel: a setting's row
/// takes no hover, so what moves is the control the pointer rests on.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_panes_hover_to_the_control_now_under_it() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 300);
    let accessibility = || {
        let mut shell = shell();
        let mut sink = damage();
        shell.go_to_for_test(
            Location {
                category: Category::Accessibility,
                pane: Pane::Accessibility,
            },
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
        shell.lay_out(short, Scale::ONE, &theme);
        shell
    };
    let mut shell = accessibility();
    let frame = shell.frame(short, Scale::ONE, &theme);
    let bar = frame.scrollbar.expect("the pane scrolls at this size");
    // A spot on one row's control that a detent puts on another's.
    let controls: Vec<((usize, usize), Rect)> = {
        let groups = shell.form_for_test().expect("a form").groups();
        let places = groups
            .iter()
            .enumerate()
            .flat_map(|(group, plate)| (0..plate.rows().len()).map(move |row| (group, row)));
        places
            .filter_map(|place| {
                shell
                    .row_control_rect_for_test(place, short, Scale::ONE, &theme)
                    .map(|rect| (place, rect))
            })
            .collect()
    };
    let (first, at) = controls
        .iter()
        .find_map(|(place, rect)| {
            controls.iter().find_map(|(other, below)| {
                let carried = Rect::new(
                    below.left(),
                    below.top() - to_i32(WHEEL_STEP),
                    below.width,
                    below.height,
                );
                let both = rect.intersection(&carried);
                (other != place && !both.is_empty()).then_some((*place, both.center()))
            })
        })
        .expect("a detent carries one control onto another's place");
    let mut sink = damage();
    point_at(&mut shell, at, short, &theme, &mut sink);
    assert_eq!(form_control_under(&shell, at, short, &theme), Some(first));
    let stale = shell.form_for_test().expect("a form").groups().to_vec();

    let mut drew = damage();
    detent(&mut shell, short, &theme, &mut drew);
    assert_eq!(shell.scroll_offset(), u64::from(WHEEL_STEP));
    let now_under = form_control_under(&shell, at, short, &theme);
    assert!(
        now_under.is_some_and(|place| place != first),
        "a detent brings another row's control under the pointer: {now_under:?}"
    );
    let mut fresh = accessibility();
    point_at(&mut fresh, at, short, &theme, &mut sink);
    detent(&mut fresh, short, &theme, &mut sink);
    re_point(&mut fresh, at, short, &theme);
    let fresh = fresh.form_for_test().expect("a form").groups().to_vec();
    assert_ne!(fresh, stale, "the premise: the lit control moves");
    assert_eq!(
        shell.form_for_test().expect("a form").groups(),
        fresh.as_slice(),
        "the hover stayed on the control the wheel carried away"
    );
    assert!(covers(&drew, frame.content), "the rows slid unrepainted");
    assert!(covers(&drew, bar), "the thumb moved unrepainted");
}

/// The same for the pictures of a chooser, which scroll with the rows.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_hover_to_the_picture_now_under_it() {
    let theme = theme();
    let window = Rect::new(0, 0, 900, 900);
    let mut shell = pictures(60, window);
    let bar = shell
        .frame(window, Scale::ONE, &theme)
        .scrollbar
        .expect("sixty pictures overflow the column");
    let rect = |shell: &Shell, index: usize| {
        shell.picture_rect(Chooser::Wallpaper, index, window, Scale::ONE, &theme)
    };
    let first = rect(&shell, 1).expect("the first line of pictures shows");
    let below = (2..61)
        .find(|index| rect(&shell, *index).is_some_and(|tile| tile.top() > first.bottom()))
        .expect("a second line shows");
    let target = rect(&shell, below).expect("it shows");
    // Over the first line now, and just inside the second's top edge once a
    // detent has carried it up.
    let at = Point::new(target.center().x, target.top() + 1 - to_i32(WHEEL_STEP));
    assert!(
        first.contains(at),
        "the premise: a detent is less than a line"
    );
    let mut sink = damage();
    point_at(&mut shell, at, window, &theme, &mut sink);
    let stale = shell.form_for_test().expect("a form").groups().to_vec();

    let mut drew = damage();
    detent(&mut shell, window, &theme, &mut drew);
    assert_eq!(shell.scroll_offset(), u64::from(WHEEL_STEP));
    let mut fresh = pictures(60, window);
    point_at(&mut fresh, at, window, &theme, &mut sink);
    detent(&mut fresh, window, &theme, &mut sink);
    re_point(&mut fresh, at, window, &theme);
    let fresh = fresh.form_for_test().expect("a form").groups().to_vec();
    assert_ne!(fresh, stale, "the premise: the lit picture moves");
    assert_eq!(
        shell.form_for_test().expect("a form").groups(),
        fresh.as_slice(),
        "the hover stayed on the picture the wheel carried away"
    );
    let frame = shell.frame(window, Scale::ONE, &theme);
    assert!(
        covers(&drew, frame.content),
        "the pictures slid unrepainted"
    );
    assert!(covers(&drew, bar), "the thumb moved unrepainted");
}

/// A key that scrolls the strip to reveal its cursor moves the hover with the
/// rows exactly as the wheel does.
#[test]
fn a_keyboard_reveal_under_a_still_pointer_moves_the_hover_to_the_row_now_under_it() {
    let theme = theme();
    let (mut shell, short) = short_strip();
    let at = row_point(&shell, 1, short, &theme).expect("row 1 shows");
    let mut sink = damage();
    point_at(&mut shell, at, short, &theme, &mut sink);
    let stale = shell.strip_for_test().clone();

    press_in(
        &mut shell,
        Key::Named(NamedKey::End),
        short,
        &theme,
        &mut sink,
    );
    assert!(shell.strip_offset_for_test() > 0, "End scrolled the strip");
    let now_under = strip_row_under(&shell, at, short, &theme);
    assert!(
        now_under.is_some_and(|row| row != 1),
        "the reveal brought another row under the pointer: {now_under:?}"
    );
    let (mut fresh, _) = short_strip();
    point_at(&mut fresh, at, short, &theme, &mut sink);
    press_in(
        &mut fresh,
        Key::Named(NamedKey::End),
        short,
        &theme,
        &mut sink,
    );
    re_point(&mut fresh, at, short, &theme);
    assert_ne!(
        fresh.strip_for_test(),
        &stale,
        "the premise: the lit row moves"
    );
    assert_eq!(
        shell.strip_for_test(),
        fresh.strip_for_test(),
        "the hover stayed on the row the reveal carried away"
    );
}

/// A relayout that clamps the pane's offset — here a window grown tall enough
/// to hold the whole pane — re-derives the hover from where the pointer
/// rests, as the rounds that scroll do.
#[test]
fn a_relayout_that_clamps_the_offset_moves_the_hover_to_the_control_now_under_it() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 300);
    let tall = Rect::new(0, 0, 900, 1600);
    let scrolled = || {
        let mut shell = shell();
        let mut sink = damage();
        shell.go_to_for_test(
            Location {
                category: Category::Accessibility,
                pane: Pane::Accessibility,
            },
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
        shell.lay_out(short, Scale::ONE, &theme);
        let bar = shell
            .frame(short, Scale::ONE, &theme)
            .scrollbar
            .expect("the pane scrolls at this size");
        point_at(&mut shell, bar.center(), short, &theme, &mut sink);
        for _ in 0..3 {
            detent(&mut shell, short, &theme, &mut sink);
        }
        shell
    };
    let mut shell = scrolled();
    assert!(
        shell.scroll_offset() > 0,
        "the premise: the pane is scrolled"
    );
    let content = shell.frame(short, Scale::ONE, &theme).content;
    let rows = shell
        .form_for_test()
        .expect("a form")
        .groups()
        .iter()
        .enumerate();
    let at = rows
        .flat_map(|(group, plate)| (0..plate.rows().len()).map(move |row| (group, row)))
        .find_map(|place| {
            shell
                .row_control_rect_for_test(place, short, Scale::ONE, &theme)
                .map(|rect| rect.center())
                .filter(|centre| content.contains(*centre))
        })
        .expect("a control shows in the scrolled pane");
    let mut sink = damage();
    point_at(&mut shell, at, short, &theme, &mut sink);
    let stale = shell.form_for_test().expect("a form").groups().to_vec();

    shell.lay_out(tall, Scale::ONE, &theme);
    assert_eq!(shell.scroll_offset(), 0, "a pane that fits keeps no offset");
    let mut fresh = scrolled();
    point_at(&mut fresh, at, short, &theme, &mut sink);
    fresh.lay_out(tall, Scale::ONE, &theme);
    re_point(&mut fresh, at, tall, &theme);
    let fresh = fresh.form_for_test().expect("a form").groups().to_vec();
    assert_ne!(fresh, stale, "the premise: the lit control moves");
    assert_eq!(
        shell.form_for_test().expect("a form").groups(),
        fresh.as_slice(),
        "the hover stayed on the control the relayout moved away"
    );
}

/// The replay is a move and nothing else: it never takes the keyboard cursor,
/// and a press held across the wheel turn is released over another row, so
/// it activates neither.
#[test]
fn a_replayed_move_neither_takes_the_keyboard_cursor_nor_moves_a_held_press() {
    let theme = theme();
    let (mut shell, short) = short_strip();
    let search = shell
        .frame(short, Scale::ONE, &theme)
        .search
        .expect("a search field");
    click(&mut shell, search.center(), short, &theme);
    let at = row_point(&shell, 1, short, &theme).expect("row 1 shows");
    let mut sink = damage();
    point_at(&mut shell, at, short, &theme, &mut sink);
    detent(&mut shell, short, &theme, &mut sink);
    shell.on_key(
        keystroke(Key::Char('w')),
        short,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert_eq!(
        shell.search_text_for_test(),
        "w",
        "the replay took the cursor"
    );

    let (mut shell, short) = short_strip();
    let opened = shell.location();
    point_at(&mut shell, at, short, &theme, &mut sink);
    for event in [
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        shell.on_pointer(&event, short, Scale::ONE, &theme, &mut sink);
    }
    assert!(
        shell.strip_offset_for_test() > 0,
        "the wheel turned mid-press"
    );
    assert_eq!(
        shell.location(),
        opened,
        "a press begun on one row was released over another and chose it"
    );
}

// --- The screensaver pane ------------------------------------------------

/// Click `at` in `viewport`, answering what the release concluded.
fn captions(shell: &Shell) -> alloc::vec::Vec<&str> {
    shell
        .form_for_test()
        .expect("a form")
        .groups()
        .iter()
        .map(FieldGroup::caption)
        .collect()
}

fn row_labels(shell: &Shell, group: usize) -> alloc::vec::Vec<&str> {
    shell.form_for_test().expect("a form").groups()[group]
        .rows()
        .iter()
        .map(tairix_controls::FieldRow::label)
        .collect()
}

/// The Screensaver pane showing `settings`, laid out for [`WIDE`].
fn screensaver_showing(settings: DesktopSettings) -> Shell {
    let mut shell = Shell::new(settings).expect("a shell");
    let mut sink = damage();
    assert!(shell.go_to_pane("screensaver", WIDE, Scale::ONE, &theme(), &mut sink));
    shell.lay_out(WIDE, Scale::ONE, &theme());
    shell
}

/// Choosing a screensaver's picture posts it and brings that screensaver's own
/// group — its options, then the button that shows it — in place of the last
/// one's, laid out afresh and repainted.
#[test]
fn choosing_a_screensaver_brings_its_own_options_in_place_of_the_last() {
    let theme = theme();
    let mut shell = screensaver_showing(DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::Ribbon,
        ..DesktopSettings::default()
    });
    assert_eq!(
        captions(&shell),
        ["SCREENSAVER", "MINIMAL CLOCK", "ENERGY SAVING"]
    );
    assert_eq!(
        row_labels(&shell, 1),
        [SaverOption::RibbonDate.label(), "Preview"]
    );
    let life = shell
        .picture_rect(Chooser::Screensaver, 6, WIDE, Scale::ONE, &theme)
        .expect("the Game of Life shows");
    let mut drew = damage();
    let acted = clicked(&mut shell, life.center(), WIDE, &theme, &mut drew);
    let document = acted.document().expect("the choice posts a document");
    assert!(document.contains("screensaver.kind = life"), "{document}");
    assert!(
        !document.contains(SettingsKey::Wallpaper.name()),
        "{document}"
    );
    assert_eq!(
        captions(&shell),
        ["SCREENSAVER", "GAME OF LIFE", "ENERGY SAVING"]
    );
    assert_eq!(
        row_labels(&shell, 1),
        [
            SaverOption::LifeCells.label(),
            SaverOption::LifeSpeed.label(),
            "Preview"
        ]
    );
    let frame = shell.frame(WIDE, Scale::ONE, &theme);
    assert!(
        covers(&drew, frame.content),
        "the new rows were not repainted"
    );
    // Laid out exactly as a pane opened on the Game of Life is.
    let fresh = screensaver_showing(DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::Life,
        ..DesktopSettings::default()
    });
    assert_eq!(frame, fresh.frame(WIDE, Scale::ONE, &theme));
    assert_eq!(
        shell.row_rect_for_test((2, 0), WIDE, Scale::ONE, &theme),
        fresh.row_rect_for_test((2, 0), WIDE, Scale::ONE, &theme),
    );
    assert_eq!(
        shell.pane_height(600, Scale::ONE, &theme),
        fresh.pane_height(600, Scale::ONE, &theme)
    );
}

/// Test asks for the screensaver exactly as the pane shows it, and only for
/// that: it is not a setting, so it posts nothing.
#[test]
fn the_test_button_asks_for_the_screensaver_as_the_pane_shows_it() {
    let theme = theme();
    let mut settings = DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::Life,
        ..DesktopSettings::default()
    };
    settings.screensaver_options.life.speed = tairix_wallpaper::Pace::Fast;
    let mut shell = screensaver_showing(settings);
    // Tall enough to show the chooser and the Game of Life's group whole.
    let tall = Rect::new(0, 0, WIDE.width, 1000);
    shell.lay_out(tall, Scale::ONE, &theme);
    let test = shell
        .row_control_rect_for_test((1, 2), tall, Scale::ONE, &theme)
        .expect("the Test button shows");
    let mut sink = damage();
    let acted = clicked(&mut shell, test.center(), tall, &theme, &mut sink);
    let ShellOutcome::PreviewScreensaver(document) = &acted else {
        panic!("Test asks for a preview: {acted:?}");
    };
    assert!(acted.document().is_none(), "a preview posts nothing");
    assert!(document.contains("screensaver.kind = life"), "{document}");
    assert!(
        document.contains("screensaver.life.speed = fast"),
        "{document}"
    );
    assert!(
        !document.contains(SettingsKey::Wallpaper.name()),
        "{document}"
    );
    assert!(
        !document.contains(SettingsKey::LockAfter.name()),
        "{document}"
    );
}

/// A preview the desktop refused says so on the row that asked, until one is
/// shown.
#[test]
fn a_refused_preview_is_stated_on_its_row() {
    // Black has nothing to set, so its Preview row is its group's first.
    let mut shell = screensaver_showing(DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::Blank,
        ..DesktopSettings::default()
    });
    let description = |shell: &Shell| {
        shell.form_for_test().expect("a form").groups()[1].rows()[0]
            .description()
            .map(alloc::string::String::from)
    };
    let offered = description(&shell);
    shell.adopt_preview_answer(Err(tairix_abi::Errno::SeatBusy));
    let refused = description(&shell).expect("a description");
    assert!(
        refused.contains("would not show the screensaver"),
        "{refused}"
    );
    let row = &shell.form_for_test().expect("a form").groups()[1].rows()[0];
    assert_eq!(
        row.state().validation,
        tairix_controls::ValidationState::Invalid
    );
    shell.adopt_preview_answer(Ok(()));
    assert_eq!(description(&shell), offered);
}

/// The value `document` gives `key`, if it names it.
fn value_of(document: &str, key: SettingsKey) -> Option<&str> {
    document.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key.name()).then(|| value.trim())
    })
}

/// The ray tracer's group sets how much of the machine it traces on, idle
/// time first and chosen until told otherwise, whether its pictures are
/// kept, off until told otherwise, and how much its scenes set out, simply
/// until told otherwise, above the button that shows it.
#[test]
fn the_ray_tracer_offers_its_processor_use_its_pictures_its_detail_and_its_preview() {
    let mut shell = screensaver_showing(DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::Raytrace,
        ..DesktopSettings::default()
    });
    assert_eq!(
        captions(&shell),
        ["SCREENSAVER", "RAY TRACER", "ENERGY SAVING"]
    );
    assert_eq!(
        row_labels(&shell, 1),
        ["Processor use", "Save pictures", "Detail", "Preview"]
    );
    let form = shell.form_mut_for_test().expect("a form");
    let tairix_controls::FieldControl::Combo(combo) = form.groups()[1].rows()[0].control() else {
        panic!("processor use is a choice");
    };
    assert_eq!(combo.choices(), ["Idle time", "Performance"]);
    assert_eq!(combo.selected(), Some(0));
    let tairix_controls::FieldControl::Combo(save) = form.groups()[1].rows()[1].control() else {
        panic!("saving pictures is a choice");
    };
    assert_eq!(save.choices(), ["On", "Off"]);
    assert_eq!(save.selected(), Some(1), "pictures are not kept unasked");
    let tairix_controls::FieldControl::Combo(detail) = form.groups()[1].rows()[2].control() else {
        panic!("the detail is a choice");
    };
    assert_eq!(detail.choices(), ["Simple", "Maximum realism"]);
    assert_eq!(
        detail.selected(),
        Some(0),
        "scenes are set out simply unasked"
    );
}

/// The System Monitor names the busiest tasks until told not to, and the row
/// that says so warns that the names show on a locked screen.
#[test]
fn the_system_monitor_offers_to_hide_its_task_names() {
    let mut shell = screensaver_showing(DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::SystemMonitor,
        ..DesktopSettings::default()
    });
    assert_eq!(
        captions(&shell),
        ["SCREENSAVER", "SYSTEM MONITOR", "ENERGY SAVING"]
    );
    assert_eq!(row_labels(&shell, 1), ["Name the busiest tasks", "Preview"]);
    let form = shell.form_mut_for_test().expect("a form");
    let row = &form.groups()[1].rows()[0];
    assert!(
        row.description()
            .is_some_and(|text| text.contains("locked")),
        "{:?}",
        row.description()
    );
    let tairix_controls::FieldControl::Combo(combo) = row.control() else {
        panic!("naming the tasks is a choice");
    };
    assert_eq!(combo.choices(), ["On", "Off"]);
    assert_eq!(combo.selected(), Some(0));
}

/// Every option a screensaver offers posts its own key.
#[test]
fn every_screensaver_option_posts_its_own_key() {
    for (kind, keys) in [
        (
            tairix_wallpaper::ScreensaverKind::Slideshow,
            &[SettingsKey::SlideInterval, SettingsKey::SlideOrder][..],
        ),
        (
            tairix_wallpaper::ScreensaverKind::Clock,
            &[SettingsKey::ClockDate, SettingsKey::ClockIdentity],
        ),
        (
            tairix_wallpaper::ScreensaverKind::Ribbon,
            &[SettingsKey::RibbonDate],
        ),
        (
            tairix_wallpaper::ScreensaverKind::Starfield,
            &[SettingsKey::StarDensity, SettingsKey::StarWarp],
        ),
        (
            tairix_wallpaper::ScreensaverKind::Life,
            &[SettingsKey::LifeCells, SettingsKey::LifeSpeed],
        ),
        (
            tairix_wallpaper::ScreensaverKind::Raytrace,
            &[
                SettingsKey::RaytraceCpu,
                SettingsKey::RaytraceSave,
                SettingsKey::RaytraceDetail,
            ],
        ),
        (
            tairix_wallpaper::ScreensaverKind::RetroGames,
            &[SettingsKey::RetroGamesSpeed],
        ),
        (
            tairix_wallpaper::ScreensaverKind::SystemMonitor,
            &[SettingsKey::MonitorTasks],
        ),
    ] {
        let mut shell = screensaver_showing(DesktopSettings {
            screensaver: kind,
            ..DesktopSettings::default()
        });
        for (row, key) in keys.iter().enumerate() {
            let form = shell.form_mut_for_test().expect("a form");
            let tairix_controls::FieldControl::Combo(combo) =
                form.groups()[1].rows()[row].control()
            else {
                panic!("{kind:?} row {row} is a choice");
            };
            let other = (combo.selected().unwrap_or(0) + 1) % combo.choices().len();
            let before = DesktopSettings::default().document_of(&[*key]).render();
            let crate::FormOutcome::Apply(document) = form.choose_for_test(1, row, other) else {
                panic!("{kind:?} row {row} posts a document");
            };
            let (was, now) = (value_of(&before, *key), value_of(&document, *key));
            assert!(now.is_some() && now != was, "{key:?}: {was:?} -> {now:?}");
        }
    }
}

/// The kept-pictures sentence names the folders a home keeps them in.
#[test]
fn the_kept_pictures_option_names_where_pictures_go() {
    use tairix_abi::home::{
        HOME_USER_FILES_DIR, USER_FILES_DOCUMENTS_DIR, USER_FILES_PICTURES_DIR,
    };
    let text = SaverOption::RaytraceSave.description();
    assert!(text.contains(HOME_USER_FILES_DIR), "{text}");
    assert!(text.contains(USER_FILES_PICTURES_DIR), "{text}");
    assert!(!text.contains(USER_FILES_DOCUMENTS_DIR), "{text}");
}

/// The slideshow is narrowed to a category the catalog files a picture under,
/// or to one an update has since taken away, which is still offered.
#[test]
fn the_slideshow_offers_every_category_and_keeps_one_the_store_lost() {
    let mut settings = DesktopSettings {
        screensaver: tairix_wallpaper::ScreensaverKind::Slideshow,
        ..DesktopSettings::default()
    };
    settings.screensaver_options.slideshow.source =
        tairix_wallpaper::WallpaperCategory::new("Gone").map_or(
            tairix_wallpaper::SlideSource::Every,
            tairix_wallpaper::SlideSource::Category,
        );
    settings.screensaver_options.slideshow.interval = tairix_abi::time::Duration64::from_secs(90);
    let mut shell = screensaver_showing(settings);
    shell.adopt_catalog(
        ["Abstract", "Nature", "Nature"]
            .iter()
            .enumerate()
            .map(|(at, category)| tairix_wallpaper::CatalogItem {
                category: alloc::string::String::from(*category),
                file: alloc::format!("{at}.jpg"),
            })
            .collect(),
    );
    let form = shell.form_for_test().expect("a form");
    let combo = |row: usize| match form.groups()[1].rows()[row].control() {
        tairix_controls::FieldControl::Combo(combo) => combo,
        _ => panic!("row {row} is a choice"),
    };
    assert_eq!(
        combo(2).choices(),
        ["Every category", "Abstract", "Nature", "Gone"]
    );
    assert_eq!(combo(2).selected_text(), Some("Gone"));
    let intervals = combo(0).choices();
    assert_eq!(
        intervals.first().map(alloc::string::String::as_str),
        Some("5 seconds")
    );
    assert_eq!(
        intervals.last().map(alloc::string::String::as_str),
        Some("1 hour")
    );
    assert_eq!(combo(0).selected_text(), Some("1 minute 30 seconds"));
    assert!(intervals.iter().any(|choice| choice == "2 minutes"));
}

/// The pane asks for the pictures on screen first and a screen's height
/// beyond, and asking again once everything wanted has landed costs nothing
/// until the pane moves.
#[test]
fn the_wallpaper_pane_asks_for_what_shows_and_settles_until_it_moves() {
    let theme = theme();
    let mut shell = pictures(60, WIDE);
    let mut asked = alloc::vec::Vec::new();
    while let Some(wanted) =
        shell.next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (0, |_| false))
    {
        let mut drew = damage();
        let pixels = alloc::vec![0xFF; wanted.bytes()];
        shell.set_picture(wanted, &pixels, (WIDE, Scale::ONE, &theme), &mut drew);
        assert!(!drew.is_empty(), "a picture on screen landed unrepainted");
        let frame = shell.frame(WIDE, Scale::ONE, &theme);
        assert!(
            !covers(&drew, frame.content),
            "one landed picture repainted the pane"
        );
        asked.push(wanted.subject);
        assert!(asked.len() <= 60, "asked for more than exists");
    }
    assert!(!asked.is_empty(), "the pictures on screen are asked for");
    assert!(asked.len() < 60, "and nothing off screen: {}", asked.len());
    assert_eq!(
        asked[0],
        tairix_abi::window_ipc::PreviewSubject::Wallpaper(0)
    );
    assert_eq!(
        shell.next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (0, |_| false)),
        None
    );
    // Room to spare reaches past the screen's edge.
    let beyond = shell
        .next_picture_wanted(WIDE, (Scale::ONE, &theme), true, (0, |_| false))
        .expect("one within reach");
    assert!(!asked.contains(&beyond.subject));
    shell.mark_picture_refused(beyond.subject);
    let next = shell
        .next_picture_wanted(WIDE, (Scale::ONE, &theme), true, (0, |_| false))
        .expect("the next within reach");
    assert_ne!(next.subject, beyond.subject, "a refusal is not asked again");
}

/// Several renders run at once, so a picture already asked for is passed over
/// for the next. A round that found nothing settles against the pictures then
/// asked: asking again runs no round until they change, where a round passed
/// over and left unsettled re-ran the whole pane on every event.
#[test]
fn a_pane_settled_against_the_pictures_asked_is_asked_again_once_they_change() {
    let theme = theme();
    let mut shell = pictures(60, WIDE);
    let first = shell
        .next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (0, |_| false))
        .expect("a picture on screen");
    let second = shell
        .next_picture_wanted(
            WIDE,
            (Scale::ONE, &theme),
            false,
            (1, |subject| subject == first.subject),
        )
        .expect("the next picture on screen");
    assert_ne!(
        second.subject, first.subject,
        "an asked picture was asked again"
    );

    assert_eq!(
        shell.next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (2, |_| true)),
        None
    );
    let rounds = core::cell::Cell::new(0);
    let counted = |_| {
        rounds.set(rounds.get() + 1);
        false
    };
    assert_eq!(
        shell.next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (2, counted)),
        None,
        "nothing asked or answered since"
    );
    assert_eq!(rounds.get(), 0, "the settled question ran no round");
    assert_eq!(
        shell
            .next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (3, |_| false))
            .map(|wanted| wanted.subject),
        Some(first.subject),
        "an answer landing nothing asks again"
    );
}

/// Answer every picture the pane asks for with memory plentiful, recording
/// each subject asked for and refusing any asked for twice.
fn settle_pictures(
    shell: &mut Shell,
    theme: &Theme,
    asked: &mut alloc::collections::BTreeSet<tairix_abi::window_ipc::PreviewSubject>,
) {
    while let Some(wanted) =
        shell.next_picture_wanted(WIDE, (Scale::ONE, theme), true, (0, |_| false))
    {
        assert!(
            asked.insert(wanted.subject),
            "{:?} was asked for again",
            wanted.subject
        );
        let pixels = alloc::vec![0xFF; wanted.bytes()];
        shell.set_picture(wanted, &pixels, (WIDE, Scale::ONE, theme), &mut damage());
    }
}

/// Scrolling the Wallpaper pane to its far end and back asks the desktop for
/// no picture twice while memory is plentiful: a thumbnail handed over is kept
/// for the life of the pane, however far off screen it is scrolled.
#[test]
fn scrolling_the_wallpaper_pane_away_and_back_asks_for_no_picture_twice() {
    let theme = theme();
    let mut shell = pictures(60, WIDE);
    let mut asked = alloc::collections::BTreeSet::new();
    settle_pictures(&mut shell, &theme, &mut asked);
    let over = shell.frame(WIDE, Scale::ONE, &theme).content.center();
    point_at(&mut shell, over, WIDE, &theme, &mut damage());
    for dy in [SCROLL_UNITS_PER_DETENT, -SCROLL_UNITS_PER_DETENT] {
        loop {
            let before = shell.scroll_offset();
            shell.on_pointer(
                &InputEvent::PointerScrolled { dx: 0, dy },
                WIDE,
                Scale::ONE,
                &theme,
                &mut damage(),
            );
            settle_pictures(&mut shell, &theme, &mut asked);
            if shell.scroll_offset() == before {
                break;
            }
        }
    }
    assert_eq!(
        shell.scroll_offset(),
        0,
        "the pane scrolled back to its top"
    );
    assert_eq!(asked.len(), 60, "every picture was asked for once");
    assert_eq!(pictures_held(&shell), 60, "and every one is still held");
}

/// How many of the wallpaper chooser's pictures hold a rendered picture.
fn pictures_held(shell: &Shell) -> usize {
    let choice = chooser(shell);
    (0..choice.len())
        .filter(|index| choice.item(*index).is_some_and(|item| item.art().is_some()))
        .count()
}

/// A picture sets the picture it shows, even after a choice has left the one
/// in effect — a picture the catalog lacks, which still stands in the list —
/// behind.
#[test]
fn a_second_choice_after_leaving_a_picture_the_catalog_lacks_sets_the_one_shown() {
    let gone = tairix_wallpaper::WallpaperPath::new(&tairix_wallpaper::wallpaper_path(
        "Nature", "gone.jpg",
    ))
    .expect("a path");
    let mut shell = Shell::new(DesktopSettings {
        wallpaper: tairix_wallpaper::WallpaperChoice::Image(gone),
        ..DesktopSettings::default()
    })
    .expect("a shell");
    let mut sink = damage();
    assert!(shell.go_to_pane("wallpaper", WIDE, Scale::ONE, &theme(), &mut sink));
    shell.adopt_catalog(
        [
            ("Abstract", "a.jpg"),
            ("Nature", "n0.jpg"),
            ("Space", "s.jpg"),
        ]
        .iter()
        .map(|(category, file)| tairix_wallpaper::CatalogItem {
            category: alloc::string::String::from(*category),
            file: alloc::string::String::from(*file),
        })
        .collect(),
    );
    // No picture, a.jpg, n0.jpg, gone.jpg, s.jpg.
    let form = shell.form_mut_for_test().expect("a form");
    let crate::FormOutcome::Apply(first) = form.choose_for_test(1, 0, 1) else {
        panic!("a choice posts a document");
    };
    assert!(first.contains("Abstract/a.jpg"), "{first}");
    // The desktop adopts it, which is what the pane already shows.
    let adopted = shell.form_for_test().expect("a form").settings().clone();
    shell.adopt_settings(adopted);
    let form = shell.form_mut_for_test().expect("a form");
    let crate::FormOutcome::Apply(second) = form.choose_for_test(1, 0, 4) else {
        panic!("a choice posts a document");
    };
    assert!(second.contains("Space/s.jpg"), "{second}");
}

/// Choosing a backdrop colour repaints *No picture* in it at once, and
/// reports that tile.
#[test]
fn choosing_a_backdrop_colour_repaints_no_picture_in_it() {
    let theme = theme();
    let mut shell = pictures(6, WIDE);
    let field = shell
        .row_control_rect_for_test((0, 1), WIDE, Scale::ONE, &theme)
        .expect("the backdrop row shows");
    click(&mut shell, field.center(), WIDE, &theme);
    let black = shell
        .choice_rect(1, WIDE, Scale::ONE, &theme)
        .expect("the backdrop list opened");
    let mut drew = damage();
    let acted = clicked(&mut shell, black.center(), WIDE, &theme, &mut drew);
    let document = acted.document().expect("the choice posts a document");
    assert!(document.contains("backdrop = 000000"), "{document}");
    let swatch = tairix_controls::PictureItem::swatch(
        crate::NONE_LABEL,
        tairix_controls::Swatch::Fixed(tairix_colour::Rgba::rgb(0, 0, 0)),
    );
    assert_eq!(chooser(&shell).item(0), Some(&swatch));
    let tile = shell
        .picture_rect(Chooser::Wallpaper, 0, WIDE, Scale::ONE, &theme)
        .expect("no picture shows");
    assert!(covers(&drew, tile), "the swatch changed unrepainted");
}

/// Memory growing short lets go at once of every picture off screen, render
/// outstanding or not.
#[test]
fn memory_growing_short_lets_go_of_the_pictures_off_screen() {
    let theme = theme();
    let mut shell = pictures(60, WIDE);
    while let Some(wanted) =
        shell.next_picture_wanted(WIDE, (Scale::ONE, &theme), true, (0, |_| false))
    {
        let pixels = alloc::vec![0xFF; wanted.bytes()];
        shell.set_picture(wanted, &pixels, (WIDE, Scale::ONE, &theme), &mut damage());
    }
    let roomy = pictures_held(&shell);
    shell.trim_pictures(WIDE, (Scale::ONE, &theme), false);
    let short = pictures_held(&shell);
    assert!(short > 0, "what is on screen is kept");
    assert!(short < roomy, "{roomy} held, then {short}");
    assert_eq!(
        shell.next_picture_wanted(WIDE, (Scale::ONE, &theme), false, (0, |_| false)),
        None,
        "and nothing is asked for again while it stays short"
    );
}

/// What an answer redraws is the pane's column down to the window's foot,
/// its action band included, and none of the strip, search field or trail.
#[test]
fn an_answer_redraws_the_pane_and_nothing_of_the_chrome() {
    let theme = theme();
    let shell = crate::test_support::showing("lock-screen");
    let frame = shell.frame(WIDE, Scale::ONE, &theme);
    let region = shell.pane_region(WIDE, Scale::ONE, &theme);
    for part in [Some(frame.content), frame.footer, frame.scrollbar]
        .into_iter()
        .flatten()
    {
        assert_eq!(region.intersection(&part), part, "{part:?}");
    }
    for chrome in [frame.sidebar, frame.search, Some(frame.breadcrumb)]
        .into_iter()
        .flatten()
    {
        assert!(region.intersection(&chrome).is_empty(), "{chrome:?}");
    }
}

/// Opening a choice list reports the whole plate it opens into, so the frame
/// presented shows every choice rather than only what overlaps its field.
#[test]
fn opening_a_list_reports_every_choice_it_draws() {
    let theme = theme();
    let mut shell = shell_at(Location {
        category: Category::Appearance,
        pane: Pane::Appearance,
    });
    let combo = shell
        .setting_rect(Setting::Appearance, WIDE, Scale::ONE, &theme)
        .expect("the pane draws the appearance row");
    let centre = Point::new(
        combo.left() + to_i32(combo.width / 2),
        combo.top() + to_i32(combo.height / 2),
    );
    let click = [
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ];
    // Opened and closed once, so the pointer rests on the field and the
    // field holds the keyboard, as when a reader opens it again.
    let mut settling = damage();
    let _ = shell.on_pointer(
        &InputEvent::PointerMoved { to: centre },
        WIDE,
        Scale::ONE,
        &theme,
        &mut settling,
    );
    for event in &click {
        let _ = shell.on_pointer(event, WIDE, Scale::ONE, &theme, &mut settling);
    }
    let _ = press(
        &mut shell,
        Key::Named(NamedKey::Escape),
        &theme,
        &mut settling,
    );
    assert!(shell.choice_rect(0, WIDE, Scale::ONE, &theme).is_none());
    let mut sink = damage();
    for event in &click {
        let _ = shell.on_pointer(event, WIDE, Scale::ONE, &theme, &mut sink);
    }
    for index in 0..Appearance::ALL.len() {
        let choice = shell
            .choice_rect(index, WIDE, Scale::ONE, &theme)
            .expect("the press opened the list");
        assert!(
            covers(&sink, choice),
            "choice {index} at {choice:?} was drawn but not reported"
        );
    }
}
