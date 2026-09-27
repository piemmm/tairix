//! Unit tests for the settings shell.
//!
//! These cover what the shell exists to get right: the frame's regions and
//! what a narrow window sheds, the strip's shape and the cursor reaching
//! every row of it, the search filtering the strip, the trail rewriting
//! itself, the category list a shed strip becomes, and the scroll a pane too
//! tall for its column gets.

use tairix_abi::blkio::BlkDeviceClass;
use tairix_abi::desktop::{Appearance, Contrast, Density};
use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
use tairix_abi::sysinfo::{MountAvailability, MountRecord, MountVolumeState};
use tairix_abi::window_ipc::SCROLL_UNITS_PER_DETENT;
use tairix_controls::WHEEL_STEP;
use tairix_font::install_test_transport;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::{CursorSetId, Theme};
use tairix_wallpaper::{DesktopSettings, SettingsKey};

use crate::form::{Composition, FormPlace, Setting};
use crate::frame::{resolve_frame, Actions, Overflow, CONTENT_FLOOR, SIDEBAR_WIDTH};
use crate::registry::{Category, Location, Pane, PaneContent, StripRow, CATEGORIES};
use crate::shell::{Shell, ShellOutcome};
use crate::test_support::{damage, theme, WIDE};
use crate::volumes::VolumeReading;

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

/// Press and release the primary button at `at`.
fn click(shell: &mut Shell, at: Point, viewport: Rect, theme: &Theme) {
    let mut sink = damage();
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        shell.on_pointer(&event, viewport, Scale::ONE, theme, &mut sink);
    }
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

#[test]
fn a_wide_window_seats_every_region() {
    let theme = theme();
    let frame = resolve_frame(WIDE, Scale::ONE, &theme, Overflow::default(), Actions::None);
    let search = frame.search.expect("a search field");
    let sidebar = frame.sidebar.expect("a strip");
    assert_eq!(search.width, sidebar.width);
    assert_eq!(search.left(), sidebar.left());
    // The band sits above the body, and the two columns do not overlap.
    assert_eq!(search.bottom(), sidebar.top());
    assert_eq!(frame.breadcrumb.bottom(), frame.content.top());
    assert!(frame.content.left() >= sidebar.right());
    assert_eq!(frame.breadcrumb.left(), frame.content.left());
    assert!(frame.scrollbar.is_none(), "nothing to scroll");
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
    let sidebar = frame.sidebar.expect("a strip");
    assert_eq!(sidebar.width, scale.scale_length(SIDEBAR_WIDTH).max(1));
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
            Key::Named(NamedKey::Down),
            Modifiers::default(),
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    // Walking down and choosing each row in turn reaches every location the
    // strip offers, which is what "the cursor reaches every row" means.
    for index in 0..shell.rows().len() {
        let Some(at) = row_point(&shell, index, WIDE, &theme) else {
            continue;
        };
        let expected = shell.rows()[index].location().expect("a location");
        click(&mut shell, at, WIDE, &theme);
        assert_eq!(shell.location(), expected, "row {index}");
    }
}

#[test]
fn choosing_a_disclosing_category_opens_its_first_pane_and_discloses_it() {
    let theme = theme();
    let mut shell = shell();
    let mut sink = damage();
    let networking = shell
        .rows()
        .iter()
        .position(|row| matches!(row, StripRow::Category(Category::Networking)))
        .expect("the networking row");
    let at = row_point(&shell, networking, WIDE, &theme).expect("a row");
    click(&mut shell, at, WIDE, &theme);
    assert_eq!(shell.location().category, Category::Networking);
    assert_eq!(shell.location().pane, Pane::Ethernet);
    assert!(
        shell
            .rows()
            .iter()
            .any(|row| matches!(row, StripRow::Pane(Category::Networking, _))),
        "its panes are disclosed"
    );
    // General's panes are no longer disclosed: one category opens at a time.
    assert!(!shell
        .rows()
        .iter()
        .any(|row| matches!(row, StripRow::Pane(Category::General, _))));
    let _ = &mut sink;
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
            Key::Char(ch),
            Modifiers::default(),
            WIDE,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    assert_eq!(shell.rows(), &[StripRow::Category(Category::Sound)]);

    // Escape clears the query, and the strip is whole again.
    shell.on_key(
        Key::Named(NamedKey::Escape),
        Modifiers::default(),
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
        Key::Named(NamedKey::Escape),
        Modifiers::default(),
        NARROW,
        Scale::ONE,
        &theme,
        &mut sink,
    );
    assert!(!shell.category_list_open(), "escape dismisses it");
}

