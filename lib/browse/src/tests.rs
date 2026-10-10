//! Headless unit tests for the filesystem browser.
//!
//! Every test drives the [`Browser`] against an in-memory [`MockFs`] tree, so
//! the navigation and rendering logic is exercised without a kernel or a real
//! VFS.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::BlkDeviceClass;
use tairix_abi::Errno;
use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_icon::{IconArtwork, IconKind, IconPicture, IconRequest, NoArtwork};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::browser::Browser;
use crate::clipboard::{plan_paste, Clipboard, ClipboardOp, PasteError};
use crate::delete::{DeleteAction, DeleteError, DeletePlan, DeleteWalk, MAX_DELETE_DEPTH};
use crate::entry::Entry;
use crate::error::BrowseError;
use crate::execute::{
    paste_strategy, CopyAction, CopyCursor, CopyError, CopyKind, CopyWalk, CopyWalkError,
    PasteStrategy, VolumeId, COPY_CHUNK_LEN, MAX_COPY_DEPTH,
};
use crate::media::MediaType;
use crate::places::{PlaceKind, Places, Volume};
use crate::select::Selection;
use crate::source::{DirectorySource, Listing, Probe};

/// The chrome these tests measure against unless they say otherwise: the
/// command band shown, so the listing has a header to be offset by. The
/// hidden band has its own tests.
const BAND: crate::ToolbarBand = crate::ToolbarBand::Shown;

/// The entry the focus rests on, chosen or not.
pub(crate) fn focused<S: DirectorySource>(browser: &Browser<S>) -> Option<&Entry> {
    browser.focus_index().map(|index| &browser.entries()[index])
}

/// Paint `browser` into a freshly allocated `viewport`-sized surface.
///
/// The renderer paints into a surface its caller owns and keeps for the life
/// of its window; a test that only wants the pixels of one frame allocates one
/// here rather than each repeating the two lines.
fn paint<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    chrome: &crate::ManagerChrome<'_>,
    artwork: &mut dyn IconArtwork,
) -> Surface {
    let mut surface = Surface::new(viewport.width, viewport.height).expect("surface");
    crate::render_into(
        &mut surface,
        browser,
        scale,
        theme,
        viewport,
        chrome,
        artwork,
    );
    surface
}

/// The absolute-path key the mock indexes a directory by — the one shared
/// spelling, so tests, the model, and the VFS engine agree on the path
/// string.
fn key(components: &[String]) -> String {
    crate::vfs::spell_absolute_path(components)
}

// --- The places / devices rail -----------------------------------------

/// A home directory's path components, as the app reads them from the user's
/// own identity.
fn home() -> Vec<String> {
    vec!["Users".to_string(), "ann".to_string()]
}

/// One offered volume.
fn volume(label: &str, target: &str, medium: Option<BlkDeviceClass>) -> Volume {
    Volume {
        label: label.to_string(),
        target: target.to_string(),
        medium,
    }
}

/// Every row's label, in rail order.
fn place_labels(places: &Places) -> Vec<String> {
    places
        .rows()
        .iter()
        .map(|row| row.label().to_string())
        .collect()
}

#[test]
fn the_rail_lists_the_users_places_then_the_volumes_in_one_order() {
    let places = Places::new(
        &home(),
        &[
            volume(
                "Scratch",
                "/Storage/Scratch",
                Some(BlkDeviceClass::SolidState),
            ),
            volume(
                "Backup",
                "/Storage/Backup",
                Some(BlkDeviceClass::Rotational),
            ),
        ],
    );
    // The user's own places first, in their fixed order, then the volumes
    // sorted by label whatever order the mount table paged them out in.
    assert_eq!(
        place_labels(&places),
        [
            "Home",
            "Desktop",
            "UserFiles",
            "Apps",
            "System",
            "Backup",
            "Scratch"
        ]
    );
    assert_eq!(places.volume_start(), Some(5));
    assert_eq!(places.rows()[0].kind(), PlaceKind::Home);
    assert_eq!(places.rows()[1].kind(), PlaceKind::UserFolder);
    assert_eq!(places.rows()[3].kind(), PlaceKind::SystemRoot);
    assert_eq!(places.rows()[5].kind(), PlaceKind::Volume);
    // The fixed places navigate where their names say, whether or not those
    // directories exist — the model performs no I/O and never checks.
    assert_eq!(places.rows()[0].components(), home().as_slice());
    assert_eq!(
        places.rows()[2].components(),
        ["Users", "ann", "UserFiles"].map(String::from)
    );
    assert_eq!(places.rows()[3].components(), ["Apps"].map(String::from));
    // With no volumes there is nothing to separate.
    assert_eq!(Places::new(&home(), &[]).volume_start(), None);
    // Without a home there is nothing for the three home rows to hang off, so
    // only the machine-wide roots remain — never a row navigating nowhere.
    assert_eq!(place_labels(&Places::new(&[], &[])), ["Apps", "System"]);
}

/// A bare open lands among the user's own files first and their home next;
/// the root view is the caller's last resort.
#[test]
fn a_bare_open_tries_the_user_files_then_the_home() {
    let [first, second] = crate::bare_open_places(&home());
    assert_eq!(first, ["Users", "ann", "UserFiles"].map(String::from));
    assert_eq!(second, home());
}

#[test]
fn every_storage_medium_draws_its_own_drive_icon() {
    let places = Places::new(
        &[],
        &[
            volume("Disk", "/Storage/Disk", Some(BlkDeviceClass::Rotational)),
            volume("Fast", "/Storage/Fast", Some(BlkDeviceClass::SolidState)),
            volume("Stick", "/Storage/Stick", Some(BlkDeviceClass::Removable)),
            volume("Guest", "/Storage/Guest", Some(BlkDeviceClass::Virtual)),
            volume("Plain", "/Storage/Plain", None),
        ],
    );
    let icons: Vec<(String, IconKind)> = places
        .rows()
        .iter()
        .filter(|row| row.kind() == PlaceKind::Volume)
        .map(|row| (row.label().to_string(), row.icon()))
        .collect();
    // Sorted by label: Disk, Fast, Guest, Plain, Stick. A paravirtual device
    // and an unreported medium both draw the generic drive — never a guess at
    // hardware that was not reported.
    assert_eq!(
        icons,
        [
            ("Disk".to_string(), IconKind::DiskHard),
            ("Fast".to_string(), IconKind::DiskSolidState),
            ("Guest".to_string(), IconKind::Disk),
            ("Plain".to_string(), IconKind::Disk),
            ("Stick".to_string(), IconKind::DiskUsb),
        ]
    );
}

#[test]
fn a_malformed_or_duplicate_volume_is_dropped_never_guessed_at() {
    let over_long = "v".repeat(crate::MAX_PLACE_LABEL + 1);
    let places = Places::new(
        &home(),
        &[
            volume("Good", "/Storage/Good", None),
            // No label to show.
            volume("", "/Storage/Nameless", None),
            // A label longer than a row will ever accept.
            volume(&over_long, "/Storage/Long", None),
            // A label carrying a control character.
            volume("Ba\nd", "/Storage/Control", None),
            // A target that is not an absolute path.
            volume("Relative", "Storage/Relative", None),
            // A second row for a target an accepted row already covers.
            volume("Twin", "/Storage/Good", None),
            // A volume landing on a fixed place's own target.
            volume("Shadow", "/Apps", None),
        ],
    );
    assert_eq!(
        place_labels(&places),
        ["Home", "Desktop", "UserFiles", "Apps", "System", "Good"]
    );
    // The duplicate never displaced the fixed row it collided with.
    assert_eq!(places.rows()[3].kind(), PlaceKind::SystemRoot);
}

#[test]
fn the_rail_hit_test_inverts_the_layout_exactly_at_the_row_boundaries() {
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 400);
    let places = Places::new(&home(), &[volume("Backup", "/Storage/Backup", None)]);
    let view =
        crate::render::sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    assert_eq!(view.bar_rect(), None, "every row fits, so no bar");

    for index in 0..places.len() {
        let rect = view.shown_row_rect(index).expect("a shown row");
        let (left, top) = (rect.origin.x, rect.origin.y);
        let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
        // Both corners of the row resolve to it, and the pixel above its top
        // belongs to whatever is above — never to this row.
        assert_eq!(view.index_at(Point::new(left, top)), Some(index));
        assert_eq!(view.index_at(Point::new(right, bottom)), Some(index));
        assert_ne!(view.index_at(Point::new(left, top - 1)), Some(index));
    }
    // The separation between the user's places and the volumes is not a row.
    let band = view.separator_rect().expect("separator");
    assert_eq!(view.index_at(band.origin), None);
    // Nothing outside the rail resolves: past its right edge, or below the
    // last row.
    let width = i32::try_from(view.width()).expect("rail width");
    assert_eq!(view.index_at(Point::new(width, band.origin.y)), None);
    let last = view.shown_row_rect(places.len() - 1).expect("last row");
    assert_eq!(view.index_at(Point::new(0, last.bottom())), None);
}

/// A rail longer than the window scrolls: every row is laid out whole, the
/// rail carries a bar carved from its own trailing edge, and a row past the
/// window's end is reached by scrolling rather than dropped.
#[test]
fn a_rail_longer_than_its_window_scrolls_every_row_into_reach() {
    use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let volumes: Vec<Volume> = (0..24)
        .map(|at| {
            volume(
                &alloc::format!("Disk {at:02}"),
                &alloc::format!("/Storage/d{at:02}"),
                None,
            )
        })
        .collect();
    let mut places = Places::new(&home(), &volumes);
    let window = Rect::new(0, 0, 400, 300);
    let rail = |places: &Places| {
        crate::render::sidebar_view(window, Scale::ONE, &theme, Some(places), BAND).expect("rail")
    };
    let view = rail(&places);
    let bar = view.bar_rect().expect("a rail this long carries a bar");
    assert_eq!(
        bar.right(),
        view.rail_rect().right(),
        "carved from the rail"
    );
    assert_eq!(view.rows_area().right(), bar.left(), "beside the rows");
    assert!(view.content_height() > u64::from(view.rail_rect().height));
    let last = places.len() - 1;
    assert!(view.row_rect(last).is_some(), "the last row is laid out");
    assert_eq!(view.shown_row_rect(last), None, "past the window's end");

    let mut damage = tairix_controls::damage::sink();
    let wheel = crate::render::sidebar_scroll_wheel(
        &mut places,
        (window, Scale::ONE, &theme),
        BAND,
        (0, 100 * SCROLL_UNITS_PER_DETENT),
        &mut damage,
    );
    assert!(wheel, "the wheel scrolled the rail");
    assert!(damage.bounds().contains(bar.origin), "the bar was reported");
    let view = rail(&places);
    let shown = view.shown_row_rect(last).expect("scrolled into view");
    let at = Point::new(shown.origin.x + 1, shown.origin.y + 1);
    assert_eq!(
        crate::render::sidebar_index_at(window, Scale::ONE, &theme, Some(&places), BAND, at),
        Some(last),
        "a press where the last row shows lands on it"
    );
    assert_eq!(
        view.index_at(Point::new(bar.origin.x, at.y)),
        None,
        "the bar is no row"
    );
}

/// A row the rail's edge cuts is drawn cut and still chosen where it shows,
/// and a key that moves the cursor off the shown rows scrolls it back in.
#[test]
fn a_part_scrolled_rail_row_is_chosen_where_it_shows_and_the_cursor_is_revealed() {
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let volumes: Vec<Volume> = (0..24)
        .map(|at| {
            volume(
                &alloc::format!("Disk {at:02}"),
                &alloc::format!("/Storage/d{at:02}"),
                None,
            )
        })
        .collect();
    let mut places = Places::new(&home(), &volumes);
    let window = Rect::new(0, 0, 400, 300);
    places.scroll_mut().set_offset(5);
    let view =
        crate::render::sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    let whole = view.row_rect(0).expect("the first row");
    let cut = view.shown_row_rect(0).expect("part of it shows");
    assert!(
        cut.height < whole.height,
        "{cut:?} is only part of {whole:?}"
    );
    assert_eq!(
        cut.origin.y,
        view.rows_area().origin.y,
        "cut at the rail's top"
    );
    assert_eq!(
        view.index_at(Point::new(cut.origin.x, cut.origin.y)),
        Some(0)
    );

    places.set_cursor(places.len() - 1);
    let mut damage = tairix_controls::damage::sink();
    assert!(crate::render::sidebar_reveal(
        &mut places,
        (window, Scale::ONE, &theme),
        BAND,
        &mut damage
    ));
    let view =
        crate::render::sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    let shown = view.shown_row_rect(places.len() - 1).expect("revealed");
    assert_eq!(shown.height, whole.height, "revealed whole");
    assert!(
        !crate::render::sidebar_reveal(
            &mut places,
            (window, Scale::ONE, &theme),
            BAND,
            &mut damage
        ),
        "a row already shown whole needs no scroll"
    );
}

#[test]
fn the_content_area_is_inset_by_the_rail_and_untouched_without_one() {
    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 300);
    let places = Places::new(&home(), &[]);
    let view =
        crate::render::sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    let rail = view.width();
    assert!(rail > 0);

    let inset = crate::render::content_area(window, Scale::ONE, &theme, Some(&places), BAND);
    assert_eq!(inset.origin.x, i32::try_from(rail).expect("rail width"));
    assert_eq!(inset.width, window.width - rail);
    assert_eq!(inset.origin.y, window.origin.y);
    assert_eq!(inset.height, window.height);
    // Everything the view lays out follows the inset, so the scrollbar sits
    // against the window's right edge rather than the rail's.
    let bar = crate::render::scrollbar_bounds(Scale::ONE, &theme, inset, BAND).expect("scrollbar");
    assert!(bar.origin.x > inset.origin.x);
    assert!(u32::try_from(bar.origin.x).expect("bar x") + bar.width <= window.width);

    // With no rail the area is the window, byte for byte, so a view without a
    // sidebar is laid out exactly as it was before there was one.
    assert_eq!(
        crate::render::content_area(window, Scale::ONE, &theme, None, BAND),
        window
    );
    // An empty rail is no rail at all.
    let empty = Places::default();
    assert!(crate::render::sidebar_view(window, Scale::ONE, &theme, Some(&empty), BAND).is_none());
    assert_eq!(
        crate::render::content_area(window, Scale::ONE, &theme, Some(&empty), BAND),
        window
    );
}

/// A hidden command band reserves nothing and resolves nothing: the listing
/// starts at the top of the window, the rail beside it does too, and no point
/// in the window answers as a toolbar command or a manager tool.
///
/// The file manager opens with the band hidden, so this is the layout every
/// window is drawn in until the user asks for the strip.
#[test]
fn a_hidden_toolbar_band_reserves_no_height_and_resolves_nothing() {
    use crate::chrome::{ManagerToolModel, ToolbarBand, MANAGER_TOOLS};
    use crate::render::{
        chrome_height, manager_tool_at, sidebar_view, toolbar_command_at, toolbar_height,
    };
    use tairix_geometry::Point;

    const HIDDEN: ToolbarBand = ToolbarBand::Hidden;

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 300);
    let places = Places::new(&home(), &[]);
    let browser = Browser::open_root(MockFs::fixture()).expect("root");

    assert!(
        toolbar_height(Scale::ONE, &theme) > 0,
        "the band has a size"
    );
    assert_eq!(
        chrome_height(Scale::ONE, &theme, HIDDEN),
        0,
        "but a window that does not draw it reserves none of it"
    );
    assert_eq!(
        chrome_height(Scale::ONE, &theme, BAND),
        toolbar_height(Scale::ONE, &theme)
    );

    // The rail starts at the top of the window rather than below a band that
    // is not there.
    let hidden_rail =
        sidebar_view(window, Scale::ONE, &theme, Some(&places), HIDDEN).expect("rail");
    let shown_rail = sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    assert_eq!(hidden_rail.rail_rect().origin.y, window.origin.y);
    assert!(shown_rail.rail_rect().origin.y > hidden_rail.rail_rect().origin.y);
    assert!(hidden_rail.rail_rect().height > shown_rail.rail_rect().height);

    // No point in the window resolves to a command or a tool: there is no
    // drawn button for a press to land on, and hit-testing a band that was
    // never painted would take presses from the listing beneath it.
    for (x, y) in (0..window.width).flat_map(|x| [0_i32, 4, 12, 40, 299].map(move |y| (x, y))) {
        let point = Point::new(i32::try_from(x).expect("window width"), y);
        assert_eq!(
            toolbar_command_at(&browser, Scale::ONE, &theme, window, HIDDEN, point),
            None,
            "no command at {point:?}"
        );
        assert_eq!(
            manager_tool_at(
                &browser,
                Scale::ONE,
                &theme,
                window,
                HIDDEN,
                point,
                MANAGER_TOOLS,
                ManagerToolModel::new(true),
            ),
            None,
            "no tool at {point:?}"
        );
    }
    // Shown, the same band answers — so the hidden case is the flag, not a
    // window that could never have resolved one.
    let band = toolbar_height(Scale::ONE, &theme);
    let hit = (0..band)
        .flat_map(|y| (0..window.width).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            let point = Point::new(i32::try_from(x).ok()?, i32::try_from(y).ok()?);
            toolbar_command_at(&browser, Scale::ONE, &theme, window, BAND, point)
        })
        .is_some();
    assert!(hit, "the drawn band resolves a command");
}

/// The command toolbar is window chrome: its band spans the whole window, so
/// it reaches the leading edge and aligns with the rest of the desktop's
/// chrome whether or not a rail is drawn below it.
///
/// The band was inset by the rail, so the manager's toolbar started a rail's
/// width in from the window edge while the picker's did not.
#[test]
fn the_toolbar_band_spans_the_whole_window_above_the_rail() {
    use crate::chrome::{ManagerToolModel, ToolbarCommand, MANAGER_TOOLS};
    use crate::render::{sidebar_index_at, sidebar_view, toolbar_command_at, toolbar_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 300);
    let places = Places::new(&home(), &[]);
    let band = toolbar_height(Scale::ONE, &theme);
    let rail = sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND)
        .expect("rail")
        .width();
    assert!(rail > 0);

    // Descended, so the leading navigation command is enabled and resolves.
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");

    let paint = |sidebar: Option<&Places>| {
        paint(
            &browser,
            Scale::ONE,
            &theme,
            window,
            &crate::ManagerChrome {
                tools: MANAGER_TOOLS,
                tool_model: ManagerToolModel::new(true),
                sidebar,
                toolbar: BAND,
            },
            &mut NoArtwork,
        )
    };
    // The band is measured from the window, so drawing a rail changes not one
    // pixel of it: the manager's toolbar sits exactly where the picker's does.
    let with_rail = paint(Some(&places));
    let without_rail = paint(None);
    for y in 0..band {
        for x in 0..window.width {
            assert_eq!(
                with_rail.get(x, y),
                without_rail.get(x, y),
                "the toolbar band differs at ({x}, {y}) when a rail is drawn"
            );
        }
    }

    // The leading command sits in the strip the rail covers below the band,
    // and a press there is chrome rather than a place.
    let y = i32::try_from(band / 2).expect("band mid");
    let (x, command) = (0..window.width)
        .find_map(|x| {
            let probe = Point::new(i32::try_from(x).ok()?, y);
            toolbar_command_at(&browser, Scale::ONE, &theme, window, BAND, probe)
                .map(|cmd| (x, cmd))
        })
        .expect("an enabled command is drawn in the band");
    assert_eq!(command, ToolbarCommand::Back);
    assert!(
        x < rail,
        "the leading command at x={x} is within the rail's own {rail}-pixel strip"
    );
    assert_eq!(
        sidebar_index_at(
            window,
            Scale::ONE,
            &theme,
            Some(&places),
            BAND,
            Point::new(i32::try_from(x).expect("command x"), y),
        ),
        None
    );
}

/// The rail is laid out below the toolbar band, so its rows land on exactly
/// the row grid the path bar and the listing rows share.
///
/// The rail ran the full window height from the very top, so its rows sat a
/// band out of step with the listing beside them and read as misaligned.
#[test]
fn the_rail_starts_below_the_toolbar_band_on_the_listings_row_grid() {
    use crate::render::{
        chrome_height, content_area, entry_rect, row_height, sidebar_view, toolbar_height,
    };

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 400);
    let places = Places::new(&home(), &[volume("Backup", "/Storage/Backup", None)]);
    let band = toolbar_height(Scale::ONE, &theme);
    let row = row_height(Scale::ONE, &theme);
    let view = sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND).expect("rail");

    let rail = view.rail_rect();
    assert_eq!(rail.origin.x, window.origin.x);
    assert_eq!(rail.origin.y, i32::try_from(band).expect("band"));
    assert_eq!(rail.height, window.height - band);

    // The toolbar is the whole chrome, so the first rail row shares its top
    // with the first drawn listing row and the two columns read on one grid.
    let first = view.row_rect(0).expect("first row");
    assert_eq!(first.origin.y, i32::try_from(band).expect("band"));
    assert_eq!(first.height, row);

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select(0).expect("select the first entry");
    let area = content_area(window, Scale::ONE, &theme, Some(&places), BAND);
    let listing = entry_rect(&browser, Scale::ONE, &theme, area, BAND, 0).expect("first row drawn");
    assert_eq!(
        listing.origin.y,
        i32::try_from(chrome_height(Scale::ONE, &theme, BAND)).expect("chrome")
    );
    assert_eq!(first.origin.y, listing.origin.y);

    // Every drawn row is on that grid, displaced only by the volumes' band.
    let separator = view.separator_rect().expect("separator").height;
    let volume_start = places.volume_start().expect("a volume is offered");
    for index in 0..places.len() {
        let Some(rect) = view.row_rect(index) else {
            break;
        };
        let step = u32::try_from(index).expect("row index");
        let expected = band + row * step + if index >= volume_start { separator } else { 0 };
        assert_eq!(u32::try_from(rect.origin.y).expect("row top"), expected);
    }
}

/// A press in the toolbar band belongs to the toolbar, never to the rail.
///
/// The rail owned the band's leading strip, so a click aimed at the first
/// command selected a place instead.
#[test]
fn a_press_in_the_toolbar_band_is_never_a_rail_row() {
    use crate::render::{sidebar_index_at, sidebar_view, toolbar_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 400);
    let places = Places::new(&home(), &[volume("Backup", "/Storage/Backup", None)]);
    let band = toolbar_height(Scale::ONE, &theme);
    let rail = sidebar_view(window, Scale::ONE, &theme, Some(&places), BAND)
        .expect("rail")
        .width();

    for y in 0..band {
        for x in 0..rail {
            assert_eq!(
                sidebar_index_at(
                    window,
                    Scale::ONE,
                    &theme,
                    Some(&places),
                    BAND,
                    Point::new(i32::try_from(x).expect("x"), i32::try_from(y).expect("y")),
                ),
                None,
                "({x}, {y}) in the toolbar band resolved to a place"
            );
        }
    }
    // The first row of pixels below the band is the rail's first row.
    assert_eq!(
        sidebar_index_at(
            window,
            Scale::ONE,
            &theme,
            Some(&places),
            BAND,
            Point::new(0, i32::try_from(band).expect("band")),
        ),
        Some(0)
    );
}

/// Paint and hit-test agree about where a toolbar tool is: the tool is drawn
/// inside the very rectangle the window-based hit-test resolves it at, rail or
/// no rail. A view with no rail — the trusted file picker — keeps the whole
/// window and resolves no place at all.
///
/// The manager painted its band inset by the rail while the hit-tests measured
/// from the window, so the two disagreed by the rail's width.
#[test]
fn the_toolbar_is_painted_where_the_window_hit_test_finds_it() {
    use crate::chrome::{ManagerTool, ManagerToolModel, MANAGER_TOOLS};
    use crate::render::{content_area, manager_tool_rect, sidebar_index_at};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 300);
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let places = Places::new(&home(), &[]);

    // The picker keeps the whole window and lays out no rail to press.
    assert_eq!(content_area(window, Scale::ONE, &theme, None, BAND), window);
    assert_eq!(
        sidebar_index_at(window, Scale::ONE, &theme, None, BAND, Point::new(0, 0)),
        None
    );

    let rect = manager_tool_rect(
        &browser,
        Scale::ONE,
        &theme,
        window,
        BAND,
        MANAGER_TOOLS,
        ManagerTool::NewFolder,
    )
    .expect("the New Folder tool is laid out");
    let left = u32::try_from(rect.left()).expect("tool x");
    let top = u32::try_from(rect.top()).expect("tool y");

    let paint = |sidebar: Option<&Places>, tools: &[ManagerTool]| {
        paint(
            &browser,
            Scale::ONE,
            &theme,
            window,
            &crate::ManagerChrome {
                tools,
                tool_model: ManagerToolModel::new(true),
                sidebar,
                toolbar: BAND,
            },
            &mut NoArtwork,
        )
    };
    for sidebar in [None, Some(&places)] {
        // The write tools are the only difference between the two renders, so
        // a pixel differing inside the resolved rectangle is the tool itself.
        let drawn = paint(sidebar, MANAGER_TOOLS);
        let bare = paint(sidebar, &[]);
        let painted = (left..left + rect.width)
            .any(|x| (top..top + rect.height).any(|y| drawn.get(x, y) != bare.get(x, y)));
        assert!(
            painted,
            "the New Folder tool is drawn in the rectangle the hit-test resolves it at"
        );
    }
}

/// Every chrome geometry stays total on a window too small to hold it: no
/// panic, and no row a user could not have seen.
#[test]
fn the_chrome_geometry_stays_total_for_a_degenerate_window() {
    use crate::chrome::{ManagerToolModel, MANAGER_TOOLS};
    use crate::render::{content_area, sidebar_index_at, sidebar_view, toolbar_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let places = Places::new(&home(), &[volume("Backup", "/Storage/Backup", None)]);
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert!(toolbar_height(Scale::ONE, &theme) > 1);

    // Shorter than the toolbar band: the band owns every pixel, so the rail
    // has no height and nothing in the window resolves to a place.
    let squat = Rect::new(0, 0, 400, 1);
    let view = sidebar_view(squat, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    assert_eq!(view.rail_rect().height, 0);
    assert_eq!(view.shown_row_rect(0), None);
    assert_eq!(
        sidebar_index_at(
            squat,
            Scale::ONE,
            &theme,
            Some(&places),
            BAND,
            Point::new(0, 0)
        ),
        None
    );

    // Narrower than the rail: the rail is clamped to the window and the
    // content area keeps what is left rather than wrapping past the edge.
    let narrow = Rect::new(0, 0, 3, 400);
    let thin = sidebar_view(narrow, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    assert!(thin.width() <= narrow.width);
    let area = content_area(narrow, Scale::ONE, &theme, Some(&places), BAND);
    assert!(u32::try_from(area.origin.x).expect("area x") + area.width <= narrow.width);

    // No pixels at all: no row, and still a surface rather than a panic.
    let nothing = Rect::new(0, 0, 0, 0);
    let none = sidebar_view(nothing, Scale::ONE, &theme, Some(&places), BAND).expect("rail");
    assert_eq!(none.shown_row_rect(0), None);
    assert_eq!(
        sidebar_index_at(
            nothing,
            Scale::ONE,
            &theme,
            Some(&places),
            BAND,
            Point::new(0, 0)
        ),
        None
    );
    for window in [squat, narrow, nothing] {
        let surface = paint(
            &browser,
            Scale::ONE,
            &theme,
            window,
            &crate::ManagerChrome {
                tools: MANAGER_TOOLS,
                tool_model: ManagerToolModel::new(true),
                sidebar: Some(&places),
                toolbar: BAND,
            },
            &mut NoArtwork,
        );
        assert_eq!(surface.width(), window.width);
        assert_eq!(surface.height(), window.height);
    }
}

#[test]
fn the_rail_selects_the_row_matching_the_browsers_location() {
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let places = Places::new(&home(), &[]);
    // At the root, no place matches, so nothing is selected.
    assert_eq!(places.index_of(browser.components()), None);
    // Standing on a place selects exactly it.
    assert_eq!(places.index_of(&["Apps".to_string()]), Some(3));
    // A directory *inside* a place is not that place: an exact match only,
    // never a claim that the user is somewhere they are not.
    assert_eq!(
        places.index_of(&["Apps".to_string(), "Notes.app".to_string()]),
        None
    );

    // The selection reaches the drawn rail: the frame differs once the
    // browser stands on a place.
    let theme = Theme::dark();
    let viewport = Rect::new(0, 0, 400, 300);
    let chrome = crate::ManagerChrome {
        tools: &[],
        tool_model: crate::ManagerToolModel::none(),
        sidebar: Some(&places),
        toolbar: BAND,
    };
    let unselected = paint(
        &browser,
        Scale::ONE,
        &theme,
        viewport,
        &chrome,
        &mut NoArtwork,
    );
    let mut at_place = Browser::open_root(MockFs::fixture()).expect("root");
    at_place
        .navigate_to(vec!["System".to_string()])
        .expect("navigate to System");
    let selected = paint(
        &at_place,
        Scale::ONE,
        &theme,
        viewport,
        &chrome,
        &mut NoArtwork,
    );
    let rail = crate::render::sidebar_view(viewport, Scale::ONE, &theme, Some(&places), BAND)
        .expect("rail")
        .row_rect(4)
        .expect("the System row");
    let row_y = usize::try_from(rail.origin.y).expect("row top");
    let width = usize::try_from(viewport.width).expect("width");
    let at = row_y * width;
    assert_ne!(
        unselected.pixels()[at..at + usize::try_from(rail.width).expect("row width")],
        selected.pixels()[at..at + usize::try_from(rail.width).expect("row width")]
    );
}

#[test]
fn a_rail_row_carries_every_state_the_control_offers() {
    let mut places = Places::new(&home(), &[volume("Backup", "/Storage/Backup", None)]);
    // Focus, cursor, and hover are the rail's own; each is reachable and each
    // reports whether it actually moved so a caller repaints only when needed.
    assert!(!places.is_focused());
    places.set_focused(true);
    assert!(places.is_focused());
    assert_eq!(places.cursor(), 0);
    assert!(places.move_cursor(1));
    assert_eq!(places.cursor(), 1);
    assert!(places.move_cursor(-5));
    assert_eq!(places.cursor(), 0);
    // Clamped at both ends: a held arrow never wraps round the rail.
    assert!(!places.move_cursor(-1));
    assert!(places.move_cursor(1_000));
    assert_eq!(places.cursor(), places.len() - 1);
    assert!(!places.move_cursor(1));
    // An index the rail does not have is ignored rather than stored.
    places.set_cursor(places.len());
    assert_eq!(places.cursor(), places.len() - 1);

    assert_eq!(places.hovered(), None);
    assert!(places.set_hovered(Some(2)));
    assert_eq!(places.hovered(), Some(2));
    assert!(!places.set_hovered(Some(2)));
    // A row the rail does not have clears the highlight rather than storing
    // an index nothing will ever draw.
    assert!(places.set_hovered(Some(places.len())));
    assert_eq!(places.hovered(), None);

    // Availability is only ever *taken away*, and only for a row that exists.
    assert!(places.rows().iter().all(crate::Place::is_available));
    places.set_unavailable(1);
    assert!(!places.rows()[1].is_available());
    places.set_unavailable(places.len());
    assert_eq!(
        places.rows().iter().filter(|r| !r.is_available()).count(),
        1
    );
}

/// An in-memory directory tree with an optional set of unreadable paths.
struct MockFs {
    dirs: BTreeMap<String, Vec<Entry>>,
    denied: BTreeSet<String>,
    /// Paths that list successfully once and then fail closed on every later
    /// read, modelling a directory that becomes unreadable underneath the
    /// browser (e.g. its capability is revoked between visits).
    deny_after_first: BTreeSet<String>,
    /// How many times each path has been listed, so the read-count-dependent
    /// behaviours below can trigger.
    reads: BTreeMap<String, usize>,
    root_after_refresh: Option<Vec<Entry>>,
}

impl MockFs {
    /// The-shaped fixture: the four top-level directories, a populated
    /// `/System`, an empty `/System/Fonts`, and a `/System/Security` that
    /// exists but is unreadable (capability-gated).
    fn fixture() -> Self {
        let mut dirs = BTreeMap::new();
        dirs.insert(
            "/".to_string(),
            vec![
                Entry::directory("System"),
                Entry::directory("Users"),
                Entry::directory("Apps"),
                Entry::directory("Storage"),
            ],
        );
        dirs.insert(
            "/System".to_string(),
            vec![
                Entry::directory("Fonts"),
                Entry::directory("Security"),
                Entry::file("Kernel"),
            ],
        );
        dirs.insert("/System/Fonts".to_string(), Vec::new());
        dirs.insert("/Users".to_string(), vec![Entry::directory("alice")]);

        let mut denied = BTreeSet::new();
        denied.insert("/System/Security".to_string());

        Self {
            dirs,
            denied,
            deny_after_first: BTreeSet::new(),
            reads: BTreeMap::new(),
            root_after_refresh: None,
        }
    }
}

impl DirectorySource for MockFs {
    fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
        let path = key(components);
        if self.denied.contains(&path) {
            return Err(Errno::PermissionDenied);
        }
        let reads = self.reads.entry(path.clone()).or_insert(0);
        *reads += 1;
        let count = *reads;
        if count > 1 && self.deny_after_first.contains(&path) {
            return Err(Errno::PermissionDenied);
        }
        if path == "/" && count > 1 {
            if let Some(after) = &self.root_after_refresh {
                return Ok(Listing::Ready(after.clone()));
            }
        }
        self.dirs
            .get(&path)
            .cloned()
            .map(Listing::Ready)
            .ok_or(Errno::NotFound)
    }
}

fn names(browser: &Browser<MockFs>) -> Vec<&str> {
    browser.entries().iter().map(Entry::name).collect()
}

#[test]
fn open_root_lists_the_four_top_level_directories() {
    let browser = Browser::open_root(MockFs::fixture()).expect("root lists");
    assert!(browser.is_root());
    assert_eq!(browser.path(), "/");
    // The source lists the four in insertion order; the browser shows them in
    // the shared default order (directories, then case-insensitive by name).
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    assert_eq!(browser.focus_index(), Some(0));
}

#[test]
fn open_root_fails_closed_when_the_root_is_unreadable() {
    let mut fs = MockFs::fixture();
    fs.denied.insert("/".to_string());
    let result = Browser::open_root(fs);
    assert_eq!(
        result.err(),
        Some(BrowseError::Source(Errno::PermissionDenied))
    );
}

#[test]
fn open_at_starts_at_the_named_directory_with_working_climb() {
    // Opening at `/System` starts *there* — its listing, its breadcrumb — and
    // climbing returns toward the root exactly as a descent would have.
    let start = crate::vfs::components_from_absolute_path("/System").expect("valid path");
    let mut browser = Browser::open_at(MockFs::fixture(), start).expect("System lists");
    assert!(!browser.is_root());
    assert_eq!(browser.path(), "/System");
    assert_eq!(names(&browser), ["Fonts", "Security", "Kernel"]);
    assert_eq!(browser.focus_index(), Some(0));
    // A fresh open has no back history, so the first climb goes to the parent
    // rather than a remembered directory.
    assert_eq!(browser.go_up(), Ok(true));
    assert_eq!(browser.path(), "/");
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
}

#[test]
fn open_at_empty_components_is_exactly_open_root() {
    // `open_root` is defined as `open_at(source, [])`; prove the empty path
    // opens the root so the trusted picker's home-or-root fallback is honest.
    let browser = Browser::open_at(MockFs::fixture(), Vec::new()).expect("root lists");
    assert!(browser.is_root());
    assert_eq!(browser.path(), "/");
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
}

#[test]
fn open_at_fails_closed_when_the_directory_is_unreadable() {
    // A start directory that cannot be listed refuses the open (an `Err`), so
    // the caller can fall back to a directory it can name rather than opening
    // an empty or guessed view.
    let start = crate::vfs::components_from_absolute_path("/System/Security").expect("valid path");
    let result = Browser::open_at(MockFs::fixture(), start);
    assert_eq!(
        result.err(),
        Some(BrowseError::Source(Errno::PermissionDenied))
    );
}

#[test]
fn components_from_absolute_path_parses_and_collapses_slashes() {
    use crate::vfs::components_from_absolute_path as parse;
    // The bare root and the empty string carry no components.
    assert_eq!(parse("/"), Ok(Vec::new()));
    assert_eq!(parse(""), Ok(Vec::new()));
    // Leading, trailing, and repeated separators collapse to the same list.
    let want = vec!["Users".to_string(), "root".to_string()];
    assert_eq!(parse("/Users/root"), Ok(want.clone()));
    assert_eq!(parse("/Users/root/"), Ok(want.clone()));
    assert_eq!(parse("//Users//root//"), Ok(want));
    // The parse is the exact inverse of the shared spelling.
    let round = parse("/Users/root").expect("parses");
    assert_eq!(crate::vfs::spell_absolute_path(&round), "/Users/root");
}

#[test]
fn components_from_absolute_path_rejects_a_malformed_segment() {
    use crate::vfs::components_from_absolute_path as parse;
    // `.`/`..` and a resource-reference `:` are not real leaf names, so the
    // whole path is refused rather than silently reinterpreted.
    assert_eq!(parse("/Users/.."), Err(Errno::OutOfRange));
    assert_eq!(parse("/Users/."), Err(Errno::OutOfRange));
    assert_eq!(parse("/disk:backup"), Err(Errno::OutOfRange));
}

#[test]
fn descend_and_climb_track_the_path_and_entries() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Sorted root order is [Apps, Storage, System, Users]; System is index 2.
    browser.open_index(2).expect("enter System");
    assert!(!browser.is_root());
    assert_eq!(browser.path(), "/System");
    assert_eq!(names(&browser), ["Fonts", "Security", "Kernel"]);

    assert_eq!(browser.go_up(), Ok(true));
    assert_eq!(browser.path(), "/");
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
}

#[test]
fn go_up_at_the_root_is_a_no_op() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert_eq!(browser.go_up(), Ok(false));
    assert!(browser.is_root());
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
}

#[test]
fn opening_a_regular_file_is_rejected_and_changes_nothing() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // Index 2 under /System is the regular file "Kernel".
    assert_eq!(browser.open_index(2), Err(BrowseError::NotADirectory));
    assert_eq!(browser.path(), "/System");
    assert_eq!(names(&browser), ["Fonts", "Security", "Kernel"]);
}

#[test]
fn opening_an_out_of_range_index_is_rejected() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert_eq!(browser.open_index(99), Err(BrowseError::NoSuchEntry));
    assert!(browser.is_root());
}

#[test]
fn descending_into_an_unreadable_directory_fails_closed() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // Index 1 under /System is "Security", which exists but is denied: the
    // read fails and the browser stays on /System with its entries intact.
    assert_eq!(
        browser.open_index(1),
        Err(BrowseError::Source(Errno::PermissionDenied))
    );
    assert_eq!(browser.path(), "/System");
    assert_eq!(names(&browser), ["Fonts", "Security", "Kernel"]);
}

#[test]
fn an_empty_directory_has_no_selection() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    browser.open_index(0).expect("enter Fonts");
    assert_eq!(browser.path(), "/System/Fonts");
    assert!(browser.entries().is_empty());
    assert_eq!(browser.focus_index(), None);
    assert_eq!(focused(&browser), None);
}

#[test]
fn open_selected_descends_into_the_selected_directory() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Sorted root order is [Apps, Storage, System, Users]; Users is index 3.
    browser.select(3).expect("select Users");
    browser.open_selected().expect("enter Users");
    assert_eq!(browser.path(), "/Users");
    assert_eq!(names(&browser), ["alice"]);
}

#[test]
fn selection_movement_clamps_at_both_ends() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select_previous();
    assert_eq!(browser.focus_index(), Some(0));
    for _ in 0..10 {
        browser.select_next();
    }
    assert_eq!(browser.focus_index(), Some(3));
    assert_eq!(browser.select(99), Err(BrowseError::NoSuchEntry));
    assert_eq!(browser.focus_index(), Some(3));
}

#[test]
fn refresh_clamps_a_stale_selection_into_the_new_listing() {
    // The root shrinks to a single entry the next time it is read, modelling
    // the directory changing underneath the browser.
    let mut fs = MockFs::fixture();
    fs.root_after_refresh = Some(vec![Entry::directory("System")]);
    let mut browser = Browser::open_root(fs).expect("root");
    browser.select(3).expect("select Storage");

    browser.refresh().expect("refresh");
    assert_eq!(names(&browser), ["System"]);
    assert_eq!(browser.focus_index(), Some(0));
}

#[test]
fn render_produces_a_surface_the_size_of_the_viewport() {
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let theme = Theme::dark();
    let surface = paint(
        &browser,
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 200, 120),
        &crate::ManagerChrome::none(),
        &mut NoArtwork,
    );
    assert_eq!(surface.width(), 200);
    assert_eq!(surface.height(), 120);
}

#[test]
fn render_gives_the_selected_entry_the_shared_selection_chrome() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select(1).expect("select second entry");
    let theme = Theme::dark();
    // Measured on the face the engine itself resolves: a hand-picked one
    // answers a question about a face nothing draws with.
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, Scale::ONE);
    let row_height = font.glyph_height() + 4;
    let header = crate::render::chrome_height(Scale::ONE, &theme, BAND);
    let surface = paint(
        &browser,
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 200, header + row_height * 3),
        &crate::ManagerChrome::none(),
        &mut NoArtwork,
    );

    let accent = Color::from(theme.palette().accent).premultiply();
    let raised = Color::from(theme.palette().surface_raised).premultiply();
    let base = Color::from(theme.palette().surface).premultiply();
    // The chrome strip (toolbar over the path bar, top-left) carries the
    // raised role.
    assert_eq!(surface.get(0, 0), Some(raised));

    // The list is drawn through the shared `TableRow` chrome below the toolbar
    // and path bar, so entry index 0 is the first content row and the selected
    // entry index 1 the next. The selected row lifts to the raised surface
    // and shows the accent *selection rail* in its leading gutter (not a full
    // accent fill), and an unselected row stays the base surface — the one
    // selection look every collection view shares. We sample inside the
    // content column (x = 100), clear of the leading rail gutter and of the
    // reserved right-edge scrollbar gutter.
    // Entry rows begin below the chrome (toolbar + path bar): entry 0 is the
    // first content row and the selected entry 1 the second.
    let unselected_y = header + 1;
    let selected_y = header + row_height + 1;
    // The unselected row's body is the base surface.
    assert_eq!(surface.get(100, unselected_y), Some(base));
    // The selected row's body lifts to the raised surface.
    assert_eq!(surface.get(100, selected_y), Some(raised));
    // The accent selection rail sits in the selected row's leading gutter.
    let has_accent_rail = (0..20).any(|x| surface.get(x, selected_y) == Some(accent));
    assert!(
        has_accent_rail,
        "the selected row shows the shared accent selection rail"
    );
}

/// Every selected entry is drawn selected, not just the one the focus rests
/// on: a multi-selection the view did not show was one the user could not
/// see they had made. A listing with nothing selected draws no row selected.
#[test]
fn render_draws_every_selected_entry_and_none_while_nothing_is_selected() {
    let theme = Theme::dark();
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, Scale::ONE);
    let row_height = font.glyph_height() + 4;
    let header = crate::render::chrome_height(Scale::ONE, &theme, BAND);
    let raised = Color::from(theme.palette().surface_raised).premultiply();
    let rows_lifted = |browser: &Browser<MockFs>| -> Vec<bool> {
        let surface = paint(
            browser,
            Scale::ONE,
            &theme,
            Rect::new(0, 0, 200, header + row_height * 4),
            &crate::ManagerChrome::none(),
            &mut NoArtwork,
        );
        (0..4)
            .map(|row| surface.get(100, header + row_height * row + 1) == Some(raised))
            .collect()
    };

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert_eq!(rows_lifted(&browser), [false; 4]);
    browser.select(0).expect("first");
    browser.toggle_selection(2).expect("and third");
    assert_eq!(rows_lifted(&browser), [true, false, true, false]);
}

#[test]
fn render_into_a_tiny_viewport_does_not_panic() {
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let theme = Theme::dark();
    // Too short for even the path bar: paints what it can and returns a
    // surface rather than panicking.
    let surface = paint(
        &browser,
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 4, 3),
        &crate::ManagerChrome::none(),
        &mut NoArtwork,
    );
    assert_eq!(surface.width(), 4);
    assert_eq!(surface.height(), 3);
}

/// A tile is exactly as tall as its picture and two name lines under it: the
/// picture keeps the side tiles have always drawn, the name sits half an inset
/// beneath it, and nothing is left over below.
#[test]
fn a_grid_tile_is_as_tall_as_its_picture_and_two_name_lines() {
    use crate::render::{grid_metrics, TILE_LAYOUT};

    let theme = Theme::dark();
    for (percent, picture, height) in [(100, 37, 89), (200, 74, 178)] {
        let scale = Scale::from_percent(percent).expect("a valid scale");
        let tiles = grid_metrics(scale, &theme);
        assert_eq!(tiles.cell_height, height, "at {percent}%");
        let cell = Rect::new(0, 0, tiles.cell_width, tiles.cell_height);
        assert_eq!(TILE_LAYOUT.icon_side(cell, scale, &theme), picture);
        assert_eq!(TILE_LAYOUT.label_lines(cell, scale, &theme), 2);
    }
}

/// A tile keeps whatever ends a name whole when it has to cut it: the
/// extension after the last dot, or a RISC OS file type after the last comma.
#[test]
fn a_tile_cuts_a_name_in_its_middle_keeping_its_ending() {
    use crate::render::name_cut;
    use tairix_font::Cut;

    for (name, keep) in [
        ("holiday.jpeg", 5),
        ("archive.tar.gz", 3),
        ("Sprites,ff9", 4),
        ("Makefile", 0),
        (".profile", 0),
        ("Editor.app", 4),
    ] {
        assert_eq!(name_cut(name), Cut::Middle { keep }, "{name}");
    }
}

/// A rename opens with the name's stem selected — what typing replaces —
/// keeping what ends it; a folder's name is selected whole.
#[test]
fn a_rename_selects_the_stem_and_keeps_the_ending() {
    use crate::rename_selection;

    for (entry, range) in [
        (Entry::file("holiday.jpeg"), 0..7),
        (Entry::file("Sprites,ff9"), 0..7),
        (Entry::file("archive.tar.gz"), 0..11),
        (Entry::file("Makefile"), 0..8),
        (Entry::file(".profile"), 0..8),
        (
            Entry::new("Editor.app", EntryKind::Bundle, 0, Time64::UNIX_EPOCH),
            0..6,
        ),
        (Entry::directory("v1.2"), 0..4),
    ] {
        assert_eq!(rename_selection(&entry), range, "{}", entry.name());
    }
}

/// Every chrome length tracks the desktop's density, not just the glyphs.
///
/// The renderer had no `Scale` at all and pinned every length it derived from
/// the theme at 100%, so the toolbar strip, the scrollbar gutter, the rail's
/// padding, the tile grid and the overlays' title bands measured exactly the
/// same at 200% as at 100% while only the text around them grew.
#[test]
fn the_chrome_scales_with_the_desktop_density_not_only_its_text() {
    use crate::render::{
        chrome_height, delete_dialog_rect, grid_metrics, open_with_chooser_extent, row_height,
        scrollbar_bounds, sidebar_view, toolbar_height,
    };

    let theme = Theme::dark();
    let hidpi = Scale::from_percent(200).expect("200% is a valid scale");
    let vp = Rect::new(0, 0, 600, 600);
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let places = Places::new(&home(), &[]);

    // The horizontal bands, top to bottom.
    assert!(toolbar_height(hidpi, &theme) > toolbar_height(Scale::ONE, &theme));
    assert!(row_height(hidpi, &theme) > row_height(Scale::ONE, &theme));
    assert!(chrome_height(hidpi, &theme, BAND) > chrome_height(Scale::ONE, &theme, BAND));

    // The reserved scrollbar gutter, and the header it begins below.
    let bar = scrollbar_bounds(Scale::ONE, &theme, vp, BAND).expect("a gutter at 100%");
    let bar_hidpi = scrollbar_bounds(hidpi, &theme, vp, BAND).expect("a gutter at 200%");
    assert!(bar_hidpi.width > bar.width);
    assert!(bar_hidpi.origin.y > bar.origin.y);

    // The places rail and the icon grid's tiles.
    let rail = sidebar_view(vp, Scale::ONE, &theme, Some(&places), BAND).expect("a rail at 100%");
    let rail_hidpi = sidebar_view(vp, hidpi, &theme, Some(&places), BAND).expect("a rail at 200%");
    assert!(rail_hidpi.width() > rail.width());
    let tiles = grid_metrics(Scale::ONE, &theme);
    let tiles_hidpi = grid_metrics(hidpi, &theme);
    assert!(tiles_hidpi.cell_width > tiles.cell_width);
    assert!(tiles_hidpi.cell_height > tiles.cell_height);
    assert!(tiles_hidpi.gap > tiles.gap);

    // The overlays this surface still owns: the delete confirmation and the
    // "Open With…" chooser. The right-click menu is not among them — its
    // plates are the desktop's, placed by the one shared rule that reads the
    // desktop's own density.
    assert!(
        delete_dialog_rect(vp, hidpi, &theme).height
            > delete_dialog_rect(vp, Scale::ONE, &theme).height
    );
    // The chooser is its own popup, sized to its content, so what scales with
    // the density is the extent it asks that popup to be — in both axes, since
    // both are measured from what it draws.
    {
        use crate::open_with::{AppAssociation, OpenWithChooser};
        let apps: alloc::vec::Vec<AppAssociation> = (0..3)
            .map(|n| AppAssociation::new(alloc::format!("App{n}"), "/Apps/A.app", alloc::vec![]))
            .collect();
        let refs: alloc::vec::Vec<&AppAssociation> = apps.iter().collect();
        let chooser = OpenWithChooser::new(&refs, "/f", "f").expect("three candidates");
        let dense = open_with_chooser_extent(&chooser, hidpi, &theme, vp);
        let plain = open_with_chooser_extent(&chooser, Scale::ONE, &theme, vp);
        assert!(dense.0 > plain.0 && dense.1 > plain.1);
    }

    // And the whole view still paints at the higher density.
    assert!(paint(
        &browser,
        hidpi,
        &theme,
        vp,
        &crate::ManagerChrome::none(),
        &mut NoArtwork,
    )
    .pixels()
    .iter()
    .any(|p| p.a > 0));
}

// --- The VFS engine ------------------------------------------------------
//
// `VfsDirectorySource` is the production source; these tests drive it (and
// a `Browser` over it) against in-memory *encoded* `DirEntry` streams — the
// exact bytes a kernel `fs_readdir` transfer produces — so the spelling,
// decode, and refusal branches are all host-proven.

use tairix_abi::fs::{DirEntry, FileKind, FS_PATH_MAX};
use tairix_abi::time::Time64;

use crate::entry::{LinkResolution, LinkTarget};
use crate::vfs::{
    absolute_path, entries_from_dir_stream, LinkInfo, LinkReader, NoLinks, VfsDirectorySource,
};

/// Encode `(name, kind)` children as one packed `DirEntry` stream.
fn encoded_stream(children: &[(&[u8], FileKind)]) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    let mut off = 0;
    for (name, kind) in children {
        off += DirEntry {
            kind: *kind,
            size: 0,
            allocated: 0,
            modified: Time64::UNIX_EPOCH,
            id: tairix_abi::FileId::NONE,
            nlink: 1,
            name,
            content_gen: 0,
        }
        .encode_into(&mut buf[off..])
        .expect("fits");
    }
    buf.truncate(off);
    buf
}

/// A [`LinkReader`] over an in-memory table of `link path -> (target, kind)`,
/// where a `None` kind is a link whose target cannot be reached.
#[derive(Clone, Default)]
struct FixtureLinks {
    links: BTreeMap<String, (String, Option<FileKind>)>,
}

impl FixtureLinks {
    /// A link at `path` naming `target`, which resolves to `kind` (or to
    /// nothing when `kind` is `None`).
    fn with(mut self, path: &str, target: &str, kind: Option<FileKind>) -> Self {
        self.links
            .insert(path.to_string(), (target.to_string(), kind));
        self
    }
}

impl LinkReader for FixtureLinks {
    fn describe(&mut self, path: &str) -> Option<LinkInfo> {
        self.links.get(path).map(|(target, kind)| LinkInfo {
            target: target.clone(),
            kind: *kind,
        })
    }
}

/// A source over an in-memory path → encoded-stream tree, with no links.
fn tree_source(
    dirs: BTreeMap<String, Vec<u8>>,
) -> VfsDirectorySource<impl FnMut(&str) -> Result<Vec<u8>, Errno>, NoLinks> {
    VfsDirectorySource::new(
        move |path: &str| dirs.get(path).cloned().ok_or(Errno::NotFound),
        NoLinks,
    )
}

/// A source over an in-memory tree whose links `links` describes.
fn tree_source_with_links(
    dirs: BTreeMap<String, Vec<u8>>,
    links: FixtureLinks,
) -> VfsDirectorySource<impl FnMut(&str) -> Result<Vec<u8>, Errno>, FixtureLinks> {
    VfsDirectorySource::new(
        move |path: &str| dirs.get(path).cloned().ok_or(Errno::NotFound),
        links,
    )
}

#[test]
fn absolute_path_spells_root_and_nested_directories() {
    assert_eq!(absolute_path(&[]).expect("root"), "/");
    assert_eq!(
        absolute_path(&["System".to_string(), "Fonts".to_string()]).expect("nested"),
        "/System/Fonts"
    );
}

#[test]
fn absolute_path_refuses_malformed_components() {
    for bad in ["", ".", "..", "a/b", "nul\0byte"] {
        assert_eq!(
            absolute_path(&[bad.to_string()]),
            Err(Errno::OutOfRange),
            "component {bad:?} must be refused before any syscall"
        );
    }
}

#[test]
fn absolute_path_enforces_the_kernel_path_bound() {
    // Each component is a valid single name (well within the per-name bound),
    // so the *whole-path* FS_PATH_MAX bound is what trips: enough 250-byte
    // components that the spelled path runs past FS_PATH_MAX.
    let component = "a".repeat(250);
    let deep: Vec<String> = core::iter::repeat_n(component, FS_PATH_MAX / 250 + 1).collect();
    assert_eq!(
        absolute_path(&deep),
        Err(Errno::LengthOutOfRange),
        "a spelled path over FS_PATH_MAX must never reach the kernel"
    );
}

#[test]
fn absolute_path_refuses_an_over_long_component() {
    // A single component past the per-name bound is refused as a malformed
    // component, before the whole-path length is even considered.
    let huge = "a".repeat(300);
    assert_eq!(absolute_path(&[huge]), Err(Errno::OutOfRange));
}

#[test]
fn entries_from_dir_stream_maps_names_and_kinds_in_order() {
    let stream = encoded_stream(&[
        (b"Logs", FileKind::Directory),
        (b"motd.txt", FileKind::Regular),
    ]);
    let entries = entries_from_dir_stream("/", &stream, &mut NoLinks).expect("valid stream");
    assert_eq!(
        entries,
        vec![Entry::directory("Logs"), Entry::file("motd.txt")]
    );
}

/// A listed entry names the file, and the version of its data, its record
/// named — which a thumbnail checks an open against — and a new version is a
/// changed listing.
#[test]
fn a_listed_entry_names_the_file_and_version_its_record_named() {
    let id = tairix_abi::FileId {
        volume: [3; 16],
        node: 9,
    };
    let mut buf = vec![0u8; 256];
    let len = DirEntry {
        kind: FileKind::Regular,
        size: 5,
        allocated: 0,
        modified: Time64::UNIX_EPOCH,
        id,
        nlink: 1,
        content_gen: 42,
        name: b"cat.png",
    }
    .encode_into(&mut buf)
    .expect("fits");
    let entries = entries_from_dir_stream("/", &buf[..len], &mut NoLinks).expect("valid stream");
    let [entry] = entries.as_slice() else {
        panic!("one entry: {entries:?}");
    };
    assert_eq!(
        entry.stamp(),
        tairix_icon::DocumentStamp {
            size: 5,
            modified: Time64::UNIX_EPOCH,
            id,
            content_gen: 42,
        }
    );
    let rewritten = entry.clone().with_content_gen(43);
    assert!(!entry.same_listing(&rewritten), "a new version is a change");
}

#[test]
fn entries_from_dir_stream_refuses_a_non_utf8_name_whole() {
    let stream = encoded_stream(&[(b"ok", FileKind::Regular), (b"\xff\xfe", FileKind::Regular)]);
    assert_eq!(
        entries_from_dir_stream("/", &stream, &mut NoLinks),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn entries_from_dir_stream_refuses_a_truncated_stream_whole() {
    let mut stream = encoded_stream(&[(b"ok", FileKind::Regular)]);
    stream.extend_from_slice(&[0u8; 3]);
    assert_eq!(
        entries_from_dir_stream("/", &stream, &mut NoLinks),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn a_browser_navigates_the_vfs_source_end_to_end() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream(&[(b"System", FileKind::Directory)]),
    );
    dirs.insert(
        "/System".to_string(),
        encoded_stream(&[
            (b"Fonts", FileKind::Directory),
            (b"motd.txt", FileKind::Regular),
        ]),
    );
    dirs.insert("/System/Fonts".to_string(), encoded_stream(&[]));

    let mut browser = Browser::open_root(tree_source(dirs)).expect("root opens");
    assert_eq!(browser.entries(), &[Entry::directory("System")]);

    browser.open_index(0).expect("descend into /System");
    assert_eq!(browser.path(), "/System");
    assert_eq!(
        browser.entries(),
        &[Entry::directory("Fonts"), Entry::file("motd.txt")]
    );

    browser.open_index(0).expect("descend into /System/Fonts");
    assert_eq!(browser.path(), "/System/Fonts");
    assert!(browser.entries().is_empty());

    assert!(browser.go_up().expect("climb back"));
    assert_eq!(browser.path(), "/System");
}

#[test]
fn entry_index_at_mirrors_the_rendered_rows() {
    use crate::render::{chrome_height, entry_index_at, row_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let browser = Browser::open_root(MockFs::fixture()).expect("root opens");
    let row = row_height(Scale::ONE, &theme);
    // The chrome (toolbar + path bar) reserved above the first entry row.
    let header = chrome_height(Scale::ONE, &theme, BAND);
    // A window wide enough for content beside the scrollbar gutter, the chrome
    // plus several entry rows tall. Clicks land in the content column (x=4).
    let vp = |h: u32| Rect::new(0, 0, 200, h);
    let at = |b: &Browser<MockFs>, h: u32, y: u32| {
        entry_index_at(
            b,
            Scale::ONE,
            &theme,
            vp(h),
            BAND,
            Point::new(4, i32::try_from(y).unwrap()),
        )
    };
    let viewport_height = header + row * 4;

    // The chrome resolves to no entry; the first list row is entry 0.
    assert_eq!(at(&browser, viewport_height, 0), None);
    assert_eq!(at(&browser, viewport_height, header - 1), None);
    assert_eq!(at(&browser, viewport_height, header), Some(0));
    assert_eq!(
        at(&browser, viewport_height, header + row + row / 2),
        Some(1)
    );
    // A row past the listing's end and a coordinate outside the viewport
    // resolve to nothing rather than a clamped guess.
    let last = u32::try_from(browser.entries().len()).expect("a tiny fixture listing");
    assert_eq!(
        at(&browser, header + row * (last + 1), header + row * last),
        None
    );
    assert_eq!(at(&browser, viewport_height, viewport_height), None);
    // A degenerate viewport (chrome only) has no clickable rows.
    assert_eq!(at(&browser, header, header), None);
    // A click in the reserved scrollbar gutter resolves to no row.
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp(viewport_height),
            BAND,
            Point::new(199, i32::try_from(header).unwrap())
        ),
        None
    );
}

#[test]
fn entry_index_at_accounts_for_the_scroll_anchor() {
    use crate::render::{chrome_height, entry_index_at, reveal_selection, row_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root opens");
    let row = row_height(Scale::ONE, &theme);
    let header = chrome_height(Scale::ONE, &theme, BAND);
    // Two visible entry rows below the chrome; select the last entry and reveal
    // it so the list scrolls to keep it on the bottom row — as the app does.
    let viewport_height = header + row * 2;
    let vp = Rect::new(0, 0, 200, viewport_height);
    let last = browser.entries().len() - 1;
    browser.select(last).expect("selectable");
    reveal_selection(&mut browser, Scale::ONE, &theme, vp, BAND);
    // The bottom visible row is the selected (last) entry; the row above
    // it is its predecessor — exactly what `render` draws.
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(4, i32::try_from(header + row).unwrap())
        ),
        Some(last)
    );
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(4, i32::try_from(header).unwrap())
        ),
        Some(last - 1)
    );
}

#[test]
fn a_missing_directory_surfaces_the_fetch_refusal() {
    let mut source = tree_source(BTreeMap::new());
    assert_eq!(
        source.list(&["System".to_string()]),
        Err(Errno::NotFound),
        "the engine adds no authority and fabricates no listing"
    );
}

// --- FM1: richer entries, bundle recognition, and the shared sort --------

use crate::entry::{is_bundle_name, EntryKind};
use crate::sort::{sort_entries, SortDirection, SortKey, SortMode};

/// Encode `(name, kind, size, modified)` children as one packed `DirEntry`
/// stream — the metadata-carrying sibling of [`encoded_stream`].
fn encoded_stream_meta(children: &[(&[u8], FileKind, u64, Time64)]) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    let mut off = 0;
    for (name, kind, size, modified) in children {
        off += DirEntry {
            kind: *kind,
            size: *size,
            allocated: 0,
            modified: *modified,
            id: tairix_abi::FileId::NONE,
            nlink: 1,
            name,
            content_gen: 0,
        }
        .encode_into(&mut buf[off..])
        .expect("fits");
    }
    buf.truncate(off);
    buf
}

#[test]
fn entries_carry_size_and_modified_from_the_stream() {
    let modified = Time64::new(1_700_000_000, 500).expect("canonical");
    let stream = encoded_stream_meta(&[
        (b"data.bin", FileKind::Regular, 4096, modified),
        (b"Docs", FileKind::Directory, 0, Time64::from_secs(-100)),
    ]);
    let entries = entries_from_dir_stream("/", &stream, &mut NoLinks).expect("valid");
    // The stream order is preserved here (the browser applies the sort).
    assert_eq!(entries[0].name(), "data.bin");
    assert_eq!(entries[0].size(), 4096);
    assert_eq!(entries[0].modified(), modified);
    assert_eq!(entries[0].kind(), EntryKind::File);
    assert_eq!(entries[1].name(), "Docs");
    assert_eq!(entries[1].size(), 0);
    assert_eq!(entries[1].modified(), Time64::from_secs(-100));
    assert_eq!(entries[1].kind(), EntryKind::Directory);
}

#[test]
fn a_bad_record_still_refuses_the_whole_listing() {
    // A directory record whose kind byte is corrupt fails the whole stream:
    // the metadata path never shows a partial listing (fail closed).
    let mut stream = encoded_stream_meta(&[(b"ok", FileKind::Regular, 1, Time64::UNIX_EPOCH)]);
    let mut bad = encoded_stream_meta(&[(b"x", FileKind::Regular, 0, Time64::UNIX_EPOCH)]);
    bad[0] = 9;
    stream.extend_from_slice(&bad);
    assert_eq!(
        entries_from_dir_stream("/", &stream, &mut NoLinks),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn is_bundle_name_matches_only_a_named_dot_app() {
    assert!(is_bundle_name("Example.app"));
    assert!(is_bundle_name("Text Editor.app"));
    // Case-insensitive suffix so a volume's casing does not hide a bundle.
    assert!(is_bundle_name("Thing.APP"));
    // A base name is required: the bare suffix is not a bundle.
    assert!(!is_bundle_name(".app"));
    assert!(!is_bundle_name("app"));
    assert!(!is_bundle_name("notes.txt"));
    assert!(!is_bundle_name(""));
    assert!(!is_bundle_name("Example.apple"));
}

#[test]
fn a_dot_app_directory_is_a_bundle_not_a_folder_to_descend() {
    let stream = encoded_stream(&[
        (b"Editor.app", FileKind::Directory),
        (b"plain", FileKind::Directory),
        // A regular file that merely ends in .app is a file, not a bundle:
        // only a *directory* named <Name>.app is a bundle.
        (b"report.app", FileKind::Regular),
    ]);
    let entries = entries_from_dir_stream("/", &stream, &mut NoLinks).expect("valid");
    assert_eq!(entries[0].kind(), EntryKind::Bundle);
    assert!(entries[0].is_bundle());
    assert!(!entries[0].is_directory(), "a bundle is not descended into");
    assert_eq!(entries[1].kind(), EntryKind::Directory);
    assert!(entries[1].is_directory());
    assert_eq!(entries[2].kind(), EntryKind::File);
}

#[test]
fn a_browser_refuses_to_descend_into_a_bundle() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream(&[(b"Editor.app", FileKind::Directory)]),
    );
    let mut browser = Browser::open_root(tree_source(dirs)).expect("root opens");
    // The bundle is modelled as a sealed unit: the browser opens nothing
    // itself (launching is the app layer's job), so descending is refused.
    assert_eq!(browser.open_index(0), Err(BrowseError::NotADirectory));
    assert!(browser.is_root());
}

/// A short listing spanning both groups and mixed case, in a deliberately
/// unsorted source order.
fn mixed_listing() -> Vec<Entry> {
    vec![
        Entry::new("banana.txt", EntryKind::File, 30, Time64::from_secs(300)),
        Entry::directory("Zebra"),
        Entry::new("Apple.txt", EntryKind::File, 10, Time64::from_secs(100)),
        Entry::directory("apricot"),
        Entry::new("Editor.app", EntryKind::Bundle, 0, Time64::from_secs(200)),
        Entry::new("cherry.txt", EntryKind::File, 20, Time64::from_secs(400)),
    ]
}

#[test]
fn default_sort_is_directories_first_then_case_insensitive_name() {
    let mut entries = mixed_listing();
    sort_entries(&mut entries, SortMode::default_order());
    let ordered: Vec<&str> = entries.iter().map(Entry::name).collect();
    // Directories first (apricot < Zebra, case-folded); then files and the
    // bundle together, case-insensitively by name.
    assert_eq!(
        ordered,
        [
            "apricot",
            "Zebra",
            "Apple.txt",
            "banana.txt",
            "cherry.txt",
            "Editor.app",
        ]
    );
}

#[test]
fn sort_by_size_descending_keeps_directories_first() {
    let mut entries = mixed_listing();
    sort_entries(
        &mut entries,
        SortMode {
            key: SortKey::Size,
            direction: SortDirection::Descending,
        },
    );
    let ordered: Vec<&str> = entries.iter().map(Entry::name).collect();
    // Directories still lead (grouping is fixed); among the rest, largest
    // size first, with the two zero-size entries settled by the name tiebreak.
    assert_eq!(
        ordered,
        [
            "apricot",
            "Zebra",
            "banana.txt",
            "cherry.txt",
            "Apple.txt",
            "Editor.app",
        ]
    );
}

#[test]
fn sort_by_modified_orders_within_the_file_group() {
    let mut entries = mixed_listing();
    sort_entries(
        &mut entries,
        SortMode {
            key: SortKey::Modified,
            direction: SortDirection::Ascending,
        },
    );
    let files: Vec<&str> = entries
        .iter()
        .filter(|e| !e.is_directory())
        .map(Entry::name)
        .collect();
    // Earliest modified first: Apple(100) < Editor(200) < banana(300) < cherry(400).
    assert_eq!(
        files,
        ["Apple.txt", "Editor.app", "banana.txt", "cherry.txt"]
    );
}

#[test]
fn sort_by_kind_clusters_a_typed_name_by_its_extension() {
    let mut entries = vec![
        Entry::new("b.zip", EntryKind::File, 0, Time64::from_secs(0)),
        Entry::new("c.txt", EntryKind::File, 0, Time64::from_secs(0)),
        Entry::new("a.zip,a91", EntryKind::File, 0, Time64::from_secs(0)),
    ];
    sort_entries(
        &mut entries,
        SortMode {
            key: SortKey::Kind,
            direction: SortDirection::Ascending,
        },
    );
    let ordered: Vec<&str> = entries.iter().map(Entry::name).collect();
    assert_eq!(ordered, ["c.txt", "a.zip,a91", "b.zip"]);
}

#[test]
fn sort_by_kind_clusters_bundles_ahead_of_files_within_the_non_directory_group() {
    let mut entries = mixed_listing();
    sort_entries(
        &mut entries,
        SortMode {
            key: SortKey::Kind,
            direction: SortDirection::Ascending,
        },
    );
    let ordered: Vec<&str> = entries.iter().map(Entry::name).collect();
    // Directories still lead (grouping is fixed, and both are unaffected by
    // kind); among the rest, the bundle sorts ahead of the plain files (all
    // sharing one extension, so the name tiebreak settles them) — a genuinely
    // different order from name or size, which both put the bundle last.
    assert_eq!(
        ordered,
        [
            "apricot",
            "Zebra",
            "Editor.app",
            "Apple.txt",
            "banana.txt",
            "cherry.txt",
        ]
    );
}

#[test]
fn sort_of_an_empty_listing_is_a_no_op() {
    let mut entries: Vec<Entry> = Vec::new();
    sort_entries(&mut entries, SortMode::default_order());
    assert!(entries.is_empty());
}

#[test]
fn set_sort_mode_keeps_the_selection_on_the_same_entry() {
    // A source whose entries only sort differently under each mode.
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream_meta(&[
            (b"a.txt", FileKind::Regular, 300, Time64::UNIX_EPOCH),
            (b"b.txt", FileKind::Regular, 100, Time64::UNIX_EPOCH),
            (b"c.txt", FileKind::Regular, 200, Time64::UNIX_EPOCH),
        ]),
    );
    let mut browser = Browser::open_root(tree_source(dirs)).expect("root");
    // Default (name asc): [a, b, c]; select "b".
    assert_eq!(browser.select(1), Ok(()));
    assert_eq!(focused(&browser).map(Entry::name), Some("b.txt"));

    browser.set_sort_mode(SortMode {
        key: SortKey::Size,
        direction: SortDirection::Ascending,
    });
    // Now [b(100), c(200), a(300)]; the selection followed "b" to index 0.
    let names: Vec<&str> = browser.entries().iter().map(Entry::name).collect();
    assert_eq!(names, ["b.txt", "c.txt", "a.txt"]);
    assert_eq!(focused(&browser).map(Entry::name), Some("b.txt"));
    assert_eq!(browser.focus_index(), Some(0));
    assert_eq!(browser.sort_mode().key, SortKey::Size);
}

#[test]
fn set_sort_mode_is_a_no_op_when_the_mode_is_unchanged() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select(2).expect("select");
    let before: Vec<String> = browser
        .entries()
        .iter()
        .map(|e| e.name().to_string())
        .collect();
    browser.set_sort_mode(SortMode::default_order());
    let after: Vec<String> = browser
        .entries()
        .iter()
        .map(|e| e.name().to_string())
        .collect();
    assert_eq!(before, after);
    assert_eq!(browser.focus_index(), Some(2));
}

// --- FM2b: the view toggle, the icon grid, and the drawn scrollbar -------

use crate::layout::ViewMode;

/// A browser over a root of `n` regular files (`f0`, `f1`, …), enough to
/// scroll a modest window.
fn many_files(n: usize) -> Browser<MockFs> {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        (0..n)
            .map(|i| Entry::file(alloc::format!("f{i:03}")))
            .collect(),
    );
    let fs = MockFs {
        dirs,
        denied: BTreeSet::new(),
        deny_after_first: BTreeSet::new(),
        reads: BTreeMap::new(),
        root_after_refresh: None,
    };
    Browser::open_root(fs).expect("root opens")
}

#[test]
fn the_view_mode_defaults_to_list_and_toggles_preserving_selection() {
    let mut browser = many_files(20);
    assert_eq!(browser.view_mode(), ViewMode::List);
    browser.select(7).expect("selectable");
    let names_before: Vec<String> = browser
        .entries()
        .iter()
        .map(|e| e.name().to_string())
        .collect();

    browser.set_view_mode(ViewMode::Grid);
    assert_eq!(browser.view_mode(), ViewMode::Grid);
    // The selection stays on the same entry and the listing is untouched.
    assert_eq!(browser.focus_index(), Some(7));
    let names_after: Vec<String> = browser
        .entries()
        .iter()
        .map(|e| e.name().to_string())
        .collect();
    assert_eq!(names_before, names_after);
    // Switching unit resets the scroll to the top.
    assert_eq!(browser.scroll_offset(), 0);
    // Toggling back is symmetric.
    browser.set_view_mode(ViewMode::List);
    assert_eq!(browser.view_mode(), ViewMode::List);
    assert_eq!(browser.focus_index(), Some(7));
}

/// One wheel detent, in the seat's scroll units.
const DETENT: i32 = tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

/// Turn the wheel over `browser`'s listing by `(0, dy)` scroll units at
/// [`Scale::ONE`], answering whether it moved and what it reported.
fn turn(
    browser: &mut Browser<MockFs>,
    theme: &Theme,
    vp: Rect,
    dy: i32,
) -> (bool, tairix_geometry::Region) {
    let mut damage = tairix_controls::damage::sink();
    let moved =
        crate::render::scroll_wheel(browser, Scale::ONE, theme, vp, BAND, (0, dy), &mut damage);
    (moved, damage)
}

/// A window the chrome plus `rows` list rows tall.
fn rows_tall(theme: &Theme, rows: u32) -> Rect {
    let row = crate::render::row_height(Scale::ONE, theme);
    Rect::new(
        0,
        0,
        200,
        crate::render::chrome_height(Scale::ONE, theme, BAND) + row * rows,
    )
}

#[test]
fn a_wheel_detent_moves_the_listing_the_wheel_step_and_clamps_at_the_ends() {
    use crate::render::row_height;
    use tairix_controls::scroll::WHEEL_STEP;

    let theme = Theme::dark();
    let mut browser = many_files(20);
    let row = row_height(Scale::ONE, &theme);
    let vp = rows_tall(&theme, 4);

    assert!(
        !turn(&mut browser, &theme, vp, -DETENT).0,
        "already at the top"
    );
    assert_eq!(browser.scroll_offset(), 0);
    assert!(turn(&mut browser, &theme, vp, DETENT).0);
    assert_eq!(
        browser.scroll_offset(),
        u64::from(Scale::ONE.scale_length(WHEEL_STEP)),
        "a detent is a fixed distance, not a count of rows"
    );
    // Twenty rows through four: the view can rest sixteen rows down at most.
    assert!(turn(&mut browser, &theme, vp, DETENT * 1000).0);
    assert_eq!(browser.scroll_offset(), u64::from(row * 16));
    assert!(!turn(&mut browser, &theme, vp, DETENT).0);
    assert_eq!(browser.scroll_offset(), u64::from(row * 16));
}

/// The listing's bar keeps what a turn leaves short of a pixel, so turns too
/// small to move it on their own add up across calls instead of being lost.
#[test]
fn a_wheel_turn_short_of_a_pixel_carries_into_the_next() {
    use tairix_controls::scroll::WHEEL_STEP;

    let theme = Theme::dark();
    let mut browser = many_files(20);
    let vp = rows_tall(&theme, 4);
    let per_pixel = DETENT.unsigned_abs().div_ceil(WHEEL_STEP);
    for _ in 1..per_pixel {
        assert!(
            !turn(&mut browser, &theme, vp, 1).0,
            "a unit is under a pixel"
        );
    }
    assert_eq!(browser.scroll_offset(), 0);
    assert!(
        turn(&mut browser, &theme, vp, 1).0,
        "the carried units add up"
    );
    assert_eq!(browser.scroll_offset(), 1);
}

/// A wheel scroll repaints what it moved: the bar's thumb and every item in
/// the list area — and nothing of the chrome above them.
#[test]
fn a_wheel_scroll_reports_the_bar_and_the_items_it_slid() {
    use crate::render::{chrome_height, entry_rect, scrollbar_bounds, visible_range};

    let theme = Theme::dark();
    let mut browser = many_files(20);
    let vp = rows_tall(&theme, 4);
    let (moved, damage) = turn(&mut browser, &theme, vp, DETENT);
    assert!(moved);
    let covered = damage.bounds();
    let bar = scrollbar_bounds(Scale::ONE, &theme, vp, BAND).expect("a gutter");
    assert_eq!(covered.intersection(&bar), bar, "the thumb moved");
    for index in visible_range(&browser, Scale::ONE, &theme, vp, BAND) {
        let item = entry_rect(&browser, Scale::ONE, &theme, vp, BAND, index).expect("shown");
        assert_eq!(covered.intersection(&item), item, "row {index} moved");
    }
    let header = chrome_height(Scale::ONE, &theme, BAND);
    assert_eq!(
        covered.top(),
        i32::try_from(header).unwrap(),
        "the toolbar band did not move"
    );
    let (_, idle) = turn(&mut browser, &theme, vp, 0);
    assert!(
        idle.is_empty(),
        "a turn that moves nothing repaints nothing"
    );
}

#[test]
fn the_drawn_scrollbar_reflects_the_scroll_offset() {
    use crate::render::{row_height, scroll_model};
    use tairix_controls::{ScrollBar, ScrollOrientation};

    let theme = Theme::dark();
    let mut browser = many_files(40);
    let row = row_height(Scale::ONE, &theme);
    let vp = Rect::new(0, 0, 200, row * 6);
    // Twenty times more content than the viewport shows: the bar is a real,
    // draggable thumb, not a full-track placeholder.
    let model = scroll_model(&browser, Scale::ONE, &theme, vp, BAND);
    assert!(model.range().is_scrollable());

    let bar_bounds = Rect::new(184, i32::try_from(row).unwrap(), 16, row * 5);
    let bar = ScrollBar::new(ScrollOrientation::Vertical, model);
    let geometry = bar
        .geometry(bar_bounds, Scale::ONE, &theme)
        .expect("a live bar");
    assert!(geometry.draggable());
    let top_thumb = geometry.thumb().start;

    // Scroll to the end; the drawn thumb moves to the bottom of its travel.
    turn(&mut browser, &theme, vp, DETENT * 1000);
    let bar = ScrollBar::new(
        ScrollOrientation::Vertical,
        scroll_model(&browser, Scale::ONE, &theme, vp, BAND),
    );
    let end_geometry = bar
        .geometry(bar_bounds, Scale::ONE, &theme)
        .expect("a live bar");
    assert!(end_geometry.thumb().start > top_thumb);
    assert_eq!(end_geometry.thumb().start, end_geometry.travel());
}

/// The pixels of `rows` of `surface`, `width` columns from its left edge — the
/// part of a frame one scroll offset shifts against another.
fn item_rows(
    surface: &Surface,
    width: u32,
    rows: core::ops::Range<u32>,
) -> Vec<Vec<Option<tairix_raster::Pixel>>> {
    rows.map(|y| (0..width).map(|x| surface.get(x, y)).collect())
        .collect()
}

/// A listing resting part-way through a row draws the row its top edge
/// crosses whole and cut there, and the one its foot crosses the same way:
/// every row of the item area is the unscrolled frame's row half a row
/// further down, so no row was squeezed into what shows and none was skipped.
#[test]
fn a_list_scrolled_part_way_draws_the_rows_its_edges_cut_whole() {
    use crate::render::{chrome_height, entry_index_at, item_area, row_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let row = row_height(Scale::ONE, &theme);
    let header = chrome_height(Scale::ONE, &theme, BAND);
    let mut browser = many_files(20);
    browser.select(0).expect("the first entry");
    let vp = rows_tall(&theme, 4);
    let half = row / 2;
    // The reference is taller by a row, so the row the scrolled frame's foot
    // cuts is laid out whole in it.
    let tall = Rect::new(0, 0, vp.width, vp.height + row);
    let chrome = crate::ManagerChrome::none();
    let reference = paint(&browser, Scale::ONE, &theme, tall, &chrome, &mut NoArtwork);
    browser.set_scroll_offset(u64::from(half));
    let scrolled = paint(&browser, Scale::ONE, &theme, vp, &chrome, &mut NoArtwork);

    let width = item_area(Scale::ONE, &theme, vp).width;
    assert_eq!(
        item_rows(&scrolled, width, header..vp.height),
        item_rows(&reference, width, header + half..vp.height + half),
        "every item row is the unscrolled one shifted up by the scroll"
    );
    assert_eq!(
        item_rows(&scrolled, vp.width, 0..header),
        item_rows(&reference, vp.width, 0..header),
        "and nothing was drawn over the chrome"
    );
    let at = |y: u32| {
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(4, i32::try_from(y).unwrap()),
        )
    };
    assert_eq!(
        at(header),
        Some(0),
        "the sliver of the first row is its own"
    );
    assert_eq!(at(header + half), Some(1));
    assert_eq!(
        at(vp.height - 1),
        Some(4),
        "the row the foot cuts is hit where it shows"
    );
}

/// The same for the grid: a view resting part-way through a line of tiles
/// draws the tiles either edge crosses whole and cut, and a press on the part
/// of a tile that shows finds it.
#[test]
fn a_grid_scrolled_part_way_draws_the_tiles_its_edges_cut_whole() {
    use crate::render::{chrome_height, entry_index_at, entry_rect, grid_metrics, item_area};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let header = chrome_height(Scale::ONE, &theme, BAND);
    let metrics = grid_metrics(Scale::ONE, &theme);
    let pitch = metrics.cell_height + metrics.gap;
    let mut browser = many_files(40);
    browser.set_view_mode(ViewMode::Grid);
    browser.select(0).expect("the first entry");
    // Two lines and a quarter of a tile: resting half a tile down, the foot
    // cuts the third line through its tiles, not through the gap after them.
    let vp = Rect::new(0, 0, 400, header + pitch * 2 + metrics.cell_height / 4);
    let tall = Rect::new(0, 0, vp.width, vp.height + pitch);
    let chrome = crate::ManagerChrome::none();
    let reference = paint(&browser, Scale::ONE, &theme, tall, &chrome, &mut NoArtwork);
    let offset = metrics.cell_height / 2;
    browser.set_scroll_offset(u64::from(offset));
    let scrolled = paint(&browser, Scale::ONE, &theme, vp, &chrome, &mut NoArtwork);

    let width = item_area(Scale::ONE, &theme, vp).width;
    assert_eq!(
        item_rows(&scrolled, width, header..vp.height),
        item_rows(&reference, width, header + offset..vp.height + offset),
        "every tile row is the unscrolled one shifted up by the scroll"
    );
    let first = entry_rect(&browser, Scale::ONE, &theme, vp, BAND, 0).expect("cut, not gone");
    assert_eq!(first.top(), i32::try_from(header).unwrap());
    assert_eq!(first.height, metrics.cell_height - offset);
    // The cut tile is hit on its name, which shows; the ground beside it is
    // not the tile.
    let across = i32::try_from(metrics.cell_width / 2).unwrap();
    let name = crate::render::TILE_LAYOUT
        .label_rect(
            Rect::new(0, 0, metrics.cell_width, metrics.cell_height),
            Scale::ONE,
            &theme,
        )
        .expect("a name band");
    let on_name = Point::new(
        first.left() + across,
        first.top() + name.top() + 2 - i32::try_from(offset).unwrap(),
    );
    assert_eq!(
        entry_index_at(&browser, Scale::ONE, &theme, vp, BAND, on_name),
        Some(0)
    );
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(first.left() + 1, first.top())
        ),
        None
    );
    // The third line of tiles shows only its head at the foot, and is hit
    // there all the same.
    let per_line = crate::render::visible_range(&browser, Scale::ONE, &theme, vp, BAND).len() / 3;
    let foot = entry_rect(&browser, Scale::ONE, &theme, vp, BAND, per_line * 2)
        .expect("the third line shows its head");
    assert_eq!(foot.bottom(), i32::try_from(vp.height).unwrap());
    assert!(foot.height < metrics.cell_height);
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(foot.left() + across, foot.bottom() - 1)
        ),
        Some(per_line * 2)
    );
}

/// A row past the whole rows a window holds is still a row: shown to the
/// foot, and hit where it shows.
#[test]
fn a_row_past_the_whole_rows_that_fit_is_hit_where_it_shows() {
    use crate::render::{chrome_height, entry_index_at, entry_rect, row_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let row = row_height(Scale::ONE, &theme);
    let header = chrome_height(Scale::ONE, &theme, BAND);
    let browser = many_files(20);
    let vp = Rect::new(0, 0, 200, header + row * 3 + row / 3);
    let partial = entry_rect(&browser, Scale::ONE, &theme, vp, BAND, 3).expect("shown");
    assert_eq!(partial.height, row / 3);
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(4, i32::try_from(header + row * 3).unwrap())
        ),
        Some(3)
    );
    assert_eq!(entry_rect(&browser, Scale::ONE, &theme, vp, BAND, 4), None);
}

/// Revealing a selection scrolls the least number of *pixels* that shows it
/// whole: a row below the fold rests its foot on the list's, one above rests
/// its head on the list's, and one already whole leaves the view alone.
#[test]
fn revealing_the_selection_scrolls_the_least_pixels_that_show_it_whole() {
    use crate::render::{chrome_height, entry_rect, reveal_selection, row_height};

    let theme = Theme::dark();
    let row = row_height(Scale::ONE, &theme);
    let header = chrome_height(Scale::ONE, &theme, BAND);
    let mut browser = many_files(20);
    let seen = row * 2 + row / 2;
    let vp = Rect::new(0, 0, 200, header + seen);
    browser.select(5).expect("selectable");
    reveal_selection(&mut browser, Scale::ONE, &theme, vp, BAND);
    assert_eq!(browser.scroll_offset(), u64::from(row * 6 - seen));
    let shown = entry_rect(&browser, Scale::ONE, &theme, vp, BAND, 5).expect("revealed");
    assert_eq!(shown.height, row, "whole, not cut");
    assert_eq!(shown.bottom(), i32::try_from(vp.height).unwrap());

    browser.select(4).expect("selectable");
    reveal_selection(&mut browser, Scale::ONE, &theme, vp, BAND);
    assert_eq!(
        browser.scroll_offset(),
        u64::from(row * 6 - seen),
        "already whole"
    );

    browser.select(2).expect("selectable");
    reveal_selection(&mut browser, Scale::ONE, &theme, vp, BAND);
    assert_eq!(browser.scroll_offset(), u64::from(row * 2));
}

#[test]
fn scrollbar_bounds_matches_the_reserved_gutter() {
    use crate::render::scrollbar_bounds;

    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 200, 200);
    let header = crate::render::chrome_height(Scale::ONE, &theme, BAND);
    let bounds = scrollbar_bounds(Scale::ONE, &theme, vp, BAND).expect("a gutter exists");
    // The bar sits in the reserved right-edge gutter (its right edge is the
    // window's right edge), below the chrome header, and is a real strip wide.
    assert_eq!(bounds.right(), i32::try_from(vp.width).unwrap());
    assert_eq!(bounds.top(), i32::try_from(header).unwrap());
    assert!(bounds.width > 0);
    assert!(bounds.left() > 0 && bounds.left() < i32::try_from(vp.width).unwrap());
    // A window too short for any item area has no gutter.
    assert!(scrollbar_bounds(Scale::ONE, &theme, Rect::new(0, 0, 200, header), BAND).is_none());
}

#[test]
fn scrollbar_click_on_the_increment_button_scrolls_down() {
    use crate::render::{row_height, scroll_pointer, scrollbar_bounds};
    use tairix_controls::damage::sink;
    use tairix_geometry::Point;
    use tairix_input::{InputEvent, PointerButton};

    let theme = Theme::dark();
    let mut browser = many_files(40);
    let row = row_height(Scale::ONE, &theme);
    let vp = Rect::new(
        0,
        0,
        200,
        crate::render::chrome_height(Scale::ONE, &theme, BAND) + row * 6,
    );
    let bounds = scrollbar_bounds(Scale::ONE, &theme, vp, BAND).expect("a gutter exists");
    let cx = bounds.left() + i32::try_from(bounds.width).unwrap() / 2;

    // A press on the increment (down) button at the bottom of the bar steps
    // the offset one line — the arrow button now scrolls the listing.
    let down = Point::new(cx, bounds.bottom() - 1);
    let press = InputEvent::PointerPressed {
        button: PointerButton::Primary,
    };
    assert_eq!(
        scroll_pointer(
            &mut browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            down,
            &press,
            &mut sink()
        ),
        Some(true)
    );
    assert_eq!(browser.scroll_offset(), u64::from(row), "one row a line");

    // A press away from the gutter is not the scrollbar's: it falls through to
    // the content (the helper reports it did not consume it).
    let off = Point::new(10, bounds.top() + 4);
    assert_eq!(
        scroll_pointer(
            &mut browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            off,
            &press,
            &mut sink()
        ),
        None
    );
}

#[test]
fn scrollbar_thumb_drag_scrolls_and_release_ends_the_capture() {
    use crate::render::{row_height, scroll_model, scroll_pointer, scrollbar_bounds};
    use tairix_controls::damage::sink;
    use tairix_controls::{ScrollBar, ScrollOrientation, ScrollPart};
    use tairix_geometry::Point;
    use tairix_input::{InputEvent, PointerButton};

    let theme = Theme::dark();
    let mut browser = many_files(60);
    let row = row_height(Scale::ONE, &theme);
    let vp = Rect::new(
        0,
        0,
        200,
        crate::render::chrome_height(Scale::ONE, &theme, BAND) + row * 6,
    );
    let bounds = scrollbar_bounds(Scale::ONE, &theme, vp, BAND).expect("a gutter exists");
    let cx = bounds.left() + i32::try_from(bounds.width).unwrap() / 2;

    // Find a point on the thumb using the same layout the router uses.
    let probe = ScrollBar::new(
        ScrollOrientation::Vertical,
        scroll_model(&browser, Scale::ONE, &theme, vp, BAND),
    );
    let thumb_y = (bounds.top()..bounds.bottom())
        .find(|&y| {
            probe.part_at(bounds, Point::new(cx, y), Scale::ONE, &theme) == ScrollPart::Thumb
        })
        .expect("the bar has a draggable thumb");

    // Press the thumb: the drag is captured but nothing has moved yet.
    let press = InputEvent::PointerPressed {
        button: PointerButton::Primary,
    };
    assert_eq!(
        scroll_pointer(
            &mut browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(cx, thumb_y),
            &press,
            &mut sink()
        ),
        Some(true)
    );
    assert_eq!(browser.scroll_offset(), 0);

    // Dragging the thumb toward the bottom scrolls the listing down.
    let to = Point::new(cx, bounds.bottom() - 2);
    let moved = InputEvent::PointerMoved { to };
    assert_eq!(
        scroll_pointer(
            &mut browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            to,
            &moved,
            &mut sink()
        ),
        Some(true)
    );
    assert!(browser.scroll_offset() > 0);

    // Releasing ends the capture; a later move off the bar is no longer the
    // scrollbar's (it reports it consumed nothing).
    let release = InputEvent::PointerReleased {
        button: PointerButton::Primary,
    };
    assert_eq!(
        scroll_pointer(
            &mut browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            to,
            &release,
            &mut sink()
        ),
        Some(true)
    );
    let off = InputEvent::PointerMoved {
        to: Point::new(10, bounds.top() + 3),
    };
    assert_eq!(
        scroll_pointer(
            &mut browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(10, bounds.top() + 3),
            &off,
            &mut sink()
        ),
        None
    );
}

#[test]
fn the_grid_view_renders_and_hit_tests_the_first_tile() {
    use crate::render::{entry_index_at, entry_rect};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let mut browser = many_files(20);
    browser.set_view_mode(ViewMode::Grid);
    browser.select(0).expect("the first entry");
    // A window wide and tall enough for several tiles.
    let vp = Rect::new(0, 0, 400, 400);
    let surface = paint(
        &browser,
        Scale::ONE,
        &theme,
        vp,
        &crate::ManagerChrome::none(),
        &mut NoArtwork,
    );
    assert_eq!(surface.width(), 400);

    // The row shares its leftover width out between its tiles, so the first
    // tile is asked where it is rather than assumed to hug the window's edge.
    let first =
        entry_rect(&browser, Scale::ONE, &theme, vp, BAND, 0).expect("the first tile is on screen");
    assert!(
        first.left() > 0,
        "the shared-out width reaches the row's leading end: {first:?}"
    );
    // A click on that tile's picture resolves to entry 0, and one on the
    // ground in its cell's corner to none.
    let core = crate::render::tile_core(Scale::ONE, &theme).center();
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(first.left() + core.x, first.top() + core.y)
        ),
        Some(0)
    );
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(first.left() + 1, first.top() + 1)
        ),
        None
    );
    // The margin before it belongs to no entry.
    assert_eq!(
        entry_index_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(0, first.top() + 1)
        ),
        None
    );
    // A click on the header resolves to nothing.
    assert_eq!(
        entry_index_at(&browser, Scale::ONE, &theme, vp, BAND, Point::new(4, 0)),
        None
    );
}

/// An artwork lookup that answers every request with one solid-colour square
/// and records what it was asked for, so a render can be proven to have gone
/// through the seam rather than straight to the built-in glyph.
struct RecordingArtwork {
    art: Surface,
    asked: Vec<(IconKind, u32)>,
}

impl RecordingArtwork {
    fn new(side: u32, color: Color) -> Self {
        let mut art = Surface::new(side, side).expect("artwork surface");
        art.fill(color);
        Self {
            art,
            asked: Vec::new(),
        }
    }
}

impl IconArtwork for RecordingArtwork {
    fn artwork(&mut self, request: IconRequest<'_>, side: u32) -> Option<IconPicture<'_>> {
        self.asked.push((request.icon_kind(), side));
        Some(IconPicture::coloured(&self.art))
    }
}

/// Whether `surface` shows `color` anywhere.
fn shows(surface: &Surface, color: Color) -> bool {
    let wanted = color.premultiply();
    (0..surface.height()).any(|y| (0..surface.width()).any(|x| surface.get(x, y) == Some(wanted)))
}

/// A frame painted a damaged part at a time draws the rail, the toolbar and
/// the scrollbar only for the parts that reach them, so a part inside the
/// listing asks the artwork for none of their icons.
#[test]
fn a_part_inside_the_listing_draws_none_of_the_chrome() {
    use crate::chrome::{ManagerToolModel, MANAGER_TOOLS};

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 400, 300);
    let places = Places::new(&home(), &[]);
    let mut browser = many_files(20);
    browser.set_view_mode(ViewMode::Grid);
    let chrome = crate::ManagerChrome {
        tools: MANAGER_TOOLS,
        tool_model: ManagerToolModel::new(true),
        sidebar: Some(&places),
        toolbar: BAND,
    };
    let colour = Color::rgb(255, 0, 255);
    let mut whole = RecordingArtwork::new(24, colour);
    paint(&browser, Scale::ONE, &theme, window, &chrome, &mut whole);
    assert!(
        whole.asked.iter().any(|(kind, _)| *kind != IconKind::File),
        "the whole frame draws the chrome's icons"
    );

    let mut part = RecordingArtwork::new(24, colour);
    let mut surface = Surface::new(window.width, window.height).expect("surface");
    surface.with_clip(300, 200, 40, 40, |surface| {
        crate::render_into(
            surface,
            &browser,
            Scale::ONE,
            &theme,
            window,
            &chrome,
            &mut part,
        );
    });
    assert!(
        part.asked.iter().all(|(kind, _)| *kind == IconKind::File),
        "{:?}",
        part.asked
    );
}

#[test]
fn the_grid_resolves_each_tile_through_the_artwork_lookup_and_draws_what_it_returns() {
    let theme = Theme::dark();
    let mut browser = many_files(20);
    browser.set_view_mode(ViewMode::Grid);
    let vp = Rect::new(0, 0, 400, 400);

    // The fixture's names carry no extension, so every tile classifies as the
    // generic content type and asks for that kind's artwork at the card's own
    // icon slot.
    let art_colour = Color::rgb(255, 0, 255);
    let mut artwork = RecordingArtwork::new(24, art_colour);
    let drawn = paint(
        &browser,
        Scale::ONE,
        &theme,
        vp,
        &crate::ManagerChrome::none(),
        &mut artwork,
    );
    assert!(!artwork.asked.is_empty(), "the grid consults the lookup");
    assert!(!content_kinds(&artwork.asked).is_empty());
    assert!(content_kinds(&artwork.asked)
        .iter()
        .all(|kind| *kind == IconKind::File));
    assert!(artwork.asked.iter().all(|(_, side)| *side > 0));
    assert!(
        shows(&drawn, art_colour),
        "the supplied artwork reaches the tile"
    );

    // Without a lookup the same grid draws the built-in glyph instead, so the
    // colour above can only have come through the seam.
    let plain = paint(
        &browser,
        Scale::ONE,
        &theme,
        vp,
        &crate::ManagerChrome::none(),
        &mut NoArtwork,
    );
    assert!(!shows(&plain, art_colour));
}

#[test]
fn the_list_view_resolves_each_rows_icon_through_the_artwork_lookup() {
    // A row's icon costs a cache lookup, never a rasterise: the built-in glyph
    // is the lookup's last tier, so a list of a hundred rows scrolling past
    // resolves coverage once per (kind, side) rather than once per row drawn.
    let theme = Theme::dark();
    let browser = many_files(20);
    let mut artwork = RecordingArtwork::new(24, Color::rgb(255, 0, 255));
    paint(
        &browser,
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 400, 400),
        &crate::ManagerChrome::none(),
        &mut artwork,
    );
    let asked = content_kinds(&artwork.asked);
    assert!(!asked.is_empty(), "the list consults the lookup");
    // The fixture's names carry no extension, so every row is the generic
    // content type — asked for by *class*, never by naming a bundle: a
    // row-height picture cannot show one application apart from another, and
    // reading each bundle's manifest to find out would be work the reader
    // never sees.
    assert!(asked.iter().all(|kind| *kind == IconKind::File));
    assert!(artwork.asked.iter().all(|(_, side)| *side > 0));
}

// --- The grid over the real shipped-artwork cache ------------------------
//
// The file manager draws its grid through the shared reclaim-governed
// `ArtworkCache` bound to a read seam and a *sandboxed* rasterise seam. These
// tests drive that exact composition with fakes for the two seams — no live
// sandbox — so the safety properties are host-proven: the fallback chain is
// total, a reply that cannot be believed is refused, and the cache decodes
// each `(asset, side)` once no matter how many tiles want it.

use alloc::boxed::Box;

use tairix_icon::{
    artwork_cache, icon_artwork_path, icon_vector_path, ArtworkCache, ArtworkRasteriser,
    ArtworkReader, IconArtworkSource, InlineArtwork, MAX_ARTWORK_BYTES,
};
use tairix_log::DiscardSink;
use tairix_reclaim::pressure::{PressureBand, ReportedPressure};

/// A reader over an in-memory asset table that records every path it was
/// asked for, so a test can prove which kinds were resolved and that a second
/// tile of the same kind was served from the cache rather than read again.
struct CountingReader {
    assets: BTreeMap<String, Vec<u8>>,
    read: Vec<String>,
}

impl CountingReader {
    fn new() -> Self {
        Self {
            assets: BTreeMap::new(),
            read: Vec::new(),
        }
    }

    /// Ship `bytes` as the raster master for `kind`, at the one shared asset
    /// path the desktop resolves that kind to.
    fn shipping(mut self, kind: IconKind, bytes: Vec<u8>) -> Self {
        self.assets.insert(icon_artwork_path(kind), bytes);
        self
    }

    /// Hold `bytes` at an exact path — a bundle's own manifest or the icon
    /// asset that manifest names.
    fn holding(mut self, path: &str, bytes: Vec<u8>) -> Self {
        self.assets.insert(path.to_string(), bytes);
        self
    }
}

impl ArtworkReader for CountingReader {
    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        self.read.push(path.to_string());
        self.assets.get(path).cloned()
    }
}

/// A rasteriser that answers with one solid opaque colour at the requested
/// side and counts every decode, standing in for the sandboxed worker.
struct CountingRasteriser {
    color: Color,
    decodes: usize,
}

impl CountingRasteriser {
    fn new(color: Color) -> Self {
        Self { color, decodes: 0 }
    }
}

impl ArtworkRasteriser for CountingRasteriser {
    fn rasterise(&mut self, side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
        self.decodes += 1;
        let pixel = [self.color.r, self.color.g, self.color.b, 0xff];
        Some(
            pixel
                .iter()
                .copied()
                .cycle()
                .take((side as usize) * (side as usize) * 4)
                .collect(),
        )
    }
}

/// A rasteriser whose reply is the wrong length, modelling a worker that
/// lies about the geometry it produced.
struct ShortRasteriser;

impl ArtworkRasteriser for ShortRasteriser {
    fn rasterise(&mut self, _side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
        Some(vec![0xff; 3])
    }
}

/// A rasteriser that must never run: the caller is required to refuse the
/// input before any decode happens.
struct PanicRasteriser;

impl ArtworkRasteriser for PanicRasteriser {
    fn rasterise(&mut self, _side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
        panic!("no byte of a missing or over-long asset may reach the decoder");
    }
}

/// The shared artwork cache wired as the file manager wires it, at a normal
/// pressure band so it retains what it decodes.
fn test_artwork_cache() -> ArtworkCache {
    let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
    gauge.report(PressureBand::Normal);
    let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
    artwork_cache("browse.test-artwork", 1, 1920 * 1080 * 4, gauge, sink)
}

/// Render `browser` into `vp` through the artwork lookup `artwork`.
fn grid_surface<S: DirectorySource>(
    browser: &Browser<S>,
    vp: Rect,
    artwork: &mut dyn IconArtwork,
) -> Surface {
    paint(
        browser,
        Scale::ONE,
        &Theme::dark(),
        vp,
        &crate::ManagerChrome::none(),
        artwork,
    )
}

/// The toolbar's own icons. Every window render resolves its chrome through the
/// same cache as its content, so a test about the content filters these out
/// rather than restating the toolbar's contents beside its own subject.
const CHROME_KINDS: &[IconKind] = &[
    IconKind::NavBack,
    IconKind::NavForward,
    IconKind::NavUp,
    IconKind::Refresh,
    IconKind::ViewToggle,
    IconKind::Sort,
];

/// `asked` with the window chrome's own icons dropped: what the *content*
/// asked for.
fn content_kinds(asked: &[(IconKind, u32)]) -> Vec<IconKind> {
    asked
        .iter()
        .map(|(kind, _)| *kind)
        .filter(|kind| !CHROME_KINDS.contains(kind))
        .collect()
}

/// `read` with the window chrome's own asset paths dropped.
fn content_reads(read: &[String]) -> Vec<String> {
    let chrome: Vec<String> = CHROME_KINDS
        .iter()
        .flat_map(|&kind| [icon_artwork_path(kind), icon_vector_path(kind)])
        .collect();
    read.iter()
        .filter(|path| !chrome.contains(path))
        .cloned()
        .collect()
}

/// A grid of `n` extension-less files, every one of them the generic content
/// type, so all tiles resolve to the same icon kind.
fn generic_grid(n: usize) -> Browser<MockFs> {
    let mut browser = many_files(n);
    browser.set_view_mode(ViewMode::Grid);
    browser
}

/// A grid holding one application bundle, so its tile resolves through the
/// bundle tier: the icon the bundle itself carries, then the class artwork,
/// then the glyph.
fn bundle_grid() -> Browser<MockFs> {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        vec![Entry::new(
            "Editor.app",
            EntryKind::Bundle,
            0,
            Time64::UNIX_EPOCH,
        )],
    );
    let mut browser = Browser::open_root(MockFs {
        dirs,
        denied: BTreeSet::new(),
        deny_after_first: BTreeSet::new(),
        reads: BTreeMap::new(),
        root_after_refresh: None,
    })
    .expect("root");
    browser.set_view_mode(ViewMode::Grid);
    browser
}

/// A grid whose first `png` entries are PNG images and whose last `txt`
/// entries are plain text, so the two halves resolve to different icon kinds
/// and sort into that order.
fn two_kind_grid(png: usize, txt: usize) -> Browser<MockFs> {
    let mut entries = Vec::new();
    for i in 0..png {
        entries.push(Entry::file(format!("a{i:03}.png")));
    }
    for i in 0..txt {
        entries.push(Entry::file(format!("z{i:03}.txt")));
    }
    let mut dirs = BTreeMap::new();
    dirs.insert("/".to_string(), entries);
    let mut browser = Browser::open_root(MockFs {
        dirs,
        denied: BTreeSet::new(),
        deny_after_first: BTreeSet::new(),
        reads: BTreeMap::new(),
        root_after_refresh: None,
    })
    .expect("root");
    browser.set_view_mode(ViewMode::Grid);
    browser
}

#[test]
fn a_grid_tile_blits_shipped_artwork_and_falls_back_to_the_glyph_without_it() {
    let browser = generic_grid(6);
    let vp = Rect::new(0, 0, 400, 400);
    let art_colour = Color::rgb(0x11, 0x22, 0x33);
    // The all-glyph frame every fallback case below must reproduce exactly.
    let glyphs = grid_surface(&browser, vp, &mut NoArtwork);

    // The system ships artwork for the tile's kind: the decoded pixels reach
    // the tile.
    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new().shipping(IconKind::File, vec![0xab; 64]);
    let mut rasteriser = CountingRasteriser::new(art_colour);
    let drawn = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );
    assert_eq!(rasteriser.decodes, 1);
    assert!(shows(&drawn, art_colour), "the shipped artwork is blitted");
    assert_ne!(drawn.pixels(), glyphs.pixels());

    // No asset on disk: nothing is decoded and the tile is the built-in glyph
    // — never a blank tile.
    let mut cache = test_artwork_cache();
    let mut absent = CountingReader::new();
    let missing = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut absent, &mut PanicRasteriser),
        ),
    );
    assert_eq!(missing.pixels(), glyphs.pixels());

    // An asset longer than the shared artwork ceiling is refused *before* the
    // decoder runs — `PanicRasteriser` would fire if a byte of it reached one
    // — and the tile is the glyph again.
    let mut cache = test_artwork_cache();
    let mut oversize =
        CountingReader::new().shipping(IconKind::File, vec![0u8; MAX_ARTWORK_BYTES + 1]);
    let refused = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut oversize, &mut PanicRasteriser),
        ),
    );
    assert_eq!(refused.pixels(), glyphs.pixels());
}

#[test]
fn a_bundle_tile_draws_the_icon_the_bundle_itself_carries() {
    let browser = bundle_grid();
    let vp = Rect::new(0, 0, 400, 400);
    let art_colour = Color::rgb(0x44, 0x55, 0x66);
    let glyphs = grid_surface(&browser, vp, &mut NoArtwork);

    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new()
        .holding(
            "/Editor.app/AppInfo",
            build_appinfo("Editor", Some("editor.png"), &[]),
        )
        .holding("/Editor.app/Resources/editor.png", vec![0xab; 64])
        // The class artwork is shipped too, so preferring the bundle's own
        // icon is a choice the resolver made rather than the only asset it
        // could find.
        .shipping(IconKind::AppBundle, vec![0xcd; 64]);
    let mut rasteriser = CountingRasteriser::new(art_colour);
    let drawn = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );

    assert!(
        shows(&drawn, art_colour),
        "the bundle's own icon is blitted"
    );
    assert_ne!(drawn.pixels(), glyphs.pixels());
    assert!(
        reader.read.iter().any(|p| p == "/Editor.app/AppInfo"),
        "the tile asks the bundle's own manifest which icon it carries: {:?}",
        reader.read
    );
    assert!(
        reader
            .read
            .iter()
            .any(|p| p == "/Editor.app/Resources/editor.png"),
        "the named asset is read from inside the bundle: {:?}",
        reader.read
    );
    assert!(
        !reader
            .read
            .iter()
            .any(|p| *p == icon_artwork_path(IconKind::AppBundle)),
        "the class artwork is not consulted when the bundle has its own: {:?}",
        reader.read
    );
    assert_eq!(rasteriser.decodes, 1);
}

#[test]
fn a_bundle_with_no_icon_of_its_own_falls_back_to_the_class_artwork_then_the_glyph() {
    let browser = bundle_grid();
    let vp = Rect::new(0, 0, 400, 400);
    let art_colour = Color::rgb(0x77, 0x11, 0x22);
    let glyphs = grid_surface(&browser, vp, &mut NoArtwork);

    // A manifest that names no icon: the shipped class artwork answers.
    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new()
        .holding("/Editor.app/AppInfo", build_appinfo("Editor", None, &[]))
        .shipping(IconKind::AppBundle, vec![0xcd; 64]);
    let mut rasteriser = CountingRasteriser::new(art_colour);
    let drawn = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );
    assert!(shows(&drawn, art_colour), "the class artwork is blitted");
    assert!(reader
        .read
        .iter()
        .any(|p| *p == icon_artwork_path(IconKind::AppBundle)));

    // Neither a manifest nor class artwork: the tile is the built-in glyph,
    // never a blank tile — and no byte ever reaches a decoder.
    let mut cache = test_artwork_cache();
    let mut bare = CountingReader::new();
    let plain = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut bare, &mut PanicRasteriser),
        ),
    );
    assert_eq!(plain.pixels(), glyphs.pixels());
}

#[test]
fn a_rasteriser_reply_of_the_wrong_length_is_refused_and_never_reaches_the_frame() {
    let browser = generic_grid(6);
    let vp = Rect::new(0, 0, 400, 400);
    let glyphs = grid_surface(&browser, vp, &mut NoArtwork);

    // The worker claims success but hands back three bytes where a full
    // square was promised: the reply is not believed, so the frame is the
    // glyph frame exactly — no partial blit, no torn tile.
    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new().shipping(IconKind::File, vec![0xab; 64]);
    let drawn = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut ShortRasteriser),
        ),
    );
    assert_eq!(drawn.width(), vp.width);
    assert_eq!(drawn.height(), vp.height);
    assert_eq!(drawn.pixels(), glyphs.pixels());
}

#[test]
fn a_hundred_tiles_of_one_kind_are_read_and_decoded_exactly_once() {
    let browser = generic_grid(100);
    let vp = Rect::new(0, 0, 400, 400);
    let art_colour = Color::rgb(0x11, 0x22, 0x33);

    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new().shipping(IconKind::File, vec![0xab; 64]);
    let mut rasteriser = CountingRasteriser::new(art_colour);
    let drawn = grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );
    assert!(shows(&drawn, art_colour));
    // Every tile shares one `(asset, side)` key, so a hundred-entry grid costs
    // one read of that asset and one decode, not a hundred of each. The
    // toolbar above the grid resolves its own icons through the same cache, so
    // the count is asserted for the *tiles'* asset rather than over the whole
    // window.
    let tile_asset = icon_artwork_path(IconKind::File);
    assert_eq!(
        reader
            .read
            .iter()
            .filter(|path| **path == tile_asset)
            .count(),
        1
    );

    // And the cost does not grow with the grid: a ten-tile grid of the same
    // kind reads and decodes exactly as much as the hundred-tile one.
    let mut small_reader = CountingReader::new().shipping(IconKind::File, vec![0xab; 64]);
    let mut small_rasteriser = CountingRasteriser::new(art_colour);
    let mut small_cache = test_artwork_cache();
    let _ = grid_surface(
        &generic_grid(10),
        vp,
        &mut IconArtworkSource::new(
            &mut small_cache,
            &mut InlineArtwork::new(&mut small_reader, &mut small_rasteriser),
        ),
    );
    assert_eq!(reader.read.len(), small_reader.read.len());
    assert_eq!(rasteriser.decodes, small_rasteriser.decodes);
}

#[test]
fn scrolling_to_new_entries_decodes_only_the_newly_visible_kinds() {
    let mut browser = two_kind_grid(40, 40);
    let vp = Rect::new(0, 0, 400, 400);
    let png = icon_artwork_path(IconKind::ImagePng);
    let text = icon_artwork_path(IconKind::Text);

    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new()
        .shipping(IconKind::ImagePng, vec![0xab; 64])
        .shipping(IconKind::Text, vec![0xcd; 64]);
    let mut rasteriser = CountingRasteriser::new(Color::rgb(0x11, 0x22, 0x33));

    // The first page is all images: the text kind is never touched, so a tile
    // scrolled out of view costs nothing.
    grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );
    assert_eq!(content_reads(&reader.read), core::slice::from_ref(&png));

    // Scroll to the end (the layout clamps the request to the last page):
    // only the kind that just became visible is read and decoded, and the
    // image artwork already held is not resolved again.
    browser.set_scroll_offset(u64::from(u32::MAX));
    grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );
    assert_eq!(content_reads(&reader.read), [png, text]);
    // The chrome's own icons were resolved on the first frame and are not
    // resolved again, so the only new decodes are the content's.
    assert_eq!(
        content_reads(&reader.read).len(),
        2,
        "the image artwork already held is not read a second time"
    );
}

#[test]
fn teardown_releases_the_retained_artwork() {
    let browser = generic_grid(6);
    let vp = Rect::new(0, 0, 400, 400);

    let mut cache = test_artwork_cache();
    let mut reader = CountingReader::new().shipping(IconKind::File, vec![0xab; 64]);
    let mut rasteriser = CountingRasteriser::new(Color::rgb(0x11, 0x22, 0x33));
    grid_surface(
        &browser,
        vp,
        &mut IconArtworkSource::new(
            &mut cache,
            &mut InlineArtwork::new(&mut reader, &mut rasteriser),
        ),
    );
    assert!(cache.charged_bytes() > 0, "the decode is retained");

    // Closing the window ends the cache: the decoded pixels are given back.
    cache.teardown();
    assert_eq!(cache.charged_bytes(), 0);
}

// --- FM4: navigation history and breadcrumb navigation -----------------

#[test]
fn descending_records_back_history_and_go_back_returns() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert!(!browser.can_go_back());
    assert!(!browser.can_go_forward());

    // Sorted root order is [Apps, Storage, System, Users]; System is index 2.
    browser.open_index(2).expect("enter System");
    assert_eq!(browser.path(), "/System");
    // The directory left behind is now on the back history.
    assert!(browser.can_go_back());
    assert!(!browser.can_go_forward());

    // Back returns to the root and offers the visited directory forward again.
    assert_eq!(browser.go_back(), Ok(true));
    assert_eq!(browser.path(), "/");
    assert!(browser.is_root());
    assert!(!browser.can_go_back());
    assert!(browser.can_go_forward());
    // The listing came back in the shared sorted order.
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);

    // Forward steps back into the directory we came from.
    assert_eq!(browser.go_forward(), Ok(true));
    assert_eq!(browser.path(), "/System");
    assert!(browser.can_go_back());
    assert!(!browser.can_go_forward());
}

#[test]
fn go_back_and_go_forward_are_no_ops_with_empty_history() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert_eq!(browser.go_back(), Ok(false));
    assert_eq!(browser.go_forward(), Ok(false));
    assert!(browser.is_root());
    assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
}

#[test]
fn go_up_records_history_like_any_navigation() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // Climbing up records /System so Back can return into it.
    assert_eq!(browser.go_up(), Ok(true));
    assert_eq!(browser.path(), "/");
    assert!(browser.can_go_back());
    assert_eq!(browser.go_back(), Ok(true));
    assert_eq!(browser.path(), "/System");
}

#[test]
fn a_fresh_navigation_clears_the_forward_history() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    assert_eq!(browser.go_back(), Ok(true));
    assert!(browser.can_go_forward());

    // A new descent (root → Users) abandons the forward branch, exactly as a
    // web browser's forward history is discarded when you take a new turn.
    browser.open_index(3).expect("enter Users");
    assert_eq!(browser.path(), "/Users");
    assert!(!browser.can_go_forward());
    assert!(browser.can_go_back());
    assert_eq!(browser.go_back(), Ok(true));
    assert_eq!(browser.path(), "/");
}

#[test]
fn go_back_is_transactional_when_the_previous_directory_becomes_unreadable() {
    // /System lists once (on the way in) and is refused on every later read,
    // modelling its capability being revoked while we are inside /System/Fonts.
    let mut fs = MockFs::fixture();
    fs.deny_after_first.insert("/System".to_string());
    let mut browser = Browser::open_root(fs).expect("root");
    browser.open_index(2).expect("enter System");
    browser.open_index(0).expect("enter Fonts");
    assert_eq!(browser.path(), "/System/Fonts");

    // Back to the now-unreadable /System fails closed: the browser and its
    // history are left exactly as they were, so Back can still be retried.
    assert_eq!(
        browser.go_back(),
        Err(BrowseError::Source(Errno::PermissionDenied))
    );
    assert_eq!(browser.path(), "/System/Fonts");
    assert!(browser.can_go_back());
    assert!(!browser.can_go_forward());
}

#[test]
fn navigation_history_is_bounded_and_drops_the_oldest() {
    use crate::browser::HISTORY_MAX;

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Alternate root → /System → root … more times than the history bound, so
    // the back stack is driven well past its cap. Each move records exactly
    // one location on the back stack (and clears forward, so forward stays
    // empty throughout the building phase).
    for _ in 0..(HISTORY_MAX + 50) {
        if browser.is_root() {
            browser.open_index(2).expect("enter System");
        } else {
            assert_eq!(browser.go_up(), Ok(true));
        }
    }

    // However far we drove it, the retained history is capped at HISTORY_MAX:
    // exactly that many Back steps succeed before it is exhausted, proving the
    // oldest locations were dropped rather than the stack growing unbounded.
    let mut steps = 0usize;
    while browser.go_back().expect("readable both ways") {
        steps += 1;
    }
    assert_eq!(steps, HISTORY_MAX);
    assert!(!browser.can_go_back());
}

// --- In-place rename (FM5) ---------------------------------------------
//
// The rename model is host-proven end to end over `MockFs`: the injected
// `rename` seam records the two paths it is asked to move between (or refuses),
// and `MockFs::root_after_refresh` supplies the post-rename listing the commit
// re-reads — so validation, the transactional VFS call, and the refresh all run
// without a kernel.

mod rename_model {
    use super::BAND;

    use tairix_abi::Errno;
    use tairix_geometry::{Rect, Scale};
    use tairix_theme::Theme;

    use super::{focused, names, MockFs};
    use crate::browser::Browser;
    use crate::entry::Entry;
    use crate::rename::{validate_new_name, RenameError};

    /// A `MockFs` whose root re-reads as `after` once a commit refreshes it.
    fn fs_with_refreshed_root(after: alloc::vec::Vec<Entry>) -> MockFs {
        let mut fs = MockFs::fixture();
        fs.root_after_refresh = Some(after);
        fs
    }

    #[test]
    fn commit_moves_the_selected_item_and_refreshes_onto_the_new_name() {
        // Root sorts to [Apps, Storage, System, Users]; rename Apps -> Downloads.
        let fs = fs_with_refreshed_root(alloc::vec![
            Entry::directory("Downloads"),
            Entry::directory("Storage"),
            Entry::directory("System"),
            Entry::directory("Users"),
        ]);
        let mut browser = Browser::open_root(fs).expect("root");
        browser.select(0).expect("select Apps");
        assert_eq!(focused(&browser).map(Entry::name), Some("Apps"));

        let pending = browser.prepare_rename("Downloads").expect("a valid rename");
        assert_eq!((pending.from(), pending.to()), ("/Apps", "/Downloads"));
        assert_eq!(browser.finish_rename(&pending, Ok(())), Ok(true));
        // The listing refreshed and the selection followed the entry.
        assert_eq!(names(&browser), ["Downloads", "Storage", "System", "Users"]);
        assert_eq!(focused(&browser).map(Entry::name), Some("Downloads"));
    }

    /// A rename answered after the view left its folder changes nothing here:
    /// following its new name in another folder could select an unrelated
    /// entry of that name.
    #[test]
    fn a_rename_answered_elsewhere_follows_nothing() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");
        let pending = browser.prepare_rename("Downloads").expect("a valid rename");
        browser.open_index(2).expect("enter System");
        let before = focused(&browser).map(|entry| alloc::string::String::from(entry.name()));
        assert_eq!(browser.finish_rename(&pending, Ok(())), Ok(false));
        assert_eq!(
            focused(&browser).map(|entry| alloc::string::String::from(entry.name())),
            before
        );
    }

    #[test]
    fn an_invalid_name_is_refused_before_any_syscall() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");

        for (name, expected) in [
            ("", RenameError::Empty),
            (".", RenameError::Reserved),
            ("..", RenameError::Reserved),
            ("a/b", RenameError::Separator),
            ("bad:name", RenameError::Invalid),
        ] {
            assert_eq!(browser.prepare_rename(name), Err(expected));
        }
        // The listing is untouched.
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
        assert_eq!(focused(&browser).map(Entry::name), Some("Apps"));
    }

    #[test]
    fn a_clash_with_an_existing_sibling_is_refused_before_any_syscall() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");
        assert_eq!(browser.prepare_rename("System"), Err(RenameError::Clash));
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    #[test]
    fn renaming_to_the_same_name_is_a_no_op() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");
        assert_eq!(browser.prepare_rename("Apps"), Err(RenameError::Unchanged));
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    #[test]
    fn a_vfs_refusal_is_surfaced_and_leaves_the_listing_unchanged() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");
        let pending = browser.prepare_rename("Downloads").expect("a valid rename");
        assert_eq!(
            browser.finish_rename(&pending, Err(Errno::PermissionDenied)),
            Err(RenameError::Refused(Errno::PermissionDenied))
        );
        // No refresh happened: the original listing and selection stand.
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
        assert_eq!(focused(&browser).map(Entry::name), Some("Apps"));
    }

    /// The volume decides a clash under its own rule — a sibling spelled
    /// differently in case only, where case is ignored — and refuses the
    /// no-replace move; that is a clash to the user, not an opaque refusal.
    #[test]
    fn a_name_the_volume_finds_taken_is_a_clash() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");
        let pending = browser.prepare_rename("STORAGE").expect("no exact clash");
        assert_eq!(
            browser.finish_rename(&pending, Err(Errno::AlreadyExists)),
            Err(RenameError::Clash)
        );
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    /// A re-spelling of the entry's own name is a real rename: the exact
    /// pre-check passes it to the volume, which renames the entry itself.
    #[test]
    fn a_re_spelling_of_the_entry_s_own_name_is_offered_to_the_volume() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(0).expect("select Apps");
        let pending = browser
            .prepare_rename("APPS")
            .expect("a case change is a change");
        assert!(pending.to().ends_with("/APPS"));
    }

    #[test]
    fn an_empty_directory_reports_no_selection() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        // Enter the empty /System/Fonts.
        browser.open_index(2).expect("enter System");
        browser
            .open_index(0)
            .expect("enter the empty Fonts directory");
        assert_eq!(focused(&browser).map(Entry::name), None);
        assert_eq!(browser.prepare_rename("x"), Err(RenameError::NoSelection));
    }

    #[test]
    fn validate_new_name_is_pure_and_covers_the_model_rules() {
        let siblings = alloc::vec![Entry::directory("Apps"), Entry::file("notes.txt")];
        assert_eq!(validate_new_name("Documents", "Apps", &siblings), Ok(()));
        assert_eq!(
            validate_new_name("Apps", "Apps", &siblings),
            Err(RenameError::Unchanged)
        );
        assert_eq!(
            validate_new_name("notes.txt", "Apps", &siblings),
            Err(RenameError::Clash)
        );
    }

    #[test]
    fn every_rename_error_has_a_nonempty_message() {
        for err in [
            RenameError::NoSelection,
            RenameError::Empty,
            RenameError::Reserved,
            RenameError::Separator,
            RenameError::Invalid,
            RenameError::TooLong,
            RenameError::Clash,
            RenameError::Unchanged,
            RenameError::Refused(Errno::PermissionDenied),
            RenameError::Source(Errno::NotFound),
        ] {
            assert!(!err.message().is_empty());
        }
    }

    #[test]
    fn entry_rect_locates_a_drawn_row_and_is_none_when_the_view_seats_nothing() {
        let theme = Theme::dark();
        let viewport = Rect::new(0, 0, 200, 200);

        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        let rect = crate::render::entry_rect(&browser, Scale::ONE, &theme, viewport, BAND, 1)
            .expect("a drawn row has a rectangle");
        // It lies within the window and below the one-row path-bar header.
        let header = crate::render::row_height(Scale::ONE, &theme);
        assert!(rect.origin.y >= i32::try_from(header).unwrap());
        assert!(rect.width > 0 && rect.height > 0);

        // The empty /System/Fonts seats no entry, hence no rectangle.
        browser.open_index(2).expect("enter System");
        browser.open_index(0).expect("enter Fonts");
        assert_eq!(
            crate::render::entry_rect(&browser, Scale::ONE, &theme, viewport, BAND, 0),
            None
        );
    }

    /// The in-place rename field goes over the item's **name**, in both views
    /// — not over the whole row (icon, name, size and date) or the whole tile
    /// (picture above the label).
    ///
    /// Regression: the editor was drawn at the item rectangle, so in the list
    /// view it covered the icon and both trailing columns and in the icon view
    /// it covered the picture.
    #[test]
    fn the_rename_field_sits_over_the_name_in_both_views() {
        use crate::layout::ViewMode;

        let theme = Theme::dark();
        let viewport = Rect::new(0, 0, 400, 300);
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        browser.select(1).expect("select the second entry");

        for view in [ViewMode::List, ViewMode::Grid] {
            browser.set_view_mode(view);
            let item = crate::render::entry_rect(&browser, Scale::ONE, &theme, viewport, BAND, 1)
                .expect("the selected item is drawn");
            let name =
                crate::render::selection_name_rect(&browser, Scale::ONE, &theme, viewport, BAND)
                    .expect("and so is its name");
            assert!(!name.is_empty(), "{view:?}: the field has somewhere to go");
            assert_eq!(
                name.intersection(&item),
                name,
                "{view:?}: the field stays inside the item it is renaming"
            );
            assert!(
                name.width < item.width || name.height < item.height,
                "{view:?}: the name is a part of the item, not the whole of it: \
                 {name:?} of {item:?}"
            );
        }

        // In the list view the name is the leading cell, so the field starts
        // past the icon and stops well before the size and date columns.
        browser.set_view_mode(ViewMode::List);
        let item = crate::render::entry_rect(&browser, Scale::ONE, &theme, viewport, BAND, 1)
            .expect("the row");
        let name = crate::render::selection_name_rect(&browser, Scale::ONE, &theme, viewport, BAND)
            .expect("the name cell");
        assert!(
            name.left() > item.left(),
            "the row's icon is not under the field"
        );
        assert!(
            name.right() < item.right(),
            "and neither are the size and date columns"
        );

        // In the icon view the name is the label band, so the field is below
        // the picture.
        browser.set_view_mode(ViewMode::Grid);
        let tile = crate::render::entry_rect(&browser, Scale::ONE, &theme, viewport, BAND, 1)
            .expect("the tile");
        let label =
            crate::render::selection_name_rect(&browser, Scale::ONE, &theme, viewport, BAND)
                .expect("the label band");
        assert!(
            label.top() > tile.top(),
            "the tile's picture is not under the field"
        );

        // Nothing selected, nothing to edit.
        browser.open_index(2).expect("enter System");
        browser.open_index(0).expect("enter the empty Fonts");
        assert_eq!(
            crate::render::selection_name_rect(&browser, Scale::ONE, &theme, viewport, BAND),
            None
        );
    }

    /// A press on an entry's name lands on its drawn text, in both views: the
    /// list row's name cell rather than its icon or trailing columns, and the
    /// tile's lines below its picture, each inside what a press on the entry
    /// hits at all.
    #[test]
    fn a_press_on_the_name_lands_on_its_drawn_text_in_both_views() {
        use crate::layout::ViewMode;

        let theme = Theme::dark();
        let viewport = Rect::new(0, 0, 400, 300);
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        for view in [ViewMode::List, ViewMode::Grid] {
            browser.set_view_mode(view);
            let target =
                crate::render::entry_target(&browser, Scale::ONE, &theme, viewport, BAND, 1)
                    .expect("the entry is pressable");
            let name =
                crate::render::entry_name_target(&browser, Scale::ONE, &theme, viewport, BAND, 1)
                    .expect("and so is its name");
            assert_eq!(
                name.intersection(&target),
                name,
                "{view:?}: {name:?} of {target:?}"
            );
            assert!(
                name.width < target.width || name.height < target.height,
                "{view:?}: the name is part of the entry: {name:?} of {target:?}"
            );
            let centre = tairix_geometry::Point::new(
                name.left() + i32::try_from(name.width / 2).expect("small"),
                name.top() + i32::try_from(name.height / 2).expect("small"),
            );
            assert_eq!(
                crate::render::entry_index_at(&browser, Scale::ONE, &theme, viewport, BAND, centre),
                Some(1),
                "{view:?}: a press on the name is a press on its entry"
            );
        }
        browser.set_view_mode(ViewMode::Grid);
        let tile = crate::render::entry_rect(&browser, Scale::ONE, &theme, viewport, BAND, 1)
            .expect("the tile");
        let name =
            crate::render::entry_name_target(&browser, Scale::ONE, &theme, viewport, BAND, 1)
                .expect("the tile's name");
        assert!(name.top() > tile.top(), "the picture is not the name");
        assert_eq!(
            crate::render::entry_name_target(&browser, Scale::ONE, &theme, viewport, BAND, 99),
            None,
            "no entry, no name"
        );
    }
}

// --- The New ▸ create model over the `MockFs` fixture -----------------------

mod create_model {
    use core::cell::RefCell;

    use alloc::string::ToString;

    use tairix_abi::Errno;

    use super::{focused, names, MockFs};
    use crate::browser::Browser;
    use crate::create::{validate_new_entry_name, CreateError};
    use crate::entry::Entry;

    /// A `MockFs` whose root re-reads as `after` once a commit refreshes it.
    fn fs_with_refreshed_root(after: alloc::vec::Vec<Entry>) -> MockFs {
        let mut fs = MockFs::fixture();
        fs.root_after_refresh = Some(after);
        fs
    }

    #[test]
    fn commit_creates_the_folder_and_follows_the_selection_onto_it() {
        // Root sorts to [Apps, Storage, System, Users]; create Downloads.
        let fs = fs_with_refreshed_root(alloc::vec![
            Entry::directory("Apps"),
            Entry::directory("Downloads"),
            Entry::directory("Storage"),
            Entry::directory("System"),
            Entry::directory("Users"),
        ]);
        let mut browser = Browser::open_root(fs).expect("root");

        let seen = RefCell::new(None);
        let result = browser.create_entry("Downloads", |path| {
            *seen.borrow_mut() = Some(path.to_string());
            Ok(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(*seen.borrow(), Some("/Downloads".to_string()));
        // The listing refreshed and the selection landed on the new folder,
        // ready for the app's inline rename.
        assert_eq!(
            names(&browser),
            ["Apps", "Downloads", "Storage", "System", "Users"]
        );
        assert_eq!(focused(&browser).map(Entry::name), Some("Downloads"));
    }

    #[test]
    fn an_invalid_name_is_refused_before_any_syscall() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        for (name, expected) in [
            ("", CreateError::Empty),
            (".", CreateError::Reserved),
            ("..", CreateError::Reserved),
            ("a/b", CreateError::Separator),
            ("bad:name", CreateError::Invalid),
        ] {
            let result = browser.create_entry(name, |_| {
                panic!("the VFS must not be touched for an invalid name");
            });
            assert_eq!(result, Err(expected));
        }
        // The listing is untouched.
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    #[test]
    fn a_clash_with_an_existing_sibling_is_refused_before_any_syscall() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        let result = browser.create_entry("System", |_| {
            panic!("a clashing create must not reach the VFS");
        });
        assert_eq!(result, Err(CreateError::Clash));
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    #[test]
    fn a_vfs_refusal_is_surfaced_and_leaves_the_listing_unchanged() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        let result = browser.create_entry("Downloads", |_| Err(Errno::PermissionDenied));
        assert_eq!(result, Err(CreateError::Refused(Errno::PermissionDenied)));
        // No refresh happened: the original listing stands.
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    #[test]
    fn a_name_the_volume_finds_taken_is_a_clash() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        let result = browser.create_entry("SYSTEM", |_| Err(Errno::AlreadyExists));
        assert_eq!(result, Err(CreateError::Clash));
        assert_eq!(names(&browser), ["Apps", "Storage", "System", "Users"]);
    }

    #[test]
    fn a_create_in_an_empty_directory_needs_no_selection() {
        let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
        // Enter the empty /System/Fonts — nothing is selected there.
        browser.open_index(2).expect("enter System");
        browser
            .open_index(0)
            .expect("enter the empty Fonts directory");
        assert_eq!(focused(&browser).map(Entry::name), None);

        let seen = RefCell::new(None);
        let result = browser.create_entry("New Folder", |path| {
            *seen.borrow_mut() = Some(path.to_string());
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(*seen.borrow(), Some("/System/Fonts/New Folder".to_string()));
    }

    #[test]
    fn a_failed_post_create_relist_is_surfaced() {
        let mut fs = MockFs::fixture();
        // The root lists once (open_root) and then refuses, modelling a
        // directory that becomes unreadable between the create and the refresh.
        fs.deny_after_first.insert("/".to_string());
        let mut browser = Browser::open_root(fs).expect("root");
        let result = browser.create_entry("Downloads", |_| Ok(()));
        assert_eq!(result, Err(CreateError::Source(Errno::PermissionDenied)));
    }

    #[test]
    fn validate_new_entry_name_is_pure_and_covers_the_model_rules() {
        let siblings = alloc::vec![Entry::directory("Apps"), Entry::file("notes.txt")];
        assert_eq!(validate_new_entry_name("Documents", &siblings), Ok(()));
        assert_eq!(
            validate_new_entry_name("Apps", &siblings),
            Err(CreateError::Clash)
        );
        assert_eq!(
            validate_new_entry_name("notes.txt", &siblings),
            Err(CreateError::Clash)
        );
        assert_eq!(
            validate_new_entry_name("", &siblings),
            Err(CreateError::Empty)
        );
        assert_eq!(
            validate_new_entry_name("..", &siblings),
            Err(CreateError::Reserved)
        );
    }

    #[test]
    fn every_create_error_has_a_nonempty_message() {
        for err in [
            CreateError::Empty,
            CreateError::Reserved,
            CreateError::Separator,
            CreateError::Invalid,
            CreateError::TooLong,
            CreateError::Clash,
            CreateError::Refused(Errno::PermissionDenied),
            CreateError::Source(Errno::NotFound),
        ] {
            assert!(!err.message().is_empty());
        }
    }
}

// --- FM8b: the permission-edit model (validate + commit a new mode) --------
//
// Every validation, transactional, and fail-closed branch of `set_mode` runs
// in `cargo test` without a kernel.

mod mode_edit_model {
    use core::cell::RefCell;

    use alloc::string::ToString;

    use tairix_abi::fs::FS_MODE_MASK;
    use tairix_abi::Errno;

    use crate::mode_edit::{set_mode, validate_mode, ModeError};

    #[test]
    fn commit_applies_the_mode_to_the_named_node() {
        let seen = RefCell::new(None);
        let result = set_mode("/Apps", 0o750, |path, mode| {
            *seen.borrow_mut() = Some((path.to_string(), mode));
            Ok(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(*seen.borrow(), Some(("/Apps".to_string(), 0o750)));
    }

    #[test]
    fn a_mode_carrying_a_bit_above_the_mask_is_refused_before_any_syscall() {
        // A file-type bit (above the 0o7777 permission word) is not settable.
        let result = set_mode("/Apps", FS_MODE_MASK + 1, |_, _| {
            panic!("the VFS must not be touched for an invalid mode");
        });
        assert_eq!(result, Err(ModeError::Invalid));
    }

    #[test]
    fn a_vfs_refusal_is_surfaced_and_the_node_is_unchanged() {
        let result = set_mode("/Apps", 0o644, |_, _| Err(Errno::PermissionDenied));
        assert_eq!(result, Err(ModeError::Refused(Errno::PermissionDenied)));
    }

    #[test]
    fn validate_mode_accepts_the_whole_mask_and_refuses_above_it() {
        // Every bit of the settable permission word is accepted, including the
        // setuid/setgid/sticky bits.
        assert_eq!(validate_mode(0), Ok(()));
        assert_eq!(validate_mode(FS_MODE_MASK), Ok(()));
        assert_eq!(validate_mode(0o755), Ok(()));
        // One bit above the mask fails closed, never masked into a lesser word.
        assert_eq!(validate_mode(FS_MODE_MASK + 1), Err(ModeError::Invalid));
        assert_eq!(validate_mode(0xFFFF_F000), Err(ModeError::Invalid));
    }

    #[test]
    fn every_mode_error_has_a_nonempty_message() {
        for err in [
            ModeError::Invalid,
            ModeError::Refused(Errno::PermissionDenied),
        ] {
            assert!(!err.message().is_empty());
        }
    }
}

// --- FM8b: the ownership-edit model (validate + commit a new owner) --------
//
// Every validation, transactional, and fail-closed branch of `set_owner`
// runs in `cargo test` without a kernel. The authority rule itself
// (`CAP_FS_CHOWN`, group membership, set-*id* strip) is the kernel's and is
// proven in `kernel/core`; here the engine only names the change and surfaces
// the seam's outcome.

mod owner_edit_model {
    use core::cell::RefCell;

    use alloc::string::ToString;

    use tairix_abi::fs::FS_OWNER_UNCHANGED;
    use tairix_abi::Errno;

    use crate::owner_edit::{set_owner, validate_owner, OwnerChange, OwnerError};

    #[test]
    fn commit_applies_the_owner_to_the_named_node() {
        let seen = RefCell::new(None);
        let result = set_owner(
            "/Apps",
            OwnerChange {
                uid: Some(1000),
                gid: Some(50),
            },
            |path, uid, gid| {
                *seen.borrow_mut() = Some((path.to_string(), uid, gid));
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(*seen.borrow(), Some(("/Apps".to_string(), 1000, 50)));
    }

    #[test]
    fn an_unchanged_field_marshals_the_sentinel() {
        let seen = RefCell::new(None);
        // Group-only change: the uid field must reach the seam as the
        // reserved "unchanged" sentinel, never a fabricated id.
        let result = set_owner("/Apps", OwnerChange::group(7), |_, uid, gid| {
            *seen.borrow_mut() = Some((uid, gid));
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(*seen.borrow(), Some((FS_OWNER_UNCHANGED, 7)));
    }

    #[test]
    fn a_sentinel_target_is_refused_before_any_syscall() {
        // The reserved sentinel is not a real id: naming it as a target is
        // refused rather than misread as "leave unchanged".
        let result = set_owner("/Apps", OwnerChange::user(FS_OWNER_UNCHANGED), |_, _, _| {
            panic!("the VFS must not be touched for an invalid id");
        });
        assert_eq!(result, Err(OwnerError::Invalid));
    }

    #[test]
    fn a_vfs_refusal_is_surfaced_and_the_node_is_unchanged() {
        // The missing-`CAP_FS_CHOWN` denial (or any VFS refusal) surfaces as
        // `Refused`, leaving the node as it was.
        let result = set_owner("/Apps", OwnerChange::user(0), |_, _, _| {
            Err(Errno::PermissionDenied)
        });
        assert_eq!(result, Err(OwnerError::Refused(Errno::PermissionDenied)));
    }

    #[test]
    fn validate_owner_accepts_real_ids_and_refuses_the_sentinel() {
        assert_eq!(validate_owner(OwnerChange::default()), Ok(()));
        assert_eq!(validate_owner(OwnerChange::user(0)), Ok(()));
        assert_eq!(validate_owner(OwnerChange::group(1000)), Ok(()));
        assert_eq!(
            validate_owner(OwnerChange {
                uid: Some(1),
                gid: Some(2)
            }),
            Ok(())
        );
        assert_eq!(
            validate_owner(OwnerChange::user(FS_OWNER_UNCHANGED)),
            Err(OwnerError::Invalid)
        );
        assert_eq!(
            validate_owner(OwnerChange::group(FS_OWNER_UNCHANGED)),
            Err(OwnerError::Invalid)
        );
    }

    #[test]
    fn owner_change_constructors_and_is_empty() {
        assert_eq!(OwnerChange::user(5).uid, Some(5));
        assert_eq!(OwnerChange::user(5).gid, None);
        assert_eq!(OwnerChange::group(9).gid, Some(9));
        assert_eq!(OwnerChange::group(9).uid, None);
        assert!(OwnerChange::default().is_empty());
        assert!(!OwnerChange::user(0).is_empty());
    }

    #[test]
    fn every_owner_error_has_a_nonempty_message() {
        for err in [
            OwnerError::Invalid,
            OwnerError::Refused(Errno::PermissionDenied),
        ] {
            assert!(!err.message().is_empty());
        }
    }
}

// --- FM6a: activating an entry (descend / launch a bundle / open a file) --

use crate::activate::{Activation, BundleIntent};

/// A tree source whose root holds a plain subdirectory, an application
/// bundle, and a regular file — the three activation kinds side by side.
fn activation_source() -> VfsDirectorySource<impl FnMut(&str) -> Result<Vec<u8>, Errno>, NoLinks> {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream(&[
            (b"Docs", FileKind::Directory),
            (b"Editor.app", FileKind::Directory),
            (b"notes.txt", FileKind::Regular),
        ]),
    );
    dirs.insert("/Docs".to_string(), encoded_stream(&[]));
    // A bundle is a real directory on disk, so the browse-intent activation
    // has something to list.
    dirs.insert(
        "/Editor.app".to_string(),
        encoded_stream(&[(b"AppInfo", FileKind::Regular), (b"Run", FileKind::Regular)]),
    );
    tree_source(dirs)
}

#[test]
fn activating_a_directory_descends_into_it() {
    let mut browser = Browser::open_root(activation_source()).expect("root");
    // Default order: the directory first, then the bundle and the file.
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Err(BrowseError::NoSuchEntry),
        "nothing is chosen until the user picks it"
    );
    browser.select(0).expect("select Docs");
    assert_eq!(focused(&browser).map(Entry::name), Some("Docs"));
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Ok(Activation::Descended)
    );
    // The engine performed the navigation itself: the listing changed.
    assert_eq!(browser.path(), "/Docs");
    assert!(!browser.is_root());
}

#[test]
fn activating_a_bundle_names_it_for_launch_without_descending() {
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(1).expect("select Editor.app");
    assert_eq!(focused(&browser).map(Entry::name), Some("Editor.app"));
    // A bundle is a sealed unit: the engine names it for the launcher and does
    // not descend — the browser stays exactly where it was.
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Ok(Activation::LaunchBundle {
            path: "/Editor.app".to_string()
        })
    );
    assert!(browser.is_root());
    assert_eq!(focused(&browser).map(Entry::name), Some("Editor.app"));
}

#[test]
fn browsing_a_bundle_descends_into_it_instead_of_launching_it() {
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(1).expect("select Editor.app");
    // Shift-activation: a bundle is also a directory, and this is the gesture
    // that says which the user meant.
    assert_eq!(
        browser.activate_selected(BundleIntent::Browse),
        Ok(Activation::Descended)
    );
    assert_eq!(browser.path(), "/Editor.app");
    assert!(!browser.is_root());
    // What is inside a bundle is what the user asked to see.
    let inside: Vec<&str> = browser.entries().iter().map(Entry::name).collect();
    assert_eq!(inside, ["AppInfo", "Run"]);
}

#[test]
fn the_browse_intent_changes_nothing_for_a_directory_or_a_file() {
    // Only a bundle is ambiguous; every other kind activates the same way
    // whether or not the modifier was held.
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(0).expect("select Docs");
    assert_eq!(
        browser.activate_selected(BundleIntent::Browse),
        Ok(Activation::Descended)
    );
    assert_eq!(browser.path(), "/Docs");

    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    assert_eq!(
        browser.activate_selected(BundleIntent::Browse),
        Ok(Activation::OpenFile {
            path: "/notes.txt".to_string()
        })
    );
    assert!(browser.is_root());
}

#[test]
fn browsing_an_unreadable_bundle_fails_closed_and_stays_put() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream(&[(b"Sealed.app", FileKind::Directory)]),
    );
    // `/Sealed.app` is deliberately absent from the tree, so listing it fails.
    let mut browser = Browser::open_root(tree_source(dirs)).expect("root");
    browser.select(0).expect("select Sealed.app");
    assert!(matches!(
        browser.activate_selected(BundleIntent::Browse),
        Err(BrowseError::Source(_))
    ));
    assert_eq!(browser.path(), "/");
    assert_eq!(focused(&browser).map(Entry::name), Some("Sealed.app"));
}

#[test]
fn activating_a_file_names_it_for_open_without_descending() {
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Ok(Activation::OpenFile {
            path: "/notes.txt".to_string()
        })
    );
    assert!(browser.is_root());
    assert_eq!(focused(&browser).map(Entry::name), Some("notes.txt"));
}

#[test]
fn activate_index_spells_a_nested_target_path() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream(&[(b"System", FileKind::Directory)]),
    );
    dirs.insert(
        "/System".to_string(),
        encoded_stream(&[(b"motd.txt", FileKind::Regular)]),
    );
    let mut browser = Browser::open_root(tree_source(dirs)).expect("root");
    browser.open_index(0).expect("enter /System");
    // The named target is spelled through the one shared path spelling, so it
    // reflects the current directory, not just the leaf name.
    assert_eq!(
        browser.activate_index(0, BundleIntent::Launch),
        Ok(Activation::OpenFile {
            path: "/System/motd.txt".to_string()
        })
    );
}

#[test]
fn activating_with_no_selection_is_refused() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Descend into the empty /System/Fonts, which has no selection.
    browser.open_index(2).expect("enter System");
    browser.open_index(0).expect("enter the empty Fonts");
    assert_eq!(focused(&browser).map(Entry::name), None);
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Err(BrowseError::NoSuchEntry)
    );
}

#[test]
fn activating_an_out_of_range_index_is_refused() {
    let mut browser = Browser::open_root(activation_source()).expect("root");
    assert_eq!(
        browser.activate_index(99, BundleIntent::Launch),
        Err(BrowseError::NoSuchEntry)
    );
    // The browser is untouched by the refused activation.
    assert!(browser.is_root());
}

#[test]
fn activating_an_unreadable_directory_fails_closed_and_stays_put() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // /System/Security exists but is capability-gated (unreadable).
    let names_before: Vec<String> = names(&browser).iter().map(ToString::to_string).collect();
    let security = browser
        .entries()
        .iter()
        .position(|e| e.name() == "Security")
        .expect("Security is listed");
    assert_eq!(
        browser.activate_index(security, BundleIntent::Launch),
        Err(BrowseError::Source(Errno::PermissionDenied))
    );
    // The descent failed before any state changed: the browser is still on
    // /System, showing the same entries.
    assert_eq!(browser.path(), "/System");
    assert_eq!(names(&browser), names_before);
}

// --- open_with: the "Open With…" type→bundle association model (FM6b) ---

use crate::open_with::{applications_for, AppAssociation};

/// The association model derives a file's type through the shared registry, so
/// the type a bundle is matched against is exactly the one the tile draws
/// (the registry's own mapping is proven in `media_tests.rs`).
#[test]
fn the_offered_type_is_the_registry_type() {
    for (name, media) in [
        ("notes.txt", MediaType::TextPlain),
        ("README.md", MediaType::TextMarkdown),
        ("data.json", MediaType::Json),
        ("photo.png", MediaType::ImagePng),
        ("tool.rxe", MediaType::TairixRxe),
    ] {
        let claimant = AppAssociation::new(
            "claimant",
            "/Apps/claimant.app",
            vec![media.as_str().to_string()],
        );
        let offered = applications_for(name, core::slice::from_ref(&claimant));
        assert_eq!(offered.len(), 1, "{name}");
    }
}

#[test]
fn a_file_the_registry_cannot_type_is_offered_nothing() {
    // An unrecognised extension, no extension at all, and a bare dotfile each
    // yield an honest empty answer rather than a guessed default — even from a
    // store whose bundle claims the generic type.
    let catch_all = AppAssociation::new(
        "catch-all",
        "/Apps/catch-all.app",
        vec!["application/octet-stream".to_string()],
    );
    let bundles = [catch_all];
    for name in ["mystery.xyz", "Makefile", ".profile", "archive.", ""] {
        assert!(applications_for(name, &bundles).is_empty(), "{name}");
    }
}

#[test]
fn handles_matches_a_declared_type_case_insensitively() {
    let assoc = AppAssociation::new(
        "viewer",
        "/System/Applications/viewer.app",
        vec!["text/plain".to_string(), "text/markdown".to_string()],
    );
    assert!(assoc.handles("text/plain"));
    assert!(assoc.handles("TEXT/PLAIN"));
    assert!(!assoc.handles("image/png"));
    assert_eq!(assoc.name(), "viewer");
    assert_eq!(assoc.bundle_path(), "/System/Applications/viewer.app");
    assert_eq!(assoc.mime_types(), ["text/plain", "text/markdown"]);
}

/// A text viewer, an image viewer, and a "studio" that claims both — the
/// shapes the match / bundle / none cases need.
fn open_with_store() -> Vec<AppAssociation> {
    vec![
        AppAssociation::new(
            "viewer",
            "/System/Applications/viewer.app",
            vec!["text/plain".to_string()],
        ),
        AppAssociation::new(
            "images",
            "/Apps/images.app",
            vec!["image/png".to_string(), "image/jpeg".to_string()],
        ),
        AppAssociation::new(
            "studio",
            "/Apps/studio.app",
            vec!["text/plain".to_string(), "image/png".to_string()],
        ),
    ]
}

#[test]
fn applications_for_offers_every_bundle_that_claims_the_type_in_order() {
    let bundles = open_with_store();
    // A text file is offered the text viewer and the studio, in enumeration
    // order — never the image-only bundle.
    let names: Vec<&str> = applications_for("notes.txt", &bundles)
        .iter()
        .map(|b| b.name())
        .collect();
    assert_eq!(names, ["viewer", "studio"]);
}

#[test]
fn applications_for_offers_a_single_matching_bundle() {
    let bundles = open_with_store();
    let matches = applications_for("scan.jpeg", &bundles);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name(), "images");
    assert_eq!(matches[0].bundle_path(), "/Apps/images.app");
}

#[test]
fn applications_for_is_empty_when_no_bundle_claims_a_known_type() {
    let bundles = open_with_store();
    // A recognised type (gzip archive) that no installed bundle handles is an
    // honest "no application" answer, not a fabricated default.
    assert!(applications_for("backup.tgz", &bundles).is_empty());
}

#[test]
fn new_offers_each_type_a_writing_application_declares_exactly() {
    use crate::open_with::blank_documents;

    let nouns = |bundles: &[AppAssociation]| -> Vec<&'static str> {
        blank_documents(bundles)
            .iter()
            .map(|document| document.noun())
            .collect()
    };
    let editor = |types: &[&str]| {
        AppAssociation::new(
            "editor",
            "/Apps/editor.app",
            types.iter().map(ToString::to_string).collect(),
        )
        .writing_documents()
    };
    // A viewer opens text but makes none.
    assert!(nouns(&open_with_store()).is_empty());
    assert_eq!(nouns(&[editor(&["text/plain"])]), ["Text Document"]);
    // Opening Rust source through the subclass chain is not declaring it.
    assert_eq!(
        nouns(&[editor(&["text/plain"]), editor(&["text/x-rust"])]),
        ["Text Document", "Rust Source File"]
    );
    // An empty file is not a picture, or JSON.
    assert!(nouns(&[editor(&["image/png", "application/json"])]).is_empty());
    // Registry order, once each, however the bundles declare them.
    assert_eq!(
        nouns(&[
            editor(&["TEXT/MARKDOWN", "text/plain"]),
            editor(&["text/plain"]),
        ]),
        ["Text Document", "Markdown Document"]
    );
}

#[test]
fn a_drop_copies_by_default_moves_with_shift_and_refuses_what_moves_nothing() {
    use crate::clipboard::{drop_operation, ClipboardOp};

    let path = |parts: &[&str]| -> Vec<String> { parts.iter().map(ToString::to_string).collect() };
    let items = [
        path(&["Users", "ann", "a.txt"]),
        path(&["Users", "ann", "Work"]),
    ];
    let elsewhere = path(&["Users", "ann", "Desktop"]);
    assert_eq!(
        drop_operation(&items, &elsewhere, false),
        Some(ClipboardOp::Copy)
    );
    assert_eq!(
        drop_operation(&items, &elsewhere, true),
        Some(ClipboardOp::Cut)
    );
    // Back into the folder they came from moves nothing.
    assert_eq!(
        drop_operation(&items, &path(&["Users", "ann"]), false),
        None
    );
    // A folder into itself, or anywhere inside it, is refused outright.
    assert_eq!(
        drop_operation(&items, &path(&["Users", "ann", "Work"]), false),
        None
    );
    assert_eq!(
        drop_operation(&items, &path(&["Users", "ann", "Work", "Deep"]), true),
        None
    );
    // A sibling whose name only starts like an item's is not inside it.
    assert_eq!(
        drop_operation(&items, &path(&["Users", "ann", "Workshop"]), false),
        Some(ClipboardOp::Copy)
    );
    // Nothing dragged, and the root, move nothing.
    assert_eq!(drop_operation(&[], &elsewhere, false), None);
    assert_eq!(drop_operation(&[Vec::new()], &elsewhere, false), None);
}

#[test]
fn a_drop_lands_in_the_folder_under_the_point_or_the_listing_s_own() {
    use crate::chrome::ToolbarBand;
    use crate::render::{drop_folder_at, entry_rect, listing_area};

    let theme = Theme::dark();
    let window = Rect::new(0, 0, 480, 320);
    let band = ToolbarBand::Shown;
    let browser = Browser::open_root(activation_source()).expect("root");
    let at = |index: usize| {
        entry_rect(&browser, Scale::ONE, &theme, window, band, index)
            .expect("the entry shows")
            .center()
    };
    let drop = |point| drop_folder_at(&browser, Scale::ONE, &theme, window, band, point);
    assert_eq!(
        drop(at(0)),
        Some((alloc::vec!["Docs".to_string()], Some(0))),
        "a folder takes the drop itself"
    );
    assert_eq!(
        drop(at(1)),
        Some((Vec::new(), None)),
        "a bundle is no folder to drop in"
    );
    assert_eq!(drop(at(2)), Some((Vec::new(), None)), "nor is a file");
    let area = listing_area(&browser, Scale::ONE, &theme, window, band);
    assert_eq!(
        drop(Point::new(area.left() + 4, area.bottom() - 2)),
        Some((Vec::new(), None)),
        "the ground below the entries is the listing's own folder"
    );
    assert_eq!(
        drop(Point::new(area.left() + 4, area.top() - 1)),
        None,
        "the toolbar takes none"
    );
}

#[test]
fn a_drop_mark_lights_one_listed_entry_until_the_listing_changes() {
    let mut browser = Browser::open_root(activation_source()).expect("root");
    assert_eq!(browser.set_drop_mark(Some(9)), None);
    assert_eq!(
        browser.drop_mark(),
        None,
        "an index past the listing lights nothing"
    );
    assert_eq!(browser.set_drop_mark(Some(0)), None);
    assert_eq!(browser.set_drop_mark(Some(0)), Some(0));
    assert_eq!(browser.drop_mark(), Some(0));
    browser.set_sort_mode(browser.sort_mode().next());
    assert_eq!(browser.drop_mark(), None, "a reorder puts it out");
    browser.set_drop_mark(Some(1));
    browser.refresh().expect("relisted");
    assert_eq!(browser.drop_mark(), None, "a relist puts it out");
}

#[test]
fn applications_for_is_empty_for_an_unrecognised_type() {
    let bundles = open_with_store();
    // The file's type cannot be derived, so nothing is offered even though
    // bundles exist.
    assert!(applications_for("mystery.xyz", &bundles).is_empty());
    assert!(applications_for("Makefile", &bundles).is_empty());
}

/// An application declaring only the broad `text/plain` type — a plain text
/// editor, the case the subclass chain exists for.
fn plain_text_editor() -> AppAssociation {
    AppAssociation::new("editor", "/Apps/editor.app", vec!["text/plain".to_string()])
}

#[test]
fn a_generic_text_application_opens_a_specific_text_file() {
    let bundles = [plain_text_editor()];
    for name in [
        "notes.txt",
        "main.rs",
        "install.sh",
        "parse.c",
        "parse.h",
        "README.md",
        "rows.csv",
        "data.json",
        "deploy.yaml",
        "layout.xml",
        "index.html",
        "Main.java",
        "logo.svg",
    ] {
        let offered = applications_for(name, &bundles);
        assert_eq!(offered.len(), 1, "{name}");
        assert_eq!(offered[0].name(), "editor", "{name}");
    }
}

#[test]
fn a_generic_text_application_is_not_offered_for_binary_content() {
    // The chain widens a type, it does not open everything: nothing binary
    // subclasses plain text.
    let bundles = [plain_text_editor()];
    for name in [
        "photo.png",
        "release.zip",
        "manual.pdf",
        "tool.rxe",
        "tile.spr",
    ] {
        assert!(applications_for(name, &bundles).is_empty(), "{name}");
    }
}

#[test]
fn a_specific_declaration_outranks_a_generic_one() {
    // The generic editor is enumerated first, so only specificity ranking can
    // put the Rust application ahead of it.
    let bundles = [
        plain_text_editor(),
        AppAssociation::new(
            "rustide",
            "/Apps/rustide.app",
            vec!["text/x-rust".to_string()],
        ),
    ];
    let offered: Vec<&str> = applications_for("main.rs", &bundles)
        .iter()
        .map(|b| b.name())
        .collect();
    assert_eq!(offered, ["rustide", "editor"]);
    // A file the specific application does not claim leaves the answer alone.
    let offered: Vec<&str> = applications_for("notes.txt", &bundles)
        .iter()
        .map(|b| b.name())
        .collect();
    assert_eq!(offered, ["editor"]);
}

#[test]
fn a_two_step_chain_ranks_each_ancestor_in_turn() {
    // An SVG is XML and XML is text, so all three are offered — nearest claim
    // first, whatever order the store enumerated them in.
    let bundles = [
        plain_text_editor(),
        AppAssociation::new(
            "xmltool",
            "/Apps/xmltool.app",
            vec!["application/xml".to_string()],
        ),
        AppAssociation::new("draw", "/Apps/draw.app", vec!["image/svg+xml".to_string()]),
    ];
    let offered: Vec<&str> = applications_for("logo.svg", &bundles)
        .iter()
        .map(|b| b.name())
        .collect();
    assert_eq!(offered, ["draw", "xmltool", "editor"]);
}

#[test]
fn a_bundle_claiming_both_a_type_and_its_ancestor_is_offered_once() {
    let bundles = [
        AppAssociation::new(
            "studio",
            "/Apps/studio.app",
            vec!["text/plain".to_string(), "text/x-rust".to_string()],
        ),
        plain_text_editor(),
    ];
    let offered: Vec<&str> = applications_for("main.rs", &bundles)
        .iter()
        .map(|b| b.name())
        .collect();
    assert_eq!(offered, ["studio", "editor"]);
}

/// Build a well-formed `AppInfo` wire image — the neutral shared header, then
/// the MIME table these tests are actually about.
///
/// The signature is left zero: [`association_from_manifest`] reads the declared
/// types as a display hint and never verifies one (the signed load gate does
/// that at launch), so an unsigned fixture exercises exactly the decode.
fn build_appinfo(name: &str, icon: Option<&str>, mimes: &[&str]) -> Vec<u8> {
    use tairix_abi::{manifest_header, MIME_ENTRY_LEN, MIME_TYPE_MAX};

    let mut header = manifest_header("os.tairix.fixture", name);
    if let Some(icon) = icon {
        header.library_icon_len = u8::try_from(icon.len()).expect("icon fits");
        header.library_icon[..icon.len()].copy_from_slice(icon.as_bytes());
    }
    header.mime_count = u16::try_from(mimes.len()).expect("mime count fits");
    let mut bytes = header.to_le_bytes().to_vec();
    for mime in mimes {
        let mut entry = [0u8; MIME_ENTRY_LEN];
        assert!(mime.len() <= MIME_TYPE_MAX);
        entry[0] = u8::try_from(mime.len()).expect("mime fits");
        entry[1..=mime.len()].copy_from_slice(mime.as_bytes());
        bytes.extend_from_slice(&entry);
    }
    bytes
}

/// Decode `bytes` and build the association for the bundle at `path`, the way
/// the shared store walk hands a bundle over.
fn association(path: &str, bytes: &[u8]) -> Option<crate::open_with::AppAssociation> {
    let header = tairix_abi::AppInfoHeader::from_bytes(bytes).ok()?;
    crate::open_with::association_from_manifest(path, &header, bytes)
}

#[test]
fn an_association_reads_the_bundle_name_and_its_declared_types() {
    let bytes = build_appinfo("viewer", None, &["text/plain", "text/markdown"]);
    let assoc = association("/System/Applications/viewer.app", &bytes).expect("decodes");
    assert_eq!(assoc.name(), "viewer");
    assert_eq!(assoc.bundle_path(), "/System/Applications/viewer.app");
    assert_eq!(assoc.mime_types(), ["text/plain", "text/markdown"]);
    // It composes with the matcher exactly as the mock store does.
    assert!(applications_for("notes.txt", core::slice::from_ref(&assoc))
        .iter()
        .any(|b| b.name() == "viewer"));
}

#[test]
fn an_editor_s_signed_claim_to_edit_reaches_its_association_and_candidate() {
    let viewer = association(
        "/System/Applications/view.app",
        &build_appinfo("view", None, &["text/plain"]),
    )
    .expect("decodes");
    assert!(
        !viewer.writes_documents(),
        "a manifest that says nothing edits nothing"
    );
    let mut bytes = build_appinfo("TextEdit", None, &["text/plain"]);
    let mut header = tairix_abi::AppInfoHeader::from_bytes(&bytes).expect("decodes");
    header.flags |= tairix_abi::APPINFO_FLAG_DOCUMENT_WRITE;
    bytes[..tairix_abi::AppInfoHeader::WIRE_LEN].copy_from_slice(&header.to_le_bytes());
    let editor = association("/System/Applications/TextEdit.app", &bytes).expect("decodes");
    assert!(editor.writes_documents());
    assert!(crate::open_with::OpenWithCandidate::of(&editor).writes_documents());
    assert!(!crate::open_with::OpenWithCandidate::of(&viewer).writes_documents());
}

#[test]
fn a_bundle_that_declares_no_types_is_never_an_open_with_candidate() {
    // A pure command declares no associations: it decodes to an empty MIME
    // set (never an error) and is simply never an "open with" candidate.
    let bytes = build_appinfo("printf", None, &[]);
    let assoc = association("/System/Commands/printf.app", &bytes).expect("decodes");
    assert!(assoc.mime_types().is_empty());
    assert!(applications_for("notes.txt", core::slice::from_ref(&assoc)).is_empty());
}

#[test]
fn an_association_fails_closed_on_a_mime_table_the_body_does_not_carry() {
    let mut bytes = build_appinfo("viewer", None, &["text/plain"]);
    let short = bytes.len() - 4;
    bytes.truncate(short);
    let header = tairix_abi::AppInfoHeader::from_bytes(&bytes).expect("the header still decodes");
    assert!(
        crate::open_with::association_from_manifest("/x.app", &header, &bytes).is_none(),
        "a header claiming an entry the body does not hold is skipped, never guessed"
    );
}

// ---------------------------------------------------------------------------
// FM7 — multi-selection and the cut/copy clipboard (pure engine model).
// ---------------------------------------------------------------------------

/// Build a root-first component path from string literals.
fn comps(parts: &[&str]) -> Vec<String> {
    parts.iter().copied().map(str::to_string).collect()
}

/// The selected indices, low-to-high, as a `Vec` for assertions.
fn selected(selection: &Selection) -> Vec<usize> {
    selection.iter().collect()
}

#[test]
fn selection_single_replaces_and_sets_the_anchor() {
    let mut s = Selection::new();
    s.single(3);
    assert_eq!(selected(&s), [3]);
    assert_eq!(s.anchor(), Some(3));
    s.single(1);
    assert_eq!(selected(&s), [1]);
    assert_eq!(s.anchor(), Some(1));
}

#[test]
fn selection_toggle_adds_then_removes_and_moves_the_anchor() {
    let mut s = Selection::new();
    s.toggle(2);
    assert!(s.contains(2));
    assert_eq!(s.anchor(), Some(2));
    s.toggle(4);
    assert_eq!(selected(&s), [2, 4]);
    assert_eq!(s.anchor(), Some(4));
    s.toggle(2);
    assert_eq!(selected(&s), [4]);
    // Un-selecting still moves the anchor to the acted-on entry.
    assert_eq!(s.anchor(), Some(2));
}

#[test]
fn selection_range_to_covers_both_directions_and_keeps_the_anchor() {
    let mut s = Selection::new();
    s.single(2);
    s.range_to(5);
    assert_eq!(selected(&s), [2, 3, 4, 5]);
    assert_eq!(s.anchor(), Some(2));
    // A second shift-click re-grows from the same anchor, replacing the range.
    s.range_to(0);
    assert_eq!(selected(&s), [0, 1, 2]);
    assert_eq!(s.anchor(), Some(2));
}

#[test]
fn selection_range_to_without_an_anchor_is_a_single_select() {
    let mut s = Selection::new();
    s.range_to(4);
    assert_eq!(selected(&s), [4]);
    assert_eq!(s.anchor(), Some(4));
}

#[test]
fn selection_select_all_selects_the_range_and_empty_stays_empty() {
    let mut s = Selection::new();
    s.select_all(3);
    assert_eq!(selected(&s), [0, 1, 2]);
    assert_eq!(s.anchor(), Some(0));
    s.select_all(0);
    assert!(s.is_empty());
    assert_eq!(s.anchor(), None);
}

#[test]
fn selection_clear_drops_everything() {
    let mut s = Selection::new();
    s.select_all(4);
    s.clear();
    assert!(s.is_empty());
    assert_eq!(s.anchor(), None);
}

/// A listing opens with nothing selected: the focus rests on the first entry
/// without choosing it, so no verb acts until the user picks something, and
/// the first arrow key picks the entry the focus rests on.
#[test]
fn a_listing_opens_with_nothing_selected() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    assert!(browser.selection().is_empty());
    assert_eq!(browser.focus_index(), Some(0));
    assert_eq!(browser.chosen_index(), None);
    browser.select_next();
    assert_eq!(selected(browser.selection()), [0]);
    browser.select_next();
    assert_eq!(selected(browser.selection()), [1]);
}

#[test]
fn select_all_selects_every_entry_in_the_listing() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select_all();
    assert_eq!(selected(browser.selection()), [0, 1, 2, 3]);
}

#[test]
fn toggle_and_extend_build_a_multi_selection() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select(0).expect("focus 0");
    browser.toggle_selection(2).expect("toggle 2");
    assert_eq!(selected(browser.selection()), [0, 2]);
    // Extend grows from the toggle's anchor (2) to 3.
    browser.extend_selection_to(3).expect("extend 3");
    assert_eq!(selected(browser.selection()), [2, 3]);
    assert_eq!(browser.focus_index(), Some(3));
}

#[test]
fn out_of_range_selection_ops_are_refused_and_change_nothing() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select(1).expect("focus 1");
    assert_eq!(browser.toggle_selection(99), Err(BrowseError::NoSuchEntry));
    assert_eq!(
        browser.extend_selection_to(99),
        Err(BrowseError::NoSuchEntry)
    );
    assert_eq!(selected(browser.selection()), [1]);
    assert_eq!(browser.focus_index(), Some(1));
}

#[test]
fn an_unmodified_move_collapses_a_multi_selection() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select_all();
    assert_eq!(browser.selection().len(), 4);
    browser.select_next();
    assert_eq!(selected(browser.selection()), [1]);
}

/// A fresh directory selects nothing: the old indices name nothing there.
#[test]
fn navigation_clears_the_selection() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select_all();
    // Enter /System (index 2 in the sorted root).
    browser.open_index(2).expect("enter System");
    assert_eq!(browser.path(), "/System");
    assert!(browser.selection().is_empty());
    assert_eq!(browser.focus_index(), Some(0));
}

/// A reorder carries the selection and the focus with the entries they named,
/// rather than with the positions those entries left.
#[test]
fn a_reorder_keeps_the_selection_on_its_entries() {
    use crate::sort::{SortDirection, SortKey, SortMode};
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.select(0).expect("first");
    browser.toggle_selection(2).expect("third");
    let chosen: BTreeSet<String> = browser
        .selection()
        .iter()
        .map(|i| browser.entries()[i].name().to_string())
        .collect();
    let focus = focused(&browser).map(|e| e.name().to_string());
    browser.set_sort_mode(SortMode {
        key: SortKey::Name,
        direction: SortDirection::Descending,
    });
    let after: BTreeSet<String> = browser
        .selection()
        .iter()
        .map(|i| browser.entries()[i].name().to_string())
        .collect();
    assert_eq!(after, chosen);
    assert_eq!(focused(&browser).map(|e| e.name().to_string()), focus);
}

#[test]
fn an_empty_directory_has_an_empty_selection() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // /System index 0 is Fonts, an empty directory.
    browser.open_index(0).expect("enter Fonts");
    assert_eq!(browser.path(), "/System/Fonts");
    assert!(browser.entries().is_empty());
    assert!(browser.selection().is_empty());
    assert!(browser.clipboard(ClipboardOp::Copy).is_none());
}

#[test]
fn clipboard_captures_the_selected_entries_absolute_paths() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // /System sorted: [Fonts, Security, Kernel]. Select Fonts and Kernel.
    browser.select(0).expect("focus Fonts");
    browser.toggle_selection(2).expect("also Kernel");
    let clipboard = browser.clipboard(ClipboardOp::Cut).expect("clipboard");
    assert_eq!(clipboard.op(), ClipboardOp::Cut);
    assert_eq!(
        clipboard.items(),
        &[comps(&["System", "Fonts"]), comps(&["System", "Kernel"])]
    );
}

#[test]
fn clipboard_is_none_when_nothing_is_selected() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.clear_selection();
    assert!(browser.selection().is_empty());
    assert!(browser.clipboard(ClipboardOp::Copy).is_none());
}

#[test]
fn clipboard_new_refuses_empty_or_root_items() {
    assert!(Clipboard::new(ClipboardOp::Copy, Vec::new()).is_none());
    // A root (empty component) item is not a real entry.
    assert!(Clipboard::new(ClipboardOp::Copy, vec![Vec::new()]).is_none());
    assert!(Clipboard::new(ClipboardOp::Copy, vec![comps(&["a"]), Vec::new()]).is_none());
    let ok = Clipboard::new(ClipboardOp::Copy, vec![comps(&["a"])]).expect("built");
    assert_eq!(ok.len(), 1);
    assert!(!ok.is_empty());
}

#[test]
fn plan_paste_maps_each_source_into_the_target() {
    let clipboard = Clipboard::new(
        ClipboardOp::Cut,
        vec![comps(&["Users", "alice"]), comps(&["System", "Kernel"])],
    )
    .expect("clipboard");
    let plan = plan_paste(&clipboard, &comps(&["Storage"])).expect("plan");
    assert_eq!(plan.op(), ClipboardOp::Cut);
    assert_eq!(plan.items().len(), 2);
    assert_eq!(
        plan.items()[0].source(),
        comps(&["Users", "alice"]).as_slice()
    );
    assert_eq!(
        plan.items()[0].dest(),
        comps(&["Storage", "alice"]).as_slice()
    );
    assert!(!plan.items()[0].overwrites_source());
    assert_eq!(
        plan.items()[1].dest(),
        comps(&["Storage", "Kernel"]).as_slice()
    );
}

#[test]
fn plan_paste_flags_a_paste_back_into_the_same_directory() {
    let clipboard =
        Clipboard::new(ClipboardOp::Copy, vec![comps(&["System", "Fonts"])]).expect("clipboard");
    // Paste into /System, where Fonts already lives.
    let plan = plan_paste(&clipboard, &comps(&["System"])).expect("plan");
    let item = &plan.items()[0];
    assert_eq!(item.dest(), comps(&["System", "Fonts"]).as_slice());
    assert!(item.overwrites_source());
}

#[test]
fn plan_paste_refuses_a_folder_into_itself_or_a_descendant() {
    let clipboard = Clipboard::new(ClipboardOp::Cut, vec![comps(&["System"])]).expect("clipboard");
    // Into itself.
    assert_eq!(
        plan_paste(&clipboard, &comps(&["System"])),
        Err(PasteError::WouldRecurse)
    );
    // Into a descendant.
    assert_eq!(
        plan_paste(&clipboard, &comps(&["System", "Fonts"])),
        Err(PasteError::WouldRecurse)
    );
    // A sibling prefix (`/Systematic`) is not a descendant.
    assert!(plan_paste(&clipboard, &comps(&["Systematic"])).is_ok());
}

#[test]
fn paste_error_message_is_non_empty() {
    assert!(!PasteError::WouldRecurse.to_string().is_empty());
}

// ---- FM7b: the delete model (`delete`) ----

/// The delete target whose leaf name is `name`, for order-independent
/// assertions (a `plan_delete` orders by listing, not by selection order).
fn delete_target<'a>(
    plan: &'a crate::delete::DeletePlan,
    name: &str,
) -> &'a crate::delete::DeleteTarget {
    plan.targets()
        .iter()
        .find(|t| t.name() == name)
        .expect("a target with that name")
}

#[test]
fn plan_delete_captures_the_selected_targets_and_their_kinds() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // /System sorted: [Fonts (dir), Security (dir), Kernel (file)]. Select the
    // directory Fonts and the file Kernel.
    browser.select(0).expect("focus Fonts");
    browser.toggle_selection(2).expect("also Kernel");

    let plan = browser.plan_delete().expect("a plan");
    assert_eq!(plan.len(), 2);
    assert!(!plan.is_empty());
    assert!(plan.has_directories());

    let fonts = delete_target(&plan, "Fonts");
    assert_eq!(fonts.path(), comps(&["System", "Fonts"]).as_slice());
    assert!(fonts.is_directory());

    let kernel = delete_target(&plan, "Kernel");
    assert_eq!(kernel.path(), comps(&["System", "Kernel"]).as_slice());
    assert!(!kernel.is_directory());
}

#[test]
fn plan_delete_is_none_when_nothing_is_selected() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.clear_selection();
    assert!(browser.selection().is_empty());
    assert!(browser.plan_delete().is_none());
}

#[test]
fn plan_delete_of_only_files_has_no_directories() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    // Select only the regular file Kernel (index 2).
    browser.select(2).expect("focus Kernel");
    let plan = browser.plan_delete().expect("a plan");
    assert_eq!(plan.len(), 1);
    assert!(!plan.has_directories());
    assert!(!plan.targets()[0].is_directory());
}

#[test]
fn plan_delete_marks_a_bundle_as_directory_backed() {
    use tairix_abi::time::Time64;
    let mut fs = MockFs::fixture();
    fs.dirs.insert(
        "/Apps".to_string(),
        vec![
            Entry::new(
                "Example.app",
                crate::entry::EntryKind::Bundle,
                0,
                Time64::UNIX_EPOCH,
            ),
            Entry::file("notes.txt"),
        ],
    );
    let mut browser = Browser::open_root(fs).expect("root");
    // Sorted root order is [Apps, Storage, System, Users]; Apps is index 0.
    browser.open_index(0).expect("enter Apps");
    browser.select_all();

    let plan = browser.plan_delete().expect("a plan");
    assert_eq!(plan.len(), 2);
    // A bundle is directory-backed on disk even though the browser does not
    // descend into it, so it is removed recursively as the directory it is.
    assert!(delete_target(&plan, "Example.app").is_directory());
    assert!(!delete_target(&plan, "notes.txt").is_directory());
    assert!(plan.has_directories());
}

#[test]
fn delete_plan_new_refuses_empty_or_root_targets() {
    // Nothing to delete.
    assert!(DeletePlan::new(Vec::new()).is_none());
    // A root (empty component) target could remove the root itself.
    assert!(DeletePlan::new(vec![(Vec::new(), true)]).is_none());
    // Any root target in the set poisons the whole plan (fail closed).
    assert!(DeletePlan::new(vec![(comps(&["a"]), false), (Vec::new(), true)]).is_none());

    let plan = DeletePlan::new(vec![(comps(&["System", "Kernel"]), false)]).expect("a valid plan");
    assert_eq!(plan.len(), 1);
    let target = &plan.targets()[0];
    assert_eq!(target.name(), "Kernel");
    assert_eq!(target.path(), comps(&["System", "Kernel"]).as_slice());
    assert!(!target.is_directory());
}

// ---- FM7b: the recursive-delete execution model (`DeleteWalk`) ----

/// Drive a [`DeleteWalk`] to completion against an in-memory tree keyed by
/// absolute-path spelling → children `(name, is_directory)`, returning the
/// absolute-path spelling of every node in the order it was removed.
///
/// A directory absent from `tree` lists as empty. Every `expand`/
/// `complete_removal` must succeed — a walk driven strictly in step never
/// errors — so any protocol slip is a test failure, not a swallowed result.
fn drive_delete(plan: &DeletePlan, tree: &BTreeMap<String, Vec<(String, bool)>>) -> Vec<String> {
    let mut walk = DeleteWalk::from_plan(plan);
    let mut order = Vec::new();
    // A generous ceiling so a modelling bug (a walk that never completes) fails
    // the test rather than looping forever.
    for _ in 0..10_000 {
        match walk.next_action() {
            None => return order,
            Some(DeleteAction::List(path)) => {
                let children = tree.get(&key(path)).cloned().unwrap_or_default();
                walk.expand(&children).expect("expand in step");
            }
            Some(DeleteAction::Remove { path, .. }) => {
                order.push(key(path));
                walk.complete_removal().expect("remove in step");
            }
        }
    }
    panic!("delete walk did not complete within the step ceiling");
}

#[test]
fn delete_walk_removes_a_single_file() {
    let plan = DeletePlan::new(vec![(comps(&["System", "Kernel"]), false)]).expect("plan");
    let mut walk = DeleteWalk::from_plan(&plan);

    assert!(!walk.is_complete());
    assert_eq!(walk.removed(), 0);
    match walk.next_action() {
        Some(DeleteAction::Remove { path, is_directory }) => {
            assert_eq!(path, comps(&["System", "Kernel"]).as_slice());
            assert!(!is_directory);
        }
        other => panic!("expected a Remove, got {other:?}"),
    }
    walk.complete_removal().expect("remove the file");
    assert!(walk.is_complete());
    assert_eq!(walk.removed(), 1);
    assert!(walk.next_action().is_none());
}

#[test]
fn delete_walk_lists_an_empty_directory_then_removes_it() {
    let plan = DeletePlan::new(vec![(comps(&["Storage", "empty"]), true)]).expect("plan");
    let mut walk = DeleteWalk::from_plan(&plan);

    // A directory is always listed first, even when it turns out empty.
    match walk.next_action() {
        Some(DeleteAction::List(path)) => {
            assert_eq!(path, comps(&["Storage", "empty"]).as_slice());
        }
        other => panic!("expected a List, got {other:?}"),
    }
    walk.expand(&[]).expect("expand empty");
    // Now the emptied directory is removed as a leaf.
    match walk.next_action() {
        Some(DeleteAction::Remove { path, is_directory }) => {
            assert_eq!(path, comps(&["Storage", "empty"]).as_slice());
            assert!(is_directory);
        }
        other => panic!("expected a Remove, got {other:?}"),
    }
    walk.complete_removal().expect("remove the directory");
    assert!(walk.is_complete());
    assert_eq!(walk.removed(), 1);
}

#[test]
fn delete_walk_removes_contents_before_the_directory_depth_first() {
    // /Storage/tree/{a.txt, sub/{b.txt}}, listed in that order.
    let mut tree: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    tree.insert(
        key(&comps(&["Storage", "tree"])),
        vec![("a.txt".to_string(), false), ("sub".to_string(), true)],
    );
    tree.insert(
        key(&comps(&["Storage", "tree", "sub"])),
        vec![("b.txt".to_string(), false)],
    );

    let plan = DeletePlan::new(vec![(comps(&["Storage", "tree"]), true)]).expect("plan");
    let order = drive_delete(&plan, &tree);

    // Contents before their container, listing order among siblings, and the
    // subtree fully removed before we come back up to the parent.
    assert_eq!(
        order,
        vec![
            key(&comps(&["Storage", "tree", "a.txt"])),
            key(&comps(&["Storage", "tree", "sub", "b.txt"])),
            key(&comps(&["Storage", "tree", "sub"])),
            key(&comps(&["Storage", "tree"])),
        ]
    );
}

#[test]
fn delete_walk_processes_multiple_targets_in_listing_order() {
    let mut tree: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    tree.insert(
        key(&comps(&["Users", "dir"])),
        vec![("inner".to_string(), false)],
    );

    // A file, then a directory, then another file — the plan's listing order.
    let plan = DeletePlan::new(vec![
        (comps(&["Users", "first.txt"]), false),
        (comps(&["Users", "dir"]), true),
        (comps(&["Users", "last.txt"]), false),
    ])
    .expect("plan");
    let order = drive_delete(&plan, &tree);

    assert_eq!(
        order,
        vec![
            key(&comps(&["Users", "first.txt"])),
            key(&comps(&["Users", "dir", "inner"])),
            key(&comps(&["Users", "dir"])),
            key(&comps(&["Users", "last.txt"])),
        ]
    );
}

#[test]
fn delete_walk_expand_refuses_a_tree_deeper_than_the_bound() {
    // A directory target that already sits at the maximum depth: expanding it
    // would name a child one component deeper than the bound.
    let deep: Vec<String> = (0..MAX_DELETE_DEPTH).map(|i| format!("d{i}")).collect();
    assert_eq!(deep.len(), MAX_DELETE_DEPTH);
    let plan = DeletePlan::new(vec![(deep, true)]).expect("plan");
    let mut walk = DeleteWalk::from_plan(&plan);

    assert!(matches!(walk.next_action(), Some(DeleteAction::List(_))));
    assert_eq!(
        walk.expand(&[("child".to_string(), false)]),
        Err(DeleteError::TooDeep)
    );
    // Refused, and the walk is left exactly where it was (fail closed): still a
    // List of the same directory, nothing removed.
    assert!(matches!(walk.next_action(), Some(DeleteAction::List(_))));
    assert_eq!(walk.removed(), 0);
}

#[test]
fn delete_walk_fails_closed_when_driven_out_of_step() {
    // `expand` on a leaf file is out of step.
    let file_plan = DeletePlan::new(vec![(comps(&["System", "Kernel"]), false)]).expect("plan");
    let mut walk = DeleteWalk::from_plan(&file_plan);
    assert!(matches!(
        walk.next_action(),
        Some(DeleteAction::Remove { .. })
    ));
    assert_eq!(walk.expand(&[]), Err(DeleteError::OutOfStep));
    // Unchanged.
    assert!(matches!(
        walk.next_action(),
        Some(DeleteAction::Remove { .. })
    ));

    // `complete_removal` on a directory whose contents were not listed is out
    // of step.
    let dir_plan = DeletePlan::new(vec![(comps(&["Storage", "d"]), true)]).expect("plan");
    let mut walk = DeleteWalk::from_plan(&dir_plan);
    assert!(matches!(walk.next_action(), Some(DeleteAction::List(_))));
    assert_eq!(walk.complete_removal(), Err(DeleteError::OutOfStep));
    assert!(matches!(walk.next_action(), Some(DeleteAction::List(_))));

    // Either driver call on a finished walk is out of step.
    walk.expand(&[]).expect("empty the directory");
    walk.complete_removal().expect("remove it");
    assert!(walk.is_complete());
    assert_eq!(walk.expand(&[]), Err(DeleteError::OutOfStep));
    assert_eq!(walk.complete_removal(), Err(DeleteError::OutOfStep));
}

#[test]
fn delete_walk_holds_its_position_across_an_interruption() {
    let mut tree: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    tree.insert(
        key(&comps(&["Users", "dir"])),
        vec![("x".to_string(), false), ("y".to_string(), false)],
    );
    let plan = DeletePlan::new(vec![(comps(&["Users", "dir"]), true)]).expect("plan");
    let mut walk = DeleteWalk::from_plan(&plan);

    // List, then remove exactly one child, then "stop" (as a Cancel or a
    // preemption would) holding the walk.
    let list = walk.next_action().expect("a step");
    assert!(matches!(list, DeleteAction::List(_)));
    walk.expand(&[("x".to_string(), false), ("y".to_string(), false)])
        .expect("expand");
    if let Some(DeleteAction::Remove { .. }) = walk.next_action() {
        walk.complete_removal().expect("remove x");
    }
    assert_eq!(walk.removed(), 1);
    assert!(!walk.is_complete());

    // Resuming from exactly here removes the remaining child then the directory
    // — no repeat of the child already removed, no skip.
    let mut order = Vec::new();
    while let Some(action) = walk.next_action() {
        match action {
            DeleteAction::List(path) => {
                let children = tree.get(&key(path)).cloned().unwrap_or_default();
                walk.expand(&children).expect("expand");
            }
            DeleteAction::Remove { path, .. } => {
                order.push(key(path));
                walk.complete_removal().expect("remove");
            }
        }
    }
    assert_eq!(
        order,
        vec![
            key(&comps(&["Users", "dir", "y"])),
            key(&comps(&["Users", "dir"])),
        ]
    );
    assert_eq!(walk.removed(), 3);
}

#[test]
fn delete_error_messages_are_non_empty() {
    assert!(!DeleteError::TooDeep.to_string().is_empty());
    assert!(!DeleteError::OutOfStep.to_string().is_empty());
}

// ---- FM7b: the paste-execution model (`execute`) ----

/// Two distinct volume ids so the move-vs-copy tests read clearly.
fn vol(tag: u8) -> VolumeId {
    let mut bytes = [0u8; 16];
    bytes[0] = tag;
    VolumeId::new(bytes)
}

#[test]
fn volume_id_round_trips_its_bytes_and_compares() {
    let bytes = *b"0123456789abcdef";
    assert_eq!(VolumeId::new(bytes).bytes(), bytes);
    assert_eq!(vol(1), vol(1));
    assert_ne!(vol(1), vol(2));
}

#[test]
fn a_copy_always_streams_regardless_of_volume() {
    assert_eq!(
        paste_strategy(ClipboardOp::Copy, vol(1), vol(1)),
        PasteStrategy::Copy
    );
    assert_eq!(
        paste_strategy(ClipboardOp::Copy, vol(1), vol(2)),
        PasteStrategy::Copy
    );
}

#[test]
fn a_cut_within_one_volume_renames() {
    assert_eq!(
        paste_strategy(ClipboardOp::Cut, vol(7), vol(7)),
        PasteStrategy::Rename
    );
}

#[test]
fn a_cut_across_volumes_copies_then_deletes() {
    assert_eq!(
        paste_strategy(ClipboardOp::Cut, vol(1), vol(2)),
        PasteStrategy::CopyThenDelete
    );
}

#[test]
fn an_empty_source_needs_no_chunk_and_is_complete() {
    let cursor = CopyCursor::new(0);
    assert!(cursor.is_complete());
    assert_eq!(cursor.remaining(), 0);
    assert_eq!(cursor.next_chunk(), None);
}

#[test]
fn a_small_source_is_one_short_chunk() {
    let cursor = CopyCursor::new(10);
    let chunk = cursor.next_chunk().expect("a chunk");
    assert_eq!(chunk.offset(), 0);
    assert_eq!(chunk.len(), 10);
    assert!(!chunk.is_empty());
}

#[test]
fn a_large_source_is_walked_in_bounded_chunks_to_completion() {
    let total = COPY_CHUNK_LEN * 2 + 5;
    let mut cursor = CopyCursor::new(total);

    let first = cursor.next_chunk().expect("first");
    assert_eq!(first.offset(), 0);
    assert_eq!(first.len(), COPY_CHUNK_LEN);
    cursor.advance(first.len()).expect("advance first");

    let second = cursor.next_chunk().expect("second");
    assert_eq!(second.offset(), COPY_CHUNK_LEN);
    assert_eq!(second.len(), COPY_CHUNK_LEN);
    cursor.advance(second.len()).expect("advance second");

    let third = cursor.next_chunk().expect("third");
    assert_eq!(third.offset(), COPY_CHUNK_LEN * 2);
    assert_eq!(third.len(), 5);
    cursor.advance(third.len()).expect("advance third");

    assert!(cursor.is_complete());
    assert_eq!(cursor.copied(), total);
    assert_eq!(cursor.next_chunk(), None);
}

#[test]
fn a_short_transfer_advances_only_by_what_moved() {
    let mut cursor = CopyCursor::new(10);
    // The read carried 4 of the 10 bytes it was asked for.
    cursor.advance(4).expect("short advance");
    assert_eq!(cursor.copied(), 4);
    let next = cursor.next_chunk().expect("remainder");
    assert_eq!(next.offset(), 4);
    assert_eq!(next.len(), 6);
    cursor.advance(6).expect("finish");
    assert!(cursor.is_complete());
}

#[test]
fn a_cursor_resumes_from_a_persisted_offset() {
    let mut cursor = CopyCursor::resume(100, 40).expect("resume");
    assert_eq!(cursor.copied(), 40);
    assert_eq!(cursor.remaining(), 60);
    let chunk = cursor.next_chunk().expect("chunk");
    assert_eq!(chunk.offset(), 40);
    assert_eq!(chunk.len(), 60);
    cursor.advance(60).expect("finish");
    assert!(cursor.is_complete());
}

#[test]
fn resuming_past_the_total_is_overrun() {
    assert_eq!(CopyCursor::resume(10, 11), Err(CopyError::Overrun));
    // Resuming exactly at the end is a complete, valid cursor.
    let done = CopyCursor::resume(10, 10).expect("at end");
    assert!(done.is_complete());
    assert_eq!(done.next_chunk(), None);
}

#[test]
fn advancing_past_the_source_length_fails_closed() {
    let mut cursor = CopyCursor::new(10);
    assert_eq!(cursor.advance(11), Err(CopyError::Overrun));
    // The cursor is left untouched by the refused advance.
    assert_eq!(cursor.copied(), 0);
    // A valid advance to the exact end still works afterwards.
    cursor.advance(10).expect("advance to end");
    assert!(cursor.is_complete());
    assert_eq!(cursor.advance(1), Err(CopyError::Overrun));
}

#[test]
fn copy_error_message_is_non_empty() {
    assert!(!CopyError::Overrun.to_string().is_empty());
}

// ---- FM7b: the recursive-copy execution model (`CopyWalk`) ----

/// Drive a [`CopyWalk`] to completion against an in-memory *source* tree keyed
/// by absolute-path spelling → children `(name, kind)`, returning the
/// absolute *destination*-path spelling of every node in the order the copy
/// performed it (a directory when created, a file when copied).
///
/// A source directory absent from `tree` lists as empty. Every driver call
/// must succeed — a walk driven strictly in step never errors — so any protocol
/// slip is a test failure, not a swallowed result.
fn drive_copy(
    walk: &mut CopyWalk,
    tree: &BTreeMap<String, Vec<(String, CopyKind)>>,
) -> Vec<String> {
    let mut order = Vec::new();
    // A generous ceiling so a modelling bug (a walk that never completes) fails
    // the test rather than looping forever.
    for _ in 0..10_000 {
        match walk.next_action() {
            None => return order,
            Some(CopyAction::MakeDir { dest }) => {
                order.push(key(dest));
                walk.created().expect("created in step");
            }
            Some(CopyAction::List { source }) => {
                let children = tree.get(&key(source)).cloned().unwrap_or_default();
                walk.expand(&children).expect("expand in step");
            }
            Some(CopyAction::CopyFile { dest, .. }) => {
                order.push(key(dest));
                walk.copied_file().expect("copy file in step");
            }
            Some(CopyAction::CopyLink { dest, .. }) => {
                order.push(key(dest));
                walk.copied_link().expect("copy link in step");
            }
        }
    }
    panic!("copy walk did not complete within the step ceiling");
}

#[test]
fn copy_walk_copies_a_single_file() {
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "a.txt"]),
        comps(&["Storage", "a.txt"]),
        CopyKind::File,
    )])
    .expect("walk");

    assert!(!walk.is_complete());
    assert_eq!(walk.copied(), 0);
    match walk.next_action() {
        Some(CopyAction::CopyFile { source, dest }) => {
            assert_eq!(source, comps(&["Users", "a.txt"]).as_slice());
            assert_eq!(dest, comps(&["Storage", "a.txt"]).as_slice());
        }
        other => panic!("expected a CopyFile, got {other:?}"),
    }
    walk.copied_file().expect("copy the file");
    assert!(walk.is_complete());
    assert_eq!(walk.copied(), 1);
    assert!(walk.next_action().is_none());
}

#[test]
fn copy_walk_makes_the_destination_directory_before_listing_an_empty_one() {
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "empty"]),
        comps(&["Storage", "empty"]),
        CopyKind::Directory,
    )])
    .expect("walk");

    // The destination directory is created first, before its contents are read.
    match walk.next_action() {
        Some(CopyAction::MakeDir { dest }) => {
            assert_eq!(dest, comps(&["Storage", "empty"]).as_slice());
        }
        other => panic!("expected a MakeDir, got {other:?}"),
    }
    walk.created().expect("dest dir made");
    assert_eq!(walk.copied(), 1);
    // Then the source is listed; an empty listing finishes the directory.
    match walk.next_action() {
        Some(CopyAction::List { source }) => {
            assert_eq!(source, comps(&["Users", "empty"]).as_slice());
        }
        other => panic!("expected a List, got {other:?}"),
    }
    walk.expand(&[]).expect("expand empty");
    assert!(walk.is_complete());
    assert_eq!(walk.copied(), 1);
}

#[test]
fn copy_walk_creates_containers_before_contents_depth_first() {
    // /Users/tree/{a.txt, sub/{b.txt}}, listed in that order → /Storage/tree.
    let mut tree: BTreeMap<String, Vec<(String, CopyKind)>> = BTreeMap::new();
    tree.insert(
        key(&comps(&["Users", "tree"])),
        vec![
            ("a.txt".to_string(), CopyKind::File),
            ("sub".to_string(), CopyKind::Directory),
        ],
    );
    tree.insert(
        key(&comps(&["Users", "tree", "sub"])),
        vec![("b.txt".to_string(), CopyKind::File)],
    );

    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "tree"]),
        comps(&["Storage", "tree"]),
        CopyKind::Directory,
    )])
    .expect("walk");
    let order = drive_copy(&mut walk, &tree);

    // A container is created before its contents, siblings keep listing order,
    // and a subtree is fully copied before we return to the parent.
    assert_eq!(
        order,
        vec![
            key(&comps(&["Storage", "tree"])),
            key(&comps(&["Storage", "tree", "a.txt"])),
            key(&comps(&["Storage", "tree", "sub"])),
            key(&comps(&["Storage", "tree", "sub", "b.txt"])),
        ]
    );
    // Every node counted once: tree, a.txt, sub, b.txt.
    assert_eq!(walk.copied(), 4);
}

#[test]
fn copy_walk_processes_multiple_items_in_order() {
    let mut tree: BTreeMap<String, Vec<(String, CopyKind)>> = BTreeMap::new();
    tree.insert(
        key(&comps(&["Users", "dir"])),
        vec![("inner".to_string(), CopyKind::File)],
    );

    let mut walk = CopyWalk::from_items(vec![
        (
            comps(&["Users", "first.txt"]),
            comps(&["Storage", "first.txt"]),
            CopyKind::File,
        ),
        (
            comps(&["Users", "dir"]),
            comps(&["Storage", "dir"]),
            CopyKind::Directory,
        ),
        (
            comps(&["Users", "last.txt"]),
            comps(&["Storage", "last.txt"]),
            CopyKind::File,
        ),
    ])
    .expect("walk");
    let order = drive_copy(&mut walk, &tree);

    assert_eq!(
        order,
        vec![
            key(&comps(&["Storage", "first.txt"])),
            key(&comps(&["Storage", "dir"])),
            key(&comps(&["Storage", "dir", "inner"])),
            key(&comps(&["Storage", "last.txt"])),
        ]
    );
    assert_eq!(walk.copied(), 4);
}

#[test]
fn copy_walk_expand_refuses_a_tree_deeper_than_the_bound() {
    // A directory source that already sits at the maximum depth: expanding it
    // would name a child one component deeper than the bound.
    let deep: Vec<String> = (0..MAX_COPY_DEPTH).map(|i| format!("d{i}")).collect();
    assert_eq!(deep.len(), MAX_COPY_DEPTH);
    let mut walk = CopyWalk::from_items(vec![(
        deep,
        comps(&["Storage", "dst"]),
        CopyKind::Directory,
    )])
    .expect("walk");

    // Make the destination, then the list step refuses to descend further.
    assert!(matches!(
        walk.next_action(),
        Some(CopyAction::MakeDir { .. })
    ));
    walk.created().expect("dest dir made");
    assert!(matches!(walk.next_action(), Some(CopyAction::List { .. })));
    assert_eq!(
        walk.expand(&[("child".to_string(), CopyKind::File)]),
        Err(CopyWalkError::TooDeep)
    );
    // Refused, and the walk is left exactly where it was (fail closed): still a
    // List of the same directory.
    assert!(matches!(walk.next_action(), Some(CopyAction::List { .. })));
}

#[test]
fn copy_walk_fails_closed_when_driven_out_of_step() {
    // `created` / `expand` on a leaf-file step are out of step.
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "a.txt"]),
        comps(&["Storage", "a.txt"]),
        CopyKind::File,
    )])
    .expect("walk");
    assert!(matches!(
        walk.next_action(),
        Some(CopyAction::CopyFile { .. })
    ));
    assert_eq!(walk.created(), Err(CopyWalkError::OutOfStep));
    assert_eq!(walk.expand(&[]), Err(CopyWalkError::OutOfStep));
    assert!(matches!(
        walk.next_action(),
        Some(CopyAction::CopyFile { .. })
    ));

    // `expand` on a not-yet-created directory, and `copied_file` on a directory,
    // are out of step.
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "d"]),
        comps(&["Storage", "d"]),
        CopyKind::Directory,
    )])
    .expect("walk");
    assert!(matches!(
        walk.next_action(),
        Some(CopyAction::MakeDir { .. })
    ));
    assert_eq!(walk.expand(&[]), Err(CopyWalkError::OutOfStep));
    assert_eq!(walk.copied_file(), Err(CopyWalkError::OutOfStep));
    assert!(matches!(
        walk.next_action(),
        Some(CopyAction::MakeDir { .. })
    ));

    // Any driver call on a finished walk is out of step.
    walk.created().expect("dest dir made");
    walk.expand(&[]).expect("empty the directory");
    assert!(walk.is_complete());
    assert_eq!(walk.created(), Err(CopyWalkError::OutOfStep));
    assert_eq!(walk.expand(&[]), Err(CopyWalkError::OutOfStep));
    assert_eq!(walk.copied_file(), Err(CopyWalkError::OutOfStep));
}

#[test]
fn copy_walk_holds_its_position_across_an_interruption() {
    let mut tree: BTreeMap<String, Vec<(String, CopyKind)>> = BTreeMap::new();
    tree.insert(
        key(&comps(&["Users", "dir"])),
        vec![
            ("x".to_string(), CopyKind::File),
            ("y".to_string(), CopyKind::File),
        ],
    );
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "dir"]),
        comps(&["Storage", "dir"]),
        CopyKind::Directory,
    )])
    .expect("walk");

    // Make the dest dir, list it, then copy exactly one child and "stop" (as a
    // Cancel or a preemption would) holding the walk.
    assert!(matches!(
        walk.next_action(),
        Some(CopyAction::MakeDir { .. })
    ));
    walk.created().expect("dest dir made");
    assert!(matches!(walk.next_action(), Some(CopyAction::List { .. })));
    walk.expand(&[
        ("x".to_string(), CopyKind::File),
        ("y".to_string(), CopyKind::File),
    ])
    .expect("expand");
    if let Some(CopyAction::CopyFile { .. }) = walk.next_action() {
        walk.copied_file().expect("copy x");
    }
    // The directory and the first child are done; the second child remains.
    assert_eq!(walk.copied(), 2);
    assert!(!walk.is_complete());

    // Resuming from exactly here copies the remaining child — no repeat of the
    // child already copied, no skip.
    let order = drive_copy(&mut walk, &tree);
    assert_eq!(order, vec![key(&comps(&["Storage", "dir", "y"]))]);
    assert_eq!(walk.copied(), 3);
}

#[test]
fn copy_walk_from_items_fails_closed() {
    // Nothing to copy.
    assert!(CopyWalk::from_items(vec![]).is_none());
    // A source or destination that names the root (an empty component list).
    assert!(
        CopyWalk::from_items(vec![(Vec::new(), comps(&["Storage", "a"]), CopyKind::File)])
            .is_none()
    );
    assert!(
        CopyWalk::from_items(vec![(comps(&["Users", "a"]), Vec::new(), CopyKind::File)]).is_none()
    );
    // A valid item builds a walk.
    assert!(CopyWalk::from_items(vec![(
        comps(&["Users", "a"]),
        comps(&["Storage", "a"]),
        CopyKind::File
    )])
    .is_some());
}

#[test]
fn copy_walk_error_messages_are_non_empty() {
    assert!(!CopyWalkError::TooDeep.to_string().is_empty());
    assert!(!CopyWalkError::OutOfStep.to_string().is_empty());
}

// ---------------------------------------------------------------------------
// FM4b — the toolbar + breadcrumb frame model (pure chrome model).
// ---------------------------------------------------------------------------

#[test]
fn the_toolbar_disables_back_forward_and_up_at_the_root() {
    use crate::chrome::{ToolbarCommand, ToolbarModel};

    // At the root, fresh: no history either way and no parent to climb to, so
    // the three navigation tools render disabled; refresh, view, and sort are
    // always actionable.
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let toolbar = ToolbarModel::for_browser(&browser);
    assert!(!toolbar.is_enabled(ToolbarCommand::Back));
    assert!(!toolbar.is_enabled(ToolbarCommand::Forward));
    assert!(!toolbar.is_enabled(ToolbarCommand::Up));
    assert!(toolbar.is_enabled(ToolbarCommand::Refresh));
    assert!(toolbar.is_enabled(ToolbarCommand::ToggleView));
    assert!(toolbar.is_enabled(ToolbarCommand::Sort));
}

#[test]
fn the_toolbar_enables_back_and_up_after_descending() {
    use crate::chrome::{ToolbarCommand, ToolbarModel};

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    let toolbar = ToolbarModel::for_browser(&browser);
    // We can climb to the parent and go back to the root, but there is nothing
    // ahead of us yet.
    assert!(toolbar.is_enabled(ToolbarCommand::Up));
    assert!(toolbar.is_enabled(ToolbarCommand::Back));
    assert!(!toolbar.is_enabled(ToolbarCommand::Forward));
}

#[test]
fn the_toolbar_enables_forward_only_after_going_back() {
    use crate::chrome::{ToolbarCommand, ToolbarModel};

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    assert_eq!(browser.go_back(), Ok(true));
    let toolbar = ToolbarModel::for_browser(&browser);
    // Back to the root: Forward is now available, Back and Up are not.
    assert!(toolbar.is_enabled(ToolbarCommand::Forward));
    assert!(!toolbar.is_enabled(ToolbarCommand::Back));
    assert!(!toolbar.is_enabled(ToolbarCommand::Up));
}

#[test]
fn the_toolbar_reports_the_active_view_and_sort() {
    use crate::chrome::ToolbarModel;
    use crate::layout::ViewMode;
    use crate::sort::{SortDirection, SortKey, SortMode};

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    let toolbar = ToolbarModel::for_browser(&browser);
    assert_eq!(toolbar.view_mode(), ViewMode::List);
    assert_eq!(toolbar.sort_mode(), SortMode::default_order());

    browser.set_view_mode(ViewMode::Grid);
    let by_size_desc = SortMode {
        key: SortKey::Size,
        direction: SortDirection::Descending,
    };
    browser.set_sort_mode(by_size_desc);
    let toolbar = ToolbarModel::for_browser(&browser);
    assert_eq!(toolbar.view_mode(), ViewMode::Grid);
    assert_eq!(toolbar.sort_mode(), by_size_desc);
}

#[test]
fn toolbar_commands_list_covers_every_variant_once() {
    use crate::chrome::{ToolbarCommand, TOOLBAR_COMMANDS};

    // The drawn chrome iterates TOOLBAR_COMMANDS, so it must hold each command
    // exactly once, in a stable order.
    assert_eq!(
        TOOLBAR_COMMANDS,
        &[
            ToolbarCommand::Back,
            ToolbarCommand::Forward,
            ToolbarCommand::Up,
            ToolbarCommand::Refresh,
            ToolbarCommand::ToggleView,
            ToolbarCommand::Sort,
        ]
    );
}

#[test]
fn the_context_menu_needs_a_selection_for_the_item_commands() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    // An empty directory offers no selection, so every command that acts on a
    // selected entry renders disabled; Paste still depends only on the held
    // clipboard.
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    browser.open_index(0).expect("enter the empty Fonts");
    assert_eq!(focused(&browser).map(Entry::name), None);

    let menu = ContextMenuModel::for_browser(&browser, false);
    for command in [
        ContextCommand::Open,
        ContextCommand::OpenAndClose,
        ContextCommand::OpenWith,
        ContextCommand::Rename,
        ContextCommand::Cut,
        ContextCommand::Copy,
        ContextCommand::ClearSelection,
        ContextCommand::Properties,
        ContextCommand::Delete,
    ] {
        assert!(!menu.is_enabled(command), "{command:?} without a selection");
        assert_eq!(
            menu.reason(command),
            "nothing selected",
            "{command:?} says why"
        );
    }
    assert!(!menu.is_enabled(ContextCommand::Paste));
}

#[test]
fn select_all_needs_an_entry_left_and_clear_selection_a_selection() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    let mut browser = Browser::open_root(activation_source()).expect("root");
    let nothing = ContextMenuModel::for_browser(&browser, false);
    assert_eq!(nothing.reason(ContextCommand::SelectAll), "");
    assert_eq!(
        nothing.reason(ContextCommand::ClearSelection),
        "nothing selected"
    );

    browser.select(1).expect("one entry");
    let one = ContextMenuModel::for_browser(&browser, false);
    assert!(one.is_enabled(ContextCommand::SelectAll));
    assert!(one.is_enabled(ContextCommand::ClearSelection));

    browser.select_all();
    let every = ContextMenuModel::for_browser(&browser, false);
    assert_eq!(
        every.reason(ContextCommand::SelectAll),
        "everything is selected"
    );
    assert!(every.is_enabled(ContextCommand::ClearSelection));

    let mut empty = Browser::open_root(MockFs::fixture()).expect("root");
    empty.open_index(2).expect("enter System");
    empty.open_index(0).expect("enter the empty Fonts");
    assert_eq!(
        ContextMenuModel::for_browser(&empty, false).reason(ContextCommand::SelectAll),
        "the folder is empty"
    );
}

#[test]
fn the_context_menu_offers_single_entry_verbs_only_for_one_chosen_entry() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    let mut browser = Browser::open_root(activation_source()).expect("root");
    let nothing = ContextMenuModel::for_browser(&browser, true);
    for command in [ContextCommand::Open, ContextCommand::Delete] {
        assert_eq!(nothing.reason(command), "nothing selected", "{command:?}");
    }
    assert_eq!(nothing.reason(ContextCommand::Paste), "");

    // Several selected: the set verbs act on them all, and the verbs that act
    // on one entry say why they cannot rather than picking one.
    browser.select(1).expect("Editor.app");
    browser.toggle_selection(2).expect("and notes.txt");
    let several = ContextMenuModel::for_browser(&browser, false);
    for command in [
        ContextCommand::Cut,
        ContextCommand::Copy,
        ContextCommand::Delete,
    ] {
        assert!(several.is_enabled(command), "{command:?}");
    }
    for command in [
        ContextCommand::Open,
        ContextCommand::OpenWith,
        ContextCommand::Rename,
        ContextCommand::Properties,
    ] {
        assert_eq!(
            several.reason(command),
            "several items selected",
            "{command:?}"
        );
    }
}

#[test]
fn the_context_menu_enables_the_item_commands_on_a_directory() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    // A directory descends on Open; every selection-scoped command is offered,
    // but Open With… is not — a directory has no application to choose.
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(0).expect("select Docs");
    assert_eq!(focused(&browser).map(Entry::name), Some("Docs"));
    let menu = ContextMenuModel::for_browser(&browser, false);
    assert!(menu.is_enabled(ContextCommand::Open));
    assert!(!menu.is_enabled(ContextCommand::OpenWith));
    // A folder becomes this window's own new content, so there is no hand-off
    // for Open and Close to close the window after.
    assert!(!menu.is_enabled(ContextCommand::OpenAndClose));
    assert!(menu.is_enabled(ContextCommand::Rename));
    assert!(menu.is_enabled(ContextCommand::Cut));
    assert!(menu.is_enabled(ContextCommand::Copy));
    assert!(menu.is_enabled(ContextCommand::Properties));
    assert!(menu.is_enabled(ContextCommand::Delete));
}

#[test]
fn the_context_menu_enables_the_item_commands_on_a_bundle() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    // A bundle is a selection like any other: Open launches it, and the
    // selection-scoped commands apply. Open With… is not offered — a bundle
    // launches itself, so there is no application to choose for it.
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(1).expect("select Editor.app");
    assert!(focused(&browser).expect("bundle").is_bundle());
    let menu = ContextMenuModel::for_browser(&browser, false);
    assert!(menu.is_enabled(ContextCommand::Open));
    assert!(!menu.is_enabled(ContextCommand::OpenWith));
    // Launching a bundle hands the entry to another program, so the window may
    // close behind it.
    assert!(menu.is_enabled(ContextCommand::OpenAndClose));
    assert!(menu.is_enabled(ContextCommand::Rename));
    assert!(menu.is_enabled(ContextCommand::Properties));
    assert!(menu.is_enabled(ContextCommand::Delete));
}

#[test]
fn the_context_menu_enables_the_item_commands_on_a_file() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    assert_eq!(focused(&browser).map(Entry::name), Some("notes.txt"));
    let menu = ContextMenuModel::for_browser(&browser, false);
    assert!(menu.is_enabled(ContextCommand::Open));
    // Open With… is offered only for a regular file, so it is enabled here, and
    // opening a file hands it over, so Open and Close is too.
    assert!(menu.is_enabled(ContextCommand::OpenWith));
    assert!(menu.is_enabled(ContextCommand::OpenAndClose));
    assert!(menu.is_enabled(ContextCommand::Rename));
    assert!(menu.is_enabled(ContextCommand::Cut));
    assert!(menu.is_enabled(ContextCommand::Copy));
    assert!(menu.is_enabled(ContextCommand::Properties));
    assert!(menu.is_enabled(ContextCommand::Delete));
}

#[test]
fn the_context_menu_enables_paste_only_when_a_clipboard_is_held() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    // Paste targets the current directory and needs a held clipboard, not a
    // selection: the app threads its own clipboard state in.
    let browser = Browser::open_root(activation_source()).expect("root");
    assert!(!ContextMenuModel::for_browser(&browser, false).is_enabled(ContextCommand::Paste));
    assert!(ContextMenuModel::for_browser(&browser, true).is_enabled(ContextCommand::Paste));
}

#[test]
fn context_commands_list_covers_every_variant_once() {
    use crate::chrome::{ContextCommand, CONTEXT_COMMANDS};

    // The declared menu iterates CONTEXT_COMMANDS and a row's id is a command's
    // position in it, so it must hold each command exactly once in a stable
    // order. Open and Close carries the "open this and I am done here" verb the
    // desktop's menu grab took the right-double-click gesture's second press
    // away from (`plans/NEW-MENUS.md` D20). New Folder stays absent — the menu
    // has no verb to invoke for it (it is a toolbar write tool), so it would be
    // speculative surface here.
    assert_eq!(
        CONTEXT_COMMANDS,
        &[
            ContextCommand::Open,
            ContextCommand::OpenAndClose,
            ContextCommand::OpenWith,
            ContextCommand::Rename,
            ContextCommand::Cut,
            ContextCommand::Copy,
            ContextCommand::Paste,
            ContextCommand::SelectAll,
            ContextCommand::ClearSelection,
            ContextCommand::Properties,
            ContextCommand::Delete,
        ]
    );
}

#[test]
fn the_context_menu_row_ids_are_the_inverse_of_the_command_list() {
    use crate::chrome::{context_choice_from_item, ContextChoice, CONTEXT_COMMANDS};
    use crate::open_with::OPEN_WITH_QUICK_MAX;
    use tairix_abi::window_ipc::AppMenuItemId;

    // The numbering runs: the commands in CONTEXT_COMMANDS order, the Rename
    // row's quick-entry field, a block as long as a plate offers candidates,
    // New ▸ Folder, then the New ▸ documents. Reading a chosen row back is the
    // exact inverse of numbering it, so no kind of answer can be mistaken for
    // another.
    let id = |index: usize| {
        AppMenuItemId::new(u16::try_from(index + 1).expect("a small index")).expect("non-zero")
    };
    for (index, &command) in CONTEXT_COMMANDS.iter().enumerate() {
        assert_eq!(
            context_choice_from_item(id(index)),
            Some(ContextChoice::Command(command))
        );
    }
    assert_eq!(
        context_choice_from_item(id(CONTEXT_COMMANDS.len())),
        Some(ContextChoice::RenameCommit)
    );
    for candidate in 0..OPEN_WITH_QUICK_MAX {
        assert_eq!(
            context_choice_from_item(id(CONTEXT_COMMANDS.len() + 1 + candidate)),
            Some(ContextChoice::OpenWithCandidate(candidate))
        );
    }
    let new_folder = CONTEXT_COMMANDS.len() + 1 + OPEN_WITH_QUICK_MAX;
    assert_eq!(
        context_choice_from_item(id(new_folder)),
        Some(ContextChoice::NewFolder)
    );
    for document in 0..4 {
        assert_eq!(
            context_choice_from_item(id(new_folder + 1 + document)),
            Some(ContextChoice::NewDocument(document))
        );
    }
}

#[test]
fn new_sits_above_properties_and_offers_folder_then_the_documents() {
    use crate::chrome::{
        context_choice_from_item, context_menu, ContextChoice, ContextMenuModel, ContextQuick,
    };
    use crate::media::{BlankDocument, MediaType};
    use tairix_abi::window_ipc::AppMenuRowView;

    let browser = Browser::open_root(activation_source()).expect("root");
    let documents = [
        BlankDocument::of(MediaType::TextPlain).expect("text starts empty"),
        BlankDocument::of(MediaType::TextMarkdown).expect("markdown starts empty"),
    ];
    let menu = context_menu(
        ContextMenuModel::for_browser(&browser, false),
        "Files",
        ContextQuick {
            documents: &documents,
            ..ContextQuick::default()
        },
    )
    .expect("the rows fit the bounds");
    let rows: alloc::vec::Vec<_> = menu.rows().collect();
    let new_at = rows
        .iter()
        .position(|(row, _)| {
            matches!(
                row,
                AppMenuRowView::Submenu {
                    label: "New",
                    enabled: true
                }
            )
        })
        .expect("New is declared, and opens");
    assert_eq!(rows[new_at].1, None, "New is on the root plate");
    assert!(matches!(rows[new_at - 1].0, AppMenuRowView::Separator));
    assert!(matches!(rows[new_at + 1].0, AppMenuRowView::Separator));
    let label_at = |at: usize| match rows[at].0 {
        AppMenuRowView::Item(item) => item.label,
        _ => "",
    };
    assert_eq!(label_at(new_at - 2), "Clear Selection");
    assert_eq!(label_at(new_at - 3), "Select All");
    assert!(matches!(rows[new_at - 4].0, AppMenuRowView::Separator));
    assert_eq!(label_at(new_at - 5), "Paste");
    assert_eq!(label_at(new_at + 2), "Properties");

    let children: alloc::vec::Vec<_> = rows
        .iter()
        .filter(|(_, parent)| *parent == Some(new_at))
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(*item),
            _ => None,
        })
        .collect();
    let labels: alloc::vec::Vec<_> = children.iter().map(|item| item.label).collect();
    assert_eq!(labels, ["Folder", "Text Document", "Markdown Document"]);
    assert_eq!(children[0].shortcut, "Ctrl+Shift+N");
    let choices: alloc::vec::Vec<_> = children
        .iter()
        .map(|item| context_choice_from_item(item.id))
        .collect();
    assert_eq!(
        choices,
        [
            Some(ContextChoice::NewFolder),
            Some(ContextChoice::NewDocument(0)),
            Some(ContextChoice::NewDocument(1)),
        ]
    );
}

#[test]
fn new_offers_folder_when_no_editor_makes_a_document() {
    use crate::chrome::{context_menu, ContextMenuModel, ContextQuick};
    use tairix_abi::window_ipc::AppMenuRowView;

    let browser = Browser::open_root(activation_source()).expect("root");
    let menu = context_menu(
        ContextMenuModel::for_browser(&browser, false),
        "Files",
        ContextQuick::default(),
    )
    .expect("the rows fit the bounds");
    let rows: alloc::vec::Vec<_> = menu.rows().collect();
    let new_at = rows
        .iter()
        .position(|(row, _)| matches!(row, AppMenuRowView::Submenu { label: "New", .. }))
        .expect("New is declared");
    let children: alloc::vec::Vec<_> = rows
        .iter()
        .filter(|(_, parent)| *parent == Some(new_at))
        .collect();
    assert_eq!(children.len(), 1);
    assert!(matches!(children[0].0, AppMenuRowView::Item(item) if item.label == "Folder"));
}

#[test]
fn more_candidates_than_a_plate_offers_never_reach_the_new_rows_ids() {
    use crate::chrome::{
        context_choice_from_item, context_menu, ContextChoice, ContextMenuModel, ContextQuick,
    };
    use crate::open_with::OPEN_WITH_QUICK_MAX;
    use tairix_abi::window_ipc::AppMenuRowView;

    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    let apps: alloc::vec::Vec<_> = (0..OPEN_WITH_QUICK_MAX + 3)
        .map(|n| {
            AppAssociation::new(
                alloc::format!("app{n}"),
                alloc::format!("/Apps/app{n}.app"),
                vec!["text/plain".to_string()],
            )
        })
        .collect();
    let candidates: alloc::vec::Vec<_> = apps.iter().collect();
    let menu = context_menu(
        ContextMenuModel::for_browser(&browser, false),
        "Files",
        ContextQuick {
            name: "notes.txt",
            candidates: &candidates,
            documents: &[],
        },
    )
    .expect("the rows fit the bounds");
    let offered: alloc::vec::Vec<_> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => context_choice_from_item(item.id),
            _ => None,
        })
        .filter(|choice| matches!(choice, ContextChoice::OpenWithCandidate(_)))
        .collect();
    assert_eq!(offered.len(), OPEN_WITH_QUICK_MAX);
}

#[test]
fn the_context_menu_declares_one_row_per_command_with_its_label_and_caption() {
    use crate::chrome::{context_menu, ContextMenuModel, ContextQuick, CONTEXT_COMMANDS};
    use tairix_abi::window_ipc::AppMenuRowView;

    // The desktop draws the menu, so what this asserts is the *declaration*:
    // one item row per command in order, carrying that command's own label and
    // accelerator caption. Separators are declared too, but the chain folds
    // each into the following row's group break, so they are not rows a reader
    // has to skip to line a command up with its id.
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    let model = ContextMenuModel::for_browser(&browser, true);
    let menu = context_menu(model, "Files", ContextQuick::default())
        .expect("the fixed rows fit the bounds");
    assert_eq!(menu.title(), "Files");

    let items: alloc::vec::Vec<_> = menu
        .rows()
        .filter_map(|(row, parent)| match row {
            AppMenuRowView::Item(item) if parent.is_none() => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), CONTEXT_COMMANDS.len());
    for (item, &command) in items.iter().zip(CONTEXT_COMMANDS) {
        assert_eq!(item.label, command.label(), "{command:?} label");
        assert_eq!(item.shortcut, command.shortcut(), "{command:?} caption");
    }
}

#[test]
fn a_declared_context_row_is_disabled_with_its_reason_never_left_out() {
    use crate::chrome::{
        context_menu, ContextCommand, ContextMenuModel, ContextQuick, CONTEXT_COMMANDS,
    };
    use tairix_abi::window_ipc::AppMenuRowView;

    // An empty directory makes every selection-scoped command inactionable.
    // The menu's shape must not move with the selection, so each is declared
    // *disabled with its reason* rather than omitted — which is also what keeps
    // a row's id equal to its command's position however few are actionable.
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    browser.open_index(2).expect("enter System");
    browser.open_index(0).expect("enter the empty Fonts");
    assert_eq!(focused(&browser).map(Entry::name), None);

    let model = ContextMenuModel::for_browser(&browser, false);
    let menu = context_menu(model, "Files", ContextQuick::default())
        .expect("the fixed rows fit the bounds");
    let items: alloc::vec::Vec<_> = menu
        .rows()
        .filter_map(|(row, parent)| match row {
            AppMenuRowView::Item(item) if parent.is_none() => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), CONTEXT_COMMANDS.len());
    for (item, &command) in items.iter().zip(CONTEXT_COMMANDS) {
        assert!(!item.enabled, "{command:?} is inactionable here");
        assert_eq!(item.reason, model.reason(command), "{command:?} reason");
        assert!(!item.reason.is_empty(), "{command:?} says why");
    }
    // The reasons are the model's, never the builder's invention.
    assert_eq!(model.reason(ContextCommand::Paste), "nothing to paste");
}

#[test]
fn the_context_menu_reason_distinguishes_why_a_row_cannot_act() {
    use crate::chrome::{ContextCommand, ContextMenuModel};

    // Enablement alone cannot say *why*, and a row that greys out with no
    // reason is the thing the wire model's reason field exists to fix. A
    // directory, a bundle, and a file each turn Open With… and Open and Close
    // down for a different reason, and each states it.
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(0).expect("select Docs");
    assert_eq!(focused(&browser).map(Entry::name), Some("Docs"));
    let on_directory = ContextMenuModel::for_browser(&browser, false);
    assert_eq!(
        on_directory.reason(ContextCommand::OpenWith),
        "only a file opens with an application"
    );
    assert_eq!(
        on_directory.reason(ContextCommand::OpenAndClose),
        "a folder opens in this window",
        "a descent is this window's new content, so closing it would leave nothing"
    );
    assert_eq!(on_directory.reason(ContextCommand::Open), "");

    browser.select(1).expect("select Editor.app");
    let on_bundle = ContextMenuModel::for_browser(&browser, false);
    assert_eq!(
        on_bundle.reason(ContextCommand::OpenWith),
        "only a file opens with an application",
        "a bundle launches itself"
    );
    assert_eq!(
        on_bundle.reason(ContextCommand::OpenAndClose),
        "",
        "launching a bundle hands the entry over, so the window may close"
    );

    browser.select(2).expect("select notes.txt");
    let on_file = ContextMenuModel::for_browser(&browser, false);
    assert_eq!(on_file.reason(ContextCommand::OpenWith), "");
    assert_eq!(on_file.reason(ContextCommand::OpenAndClose), "");
    // Enablement is derived from the reason, so the two cannot disagree.
    for command in crate::chrome::CONTEXT_COMMANDS {
        assert_eq!(
            on_file.is_enabled(*command),
            on_file.reason(*command).is_empty(),
            "{command:?}"
        );
    }
}

/// A declared reason is tip text: the decoded row states it for the seat to
/// show on dwell, and the drawn row carries none of it.
///
/// The reported defect was the other half of that — every reason drawn beside
/// its label, so this menu was as wide as "only a file opens with an
/// application".
#[test]
fn a_declared_context_reason_reaches_a_tip_and_never_the_drawn_row() {
    use crate::chrome::{context_menu, ContextCommand, ContextMenuModel, ContextQuick};
    use tairix_controls::{ChainModel, Menu};
    use tairix_geometry::Scale;
    use tairix_theme::Theme;

    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(0).expect("select Docs");
    assert_eq!(
        focused(&browser).map(Entry::name),
        Some("Docs"),
        "a directory"
    );
    let model = ContextMenuModel::for_browser(&browser, false);
    let refused = model.reason(ContextCommand::OpenWith);
    assert!(!refused.is_empty(), "the directory refuses Open With…");

    let wire = context_menu(model, "Files", ContextQuick::default())
        .expect("the fixed rows fit the bounds");
    let decoded = ChainModel::from_app_menu("Files", &wire, None);
    let row = decoded
        .rows()
        .iter()
        .find(|row| row.drawn().label() == ContextCommand::OpenWith.label())
        .expect("the Open With… row is declared, not left out");

    assert_eq!(row.tip(), Some(refused), "the row still says why, as a tip");
    assert!(
        ContextCommand::OpenWith.shortcut().is_empty(),
        "the row advertises no accelerator, so its drawn form is bare"
    );
    assert_eq!(
        row.drawn(),
        &tairix_controls::MenuItem::new(ContextCommand::OpenWith.label())
            .with_state(tairix_controls::ControlState::default().with_enabled(false)),
        "the drawn row is the label and the refusal, with no reason on it"
    );

    // And the plate is the width of its labels: no reason is measured into it.
    let theme = Theme::dark();
    let drawn = Menu::new(
        decoded
            .rows()
            .iter()
            .map(|row| row.drawn().clone())
            .collect::<alloc::vec::Vec<_>>(),
    );
    let widest_label = decoded
        .rows()
        .iter()
        .map(|row| row.drawn().label().len())
        .max()
        .expect("rows");
    assert!(
        widest_label >= ContextCommand::Open.label().len(),
        "the widest label is a label, not an excuse"
    );
    assert!(
        drawn.preferred_width(Scale::ONE, &theme)
            < Menu::new(alloc::vec![tairix_controls::MenuItem::new(refused)])
                .preferred_width(Scale::ONE, &theme),
        "a plate carrying no reason is narrower than one reason would make it"
    );
}

#[test]
fn only_removal_declares_the_destructive_emphasis() {
    use crate::chrome::{
        context_menu, ContextCommand, ContextMenuModel, ContextQuick, CONTEXT_COMMANDS,
    };
    use tairix_abi::window_ipc::{AppMenuRole, AppMenuRowView};

    // Emphasis is a property of the command, not of whether it is actionable,
    // so it is asserted over a fully-enabled menu: exactly the one row whose
    // verb destroys something wears the destructive role.
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    let menu = context_menu(
        ContextMenuModel::for_browser(&browser, true),
        "Files",
        ContextQuick::default(),
    )
    .expect("the fixed rows fit the bounds");
    let roles: alloc::vec::Vec<_> = menu
        .rows()
        .filter_map(|(row, _parent)| match row {
            AppMenuRowView::Item(item) => Some(item.role),
            _ => None,
        })
        .collect();
    for (role, &command) in roles.iter().zip(CONTEXT_COMMANDS) {
        let expected = if command == ContextCommand::Delete {
            AppMenuRole::Destructive
        } else {
            AppMenuRole::Neutral
        };
        assert_eq!(*role, expected, "{command:?} emphasis");
    }
}

#[test]
fn a_context_menu_title_the_bounds_refuse_opens_nothing() {
    use crate::chrome::{context_menu, ContextMenuModel, ContextQuick};
    use tairix_abi::window_ipc::APP_MENU_LABEL_MAX;

    // The title is untrusted display text bounded exactly as a row label is, so
    // an over-long one is refused at build rather than encoded and quietly
    // trimmed: the caller reports the refusal and opens no menu.
    let browser = Browser::open_root(activation_source()).expect("root");
    let model = ContextMenuModel::for_browser(&browser, false);
    let over_long = "F".repeat(APP_MENU_LABEL_MAX + 1);
    assert!(context_menu(model, &over_long, ContextQuick::default()).is_err());
    assert!(context_menu(model, "Files", ContextQuick::default()).is_ok());
}

#[test]
fn the_open_with_chooser_needs_a_candidate_and_keeps_the_ranked_order() {
    use crate::open_with::{AppAssociation, OpenWithChooser};

    // No installed application claiming the type is an honest empty answer the
    // caller states on its error stream; a chooser is never built over nothing,
    // so its selection always names a real row.
    assert!(OpenWithChooser::new(&[], "/Users/u/notes.txt", "notes.txt").is_none());

    let editor = AppAssociation::new(
        "Editor",
        "/Apps/Editor.app",
        alloc::vec![String::from("text/plain")],
    );
    let viewer = AppAssociation::new(
        "Viewer",
        "/Apps/Viewer.app",
        alloc::vec![String::from("text/plain")],
    );
    let chooser = OpenWithChooser::new(&[&editor, &viewer], "/Users/u/notes.txt", "notes.txt")
        .expect("two candidates");
    assert_eq!(chooser.candidates().len(), 2);
    assert_eq!(chooser.candidates()[0].name(), "Editor");
    assert_eq!(chooser.candidates()[1].bundle_path(), "/Apps/Viewer.app");
    assert_eq!(chooser.file_path(), "/Users/u/notes.txt");
    assert_eq!(chooser.display_name(), "notes.txt");
    // The first candidate is current, so an immediate activation opens the file
    // with the same application the default open would have picked.
    assert_eq!(chooser.selected(), 0);
    assert_eq!(chooser.chosen().expect("a current row").name(), "Editor");
}

#[test]
fn the_open_with_selection_clamps_at_both_ends() {
    use crate::open_with::{AppAssociation, OpenWithChooser};

    let apps: alloc::vec::Vec<AppAssociation> = (0..3)
        .map(|n| AppAssociation::new(alloc::format!("App{n}"), "/Apps/A.app", alloc::vec![]))
        .collect();
    let refs: alloc::vec::Vec<&AppAssociation> = apps.iter().collect();
    let mut chooser = OpenWithChooser::new(&refs, "/f", "f").expect("three candidates");

    assert!(chooser.step(1));
    assert_eq!(chooser.selected(), 1);
    assert!(chooser.step(5));
    assert_eq!(chooser.selected(), 2, "stops at the last candidate");
    assert!(!chooser.step(1), "already at the end: nothing moved");
    assert!(chooser.step(-9));
    assert_eq!(chooser.selected(), 0, "stops at the first candidate");
    assert!(chooser.select(99));
    assert_eq!(chooser.selected(), 2, "an out-of-range index clamps");
}

/// A chooser over `count` candidates, and the popup it asks for on a modest
/// screen.
fn chooser_of(count: usize) -> (crate::OpenWithChooser, Rect) {
    use crate::open_with::{AppAssociation, OpenWithChooser};
    use crate::render::open_with_chooser_extent;

    let apps: Vec<AppAssociation> = (0..count)
        .map(|n| AppAssociation::new(format!("App{n}"), "/Apps/A.app", vec![]))
        .collect();
    let refs: Vec<&AppAssociation> = apps.iter().collect();
    let chooser = OpenWithChooser::new(&refs, "/f", "f").expect("a candidate");
    let (w, h) = open_with_chooser_extent(
        &chooser,
        Scale::ONE,
        &Theme::dark(),
        Rect::new(0, 0, 800, 600),
    );
    (chooser, Rect::new(0, 0, w, h))
}

/// The candidates the chooser's list shows any part of, top to bottom, found
/// by pressing down its rows' own column — so what is asserted is what a press
/// reaches, not a re-derivation of the band.
fn chooser_rows(chooser: &crate::OpenWithChooser, vp: Rect) -> Vec<usize> {
    let theme = Theme::dark();
    let mut found = Vec::new();
    for y in vp.top()..vp.bottom() {
        if let Some(index) =
            crate::render::open_with_row_at(chooser, vp, Scale::ONE, &theme, Point::new(4, y))
        {
            if found.last() != Some(&index) {
                found.push(index);
            }
        }
    }
    found
}

/// Revealing the current candidate scrolls the least number of pixels that
/// shows it whole — the rule keyboard traversal moves through, so a selection
/// can never sit outside the drawn rows.
#[test]
fn the_open_with_list_reveals_the_selection_by_the_least_pixels() {
    use crate::render::{open_with_reveal, row_height, OPEN_WITH_MAX_ROWS};

    let theme = Theme::dark();
    let row = u64::from(row_height(Scale::ONE, &theme));
    let (mut chooser, vp) = chooser_of(OPEN_WITH_MAX_ROWS + 2);
    chooser.select(OPEN_WITH_MAX_ROWS + 1);
    assert!(open_with_reveal(&mut chooser, Scale::ONE, &theme, vp));
    assert_eq!(
        chooser.offset(),
        row * 2,
        "the last row sits at the list's foot"
    );
    assert_eq!(
        chooser_rows(&chooser, vp).last(),
        Some(&(OPEN_WITH_MAX_ROWS + 1))
    );
    chooser.select(0);
    assert!(open_with_reveal(&mut chooser, Scale::ONE, &theme, vp));
    assert_eq!(chooser.offset(), 0, "the first row sits at its head");
    chooser.select(1);
    assert!(
        !open_with_reveal(&mut chooser, Scale::ONE, &theme, vp),
        "already whole: nothing moved"
    );
}

/// The chooser's wheel moves its list a fixed distance a detent, carries what
/// is short of a pixel, stops at either end, and does nothing to a list that
/// shows everything.
#[test]
fn the_open_with_wheel_scrolls_the_list_and_clamps_at_the_ends() {
    use crate::render::{open_with_scroll_wheel, row_height, OPEN_WITH_MAX_ROWS};
    use tairix_controls::damage::sink;
    use tairix_controls::scroll::WHEEL_STEP;

    let theme = Theme::dark();
    let row = u64::from(row_height(Scale::ONE, &theme));
    let (mut chooser, vp) = chooser_of(OPEN_WITH_MAX_ROWS + 20);
    let turn = |chooser: &mut crate::OpenWithChooser, dy: i32| {
        open_with_scroll_wheel(chooser, Scale::ONE, &theme, vp, (0, dy), &mut sink())
    };
    assert!(!turn(&mut chooser, -DETENT), "already at the top");
    assert!(turn(&mut chooser, DETENT));
    assert_eq!(chooser.offset(), u64::from(WHEEL_STEP));
    assert!(turn(&mut chooser, DETENT * 1000));
    assert_eq!(chooser.offset(), row * 20, "the last row rests at the foot");
    assert!(!turn(&mut chooser, DETENT));

    let (mut short, short_vp) = chooser_of(3);
    assert!(!open_with_scroll_wheel(
        &mut short,
        Scale::ONE,
        &theme,
        short_vp,
        (0, DETENT),
        &mut sink()
    ));
    assert_eq!(short.offset(), 0);
}

/// A list resting a pixel into its first row shows that row cut at the top
/// and a sliver of the next one past the popup's rows at the foot; both are
/// drawn whole and cut, and each is hit where it shows.
#[test]
fn a_chooser_scrolled_part_way_draws_and_hits_the_rows_its_edges_cut() {
    use crate::render::{draw_open_with_chooser, open_with_scroll_wheel, OPEN_WITH_MAX_ROWS};
    use tairix_controls::damage::sink;
    use tairix_controls::scroll::WHEEL_STEP;

    let theme = Theme::dark();
    let (mut chooser, vp) = chooser_of(OPEN_WITH_MAX_ROWS + 4);
    let rows = chooser_rows(&chooser, vp);
    assert_eq!(rows, (0..OPEN_WITH_MAX_ROWS).collect::<Vec<_>>());
    let band_top = (vp.top()..vp.bottom())
        .find(|&y| {
            crate::render::open_with_row_at(&chooser, vp, Scale::ONE, &theme, Point::new(4, y))
                .is_some()
        })
        .expect("a row shows");
    let row = crate::render::row_height(Scale::ONE, &theme);
    let band_foot =
        band_top + i32::try_from(row * u32::try_from(OPEN_WITH_MAX_ROWS).unwrap()).unwrap();

    let mut before = Surface::new(vp.width, vp.height).expect("surface");
    draw_open_with_chooser(
        &mut before,
        &chooser,
        Scale::ONE,
        &theme,
        vp,
        &mut NoArtwork,
    );
    // The fewest units that make up one whole pixel of scroll.
    let per_pixel = i32::try_from(DETENT.unsigned_abs().div_ceil(WHEEL_STEP)).unwrap();
    assert!(open_with_scroll_wheel(
        &mut chooser,
        Scale::ONE,
        &theme,
        vp,
        (0, per_pixel),
        &mut sink()
    ));
    assert_eq!(chooser.offset(), 1);
    let mut after = Surface::new(vp.width, vp.height).expect("surface");
    draw_open_with_chooser(&mut after, &chooser, Scale::ONE, &theme, vp, &mut NoArtwork);

    let at = |y: i32| {
        crate::render::open_with_row_at(&chooser, vp, Scale::ONE, &theme, Point::new(4, y))
    };
    assert_eq!(at(band_top), Some(0), "the first row, cut by a pixel");
    assert_eq!(
        at(band_foot - 1),
        Some(OPEN_WITH_MAX_ROWS),
        "the sliver of the next row is hit where it shows"
    );
    assert_eq!(at(band_foot), None, "and the band ends where it did");
    let rows_wide = 40;
    let top = u32::try_from(band_top).unwrap();
    let foot = u32::try_from(band_foot).unwrap();
    assert_eq!(
        item_rows(&after, rows_wide, top..foot - 1),
        item_rows(&before, rows_wide, top + 1..foot),
        "the rows moved up by the pixel the list scrolled, drawn whole"
    );
}

#[test]
fn the_open_with_chooser_draws_and_hit_tests_the_same_rows() {
    use crate::render::{
        draw_open_with_chooser, open_with_chooser_rect, open_with_reveal, open_with_row_at,
        row_height, OPEN_WITH_MAX_ROWS,
    };
    use tairix_icon::NoArtwork;

    // Paint and press resolve through one placement, so a click lands on
    // exactly the row the user saw — including after a scroll, where the row at
    // a given position on screen is a *different* candidate. A press off the
    // rows resolves to nothing (fail closed).
    let theme = Theme::dark();
    let (mut chooser, vp) = chooser_of(20);
    assert_eq!(
        chooser_rows(&chooser, vp),
        (0..OPEN_WITH_MAX_ROWS).collect::<Vec<_>>(),
        "a list longer than the bound fills the popup the bound sizes, and scrolls inside it"
    );

    let bounds = open_with_chooser_rect(vp);
    // Probe down the panel's own column until a row answers, so this asserts
    // the slot → candidate mapping rather than re-deriving the band's height.
    let topmost = |chooser: &crate::OpenWithChooser| {
        let bottom = bounds.top() + i32::try_from(bounds.height).unwrap_or(i32::MAX);
        (bounds.top()..bottom).find_map(|y| {
            open_with_row_at(
                chooser,
                vp,
                Scale::ONE,
                &theme,
                Point::new(bounds.left() + 4, y),
            )
        })
    };
    assert_eq!(
        topmost(&chooser),
        Some(0),
        "unscrolled, the top row is the first"
    );
    chooser.select(OPEN_WITH_MAX_ROWS + 4);
    assert!(open_with_reveal(&mut chooser, Scale::ONE, &theme, vp));
    assert_eq!(
        chooser.offset(),
        u64::from(row_height(Scale::ONE, &theme)) * 5
    );
    assert_eq!(topmost(&chooser), Some(5), "scrolled, it is the sixth");

    // Off the panel entirely, and on its title band, resolve to nothing.
    assert_eq!(
        open_with_row_at(&chooser, vp, Scale::ONE, &theme, Point::new(-10, -10)),
        None
    );
    assert_eq!(
        open_with_row_at(
            &chooser,
            vp,
            Scale::ONE,
            &theme,
            Point::new(bounds.left() + 4, bounds.top())
        ),
        None,
        "the identity band naming the file is not a row"
    );

    // Drawing is clip-safe: a viewport with no room for the panel paints
    // nothing rather than faulting.
    let mut surface = Surface::new(480, 480).expect("surface");
    draw_open_with_chooser(
        &mut surface,
        &chooser,
        Scale::ONE,
        &theme,
        vp,
        &mut NoArtwork,
    );
    let mut tiny = Surface::new(8, 8).expect("surface");
    draw_open_with_chooser(
        &mut tiny,
        &chooser,
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 8, 8),
        &mut NoArtwork,
    );
}

/// The popup is sized to its content: a chooser with one candidate is one row
/// tall, not eight, and whatever it is sized to is what its list shows.
///
/// Regression: the chooser was always eight rows tall and 80% of the window
/// wide however many candidates there were, while the row count was derived
/// independently from the content height — so the sizing and the list
/// disagreed and a single candidate got a plate of dead space.
#[test]
fn the_open_with_chooser_is_sized_to_its_candidates_and_its_rows_agree() {
    use crate::open_with::{AppAssociation, OpenWithChooser};
    use crate::render::{open_with_chooser_extent, OPEN_WITH_MAX_ROWS};

    let theme = Theme::dark();
    let screen = Rect::new(0, 0, 800, 600);

    let mut last = 0;
    for count in [1usize, 2, 3, OPEN_WITH_MAX_ROWS, OPEN_WITH_MAX_ROWS + 12] {
        let (chooser, vp) = chooser_of(count);
        assert_eq!(chooser.candidates().len(), count);
        let (w, h) = (vp.width, vp.height);
        assert_eq!(
            chooser_rows(&chooser, vp),
            (0..count.min(OPEN_WITH_MAX_ROWS)).collect::<Vec<_>>(),
            "{count} candidates: the popup shows exactly the whole rows its extent was sized for"
        );
        // Growing the list grows the popup, up to the bound.
        if count <= OPEN_WITH_MAX_ROWS {
            assert!(
                h > last,
                "{count} candidates: a shorter list is a shorter popup"
            );
        } else {
            assert_eq!(
                h, last,
                "past the bound the popup stops growing and scrolls"
            );
        }
        last = h;
        // The width is its content's too, and never the letterbox proportion
        // of a screen it is not drawn over: every candidate's row, the title,
        // and the action band fit, and nothing else widens it.
        assert!(
            w < screen.width,
            "{count} candidates: a chooser of short names is not four fifths of the display"
        );
        assert!(
            w >= Scale::ONE.scale_length(200),
            "{count} candidates: nor a sliver"
        );
    }

    // A candidate whose name is longer than the floor widens the popup to hold
    // it; one that would outgrow the screen is clamped to it instead.
    let long = AppAssociation::new(
        "An Application With A Remarkably Long Display Name Indeed",
        "/Apps/Long.app",
        alloc::vec![],
    );
    let refs = alloc::vec![&long];
    let chooser = OpenWithChooser::new(&refs, "/f", "f").expect("one candidate");
    let (wide, _) = open_with_chooser_extent(&chooser, Scale::ONE, &theme, screen);
    let short = AppAssociation::new("A", "/Apps/A.app", alloc::vec![]);
    let refs = alloc::vec![&short];
    let brief = OpenWithChooser::new(&refs, "/f", "f").expect("one candidate");
    let (narrow, _) = open_with_chooser_extent(&brief, Scale::ONE, &theme, screen);
    assert!(wide > narrow, "the width is measured from what it draws");
    let narrow_screen = Rect::new(0, 0, 120, 600);
    let (clamped, _) = open_with_chooser_extent(&chooser, Scale::ONE, &theme, narrow_screen);
    assert!(
        clamped <= narrow_screen.width,
        "a popup never asks to be wider than the screen"
    );
}

/// The chooser's Open and Cancel actions are drawn and hit-tested through one
/// definition, and Open with no current candidate resolves to nothing.
#[test]
fn the_open_with_chooser_offers_open_and_cancel() {
    use crate::open_with::{AppAssociation, OpenWithChooser};
    use crate::render::{
        open_with_action_at, open_with_chooser_extent, open_with_row_at, OpenWithAction,
    };
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let screen = Rect::new(0, 0, 800, 600);
    let app = AppAssociation::new("Viewer", "/Apps/Viewer.app", alloc::vec![]);
    let refs = alloc::vec![&app];
    let chooser = OpenWithChooser::new(&refs, "/f", "f").expect("one candidate");
    let (w, h) = open_with_chooser_extent(&chooser, Scale::ONE, &theme, screen);
    let vp = Rect::new(0, 0, w, h);

    // Both actions are reachable, and each resolves to itself.
    let found: alloc::vec::Vec<OpenWithAction> = (vp.top()..vp.bottom())
        .flat_map(|y| (vp.left()..vp.right()).map(move |x| Point::new(x, y)))
        .filter_map(|at| open_with_action_at(&chooser, vp, Scale::ONE, &theme, at))
        .collect();
    assert!(
        found.contains(&OpenWithAction::Open) && found.contains(&OpenWithAction::Cancel),
        "a chooser that cannot be left by a button is one Escape away from being stuck"
    );

    // The action band is not a candidate row, so a press on Open never also
    // selects a row.
    let on_open = (vp.top()..vp.bottom())
        .flat_map(|y| (vp.left()..vp.right()).map(move |x| Point::new(x, y)))
        .find(|at| {
            open_with_action_at(&chooser, vp, Scale::ONE, &theme, *at) == Some(OpenWithAction::Open)
        })
        .expect("Open is drawn");
    assert_eq!(
        open_with_row_at(&chooser, vp, Scale::ONE, &theme, on_open),
        None
    );
}

/// The quick submenu offers the highest-ranked candidates a plate can hold,
/// and the complete chooser still holds the rest.
#[test]
fn the_quick_open_with_candidates_are_the_top_of_the_ranked_list() {
    use crate::open_with::{quick_applications, AppAssociation, OPEN_WITH_QUICK_MAX};

    let apps: alloc::vec::Vec<AppAssociation> = (0..OPEN_WITH_QUICK_MAX + 5)
        .map(|n| AppAssociation::new(alloc::format!("App{n}"), "/Apps/A.app", alloc::vec![]))
        .collect();
    let ranked: alloc::vec::Vec<&AppAssociation> = apps.iter().collect();
    let quick = quick_applications(&ranked);
    assert_eq!(quick.len(), OPEN_WITH_QUICK_MAX, "bounded by the plate");
    let names: alloc::vec::Vec<&str> = quick.iter().map(|app| app.name()).collect();
    assert_eq!(
        names,
        ranked
            .iter()
            .take(OPEN_WITH_QUICK_MAX)
            .map(|app| app.name())
            .collect::<alloc::vec::Vec<&str>>(),
        "in the ranked order, so the first row is the best match"
    );
    // A shorter list is offered whole, and an empty one offers nothing.
    let two: alloc::vec::Vec<&AppAssociation> = apps.iter().take(2).collect();
    assert_eq!(quick_applications(&two).len(), 2);
    assert!(quick_applications(&[]).is_empty());
}

/// The Rename row carries a pre-filled field and the "Open With…" row carries
/// the candidates, each only where its own row is actionable.
#[test]
fn the_context_menu_offers_its_quick_actions_only_where_the_row_can_act() {
    use crate::chrome::{
        context_menu, ContextCommand, ContextMenuModel, ContextQuick, CONTEXT_COMMANDS,
    };
    use crate::open_with::AppAssociation;
    use tairix_abi::window_ipc::AppMenuRowView;

    let viewer = AppAssociation::new("Viewer", "/Apps/Viewer.app", alloc::vec![]);
    let candidates = alloc::vec![&viewer];
    let mut browser = Browser::open_root(activation_source()).expect("root");
    browser.select(2).expect("select notes.txt");
    let model = ContextMenuModel::for_browser(&browser, true);
    let quick = ContextQuick {
        name: "notes.txt",
        candidates: &candidates,
        documents: &[],
    };
    let menu = context_menu(model, "Files", quick).expect("the rows fit");

    let rows: alloc::vec::Vec<(AppMenuRowView<'_>, Option<usize>)> = menu.rows().collect();
    let rename = rows
        .iter()
        .find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.label == ContextCommand::Rename.label() => {
                Some(*item)
            }
            _ => None,
        })
        .expect("the Rename row");
    let field = rename.entry.expect("it carries a field");
    assert_eq!(
        field.initial, "notes.txt",
        "pre-filled with the current name"
    );
    assert_ne!(field.id, rename.id, "and answering an id of its own");

    // The candidate is filed under the Open With… row, carries its bundle,
    // and the commands all come first.
    let open_with = rows
        .iter()
        .position(|(row, _)| {
            matches!(row, AppMenuRowView::Item(item) if item.label == ContextCommand::OpenWith.label())
        })
        .expect("the Open With… row");
    let (candidate, parent) = rows
        .iter()
        .find(|(row, _)| matches!(row, AppMenuRowView::Item(item) if item.label == "Viewer"))
        .expect("the candidate row");
    assert_eq!(*parent, Some(open_with));
    let AppMenuRowView::Item(candidate) = candidate else {
        panic!("the candidate is an item");
    };
    assert_eq!(candidate.icon_bundle, "/Apps/Viewer.app");
    let last_command = rows
        .iter()
        .rposition(|(row, _)| {
            matches!(row, AppMenuRowView::Item(item)
                if CONTEXT_COMMANDS.iter().any(|c| c.label() == item.label))
        })
        .expect("a command row");
    assert!(
        rows.iter().position(
            |(row, _)| matches!(row, AppMenuRowView::Item(item) if item.label == "Viewer")
        ) > Some(last_command),
        "every command is pushed before any candidate, so a long list crowds none off the plate"
    );

    // An empty directory makes both rows inactionable, so neither quick
    // action is offered — a field could not commit a rename that cannot
    // happen, and a submenu could not open a file that is not selected.
    let mut empty = Browser::open_root(MockFs::fixture()).expect("root");
    empty.open_index(2).expect("enter System");
    empty.open_index(0).expect("enter the empty Fonts");
    let menu = context_menu(ContextMenuModel::for_browser(&empty, false), "Files", quick)
        .expect("the rows fit");
    let rows: alloc::vec::Vec<_> = menu.rows().collect();
    let open_with = rows
        .iter()
        .position(|(row, _)| {
            matches!(row, AppMenuRowView::Item(item) if item.label == ContextCommand::OpenWith.label())
        })
        .expect("the Open With… row");
    assert!(
        rows.iter().all(|(row, parent)| {
            *parent != Some(open_with)
                && match row {
                    AppMenuRowView::Item(item) => item.entry.is_none(),
                    _ => true,
                }
        }),
        "no field and no candidate where no row can act"
    );
}

// ---------------------------------------------------------------------------
// FM4b — the drawn toolbar: command dispatch, glyphs, and pointer resolution.
// ---------------------------------------------------------------------------

#[test]
fn view_mode_toggled_swaps_list_and_grid() {
    use crate::layout::ViewMode;
    assert_eq!(ViewMode::List.toggled(), ViewMode::Grid);
    assert_eq!(ViewMode::Grid.toggled(), ViewMode::List);
}

#[test]
fn sort_mode_next_cycles_through_all_eight_modes_and_wraps() {
    use crate::sort::SortMode;
    // The Sort command walks every (key, direction) once, in a fixed order,
    // then returns to the start — a total cycle with no unreachable mode.
    let start = SortMode::default_order();
    let mut mode = start;
    let mut seen = Vec::new();
    for _ in 0..8 {
        seen.push(mode);
        mode = mode.next();
    }
    // Back to the start after eight steps.
    assert_eq!(mode, start);
    // All eight are distinct.
    for (i, a) in seen.iter().enumerate() {
        for b in &seen[i + 1..] {
            assert_ne!(a, b, "sort cycle repeated a mode early");
        }
    }
    assert_eq!(seen.len(), 8);
}

#[test]
fn toolbar_command_icon_maps_each_command_to_a_distinct_glyph() {
    use crate::chrome::{ToolbarCommand, TOOLBAR_COMMANDS};
    use tairix_icon::IconKind;

    // Each command draws its own glyph and no two share one, so the toolbar
    // reads unambiguously.
    assert_eq!(ToolbarCommand::Back.icon(), IconKind::NavBack);
    assert_eq!(ToolbarCommand::Forward.icon(), IconKind::NavForward);
    assert_eq!(ToolbarCommand::Up.icon(), IconKind::NavUp);
    assert_eq!(ToolbarCommand::Refresh.icon(), IconKind::Refresh);
    assert_eq!(ToolbarCommand::ToggleView.icon(), IconKind::ViewToggle);
    assert_eq!(ToolbarCommand::Sort.icon(), IconKind::Sort);

    let icons: Vec<IconKind> = TOOLBAR_COMMANDS.iter().map(|c| c.icon()).collect();
    for (i, a) in icons.iter().enumerate() {
        for b in &icons[i + 1..] {
            assert_ne!(a, b, "two toolbar commands share a glyph");
        }
    }
}

#[test]
fn apply_command_drives_navigation_view_and_sort() {
    use crate::apply_command;
    use crate::chrome::ToolbarCommand;
    use crate::layout::ViewMode;

    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Back at the root has no history: a no-op, not an error, and no change.
    assert_eq!(apply_command(&mut browser, ToolbarCommand::Back), Ok(false));

    // Toggle view flips list ↔ grid and reports a change.
    assert_eq!(browser.view_mode(), ViewMode::List);
    assert_eq!(
        apply_command(&mut browser, ToolbarCommand::ToggleView),
        Ok(true)
    );
    assert_eq!(browser.view_mode(), ViewMode::Grid);

    // Sort advances to the next mode in the cycle.
    let before = browser.sort_mode();
    assert_eq!(apply_command(&mut browser, ToolbarCommand::Sort), Ok(true));
    assert_eq!(browser.sort_mode(), before.next());

    // Descend, then Up climbs back to the root. (The Sort above re-ordered the
    // listing, so find System by name rather than a fixed index.)
    let sys = browser
        .entries()
        .iter()
        .position(|e| e.name() == "System")
        .expect("fixture has System");
    browser.open_index(sys).expect("enter System");
    assert_eq!(browser.path(), "/System");
    assert_eq!(apply_command(&mut browser, ToolbarCommand::Up), Ok(true));
    assert!(browser.is_root());

    // Back now returns to /System (there is history), reporting a change.
    assert_eq!(apply_command(&mut browser, ToolbarCommand::Back), Ok(true));
    assert_eq!(browser.path(), "/System");
}

#[test]
fn apply_command_refresh_fails_closed_and_leaves_the_browser_put() {
    use crate::apply_command;
    use crate::chrome::ToolbarCommand;

    // The root lists once (at open) and is refused on every later read.
    let mut fs = MockFs::fixture();
    fs.deny_after_first.insert("/".to_string());
    let mut browser = Browser::open_root(fs).expect("root lists once");
    let before: Vec<String> = names(&browser).iter().map(ToString::to_string).collect();

    // Refresh re-reads the root, which now fails closed; the error is surfaced
    // and the previously loaded listing is left exactly as it was.
    assert!(matches!(
        apply_command(&mut browser, ToolbarCommand::Refresh),
        Err(BrowseError::Source(_))
    ));
    assert_eq!(names(&browser), before);
}

#[test]
fn render_toolbar_command_at_resolves_enabled_commands_and_fails_closed() {
    use crate::chrome::ToolbarCommand;
    use crate::render::{chrome_height, toolbar_command_at, toolbar_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 400, chrome_height(Scale::ONE, &theme, BAND) + 40);

    // Scan the toolbar strip's middle row and collect every command a click
    // resolves to, so the test does not depend on each tool's exact pixel x.
    let commands_along_toolbar = |browser: &Browser<MockFs>| -> Vec<ToolbarCommand> {
        let y = i32::try_from(toolbar_height(Scale::ONE, &theme) / 2).unwrap();
        let mut found = Vec::new();
        for x in 0..vp.width {
            if let Some(cmd) = toolbar_command_at(
                browser,
                Scale::ONE,
                &theme,
                vp,
                BAND,
                Point::new(i32::try_from(x).unwrap(), y),
            ) {
                if !found.contains(&cmd) {
                    found.push(cmd);
                }
            }
        }
        found
    };

    // At the root the three navigation tools are disabled, so a click on them
    // resolves to nothing (fail closed); the always-enabled tools resolve.
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let at_root = commands_along_toolbar(&browser);
    assert!(!at_root.contains(&ToolbarCommand::Back));
    assert!(!at_root.contains(&ToolbarCommand::Forward));
    assert!(!at_root.contains(&ToolbarCommand::Up));
    assert!(at_root.contains(&ToolbarCommand::Refresh));
    assert!(at_root.contains(&ToolbarCommand::ToggleView));
    assert!(at_root.contains(&ToolbarCommand::Sort));

    // After descending, Back and Up become enabled and now resolve.
    let mut deep = Browser::open_root(MockFs::fixture()).expect("root");
    deep.open_index(2).expect("enter System");
    let at_deep = commands_along_toolbar(&deep);
    assert!(at_deep.contains(&ToolbarCommand::Back));
    assert!(at_deep.contains(&ToolbarCommand::Up));
    assert!(!at_deep.contains(&ToolbarCommand::Forward));

    // A click below the toolbar strip (the path bar / item area) is never a
    // toolbar command.
    assert_eq!(
        toolbar_command_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(
                4,
                i32::try_from(toolbar_height(Scale::ONE, &theme)).unwrap()
            )
        ),
        None
    );
}

#[test]
fn render_manager_tool_at_resolves_new_folder_disjoint_from_the_read_only_commands() {
    use crate::chrome::{ManagerTool, ManagerToolModel, MANAGER_TOOLS};
    use crate::render::{manager_tool_at, toolbar_command_at, toolbar_height};
    use tairix_geometry::Point;

    // Every manager tool enabled, so the scan sees the full write-tool set
    // (the Empty Trash tool's own enable state is exercised separately).
    let tool_model = ManagerToolModel::new(true);

    let theme = Theme::dark();
    let vp = Rect::new(
        0,
        0,
        400,
        crate::render::chrome_height(Scale::ONE, &theme, BAND) + 40,
    );
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let y = i32::try_from(toolbar_height(Scale::ONE, &theme) / 2).unwrap();

    // Scan the toolbar's middle row: the manager write tool resolves somewhere,
    // and no pixel resolves to *both* a read-only command and a write tool —
    // the two hit-tests cover disjoint regions.
    let mut saw_new_folder = false;
    for x in 0..vp.width {
        let point = Point::new(i32::try_from(x).unwrap(), y);
        let command = toolbar_command_at(&browser, Scale::ONE, &theme, vp, BAND, point);
        let tool = manager_tool_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            point,
            MANAGER_TOOLS,
            tool_model,
        );
        assert!(
            !(command.is_some() && tool.is_some()),
            "a pixel resolved to both a command and a write tool"
        );
        if tool == Some(ManagerTool::NewFolder) {
            saw_new_folder = true;
        }
    }
    assert!(
        saw_new_folder,
        "the New Folder tool is drawn and hit-testable"
    );

    // The read-only picker hands no write tools, so none is ever resolved —
    // the type separation keeps a write action out of the picker entirely.
    for x in 0..vp.width {
        let point = Point::new(i32::try_from(x).unwrap(), y);
        assert_eq!(
            manager_tool_at(
                &browser,
                Scale::ONE,
                &theme,
                vp,
                BAND,
                point,
                &[],
                tool_model
            ),
            None
        );
    }

    // A click below the toolbar strip is never a write tool either.
    assert_eq!(
        manager_tool_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            Point::new(
                4,
                i32::try_from(toolbar_height(Scale::ONE, &theme)).unwrap()
            ),
            MANAGER_TOOLS,
            tool_model,
        ),
        None
    );
}

#[test]
fn render_manager_tool_rect_is_the_forward_mirror_of_manager_tool_at() {
    use crate::chrome::{ManagerTool, ManagerToolModel, MANAGER_TOOLS};
    use crate::render::{manager_tool_at, manager_tool_rect};
    use tairix_geometry::Point;

    let tool_model = ManagerToolModel::new(true);

    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 400, 200);
    let browser = Browser::open_root(MockFs::fixture()).expect("root");

    // The New Folder tool has a rect, and that rect's centre hit-tests back
    // to exactly the New Folder tool — paint, hit-test, and aim geometry are
    // one definition.
    let rect = manager_tool_rect(
        &browser,
        Scale::ONE,
        &theme,
        vp,
        BAND,
        MANAGER_TOOLS,
        ManagerTool::NewFolder,
    )
    .expect("the New Folder tool is laid out");
    let centre = Point::new(
        rect.left() + i32::try_from(rect.width).unwrap() / 2,
        rect.top() + i32::try_from(rect.height).unwrap() / 2,
    );
    assert_eq!(
        manager_tool_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            centre,
            MANAGER_TOOLS,
            tool_model
        ),
        Some(ManagerTool::NewFolder),
    );

    // The read-only picker (no write tools) never lays a write tool out.
    assert_eq!(
        manager_tool_rect(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            &[],
            ManagerTool::NewFolder
        ),
        None,
    );
}

#[test]
fn render_manager_tool_at_gates_empty_trash_on_the_model() {
    use crate::chrome::{ManagerTool, ManagerToolModel, MANAGER_TOOLS};
    use crate::render::{manager_tool_at, toolbar_height};
    use tairix_geometry::Point;

    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 400, 200);
    let browser = Browser::open_root(MockFs::fixture()).expect("root");
    let y = i32::try_from(toolbar_height(Scale::ONE, &theme) / 2).unwrap();

    // With the model reporting the current directory is *not* a populated
    // Trash, the Empty Trash tool is drawn (its rect exists) but renders
    // disabled, so a click on it resolves to nothing — fail closed. The
    // always-enabled New Folder tool still resolves.
    let disabled = ManagerToolModel::new(false);
    let mut saw_new_folder = false;
    for x in 0..vp.width {
        let point = Point::new(i32::try_from(x).unwrap(), y);
        let tool = manager_tool_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            point,
            MANAGER_TOOLS,
            disabled,
        );
        assert_ne!(
            tool,
            Some(ManagerTool::EmptyTrash),
            "a disabled Empty Trash tool must never resolve to an action"
        );
        if tool == Some(ManagerTool::NewFolder) {
            saw_new_folder = true;
        }
    }
    assert!(saw_new_folder, "New Folder stays actionable");

    // With the model reporting a populated Trash, the same pixels resolve the
    // Empty Trash tool — it is enabled in exactly the same place it was drawn.
    let enabled = ManagerToolModel::new(true);
    let mut saw_empty_trash = false;
    for x in 0..vp.width {
        let point = Point::new(i32::try_from(x).unwrap(), y);
        if manager_tool_at(
            &browser,
            Scale::ONE,
            &theme,
            vp,
            BAND,
            point,
            MANAGER_TOOLS,
            enabled,
        ) == Some(ManagerTool::EmptyTrash)
        {
            saw_empty_trash = true;
        }
    }
    assert!(
        saw_empty_trash,
        "an enabled Empty Trash tool is hit-testable"
    );
}

#[test]
fn browser_navigate_to_jumps_to_an_off_spine_location_and_records_history() {
    // Start at `/System`; `/Users` is neither an ancestor nor a child of it,
    // so only the jump-to-arbitrary-location primitive can reach it.
    let start = crate::vfs::components_from_absolute_path("/System").expect("valid path");
    let mut browser = Browser::open_at(MockFs::fixture(), start).expect("System lists");
    assert_eq!(browser.path(), "/System");
    assert!(!browser.can_go_back());

    let moved = browser
        .navigate_to(vec!["Users".to_string()])
        .expect("Users lists");
    assert!(moved);
    assert_eq!(browser.path(), "/Users");
    assert_eq!(names(&browser), ["alice"]);
    // History records the move like any navigation, so Back returns to where
    // the jump started.
    assert!(browser.can_go_back());
    assert!(browser.go_back().expect("back to System"));
    assert_eq!(browser.path(), "/System");
}

#[test]
fn browser_navigate_to_the_current_directory_is_a_no_op() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Navigating to the directory already shown changes nothing and records no
    // history — a no-op, not an error.
    let moved = browser.navigate_to(Vec::new()).expect("no-op");
    assert!(!moved);
    assert!(!browser.can_go_back());
    assert_eq!(browser.path(), "/");
}

#[test]
fn browser_navigate_to_an_unlistable_location_fails_closed() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // `/System/Security` exists but is capability-denied; the jump is refused
    // and the browser stays exactly where it was, with no history recorded.
    let target = crate::vfs::components_from_absolute_path("/System/Security").expect("valid");
    let result = browser.navigate_to(target);
    assert_eq!(
        result.err(),
        Some(BrowseError::Source(Errno::PermissionDenied))
    );
    assert_eq!(browser.path(), "/");
    assert!(!browser.can_go_back());
}

#[test]
fn a_new_folder_takes_the_first_free_number() {
    use crate::create::{NewEntry, NEW_FOLDER_BASE};

    let folder = |siblings: &[Entry]| NewEntry::Folder.suggest_name(siblings);
    assert_eq!(folder(&[]), NEW_FOLDER_BASE);
    assert_eq!(
        folder(&[Entry::directory("Documents"), Entry::file("notes.txt")]),
        NEW_FOLDER_BASE
    );
    assert_eq!(folder(&[Entry::directory(NEW_FOLDER_BASE)]), "New Folder 2");
    assert_eq!(
        folder(&[
            Entry::directory(NEW_FOLDER_BASE),
            Entry::directory("New Folder 2"),
            Entry::directory("New Folder 3"),
        ]),
        "New Folder 4"
    );
    // A gap is filled by the smallest free number, not the next after the
    // largest.
    assert_eq!(
        folder(&[
            Entry::directory(NEW_FOLDER_BASE),
            Entry::directory("New Folder 3"),
        ]),
        "New Folder 2"
    );
    // A file holds a name as surely as a folder does.
    assert_eq!(folder(&[Entry::file(NEW_FOLDER_BASE)]), "New Folder 2");
}

#[test]
fn only_the_exact_spelling_of_a_number_holds_it() {
    use crate::create::{NewEntry, NEW_FOLDER_BASE};

    // None of these is "New Folder 2", so none keeps it from being offered.
    let near_misses = [
        Entry::directory(NEW_FOLDER_BASE),
        Entry::directory("New Folder 02"),
        Entry::directory("New Folder  2"),
        Entry::directory("New Folder 2a"),
        Entry::directory("New Folder +2"),
        Entry::directory("New Folder "),
        Entry::directory("New Folders 2"),
        Entry::directory("new folder 2"),
        Entry::directory("New Folder 1"),
        Entry::directory("New Folder 99999999999999999999999999"),
    ];
    assert_eq!(NewEntry::Folder.suggest_name(&near_misses), "New Folder 2");
}

#[test]
fn a_new_document_is_numbered_before_its_extension() {
    use crate::create::NewEntry;
    use crate::media::{BlankDocument, MediaType};

    let text = BlankDocument::of(MediaType::TextPlain).expect("text starts empty");
    let document = |siblings: &[Entry]| NewEntry::Document(text).suggest_name(siblings);
    assert_eq!(document(&[]), "New Text Document.txt");
    // A folder of the bare stem does not hold the document's name.
    assert_eq!(
        document(&[Entry::directory("New Text Document")]),
        "New Text Document.txt"
    );
    assert_eq!(
        document(&[
            Entry::file("New Text Document.txt"),
            Entry::file("New Text Document 2.txt"),
        ]),
        "New Text Document 3.txt"
    );
    // Another extension is another name.
    assert_eq!(
        document(&[Entry::file("New Text Document.md")]),
        "New Text Document.txt"
    );
}

#[test]
fn a_listing_holding_every_number_gets_the_one_past_it() {
    use alloc::format;

    use crate::create::{NewEntry, NEW_FOLDER_BASE};

    let count = 20_000;
    let mut siblings = alloc::vec![Entry::directory(NEW_FOLDER_BASE)];
    siblings.extend(
        (2..=count)
            .rev()
            .map(|n| Entry::directory(format!("New Folder {n}"))),
    );
    assert_eq!(
        NewEntry::Folder.suggest_name(&siblings),
        format!("New Folder {}", count + 1)
    );
}

// --- FM8b: the Properties window's facts + chosen_target_path -------------

use crate::properties::Properties;
use crate::render::{general_facts, mode_reading_for_test};
use tairix_abi::fs::{FileId, FileStat};
use tairix_abi::NodeTimes;

/// A `FileStat` fixture with the given kind and mode; a 1.5 KiB file taking
/// 4 KiB on disk, owned by uid 1000 / gid 100, with a kept access time left at
/// the epoch (so the blank-stamp path is exercised).
fn props_stat(kind: FileKind, mode: u32) -> FileStat {
    FileStat {
        kind,
        nlink: 1,
        size: 1536,
        allocated: 4096,
        mode,
        uid: 1000,
        gid: 100,
        id: FileId::NONE,
        times: NodeTimes {
            created: Time64::from_secs(1_609_459_200),
            modified: Time64::from_secs(1_609_459_200 + 3661),
            accessed: Time64::UNIX_EPOCH,
            changed: Time64::from_secs(1_700_000_000),
        },
        content_gen: 0,
    }
}

#[test]
fn the_general_section_states_every_fact_in_order_from_the_model() {
    let props = Properties::from_stat(
        "notes.txt",
        crate::entry::EntryKind::File,
        &props_stat(FileKind::Regular, 0o644),
    );
    let facts = general_facts(&props);
    // A file stores no target, so it shows no alias row.
    let labels: Vec<&str> = facts.iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels,
        ["Kind", "Size", "Created", "Modified", "Accessed", "Changed"]
    );
    let value = |name: &str| -> String {
        facts
            .iter()
            .find(|(label, _)| *label == name)
            .map(|(_, v)| v.clone())
            .expect("fact present")
    };
    assert_eq!(value("Kind"), "File");
    assert_eq!(value("Size"), "1.5 KiB (4.0 KiB on disk)");
    assert_eq!(value("Modified"), "2021-01-01 01:01:01");
    // A stamp the backing does not keep renders blank, never a fabricated
    // wall time.
    assert_eq!(value("Accessed"), "");
    assert_eq!(mode_reading_for_test(&props), "-rw-r--r-- (0644)");
}

#[test]
fn a_bundle_reads_as_an_application_yet_keeps_a_directory_mode() {
    // A `<Name>.app` bundle is labelled "Application" but is a directory on
    // disk, so the mode reading still leads with `d`.
    let props = Properties::from_stat(
        "Editor.app",
        crate::entry::EntryKind::Bundle,
        &props_stat(FileKind::Directory, 0o755),
    );
    let facts = general_facts(&props);
    let kind = facts
        .iter()
        .find(|(label, _)| *label == "Kind")
        .map(|(_, value)| value.clone());
    assert_eq!(kind.as_deref(), Some("Application"));
    assert_eq!(mode_reading_for_test(&props), "drwxr-xr-x (0755)");
}

/// An alias says what it points at. Without the row a broken one gave a reader
/// nothing to explain why it is broken, and a working one nothing to say where
/// it goes.
#[test]
fn the_general_section_shows_where_an_alias_points_and_only_for_an_alias() {
    use crate::entry::LinkTarget;

    let props = Properties::from_stat(
        "Documents",
        crate::entry::EntryKind::Link(LinkTarget::Directory),
        &props_stat(FileKind::Symlink, 0o777),
    )
    .with_target("../Storage/docs");
    let facts = general_facts(&props);
    let labels: Vec<&str> = facts.iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels.first().copied(),
        Some("Kind"),
        "the kind still leads"
    );
    assert_eq!(
        labels.get(1).copied(),
        Some("Alias to"),
        "and the target reads beside it"
    );
    let target = facts
        .iter()
        .find(|(label, _)| *label == "Alias to")
        .expect("the row");
    assert_eq!(
        target.1, "../Storage/docs",
        "the spelling the link holds, verbatim"
    );

    // A link with no target attached shows no row rather than an empty one.
    let unattached = Properties::from_stat(
        "gone",
        crate::entry::EntryKind::Link(LinkTarget::Dangling),
        &props_stat(FileKind::Symlink, 0o777),
    );
    assert!(general_facts(&unattached)
        .iter()
        .all(|(label, _)| *label != "Alias to"));
}

#[test]
fn chosen_target_path_spells_the_chosen_node_and_is_none_without_one() {
    let mut browser = Browser::open_root(MockFs::fixture()).expect("root");
    // Default sort: the four view-binding directories in name order.
    browser
        .select(
            browser
                .entries()
                .iter()
                .position(|e| e.name() == "System")
                .expect("System listed"),
        )
        .expect("select System");
    assert_eq!(
        browser.chosen_target_path(),
        Some(Ok("/System".to_string()))
    );

    // Nested: the path reflects the current directory, not just the leaf.
    let system = browser
        .entries()
        .iter()
        .position(|e| e.name() == "System")
        .expect("System listed");
    browser.open_index(system).expect("descend into System");
    assert_eq!(browser.chosen_target_path(), None, "nothing chosen yet");
    browser.select(0).expect("the first entry");
    let first = focused(&browser)
        .map(Entry::name)
        .expect("a selection")
        .to_string();
    assert_eq!(
        browser.chosen_target_path(),
        Some(Ok(alloc::format!("/System/{first}")))
    );

    // The empty /System/Fonts has no selection, hence no target path.
    let mut b2 = Browser::open_root(MockFs::fixture()).expect("root");
    b2.open_index(
        b2.entries()
            .iter()
            .position(|e| e.name() == "System")
            .unwrap(),
    )
    .expect("enter System");
    b2.open_index(
        b2.entries()
            .iter()
            .position(|e| e.name() == "Fonts")
            .unwrap(),
    )
    .expect("enter Fonts");
    assert_eq!(b2.chosen_target_path(), None);
}

// --- The Properties window: fields, toggles, ownership, attributes --------

use crate::properties::{Attribute, Attributes};
use crate::render::{
    draw_properties_window, permission_cells, properties_attr_editor_rect, properties_hit,
    properties_owner_editor_rect, properties_permissions_key, properties_reveal,
    properties_scroll_pointer, properties_scroll_wheel, properties_window_extent, AttrAction,
    Identity, OwnerField, PermsCursor, PermsKeyed, PropertiesControls, PropertiesFrame,
    PropertiesTab, PropertiesTarget, PropertiesView, PERMISSION_BITS,
};
use crate::ScrollColumn;
use tairix_controls::text::TextField;
use tairix_geometry::Point;
use tairix_input::{Key, NamedKey};

/// The window the Properties surface is laid out in for these tests: the
/// extent it actually opens at, so what is asserted is what a user sees.
fn props_window() -> Rect {
    let (w, h) = properties_window_extent(Scale::ONE, &Theme::dark());
    Rect::new(0, 0, w, h)
}

/// A file with the attribute set a test needs.
fn props_with(attributes: Attributes) -> Properties {
    Properties::from_stat(
        "notes.txt",
        crate::entry::EntryKind::File,
        &props_stat(FileKind::Regular, 0o644),
    )
    .with_attributes(attributes)
}

/// `count` attributes, keyed apart so a row can be told from its neighbours.
fn some_attributes(count: usize) -> Attributes {
    Attributes::Visible(
        (0..count)
            .map(|n| {
                Attribute::new(
                    alloc::format!("user.k{n}"),
                    alloc::format!("value {n}").into_bytes(),
                )
            })
            .collect(),
    )
}

/// What the identity band of the window these tests lay out names.
fn props_identity() -> Identity<'static> {
    Identity {
        name: "notes.txt",
        detail: "Text document · 0 B",
        art: IconRequest::kind(IconKind::Text),
    }
}

/// The controls a window with nothing being typed into draws.
fn resting<'a>(bar: &'a ScrollColumn, editor: &'a TextField) -> PropertiesControls<'a> {
    PropertiesControls {
        identity: props_identity(),
        can_chown: true,
        owner: None,
        attribute: editor,
        scrollbar: bar.scrollbar(),
    }
}

/// What a scan of every section found, grouped by the kind of control.
struct Scanned {
    /// Each permission toggle's bounding box, taken from the hit-test itself —
    /// so this measures the grid a user can actually press.
    toggles: BTreeMap<u32, (i32, i32, i32, i32)>,
    owners: BTreeSet<OwnerField>,
    rows: BTreeSet<usize>,
    actions: BTreeSet<AttrAction>,
    tabs: BTreeSet<PropertiesTab>,
    editor_points: usize,
}

/// Scan every section of the window, gathering what each resolved and
/// asserting as it goes that no section resolved another's controls.
///
/// `editor` is the attribute editor's rectangle, so an action button resolved
/// over it can be caught where it is found.
fn scan_every_section(props: &Properties, window: Rect, editor: Rect) -> Scanned {
    let mut found = Scanned {
        toggles: BTreeMap::new(),
        owners: BTreeSet::new(),
        rows: BTreeSet::new(),
        actions: BTreeSet::new(),
        tabs: BTreeSet::new(),
        editor_points: 0,
    };
    for tab in PropertiesTab::ALL {
        let view = PropertiesView {
            tab,
            ..PropertiesView::default()
        };
        for (at, target) in scan(props, view, window) {
            match target {
                PropertiesTarget::Tab(named) => {
                    found.tabs.insert(named);
                }
                PropertiesTarget::Permission(bit) => {
                    assert_eq!(tab, PropertiesTab::Permissions, "a toggle off its own tab");
                    assert!(PERMISSION_BITS.contains(&bit), "resolved a non-grid bit");
                    let cell = found.toggles.entry(bit).or_insert((at.x, at.y, at.x, at.y));
                    cell.0 = cell.0.min(at.x);
                    cell.1 = cell.1.min(at.y);
                    cell.2 = cell.2.max(at.x);
                    cell.3 = cell.3.max(at.y);
                }
                PropertiesTarget::Owner(which) => {
                    assert_eq!(tab, PropertiesTab::Permissions, "an owner off its own tab");
                    found.owners.insert(which);
                }
                PropertiesTarget::Attribute(index) => {
                    assert_eq!(tab, PropertiesTab::Attributes, "a row off its own tab");
                    found.rows.insert(index);
                }
                PropertiesTarget::Action(action) => {
                    assert_eq!(tab, PropertiesTab::Attributes, "an action off its own tab");
                    found.actions.insert(action);
                    assert!(
                        at.x >= editor.left() + i32::try_from(editor.width).unwrap(),
                        "an action button overlaps the editor it sits beside"
                    );
                }
                PropertiesTarget::Editor => {
                    assert_eq!(tab, PropertiesTab::Attributes, "the editor off its own tab");
                    found.editor_points += 1;
                }
            }
        }
    }
    found
}

/// Every target the window resolves over its whole surface, in scan order.
fn scan(props: &Properties, view: PropertiesView, window: Rect) -> Vec<(Point, PropertiesTarget)> {
    let theme = Theme::dark();
    let bar = ScrollColumn::new();
    let editor = TextField::new();
    let controls = resting(&bar, &editor);
    let mut found = Vec::new();
    for y in 0..i32::try_from(window.height).unwrap() {
        for x in 0..i32::try_from(window.width).unwrap() {
            let at = Point::new(x, y);
            if let Some(target) =
                properties_hit(props, view, controls, window, Scale::ONE, &theme, at)
            {
                found.push((at, target));
            }
        }
    }
    found
}

/// One scan of each drawn section, asserting everything the window's geometry
/// must get right at once.
///
/// Every point of the client is resolved on every tab, which is the only way
/// to show that each control is reachable, that none overlaps another, that a
/// press on nothing resolves to nothing, and that a section resolves *only*
/// its own controls — a press cannot reach a toggle on a tab the user is not
/// looking at. It is one test rather than five because the scan is what costs
/// — the font client memoises behind a process-wide lock, so several of these
/// serialise against each other and each pays for the rest.
#[test]
fn each_section_resolves_every_control_it_draws_and_nothing_else() {
    let props = props_with(some_attributes(3));
    let window = props_window();
    let theme = Theme::dark();
    let field = properties_attr_editor_rect(window, Scale::ONE, &theme).expect("a field");

    let Scanned {
        toggles,
        owners,
        rows,
        actions,
        tabs,
        editor_points,
    } = scan_every_section(&props, window, field);

    assert_eq!(
        tabs,
        PropertiesTab::ALL.into_iter().collect(),
        "every section is reachable from every other"
    );
    assert_eq!(
        toggles.len(),
        PERMISSION_BITS.len(),
        "every settable bit has a reachable toggle"
    );
    for cell in toggles.values() {
        assert!(
            cell.2 - cell.0 >= 3 && cell.3 - cell.1 >= 3,
            "checkbox too small to hit"
        );
    }
    // The layout this replaced crammed nine boxes one glyph apart, so they
    // piled on top of one another.
    let cells: Vec<&(i32, i32, i32, i32)> = toggles.values().collect();
    for (i, a) in cells.iter().enumerate() {
        for b in &cells[i + 1..] {
            let disjoint_x = a.2 < b.0 || b.2 < a.0;
            let disjoint_y = a.3 < b.1 || b.3 < a.1;
            assert!(disjoint_x || disjoint_y, "permission checkboxes overlap");
        }
    }
    assert_eq!(
        owners,
        [OwnerField::Uid, OwnerField::Gid].into_iter().collect(),
        "both owning ids are reachable"
    );
    assert_eq!(rows, [0, 1, 2].into_iter().collect());
    assert_eq!(
        actions,
        [AttrAction::Set, AttrAction::Remove].into_iter().collect(),
        "a set the user cannot apply and a row they cannot remove are both dead ends"
    );
    assert!(editor_points > 0, "the field must be clickable to focus");

    // A press outside the client resolves nothing.
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    assert_eq!(
        properties_hit(
            &props,
            PropertiesView::default(),
            resting(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            Point::new(-5, -5)
        ),
        None
    );
}

/// The controls a session without `CAP_FS_CHOWN` draws, with nothing being
/// typed into.
fn refused<'a>(bar: &'a ScrollColumn, editor: &'a TextField) -> PropertiesControls<'a> {
    PropertiesControls {
        can_chown: false,
        ..resting(bar, editor)
    }
}

/// A session that may not reassign an owner is shown the cell refused, so a
/// press on it resolves to nothing rather than opening an editor whose commit
/// could only be refused.
#[test]
fn an_ownership_value_resolves_only_for_a_session_that_may_reassign_it() {
    let props = props_with(Attributes::Unsupported);
    let window = props_window();
    let theme = Theme::dark();
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let view = PropertiesView {
        tab: PropertiesTab::Permissions,
        ..PropertiesView::default()
    };
    let cell = properties_owner_editor_rect(
        &props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Uid,
    )
    .expect("the row fits the window it opens at");
    let at = Point::new(cell.left() + 2, cell.top() + 2);
    assert_eq!(
        properties_hit(
            &props,
            view,
            resting(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            at
        ),
        Some(PropertiesTarget::Owner(OwnerField::Uid))
    );
    assert_eq!(
        properties_hit(
            &props,
            view,
            refused(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            at
        ),
        None,
        "without CAP_FS_CHOWN the cell is refused, not a control"
    );
    // The capability-free toggles stay reachable either way.
    let perm = scan_first_permission(&props, view, window);
    assert!(
        properties_hit(
            &props,
            view,
            refused(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            perm
        )
        .is_some(),
        "a mode bit is editable without the chown capability"
    );
}

/// Regression: a press on the id editor being typed into resolved to its
/// ownership cell, and opening that cell again reset the editor to the stored
/// id — so clicking into the field threw the typing away.
#[test]
fn a_press_on_the_open_id_editor_leaves_the_typing_alone() {
    let props = props_with(Attributes::Unsupported);
    let window = props_window();
    let theme = Theme::dark();
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let typing = TextField::new().with_text("10");
    let editing = PropertiesControls {
        owner: Some((OwnerField::Uid, &typing)),
        ..resting(&bar, &empty)
    };
    let hit = |controls: PropertiesControls<'_>, at: Point| {
        properties_hit(
            &props,
            perms_view(),
            controls,
            window,
            Scale::ONE,
            &theme,
            at,
        )
    };
    let uid = properties_owner_editor_rect(
        &props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Uid,
    )
    .expect("the row fits");
    let gid = properties_owner_editor_rect(
        &props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Gid,
    )
    .expect("the row fits");
    let into = |rect: Rect| Point::new(rect.left() + 2, rect.top() + 2);
    assert_eq!(hit(editing, into(uid)), None, "the field keeps its typing");
    assert_eq!(
        hit(editing, into(gid)),
        Some(PropertiesTarget::Owner(OwnerField::Gid)),
        "the other id still opens"
    );
    assert_eq!(
        hit(resting(&bar, &empty), into(uid)),
        Some(PropertiesTarget::Owner(OwnerField::Uid))
    );
}

/// The first point on the permissions toggles, found down the column every
/// control in the section begins at — the ownership cells share it — rather
/// than by scanning the whole client.
fn scan_first_permission(props: &Properties, view: PropertiesView, window: Rect) -> Point {
    let theme = Theme::dark();
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let controls = resting(&bar, &empty);
    let column = properties_owner_editor_rect(
        props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Uid,
    )
    .expect("the section is seated at the window's own open size")
    .left();
    (0..i32::try_from(window.height).unwrap())
        .map(|y| Point::new(column, y))
        .find(|at| {
            matches!(
                properties_hit(props, view, controls, window, Scale::ONE, &theme, *at),
                Some(PropertiesTarget::Permission(_))
            )
        })
        .expect("the toggles must be reachable at the window's own open size")
}

/// The sections walk without wrapping, and a strip index round-trips to the
/// section it names.
#[test]
fn the_section_strip_walks_without_wrapping_past_either_end() {
    assert_eq!(PropertiesTab::default(), PropertiesTab::General);
    for (index, tab) in PropertiesTab::ALL.into_iter().enumerate() {
        assert_eq!(PropertiesTab::at(index), Some(tab));
        assert_eq!(tab.index(), index);
        assert!(!tab.label().is_empty());
    }
    assert_eq!(PropertiesTab::at(PropertiesTab::ALL.len()), None);
    assert_eq!(
        PropertiesTab::General.stepped(-1),
        PropertiesTab::General,
        "a walk off the leading end stays put"
    );
    assert_eq!(
        PropertiesTab::General.stepped(1),
        PropertiesTab::Permissions
    );
    assert_eq!(
        PropertiesTab::Attributes.stepped(1),
        PropertiesTab::Attributes,
        "a walk off the trailing end stays put"
    );
    assert_eq!(
        PropertiesTab::General.stepped(i32::MAX),
        PropertiesTab::Attributes
    );
    assert_eq!(
        PropertiesTab::Attributes.stepped(i32::MIN),
        PropertiesTab::General
    );
}

/// The attributes the list shows any part of at `scroll`, top to bottom,
/// found by pressing down the rows' own column rather than by re-deriving
/// where the band sits.
fn attributes_seen(props: &Properties, window: Rect, scroll: u64) -> Vec<usize> {
    let theme = Theme::dark();
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let view = PropertiesView {
        tab: PropertiesTab::Attributes,
        scroll,
        ..PropertiesView::default()
    };
    let mut found = Vec::new();
    for y in 0..i32::try_from(window.height).unwrap() {
        if let Some(PropertiesTarget::Attribute(index)) = properties_hit(
            props,
            view,
            resting(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            Point::new(12, y),
        ) {
            if found.last() != Some(&index) {
                found.push(index);
            }
        }
    }
    found
}

/// Scrolled on, a row names a later attribute — the scroll is what maps a
/// pressed row to the attribute under it — a row the list's edge cuts is found
/// where it shows, and the end of the list names nothing past its last row.
#[test]
fn a_scrolled_attribute_list_names_the_row_under_the_press() {
    let props = props_with(some_attributes(20));
    let window = props_window();
    let row = crate::render::row_height(Scale::ONE, &Theme::dark());
    let top = attributes_seen(&props, window, 0);
    assert_eq!(top.first(), Some(&0));
    assert!(
        top.len() < 20,
        "twenty attributes outgrow the band: {top:?}"
    );
    let down = attributes_seen(&props, window, u64::from(row));
    assert_eq!(
        down.first(),
        Some(&1),
        "a row down, the first row is the second"
    );
    let part = attributes_seen(&props, window, u64::from(row / 2));
    assert_eq!(&part[..2], &[0, 1], "the row the top edge cuts is found");
    assert!(
        part.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "and every row below it in turn: {part:?}"
    );
    let end = attributes_seen(&props, window, u64::MAX);
    assert_eq!(end.last(), Some(&19), "the end rests on the last row");

    // A list that fits its band has nowhere to scroll to.
    let short = props_with(some_attributes(3));
    assert_eq!(
        attributes_seen(&short, window, u64::from(row)),
        vec![0, 1, 2]
    );
}

#[test]
fn permission_bits_are_the_nine_settable_rwx_bits_row_major() {
    // Owner, group, other triads, each read/write/execute — the familiar
    // `rwx` set, and their union is exactly the low nine bits (0o777).
    assert_eq!(
        PERMISSION_BITS,
        [0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001]
    );
    let union = PERMISSION_BITS.iter().fold(0u32, |acc, &b| acc | b);
    assert_eq!(union, 0o777);
    // All nine bits are distinct.
    let set: BTreeSet<u32> = PERMISSION_BITS.iter().copied().collect();
    assert_eq!(set.len(), PERMISSION_BITS.len());
}

#[test]
fn permission_cells_report_exactly_the_set_rwx_bits() {
    // A clear mode shows no cell; a full 0o777 shows all nine.
    assert_eq!(permission_cells(0o000), [false; 9]);
    assert_eq!(permission_cells(0o777), [true; 9]);
    // 0o644 = owner rw-, group r--, other r--.
    assert_eq!(
        permission_cells(0o644),
        [true, true, false, true, false, false, true, false, false]
    );
    // 0o755 = owner rwx, group r-x, other r-x.
    assert_eq!(
        permission_cells(0o755),
        [true, true, true, true, false, true, true, false, true]
    );
    // The setuid/setgid/sticky and file-type bits are not part of the control:
    // 0o4755 reads the same nine cells as 0o755.
    assert_eq!(permission_cells(0o4755), permission_cells(0o755));
    assert_eq!(permission_cells(0o170_755), permission_cells(0o755));
}

#[test]
fn a_window_too_small_for_a_band_resolves_and_draws_nothing_there() {
    let props = props_with(some_attributes(2));
    let theme = Theme::dark();
    // A window dragged smaller than any of its bands places no control off
    // its own surface and offers no editor to type into.
    let tiny = Rect::new(0, 0, 20, 16);
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    for tab in PropertiesTab::ALL {
        let view = PropertiesView {
            tab,
            ..PropertiesView::default()
        };
        for y in 0..i32::try_from(tiny.height).unwrap() {
            for x in 0..i32::try_from(tiny.width).unwrap() {
                assert_eq!(
                    properties_hit(
                        &props,
                        view,
                        resting(&bar, &empty),
                        tiny,
                        Scale::ONE,
                        &theme,
                        Point::new(x, y)
                    ),
                    None
                );
            }
        }
    }
    for tab in PropertiesTab::ALL {
        let view = PropertiesView {
            tab,
            ..PropertiesView::default()
        };
        let mut scroll = ScrollColumn::new();
        assert!(!properties_scroll_wheel(
            &mut scroll,
            &props,
            view,
            true,
            (tiny, Scale::ONE, &theme),
            (0, DETENT),
            &mut tairix_controls::damage::sink()
        ));
        assert_eq!(scroll.offset(), 0, "{tab:?} has nothing to scroll");
    }
    assert_eq!(properties_attr_editor_rect(tiny, Scale::ONE, &theme), None);
    assert_eq!(
        properties_owner_editor_rect(
            &props,
            PropertiesView::default(),
            tiny,
            Scale::ONE,
            &theme,
            OwnerField::Uid
        ),
        None
    );
}

#[test]
fn each_owning_id_carries_the_editor_it_opens_inside_the_client() {
    let window = props_window();
    let theme = Theme::dark();
    let props = props_with(Attributes::Unsupported);
    for field in [OwnerField::Uid, OwnerField::Gid] {
        let rect = properties_owner_editor_rect(
            &props,
            PropertiesView::default(),
            window,
            Scale::ONE,
            &theme,
            field,
        )
        .expect("the row fits the window it opens at");
        assert!(rect.width > 0 && rect.height > 0);
        assert!(rect.left() >= window.left());
        assert!(
            rect.left() + i32::try_from(rect.width).unwrap()
                <= window.left() + i32::try_from(window.width).unwrap(),
            "the editor never runs past the client it is drawn in"
        );
        assert_eq!(
            rect.height,
            Scale::ONE.scale_length(theme.metrics().control_height),
            "an ownership cell is a control plate, not a text row"
        );
    }
    // Each owning id gets its own labelled row, one below the other.
    let uid = properties_owner_editor_rect(
        &props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Uid,
    )
    .expect("uid");
    let gid = properties_owner_editor_rect(
        &props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Gid,
    )
    .expect("gid");
    assert_eq!(uid.left(), gid.left());
    assert!(gid.top() > uid.top());
}

/// The window's frame is resolved from the client alone, so which fields a
/// node happens to show never moves a control under the pointer — the defect
/// the old single-column layout had, where an alias row pushed every band
/// below it down a line.
#[test]
fn the_frame_does_not_move_with_the_node_it_describes() {
    use crate::entry::LinkTarget;

    let theme = Theme::dark();
    let window = props_window();
    let link = Properties::from_stat(
        "Documents",
        crate::entry::EntryKind::Link(LinkTarget::Directory),
        &props_stat(FileKind::Symlink, 0o777),
    )
    .with_target("/Storage/docs")
    .with_attributes(Attributes::Unsupported);
    let plain = props_with(Attributes::Unsupported);
    assert!(
        link.target().is_some() && plain.target().is_none(),
        "the two nodes must differ in their field set for this to mean anything"
    );

    let perms_view = PropertiesView {
        tab: PropertiesTab::Permissions,
        ..PropertiesView::default()
    };
    assert_eq!(
        scan_first_permission(&link, perms_view, window),
        scan_first_permission(&plain, perms_view, window),
        "the grid sits in the same place whatever the node shows above it"
    );
    assert_eq!(
        properties_attr_editor_rect(window, Scale::ONE, &theme),
        properties_attr_editor_rect(window, Scale::ONE, &theme)
    );
}

#[test]
fn a_window_paints_each_state_it_can_be_in_without_panicking() {
    use tairix_icon::NoArtwork;
    use tairix_raster::Surface;

    let theme = Theme::dark();
    let window = props_window();
    let bar = ScrollColumn::new();
    let empty = TextField::new().with_placeholder("namespace.name = value");
    let props = props_with(some_attributes(3));

    let paint =
        |frame: PropertiesFrame<'_>, view: PropertiesView, controls: PropertiesControls<'_>| {
            let mut surface = Surface::new(window.width, window.height).expect("surface");
            draw_properties_window(
                &mut surface,
                frame,
                view,
                controls,
                Scale::ONE,
                &theme,
                window,
                &mut NoArtwork,
            );
            surface.pixels().to_vec()
        };
    let general = PropertiesView::default();
    let perms = PropertiesView {
        tab: PropertiesTab::Permissions,
        ..PropertiesView::default()
    };
    let attrs = PropertiesView {
        tab: PropertiesTab::Attributes,
        ..PropertiesView::default()
    };

    let reading = paint(PropertiesFrame::Reading, general, resting(&bar, &empty));
    let refused = paint(
        PropertiesFrame::Refused("no such file"),
        general,
        resting(&bar, &empty),
    );
    let ready = paint(
        PropertiesFrame::Ready(&props),
        general,
        resting(&bar, &empty),
    );
    assert_ne!(reading, refused, "a refusal states its own reason");
    assert_ne!(ready, reading, "the fields replace the reading notice");

    // Each section draws its own content, so switching tab changes the body.
    let on_perms = paint(PropertiesFrame::Ready(&props), perms, resting(&bar, &empty));
    let on_attrs = paint(PropertiesFrame::Ready(&props), attrs, resting(&bar, &empty));
    assert_ne!(ready, on_perms);
    assert_ne!(ready, on_attrs);
    assert_ne!(on_perms, on_attrs);

    // A volume with no attribute storage says so, which must read differently
    // from a node that simply carries none.
    let unsupported = props_with(Attributes::Unsupported);
    let none = props_with(Attributes::Visible(Vec::new()));
    assert_ne!(
        paint(
            PropertiesFrame::Ready(&unsupported),
            attrs,
            resting(&bar, &empty)
        ),
        paint(PropertiesFrame::Ready(&none), attrs, resting(&bar, &empty))
    );

    // A degenerate client draws nothing and does not panic.
    let mut tiny = Surface::new(2, 2).expect("tiny surface");
    draw_properties_window(
        &mut tiny,
        PropertiesFrame::Ready(&props),
        general,
        resting(&bar, &empty),
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 2, 2),
        &mut NoArtwork,
    );
}

/// The live editors draw in the cells they belong to, an ownership cell the
/// user may not change is drawn refused, and the identity band is drawn from
/// what the caller named.
#[test]
fn a_window_draws_its_live_controls_and_its_named_subject() {
    let theme = Theme::dark();
    let window = props_window();
    let bar = ScrollColumn::new();
    let empty = TextField::new().with_placeholder("namespace.name = value");
    let props = props_with(some_attributes(3));
    let perms = PropertiesView {
        tab: PropertiesTab::Permissions,
        ..PropertiesView::default()
    };
    let attrs = PropertiesView {
        tab: PropertiesTab::Attributes,
        ..PropertiesView::default()
    };
    let paint = |view: PropertiesView, controls: PropertiesControls<'_>| {
        let mut surface = Surface::new(window.width, window.height).expect("surface");
        draw_properties_window(
            &mut surface,
            PropertiesFrame::Ready(&props),
            view,
            controls,
            Scale::ONE,
            &theme,
            window,
            &mut NoArtwork,
        );
        surface.pixels().to_vec()
    };
    let on_perms = paint(perms, resting(&bar, &empty));
    let on_attrs = paint(attrs, resting(&bar, &empty));

    // Regression: an *idle* text field drew identically to the live one over
    // it, so a reader could not tell whether their keys were landing.
    let editor = TextField::new().with_text("1000");
    let editing = paint(
        perms,
        PropertiesControls {
            owner: Some((OwnerField::Uid, &editor)),
            ..resting(&bar, &empty)
        },
    );
    assert_ne!(
        editing, on_perms,
        "a cell being typed into must not draw like the plate that opens it"
    );
    // And it is drawn exactly where the rectangle the host feeds it keys
    // against says, so a key's repaint covers the editor it changed.
    let cell = properties_owner_editor_rect(
        &props,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Uid,
    )
    .expect("the row fits the window it opens at");
    let width = usize::try_from(window.width).unwrap();
    for (index, (now, before)) in editing.iter().zip(&on_perms).enumerate() {
        if now != before {
            let at = Point::new(
                i32::try_from(index % width).unwrap(),
                i32::try_from(index / width).unwrap(),
            );
            assert!(
                cell.contains(at),
                "the editor drew at {at:?}, outside {cell:?}"
            );
        }
    }

    let typed = TextField::new().with_text("user.note = hi");
    assert_ne!(
        paint(
            attrs,
            PropertiesControls {
                attribute: &typed,
                ..resting(&bar, &empty)
            },
        ),
        on_attrs
    );

    // A session without `CAP_FS_CHOWN` is shown the cells refused, wearing the
    // Authority Mark and the sentence saying why.
    assert_ne!(
        paint(
            perms,
            PropertiesControls {
                can_chown: false,
                ..resting(&bar, &empty)
            },
        ),
        on_perms
    );

    // The identity band names its subject, so two nodes differing only in
    // name draw differently even before a field is read.
    let general = PropertiesView::default();
    assert_ne!(
        paint(
            general,
            PropertiesControls {
                identity: Identity {
                    name: "other.txt",
                    detail: "Text document",
                    art: IconRequest::kind(IconKind::Text),
                },
                ..resting(&bar, &empty)
            },
        ),
        paint(general, resting(&bar, &empty)),
        "the identity band is drawn from what the caller named"
    );
}

/// A value from a volume is whatever somebody wrote there: control bytes
/// reaching a surface are an escape sequence, and bytes that are not text at
/// all are nothing a reader can check.
#[test]
fn an_attribute_value_is_shown_escaped_and_offered_back_only_when_it_round_trips() {
    let text = Attribute::new("user.note", b"hello".to_vec());
    assert_eq!(text.text(), Some("hello"));
    assert_eq!(text.display(), "hello");

    let binary = Attribute::new("acorn.filetype", vec![0xff, 0x0a]);
    assert_eq!(binary.text(), None, "never offered back for editing");
    assert_eq!(binary.display(), "\\xff\\x0a");

    // The key grammar bans only `/` and NUL, so a corrupt or hostile volume
    // can store one carrying an escape sequence. It is shown escaped and is
    // never offered back for editing either.
    let hostile = Attribute::new("user.a\u{1b}[31m", b"v".to_vec());
    assert_eq!(hostile.key_text(), None);
    assert_eq!(hostile.key_display(), "user.a\\x1b[31m");
    assert_eq!(
        hostile.key(),
        "user.a\u{1b}[31m",
        "the stored key is unchanged"
    );
    let plain = Attribute::new("user.note", b"v".to_vec());
    assert_eq!(plain.key_text(), Some("user.note"));
    assert_eq!(plain.key_display(), "user.note");
}

/// A node whose attributes were never read must not read as one that has
/// none: the trusted picker never asks, and its panel says nothing about them.
#[test]
fn an_unread_attribute_set_claims_nothing() {
    let props = Properties::from_stat(
        "notes.txt",
        crate::entry::EntryKind::File,
        &props_stat(FileKind::Regular, 0o644),
    );
    assert_eq!(*props.attributes(), Attributes::Unread);
    assert!(props.attributes().visible().is_empty());
    assert_eq!(
        *props_with(Attributes::Unsupported).attributes(),
        Attributes::Unsupported
    );
}

/// The window opens tall enough for its tallest section, and scrolls the
/// attribute list rather than losing a control, whatever it is resized to.
#[test]
fn the_window_opens_sized_to_its_tallest_section_and_scrolls_its_list() {
    let theme = Theme::dark();
    let (w, h) = properties_window_extent(Scale::ONE, &theme);
    assert!(w > 0 && h > 0);
    let window = Rect::new(0, 0, w, h);
    let full = props_with(some_attributes(20));

    // Every section's own controls fit at the open size.
    assert!(properties_owner_editor_rect(
        &full,
        PropertiesView::default(),
        window,
        Scale::ONE,
        &theme,
        OwnerField::Gid
    )
    .is_some());
    let mut scroll = ScrollColumn::new();
    assert!(
        properties_scroll_wheel(
            &mut scroll,
            &full,
            PropertiesView {
                tab: PropertiesTab::Attributes,
                ..PropertiesView::default()
            },
            true,
            (window, Scale::ONE, &theme),
            (0, DETENT),
            &mut tairix_controls::damage::sink()
        ),
        "a long list scrolls"
    );
    // The identity band and the tab strip are both reserved above the body,
    // so no section opens already clipped.
    let head = crate::render::identity_height(Scale::ONE, &theme);
    assert!(
        h > head,
        "the window must leave room below its identity band"
    );
    let perms_view = PropertiesView {
        tab: PropertiesTab::Permissions,
        ..PropertiesView::default()
    };
    assert!(
        scan_first_permission(&full, perms_view, window).y > i32::try_from(head).unwrap(),
        "no control is drawn over the identity band"
    );

    // A hidpi window is proportionally larger, not the same pixel count.
    let hidpi = Scale::from_dpi(192).expect("a valid scale");
    let (hw, hh) = properties_window_extent(hidpi, &theme);
    assert!(hw > w && hh > h, "the window is authored in logical pixels");
}

/// The Permissions section on show, with the keyboard still on the strip.
fn perms_view() -> PropertiesView {
    PropertiesView {
        tab: PropertiesTab::Permissions,
        ..PropertiesView::default()
    }
}

/// Each permission toggle's horizontal extent `(left, right)`, measured
/// through the hit-test along one line through each class row — found down
/// the column the section's controls begin at, so the probe costs a few
/// lines rather than a whole-client scan.
fn toggle_extents(
    props: &Properties,
    window: Rect,
    scale: Scale,
    theme: &Theme,
) -> BTreeMap<u32, (i32, i32)> {
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let controls = resting(&bar, &empty);
    let hit = |at: Point| properties_hit(props, perms_view(), controls, window, scale, theme, at);
    let column = properties_owner_editor_rect(
        props,
        PropertiesView::default(),
        window,
        scale,
        theme,
        OwnerField::Uid,
    )
    .expect("the section is seated")
    .left();
    let mut lines: Vec<i32> = Vec::new();
    let mut last = None;
    for y in 0..i32::try_from(window.height).unwrap() {
        let bit = match hit(Point::new(column + 1, y)) {
            Some(PropertiesTarget::Permission(bit)) => Some(bit),
            _ => None,
        };
        if bit.is_some() && bit != last {
            // A few pixels into the row, clear of its edge.
            lines.push(y + 3);
        }
        last = bit;
    }
    let mut extents = BTreeMap::new();
    for y in lines {
        for x in 0..i32::try_from(window.width).unwrap() {
            if let Some(PropertiesTarget::Permission(bit)) = hit(Point::new(x, y)) {
                let extent = extents.entry(bit).or_insert((x, x));
                extent.0 = extent.0.min(x);
                extent.1 = extent.1.max(x);
            }
        }
    }
    extents
}

/// The flags of one class, as the section builds them, measured.
fn class_flags_width(scale: Scale, theme: &Theme) -> u32 {
    use tairix_controls::{Checkbox, FlagSet, SelectionState};
    FlagSet::new(
        ["Read", "Write", "Execute"]
            .into_iter()
            .map(|label| Checkbox::new(label, SelectionState::Unselected))
            .collect(),
    )
    .measured_width(scale, theme)
}

/// At the size the window opens at, every class's three flags are seated
/// whole — no label the reader needs is cut — under the shipped themes, at
/// double density, and under a wider type ladder than either carries.
///
/// Regression: the section's column is capped at half a row, and the window's
/// hand-picked width left too little of a row for three labelled flags.
#[test]
fn every_flag_is_seated_whole_at_the_size_the_window_opens_at() {
    use tairix_controls::testkit::text_ladder;

    let props = props_with(Attributes::Unsupported);
    let cases = [
        (Theme::dark(), Scale::ONE),
        (Theme::light(), Scale::ONE),
        (Theme::dark(), Scale::from_percent(200).expect("scale")),
        (text_ladder(22), Scale::ONE),
    ];
    for (theme, scale) in cases {
        let (w, h) = properties_window_extent(scale, &theme);
        let window = Rect::new(0, 0, w, h);
        let extents = toggle_extents(&props, window, scale, &theme);
        assert_eq!(extents.len(), PERMISSION_BITS.len(), "{}", theme.name());
        for class in PERMISSION_BITS.chunks(3) {
            let left = extents[&class[0]].0;
            let right = extents[&class[2]].1;
            assert_eq!(
                u32::try_from(right - left + 1).unwrap(),
                class_flags_width(scale, &theme),
                "a class's flags were narrowed under {} at {}%",
                theme.name(),
                scale.percent()
            );
        }
    }
}

/// The smallest window the manager declares still seats every toggle: the
/// labels give way, and never a box.
#[test]
fn the_narrowest_window_still_seats_every_toggle_apart() {
    let theme = Theme::dark();
    let props = props_with(Attributes::Unsupported);
    let crate::WindowSizing::Resizable { min_width_px, .. } =
        crate::properties_sizing(Scale::ONE, &theme)
    else {
        panic!("the manager's windows are resizable");
    };
    let (_, h) = properties_window_extent(Scale::ONE, &theme);
    let window = Rect::new(0, 0, min_width_px, h);
    let extents = toggle_extents(&props, window, Scale::ONE, &theme);
    assert_eq!(extents.len(), PERMISSION_BITS.len());
    let side = Scale::ONE.scale_length(theme.metrics().selector_extent);
    let mut spans: Vec<(i32, i32)> = extents.values().copied().collect();
    spans.sort_unstable();
    for (left, right) in &spans {
        assert!(
            u32::try_from(right - left + 1).unwrap() >= side,
            "a box was cut"
        );
    }
    // Three to a class row, and the three apart.
    for class in PERMISSION_BITS.chunks(3) {
        for pair in class.windows(2) {
            assert!(extents[&pair[0]].1 < extents[&pair[1]].0, "toggles overlap");
        }
    }
}

/// Feed `key` to the Permissions section, keeping the cursor it answers.
fn perms_press(
    props: &Properties,
    view: &mut PropertiesView,
    controls: PropertiesControls<'_>,
    key: Key,
) -> PermsKeyed {
    let keyed = properties_permissions_key(
        props,
        *view,
        controls,
        props_window(),
        Scale::ONE,
        &Theme::dark(),
        tairix_controls::testkit::keystroke(key),
        &mut tairix_controls::damage::sink(),
    );
    view.perms = keyed.cursor;
    keyed
}

/// Everything a press can reach on the section — every toggle and both owning
/// ids, as the whole-client scan above establishes — the keyboard reaches
/// too, and names it the same way, so a toggle flipped from the keyboard is
/// the one the pointer would have flipped.
#[test]
fn the_keyboard_reaches_every_control_the_pointer_does() {
    let props = props_with(Attributes::Unsupported);
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let controls = resting(&bar, &empty);
    let mut view = perms_view();
    let named = Key::Named;

    assert!(!view.perms.holds());
    perms_press(&props, &mut view, controls, named(NamedKey::Down));
    assert_eq!(view.perms.row, Some((0, 0)), "Down takes the keyboard in");

    let mut reached = Vec::new();
    for _ in 0..6 {
        for _ in 0..3 {
            perms_press(&props, &mut view, controls, named(NamedKey::Left));
        }
        for _ in 0..3 {
            if let Some(target) = perms_press(&props, &mut view, controls, Key::Char(' ')).target {
                if !reached.contains(&target) {
                    reached.push(target);
                }
            }
            perms_press(&props, &mut view, controls, named(NamedKey::Right));
        }
        perms_press(&props, &mut view, controls, named(NamedKey::Down));
    }
    let pressable = PERMISSION_BITS
        .iter()
        .copied()
        .map(PropertiesTarget::Permission)
        .chain([OwnerField::Uid, OwnerField::Gid].map(PropertiesTarget::Owner));
    for target in pressable {
        assert!(reached.contains(&target), "{target:?} is pointer-only");
    }
    assert_eq!(reached.len(), PERMISSION_BITS.len() + 2, "{reached:?}");
}

/// The cursor walks a column of flags, carries between the two groups, and
/// hands the keyboard back to the strip; a refused ownership cell is reached
/// and read but opens nothing.
#[test]
fn the_permissions_cursor_walks_carries_and_steps_back_out() {
    let props = props_with(Attributes::Unsupported);
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let controls = resting(&bar, &empty);
    let mut view = perms_view();
    let press = |view: &mut PropertiesView, key| perms_press(&props, view, controls, key);
    let named = Key::Named;

    // Nothing but Down or Tab takes the keyboard in from the strip.
    assert_eq!(
        press(&mut view, Key::Char(' ')).cursor,
        PermsCursor::default()
    );
    press(&mut view, named(NamedKey::Tab));
    assert_eq!(view.perms.row, Some((0, 0)));
    assert_eq!(
        press(&mut view, named(NamedKey::Up)).cursor.row,
        Some((0, 0)),
        "the top of the section clamps"
    );

    // Down into the owner class, across to Write, and down the Write column.
    press(&mut view, named(NamedKey::Down));
    press(&mut view, named(NamedKey::Right));
    assert_eq!(
        press(&mut view, Key::Char(' ')).target,
        Some(PropertiesTarget::Permission(0o200)),
        "the owner's write bit"
    );
    press(&mut view, named(NamedKey::Down));
    assert_eq!(view.perms.flag, 1, "walking down a column stays in it");
    assert_eq!(
        press(&mut view, named(NamedKey::Enter)).target,
        Some(PropertiesTarget::Permission(0o020)),
        "the group's write bit"
    );

    // Off the last class and into the ownership group, then back.
    press(&mut view, named(NamedKey::Down));
    press(&mut view, named(NamedKey::Down));
    assert_eq!(
        view.perms.row,
        Some((1, 0)),
        "Down carries into the next group"
    );
    assert_eq!(
        press(&mut view, Key::Char(' ')).target,
        Some(PropertiesTarget::Owner(OwnerField::Uid))
    );
    press(&mut view, named(NamedKey::Down));
    assert_eq!(
        press(&mut view, named(NamedKey::Down)).cursor.row,
        Some((1, 1)),
        "the bottom of the section clamps"
    );
    press(&mut view, named(NamedKey::Up));
    press(&mut view, named(NamedKey::Up));
    assert_eq!(
        view.perms.row,
        Some((0, 3)),
        "Up carries back to the last class"
    );

    // Escape and Tab both hand the keyboard back.
    press(&mut view, named(NamedKey::Escape));
    assert!(!view.perms.holds());
    press(&mut view, named(NamedKey::Down));
    press(&mut view, named(NamedKey::Tab));
    assert!(!view.perms.holds());

    // A refused session reaches its ownership rows, and opens nothing there.
    let locked = refused(&bar, &empty);
    let mut view = PropertiesView {
        perms: PermsCursor {
            row: Some((1, 0)),
            flag: 0,
        },
        ..perms_view()
    };
    assert_eq!(
        perms_press(&props, &mut view, locked, Key::Char(' ')).target,
        None
    );

    // While an id editor is open the keyboard is the editor's: the cursor
    // neither moves nor acts.
    let typing = TextField::new().with_text("10");
    let editing = PropertiesControls {
        owner: Some((OwnerField::Uid, &typing)),
        ..resting(&bar, &empty)
    };
    for key in [
        named(NamedKey::Down),
        named(NamedKey::Escape),
        Key::Char(' '),
    ] {
        let keyed = perms_press(&props, &mut view, editing, key);
        assert_eq!(keyed.cursor.row, Some((1, 0)), "{key:?} moved the cursor");
        assert_eq!(keyed.target, None);
    }
}

/// Taking the keyboard in and moving it reports the rows it changed, so the
/// host repaints them rather than the window.
#[test]
fn the_permissions_cursor_reports_what_it_repaints() {
    let props = props_with(Attributes::Unsupported);
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let controls = resting(&bar, &empty);
    let theme = Theme::dark();
    let window = props_window();
    let key = |view: PropertiesView, key: Key| {
        let mut damage = tairix_controls::damage::sink();
        let keyed = properties_permissions_key(
            &props,
            view,
            controls,
            window,
            Scale::ONE,
            &theme,
            tairix_controls::testkit::keystroke(key),
            &mut damage,
        );
        (keyed, damage)
    };
    let (entered, damage) = key(perms_view(), Key::Named(NamedKey::Down));
    assert!(!damage.is_empty(), "the first row gains its ring");
    let area = |region: &tairix_geometry::Region| {
        region
            .rects()
            .iter()
            .map(|rect| u64::from(rect.width) * u64::from(rect.height))
            .sum::<u64>()
    };
    assert!(
        area(&damage) < u64::from(window.width) * u64::from(window.height) / 4,
        "a ring move is not a whole-window repaint"
    );
    let (_, idle) = key(
        PropertiesView {
            perms: entered.cursor,
            ..perms_view()
        },
        Key::Char('x'),
    );
    assert!(
        idle.is_empty(),
        "a key that changes nothing repaints nothing"
    );
}

/// The gutter bar moves the same list the wheel and the rows do, and a move
/// repaints the rows it slid as well as the bar.
///
/// Regression: the bar reported only its own rectangle, so a press on its
/// track moved the thumb and left the rows it scrolled standing where they had
/// been drawn.
#[test]
fn a_press_on_the_gutter_scrolls_the_attribute_list_and_repaints_its_rows() {
    use tairix_input::{InputEvent, PointerButton};

    let theme = Theme::dark();
    let window = props_window();
    let props = props_with(some_attributes(40));
    let mut scroll = ScrollColumn::new();
    let press = InputEvent::PointerPressed {
        button: PointerButton::Primary,
    };
    // A press somewhere down the gutter pages toward the end. Where the band
    // insets its gutter is the layout's business, so the probe sweeps the
    // trailing edge rather than assuming an offset into it.
    let right = i32::try_from(window.width).unwrap();
    let mut moved = None;
    'probe: for x in (right - 40..right).rev() {
        for y in 0..i32::try_from(window.height).unwrap() {
            let view = PropertiesView {
                tab: PropertiesTab::Attributes,
                scroll: scroll.offset(),
                ..PropertiesView::default()
            };
            let mut damage = tairix_controls::damage::sink();
            let taken = properties_scroll_pointer(
                &mut scroll,
                &props,
                view,
                true,
                (window, Scale::ONE, &theme),
                (Point::new(x, y), &press),
                &mut damage,
            );
            if taken == Some(true) && scroll.offset() > 0 {
                moved = Some(damage);
                break 'probe;
            }
        }
    }
    let damage = moved.expect("the drawn bar must move the list it depicts");
    let row = crate::render::row_height(Scale::ONE, &theme);
    assert!(
        scroll.offset() <= u64::from(row * 40),
        "and never past what the list holds"
    );
    // Every row the list now shows lies inside what the press repainted.
    let shown = attributes_seen(&props, window, scroll.offset());
    assert!(!shown.is_empty());
    for y in 0..i32::try_from(window.height).unwrap() {
        let at = Point::new(12, y);
        let bar = ScrollColumn::new();
        let empty = TextField::new();
        let view = PropertiesView {
            tab: PropertiesTab::Attributes,
            scroll: scroll.offset(),
            ..PropertiesView::default()
        };
        if let Some(PropertiesTarget::Attribute(_)) = properties_hit(
            &props,
            view,
            resting(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            at,
        ) {
            assert!(damage.contains(at), "the row at {at:?} was not repainted");
        }
    }
}

/// The shortest Properties window the manager declares, at the width it opens
/// at.
fn shortest_props_window(theme: &Theme) -> Rect {
    let crate::WindowSizing::Resizable { min_height_px, .. } =
        crate::properties_sizing(Scale::ONE, theme)
    else {
        panic!("the manager's windows are resizable");
    };
    let (w, _) = properties_window_extent(Scale::ONE, theme);
    Rect::new(0, 0, w, min_height_px)
}

/// Paint `props` in `window` at `view`, returning the frame.
fn props_frame(props: &Properties, view: PropertiesView, window: Rect) -> Surface {
    let theme = Theme::dark();
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let mut surface = Surface::new(window.width, window.height).expect("surface");
    draw_properties_window(
        &mut surface,
        PropertiesFrame::Ready(props),
        view,
        resting(&bar, &empty),
        Scale::ONE,
        &theme,
        window,
        &mut NoArtwork,
    );
    surface
}

/// In the shortest window the manager declares, the General section's facts
/// outgrow the body: they are laid out at their natural height and scrolled
/// through it — a detent moves them, and what shows is the unscrolled column
/// shifted up, every fact the edges cross drawn whole and cut there.
#[test]
fn the_general_section_scrolls_its_facts_in_a_short_window() {
    let theme = Theme::dark();
    let window = shortest_props_window(&theme);
    let props = props_with(some_attributes(0));
    let view = PropertiesView::default();
    let mut scroll = ScrollColumn::new();
    let mut damage = tairix_controls::damage::sink();
    assert!(properties_scroll_wheel(
        &mut scroll,
        &props,
        view,
        true,
        (window, Scale::ONE, &theme),
        (0, DETENT),
        &mut damage
    ));
    let offset = scroll.offset();
    assert!(offset > 0);
    let rest = props_frame(&props, view, window);
    let moved = props_frame(
        &props,
        PropertiesView {
            scroll: offset,
            ..view
        },
        window,
    );

    // The body starts beneath the identity band and the strip, which neither
    // scroll nor redraw.
    let head = crate::render::identity_height(Scale::ONE, &theme);
    let body_top = (head..window.height)
        .find(|&y| {
            item_rows(&rest, window.width, y..y + 1) != item_rows(&moved, window.width, y..y + 1)
        })
        .expect("the body moved");
    assert!(body_top > head);
    assert_eq!(
        item_rows(&moved, window.width, 0..body_top),
        item_rows(&rest, window.width, 0..body_top),
        "nothing above the body moved"
    );
    let shift = u32::try_from(offset).unwrap();
    let columns = window.width / 2;
    assert_eq!(
        item_rows(&moved, columns, body_top..window.height - shift),
        item_rows(&rest, columns, body_top + shift..window.height),
        "the facts moved up by the scroll, drawn whole"
    );
    assert!(!damage.is_empty(), "and the move was reported");
}

/// In a short window the keyboard walks the Permissions section onto rows
/// the body cannot show at once, and each row it lands on is revealed: the
/// row is then whole on screen, and a press there finds the control on it.
#[test]
fn the_permissions_cursor_reveals_the_rows_it_walks_onto() {
    let theme = Theme::dark();
    let window = shortest_props_window(&theme);
    let props = props_with(Attributes::Unsupported);
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let controls = resting(&bar, &empty);
    let mut scroll = ScrollColumn::new();
    let mut view = perms_view();
    let key = |view: &mut PropertiesView, scroll: &mut ScrollColumn, key: Key| {
        let keyed = properties_permissions_key(
            &props,
            *view,
            controls,
            window,
            Scale::ONE,
            &theme,
            tairix_controls::testkit::keystroke(key),
            &mut tairix_controls::damage::sink(),
        );
        view.perms = keyed.cursor;
        properties_reveal(
            scroll,
            &props,
            *view,
            true,
            (window, Scale::ONE, &theme),
            &mut tairix_controls::damage::sink(),
        );
        view.scroll = scroll.offset();
    };
    key(&mut view, &mut scroll, Key::Named(NamedKey::Down));
    assert_eq!(view.perms.row, Some((0, 0)));
    for _ in 0..5 {
        key(&mut view, &mut scroll, Key::Named(NamedKey::Down));
    }
    assert_eq!(
        view.perms.row,
        Some((1, 1)),
        "the group id, the section's last row"
    );
    assert!(view.scroll > 0, "the body scrolled to follow the cursor");
    let cell =
        properties_owner_editor_rect(&props, view, window, Scale::ONE, &theme, OwnerField::Gid)
            .expect("the row the cursor reached shows");
    let control = Scale::ONE.scale_length(theme.metrics().control_height);
    assert_eq!(cell.height, control, "whole, not cut");
    assert_eq!(
        properties_hit(
            &props,
            view,
            controls,
            window,
            Scale::ONE,
            &theme,
            Point::new(cell.left() + 2, cell.top() + 2)
        ),
        Some(PropertiesTarget::Owner(OwnerField::Gid)),
        "a press lands on the control the scroll shows there"
    );
    // Walking back up to the first row brings the top of the section back.
    for _ in 0..5 {
        key(&mut view, &mut scroll, Key::Named(NamedKey::Up));
    }
    assert_eq!(view.perms.row, Some((0, 0)));
    assert!(
        view.scroll < scroll_bottom(&props, window),
        "{}",
        view.scroll
    );
}

/// How far the Permissions section can scroll in `window`: the offset a
/// wheel turn far past the end settles on.
fn scroll_bottom(props: &Properties, window: Rect) -> u64 {
    let mut scroll = ScrollColumn::new();
    properties_scroll_wheel(
        &mut scroll,
        props,
        perms_view(),
        true,
        (window, Scale::ONE, &Theme::dark()),
        (0, DETENT * 1000),
        &mut tairix_controls::damage::sink(),
    );
    scroll.offset()
}

/// The attribute list reveals the row the keyboard lands on in a window too
/// short to show more than a few, and the scroll gutter beside the rows is no
/// row at all.
#[test]
fn the_attribute_cursor_is_revealed_and_the_gutter_is_not_a_row() {
    let theme = Theme::dark();
    let window = shortest_props_window(&theme);
    let props = props_with(some_attributes(20));
    let mut scroll = ScrollColumn::new();
    let view = PropertiesView {
        tab: PropertiesTab::Attributes,
        cursor: 12,
        ..PropertiesView::default()
    };
    assert!(properties_reveal(
        &mut scroll,
        &props,
        view,
        true,
        (window, Scale::ONE, &theme),
        &mut tairix_controls::damage::sink()
    ));
    let seen = attributes_seen(&props, window, scroll.offset());
    assert_eq!(
        seen.last(),
        Some(&12),
        "the cursor row rests at the list's foot"
    );
    let revealed = PropertiesView {
        scroll: scroll.offset(),
        ..view
    };
    assert!(
        !properties_reveal(
            &mut scroll,
            &props,
            revealed,
            true,
            (window, Scale::ONE, &theme),
            &mut tairix_controls::damage::sink()
        ),
        "a cursor already whole moves nothing"
    );
    // The gutter sits along the band's trailing edge; nothing there is a row.
    let bar = ScrollColumn::new();
    let empty = TextField::new();
    let row_y = (0..i32::try_from(window.height).unwrap())
        .find(|&y| {
            matches!(
                properties_hit(
                    &props,
                    revealed,
                    resting(&bar, &empty),
                    window,
                    Scale::ONE,
                    &theme,
                    Point::new(12, y)
                ),
                Some(PropertiesTarget::Attribute(_))
            )
        })
        .expect("a row shows");
    let pad = Scale::ONE.scale_length(4) * 2;
    let gutter_x = i32::try_from(window.width - pad - 2).unwrap();
    assert_eq!(
        properties_hit(
            &props,
            revealed,
            resting(&bar, &empty),
            window,
            Scale::ONE,
            &theme,
            Point::new(gutter_x, row_y)
        ),
        None
    );
}

// --- FM7b: the delete-confirmation dialog ---------------------------------

use crate::render::{
    build_delete_dialog, delete_dialog_action_at, delete_dialog_rect, draw_delete_dialog,
    DELETE_CANCEL_INDEX, DELETE_CONFIRM_INDEX,
};
use crate::trash::DeleteDisposition;

/// A plan removing a single regular file.
fn one_file_plan() -> DeletePlan {
    DeletePlan::new(vec![(comps(&["System", "Kernel"]), false)]).expect("a plan")
}

/// A plan removing two items, one of them a directory.
fn folder_plan() -> DeletePlan {
    DeletePlan::new(vec![
        (comps(&["System", "Fonts"]), true),
        (comps(&["System", "Kernel"]), false),
    ])
    .expect("a plan")
}

#[test]
fn delete_dialog_titles_a_single_target_by_its_name() {
    let dialog = build_delete_dialog(&one_file_plan(), DeleteDisposition::Permanent);
    // A single target is named, so the user sees exactly what they are about
    // to remove.
    assert!(dialog.title().contains("Kernel"));
    // A files-only removal warns only that it cannot be undone.
    let message = dialog.message().expect("a message");
    assert!(message.contains("cannot be undone"));
    assert!(!message.to_ascii_lowercase().contains("folder"));
}

#[test]
fn delete_dialog_reports_the_honest_count_and_folder_warning() {
    let dialog = build_delete_dialog(&folder_plan(), DeleteDisposition::Permanent);
    // More than one target: the honest count, not a single name.
    assert!(dialog.title().contains('2'));
    // A plan that includes a directory warns that folders (and their contents)
    // are removed, so the confirmation is not misleading.
    let message = dialog.message().expect("a message");
    assert!(message.to_ascii_lowercase().contains("folder"));
}

#[test]
fn delete_dialog_offers_a_destructive_delete_and_a_recommended_cancel() {
    use tairix_controls::state::ControlRole;
    let dialog = build_delete_dialog(&one_file_plan(), DeleteDisposition::Permanent);
    let actions = dialog.actions();
    assert_eq!(actions.len(), 2);
    // The honest warmth is on the safe Cancel, never on the destructive Delete:
    // the delete carries the Destructive role, Cancel the Recommended.
    assert_eq!(
        actions[DELETE_CONFIRM_INDEX].role(),
        ControlRole::Destructive
    );
    assert_eq!(
        actions[DELETE_CANCEL_INDEX].role(),
        ControlRole::Recommended
    );
}

#[test]
fn delete_dialog_rect_is_centered_and_clamped_within_the_viewport() {
    let theme = Theme::dark();

    let vp = Rect::new(0, 0, 480, 320);
    let rect = delete_dialog_rect(vp, Scale::ONE, &theme);
    assert!(rect.width > 0 && rect.height > 0);
    assert!(rect.origin.x >= 0 && rect.origin.y >= 0);
    assert!(rect.origin.x + i32::try_from(rect.width).unwrap() <= i32::try_from(vp.width).unwrap());
    assert!(
        rect.origin.y + i32::try_from(rect.height).unwrap() <= i32::try_from(vp.height).unwrap()
    );
    // Centered on each axis (within one pixel of the integer split).
    let right_margin =
        i32::try_from(vp.width).unwrap() - (rect.origin.x + i32::try_from(rect.width).unwrap());
    assert!((rect.origin.x - right_margin).abs() <= 1);

    // A window smaller than the dialog would like still yields a drawable rect
    // clamped to the window, never a zero or over-size rectangle (no panic).
    let tiny = Rect::new(0, 0, 20, 16);
    let small = delete_dialog_rect(tiny, Scale::ONE, &theme);
    assert!(small.width >= 1 && small.width <= tiny.width);
    assert!(small.height >= 1 && small.height <= tiny.height);
}

#[test]
fn draw_delete_dialog_paints_into_the_surface_without_panicking() {
    use tairix_raster::Surface;

    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 480, 320);
    let dialog = build_delete_dialog(&folder_plan(), DeleteDisposition::Permanent);

    let mut surface = Surface::new(vp.width, vp.height).expect("surface");
    let before = surface.pixels().to_vec();
    draw_delete_dialog(&mut surface, &dialog, Scale::ONE, &theme, vp);
    assert_ne!(surface.pixels().to_vec(), before);

    // A degenerate viewport draws nothing and does not panic.
    let mut tiny = Surface::new(2, 2).expect("tiny surface");
    draw_delete_dialog(
        &mut tiny,
        &dialog,
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 2, 2),
    );
}

#[test]
fn delete_dialog_action_at_mirrors_both_buttons_and_fails_closed_off_grid() {
    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 480, 320);
    let dialog = build_delete_dialog(&folder_plan(), DeleteDisposition::Permanent);

    // Scanning the whole window, every index the hit-test resolves is one of
    // the two action buttons, and both are reachable — so the drawn buttons and
    // the hit-test cover exactly the same two distinct actions.
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let mut y = 0;
    while y < i32::try_from(vp.height).unwrap() {
        let mut x = 0;
        while x < i32::try_from(vp.width).unwrap() {
            if let Some(index) =
                delete_dialog_action_at(&dialog, vp, Scale::ONE, &theme, Point::new(x, y))
            {
                assert!(
                    index == DELETE_CONFIRM_INDEX || index == DELETE_CANCEL_INDEX,
                    "resolved a non-action index"
                );
                seen.insert(index);
            }
            x += 1;
        }
        y += 1;
    }
    assert_eq!(
        seen,
        [DELETE_CONFIRM_INDEX, DELETE_CANCEL_INDEX]
            .into_iter()
            .collect()
    );

    // A click well outside the dialog resolves nothing (fail closed).
    assert_eq!(
        delete_dialog_action_at(&dialog, vp, Scale::ONE, &theme, Point::new(-5, -5)),
        None
    );
    // On a window too small for the dialog buttons, nothing resolves (fail
    // closed) rather than placing a phantom button.
    let tiny = Rect::new(0, 0, 20, 16);
    assert_eq!(
        delete_dialog_action_at(&dialog, tiny, Scale::ONE, &theme, Point::new(5, 5)),
        None
    );
}

// --- FM10: the move-to-Trash confirmation wording -------------------------

#[test]
fn trash_dialog_is_recoverable_and_worded_honestly() {
    use tairix_controls::state::ControlRole;
    let dialog = build_delete_dialog(&one_file_plan(), DeleteDisposition::Trash);
    // The recoverable move names the item and says "Trash", never "delete" or
    // "cannot be undone" (the wording matches what will happen).
    assert!(dialog.title().contains("Kernel"));
    assert!(dialog.title().contains("Trash"));
    let message = dialog.message().expect("a message");
    assert!(message.contains("restore"));
    assert!(!message.to_ascii_lowercase().contains("cannot be undone"));
    // A recoverable move is not destructive: the confirm action is the
    // recommended (safe) primary, and Cancel carries no honest-warmth role.
    let actions = dialog.actions();
    assert_eq!(actions.len(), 2);
    assert_eq!(
        actions[DELETE_CONFIRM_INDEX].role(),
        ControlRole::Recommended
    );
    assert_eq!(actions[DELETE_CANCEL_INDEX].role(), ControlRole::Neutral);
}

#[test]
fn permanent_dialog_names_the_irreversible_delete() {
    // The permanent wording is explicit that the removal is forever, so it can
    // never be mistaken for the recoverable Trash move.
    let dialog = build_delete_dialog(&one_file_plan(), DeleteDisposition::Permanent);
    assert!(dialog.title().to_ascii_lowercase().contains("permanently"));
}

// --- FM10: the shared Trash-directory location ----------------------------

#[test]
fn trash_dir_is_the_library_trash_subtree_of_home() {
    use crate::trash::{trash_dir, TRASH_LEAF_DIR, TRASH_LIBRARY_DIR};
    let home = comps(&["Users", "root"]);
    assert_eq!(
        trash_dir(&home),
        comps(&["Users", "root", TRASH_LIBRARY_DIR, TRASH_LEAF_DIR])
    );
    // It reads only `home` — nothing is fabricated for an empty home (that
    // parses to no components, the root, upstream), and the leaves are the
    // shared constants, so the location cannot drift from the app's.
    assert_eq!(trash_dir(&[]), comps(&["Library", "Trash"]));
}

// --- FM7b: the long-operation progress + cancel surface -------------------

use crate::progress::{ProgressModel, ProgressOp};
use crate::render::{
    build_progress_cancel, draw_progress_dialog, progress_cancel_at, progress_dialog_rect,
};

#[test]
fn progress_model_reports_the_honest_count_and_never_a_percentage() {
    let mut model = ProgressModel::new(ProgressOp::Delete);
    assert_eq!(model.done(), 0);
    // Zero and plural counts read naturally; the verb matches the operation.
    assert_eq!(model.status_line(), "0 items removed");
    model.set_done(1);
    assert_eq!(model.status_line(), "1 item removed");
    model.set_done(42);
    assert_eq!(model.status_line(), "42 items removed");
    // No fabricated percentage anywhere in the caption.
    assert!(!model.status_line().contains('%'));

    // A copy model reads with the copy verb.
    let mut copy = ProgressModel::new(ProgressOp::Copy);
    copy.set_done(3);
    assert_eq!(copy.status_line(), "3 items copied");
    assert!(copy.title().starts_with("Copying"));
    assert!(model.title().starts_with("Deleting"));

    // A move-to-Trash model reads with the Trash verb (`plans/NEW-FILEMANAGER.md`
    // FM10): an honest recoverable-move caption, never "removed".
    let mut trash = ProgressModel::new(ProgressOp::Trash);
    trash.set_done(1);
    assert_eq!(trash.status_line(), "1 item moved to Trash");
    assert!(trash.title().starts_with("Moving to Trash"));
}

#[test]
fn progress_cancel_is_latched_and_shown() {
    let mut model = ProgressModel::new(ProgressOp::Delete);
    assert!(!model.is_cancel_requested());
    model.request_cancel();
    assert!(model.is_cancel_requested());
    // The title reflects the pending cancel while the current step finishes.
    assert!(model.title().starts_with("Cancelling"));
    // The latch cannot be reverted by a second request.
    model.request_cancel();
    assert!(model.is_cancel_requested());
    // The Cancel button reads differently once cancel is latched (disabled), so
    // a second press cannot re-request what is already stopping.
    let running = ProgressModel::new(ProgressOp::Delete);
    assert_ne!(
        build_progress_cancel(&model).state(),
        build_progress_cancel(&running).state()
    );
}

#[test]
fn progress_dialog_rect_is_centered_and_clamped_within_the_viewport() {
    let theme = Theme::dark();

    let vp = Rect::new(0, 0, 480, 320);
    let rect = progress_dialog_rect(vp, Scale::ONE, &theme);
    assert!(rect.width > 0 && rect.height > 0);
    assert!(rect.origin.x >= 0 && rect.origin.y >= 0);
    assert!(rect.origin.x + i32::try_from(rect.width).unwrap() <= i32::try_from(vp.width).unwrap());
    assert!(
        rect.origin.y + i32::try_from(rect.height).unwrap() <= i32::try_from(vp.height).unwrap()
    );
    // A window smaller than the panel still yields a drawable clamped rect (no
    // panic).
    let tiny = Rect::new(0, 0, 20, 16);
    let small = progress_dialog_rect(tiny, Scale::ONE, &theme);
    assert!(small.width >= 1 && small.width <= tiny.width);
    assert!(small.height >= 1 && small.height <= tiny.height);
}

#[test]
fn draw_progress_dialog_paints_into_the_surface_without_panicking() {
    use tairix_raster::Surface;

    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 480, 320);
    let mut model = ProgressModel::new(ProgressOp::Copy);
    model.set_done(7);

    let mut surface = Surface::new(vp.width, vp.height).expect("surface");
    let before = surface.pixels().to_vec();
    draw_progress_dialog(&mut surface, &model, Scale::ONE, &theme, vp);
    assert_ne!(surface.pixels().to_vec(), before);

    // A degenerate viewport draws nothing and does not panic.
    let mut tiny = Surface::new(2, 2).expect("tiny surface");
    draw_progress_dialog(&mut tiny, &model, Scale::ONE, &theme, Rect::new(0, 0, 2, 2));
}

#[test]
fn progress_cancel_at_mirrors_the_cancel_button_and_fails_closed_off_grid() {
    let theme = Theme::dark();
    let vp = Rect::new(0, 0, 480, 320);

    // Scanning the whole window, the Cancel hit-test resolves true for a
    // contiguous, reachable region and false everywhere else — so the drawn
    // button and the hit-test agree on exactly one target.
    let mut hits = 0u32;
    let mut y = 0;
    while y < i32::try_from(vp.height).unwrap() {
        let mut x = 0;
        while x < i32::try_from(vp.width).unwrap() {
            if progress_cancel_at(vp, Scale::ONE, &theme, Point::new(x, y)) {
                hits += 1;
            }
            x += 1;
        }
        y += 1;
    }
    assert!(hits > 0, "the Cancel button is reachable");

    // A click well outside the panel resolves nothing (fail closed).
    assert!(!progress_cancel_at(
        vp,
        Scale::ONE,
        &theme,
        Point::new(-5, -5)
    ));
    // On a window too small to place the button, nothing resolves (fail
    // closed) rather than placing a phantom button.
    let tiny = Rect::new(0, 0, 20, 16);
    assert!(!progress_cancel_at(
        tiny,
        Scale::ONE,
        &theme,
        Point::new(5, 5)
    ));
}

mod trash {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::execute::VolumeId;
    use crate::trash::{
        empty_trash_plan, trash_dest_path, trash_strategy, TrashError, TrashStrategy,
    };

    fn vol(byte: u8) -> VolumeId {
        VolumeId::new([byte; 16])
    }

    fn owned(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| String::from(*p)).collect()
    }

    fn child(name: &str, is_dir: bool) -> (String, bool) {
        (String::from(name), is_dir)
    }

    #[test]
    fn same_volume_moves_and_cross_volume_unlinks() {
        // An item on the same volume as Trash is a cheap recoverable rename;
        // a different volume falls back to the irreversible unlink.
        assert_eq!(trash_strategy(vol(7), vol(7)), TrashStrategy::Move);
        assert_eq!(trash_strategy(vol(1), vol(2)), TrashStrategy::Unlink);
    }

    #[test]
    fn a_free_name_lands_unchanged_under_the_trash_dir() {
        let trash = owned(&["Users", "root", "Library", "Trash"]);
        let dest = trash_dest_path(&trash, "notes.txt", &[]).expect("free name");
        assert_eq!(
            dest,
            owned(&["Users", "root", "Library", "Trash", "notes.txt"])
        );
    }

    #[test]
    fn a_clashing_name_disambiguates_before_the_extension() {
        let trash = owned(&["Trash"]);
        let taken = owned(&["notes.txt"]);
        let dest = trash_dest_path(&trash, "notes.txt", &taken).expect("disambiguated");
        assert_eq!(dest.last().map(String::as_str), Some("notes (2).txt"));
    }

    #[test]
    fn a_clashing_typed_name_disambiguates_before_its_file_type() {
        let trash = owned(&["Trash"]);
        for (leaf, expected) in [
            ("Logo,b60", "Logo (2),b60"),
            ("notes.txt,fff", "notes (2).txt,fff"),
        ] {
            let taken = owned(&[leaf]);
            let dest = trash_dest_path(&trash, leaf, &taken).expect("disambiguated");
            assert_eq!(dest.last().map(String::as_str), Some(expected), "{leaf}");
        }
    }

    #[test]
    fn disambiguation_skips_every_taken_suffix_in_order() {
        let trash = owned(&["Trash"]);
        let taken = owned(&["notes.txt", "notes (2).txt", "notes (3).txt"]);
        let dest = trash_dest_path(&trash, "notes.txt", &taken).expect("disambiguated");
        assert_eq!(dest.last().map(String::as_str), Some("notes (4).txt"));
    }

    #[test]
    fn a_name_with_no_extension_disambiguates_as_a_whole() {
        let trash = owned(&["Trash"]);
        let taken = owned(&["report"]);
        let dest = trash_dest_path(&trash, "report", &taken).expect("disambiguated");
        assert_eq!(dest.last().map(String::as_str), Some("report (2)"));
    }

    #[test]
    fn a_dotfile_disambiguates_after_the_whole_name() {
        // A leading-dot name has no extension to split on, so the suffix lands
        // after the whole name rather than before the leading dot.
        let trash = owned(&["Trash"]);
        let taken = owned(&[".profile"]);
        let dest = trash_dest_path(&trash, ".profile", &taken).expect("disambiguated");
        assert_eq!(dest.last().map(String::as_str), Some(".profile (2)"));
    }

    #[test]
    fn a_root_trash_dir_is_refused() {
        assert_eq!(
            trash_dest_path(&[], "notes.txt", &[]),
            Err(TrashError::RootTrash)
        );
    }

    #[test]
    fn an_invalid_original_name_is_refused() {
        let trash = owned(&["Trash"]);
        for bad in ["", ".", "..", "a/b", "a:b"] {
            assert_eq!(
                trash_dest_path(&trash, bad, &[]),
                Err(TrashError::InvalidName),
                "name {bad:?} must be refused"
            );
        }
    }

    #[test]
    fn a_disambiguation_past_the_name_limit_is_refused() {
        // The original leaf is exactly at the 255-byte per-name limit (a valid
        // name), but a forced " (2)" disambiguation would push it over: refused
        // (TooLong), never truncated to a name that could collide.
        let trash = owned(&["Trash"]);
        let leaf = "a".repeat(255); // exactly the per-name limit: a valid leaf.
        let taken = vec![leaf.clone()];
        assert_eq!(
            trash_dest_path(&trash, &leaf, &taken),
            Err(TrashError::TooLong)
        );
    }

    // --- FM11: emptying the Trash --------------------------------------------

    #[test]
    fn emptying_removes_every_child_under_the_trash_dir_not_the_dir_itself() {
        let trash = owned(&["Users", "root", "Library", "Trash"]);
        let children = vec![child("notes.txt", false), child("old_project", true)];
        let plan = empty_trash_plan(&trash, &children)
            .expect("a valid listing")
            .expect("a non-empty Trash yields a plan");

        // One target per child, in listing order — the Trash directory itself is
        // never a target, so emptying leaves the (now-empty) folder in place.
        assert_eq!(plan.len(), 2);
        let targets = plan.targets();
        assert_eq!(
            targets[0].path(),
            owned(&["Users", "root", "Library", "Trash", "notes.txt"]).as_slice()
        );
        assert!(!targets[0].is_directory());
        assert_eq!(
            targets[1].path(),
            owned(&["Users", "root", "Library", "Trash", "old_project"]).as_slice()
        );
        assert!(targets[1].is_directory());
        // A directory-backed child means the recursive DeleteWalk is exercised.
        assert!(plan.has_directories());
    }

    #[test]
    fn emptying_an_already_empty_trash_is_a_no_op_not_an_error() {
        let trash = owned(&["Users", "root", "Library", "Trash"]);
        assert_eq!(empty_trash_plan(&trash, &[]), Ok(None));
    }

    #[test]
    fn emptying_a_root_trash_dir_is_refused() {
        // An empty Trash path would spell each child as a top-level root entry:
        // refused rather than risk removing outside Trash.
        let children = vec![child("notes.txt", false)];
        assert_eq!(empty_trash_plan(&[], &children), Err(TrashError::RootTrash));
    }

    #[test]
    fn an_invalid_child_name_refuses_the_whole_empty() {
        let trash = owned(&["Trash"]);
        for bad in ["", ".", "..", "a/b", "a:b"] {
            let children = vec![child("safe.txt", false), child(bad, false)];
            assert_eq!(
                empty_trash_plan(&trash, &children),
                Err(TrashError::InvalidName),
                "a child named {bad:?} must refuse the whole empty"
            );
        }
    }
}

// --- The location a window title carries ----------------------------------

mod title_location {
    use super::comps;
    use crate::vfs::{
        push_title_name, spell_absolute_path, spell_title_location, write_document_title,
    };
    use alloc::string::String;
    use tairix_abi::window_ipc::{WindowTitle, WINDOW_TITLE_MAX};
    use tairix_font::ELLIPSIS;

    /// The title budget an app really has, so the tests measure the fit the
    /// window channel will accept rather than a number of their own.
    const BUDGET: usize = WINDOW_TITLE_MAX;

    #[test]
    fn a_path_that_fits_is_spelled_whole() {
        assert_eq!(spell_title_location(&comps(&[]), BUDGET), "/");
        assert_eq!(
            spell_title_location(&comps(&["Users", "root", "Documents"]), BUDGET),
            "/Users/root/Documents"
        );
    }

    /// The folder the window is *in* is what the reader needs, so the oldest
    /// ancestors go first and the leaf always survives.
    #[test]
    fn a_long_path_drops_leading_components_behind_the_mark() {
        let deep = comps(&["Users", "root", "Documents", "Projects", "tairix", "plans"]);
        let fitted = spell_title_location(&deep, 24);

        assert!(fitted.len() <= 24, "{fitted:?} exceeds the budget");
        assert!(fitted.starts_with(ELLIPSIS), "{fitted:?} carries no mark");
        assert!(fitted.ends_with("/plans"), "{fitted:?} dropped the leaf");
        assert_eq!(fitted, alloc::format!("{ELLIPSIS}/tairix/plans"));
    }

    /// Whole components are dropped, never a partial name: what is shown is a
    /// real path suffix, so it cannot read as a directory that does not exist.
    #[test]
    fn every_kept_component_is_whole() {
        let deep = comps(&["alpha", "bravo", "charlie", "delta"]);
        for budget in 0..=40 {
            let fitted = spell_title_location(&deep, budget);
            assert!(fitted.len() <= budget, "{fitted:?} exceeds {budget}");
            let Some(tail) = fitted.strip_prefix(ELLIPSIS) else {
                continue;
            };
            for component in tail.split('/').filter(|part| !part.is_empty()) {
                assert!(
                    deep.iter().any(|whole| whole == component) || "delta".starts_with(component),
                    "{component:?} is neither a whole component nor a cut leaf"
                );
            }
        }
    }

    /// A name a foreign volume can carry but a title field refuses must still
    /// yield a usable title, or a window could never state where it is. The
    /// path spelled for *opening* the directory keeps the byte as it is.
    #[test]
    fn a_control_character_in_a_name_is_shown_not_refused() {
        let hostile = comps(&["Users", "note\u{7}here"]);

        let title = spell_title_location(&hostile, BUDGET);

        assert!(
            WindowTitle::new(&title).is_ok(),
            "the channel must accept {title:?}"
        );
        assert_eq!(title, "/Users/note\u{FFFD}here");
        assert!(
            spell_absolute_path(&hostile).contains('\u{7}'),
            "the path opened is spelled exactly as it is on disk"
        );
    }

    /// The replacement mark is wider than the byte it stands for, so a name
    /// made entirely of them must still be fitted, not overflow the field.
    #[test]
    fn a_name_of_control_characters_still_fits_the_budget() {
        let noisy = comps(&["Users", &"\u{7}".repeat(40)]);

        let title = spell_title_location(&noisy, BUDGET);

        assert!(title.len() <= BUDGET, "{title:?} exceeds the budget");
        assert!(WindowTitle::new(&title).is_ok());
    }

    /// A document name is spelled into a title within any budget: whole when
    /// it fits, otherwise a prefix cut on a character before the mark, with
    /// every control character shown — so the channel always accepts it.
    #[test]
    fn a_document_name_fits_any_budget_the_channel_accepts() {
        for name in [
            "notes.txt",
            "\u{4e2d}\u{6587}\u{6587}\u{4ef6}.txt",
            "bad\u{7}name.conf",
        ] {
            for budget in 0..=24 {
                let mut title = String::from("*");
                push_title_name(&mut title, name, budget);
                let spelled = &title[1..];
                assert!(spelled.len() <= budget, "{spelled:?} exceeds {budget}");
                assert!(WindowTitle::new(&title).is_ok(), "{title:?} is refused");
                let shown: String = name
                    .chars()
                    .map(|ch| if ch.is_control() { '\u{FFFD}' } else { ch })
                    .collect();
                if shown.len() <= budget {
                    assert_eq!(spelled, shown);
                } else if budget >= ELLIPSIS.len() {
                    let cut = spelled.strip_suffix(ELLIPSIS).expect("the mark");
                    assert!(
                        shown.starts_with(cut),
                        "{cut:?} is not where {shown:?} starts"
                    );
                }
            }
        }
    }

    /// A budget too small even for the mark and the leaf still yields a
    /// readable name rather than nothing. The name shown is always a prefix of
    /// the real one, so a multi-byte `char` is never cut in half.
    #[test]
    fn a_budget_too_small_for_the_leaf_cuts_the_name() {
        const NAME: &str = "Ünterlagen";
        let leaf = comps(&[NAME]);
        for budget in 0..=16 {
            let fitted = spell_title_location(&leaf, budget);
            assert!(fitted.len() <= budget, "{fitted:?} exceeds {budget}");
            let shown = fitted
                .strip_prefix(ELLIPSIS)
                .unwrap_or(fitted.as_str())
                .trim_start_matches('/');
            assert!(
                NAME.starts_with(shown),
                "{shown:?} is not a prefix of the real name"
            );
        }
        assert_eq!(spell_title_location(&leaf, 0), "");
    }

    /// A document window's title leads with the changed mark, follows the
    /// name with the read-only one, ends with the application, and — for any
    /// name at all — is one the channel accepts, the name giving way first.
    #[test]
    fn a_document_title_carries_its_marks_and_always_fits() {
        let mut title = String::from("stale");
        write_document_title(&mut title, "notes.txt", false, false, "TextEdit");
        assert_eq!(title, "notes.txt \u{2014} TextEdit");
        write_document_title(&mut title, "notes.txt", true, true, "TextEdit");
        assert_eq!(title, "*notes.txt (read-only) \u{2014} TextEdit");

        let long = "n".repeat(4 * WINDOW_TITLE_MAX);
        write_document_title(&mut title, &long, true, true, "Paint");
        assert!(WindowTitle::new(&title).is_ok(), "{title:?} is refused");
        assert!(title.starts_with("*nnn"));
        assert!(title.ends_with(&alloc::format!("{ELLIPSIS} (read-only) \u{2014} Paint")));
    }
}

// --- Folder occupancy ----------------------------------------------------
//
// A directory's `size` is `0` and no VFS surface reports a child count, so
// "does this folder hold anything?" is a separate, per-child directory read.
// These tests pin what that read costs as much as what it draws.

mod occupancy {
    use super::BAND;
    use super::*;

    use alloc::rc::Rc;
    use core::cell::RefCell;

    use tairix_abi::fs::FileKind;
    use tairix_abi::time::Time64;
    use tairix_icon::{FolderSample, SampleCard};

    use crate::entry::{EntryKind, Occupancy};
    use crate::media::{folder_sample, icon_for_entry};
    use crate::render::visible_range;
    use crate::vfs::VfsDirectorySource;

    /// The probe tally a test reads while the browser holds the source.
    type Probes = Rc<RefCell<BTreeMap<String, usize>>>;

    /// A source over an in-memory tree that counts every occupancy probe, so
    /// a test can assert what the browser *asked the filesystem*, not only
    /// what it drew.
    struct ProbeFs {
        dirs: BTreeMap<String, Vec<Entry>>,
        refused: BTreeSet<String>,
        probes: Probes,
    }

    impl ProbeFs {
        fn new(root: Vec<Entry>) -> Self {
            let mut dirs = BTreeMap::new();
            dirs.insert("/".to_string(), root);
            Self {
                dirs,
                refused: BTreeSet::new(),
                probes: Probes::default(),
            }
        }

        fn with_dir(mut self, path: &str, children: Vec<Entry>) -> Self {
            self.dirs.insert(path.to_string(), children);
            self
        }

        fn refusing(mut self, path: &str) -> Self {
            self.refused.insert(path.to_string());
            self
        }

        fn tally(&self) -> Probes {
            Rc::clone(&self.probes)
        }
    }

    impl DirectorySource for ProbeFs {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            self.dirs
                .get(&key(components))
                .cloned()
                .map(Listing::Ready)
                .ok_or(Errno::NotFound)
        }

        fn has_children(&mut self, components: &[String]) -> Result<Probe, Errno> {
            let path = key(components);
            *self.probes.borrow_mut().entry(path.clone()).or_insert(0) += 1;
            if self.refused.contains(&path) {
                return Err(Errno::PermissionDenied);
            }
            self.dirs
                .get(&path)
                .map(|children| {
                    if children.is_empty() {
                        Probe::Empty
                    } else {
                        Probe::Holds(FolderSample::default())
                    }
                })
                .ok_or(Errno::NotFound)
        }
    }

    /// How many times `path` was probed.
    fn probes_of(tally: &Probes, path: &str) -> usize {
        tally.borrow().get(path).copied().unwrap_or(0)
    }

    /// How many probes were made in total.
    fn total_probes(tally: &Probes) -> usize {
        tally.borrow().values().sum()
    }

    /// The viewport every test in this module renders and measures against.
    fn viewport() -> Rect {
        Rect::new(0, 0, 400, 400)
    }

    /// A root holding one empty and one occupied directory.
    fn empty_and_full() -> ProbeFs {
        ProbeFs::new(vec![
            Entry::directory("aa-empty"),
            Entry::directory("bb-full"),
        ])
        .with_dir("/aa-empty", Vec::new())
        .with_dir("/bb-full", vec![Entry::file("child")])
    }

    /// Resolve occupancy for everything the browser would draw — what the app
    /// does at the head of each frame — answering whether any cue moved.
    fn resolve_visible<S: DirectorySource>(browser: &mut Browser<S>) -> bool {
        let range = visible_range(browser, Scale::ONE, &Theme::dark(), viewport(), BAND);
        browser.resolve_occupancy(range)
    }

    /// Render `browser` with no artwork, so every icon is the built-in glyph.
    fn frame<S: DirectorySource>(browser: &Browser<S>, artwork: &mut dyn IconArtwork) -> Surface {
        paint(
            browser,
            Scale::ONE,
            &Theme::dark(),
            viewport(),
            &crate::ManagerChrome::none(),
            artwork,
        )
    }

    /// The icon kind each tile of a grid render asked artwork for, in order.
    fn tile_kinds<S: DirectorySource>(browser: &Browser<S>) -> Vec<IconKind> {
        let mut artwork = RecordingArtwork::new(8, Color::rgb(0x10, 0x20, 0x30));
        let _ = frame(browser, &mut artwork);
        content_kinds(&artwork.asked)
    }

    /// A probe batch of `entries`, as one read of a directory answers it.
    fn batch(entries: &[(&str, FileKind)]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(name, kind) in entries {
            let mut record = [0u8; tairix_abi::fs::DirEntry::MAX_LEN];
            let len = tairix_abi::fs::DirEntry {
                kind,
                size: 0,
                allocated: 0,
                modified: tairix_abi::time::Time64::UNIX_EPOCH,
                id: tairix_abi::fs::FileId::NONE,
                nlink: 1,
                name: name.as_bytes(),
                content_gen: 0,
            }
            .encode_into(&mut record)
            .expect("encodes");
            out.extend_from_slice(&record[..len]);
        }
        out
    }

    #[test]
    fn a_probing_vfs_source_samples_what_one_batch_holds() {
        let held = batch(&[
            ("a.jpg", FileKind::Regular),
            ("sub", FileKind::Directory),
            ("b.jpg", FileKind::Regular),
            ("notes.txt", FileKind::Regular),
        ]);
        // The listing names no file, so the pictures are drawn as their kind.
        let want = FolderSample::new([
            SampleCard::Kind(IconKind::ImageJpeg),
            SampleCard::Kind(IconKind::Text),
            SampleCard::Kind(IconKind::ImageJpeg),
        ]);
        for (records, want) in [(Vec::new(), Probe::Empty), (held, Probe::Holds(want))] {
            let mut source = VfsDirectorySource::probing(
                |_: &str| Err::<Vec<u8>, _>(Errno::NotImplemented),
                NoLinks,
                move |_: &str, buf: &mut [u8]| {
                    buf[..records.len()].copy_from_slice(&records);
                    Ok(records.len())
                },
            );
            assert_eq!(source.has_children(&["d".to_string()]), Ok(want));
        }

        // Any other refusal is surfaced, never guessed at.
        let mut refusing = VfsDirectorySource::probing(
            |_: &str| Err::<Vec<u8>, _>(Errno::NotImplemented),
            NoLinks,
            |_: &str, _: &mut [u8]| Err(Errno::PermissionDenied),
        );
        assert_eq!(
            refusing.has_children(&["d".to_string()]),
            Err(Errno::PermissionDenied)
        );
    }

    /// One listed member of the folder `/d`: identified when `node` is
    /// non-zero.
    fn member(name: &'static str, kind: FileKind, node: u64) -> tairix_abi::fs::DirEntry<'static> {
        tairix_abi::fs::DirEntry {
            kind,
            size: 64,
            allocated: 64,
            modified: Time64::from_secs(1_700_000_000),
            id: if node == 0 {
                tairix_abi::fs::FileId::NONE
            } else {
                tairix_abi::fs::FileId {
                    volume: [5; 16],
                    node,
                }
            },
            nlink: 1,
            name: name.as_bytes(),
            content_gen: 0,
        }
    }

    /// What each card of `sample` is: the member's kind, and the path of the
    /// picture it prints, if it prints one.
    fn cards(sample: &FolderSample) -> Vec<(IconKind, Option<String>)> {
        sample
            .cards()
            .map(|card| {
                (
                    card.kind(),
                    card.picture().map(|picture| picture.path.clone()),
                )
            })
            .collect()
    }

    fn sample_of(members: &[tairix_abi::fs::DirEntry<'_>]) -> FolderSample {
        folder_sample(&["d".to_string()], members.iter().copied())
    }

    /// Variety first: one card for each of the most frequent families, ties
    /// going to whichever was seen first, each the family's first member in
    /// listing order — an identified picture as its own content. Folders and
    /// files of no recognised type make no card, and a bundle is a program.
    #[test]
    fn a_folder_sample_shows_each_frequent_family_first() {
        let sample = sample_of(&[
            member("song.mp3", FileKind::Regular, 1),
            member("a.png", FileKind::Regular, 2),
            member("b.jpg", FileKind::Regular, 3),
            member("c.jpg", FileKind::Regular, 4),
            member("film.mkv", FileKind::Regular, 5),
            member("other.mp3", FileKind::Regular, 6),
            member("blob.bin", FileKind::Regular, 7),
            member("Folder", FileKind::Directory, 8),
            member("Paint.app", FileKind::Directory, 9),
        ]);
        assert_eq!(
            cards(&sample),
            [
                (IconKind::ImagePng, Some(String::from("/d/a.png"))),
                (IconKind::Audio, None),
                (IconKind::Video, None),
            ],
            "three pictures, two songs, then a video and a program tied, the video seen first"
        );
        assert!(sample_of(&[
            member("Folder", FileKind::Directory, 1),
            member("x.bin", FileKind::Regular, 2),
        ])
        .is_empty());
    }

    /// Then fill: cards left over go round the families again while one has
    /// members not yet shown, so a folder of photos shows three of them, of
    /// text three text cards, and of photos and one PDF two photos and the PDF.
    #[test]
    fn a_folder_sample_fills_from_the_families_it_holds() {
        let photos = sample_of(&[
            member("1.jpg", FileKind::Regular, 1),
            member("2.jpg", FileKind::Regular, 2),
            member("3.jpg", FileKind::Regular, 3),
            member("4.jpg", FileKind::Regular, 4),
        ]);
        assert_eq!(
            cards(&photos),
            ["/d/1.jpg", "/d/2.jpg", "/d/3.jpg"]
                .map(|path| (IconKind::ImageJpeg, Some(String::from(path))))
        );
        let mixed = sample_of(&[
            member("1.jpg", FileKind::Regular, 1),
            member("2.jpg", FileKind::Regular, 2),
            member("report.pdf", FileKind::Regular, 3),
            member("3.jpg", FileKind::Regular, 4),
        ]);
        assert_eq!(
            cards(&mixed),
            [
                (IconKind::ImageJpeg, Some(String::from("/d/1.jpg"))),
                (IconKind::Pdf, None),
                (IconKind::ImageJpeg, Some(String::from("/d/2.jpg"))),
            ]
        );
        let text = sample_of(&[
            member("a.txt", FileKind::Regular, 1),
            member("b.txt", FileKind::Regular, 2),
            member("c.txt", FileKind::Regular, 3),
            member("d.txt", FileKind::Regular, 4),
        ]);
        assert_eq!(cards(&text), vec![(IconKind::Text, None); 3]);
        let one = sample_of(&[member("only.txt", FileKind::Regular, 1)]);
        assert_eq!(cards(&one), [(IconKind::Text, None)]);
    }

    /// A picture the listing names no file for, or one that is a link, is
    /// drawn as its kind: there is nothing an open could be checked against.
    #[test]
    fn an_unidentified_or_linked_picture_is_drawn_as_its_kind() {
        let sample = sample_of(&[
            member("plain.png", FileKind::Regular, 0),
            member("link.png", FileKind::Symlink, 2),
        ]);
        assert!(!sample.has_pictures(), "{sample:?}");
    }

    #[test]
    fn a_source_built_without_a_probe_offers_none() {
        // The trusted picker's source: it never reads a directory it is only
        // displaying, so the cue degrades to the plain folder.
        let mut picker =
            VfsDirectorySource::new(|_: &str| Err::<Vec<u8>, _>(Errno::NotFound), NoLinks);
        assert_eq!(
            picker.has_children(&["d".to_string()]),
            Err(Errno::NotImplemented)
        );
    }

    #[test]
    fn the_grid_draws_the_filled_folder_only_for_a_directory_that_holds_something() {
        let mut browser = Browser::open_root(empty_and_full()).expect("root");
        browser.set_view_mode(ViewMode::Grid);

        assert_eq!(
            tile_kinds(&browser),
            vec![IconKind::Folder, IconKind::Folder],
            "an unprobed folder never claims contents"
        );

        resolve_visible(&mut browser);
        assert_eq!(
            tile_kinds(&browser),
            vec![IconKind::Folder, IconKind::FolderFilled]
        );
    }

    #[test]
    fn the_list_row_draws_the_filled_folder_only_for_a_directory_that_holds_something() {
        let mut browser = Browser::open_root(empty_and_full()).expect("root");
        browser.set_view_mode(ViewMode::List);
        let plain = frame(&browser, &mut NoArtwork).pixels().to_vec();

        resolve_visible(&mut browser);
        assert_eq!(
            browser
                .entries()
                .iter()
                .map(|entry| icon_for_entry(entry, browser.components()))
                .collect::<Vec<_>>(),
            vec![IconKind::Folder, IconKind::FolderFilled]
        );
        assert_ne!(frame(&browser, &mut NoArtwork).pixels(), plain.as_slice());

        // A refused probe is indistinguishable from an empty folder on
        // screen: an unknown answer never claims contents.
        let mut refused = Browser::open_root(empty_and_full().refusing("/bb-full")).expect("root");
        refused.set_view_mode(ViewMode::List);
        resolve_visible(&mut refused);
        assert_eq!(frame(&refused, &mut NoArtwork).pixels(), plain.as_slice());
    }

    #[test]
    fn a_refused_probe_is_recorded_and_never_retried() {
        let source = ProbeFs::new(vec![Entry::directory("locked")]).refusing("/locked");
        let tally = source.tally();
        let mut browser = Browser::open_root(source).expect("root");

        for _ in 0..5 {
            resolve_visible(&mut browser);
        }

        assert_eq!(probes_of(&tally, "/locked"), 1);
        assert_eq!(browser.entries()[0].occupancy(), &Occupancy::Indeterminate);
        assert_eq!(
            icon_for_entry(&browser.entries()[0], browser.components()),
            IconKind::Folder
        );
    }

    /// A source that probes elsewhere leaves the entry unanswered, so a paint
    /// may resolve occupancy without doing any I/O and the cue is drawn a frame
    /// later. The re-ask is what makes that work: latching a pending probe as
    /// indeterminate would mean the answer never arrived.
    #[test]
    fn a_pending_probe_leaves_the_entry_unanswered_and_is_asked_again() {
        /// A source whose probe is answered by somebody else: the first ask
        /// records it, a later one answers.
        struct Deferring {
            entries: Vec<Entry>,
            asks: usize,
        }

        impl DirectorySource for Deferring {
            fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
                Ok(Listing::Ready(self.entries.clone()))
            }

            fn has_children(&mut self, _components: &[String]) -> Result<Probe, Errno> {
                self.asks += 1;
                if self.asks > 2 {
                    return Ok(Probe::Holds(FolderSample::default()));
                }
                Ok(Probe::Pending)
            }
        }

        let mut browser = Browser::open_root(Deferring {
            entries: vec![Entry::directory("later")],
            asks: 0,
        })
        .expect("root");

        resolve_visible(&mut browser);
        assert_eq!(
            browser.entries()[0].occupancy(),
            &Occupancy::Unprobed,
            "a pending probe must not latch an answer"
        );
        resolve_visible(&mut browser);
        assert_eq!(browser.entries()[0].occupancy(), &Occupancy::Unprobed);
        resolve_visible(&mut browser);
        assert_eq!(
            browser.entries()[0].occupancy(),
            &Occupancy::NonEmpty(FolderSample::default())
        );
    }

    /// A deferring app repaints on the resolve that *adopts* an answer, so the
    /// resolve has to say whether one moved: a delivery that answered nothing
    /// the window is showing must not cost a frame.
    /// A source whose probe is answered by somebody else, as the deferring
    /// app's is.
    struct Deferring {
        answer: Rc<RefCell<Option<bool>>>,
    }

    impl DirectorySource for Deferring {
        fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
            Ok(Listing::Ready(vec![Entry::directory("later")]))
        }

        fn has_children(&mut self, _components: &[String]) -> Result<Probe, Errno> {
            Ok(match *self.answer.borrow() {
                Some(true) => Probe::Holds(FolderSample::default()),
                Some(false) => Probe::Empty,
                None => Probe::Pending,
            })
        }
    }

    #[test]
    fn a_resolve_reports_whether_any_cue_moved() {
        let mut browser = Browser::open_root(empty_and_full()).expect("root");

        assert!(
            resolve_visible(&mut browser),
            "the first resolve answers both folders"
        );
        assert!(
            !resolve_visible(&mut browser),
            "nothing is left unprobed, so nothing moved and no frame is owed"
        );

        // A refused probe is an answer too: it moves the entry off
        // `Unprobed`, so it is adopted once and never asked again. Like an
        // empty folder it draws the plain picture it drew before, so neither
        // owes a frame.
        let source = empty_and_full().refusing("/bb-full");
        let tally = source.tally();
        let mut refused = Browser::open_root(source).expect("root");
        assert!(!resolve_visible(&mut refused));
        assert_eq!(refused.entries()[1].occupancy(), &Occupancy::Indeterminate);
        assert!(!resolve_visible(&mut refused));
        assert_eq!(probes_of(&tally, "/bb-full"), 1, "never asked again");

        // A source that answers elsewhere moves nothing until its answer
        // lands, which is exactly the deferred app's case.
        let answer = Rc::new(RefCell::new(None));
        let mut deferred = Browser::open_root(Deferring {
            answer: Rc::clone(&answer),
        })
        .expect("root");
        assert!(
            !resolve_visible(&mut deferred),
            "a pending probe moves nothing, so a paint that only asked owes no second frame"
        );
        *answer.borrow_mut() = Some(true);
        assert!(resolve_visible(&mut deferred));
        assert_eq!(
            icon_for_entry(&deferred.entries()[0], deferred.components()),
            IconKind::FolderFilled
        );
        assert!(!resolve_visible(&mut deferred));
    }

    #[test]
    fn files_and_bundles_are_never_probed() {
        let source = ProbeFs::new(vec![
            Entry::file("notes.txt"),
            Entry::new("Thing.app", EntryKind::Bundle, 0, Time64::UNIX_EPOCH),
        ])
        .with_dir("/Thing.app", vec![Entry::file("Run")]);
        let tally = source.tally();
        let mut browser = Browser::open_root(source).expect("root");

        resolve_visible(&mut browser);

        assert_eq!(total_probes(&tally), 0);
        assert!(browser
            .entries()
            .iter()
            .all(|entry| entry.occupancy() == &Occupancy::Unprobed));
    }

    #[test]
    fn an_answered_entry_is_not_probed_again_and_a_reload_re_probes() {
        let source = empty_and_full();
        let tally = source.tally();
        let mut browser = Browser::open_root(source).expect("root");

        resolve_visible(&mut browser);
        resolve_visible(&mut browser);
        assert_eq!(probes_of(&tally, "/aa-empty"), 1);
        assert_eq!(probes_of(&tally, "/bb-full"), 1);

        // A reload probes every folder again, but shows each previous answer
        // meanwhile rather than blinking back to the plain folder.
        let answered: Vec<Occupancy> = browser
            .entries()
            .iter()
            .map(|entry| entry.occupancy().clone())
            .collect();
        browser.refresh().expect("refresh");
        let shown: Vec<Occupancy> = browser
            .entries()
            .iter()
            .map(|entry| entry.occupancy().clone())
            .collect();
        assert_eq!(shown, answered);
        assert!(browser
            .entries()
            .iter()
            .filter(|entry| entry.is_directory())
            .all(Entry::needs_occupancy_probe));
        resolve_visible(&mut browser);
        assert_eq!(probes_of(&tally, "/aa-empty"), 2);
        assert_eq!(probes_of(&tally, "/bb-full"), 2);
    }

    #[test]
    fn a_huge_listing_probes_only_what_is_on_screen() {
        const ENTRIES: usize = 100_000;
        let mut root = Vec::with_capacity(ENTRIES);
        for i in 0..ENTRIES {
            root.push(Entry::directory(format!("d{i:06}")));
        }
        let mut source = ProbeFs::new(root);
        for i in 0..ENTRIES {
            source.dirs.insert(format!("/d{i:06}"), Vec::new());
        }
        let tally = source.tally();
        let mut browser = Browser::open_root(source).expect("root");
        browser.set_view_mode(ViewMode::List);

        let on_screen = visible_range(&browser, Scale::ONE, &Theme::dark(), viewport(), BAND).len();
        assert!(
            on_screen > 0 && on_screen < ENTRIES,
            "the window, not the listing"
        );

        resolve_visible(&mut browser);
        assert_eq!(total_probes(&tally), on_screen);

        // Scrolling one row on pays for the row that entered the window and
        // re-asks nothing already answered.
        browser.set_scroll_offset(u64::from(crate::render::row_height(
            Scale::ONE,
            &Theme::dark(),
        )));
        resolve_visible(&mut browser);
        assert_eq!(total_probes(&tally), on_screen + 1);

        // Scrolling back over answered rows costs nothing at all.
        browser.set_scroll_offset(0);
        resolve_visible(&mut browser);
        assert_eq!(total_probes(&tally), on_screen + 1);
    }
}

// --- Entry labels --------------------------------------------------------

mod entry_labels {
    use super::*;

    use crate::render::entry_label;

    /// One entry named `name`, of `kind`, rendered in the list view.
    fn one_entry_list(entry: Entry) -> Surface {
        let mut dirs = BTreeMap::new();
        dirs.insert("/".to_string(), vec![entry]);
        let mut browser = Browser::open_root(MockFs {
            dirs,
            denied: BTreeSet::new(),
            deny_after_first: BTreeSet::new(),
            reads: BTreeMap::new(),
            root_after_refresh: None,
        })
        .expect("root");
        browser.set_view_mode(ViewMode::List);
        paint(
            &browser,
            Scale::ONE,
            &Theme::dark(),
            Rect::new(0, 0, 400, 400),
            &crate::ManagerChrome::none(),
            &mut NoArtwork,
        )
    }

    #[test]
    fn a_label_is_the_bare_name_whatever_the_entry_is() {
        assert_eq!(entry_label(&Entry::directory("thing")), "thing");
        assert_eq!(entry_label(&Entry::file("thing")), "thing");
        assert_eq!(
            entry_label(&Entry::new(
                "Thing.app",
                crate::entry::EntryKind::Bundle,
                0,
                tairix_abi::time::Time64::UNIX_EPOCH,
            )),
            "Thing.app"
        );
    }

    #[test]
    fn a_list_row_tells_a_directory_from_a_file_by_its_icon_alone() {
        // The labels are now identical, so any difference on screen is the
        // row's leading icon — the cue that replaced the name suffix.
        let folder = one_entry_list(Entry::directory("thing"));
        let file = one_entry_list(Entry::file("thing"));
        assert_ne!(folder.pixels(), file.pixels());
    }
}

/// The asynchronous-source half of the navigation model: a listing that is not
/// ready yet, and the `resume` that commits it.
mod deferred {
    use super::*;

    use alloc::rc::Rc;
    use core::cell::RefCell;

    /// A source that answers `Pending` for every directory until the test says
    /// the answer has landed — the shape a session's worker-thread source has,
    /// with the wake replaced by a test's own `deliver`.
    #[derive(Clone)]
    struct Deferred {
        tree: BTreeMap<String, Vec<Entry>>,
        /// Directories whose answer has been delivered.
        ready: Rc<RefCell<BTreeSet<String>>>,
        /// Directories the source has been asked for, in order, so a test can
        /// see that asking twice for the same one starts no second read.
        asked: Rc<RefCell<Vec<String>>>,
        /// Directories that answer with a refusal once delivered.
        refused: Rc<RefCell<BTreeSet<String>>>,
        /// Directories the source was asked to read afresh, in order.
        refreshed: Rc<RefCell<Vec<String>>>,
    }

    impl Deferred {
        fn new(tree: BTreeMap<String, Vec<Entry>>) -> Self {
            Self {
                tree,
                ready: Rc::new(RefCell::new(BTreeSet::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
                refused: Rc::new(RefCell::new(BTreeSet::new())),
                refreshed: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn deliver(&self, path: &str) {
            self.ready.borrow_mut().insert(String::from(path));
        }

        fn refuse(&self, path: &str) {
            self.refused.borrow_mut().insert(String::from(path));
            self.deliver(path);
        }

        fn asked_for(&self, path: &str) -> usize {
            self.asked
                .borrow()
                .iter()
                .filter(|asked| asked.as_str() == path)
                .count()
        }

        fn refreshed_for(&self, path: &str) -> usize {
            self.refreshed
                .borrow()
                .iter()
                .filter(|refreshed| refreshed.as_str() == path)
                .count()
        }
    }

    impl DirectorySource for Deferred {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            let path = key(components);
            if !self.ready.borrow().contains(&path) {
                self.asked.borrow_mut().push(path);
                return Ok(Listing::Pending);
            }
            if self.refused.borrow().contains(&path) {
                return Err(Errno::PermissionDenied);
            }
            self.tree
                .get(&path)
                .cloned()
                .map(Listing::Ready)
                .ok_or(Errno::NotFound)
        }

        fn refresh(&mut self, components: &[String]) -> Result<Listing, Errno> {
            self.refreshed.borrow_mut().push(key(components));
            self.list(components)
        }
    }

    fn tree() -> BTreeMap<String, Vec<Entry>> {
        let mut tree = BTreeMap::new();
        tree.insert(
            String::from("/"),
            alloc::vec![Entry::directory("Users"), Entry::file("readme")],
        );
        tree.insert(String::from("/Users"), alloc::vec![Entry::file("who")]);
        tree
    }

    fn browser() -> (Browser<Deferred>, Deferred) {
        let source = Deferred::new(tree());
        let browser = Browser::open_root(source.clone()).expect("open");
        (browser, source)
    }

    #[test]
    fn an_open_whose_listing_is_not_ready_yet_is_empty_and_listing() {
        let (browser, source) = browser();
        assert!(browser.is_listing());
        assert_eq!(browser.listing_target(), Some(&[][..]));
        assert!(browser.entries().is_empty());
        assert_eq!(source.asked_for("/"), 1);
    }

    #[test]
    fn resume_starts_no_second_read_and_commits_when_the_answer_lands() {
        let (mut browser, source) = browser();
        assert!(!browser.resume().expect("resume"), "nothing arrived yet");
        assert!(browser.entries().is_empty());
        assert_eq!(
            source.asked_for("/"),
            2,
            "each ask is the source's own to deduplicate; the browser asks once per resume"
        );

        source.deliver("/");
        assert!(browser.resume().expect("resume"), "the answer committed");
        assert!(!browser.is_listing());
        assert_eq!(browser.entries().len(), 2);
        assert!(!browser.resume().expect("resume"), "nothing left pending");
    }

    #[test]
    fn a_pending_navigation_moves_nothing_until_it_commits() {
        let (mut browser, source) = browser();
        source.deliver("/");
        browser.resume().expect("resume");

        let before = browser.components().to_vec();
        let _ = browser.select(0);
        browser.open_selected().expect("navigate");
        assert!(browser.is_listing());
        assert_eq!(
            browser.components(),
            before.as_slice(),
            "the location moved before the listing arrived"
        );
        assert!(!browser.can_go_back(), "the history moved early");
        assert_eq!(browser.entries().len(), 2, "the entries were thrown away");

        source.deliver("/Users");
        assert!(browser.resume().expect("resume"));
        assert_eq!(browser.components(), ["Users"]);
        assert!(browser.can_go_back(), "the departure was not recorded");
        assert_eq!(browser.entries().len(), 1);
    }

    #[test]
    fn a_second_navigation_replaces_the_pending_one() {
        let (mut browser, source) = browser();
        source.deliver("/");
        browser.resume().expect("resume");

        let _ = browser.select(0);
        browser.open_selected().expect("navigate");
        assert_eq!(browser.listing_target(), Some(&[String::from("Users")][..]));
        // Asking to re-read where it still is, while `/Users` is in flight: the
        // last gesture wins and the earlier target is forgotten.
        source.ready.borrow_mut().clear();
        browser.refresh().expect("refresh");
        assert_eq!(browser.listing_target(), Some(&[][..]));
        source.deliver("/Users");
        assert!(
            !browser.resume().expect("resume"),
            "the abandoned target must not commit"
        );
    }

    #[test]
    fn a_refused_pending_listing_leaves_the_browser_where_it_was() {
        let (mut browser, source) = browser();
        source.deliver("/");
        browser.resume().expect("resume");

        let _ = browser.select(0);
        browser.open_selected().expect("navigate");
        source.refuse("/Users");
        assert!(matches!(
            browser.resume(),
            Err(BrowseError::Source(Errno::PermissionDenied))
        ));
        assert!(!browser.is_listing(), "a refusal leaves nothing pending");
        assert!(browser.components().is_empty(), "the location moved");
        assert!(!browser.can_go_back(), "the history moved");
        assert_eq!(browser.entries().len(), 2, "the entries were lost");
    }

    #[test]
    fn back_and_forward_commit_their_own_history_move() {
        let (mut browser, source) = browser();
        source.deliver("/");
        source.deliver("/Users");
        browser.resume().expect("resume");
        let _ = browser.select(0);
        browser.open_selected().expect("navigate");
        browser.resume().expect("resume");
        assert_eq!(browser.components(), ["Users"]);

        // Back and forward are pending too, and each commits exactly the move
        // its own step owes.
        source.ready.borrow_mut().clear();
        assert!(browser.go_back().expect("back"));
        assert_eq!(browser.components(), ["Users"], "back moved early");
        source.deliver("/");
        assert!(browser.resume().expect("resume"));
        assert!(browser.components().is_empty());
        assert!(browser.can_go_forward());

        source.ready.borrow_mut().clear();
        assert!(browser.go_forward().expect("forward"));
        source.deliver("/Users");
        assert!(browser.resume().expect("resume"));
        assert_eq!(browser.components(), ["Users"]);
        assert!(browser.can_go_back());
        assert!(!browser.can_go_forward());
    }

    /// A reload is asked because the directory may have changed, so only it
    /// asks the source for a read that begins now; opening, moving and
    /// resuming collect what is on its way.
    #[test]
    fn only_a_reload_asks_the_source_afresh() {
        let (mut browser, source) = browser();
        source.deliver("/");
        source.deliver("/Users");
        browser.resume().expect("resume");
        let _ = browser.select(0);
        browser.open_selected().expect("navigate");
        browser.resume().expect("resume");
        assert!(browser.go_back().expect("back"));
        assert_eq!(source.refreshed_for("/"), 0, "a move asked afresh");

        browser.refresh().expect("refresh");
        assert_eq!(source.refreshed_for("/"), 1);
        assert_eq!(source.refreshed_for("/Users"), 0);
    }

    #[test]
    fn a_refresh_that_is_pending_keeps_showing_what_it_had() {
        let (mut browser, source) = browser();
        source.deliver("/");
        browser.resume().expect("resume");

        source.ready.borrow_mut().clear();
        browser.refresh().expect("refresh");
        assert!(browser.is_listing());
        assert_eq!(browser.entries().len(), 2, "a reload blanked the view");
        source.deliver("/");
        assert!(browser.resume().expect("resume"));
        assert_eq!(browser.entries().len(), 2);
    }
}

/// The listing cue: what the view shows while a read is in flight.
mod listing_cue {
    use super::*;
    use crate::render::LISTING_MESSAGE;

    /// A source that never answers, so every view built over it is listing.
    pub(super) struct NeverReady;

    impl DirectorySource for NeverReady {
        fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
            Ok(Listing::Pending)
        }
    }

    fn painted<S: DirectorySource>(browser: &Browser<S>) -> Surface {
        paint(
            browser,
            Scale::ONE,
            &Theme::dark(),
            Rect::new(0, 0, 320, 240),
            &crate::ManagerChrome::none(),
            &mut NoArtwork,
        )
    }

    #[test]
    fn the_cue_is_shown_while_a_first_listing_is_in_flight() {
        let waiting = Browser::open_root(NeverReady).expect("open");
        let ready = Browser::open_root(MockFs::fixture()).expect("open");
        assert!(waiting.is_listing());
        assert_ne!(
            painted(&waiting).pixels(),
            painted(&ready).pixels(),
            "a listing view drew the same pixels as a listed one"
        );
        assert!(!LISTING_MESSAGE.is_empty(), "the cue has text to draw");
    }

    /// A source whose root holds `count` files and whose every other folder is
    /// still being read.
    struct RootThenPending(usize);

    impl DirectorySource for RootThenPending {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            if components.is_empty() {
                Ok(Listing::Ready(
                    (0..self.0)
                        .map(|n| Entry::file(alloc::format!("f{n}")))
                        .collect(),
                ))
            } else {
                Ok(Listing::Pending)
            }
        }
    }

    /// The window [`painted`] draws, and the band its chrome shows.
    const WINDOW: Rect = Rect::new(0, 0, 320, 240);
    const BAND: crate::ToolbarBand = crate::ManagerChrome::none().toolbar;

    #[test]
    fn the_bar_stands_beside_the_cue_as_it_does_beside_an_empty_folder() {
        let theme = Theme::dark();
        let bar = crate::render::scrollbar_bounds(Scale::ONE, &theme, WINDOW, BAND)
            .expect("the window has a gutter");
        let gutter = |surface: &Surface| -> Vec<_> {
            let (x, y) = (
                u32::try_from(bar.left()).expect("on screen"),
                u32::try_from(bar.top()).expect("on screen"),
            );
            (y..y + bar.height)
                .flat_map(|row| (x..x + bar.width).map(move |column| (column, row)))
                .map(|(column, row)| surface.get(column, row))
                .collect()
        };
        let waiting = painted(&Browser::open_root(NeverReady).expect("open"));
        let empty = painted(&Browser::open_root(RootThenPending(0)).expect("open"));
        assert_eq!(
            gutter(&waiting),
            gutter(&empty),
            "the cue keeps the resting bar a listed folder shows"
        );
        let ground = waiting.get(2, WINDOW.height - 3);
        assert!(
            gutter(&waiting).iter().any(|pixel| *pixel != ground),
            "a bar is drawn there, not the bare ground"
        );
    }

    #[test]
    fn nothing_undrawn_answers_while_another_folder_is_read() {
        let theme = Theme::dark();
        let mut browser = Browser::open_root(RootThenPending(200)).expect("open");
        browser.set_view_mode(crate::ViewMode::Grid);
        let shown = crate::render::entry_rect(&browser, Scale::ONE, &theme, WINDOW, BAND, 0)
            .expect("the premise: the root's entries show");
        browser
            .navigate_to(alloc::vec![String::from("Elsewhere")])
            .expect("the read starts");
        assert!(browser.is_listing(), "the other folder is still being read");
        assert!(
            crate::render::visible_range(&browser, Scale::ONE, &theme, WINDOW, BAND).is_empty(),
            "the entries the cue stands in for are not on screen"
        );
        assert_eq!(
            crate::render::entry_rect(&browser, Scale::ONE, &theme, WINDOW, BAND, 0),
            None
        );
        assert_eq!(
            crate::render::entry_index_at(
                &browser,
                Scale::ONE,
                &theme,
                WINDOW,
                BAND,
                shown.center()
            ),
            None,
            "a press where an undrawn entry was finds nothing"
        );
        assert!(
            !crate::render::scroll_model(&browser, Scale::ONE, &theme, WINDOW, BAND)
                .range()
                .is_scrollable(),
            "and the bar beside the cue has nothing to scroll"
        );
    }

    #[test]
    fn a_reload_of_what_is_already_shown_keeps_showing_it() {
        // A re-read of the current directory must not blank the view: a
        // periodic re-list would otherwise flicker every time.
        let mut browser = Browser::open_root(MockFs::fixture()).expect("open");
        let settled = painted(&browser).pixels().to_vec();
        let mut deferring = Browser::open_root(MockFs::fixture()).expect("open");
        deferring.refresh().expect("refresh");
        assert!(!deferring.is_listing(), "the mock source answers at once");
        browser.refresh().expect("refresh");
        assert_eq!(painted(&browser).pixels(), settled.as_slice());
    }
}

/// A file-manager window fitted to its listing (`plans/NEW-FILEMANAGER.md`
/// FM16, FM17): the height a listing fills, the ceiling that declares it, the
/// height a window opens at, and the glass it is drawn on.
mod window_fit {
    use super::*;
    use crate::render::{entry_rect, fitted_height, render_into, visible_range};
    use crate::{
        browser_floor, fitted_sizing, manager_opening, win_floor_width, ToolbarBand,
        MANAGER_TOOLBAR_BAND, MANAGER_VIEW_MODE, WIN_HEIGHT, WIN_WIDTH,
    };
    use tairix_controls::testkit::text_ladder;
    use tairix_controls::{ground_fill, ChromeLayer};

    /// A root listing `count` files.
    struct Files(usize);

    impl DirectorySource for Files {
        fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
            let entries = (0..self.0)
                .map(|n| Entry::file(alloc::format!("f{n}")))
                .collect();
            Ok(Listing::Ready(entries))
        }
    }

    fn grid(count: usize) -> Browser<Files> {
        let mut browser = Browser::open_root(Files(count)).expect("the root lists");
        browser.set_view_mode(MANAGER_VIEW_MODE);
        browser
    }

    fn fitted(count: usize, width: u32, theme: &Theme) -> u32 {
        fitted_height(
            &grid(count),
            width,
            Scale::ONE,
            theme,
            None,
            MANAGER_TOOLBAR_BAND,
        )
        .expect("a listed folder fits")
    }

    #[test]
    fn a_listing_fills_more_as_it_grows_and_less_as_it_widens() {
        let theme = Theme::dark();
        assert_eq!(
            fitted(0, WIN_WIDTH, &theme),
            0,
            "an empty folder fills nothing"
        );
        assert!(fitted(1, WIN_WIDTH, &theme) > 0);
        assert!(fitted(40, WIN_WIDTH, &theme) > fitted(4, WIN_WIDTH, &theme));
        assert!(
            fitted(40, WIN_WIDTH * 2, &theme) < fitted(40, WIN_WIDTH, &theme),
            "a wider grid folds into fewer lines"
        );
        let waiting = Browser::open_root(listing_cue::NeverReady).expect("open");
        assert_eq!(
            fitted_height(
                &waiting,
                WIN_WIDTH,
                Scale::ONE,
                &theme,
                None,
                MANAGER_TOOLBAR_BAND
            ),
            None,
            "a listing still being read fits nothing yet"
        );
    }

    /// The floor a manager window opens with at `scale` in `theme`.
    fn opening_floor(scale: Scale, theme: &Theme) -> crate::WindowSizing {
        browser_floor(
            win_floor_width(scale, theme),
            MANAGER_VIEW_MODE,
            MANAGER_TOOLBAR_BAND,
            scale,
            theme,
        )
    }

    /// A root listing `count` files, shown in `view`.
    fn viewed(count: usize, view: ViewMode) -> Browser<Files> {
        let mut browser = Browser::open_root(Files(count)).expect("the root lists");
        browser.set_view_mode(view);
        browser
    }

    /// The window can be made exactly one row of its listing tall: the floor is
    /// what a one-row listing fills, and a window at it draws one whole row and
    /// nothing of the next — for both views, either band, at any density.
    #[test]
    fn the_floor_is_one_whole_row_of_the_listing_beneath_the_bands() {
        for theme in [Theme::dark(), text_ladder(30)] {
            for scale in [Scale::ONE, Scale::from_percent(200).expect("scale")] {
                for view in [ViewMode::Grid, ViewMode::List] {
                    for toolbar in [ToolbarBand::Hidden, ToolbarBand::Shown] {
                        let width = scale.scale_length(WIN_WIDTH);
                        let floor = browser_floor(
                            win_floor_width(scale, &theme),
                            view,
                            toolbar,
                            scale,
                            &theme,
                        )
                        .min_height_px();
                        let one_row =
                            fitted_height(&viewed(1, view), width, scale, &theme, None, toolbar);
                        assert_eq!(one_row, Some(floor), "{view:?} {toolbar:?} {scale:?}");

                        let long = viewed(200, view);
                        let window = Rect::new(0, 0, width, floor);
                        let shown = visible_range(&long, scale, &theme, window, toolbar);
                        assert!(!shown.is_empty());
                        let first = entry_rect(&long, scale, &theme, window, toolbar, 0)
                            .expect("the first row shows");
                        for index in shown.clone() {
                            let rect = entry_rect(&long, scale, &theme, window, toolbar, index)
                                .expect("drawn");
                            assert_eq!(
                                (rect.top(), rect.height),
                                (first.top(), first.height),
                                "{view:?} {toolbar:?}: entry {index} is one whole row's"
                            );
                            assert!(rect.bottom() <= window.bottom());
                        }
                        assert!(
                            entry_rect(&long, scale, &theme, window, toolbar, shown.end).is_none(),
                            "{view:?} {toolbar:?}: nothing of the next row shows"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_ceiling_is_the_listing_and_never_below_the_floor() {
        let theme = Theme::dark();
        let floor = opening_floor(Scale::ONE, &theme);
        let short = fitted_sizing(floor, 1);
        assert_eq!(short.min_height_px(), floor.min_height_px());
        assert_eq!(
            short.max_height_px(),
            floor.min_height_px(),
            "a listing shorter than the floor is held to it"
        );
        let tall = fitted_sizing(floor, 5000);
        assert_eq!(tall.max_height_px(), 5000);
        assert_eq!(tall.min_width_px(), floor.min_width_px());
        assert_eq!(tall.max_width_px(), 0, "a listing re-flows into any width");
    }

    #[test]
    fn a_window_opens_as_tall_as_its_listing_up_to_the_browser_height() {
        let theme = Theme::dark();
        let size = (WIN_WIDTH, WIN_HEIGHT);
        let floor = opening_floor(Scale::ONE, &theme).min_height_px();
        let (empty, sizing) = manager_opening(&grid(0), size, Scale::ONE, &theme);
        assert_eq!(
            empty,
            (WIN_WIDTH, floor),
            "an empty folder opens a short window"
        );
        assert_eq!(sizing.max_height_px(), floor);
        let some = fitted(10, WIN_WIDTH, &theme);
        assert!(
            some > floor && some < WIN_HEIGHT,
            "the fixture must sit between the floor and the browser height: {some}"
        );
        let (opened, sizing) = manager_opening(&grid(10), size, Scale::ONE, &theme);
        assert_eq!(opened, (WIN_WIDTH, some));
        assert_eq!(sizing.max_height_px(), some);
        let (full, sizing) = manager_opening(&grid(500), size, Scale::ONE, &theme);
        assert_eq!(full, size, "a long listing opens at the browser height");
        assert!(
            sizing.max_height_px() > WIN_HEIGHT,
            "and may be dragged taller"
        );
        let mut waiting = Browser::open_root(listing_cue::NeverReady).expect("open");
        waiting.set_view_mode(MANAGER_VIEW_MODE);
        assert_eq!(
            manager_opening(&waiting, size, Scale::ONE, &theme),
            (size, opening_floor(Scale::ONE, &theme)),
            "a listing still being read opens at the browser height, unbounded"
        );
    }

    #[test]
    fn the_ground_is_glass_on_a_frosted_theme_and_solid_on_an_opaque_one() {
        for theme in [Theme::dark(), Theme::light()] {
            for drawn in [theme.clone(), theme.clone().frosted()] {
                let mut surface = Surface::new(200, 120).expect("surface");
                render_into(
                    &mut surface,
                    &grid(0),
                    Scale::ONE,
                    &drawn,
                    Rect::new(0, 0, 200, 120),
                    &crate::ManagerChrome::none(),
                    &mut NoArtwork,
                );
                let ground = ground_fill(&drawn, drawn.palette().surface, ChromeLayer::Ground);
                // Below the toolbar band, in the empty listing.
                assert_eq!(
                    surface.get(10, 110),
                    Some(Color::from(ground).premultiply()),
                    "{} on {:?}",
                    drawn.name(),
                    drawn.ground()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Symbolic links: two facts about one entry (`plans/SYMLINKS.md` S4)
// ---------------------------------------------------------------------------

/// The four link shapes side by side: to a folder, to a file, to a bundle,
/// and one that names nothing.
fn link_tree() -> (BTreeMap<String, Vec<u8>>, FixtureLinks) {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/".to_string(),
        encoded_stream(&[
            (b"Docs", FileKind::Symlink),
            (b"notes", FileKind::Symlink),
            (b"Editor", FileKind::Symlink),
            (b"gone", FileKind::Symlink),
            (b"plain.txt", FileKind::Regular),
        ]),
    );
    let links = FixtureLinks::default()
        .with("/Docs", "/Storage/docs", Some(FileKind::Directory))
        .with("/notes", "/Users/me/notes.txt", Some(FileKind::Regular))
        .with("/Editor", "/Apps/Editor.app", Some(FileKind::Directory))
        .with("/gone", "../removed", None);
    (dirs, links)
}

fn link_entries() -> Vec<Entry> {
    let (dirs, mut links) = link_tree();
    let stream = dirs.get("/").expect("root stream").clone();
    entries_from_dir_stream("/", &stream, &mut links).expect("valid stream")
}

#[test]
fn a_link_is_classified_by_its_target_and_still_shown_as_a_link() {
    let entries = link_entries();
    let kinds: Vec<EntryKind> = entries.iter().map(Entry::kind).collect();
    assert_eq!(
        kinds,
        vec![
            EntryKind::Link(LinkTarget::Directory),
            EntryKind::Link(LinkTarget::File),
            // The *target's* leaf name decides bundle-ness: a shortcut is
            // named for the application, not for `<Name>.app`.
            EntryKind::Link(LinkTarget::Bundle),
            EntryKind::Link(LinkTarget::Dangling),
            EntryKind::File,
        ]
    );
    // Every link is still a link — the fact the user must see.
    for entry in entries.iter().take(4) {
        assert!(entry.is_link(), "{} reads as a link", entry.name());
    }
    assert!(!entries[4].is_link());
}

#[test]
fn a_link_carries_the_target_it_stores_verbatim() {
    let entries = link_entries();
    assert_eq!(entries[0].target(), Some("/Storage/docs"));
    // A dangling link still shows the spelling that explains why.
    assert_eq!(entries[3].target(), Some("../removed"));
    // A plain file has no target at all.
    assert_eq!(entries[4].target(), None);
}

#[test]
fn a_link_is_never_directory_backed_however_it_resolves() {
    // The structural reading a management verb takes: removing a link must
    // unlink the *link*, never recurse into the tree it points at.
    let entries = link_entries();
    for entry in &entries {
        assert!(
            !entry.is_directory_backed(),
            "{} is a leaf on disk",
            entry.name()
        );
    }
    // A link to a directory is nevertheless one the browser descends.
    assert!(entries[0].is_directory());
    assert!(!entries[1].is_directory());
}

#[test]
fn a_link_to_a_bundle_reads_as_a_bundle_and_a_dangling_one_does_not() {
    let entries = link_entries();
    assert!(entries[2].is_bundle());
    assert!(!entries[3].is_bundle());
    // Resolution is the content reading; a dangling link resolves to nothing
    // rather than to a guessed kind.
    assert_eq!(entries[2].kind().resolved(), Some(EntryKind::Bundle));
    assert_eq!(entries[3].kind().resolved(), None);
}

#[test]
fn a_link_a_reader_cannot_describe_is_dangling_not_a_file() {
    // The fail-closed answer: with nothing describing the link, it is not
    // silently downgraded to a plain file.
    let (dirs, _) = link_tree();
    let stream = dirs.get("/").expect("root stream").clone();
    let entries = entries_from_dir_stream("/", &stream, &mut NoLinks).expect("valid stream");
    for entry in entries.iter().take(4) {
        assert_eq!(entry.kind(), EntryKind::Link(LinkTarget::Dangling));
        assert_eq!(entry.target(), None);
    }
}

#[test]
fn the_classifier_refuses_to_trust_a_target_that_is_itself_a_link() {
    // Resolution follows every hop, so a backing reporting a link as a
    // resolved target is not trusted to name anything.
    let kind = EntryKind::for_listing(
        FileKind::Symlink,
        "chain",
        Some(LinkResolution {
            kind: FileKind::Symlink,
            target_name: "next",
        }),
    );
    assert_eq!(kind, EntryKind::Link(LinkTarget::Dangling));
}

#[test]
fn a_link_sorts_and_classifies_as_what_it_names() {
    use crate::media::{media_for_entry, MediaType};
    use crate::sort::{sort_entries, SortDirection, SortKey, SortMode};
    let mut entries = link_entries();
    sort_entries(
        &mut entries,
        SortMode {
            key: SortKey::Kind,
            direction: SortDirection::Ascending,
        },
    );
    // Directories first, then bundles, then the leaves: a shortcut sorts
    // with what it names.
    let names: Vec<&str> = entries.iter().map(Entry::name).collect();
    assert_eq!(names[0], "Docs");
    assert_eq!(names[1], "Editor");
    // And its content type is its target's.
    let entries = link_entries();
    assert_eq!(media_for_entry(&entries[0], &[]), MediaType::InodeDirectory);
    assert_eq!(media_for_entry(&entries[2], &[]), MediaType::TairixApp);
}

#[test]
fn activating_a_link_acts_on_what_it_names() {
    let (dirs, links) = link_tree();
    let mut dirs = dirs;
    // The kernel resolves the link when a listing names it, so the path
    // spelled *through* the link is what a descent reads.
    dirs.insert("/Docs".to_string(), encoded_stream(&[]));
    let mut browser = Browser::open_root(tree_source_with_links(dirs, links)).expect("open");

    // A link to a directory descends *through* the link, so the browser's
    // location reads as the user navigated.
    select_named(&mut browser, "Docs");
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Ok(Activation::Descended)
    );
    assert_eq!(browser.components(), ["Docs".to_string()].as_slice());
    browser.go_up().expect("climb back");

    // A link to a file is opened through the link: the kernel follows the
    // final link on open, so the link's own path names the right bytes.
    select_named(&mut browser, "notes");
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Ok(Activation::OpenFile {
            path: "/notes".to_string()
        })
    );

    // A link to a bundle launches the **resolved** target, because the spawn
    // gate parses an entry point as `…/<Name>.app/Run` and a link named after
    // the program is not that shape.
    select_named(&mut browser, "Editor");
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Ok(Activation::LaunchBundle {
            path: "/Apps/Editor.app".to_string()
        })
    );

    // A link that names nothing has nothing to activate.
    select_named(&mut browser, "gone");
    assert_eq!(
        browser.activate_selected(BundleIntent::Launch),
        Err(crate::error::BrowseError::Source(Errno::NotFound))
    );
}

/// Select the entry called `name` in the browser's *sorted* listing, so a
/// test names what it means rather than a position the sort decides.
fn select_named<S: DirectorySource>(browser: &mut Browser<S>, name: &str) {
    let index = browser
        .entries()
        .iter()
        .position(|entry| entry.name() == name)
        .unwrap_or_else(|| panic!("the listing holds {name}"));
    browser.select(index).expect("select");
}

#[test]
fn a_relative_link_target_resolves_against_the_links_own_directory() {
    // Concatenation, never a lexical collapse: a `..` is left for the kernel
    // to resolve physically against the nodes the walk really passes through.
    assert_eq!(
        crate::entry::resolve_target("/Users/me", "notes.txt"),
        "/Users/me/notes.txt"
    );
    assert_eq!(
        crate::entry::resolve_target("/Users/me", "../shared/x"),
        "/Users/me/../shared/x"
    );
    // An absolute target names itself.
    assert_eq!(
        crate::entry::resolve_target("/Users/me", "/Apps/Editor.app"),
        "/Apps/Editor.app"
    );
    // A root parent is not doubled.
    assert_eq!(crate::entry::resolve_target("/", "x"), "/x");
}

#[test]
fn the_copy_walk_recreates_a_link_rather_than_streaming_its_bytes() {
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "shortcut"]),
        comps(&["Storage", "shortcut"]),
        CopyKind::Link,
    )])
    .expect("walk");
    match walk.next_action() {
        Some(CopyAction::CopyLink { source, dest }) => {
            assert_eq!(source, comps(&["Users", "shortcut"]).as_slice());
            assert_eq!(dest, comps(&["Storage", "shortcut"]).as_slice());
        }
        other => panic!("expected a CopyLink, got {other:?}"),
    }
    // The acknowledgement must match the step: a link is not a file.
    assert_eq!(walk.copied_file(), Err(CopyWalkError::OutOfStep));
    walk.copied_link().expect("link copied");
    assert!(walk.is_complete());
    assert_eq!(walk.copied(), 1);
}

#[test]
fn a_copied_tree_recreates_the_links_inside_it() {
    let mut tree: BTreeMap<String, Vec<(String, CopyKind)>> = BTreeMap::new();
    tree.insert(
        "/Users/dir".to_string(),
        vec![
            ("a.txt".to_string(), CopyKind::File),
            ("shortcut".to_string(), CopyKind::Link),
        ],
    );
    let mut walk = CopyWalk::from_items(vec![(
        comps(&["Users", "dir"]),
        comps(&["Storage", "dir"]),
        CopyKind::Directory,
    )])
    .expect("walk");
    let order = drive_copy(&mut walk, &tree);
    assert_eq!(
        order,
        vec![
            "/Storage/dir".to_string(),
            "/Storage/dir/a.txt".to_string(),
            "/Storage/dir/shortcut".to_string(),
        ]
    );
}

#[test]
fn open_with_is_offered_for_a_link_to_a_file_only() {
    use crate::chrome::{ContextCommand, ContextMenuModel};
    let (dirs, links) = link_tree();
    let mut browser = Browser::open_root(tree_source_with_links(dirs, links)).expect("open");
    for (name, offered) in [
        ("Docs", false),
        ("notes", true),
        ("Editor", false),
        ("gone", false),
        ("plain.txt", true),
    ] {
        select_named(&mut browser, name);
        let menu = ContextMenuModel::for_browser(&browser, false);
        assert_eq!(menu.is_enabled(ContextCommand::OpenWith), offered, "{name}");
    }
}

/// The chooser's action band is made of control plates, not text rows — the
/// defect that left every button in it an empty plate, because a plate laid
/// out on a text row pitch is shorter than the vertical budget its content
/// used to be charged.
#[test]
fn the_open_with_actions_are_control_plates_that_carry_their_labels() {
    use crate::open_with::{AppAssociation, OpenWithChooser};
    use crate::render::{
        draw_open_with_chooser, open_with_action_at, open_with_chooser_extent, OpenWithAction,
    };
    use tairix_icon::NoArtwork;
    use tairix_raster::{Color, Pixel};

    let theme = Theme::dark();
    let screen = Rect::new(0, 0, 800, 600);
    let apps = [AppAssociation::new(
        "Editor",
        "/Apps/Editor.app",
        alloc::vec![],
    )];
    let refs: alloc::vec::Vec<&AppAssociation> = apps.iter().collect();
    let chooser =
        OpenWithChooser::new(&refs, "/Users/u/notes.txt", "notes.txt").expect("one candidate");
    let (w, h) = open_with_chooser_extent(&chooser, Scale::ONE, &theme, screen);
    let vp = Rect::new(0, 0, w, h);

    // Both actions are reachable, and each spans at least a control plate's
    // height — sweeping for them rather than assuming where the band sits.
    let mut spans: alloc::collections::BTreeMap<&str, (i32, i32)> =
        alloc::collections::BTreeMap::new();
    for y in 0..i32::try_from(h).unwrap() {
        for x in 0..i32::try_from(w).unwrap() {
            let Some(action) =
                open_with_action_at(&chooser, vp, Scale::ONE, &theme, Point::new(x, y))
            else {
                continue;
            };
            let key = match action {
                OpenWithAction::Open => "Open",
                OpenWithAction::Cancel => "Cancel",
            };
            let span = spans.entry(key).or_insert((y, y));
            span.0 = span.0.min(y);
            span.1 = span.1.max(y);
        }
    }
    assert_eq!(spans.len(), 2, "both actions must be pressable");
    let plate = Scale::ONE.scale_length(theme.metrics().control_height);
    for (label, span) in &spans {
        assert_eq!(
            u32::try_from(span.1 - span.0 + 1).unwrap(),
            plate,
            "the {label} button is not a control plate"
        );
    }

    // And the labels actually ink: an empty plate is the reported defect.
    let mut surface = Surface::new(w, h).expect("surface");
    draw_open_with_chooser(
        &mut surface,
        &chooser,
        Scale::ONE,
        &theme,
        vp,
        &mut NoArtwork,
    );
    let band = spans.values().fold((i32::MAX, 0), |acc, span| {
        (acc.0.min(span.0), acc.1.max(span.1))
    });
    let label: Pixel = Color::from(theme.palette().on_surface).premultiply();
    let accent: Pixel = Color::from(theme.palette().on_accent).premultiply();
    let inked = (u32::try_from(band.0).unwrap()..=u32::try_from(band.1).unwrap())
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|(x, y)| matches!(surface.get(*x, *y), Some(p) if p == label || p == accent))
        .count();
    assert!(inked > 0, "the action band drew no label text at all");
}

/// The chooser marks which application a plain *Open* would have used, because
/// the chooser is reached to override exactly that choice.
#[test]
fn the_open_with_chooser_marks_the_default_candidate() {
    use crate::open_with::{AppAssociation, OpenWithChooser};
    use crate::render::{draw_open_with_chooser, open_with_chooser_extent};
    use tairix_icon::NoArtwork;

    let theme = Theme::dark();
    let screen = Rect::new(0, 0, 800, 600);
    let apps: alloc::vec::Vec<AppAssociation> = (0..3)
        .map(|n| AppAssociation::new(alloc::format!("App{n}"), "/Apps/A.app", alloc::vec![]))
        .collect();
    let refs: alloc::vec::Vec<&AppAssociation> = apps.iter().collect();
    let many = OpenWithChooser::new(&refs, "/f", "f").expect("three candidates");
    let one = OpenWithChooser::new(&refs[..1], "/f", "f").expect("one candidate");

    let paint = |chooser: &OpenWithChooser| {
        let (w, h) = open_with_chooser_extent(chooser, Scale::ONE, &theme, screen);
        let mut surface = Surface::new(w, h).expect("surface");
        draw_open_with_chooser(
            &mut surface,
            chooser,
            Scale::ONE,
            &theme,
            Rect::new(0, 0, w, h),
            &mut NoArtwork,
        );
        surface.pixels().to_vec()
    };
    // Only the first row carries the mark, so a chooser whose selection has
    // moved off it still says which one was the default.
    let mut moved = OpenWithChooser::new(&refs, "/f", "f").expect("three candidates");
    assert!(moved.select(2));
    assert_ne!(
        paint(&many),
        paint(&moved),
        "the selection is drawn, and it is not the default mark"
    );
    assert!(!paint(&one).is_empty());
}