// --- Painting -----------------------------------------------------------

#[test]
fn the_shell_draws_in_both_themes_and_at_both_densities() {
    install_test_transport();
    for theme in [Theme::dark(), Theme::light()] {
        for scale in [Scale::ONE, Scale::from_percent(200).expect("a valid scale")] {
            for viewport in [WIDE, NARROW] {
                let mut surface = Surface::new(viewport.width, viewport.height).expect("a surface");
                shell().render(&mut surface, viewport, scale, &theme, &mut NoArtwork);
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
        &theme,
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
        Key::Named(NamedKey::End),
        Modifiers::default(),
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
    shell.on_key(key, Modifiers::default(), WIDE, Scale::ONE, theme, sink)
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
        (Pane::Accessibility, 3),
        (Pane::Wallpaper, 1),
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
        if !matches!(
            pane.content(),
            Some(PaneContent::Form(_) | PaneContent::Pictures(_))
        ) {
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
/// reimpose a pointer value, nor the Mouse pane a repeat.
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
    let tairix_controls::FieldControl::Combo(combo) = row.control() else {
        panic!("the interval row is a choice");
    };
    assert_eq!(combo.selected_text(), Some("450 ms"));
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
    assert!(document.contains("appearance = light"), "{document}");
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

    let light = Appearance::ALL
        .iter()
        .position(|appearance| *appearance == Appearance::Light)
        .expect("light is offered");
    let choice = shell
        .choice_rect(light, WIDE, Scale::ONE, &theme)
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
    assert!(document.contains("appearance = light"), "{document}");
    assert!(
        shell.choice_rect(light, WIDE, Scale::ONE, &theme).is_none(),
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
        appearance: Appearance::Light,
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
        Some(Appearance::Light)
    );

    shell.adopt_settings(DesktopSettings::default());
    assert_eq!(
        shell.form_for_test().map(|form| form.settings().appearance),
        Some(Appearance::Dark)
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

/// Whether `drew` covers every pixel of `rect`.
fn covers(drew: &Region, rect: Rect) -> bool {
    let mut uncovered = Region::new();
    uncovered.add(rect);
    for covered in drew.rects() {
        uncovered.subtract(*covered);
    }
    uncovered.is_empty()
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
            Key::Named(NamedKey::Down),
            Modifiers::default(),
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    let (group, _) = shell
        .form_group_cursor_for_test()
        .expect("the cursor is on a row");
    assert_eq!(group, 2, "the cursor did not reach the last group");
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
            Key::Named(NamedKey::Down),
            Modifiers::default(),
            short,
            Scale::ONE,
            &theme,
            &mut sink,
        );
    }
    assert!(shell.scroll_offset() > 0, "the walk down did not scroll");

    for _ in 0..12 {
        shell.on_key(
            Key::Named(NamedKey::Up),
            Modifiers::default(),
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
    shell.render(&mut surface, short, Scale::ONE, &theme, &mut NoArtwork);
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
        Key::Named(NamedKey::Down),
        Modifiers::default(),
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
            shell.render(&mut surface, WIDE, scale, &theme, &mut NoArtwork);
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
        shell.on_key(
            Key::Char(ch),
            Modifiers::default(),
            WIDE,
            Scale::ONE,
            &theme,
            sink,
        );
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
        Key::Named(NamedKey::End),
        Modifiers::default(),
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
        Key::Named(NamedKey::Down),
        Modifiers::default(),
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
    shell.render(&mut surface, short, Scale::ONE, &theme, &mut NoArtwork);
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

/// The pinboard group sits fixed above the gallery, so its last row's list
/// hangs over the pictures: it must be drawn above them, not beneath.
#[test]
fn an_open_choice_list_is_drawn_above_the_gallery_it_hangs_over() {
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
    let groups = shell.form_for_test().expect("a form").groups().len();
    let last_group = groups - 1;
    let last_row = shell.form_for_test().expect("a form").groups()[last_group]
        .rows()
        .len()
        - 1;
    let field = shell
        .row_control_rect_for_test((last_group, last_row), WIDE, Scale::ONE, &theme)
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
    shell.render(&mut surface, WIDE, Scale::ONE, theme, &mut NoArtwork);
    surface
}

// --- The gallery is reachable from the keyboard ------------------------

/// Every control on a pane is reachable without a pointer, the pictures
/// included: Down past the form's last row steps into the gallery, the arrows
/// walk it, Enter chooses, and Up from its first line steps back out.
#[test]
fn the_keyboard_walks_from_the_form_into_the_gallery_and_back() {
    let theme = theme();
    let mut shell = crate::test_support::showing("wallpaper");
    shell.adopt_catalog(
        (0..6)
            .map(|at| tairix_wallpaper::CatalogItem {
                category: alloc::string::String::from("TAIRiX"),
                file: alloc::format!("{at}.png"),
            })
            .collect(),
    );
    shell.lay_out(WIDE, Scale::ONE, &theme);
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
    let gallery = shell.gallery_for_test().expect("a gallery");
    assert!(
        gallery.is_focused(),
        "Down past the last row reached the gallery"
    );
    assert_eq!(
        shell.form_group_cursor_for_test(),
        None,
        "and took the ring off the form"
    );

    press(&mut shell, Key::Named(NamedKey::Right), &theme, &mut sink);
    let chosen = shell.gallery_for_test().expect("a gallery").cursor();
    let acted = press(&mut shell, Key::Named(NamedKey::Enter), &theme, &mut sink);
    assert!(
        acted.document().is_some(),
        "Enter chose and posted the picture"
    );
    assert_eq!(
        shell.gallery_for_test().expect("a gallery").selected(),
        chosen
    );

    press(&mut shell, Key::Named(NamedKey::Home), &theme, &mut sink);
    press(&mut shell, Key::Named(NamedKey::Up), &theme, &mut sink);
    assert!(!shell.gallery_for_test().expect("a gallery").is_focused());
    let groups = shell.form_for_test().expect("a form").groups();
    let last = (
        groups.len() - 1,
        groups.last().map_or(0, |group| group.rows().len() - 1),
    );
    assert_eq!(
        shell.form_group_cursor_for_test(),
        Some(last),
        "Up from the first line lands on the row just above"
    );
}

#[test]
fn walking_the_gallery_scrolls_the_tile_the_cursor_lands_on_into_view() {
    let theme = theme();
    let short = Rect::new(0, 0, 900, 420);
    let mut shell = shell();
    let mut sink = damage();
    assert!(shell.go_to_pane("wallpaper", short, Scale::ONE, &theme, &mut sink));
    shell.adopt_catalog(
        (0..60)
            .map(|at| tairix_wallpaper::CatalogItem {
                category: alloc::string::String::from("TAIRiX"),
                file: alloc::format!("{at}.png"),
            })
            .collect(),
    );
    shell.lay_out(short, Scale::ONE, &theme);
    let frame = shell.frame(short, Scale::ONE, &theme);
    let bar = frame.scrollbar.expect("sixty pictures overflow the band");
    shell.focus_content_for_test(short, Scale::ONE, &theme);
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
    // The picture in effect is not in this catalog, so it is the last
    // candidate, and entering the gallery already scrolled to it.
    assert!(shell.scroll_offset() > 0, "the band followed the cursor in");
    for (key, at_end) in [(NamedKey::Home, false), (NamedKey::End, true)] {
        let mut drew = damage();
        press_in(&mut shell, Key::Named(key), short, &theme, &mut drew);
        assert_eq!(
            shell.scroll_offset() > 0,
            at_end,
            "the band followed {key:?}"
        );
        assert!(covers(&drew, bar), "{key:?} moved the thumb unrepainted");
    }
}

/// Press `key` in a window of `viewport`.
fn press_in(shell: &mut Shell, key: Key, viewport: Rect, theme: &Theme, sink: &mut Region) {
    shell.on_key(key, Modifiers::default(), viewport, Scale::ONE, theme, sink);
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

/// The form row the window draws under `at`, as its group and row.
fn form_row_under(
    shell: &Shell,
    at: Point,
    viewport: Rect,
    theme: &Theme,
) -> Option<(usize, usize)> {
    let form = shell.form_for_test()?;
    form.groups().iter().enumerate().find_map(|(group, plate)| {
        (0..plate.rows().len()).find_map(|row| {
            shell
                .row_rect_for_test((group, row), viewport, Scale::ONE, theme)
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

/// The same for a form's rows under the pane's own wheel.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_panes_hover_to_the_row_now_under_it() {
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
    let at = shell
        .row_rect_for_test((0, 0), short, Scale::ONE, &theme)
        .expect("the first row shows")
        .center();
    let mut sink = damage();
    point_at(&mut shell, at, short, &theme, &mut sink);
    let stale = shell.form_for_test().expect("a form").groups().to_vec();

    let mut drew = damage();
    detent(&mut shell, short, &theme, &mut drew);
    assert_eq!(shell.scroll_offset(), u64::from(WHEEL_STEP));
    let now_under = form_row_under(&shell, at, short, &theme);
    assert!(
        now_under.is_some_and(|row| row != (0, 0)),
        "a detent brings another row under the pointer: {now_under:?}"
    );
    let mut fresh = accessibility();
    point_at(&mut fresh, at, short, &theme, &mut sink);
    detent(&mut fresh, short, &theme, &mut sink);
    re_point(&mut fresh, at, short, &theme);
    let fresh = fresh.form_for_test().expect("a form").groups().to_vec();
    assert_ne!(fresh, stale, "the premise: the lit row moves");
    assert_eq!(
        shell.form_for_test().expect("a form").groups(),
        fresh.as_slice(),
        "the hover stayed on the row the wheel carried away"
    );
    assert!(covers(&drew, frame.content), "the rows slid unrepainted");
    assert!(covers(&drew, bar), "the thumb moved unrepainted");
}

/// A wallpaper pane with `count` pictures, laid out for `viewport`.
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

/// The same for the gallery's tiles, which scroll beneath a form that stays
/// put.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_gallerys_hover_to_the_tile_now_under_it() {
    let theme = theme();
    let window = Rect::new(0, 0, 900, 640);
    let mut shell = pictures(60, window);
    let frame = shell.frame(window, Scale::ONE, &theme);
    let bar = frame.scrollbar.expect("sixty pictures overflow the band");
    let first = shell
        .tile_rect_for_test(0, window, Scale::ONE, &theme)
        .expect("the first tile shows");
    let below = (1..60)
        .find(|&tile| {
            shell
                .tile_rect_for_test(tile, window, Scale::ONE, &theme)
                .is_some_and(|rect| rect.top() > first.bottom())
        })
        .expect("a second line of tiles shows");
    let target = shell
        .tile_rect_for_test(below, window, Scale::ONE, &theme)
        .expect("the tile shows")
        .center();
    // The point a detent brings the second line's first tile under.
    let at = Point::new(target.x, target.y - to_i32(WHEEL_STEP));
    let mut sink = damage();
    point_at(&mut shell, at, window, &theme, &mut sink);
    let stale = shell.gallery_for_test().expect("a gallery").hovered();

    let mut drew = damage();
    detent(&mut shell, window, &theme, &mut drew);
    assert_eq!(shell.scroll_offset(), u64::from(WHEEL_STEP));
    assert_ne!(stale, Some(below), "the premise: the lit tile moves");
    assert_eq!(
        shell.gallery_for_test().expect("a gallery").hovered(),
        Some(below),
        "the hover stayed on the tile the wheel carried away"
    );
    let band = shell
        .tile_rect_for_test(below, window, Scale::ONE, &theme)
        .expect("the tile shows");
    assert!(covers(&drew, band), "the tile now lit was not repainted");
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
fn a_relayout_that_clamps_the_offset_moves_the_hover_to_the_row_now_under_it() {
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
    let at = shell.frame(short, Scale::ONE, &theme).content.center();
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
    assert_ne!(fresh, stale, "the premise: the lit row moves");
    assert_eq!(
        shell.form_for_test().expect("a form").groups(),
        fresh.as_slice(),
        "the hover stayed on the row the relayout moved away"
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
        Key::Char('w'),
        Modifiers::default(),
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
