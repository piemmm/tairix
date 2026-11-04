//! Unit tests for the desktop's icon column ([`crate::desktop`]).
//!
//! Kept beside the module in its own file because `desktop.rs` is already
//! past the length at which a `#[cfg(test)]` block belongs in a sibling.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::desktop::{Appearance, Contrast, Density, Motion};
use tairix_abi::window_ipc::{AppMenuItemId, DocumentName, DragItems, DragPicture, IconAsset};
use tairix_abi::{Errno, FileId, Time64};
use tairix_browse::{
    AppAssociation, DirectorySource, Entry, EntryChange, EntryKind, GridView, LinkTarget, Listing,
};
use tairix_colour::Rgb;
use tairix_controls::{ActivityState, LabelHalo, MenuMark};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_proglib::{BundlePath, Catalog, DisplayName, EntryId, LibraryCategory, LibraryEntry};
use tairix_raster::Surface;
use tairix_theme::{CursorSetId, Theme};
use tairix_wallpaper::{
    Backdrop, CursorSize, DesktopSettings, IconFlow, IconSort, WallpaperChoice,
};
use tairix_wm::{Compositor, InputEvent, InputResponse, Key, NamedKey};

use crate::desktop::{
    BackdropWork, Desktop, DesktopAction, DesktopActivation, DesktopOutcome, GestureRefusal,
    LandingOffer, LaunchDocument, PinboardChange, SettingsEdit, DESKTOP_MARGIN,
};
use crate::launcher::Kept;
use crate::pinboard::{
    self, Arrangement, ArrangementEdit, ArrangementMemory, PinboardCommand, Place, PlaceKey,
    Placed, TidyOrder, MAX_PLACED, PLACE_UNITS,
};
use crate::shell::{DesktopShell, ShellOutcome, Stopped};
use tairix_controls::{ChainChild, ChainModel, ChainRow};

/// What a [`FakeDir`] answers with, and how often it has been asked.
///
/// `answer` is what the folder holds; `Err` models a folder the source
/// refuses (no permission, not there). Shared with the test so the folder
/// can change under a live desktop without the desktop having to hand its
/// source back out.
#[derive(Default)]
struct Folder {
    answer: Option<Result<Listing, Errno>>,
    listings: usize,
}

/// A directory seam over a shared [`Folder`].
struct FakeDir(Rc<RefCell<Folder>>);

impl DirectorySource for FakeDir {
    fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        let mut folder = self.0.borrow_mut();
        folder.listings = folder.listings.saturating_add(1);
        folder
            .answer
            .clone()
            .unwrap_or(Err(Errno::PermissionDenied))
    }
}

/// A shared folder holding `entries`.
fn holding(entries: Vec<Entry>) -> Rc<RefCell<Folder>> {
    Rc::new(RefCell::new(Folder {
        answer: Some(Ok(Listing::Ready(entries))),
        listings: 0,
    }))
}

/// How many times the folder has been listed.
fn listings(folder: &Rc<RefCell<Folder>>) -> usize {
    folder.borrow().listings
}

/// A worker that reads the folder off the loop, as the session's does.
#[derive(Default)]
struct Worker {
    /// Directory reads started.
    reads: usize,
    /// Whether a started read has not landed yet.
    reading: bool,
    /// A landed listing nobody has collected.
    landed: Option<Vec<Entry>>,
}

/// A deferred directory seam over a shared [`Worker`]: `refresh` always starts
/// a read, and `list` collects what landed or, with nothing under way, starts
/// one — exactly the listing desk's rules.
struct DeferredDir(Rc<RefCell<Worker>>);

impl DirectorySource for DeferredDir {
    fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        let mut worker = self.0.borrow_mut();
        if let Some(entries) = worker.landed.take() {
            return Ok(Listing::Ready(entries));
        }
        if !worker.reading {
            worker.reading = true;
            worker.reads += 1;
        }
        Ok(Listing::Pending)
    }

    fn refresh(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        let mut worker = self.0.borrow_mut();
        worker.landed = None;
        worker.reading = true;
        worker.reads += 1;
        Ok(Listing::Pending)
    }
}

/// The worker's read lands with `entries`.
fn land(worker: &Rc<RefCell<Worker>>, entries: Vec<Entry>) {
    let mut worker = worker.borrow_mut();
    worker.reading = false;
    worker.landed = Some(entries);
}

fn file(name: &str) -> Entry {
    Entry::new(name, EntryKind::File, 1, Time64::UNIX_EPOCH)
}

fn folder(name: &str) -> Entry {
    Entry::new(name, EntryKind::Directory, 0, Time64::UNIX_EPOCH)
}

fn bundle(name: &str) -> Entry {
    Entry::new(name, EntryKind::Bundle, 0, Time64::UNIX_EPOCH)
}

/// A shortcut: a link the listing classified as `resolves` and whose stored
/// spelling is `target`.
fn link(name: &str, resolves: LinkTarget, target: &str) -> Entry {
    Entry::new(name, EntryKind::Link(resolves), 0, Time64::UNIX_EPOCH).with_target(target)
}

/// The user's desktop folder, root-first.
fn home() -> Vec<String> {
    vec![
        "Users".to_string(),
        "ada".to_string(),
        "Desktop".to_string(),
    ]
}

/// A desktop over `folder`, already listed once.
fn desktop_over(folder: &Rc<RefCell<Folder>>) -> Desktop<FakeDir> {
    let mut desktop = Desktop::new(FakeDir(Rc::clone(folder)), home());
    desktop.relist();
    desktop
}

/// A desktop over `entries`, already listed once.
fn desktop_of(entries: Vec<Entry>) -> Desktop<FakeDir> {
    desktop_over(&holding(entries))
}

/// The default settings with the icon arrangement and sort order replaced.
fn arranged_by(icons: IconFlow, sort: IconSort) -> DesktopSettings {
    DesktopSettings {
        icons,
        sort,
        ..DesktopSettings::default()
    }
}

/// A desktop over `entries` under `settings`, already listed once.
fn desktop_with(entries: Vec<Entry>, settings: DesktopSettings) -> Desktop<FakeDir> {
    let mut desktop = Desktop::new(FakeDir(holding(entries)), home());
    let _ = desktop.apply_settings(settings);
    desktop.relist();
    desktop
}

fn theme() -> Theme {
    Theme::dark()
}

/// A work area the size of a modest screen with room for several columns.
fn work_area() -> Rect {
    Rect::new(0, 0, 800, 600)
}

fn layout_of<S: DirectorySource>(desktop: &Desktop<S>) -> GridView {
    desktop.layout(work_area(), Scale::ONE, &theme())
}

/// The cell the icon at `index` occupies: the whole of its repaint.
fn cell(layout: &GridView, index: usize) -> Rect {
    layout.shown_rect(0, index).expect("a shown icon")
}

/// The damage `gesture` reports, over a sink of its own so one step's damage
/// can never be read as another's.
fn damage_of(gesture: impl FnOnce(&mut Region)) -> Region {
    let mut damage = Region::new();
    gesture(&mut damage);
    damage
}

/// The centre of the icon at `index`, in screen coordinates.
fn centre_of(layout: &GridView, index: usize) -> Point {
    let bounds = cell(layout, index);
    Point::new(
        bounds.left() + i32::try_from(bounds.width / 2).unwrap_or(0),
        bounds.top() + i32::try_from(bounds.height / 2).unwrap_or(0),
    )
}

/// A point clear of every icon: inside the work area's margin, which no icon
/// reaches under either arrangement.
const EMPTY_DESKTOP: Point = Point::new(2, 2);

/// The catalog identifier `id`, validated.
fn entry_id(id: &str) -> EntryId {
    EntryId::new(id).expect("a valid identifier")
}

/// A catalog holding exactly one entry: `id`, shown as `name`, launching the
/// bundle at `bundle`.
fn catalog_of(id: &str, name: &str, bundle: &str) -> Catalog {
    let mut catalog = Catalog::new();
    catalog
        .insert(LibraryEntry::new(
            entry_id(id),
            DisplayName::new(name).expect("a valid display name"),
            BundlePath::new(bundle).expect("a valid bundle path"),
            LibraryCategory::Games,
            None,
        ))
        .expect("the catalog holds one entry");
    catalog
}

/// One "text/plain opens in the editor" association.
fn editor() -> Vec<AppAssociation> {
    vec![AppAssociation::new(
        "Edit",
        "/System/Applications/Edit.app",
        vec!["text/plain".to_string()],
    )]
}

// --- Listing --------------------------------------------------------------

#[test]
fn the_listing_is_the_shared_sort_order_of_the_folder() {
    let desktop = desktop_of(vec![file("zeta.txt"), folder("Work"), file("alpha.txt")]);
    let names: Vec<&str> = desktop.entries().iter().map(Entry::name).collect();
    // Directories first, then names in the shared order — the desktop adds
    // no ordering of its own.
    assert_eq!(names, vec!["Work", "alpha.txt", "zeta.txt"]);
}

#[test]
fn a_folder_that_will_not_list_shows_nothing_and_never_panics() {
    let mut desktop = Desktop::new(FakeDir(Rc::new(RefCell::new(Folder::default()))), home());
    assert!(
        !desktop.relist(),
        "an empty listing is no change from empty"
    );
    assert!(desktop.entries().is_empty());
    assert_eq!(desktop.selected(), None);
}

#[test]
fn a_relist_keeps_the_selection_on_the_same_named_icon() {
    let folder = holding(vec![file("b.txt"), file("c.txt")]);
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(desktop.entries()[1].name(), "c.txt");

    // A file appears ahead of it: the selection follows the name, not the
    // index, so the user's selection cannot silently jump to another icon.
    folder.borrow_mut().answer = Some(Ok(Listing::Ready(vec![
        file("a.txt"),
        file("b.txt"),
        file("c.txt"),
    ])));
    assert!(desktop.relist());
    assert_eq!(desktop.selected(), Some(2));
    assert_eq!(desktop.entries()[2].name(), "c.txt");
}

#[test]
fn a_relist_that_removes_the_selected_icon_selects_nothing() {
    let folder = holding(vec![file("a.txt"), file("b.txt")]);
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    folder.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("a.txt")])));
    assert!(desktop.relist());
    assert_eq!(desktop.selected(), None);
}

/// A wake shared with other work says nothing about the folder, so adopting
/// what a read delivered must never start another. Re-listing on every wake
/// did: each unrelated completion — a thumbnail, an icon — cost a directory
/// read and a second wake.
#[test]
fn resuming_adopts_the_owed_listing_and_never_starts_a_read() {
    let worker = Rc::new(RefCell::new(Worker::default()));
    let mut desktop = Desktop::new(DeferredDir(Rc::clone(&worker)), home());
    assert!(!desktop.relist(), "a deferred read changes nothing yet");
    assert!(!desktop.resume(), "nothing has landed yet");

    land(&worker, vec![file("a.txt")]);
    assert!(desktop.resume());
    assert_eq!(desktop.entries().len(), 1);

    for _ in 0..8 {
        assert!(!desktop.resume(), "a wake re-adopted a listing");
    }
    assert_eq!(worker.borrow().reads, 1, "a wake started a directory read");
}

/// A re-list is asked because the folder may have changed, so one asked while
/// an earlier read is still owed is a fresh read, not a second collection.
#[test]
fn a_relist_while_a_listing_is_owed_asks_afresh() {
    let worker = Rc::new(RefCell::new(Worker::default()));
    let mut desktop = Desktop::new(DeferredDir(Rc::clone(&worker)), home());
    desktop.relist();
    desktop.relist();
    assert_eq!(worker.borrow().reads, 2);

    land(&worker, vec![file("new.txt")]);
    assert!(desktop.resume());
    assert_eq!(desktop.entries()[0].name(), "new.txt");
}

#[test]
fn every_relist_the_session_asks_for_reads_the_folder() {
    let folder = holding(vec![file("a.txt")]);
    let mut desktop = desktop_over(&folder);
    desktop.relist();
    desktop.relist();
    assert_eq!(listings(&folder), 3);
}

#[test]
fn a_reported_change_merges_in_place_and_repaints_only_the_icons_it_moved() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("c.txt"), file("e.txt")]);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let damage = damage_of(|damage| {
        assert!(desktop.apply_changes(vec![EntryChange::Upsert(file("d.txt"))], layout_of, damage,));
    });
    let names: Vec<&str> = desktop.entries().iter().map(Entry::name).collect();
    assert_eq!(names, ["a.txt", "c.txt", "d.txt", "e.txt"]);
    assert_eq!(desktop.selected(), Some(1), "the selection stays on c.txt");
    let after = layout_of(&desktop);
    assert_eq!(
        damage.rects(),
        [cell(&after, 3)],
        "the new icon takes the next free cell, and no icon moves to make room"
    );
    assert_eq!(
        bounds(&desktop, &after, 3),
        cell(&after, 2),
        "e.txt stands where it stood"
    );
}

/// A merge that moves the icons carries the lit drop folder with its icon and
/// revises the icons, so a drag place held on an index is resolved again.
#[test]
fn a_reported_change_moves_the_drop_folder_with_its_icon_and_revises_the_icons() {
    let mut desktop = desktop_of(vec![folder("b"), folder("c")]);
    let layout = layout_of(&desktop);
    desktop.set_drop_target(Some(1), &layout, &mut Region::new());
    let before = desktop.revision();
    assert!(desktop.apply_changes(
        vec![EntryChange::Upsert(folder("a"))],
        layout_of,
        &mut Region::new(),
    ));
    assert_ne!(desktop.revision(), before);
    let after = layout_of(&desktop);
    let relit = damage_of(|damage| desktop.set_drop_target(Some(2), &after, damage));
    assert!(
        relit.rects().is_empty(),
        "the lit folder followed c: {relit:?}"
    );
}

/// An icon a reported change leaves standing where it was keeps a
/// double-click begun on it: the second press lands on the same icon.
#[test]
fn a_reported_change_keeps_a_double_click_on_an_icon_it_did_not_move() {
    let mut desktop = desktop_of(vec![folder("b"), folder("c")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    desktop.apply_changes(
        vec![EntryChange::Upsert(folder("a"))],
        layout_of,
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        acted.action,
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Desktop/b".to_string(),
        })),
        "b still stands under the pointer"
    );
}

#[test]
fn a_relist_moving_the_icons_ends_a_double_click_in_progress() {
    let folder = holding(vec![file("b.txt"), file("c.txt")]);
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    folder.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("c.txt")])));
    assert!(desktop.relist());
    let acted = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        acted.action, None,
        "b.txt went, and nothing stands where it stood"
    );
}

/// A fresh listing — a rescan, a folder gone and read again, a name the
/// desktop made — repaints the cells whose icon it changed and no more.
#[test]
fn a_relist_repaints_only_the_cells_its_listing_changed() {
    let folder = holding(vec![file("a.txt"), file("c.txt"), file("e.txt")]);
    let mut desktop = desktop_over(&folder);
    desktop.relist();
    folder.borrow_mut().answer = Some(Ok(Listing::Ready(vec![
        file("a.txt"),
        file("c.txt"),
        file("d.txt"),
        file("e.txt"),
    ])));
    let damage = damage_of(|damage| assert!(desktop.relist_into(layout_of, damage)));
    let after = layout_of(&desktop);
    assert_eq!(
        damage.rects(),
        [cell(&after, 3)],
        "the new icon takes the next free cell, and no icon moves to make room"
    );
    let unchanged = damage_of(|damage| assert!(!desktop.relist_into(layout_of, damage)));
    assert!(
        unchanged.is_empty(),
        "a listing that reads as shown costs nothing"
    );
}

/// A listing landing on a wake repaints the cells it filled, and a wake with
/// nothing owed repaints nothing.
#[test]
fn a_landed_listing_repaints_the_cells_it_filled() {
    let worker = Rc::new(RefCell::new(Worker::default()));
    let mut desktop = Desktop::new(DeferredDir(Rc::clone(&worker)), home());
    desktop.relist();
    let idle = damage_of(|damage| assert!(!desktop.resume_into(layout_of, damage)));
    assert!(idle.is_empty());
    land(&worker, vec![file("a.txt"), file("b.txt")]);
    let damage = damage_of(|damage| assert!(desktop.resume_into(layout_of, damage)));
    let after = layout_of(&desktop);
    assert_eq!(damage.rects(), [cell(&after, 0), cell(&after, 1)]);
}

/// A wake owing the desktop no listing compares nothing: every worker wake —
/// a landed decode, a published setting — passes through here.
#[test]
fn a_wake_owing_no_listing_costs_the_desktop_nothing() {
    let folder = holding(vec![file("a.txt")]);
    let mut desktop = desktop_over(&folder);
    let laid_out = Cell::new(0);
    let counted = |desk: &Desktop<FakeDir>| {
        laid_out.set(laid_out.get() + 1);
        layout_of(desk)
    };
    assert!(!desktop.resume_into(counted, &mut Region::new()));
    assert_eq!(
        laid_out.get(),
        0,
        "no listing was owed, so none was compared"
    );
    assert_eq!(listings(&folder), 1, "nor read");
}

/// A source whose listing follows the session's desktop folder, as a watched
/// one does.
struct Followed;

impl DirectorySource for Followed {
    fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        Ok(Listing::Ready(Vec::new()))
    }

    fn follows(&self, components: &[String]) -> bool {
        components == home().as_slice()
    }
}

/// The desktop asks its source about its own folder, and a source that only
/// reads follows nothing.
#[test]
fn the_desktop_follows_its_folder_only_where_its_source_does() {
    assert!(Desktop::new(Followed, home()).follows());
    assert!(!Desktop::new(Followed, vec!["Apps".to_string()]).follows());
    assert!(!desktop_of(vec![file("a.txt")]).follows());
}

/// A watched folder over a shared [`Folder`]: it reports its own changes.
struct WatchedDir(Rc<RefCell<Folder>>);

impl DirectorySource for WatchedDir {
    fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
        FakeDir(Rc::clone(&self.0)).list(components)
    }

    fn follows(&self, _components: &[String]) -> bool {
        true
    }
}

/// A name the session made in a folder no watch follows shows by reading that
/// folder again, as the mount table moving reads it again; a name made
/// elsewhere reads nothing.
#[test]
fn a_name_made_in_an_unfollowed_folder_is_shown_by_reading_it_again() {
    let shown = holding(vec![file("a.txt")]);
    let mut desktop = desktop_over(&shown);
    let read = listings(&shown);
    shown.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("a.txt"), file("saved.txt")])));
    assert!(
        !desktop.show_made(
            "/Users/ada/Documents/saved.txt",
            layout_of,
            &mut Region::new()
        ),
        "a name made elsewhere moves nothing"
    );
    assert_eq!(listings(&shown), read, "and reads nothing");
    let damage = damage_of(|damage| {
        assert!(desktop.show_made("/Users/ada/Desktop/saved.txt", layout_of, damage));
    });
    assert_eq!(listings(&shown), read + 1);
    assert!(desktop
        .entries()
        .iter()
        .any(|entry| entry.name() == "saved.txt"));
    assert!(!damage.is_empty(), "the cell the name took is repainted");

    shown.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("a.txt")])));
    assert!(desktop.relist_unfollowed(layout_of, &mut Region::new()));
    assert_eq!(desktop.entries().len(), 1);
}

/// A followed folder reports what was made in it, so neither a name made
/// there nor the mount table moving reads it again.
#[test]
fn a_followed_folder_is_not_read_again_for_a_change_it_reports() {
    let shown = holding(vec![file("a.txt")]);
    let mut desktop = Desktop::new(WatchedDir(Rc::clone(&shown)), home());
    desktop.relist();
    let read = listings(&shown);
    shown.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("a.txt"), file("saved.txt")])));
    assert!(!desktop.show_made(
        "/Users/ada/Desktop/saved.txt",
        layout_of,
        &mut Region::new()
    ));
    assert!(!desktop.relist_unfollowed(layout_of, &mut Region::new()));
    assert_eq!(
        listings(&shown),
        read,
        "the watch reports the change instead"
    );
}

#[test]
fn reading_back_a_folder_a_change_named_is_no_change() {
    let mut desktop = desktop_of(vec![folder("Work"), file("a.txt")]);
    assert!(!desktop.apply_changes(
        vec![EntryChange::Upsert(folder("Work"))],
        layout_of,
        &mut Region::new(),
    ));
    assert!(
        !desktop.relist(),
        "the listing reads as it shows, whatever was found out about the folder"
    );
}

#[test]
fn a_reported_removal_takes_its_selection_and_an_unchanged_entry_costs_nothing() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let damage = damage_of(|damage| {
        assert!(desktop.apply_changes(
            vec![EntryChange::Remove(String::from("b.txt"))],
            layout_of,
            damage,
        ));
    });
    assert_eq!(desktop.selected(), None);
    assert_eq!(
        damage.rects(),
        [cell(&layout, 1)],
        "the vacated cell is erased"
    );
    let unchanged = damage_of(|damage| {
        assert!(!desktop.apply_changes(
            vec![EntryChange::Upsert(file("a.txt"))],
            layout_of,
            damage,
        ));
    });
    assert!(unchanged.is_empty());
}

// --- Hover, selection, focus ---------------------------------------------

/// The ground around an icon's picture and name is the desktop's: a press or
/// a hover there is on no icon, though it lies inside the icon's cell.
#[test]
fn the_ground_in_an_icons_cell_is_the_desktops() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(desktop.selected(), Some(0));

    let corner = cell(&layout, 1);
    let ground = Point::new(corner.left() + 1, corner.top() + 1);
    desktop.pointer_moved(ground, &layout, Scale::ONE, &theme(), &mut Region::new());
    assert_eq!(desktop.hovered(), None);
    desktop.press(
        ground,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );
    assert_eq!(desktop.selected(), None);
}

#[test]
fn hover_follows_the_pointer_and_damages_only_the_cells_it_moves_between() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let mut damage = Region::new();

    desktop.pointer_moved(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        &mut damage,
    );
    assert_eq!(damage.rects(), [cell(&layout, 0)]);
    assert_eq!(desktop.hovered(), Some(0));

    damage.clear();
    desktop.pointer_moved(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        &mut damage,
    );
    assert!(damage.is_empty(), "the same icon is not a change");

    // Moving between icons costs both cells and nothing between them: the one
    // that lost the highlight and the one that took it.
    damage.clear();
    desktop.pointer_moved(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        &mut damage,
    );
    assert_eq!(damage.rects(), [cell(&layout, 0), cell(&layout, 1)]);
    assert_eq!(desktop.hovered(), Some(1));

    damage.clear();
    desktop.pointer_moved(EMPTY_DESKTOP, &layout, Scale::ONE, &theme(), &mut damage);
    assert_eq!(damage.rects(), [cell(&layout, 1)]);
    assert_eq!(desktop.hovered(), None);
}

/// The desktop never scrolls, so an icon the field has no free cell for is
/// left off it — neither drawn nor hit — rather than drawn cut or piled onto
/// another.
#[test]
fn an_icon_the_field_has_no_cell_for_is_left_off_it() {
    for icons in [IconFlow::Leading, IconFlow::Trailing] {
        let entries = (0..400)
            .map(|at| file(&alloc::format!("{at:03}.txt")))
            .collect();
        let mut desktop = desktop_with(entries, arranged_by(icons, IconSort::Name));
        let layout = layout_of(&desktop);
        let cells = layout.visible_range(0).len();
        assert!(cells < 400, "four hundred icons overflow the field");
        desktop.set_focused(true, &layout, &mut Region::new());
        for index in 0..400 {
            let shown = desktop.bounds_for_test(&layout, index);
            if index < cells {
                assert_eq!(shown, Some(cell(&layout, index)), "{icons:?}: {index}");
            } else {
                assert_eq!(shown, None, "{icons:?}: {index} has no cell");
            }
        }
    }
}

#[test]
fn leaving_the_desktop_clears_the_hover() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    desktop.pointer_moved(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );
    assert_eq!(
        damage_of(|damage| {
            desktop.pointer_left(&layout, damage);
        })
        .rects(),
        [cell(&layout, 0)]
    );
    assert_eq!(desktop.hovered(), None);
    assert!(
        damage_of(|damage| {
            desktop.pointer_left(&layout, damage);
        })
        .is_empty(),
        "leaving twice changes nothing"
    );
}

#[test]
fn a_press_selects_an_icon_and_a_press_on_empty_desktop_clears_it() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let mut damage = Region::new();

    desktop.press(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut damage,
    );
    assert_eq!(damage.rects(), [cell(&layout, 1)]);
    assert_eq!(desktop.selected(), Some(1));
    assert!(desktop.is_focused(), "a press moves focus to the desktop");

    // A selection that moves costs the icon it left and the icon it landed on.
    damage.clear();
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut damage,
    );
    assert_eq!(damage.rects(), [cell(&layout, 0), cell(&layout, 1)]);
    assert_eq!(desktop.selected(), Some(0));

    damage.clear();
    desktop.press(
        EMPTY_DESKTOP,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut damage,
    );
    assert_eq!(damage.rects(), [cell(&layout, 0)]);
    assert_eq!(desktop.selected(), None);

    damage.clear();
    desktop.press(
        EMPTY_DESKTOP,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut damage,
    );
    assert!(
        damage.is_empty(),
        "clearing an empty selection changes nothing"
    );
}

#[test]
fn focus_moves_the_ring_onto_the_selection_and_costs_nothing_without_one() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    assert!(!desktop.is_focused());

    // Only the selection wears the ring, so with nothing selected the click
    // that moves focus between the desktop and a window moves no pixel.
    assert!(
        damage_of(|damage| desktop.set_focused(true, &layout, damage)).is_empty(),
        "nothing is selected, so no ring appeared"
    );
    assert!(desktop.is_focused());
    assert!(
        damage_of(|damage| desktop.set_focused(false, &layout, damage)).is_empty(),
        "and none disappeared"
    );

    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert!(desktop.is_focused(), "a press claims the keyboard");
    assert_eq!(
        damage_of(|damage| desktop.set_focused(false, &layout, damage)).rects(),
        [cell(&layout, 0)],
        "the ring left the selected icon"
    );
    assert_eq!(
        damage_of(|damage| desktop.set_focused(true, &layout, damage)).rects(),
        [cell(&layout, 0)],
        "and came back to it"
    );
    assert!(
        damage_of(|damage| desktop.set_focused(true, &layout, damage)).is_empty(),
        "unchanged focus damages nothing"
    );
}

// --- Keyboard -------------------------------------------------------------

#[test]
fn the_arrows_move_the_selection_the_way_the_icons_run_under_both_arrangements() {
    // Columns grow rightward from the leading edge and leftward from the
    // trailing one, so which horizontal arrow runs later into the listing is a
    // property of the arrangement, never a constant.
    for (icons, later, earlier) in [
        (IconFlow::Leading, right(), left()),
        (IconFlow::Trailing, left(), right()),
    ] {
        let entries: Vec<Entry> = (0..12)
            .map(|n| file(&alloc::format!("f{n:02}.txt")))
            .collect();
        let mut desktop = desktop_with(entries, arranged_by(icons, IconSort::default()));
        let layout = layout_of(&desktop);
        let per_column = layout.cells_per_line();
        assert!(per_column >= 2, "the fixture needs a multi-icon column");
        desktop.set_focused(true, &layout, &mut Region::new());

        // With nothing selected the first arrow starts at the first icon.
        assert_eq!(
            damage_of(|damage| {
                desktop.key(down(), true, &layout, &[], damage);
            })
            .rects(),
            [cell(&layout, 0)]
        );
        assert_eq!(desktop.selected(), Some(0));

        desktop.key(down(), true, &layout, &[], &mut Region::new());
        assert_eq!(desktop.selected(), Some(1));
        desktop.key(up(), true, &layout, &[], &mut Region::new());
        assert_eq!(desktop.selected(), Some(0));

        // One whole column further into the listing, and back out again. The
        // two cells lie in different columns, so their order in the damage is
        // the arrangement's, not the listing's.
        let stepped = damage_of(|damage| {
            desktop.key(later, true, &layout, &[], damage);
        });
        assert_eq!(desktop.selected(), Some(per_column), "{icons:?} onward");
        assert!(
            stepped.intersects(cell(&layout, 0)) && stepped.intersects(cell(&layout, per_column)),
            "{icons:?} repaints the icon left and the icon reached"
        );
        assert_eq!(stepped.rects().len(), 2, "{icons:?} and nothing else");
        desktop.key(earlier, true, &layout, &[], &mut Region::new());
        assert_eq!(desktop.selected(), Some(0), "{icons:?} back");
    }
}

#[test]
fn the_selection_clamps_at_both_ends_of_the_listing() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    desktop.set_focused(true, &layout, &mut Region::new());
    desktop.key(down(), true, &layout, &[], &mut Region::new());
    assert!(damage_of(|damage| {
        desktop.key(up(), true, &layout, &[], damage);
    })
    .is_empty());
    assert_eq!(desktop.selected(), Some(0));

    desktop.key(down(), true, &layout, &[], &mut Region::new());
    assert_eq!(desktop.selected(), Some(1));
    assert!(damage_of(|damage| {
        desktop.key(down(), true, &layout, &[], damage);
    })
    .is_empty());
    assert_eq!(desktop.selected(), Some(1), "clamped at the last icon");
}

#[test]
fn keys_do_nothing_while_the_desktop_does_not_hold_the_keyboard() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let mut damage = Region::new();
    assert_eq!(
        desktop.key(down(), true, &layout, &[], &mut damage),
        DesktopOutcome::ignored()
    );
    assert!(damage.is_empty(), "and paints nothing either");
    assert_eq!(desktop.selected(), None);
}

#[test]
fn a_key_release_and_an_unknown_key_change_nothing() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    // One sink across all three: between them they must add no cell at all.
    let mut damage = Region::new();
    desktop.set_focused(true, &layout, &mut damage);
    assert_eq!(
        desktop.key(down(), false, &layout, &[], &mut damage),
        DesktopOutcome::ignored()
    );
    assert_eq!(
        desktop.key(Key::Char('x'), true, &layout, &[], &mut damage),
        DesktopOutcome::ignored()
    );
    assert_eq!(
        desktop.key(Key::Named(NamedKey::Tab), true, &layout, &[], &mut damage),
        DesktopOutcome::ignored()
    );
    assert!(damage.is_empty(), "and none of them paints anything");
}

#[test]
fn escape_clears_the_selection() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        damage_of(|damage| {
            desktop.key(escape(), true, &layout, &[], damage);
        })
        .rects(),
        [cell(&layout, 0)]
    );
    assert_eq!(desktop.selected(), None);
    let mut damage = Region::new();
    assert_eq!(
        desktop.key(escape(), true, &layout, &[], &mut damage),
        DesktopOutcome::ignored(),
        "nothing selected and no offer outstanding"
    );
    assert!(damage.is_empty(), "so there is nothing to repaint");
}

// --- Activation -----------------------------------------------------------

#[test]
fn double_clicking_a_folder_opens_the_file_manager_at_its_path() {
    let mut desktop = desktop_of(vec![folder("Work")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        acted.action,
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Desktop/Work".to_string(),
        }))
    );
}

#[test]
fn double_clicking_an_application_bundle_launches_its_run_binary() {
    let mut desktop = desktop_of(vec![bundle("Chess.app")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .press(
                at,
                &layout,
                Scale::ONE,
                &theme(),
                1,
                &[],
                &mut Region::new()
            )
            .action,
        Some(DesktopAction::Activate(DesktopActivation::Launch {
            run_path: "/Users/ada/Desktop/Chess.app/Run".to_string(),
            label: "Chess".to_string(),
            document: None,
        }))
    );
}

#[test]
fn double_clicking_a_file_launches_its_associated_application_with_the_file() {
    let mut desktop = desktop_of(vec![file("notes.txt")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .press(
                at,
                &layout,
                Scale::ONE,
                &theme(),
                1,
                &editor(),
                &mut Region::new()
            )
            .action,
        Some(DesktopAction::Activate(DesktopActivation::Launch {
            run_path: "/System/Applications/Edit.app/Run".to_string(),
            label: "Edit".to_string(),
            document: Some(LaunchDocument {
                path: "/Users/ada/Desktop/notes.txt".to_string(),
                edits: false,
            }),
        }))
    );
}

#[test]
fn double_clicking_a_disk_image_mounts_it_even_where_an_application_claims_it() {
    let mut desktop = desktop_of(vec![
        file("Lemmings.adf"),
        file("disc.iso"),
        file("work.hdf"),
        link("Elite.adl", LinkTarget::File, "/Users/ada/Home/Elite.adl"),
    ]);
    let layout = layout_of(&desktop);
    let burner = vec![AppAssociation::new(
        "Burn",
        "/System/Applications/Burn.app",
        vec!["application/x-cd-image".to_string()],
    )];
    let names: Vec<String> = desktop
        .entries()
        .iter()
        .map(|entry| entry.name().to_string())
        .collect();
    for ((index, name), now_ns) in names.iter().enumerate().zip((0u64..).step_by(2)) {
        let at = centre_of(&layout, index);
        let mut press = |now_ns| {
            desktop
                .press(
                    at,
                    &layout,
                    Scale::ONE,
                    &theme(),
                    now_ns,
                    &burner,
                    &mut Region::new(),
                )
                .action
        };
        let _ = press(now_ns);
        assert_eq!(
            press(now_ns + 1),
            Some(DesktopAction::Activate(DesktopActivation::MountImage {
                path: format!("/Users/ada/Desktop/{name}"),
            })),
            "{name} is mounted, a shortcut through the link"
        );
    }
}

#[test]
fn a_file_opened_with_an_editor_carries_the_editor_s_signed_claim_to_edit() {
    let mut desktop = desktop_of(vec![file("notes.txt")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    let writer: Vec<AppAssociation> = editor()
        .into_iter()
        .map(AppAssociation::writing_documents)
        .collect();
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &writer,
        &mut Region::new(),
    );
    let Some(DesktopAction::Activate(DesktopActivation::Launch { document, .. })) = desktop
        .press(
            at,
            &layout,
            Scale::ONE,
            &theme(),
            1,
            &writer,
            &mut Region::new(),
        )
        .action
    else {
        panic!("a double-click on a file launches its application");
    };
    assert_eq!(document.map(|document| document.edits), Some(true));
}

#[test]
fn a_file_no_application_opens_is_refused_with_its_reason_and_does_nothing_else() {
    let mut desktop = desktop_of(vec![file("mystery.qqq")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &editor(),
        &mut Region::new(),
    );
    assert_eq!(
        acted.action,
        Some(DesktopAction::Refuse(GestureRefusal::new(
            "No installed application opens \u{201c}mystery.qqq\u{201d}",
            "No program installed here claims files of this kind.",
        ))),
        "the refusal names the file"
    );
    assert_eq!(desktop.selected(), Some(0), "the icon stays selected");
}

#[test]
fn a_shortcut_is_named_in_the_desktop_folder_and_stores_its_target_verbatim() {
    let desktop = desktop_of(vec![]);
    let catalog = catalog_of(
        "os.tairix.chess",
        "Chess",
        "/System/Applications/games/chess.app",
    );
    assert_eq!(
        desktop.shortcut_to(&catalog, &entry_id("os.tairix.chess")),
        DesktopAction::CreateShortcut {
            link: "/Users/ada/Desktop/Chess".to_string(),
            target: "/System/Applications/games/chess.app".to_string(),
        },
        "the link is the entry's own name inside the desktop folder, and the \
         target is the bundle directory, untouched"
    );
}

#[test]
fn a_shortcut_whose_entry_left_the_catalog_is_refused_rather_than_guessed() {
    let desktop = desktop_of(vec![]);
    assert_eq!(
        desktop.shortcut_to(&Catalog::default(), &entry_id("os.tairix.chess")),
        DesktopAction::Refuse(GestureRefusal::new(
            "That program is no longer in the program library",
            "Library entry os.tairix.chess was removed or replaced since the menu opened.",
        ))
    );
}

#[test]
fn a_display_name_the_shared_rule_refuses_is_refused_with_its_reason() {
    // A display name is not automatically a file name: the library permits a
    // `/` and a `:` in one, and the filesystem does not.
    let desktop = desktop_of(vec![]);
    for (name, reason) in [
        (
            "Chess/Deluxe",
            "a file or directory name may not contain `/`",
        ),
        (
            "Chess:1",
            "path component contains a `:` (a reserved delimiter)",
        ),
        ("..", "`.` and `..` are not valid file or directory names"),
    ] {
        let catalog = catalog_of("os.tairix.chess", name, "/System/Applications/chess.app");
        assert_eq!(
            desktop.shortcut_to(&catalog, &entry_id("os.tairix.chess")),
            DesktopAction::Refuse(GestureRefusal::new(
                alloc::format!("\u{201c}{name}\u{201d} cannot name a shortcut"),
                alloc::format!("A shortcut is a file, and {reason}."),
            )),
            "the one shared name rule's own reason is what the desktop reports"
        );
    }
    // A program's name is its developer's, so what hides in it is shown.
    let catalog = catalog_of(
        "os.tairix.chess",
        "Chess\u{202e}/1",
        "/System/Applications/chess.app",
    );
    let DesktopAction::Refuse(refusal) =
        desktop.shortcut_to(&catalog, &entry_id("os.tairix.chess"))
    else {
        panic!("refused");
    };
    assert_eq!(
        refusal,
        GestureRefusal::new(
            "\u{201c}Chess\u{fffd}/1\u{201d} cannot name a shortcut",
            "A shortcut is a file, and a file or directory name may not contain `/`.",
        )
    );
}

#[test]
fn a_shortcut_is_asked_for_even_where_the_name_is_already_taken() {
    // The desktop never works a collision around: `fs_symlink` replaces no
    // name, so the authoritative answer is the kernel's `AlreadyExists` at
    // create time — not a second, differently-named shortcut chosen here off a
    // listing that may already be stale.
    let desktop = desktop_of(vec![file("Chess")]);
    let catalog = catalog_of("os.tairix.chess", "Chess", "/System/Applications/chess.app");
    assert_eq!(
        desktop.shortcut_to(&catalog, &entry_id("os.tairix.chess")),
        DesktopAction::CreateShortcut {
            link: "/Users/ada/Desktop/Chess".to_string(),
            target: "/System/Applications/chess.app".to_string(),
        }
    );
}

#[test]
fn double_clicking_a_shortcut_to_a_bundle_launches_the_resolved_target() {
    // The shortcut is named for the program, not for the bundle: bundle-ness
    // is read off the *target's* leaf, and the launch must name the bundle the
    // link resolves to, because the load gate judges the path it is handed.
    let mut desktop = desktop_of(vec![link(
        "Chess",
        LinkTarget::Bundle,
        "/System/Applications/chess.app",
    )]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .press(
                at,
                &layout,
                Scale::ONE,
                &theme(),
                1,
                &[],
                &mut Region::new()
            )
            .action,
        Some(DesktopAction::Activate(DesktopActivation::Launch {
            run_path: "/System/Applications/chess.app/Run".to_string(),
            label: "chess".to_string(),
            document: None,
        }))
    );
}

#[test]
fn double_clicking_a_shortcut_to_a_folder_or_a_file_acts_through_the_link() {
    let mut desktop = desktop_of(vec![
        link("Work", LinkTarget::Directory, "/Users/ada/Documents/Work"),
        link("notes.txt", LinkTarget::File, "/Users/ada/Documents/n.txt"),
    ]);
    let layout = layout_of(&desktop);
    let folder_at = centre_of(&layout, 0);
    desktop.press(
        folder_at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .press(
                folder_at,
                &layout,
                Scale::ONE,
                &theme(),
                1,
                &[],
                &mut Region::new()
            )
            .action,
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Desktop/Work".to_string(),
        })),
        "a directory the link names is opened through the link, which the kernel resolves"
    );
    let file_at = centre_of(&layout, 1);
    desktop.press(
        file_at,
        &layout,
        Scale::ONE,
        &theme(),
        2,
        &editor(),
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .press(
                file_at,
                &layout,
                Scale::ONE,
                &theme(),
                3,
                &editor(),
                &mut Region::new()
            )
            .action,
        Some(DesktopAction::Activate(DesktopActivation::Launch {
            run_path: "/System/Applications/Edit.app/Run".to_string(),
            label: "Edit".to_string(),
            document: Some(LaunchDocument {
                path: "/Users/ada/Desktop/notes.txt".to_string(),
                edits: false,
            }),
        })),
        "the association follows the target's kind and the document is opened through the link"
    );
}

#[test]
fn double_clicking_a_shortcut_whose_target_has_gone_is_refused_with_its_reason() {
    let mut desktop = desktop_of(vec![link(
        "Chess",
        LinkTarget::Dangling,
        "/System/Applications/chess.app",
    )]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &editor(),
        &mut Region::new(),
    );
    assert_eq!(
        acted.action,
        Some(DesktopAction::Refuse(GestureRefusal::new(
            "The shortcut \u{201c}Chess\u{201d} points at something that is not there",
            "What it named has been moved or deleted.",
        ))),
        "a dangling shortcut is never launched on the chance that it works"
    );
    assert_eq!(desktop.selected(), Some(0), "the icon stays selected");
}

#[test]
fn a_shortcut_that_names_nothing_at_all_is_refused_rather_than_opened() {
    // A listing that reported a link but no target describes nothing to act
    // on; the desktop says so instead of resolving an empty spelling.
    let mut desktop = desktop_of(vec![Entry::new(
        "Chess",
        EntryKind::Link(LinkTarget::Bundle),
        0,
        Time64::UNIX_EPOCH,
    )]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .press(
                at,
                &layout,
                Scale::ONE,
                &theme(),
                1,
                &[],
                &mut Region::new()
            )
            .action,
        Some(DesktopAction::Refuse(GestureRefusal::new(
            "The shortcut \u{201c}Chess\u{201d} names nothing",
            "It holds no target to open.",
        )))
    );
}

/// A refusal quotes an entry as its tile shows it, so what hides in a name
/// cannot make one entry's refusal read as another's.
#[test]
fn a_refused_entry_is_quoted_with_what_hides_in_its_name_shown() {
    let hidden = "a\u{202e}b";
    let double_clicked = |entry: Entry| {
        let mut desktop = desktop_of(vec![entry]);
        let layout = layout_of(&desktop);
        let at = centre_of(&layout, 0);
        for clicks in [0, 1] {
            let acted = desktop.press(
                at,
                &layout,
                Scale::ONE,
                &theme(),
                clicks,
                &editor(),
                &mut Region::new(),
            );
            if clicks == 1 {
                return acted.action;
            }
        }
        None
    };
    for (entry, headline) in [
        (
            file(hidden),
            "No installed application opens \u{201c}a\u{fffd}b\u{201d}",
        ),
        (
            link(
                hidden,
                LinkTarget::Dangling,
                "/System/Applications/chess.app",
            ),
            "The shortcut \u{201c}a\u{fffd}b\u{201d} points at something that is not there",
        ),
        (
            Entry::new(
                hidden,
                EntryKind::Link(LinkTarget::Bundle),
                0,
                Time64::UNIX_EPOCH,
            ),
            "The shortcut \u{201c}a\u{fffd}b\u{201d} names nothing",
        ),
    ] {
        let Some(DesktopAction::Refuse(refusal)) = double_clicked(entry) else {
            panic!("the entry is refused");
        };
        assert_eq!(refusal.headline(), headline);
    }
}

#[test]
fn enter_activates_the_selection_and_does_nothing_with_no_selection() {
    let mut desktop = desktop_of(vec![folder("Work")]);
    let layout = layout_of(&desktop);
    let mut damage = Region::new();
    desktop.set_focused(true, &layout, &mut damage);
    assert_eq!(
        desktop.key(enter(), true, &layout, &[], &mut damage),
        DesktopOutcome::ignored()
    );
    assert!(
        damage.is_empty(),
        "there was nothing to activate or repaint"
    );
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(
        desktop
            .key(enter(), true, &layout, &[], &mut Region::new())
            .action,
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Desktop/Work".to_string(),
        }))
    );
}

#[test]
fn two_slow_clicks_are_two_clicks_not_an_activation() {
    let mut desktop = desktop_of(vec![folder("Work")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let past = desktop.settings().double_click.saturating_total_nanos() + 1;
    let late = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        past,
        &[],
        &mut Region::new(),
    );
    assert_eq!(late.action, None);
}

/// The icons pair presses under the interval the user chose, not a fixed one.
#[test]
fn the_icons_pair_clicks_under_the_chosen_interval() {
    let mut desktop = desktop_of(vec![folder("Work")]);
    let mut settings = desktop.settings().clone();
    settings.double_click = tairix_abi::time::Duration64::from_millis(1_500);
    let _ = desktop.apply_settings(settings);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let slow = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1_000_000_000,
        &[],
        &mut Region::new(),
    );
    assert!(
        slow.action.is_some(),
        "a second press a second later is within a 1.5 s interval"
    );
}

// --- Painting -------------------------------------------------------------

#[test]
fn every_icon_the_column_shows_is_painted_even_with_no_artwork_at_all() {
    let mut desktop = desktop_of(vec![folder("Work"), file("notes.txt"), bundle("Chess.app")]);
    let layout = layout_of(&desktop);
    let theme = theme();
    let mut surface = Surface::new(800, 600).expect("a screen-sized layer");
    desktop.set_focused(true, &layout, &mut Region::new());
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme,
        0,
        &[],
        &mut Region::new(),
    );
    desktop.pointer_moved(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme,
        &mut Region::new(),
    );

    desktop.render(
        &mut surface,
        &layout,
        Scale::ONE,
        &theme,
        &mut NoArtwork,
        work_area(),
        None,
    );

    // With no artwork store at all every tile still draws its built-in
    // glyph, so no icon slot can come out blank.
    for index in 0..3 {
        assert!(
            painted(&surface, cell(&layout, index)),
            "icon {index} drew nothing at all"
        );
    }
}

#[test]
fn an_empty_desktop_paints_nothing_and_leaves_the_wallpaper_showing() {
    let desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let theme = theme();
    let mut surface = Surface::new(800, 600).expect("a screen-sized layer");
    desktop.render(
        &mut surface,
        &layout,
        Scale::ONE,
        &theme,
        &mut NoArtwork,
        work_area(),
        None,
    );
    assert!(
        !painted(&surface, work_area()),
        "an empty folder leaves the layer fully transparent"
    );
}

#[test]
fn a_render_clipped_to_one_cell_leaves_every_other_icon_untouched() {
    let desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let theme = theme();
    let mut surface = Surface::new(800, 600).expect("a screen-sized layer");

    desktop.render(
        &mut surface,
        &layout,
        Scale::ONE,
        &theme,
        &mut NoArtwork,
        cell(&layout, 1),
        None,
    );

    // A tile draws strictly inside its own cell, which is what lets a moved
    // highlight cost one cell instead of the whole layer.
    assert!(
        painted(&surface, cell(&layout, 1)),
        "the cell asked for drew"
    );
    assert!(
        !painted(&surface, cell(&layout, 0)),
        "a cell outside the repainted area is left exactly as it was"
    );
}

/// `desktop` painted whole over `ground` in the light theme, whose own ink is
/// the dark one a dark picture would swallow, with the layout it was laid out
/// in.
fn painted_over<S: DirectorySource>(
    desktop: &Desktop<S>,
    ground: Option<tairix_raster::Color>,
) -> (Surface, GridView) {
    let theme = Theme::light();
    let layout = desktop.layout(work_area(), Scale::ONE, &theme);
    let mut surface = Surface::new(800, 600).expect("a screen-sized layer");
    desktop.render(
        &mut surface,
        &layout,
        Scale::ONE,
        &theme,
        &mut NoArtwork,
        work_area(),
        ground,
    );
    (surface, layout)
}

/// Every pixel of `surface` inside `area`.
fn pixels_in(surface: &Surface, area: Rect) -> Vec<tairix_raster::Pixel> {
    let left = u32::try_from(area.left()).unwrap_or(0);
    let top = u32::try_from(area.top()).unwrap_or(0);
    (top..top.saturating_add(area.height))
        .flat_map(|y| (left..left.saturating_add(area.width)).map(move |x| (x, y)))
        .filter_map(|(x, y)| surface.get(x, y))
        .collect()
}

/// The opaque pixels of the name the icon at `index` draws: its glyphs' ink,
/// which neither a halo nor a translucent plate under it is.
fn name_ink(
    surface: &Surface,
    layout: &GridView,
    index: usize,
    theme: &Theme,
) -> Vec<tairix_raster::Pixel> {
    let band = tairix_browse::render::Tiles::Large
        .layout()
        .label_rect(cell(layout, index), Scale::ONE, theme)
        .expect("the icon draws a name");
    pixels_in(surface, band)
        .into_iter()
        .filter(|pixel| pixel.a == u8::MAX)
        .collect()
}

/// Whether there is `ink` and all of it reads light.
fn reads_light(ink: &[tairix_raster::Pixel]) -> bool {
    !ink.is_empty() && ink.iter().all(|pixel| pixel.luma() >= 0xe0)
}

/// Whether there is `ink` and all of it reads dark.
fn reads_dark(ink: &[tairix_raster::Pixel]) -> bool {
    !ink.is_empty() && ink.iter().all(|pixel| pixel.luma() <= 0x40)
}

/// A resting name stands straight on the backdrop, and the desktop cannot know
/// the light of a picture under it, so the name is light over a dark halo
/// whatever the theme's own ink.
#[test]
fn a_resting_name_over_a_picture_reads_light_over_a_dark_halo() {
    let desktop = desktop_of(vec![file("notes.txt")]);
    let theme = Theme::light();
    let (pictured, layout) = painted_over(&desktop, None);
    let (flat, _) = painted_over(&desktop, Some(tairix_raster::Color::rgb(0x10, 0x14, 0x18)));
    assert!(
        reads_light(&name_ink(&pictured, &layout, 0, &theme)),
        "the name kept the theme's dark ink"
    );
    assert!(reads_light(&name_ink(&flat, &layout, 0, &theme)));
    let inked = |surface: &Surface| {
        pixels_in(surface, cell(&layout, 0))
            .iter()
            .filter(|pixel| pixel.a != 0)
            .count()
    };
    assert!(
        inked(&pictured) > inked(&flat),
        "no halo parts the name from the picture"
    );
}

/// A flat backdrop is known, so a name on it takes whichever ink stands
/// further from it.
#[test]
fn a_name_on_a_flat_backdrop_takes_the_ink_its_luminance_favours() {
    let desktop = desktop_of(vec![file("notes.txt")]);
    let theme = Theme::light();
    let (pale, layout) = painted_over(&desktop, Some(tairix_raster::Color::rgb(0xf0, 0xf0, 0xf0)));
    assert!(reads_dark(&name_ink(&pale, &layout, 0, &theme)));
    let (deep, _) = painted_over(&desktop, Some(tairix_raster::Color::rgb(0x10, 0x20, 0x40)));
    assert!(reads_light(&name_ink(&deep, &layout, 0, &theme)));
    let (grey, _) = painted_over(&desktop, Some(tairix_raster::Color::rgb(0x80, 0x80, 0x80)));
    assert!(
        reads_dark(&name_ink(&grey, &layout, 0, &theme)),
        "a mid grey contrasts more with the dark ink"
    );
}

/// A selected name sits on its plate rather than on the backdrop, so it keeps
/// the theme's ink while a resting one beside it takes the backdrop's.
#[test]
fn a_selected_name_keeps_the_theme_ink_on_its_plate() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let theme = Theme::light();
    let layout = desktop.layout(work_area(), Scale::ONE, &theme);
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme,
        0,
        &[],
        &mut Region::new(),
    );
    assert_eq!(desktop.selected(), Some(0));
    let (surface, layout) = painted_over(&desktop, None);
    let theme_ink = tairix_raster::Color::from(theme.palette().on_surface).premultiply();
    let selected = name_ink(&surface, &layout, 0, &theme);
    assert!(
        !selected.is_empty() && selected.iter().all(|pixel| *pixel == theme_ink),
        "the selected name lost the theme's ink"
    );
    assert!(
        reads_light(&name_ink(&surface, &layout, 1, &theme)),
        "the resting name beside it kept the theme's ink"
    );
}

/// Whether anything at all was drawn inside `area` (any non-transparent
/// pixel).
fn painted(surface: &Surface, area: Rect) -> bool {
    let right = u32::try_from(area.right())
        .unwrap_or(0)
        .min(surface.width());
    let bottom = u32::try_from(area.bottom())
        .unwrap_or(0)
        .min(surface.height());
    let left = u32::try_from(area.left()).unwrap_or(0);
    let top = u32::try_from(area.top()).unwrap_or(0);
    for y in top..bottom {
        for x in left..right {
            if surface.get(x, y).is_some_and(|pixel| pixel.a != 0) {
                return true;
            }
        }
    }
    false
}

const fn down() -> Key {
    Key::Named(NamedKey::Down)
}

const fn up() -> Key {
    Key::Named(NamedKey::Up)
}

const fn left() -> Key {
    Key::Named(NamedKey::Left)
}

const fn right() -> Key {
    Key::Named(NamedKey::Right)
}

const fn enter() -> Key {
    Key::Named(NamedKey::Enter)
}

const fn escape() -> Key {
    Key::Named(NamedKey::Escape)
}

// --- The pinboard settings ------------------------------------------------

#[test]
fn each_arrangement_lays_the_column_out_at_its_own_corner_and_hit_tests_there() {
    let area = work_area();
    let margin = i32::try_from(Scale::ONE.scale_length(DESKTOP_MARGIN)).unwrap_or(0);
    let mut cells = Vec::new();
    for icons in [IconFlow::Leading, IconFlow::Trailing] {
        let mut desktop =
            desktop_with(vec![file("a.txt")], arranged_by(icons, IconSort::default()));
        let layout = layout_of(&desktop);
        let bounds = cell(&layout, 0);
        assert_eq!(
            bounds.top(),
            area.top() + margin,
            "{icons:?} starts at the top"
        );
        // The icon the user can see is the icon a press lands on, whichever
        // corner the column grew from.
        desktop.press(
            centre_of(&layout, 0),
            &layout,
            Scale::ONE,
            &theme(),
            0,
            &[],
            &mut Region::new(),
        );
        assert_eq!(desktop.selected(), Some(0), "{icons:?} hit-test");
        cells.push(bounds);
    }
    let (leading, trailing) = (cells[0], cells[1]);
    assert_eq!(
        leading.left(),
        area.left() + margin,
        "the leading column starts at the margin"
    );
    assert!(
        trailing.right() <= area.right() - margin,
        "the trailing column stays inside the margin"
    );
    assert!(
        trailing.left() > leading.left() + i32::try_from(area.width / 2).unwrap_or(0),
        "the two arrangements hug opposite edges"
    );
}

#[test]
fn changing_the_arrangement_relays_the_icons_out_without_relisting() {
    let folder = holding(vec![file("a.txt")]);
    let mut desktop = desktop_over(&folder);
    let before = cell(&layout_of(&desktop), 0);

    let change = desktop
        .apply_settings(arranged_by(IconFlow::Trailing, IconSort::default()))
        .expect("the arrangement changed");
    assert!(change.backdrop.relayout);
    assert!(!change.backdrop.wallpaper);
    assert_eq!(listings(&folder), 1, "an arrangement is not a listing");

    let after = cell(&layout_of(&desktop), 0);
    assert_ne!(before.left(), after.left(), "the column moved edge");
}

/// A new region or zone is the clocks' to read again, and moves no pixel of
/// the desktop layer.
#[test]
fn a_civil_change_owes_the_clocks_and_nothing_else() {
    for (zone, region) in [(Some("America/New_York"), None), (None, Some("US"))] {
        let mut desktop = desktop_of(vec![file("a.txt")]);
        let mut settings = desktop.settings().clone();
        settings.time_zone = zone.and_then(tairix_wallpaper::ZoneName::new);
        settings.region = region.and_then(tairix_wallpaper::RegionCode::new);
        assert_eq!(
            desktop.apply_settings(settings),
            Some(PinboardChange {
                clock: true,
                ..PinboardChange::default()
            })
        );
    }
}

#[test]
fn each_sort_order_lists_the_folder_in_the_shared_order_it_names() {
    let entries = vec![
        Entry::new("b.txt", EntryKind::File, 300, Time64::from_secs(30)),
        Entry::new("a.txt", EntryKind::File, 900, Time64::from_secs(20)),
        Entry::new("c.txt", EntryKind::File, 100, Time64::from_secs(10)),
    ];
    for (sort, expected) in [
        (IconSort::Name, ["a.txt", "b.txt", "c.txt"]),
        (IconSort::Size, ["c.txt", "b.txt", "a.txt"]),
        (IconSort::Date, ["c.txt", "a.txt", "b.txt"]),
    ] {
        let desktop = desktop_with(entries.clone(), arranged_by(IconFlow::default(), sort));
        let names: Vec<&str> = desktop.entries().iter().map(Entry::name).collect();
        assert_eq!(names, expected.to_vec(), "{sort:?} order");
    }
}

#[test]
fn the_kind_order_groups_folders_then_bundles_then_files() {
    let mixed = vec![file("b.txt"), bundle("Chess.app"), folder("Work")];
    let by_name = desktop_of(mixed.clone());
    let names: Vec<&str> = by_name.entries().iter().map(Entry::name).collect();
    assert_eq!(
        names,
        vec!["Work", "b.txt", "Chess.app"],
        "the default order"
    );

    let by_kind = desktop_with(mixed, arranged_by(IconFlow::default(), IconSort::Kind));
    let names: Vec<&str> = by_kind.entries().iter().map(Entry::name).collect();
    assert_eq!(names, vec!["Work", "Chess.app", "b.txt"]);
}

#[test]
fn adopting_settings_reports_only_the_work_the_edit_implies() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    assert_eq!(
        desktop.apply_settings(DesktopSettings::default()),
        None,
        "the settings already in force cost nothing at all"
    );

    let sorted = desktop
        .apply_settings(arranged_by(IconFlow::default(), IconSort::Size))
        .expect("the order changed");
    assert_eq!(
        sorted.backdrop,
        BackdropWork::default(),
        "a new order is the desktop's own work"
    );
    assert!(sorted.layer, "a new order moves the icons");

    let base = desktop.settings().clone();
    let recoloured = desktop.apply_settings(DesktopSettings {
        backdrop: Backdrop::Colour(Rgb::new(1, 2, 3)),
        ..base
    });
    assert_eq!(
        recoloured,
        Some(PinboardChange {
            layer: true,
            ..PinboardChange::default()
        }),
        "a new backdrop colour costs the layer's repaint alone"
    );

    let base = desktop.settings().clone();
    let papered = desktop
        .apply_settings(DesktopSettings {
            wallpaper: WallpaperChoice::None,
            ..base
        })
        .expect("the wallpaper changed");
    assert!(papered.backdrop.wallpaper);
    assert!(!papered.backdrop.relayout);
    assert!(
        !papered.layer,
        "the new ground repaints the layer when it lands"
    );
}

/// Most of the settings document moves no pixel of the desktop layer, and
/// repainting the bottom layer recomposites the screen and re-blurs every
/// frosted surface over it.
#[test]
fn a_setting_the_layer_does_not_show_repaints_none_of_it() {
    let edits: [fn(&mut DesktopSettings); 2] = [
        |s| s.cursor_locate = !s.cursor_locate,
        |s| s.cursor_shake = !s.cursor_shake,
    ];
    for edit in edits {
        let mut desktop = desktop_of(vec![file("a.txt")]);
        let mut wanted = desktop.settings().clone();
        edit(&mut wanted);
        assert_eq!(
            desktop.apply_settings(wanted),
            Some(PinboardChange::default()),
            "a pointer aid is adopted elsewhere"
        );
    }
}

#[test]
fn a_new_sort_order_reorders_the_icons_shown_without_reading_the_folder() {
    let folder = holding(vec![
        Entry::new("b.txt", EntryKind::File, 300, Time64::from_secs(30)),
        Entry::new("a.txt", EntryKind::File, 900, Time64::from_secs(20)),
        Entry::new("c.txt", EntryKind::File, 100, Time64::from_secs(10)),
    ]);
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    desktop.pointer_moved(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );

    desktop
        .apply_settings(arranged_by(IconFlow::default(), IconSort::Size))
        .expect("the order changed");
    let names: Vec<&str> = desktop.entries().iter().map(Entry::name).collect();
    assert_eq!(names, ["c.txt", "b.txt", "a.txt"]);
    assert_eq!(listings(&folder), 1, "the icons shown are the folder's");
    assert_eq!(desktop.selected(), Some(2), "the selection stays on a.txt");
    assert_eq!(
        desktop.hovered(),
        None,
        "the cell under the pointer changed"
    );
    let acted = desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );
    assert_eq!(acted.action, None, "c.txt was never pressed before");
}

// --- The context-menu gesture and its commands ----------------------------

#[test]
fn a_secondary_press_on_an_icon_selects_it_and_asks_for_the_menu() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 1);

    let mut damage = Region::new();
    assert!(
        desktop.context_press(at, &layout, Scale::ONE, &theme(), &mut damage),
        "the press landed on an icon, so the menu offers `Open`"
    );
    assert_eq!(damage.rects(), [cell(&layout, 1)]);
    assert_eq!(
        desktop.selected(),
        Some(1),
        "the menu acts on what was pointed at"
    );

    damage.clear();
    assert!(desktop.context_press(at, &layout, Scale::ONE, &theme(), &mut damage));
    assert!(damage.is_empty(), "the selection did not move");
}

#[test]
fn a_secondary_press_on_the_backdrop_leaves_the_selection_untouched() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 0),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );

    let mut damage = Region::new();
    assert!(
        !desktop.context_press(EMPTY_DESKTOP, &layout, Scale::ONE, &theme(), &mut damage),
        "a press on empty backdrop has nothing to open"
    );
    assert!(damage.is_empty(), "the backdrop menu moves no highlight");
    assert_eq!(
        desktop.selected(),
        Some(0),
        "asking for the menu is not a way to lose a selection"
    );
}

#[test]
fn the_menus_open_command_resolves_exactly_as_a_double_click_does() {
    let mut desktop = desktop_of(vec![folder("Work")]);
    let layout = layout_of(&desktop);
    let at = centre_of(&layout, 0);
    desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let clicked = desktop.press(
        at,
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );

    assert_eq!(
        desktop.command(PinboardCommand::Open, &layout, &[]).action,
        clicked.action,
        "one definition of what opening an icon means"
    );
    assert_eq!(
        clicked.action,
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Desktop/Work".to_string(),
        }))
    );
}

#[test]
fn open_with_nothing_selected_asks_for_nothing() {
    let mut desktop = desktop_of(vec![folder("Work")]);
    let layout = layout_of(&desktop);
    assert_eq!(
        desktop.command(PinboardCommand::Open, &layout, &[]),
        DesktopOutcome::ignored()
    );
}

#[test]
fn a_new_folder_is_named_through_the_shared_naming_over_the_listing() {
    let mut desktop = desktop_of(vec![folder("New Folder")]);
    let layout = layout_of(&desktop);
    assert_eq!(
        desktop
            .command(PinboardCommand::NewFolder, &layout, &[])
            .action,
        Some(DesktopAction::CreateFolder {
            path: "/Users/ada/Desktop/New Folder 2".to_string(),
        })
    );
}

#[test]
fn a_sort_or_arrangement_row_asks_the_embedder_to_adopt_the_edit() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    assert_eq!(
        desktop
            .command(PinboardCommand::SortBy(IconSort::Size), &layout, &[])
            .action,
        Some(DesktopAction::EditSettings(SettingsEdit::Sort(
            IconSort::Size
        )))
    );
    assert_eq!(
        desktop.settings().sort,
        IconSort::default(),
        "the model does not adopt settings behind the embedder's back"
    );
    assert_eq!(
        desktop
            .command(
                PinboardCommand::ArrangeFrom(IconFlow::Trailing),
                &layout,
                &[]
            )
            .action,
        Some(DesktopAction::EditSettings(SettingsEdit::Icons(
            IconFlow::Trailing
        )))
    );
    assert_eq!(
        desktop.command(PinboardCommand::SortBy(IconSort::Name), &layout, &[]),
        DesktopOutcome::ignored(),
        "the order already in force is no edit at all"
    );
}

#[test]
fn refresh_relists_now_and_the_remaining_rows_name_their_own_action() {
    let folder = holding(vec![file("a.txt")]);
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    folder.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("a.txt"), file("b.txt")])));

    // A re-list reports no icon: the icons themselves moved, so the caller
    // repaints the whole layer instead of any icon of the listing it replaced.
    assert_eq!(
        desktop.command(PinboardCommand::Refresh, &layout, &[]),
        DesktopOutcome {
            rearranged: true,
            action: None,
        }
    );
    assert_eq!(listings(&folder), 2);
    assert_eq!(desktop.entries().len(), 2);
    assert_eq!(
        desktop.command(PinboardCommand::Refresh, &layout, &[]),
        DesktopOutcome::ignored(),
        "a refresh that finds nothing changed costs nothing"
    );

    assert_eq!(
        desktop
            .command(PinboardCommand::OpenDesktopFolder, &layout, &[])
            .action,
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Desktop".to_string(),
        }))
    );
    assert_eq!(
        desktop
            .command(PinboardCommand::ChangeBackground, &layout, &[])
            .action,
        Some(DesktopAction::ChangeBackground)
    );
    assert_eq!(desktop.folder_path(), "/Users/ada/Desktop");
}

// --- The backdrop menu's row model ----------------------------------------

/// The rows the menu offers, in order, as the labels a user reads.
fn labels(model: &ChainModel) -> Vec<&str> {
    model.rows().iter().map(|row| row.drawn().label()).collect()
}

#[test]
fn the_menu_offers_exactly_the_closed_row_set_with_the_settings_in_force_marked() {
    let model = pinboard::model(true, &arranged_by(IconFlow::Trailing, IconSort::Size));

    assert_eq!(
        labels(&model),
        vec![
            "Open",
            "New Folder",
            "Sort by Name",
            "Sort by Kind",
            "Sort by Size",
            "Sort by Date",
            "Arrange from the Left",
            "Arrange from the Right",
            "Tidy Up",
            "By Name",
            "By Date",
            "By Size",
            "Refresh",
            "Open Desktop Folder",
            "Change Background…",
        ]
    );

    // The sort orders and the arrangements are each a group of alternatives,
    // so the one in force is drawn as that group's chosen member — never as
    // finished work, which is what an activity bead means.
    let marked: Vec<&str> = model
        .rows()
        .iter()
        .filter(|row| row.drawn().mark() == MenuMark::Radio)
        .map(|row| row.drawn().label())
        .collect();
    assert_eq!(marked, vec!["Sort by Size", "Arrange from the Right"]);
    for row in model.rows() {
        let item = row.drawn();
        if item.mark() != MenuMark::Radio {
            assert!(item.state().enabled, "{:?} is offered", item.label());
            assert_eq!(item.state().activity, ActivityState::Idle);
            continue;
        }
        assert!(
            !item.state().enabled,
            "the setting in force is not a command"
        );
        assert!(
            row.tip().is_some(),
            "a marked row says why it is not offered, as a tip rather than a caption"
        );
        assert_eq!(
            item.state().activity,
            ActivityState::Idle,
            "an appearance row states no progress"
        );
    }
}

#[test]
fn every_row_names_the_command_at_its_own_index() {
    let model = pinboard::model(true, &DesktopSettings::default());
    let commands: Vec<PinboardCommand> = model
        .rows()
        .iter()
        .filter_map(ChainRow::id)
        .map(|id| PinboardCommand::from_item(id).expect("a declared row"))
        .collect();
    assert_eq!(commands, PinboardCommand::ALL.to_vec());
    assert_eq!(
        PinboardCommand::from_item(
            AppMenuItemId::new(u16::try_from(PinboardCommand::ALL.len() + 1).expect("small"))
                .expect("a non-zero row id")
        ),
        None,
        "an id the menu does not have names no command"
    );
}

/// The rows a gesture leaves out must not shift what the rows around them
/// mean: an id is a command's own position, never a row's.
#[test]
fn a_menu_opened_on_the_backdrop_offers_no_open_row_and_shifts_no_id() {
    let bare = pinboard::model(false, &DesktopSettings::default());
    assert_eq!(
        bare.rows().first().map(|row| row.drawn().label()),
        Some("New Folder")
    );
    assert!(
        !bare.rows()[0].drawn().is_group_break(),
        "the first row divides nothing"
    );
    assert_eq!(
        bare.rows().iter().filter_map(ChainRow::id).count(),
        PinboardCommand::ALL.len() - 1,
        "only `Open` is left out"
    );

    let over_icon = pinboard::model(true, &DesktopSettings::default());
    assert!(
        over_icon.rows()[1].drawn().is_group_break(),
        "with Open above it, New Folder starts its own group"
    );

    // `New Folder` is `ALL` position 1, so its id is 2 whichever gesture built
    // the plate — the row it sits at moved, its command did not.
    let id = AppMenuItemId::new(2).expect("a non-zero row id");
    assert_eq!(
        PinboardCommand::from_item(id),
        Some(PinboardCommand::NewFolder)
    );
}

/// Every group break the plate draws, so a divider cannot drift onto a row
/// that begins nothing.
#[test]
fn the_menu_groups_its_rows_by_what_they_are() {
    let model = pinboard::model(true, &DesktopSettings::default());
    let grouped: Vec<&str> = model
        .rows()
        .iter()
        .filter(|row| row.drawn().is_group_break())
        .map(|row| row.drawn().label())
        .collect();
    assert_eq!(
        grouped,
        vec![
            "New Folder",
            "Sort by Name",
            "Arrange from the Left",
            "Tidy Up",
            "Refresh"
        ]
    );
}

/// The chain answers with a row id and nothing else, so every row the model
/// offers carries one it can read back or opens the plate of rows that do.
#[test]
fn the_model_declares_no_row_the_chain_cannot_answer_with() {
    for on_icon in [false, true] {
        let model = pinboard::model(on_icon, &DesktopSettings::default());
        assert!(!model.rows().is_empty(), "a plate with nothing to choose");
        for (at, row) in model.rows().iter().enumerate() {
            let label = row.drawn().label();
            match row.child() {
                ChainChild::None => assert!(row.id().is_some(), "{label:?} answers nothing"),
                ChainChild::Submenu => assert!(
                    model
                        .rows()
                        .iter()
                        .any(|child| child.parent() == Some(at) && child.id().is_some()),
                    "{label:?} opens an empty plate"
                ),
                other => panic!("the backdrop menu hangs no window: {other:?}"),
            }
        }
    }
}

/// Tidy Up's three orders sit on a plate of their own, each naming its own
/// command.
#[test]
fn tidy_up_files_its_orders_on_a_plate_of_their_own() {
    let model = pinboard::model(false, &DesktopSettings::default());
    let plate = model
        .rows()
        .iter()
        .position(|row| row.drawn().label() == "Tidy Up")
        .expect("the Tidy Up row");
    let filed: Vec<Option<PinboardCommand>> = model
        .rows()
        .iter()
        .filter(|row| row.parent() == Some(plate))
        .map(|row| row.id().and_then(PinboardCommand::from_item))
        .collect();
    assert_eq!(
        filed,
        [
            Some(PinboardCommand::TidyUp(TidyOrder::Name)),
            Some(PinboardCommand::TidyUp(TidyOrder::Date)),
            Some(PinboardCommand::TidyUp(TidyOrder::Size)),
        ]
    );
}

#[test]
fn an_appearance_edit_asks_for_a_re_theme_and_nothing_of_the_backdrop() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let base = desktop.settings().clone();
    let themed = desktop
        .apply_settings(DesktopSettings {
            contrast: Contrast::High,
            ..base
        })
        .expect("the contrast changed");
    assert!(themed.appearance.theme);
    assert!(!themed.appearance.scale);
    assert!(!themed.appearance.cursor);
    assert!(themed.appearance.any());
    assert!(themed.layer, "the tiles are drawn in the theme");
    // A contrast change must not re-list the folder or decode a wallpaper.
    assert_eq!(themed.backdrop, BackdropWork::default());

    let base = desktop.settings().clone();
    let rescaled = desktop
        .apply_settings(DesktopSettings {
            scale: Scale::from_percent(150).expect("150% is a scale"),
            ..base
        })
        .expect("the scale changed");
    assert!(rescaled.appearance.scale);
    assert!(!rescaled.appearance.theme);
    assert!(rescaled.layer, "every tile resolves at the new density");
    assert_eq!(rescaled.backdrop, BackdropWork::default());
}

/// The cursor pair moves the pointer and nothing else: a re-theme would
/// repaint every surface and a rescale would move every length, for a
/// change only the pointer can see.
#[test]
fn a_cursor_edit_asks_only_for_the_pointer() {
    let edits: [fn(&mut DesktopSettings); 2] = [
        |s| s.cursor_size = CursorSize::Larger,
        |s| {
            s.cursor_set = CursorSetId::new("High Visibility").expect("a legal set name");
        },
    ];
    for edit in edits {
        let mut desktop = desktop_of(vec![file("a.txt")]);
        let mut wanted = desktop.settings().clone();
        edit(&mut wanted);
        let change = desktop.apply_settings(wanted).expect("the pointer moved");
        assert!(change.appearance.cursor);
        assert!(!change.appearance.theme);
        assert!(!change.appearance.scale);
        assert!(!change.layer, "the pointer is drawn over the layer");
        assert_eq!(change.backdrop, BackdropWork::default());
    }
}

#[test]
fn a_backdrop_edit_asks_for_no_appearance_work() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let base = desktop.settings().clone();
    let papered = desktop
        .apply_settings(DesktopSettings {
            wallpaper: WallpaperChoice::None,
            ..base
        })
        .expect("the wallpaper changed");
    assert!(!papered.appearance.any());
}

#[test]
fn every_appearance_axis_asks_for_the_re_theme() {
    // Each of the four reaches every open application through the republish
    // it converges on; all but the motion are ways the layer is drawn.
    type Edit = fn(&mut DesktopSettings);
    let axes: [(Edit, bool); 4] = [
        (|s| s.appearance = Appearance::Dark, true),
        (|s| s.contrast = Contrast::Monochrome, true),
        (|s| s.density = Density::Compact, true),
        (|s| s.motion = Motion::Reduced, false),
    ];
    for (axis, drawn) in axes {
        let mut desktop = desktop_of(vec![file("a.txt")]);
        let mut wanted = desktop.settings().clone();
        axis(&mut wanted);
        let change = desktop.apply_settings(wanted).expect("an axis moved");
        assert!(change.appearance.theme);
        assert_eq!(change.layer, drawn);
    }
}

// --- Where an icon stands -------------------------------------------------

/// Press at `at` at monotonic time `now_ns`, as the seat does.
fn press_at(
    desktop: &mut Desktop<FakeDir>,
    layout: &GridView,
    at: Point,
    now_ns: u64,
) -> DesktopOutcome {
    desktop.press(
        at,
        layout,
        Scale::ONE,
        &theme(),
        now_ns,
        &[],
        &mut Region::new(),
    )
}

/// Where the icon at `index` stands under `layout`.
fn bounds(desktop: &Desktop<FakeDir>, layout: &GridView, index: usize) -> Rect {
    desktop
        .bounds_for_test(layout, index)
        .expect("a shown icon")
}

/// Carry the icon at `index` by its centre `by` pixels and let it go there,
/// answering what putting it down asked for.
fn carry(
    desktop: &mut Desktop<FakeDir>,
    layout: &GridView,
    index: usize,
    by: (i32, i32),
) -> DesktopOutcome {
    let from = bounds(desktop, layout, index).center();
    press_at(desktop, layout, from, 0);
    let to = Point::new(from.x + by.0, from.y + by.1);
    desktop.follow_press(to, true, layout, Scale::ONE, &mut Region::new());
    desktop.follow_press(to, false, layout, Scale::ONE, &mut Region::new())
}

/// The record `edits` make of an empty one.
fn record_of(edits: &[ArrangementEdit]) -> Arrangement {
    let mut record = Arrangement::default();
    for edit in edits {
        record.make(edit).expect("room");
    }
    record
}

/// A desktop over `entries` whose record, read, keeps each named icon at its
/// place, each put down after the one before it.
fn placed_on(entries: Vec<Entry>, places: &[(&str, Place)]) -> Desktop<FakeDir> {
    let mut desktop = desktop_of(entries);
    let set = places
        .iter()
        .zip(0..)
        .map(|(&(name, at), order)| {
            (
                name.to_string(),
                Some(Placed {
                    id: FileId::NONE,
                    at,
                    order,
                }),
            )
        })
        .collect();
    let record = record_of(&[ArrangementEdit::Set(set)]);
    desktop.adopt_arrangement(&record, true, &layout_of(&desktop), &mut Region::new());
    let _ = desktop.take_arrangement_edits();
    desktop
}

/// The place of the cell `column` columns across and `row` rows down from the
/// leading corner.
const fn cell_place(column: u32, row: u32) -> Place {
    Place {
        across: column * PLACE_UNITS,
        down: row * PLACE_UNITS,
        from: IconFlow::Leading,
    }
}

/// The index of the icon named `name`.
fn named(desktop: &Desktop<FakeDir>, name: &str) -> usize {
    desktop
        .entries()
        .iter()
        .position(|entry| entry.name() == name)
        .expect("listed")
}

/// An icon carried across the desktop stands exactly where it was put down,
/// and the record keeps it there for the next session to show it the same.
#[test]
fn a_carried_icon_stands_exactly_where_it_is_put_down_and_is_kept_there() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let was = bounds(&desktop, &layout, 0);
    assert_eq!(
        carry(&mut desktop, &layout, 0, (237, 101)),
        DesktopOutcome::ignored()
    );
    let now = bounds(&desktop, &layout, 0);
    assert_eq!(
        (now.left() - was.left(), now.top() - was.top()),
        (237, 101),
        "exactly where it was put"
    );
    assert_eq!(
        bounds(&desktop, &layout, 1),
        cell(&layout, 1),
        "the icon beside it never moved"
    );
    let edits = desktop.take_arrangement_edits();
    assert_eq!(edits.len(), 1, "one change, made once the press came up");

    let mut next = desktop_of(vec![file("a.txt"), file("b.txt")]);
    next.adopt_arrangement(&record_of(&edits), true, &layout, &mut Region::new());
    assert_eq!(
        bounds(&next, &layout, 0),
        now,
        "and the next session shows it there"
    );
}

/// The icon is carried in memory alone while the press lasts: what it covers
/// and what it left are repainted, and nothing is kept until it is put down.
#[test]
fn a_carried_icon_repaints_what_it_leaves_and_covers_and_keeps_nothing_yet() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let from = cell(&layout, 0).center();
    press_at(&mut desktop, &layout, from, 0);
    let moved = damage_of(|damage| {
        desktop.follow_press(
            Point::new(from.x + 300, from.y + 200),
            true,
            &layout,
            Scale::ONE,
            damage,
        );
    });
    let carried = bounds(&desktop, &layout, 0);
    assert!(moved.intersects(cell(&layout, 0)), "the ground it left");
    assert!(moved.intersects(carried), "and the ground it covers");
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "nothing is kept while the press lasts"
    );
}

#[test]
fn a_press_that_never_travels_is_a_click_and_moves_nothing() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let at = cell(&layout, 0).center();
    press_at(&mut desktop, &layout, at, 0);
    let still = Point::new(at.x + 1, at.y + 1);
    let held = damage_of(|damage| {
        desktop.follow_press(still, true, &layout, Scale::ONE, damage);
    });
    assert!(held.is_empty(), "a hand holding still is not a drag");
    assert_eq!(
        desktop.follow_press(still, false, &layout, Scale::ONE, &mut Region::new()),
        DesktopOutcome::ignored()
    );
    let later = Point::new(at.x + 300, at.y + 200);
    assert!(
        damage_of(|damage| {
            desktop.follow_press(later, true, &layout, Scale::ONE, damage);
        })
        .is_empty(),
        "and the press it was is over"
    );
    assert_eq!(bounds(&desktop, &layout, 0), cell(&layout, 0));
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "nothing the user did not place is stored"
    );
}

#[test]
fn where_the_grid_is_asked_for_a_carried_icon_settles_on_the_nearest_free_cell() {
    let settings = DesktopSettings {
        align_to_grid: true,
        ..DesktopSettings::default()
    };
    let mut desktop = desktop_with(vec![file("a.txt"), file("b.txt")], settings);
    let layout = layout_of(&desktop);
    let beside = cell(&layout, layout.cells_per_line());
    let was = cell(&layout, 0);
    carry(
        &mut desktop,
        &layout,
        0,
        (beside.left() - was.left() + 9, 7),
    );
    assert_eq!(bounds(&desktop, &layout, 0), beside);
}

#[test]
fn settling_on_the_grid_passes_over_a_cell_another_icon_holds() {
    let settings = DesktopSettings {
        align_to_grid: true,
        ..DesktopSettings::default()
    };
    let mut desktop = desktop_with(vec![file("a.txt"), file("b.txt")], settings);
    let layout = layout_of(&desktop);
    let (a, b) = (cell(&layout, 0), cell(&layout, 1));
    carry(
        &mut desktop,
        &layout,
        1,
        (a.left() - b.left() + 3, a.top() - b.top() + 3),
    );
    let settled = bounds(&desktop, &layout, 1);
    assert_ne!(settled, a, "a.txt holds its cell");
    assert!(
        (0..layout.visible_range(0).len()).any(|at| cell(&layout, at) == settled),
        "and b.txt settles on a cell of the grid: {settled:?}"
    );
}

/// Overlaps are allowed: an icon put down on another stands over it, and a
/// press there meets the one put there last.
#[test]
fn an_icon_put_over_another_overlaps_it_and_is_met_first() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let (a, b) = (cell(&layout, 0), cell(&layout, 1));
    carry(
        &mut desktop,
        &layout,
        0,
        (b.left() - a.left(), b.top() - a.top()),
    );
    assert_eq!(
        bounds(&desktop, &layout, 0),
        b,
        "a.txt stands on b.txt's cell"
    );
    assert_eq!(bounds(&desktop, &layout, 1), b, "and b.txt stays under it");
    press_at(&mut desktop, &layout, b.center(), 10_000_000_000);
    assert_eq!(
        desktop.selected(),
        Some(0),
        "the icon on top is the one met"
    );
}

#[test]
fn a_cell_a_removal_frees_is_the_next_a_new_icon_takes() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt"), file("c.txt")]);
    let layout = layout_of(&desktop);
    desktop.apply_changes(
        vec![EntryChange::Remove(String::from("b.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(
        bounds(&desktop, &layout, 1),
        cell(&layout, 2),
        "c.txt stays where it stood"
    );
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("z.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "z.txt")),
        cell(&layout, 1),
        "z.txt takes the cell b.txt left"
    );
}

/// A rename arrives as one name going and another coming for the same file:
/// the icon keeps the place the user put it in, and the record follows it.
#[test]
fn a_renamed_file_keeps_its_place() {
    let id = FileId {
        volume: [3; 16],
        node: 42,
    };
    let mut desktop = desktop_of(vec![file("a.txt").with_id(id), file("b.txt")]);
    let layout = layout_of(&desktop);
    carry(&mut desktop, &layout, 0, (300, 200));
    let put = bounds(&desktop, &layout, 0);
    let _ = desktop.take_arrangement_edits();
    desktop.apply_changes(
        vec![
            EntryChange::Remove(String::from("a.txt")),
            EntryChange::Upsert(file("z.txt").with_id(id)),
        ],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "z.txt")),
        put,
        "z.txt stands where a.txt was put"
    );
    assert_eq!(
        desktop.take_arrangement_edits(),
        [ArrangementEdit::Renamed {
            from: PlaceKey::of("a.txt"),
            to: String::from("z.txt"),
            id,
        }]
    );
}

#[test]
fn a_deleted_file_gives_up_its_place() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    carry(&mut desktop, &layout, 0, (300, 200));
    let _ = desktop.take_arrangement_edits();
    desktop.apply_changes(
        vec![EntryChange::Remove(String::from("a.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(
        desktop.take_arrangement_edits(),
        [ArrangementEdit::Set(vec![(String::from("a.txt"), None)])]
    );
}

/// A resolution change can leave a place off the screen: it is shown on the
/// free cell nearest it, and the place kept is not moved to where it is shown.
#[test]
fn a_place_the_screen_no_longer_reaches_is_shown_nearest_it_and_kept_as_it_was() {
    let far = Place {
        across: 1_000 * PLACE_UNITS,
        down: 1_000 * PLACE_UNITS,
        from: IconFlow::Leading,
    };
    let mut desktop = placed_on(vec![file("a.txt")], &[("a.txt", far)]);
    let layout = layout_of(&desktop);
    let last = layout.visible_range(0).len() - 1;
    assert_eq!(
        bounds(&desktop, &layout, 0),
        cell(&layout, last),
        "it is shown on the cell nearest its place"
    );
    desktop.set_focused(true, &layout, &mut Region::new());
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "and its place is kept as it was"
    );
}

#[test]
fn tidying_lays_every_icon_out_in_its_order_and_keeps_the_lot_at_once() {
    let mut desktop = desktop_of(vec![
        Entry::new("b.txt", EntryKind::File, 300, Time64::from_secs(30)),
        Entry::new("a.txt", EntryKind::File, 900, Time64::from_secs(20)),
        Entry::new("c.txt", EntryKind::File, 100, Time64::from_secs(10)),
    ]);
    let layout = layout_of(&desktop);
    carry(&mut desktop, &layout, 0, (300, 200));
    let _ = desktop.take_arrangement_edits();
    assert_eq!(
        desktop.command(PinboardCommand::TidyUp(TidyOrder::Size), &layout, &[]),
        DesktopOutcome {
            rearranged: true,
            action: None,
        }
    );
    let at = |name: &str| bounds(&desktop, &layout, named(&desktop, name));
    assert_eq!(
        [at("c.txt"), at("b.txt"), at("a.txt")],
        [cell(&layout, 0), cell(&layout, 1), cell(&layout, 2)],
        "smallest first, from the corner the icons run from"
    );
    assert_eq!(
        desktop.take_arrangement_edits().len(),
        1,
        "one change keeps them all"
    );
}

/// A new order flows again the icons no one placed, and leaves the one the
/// user put somewhere where they put it.
#[test]
fn a_new_order_flows_again_only_the_icons_no_one_placed() {
    let mut desktop = placed_on(
        vec![
            Entry::new("b.txt", EntryKind::File, 300, Time64::from_secs(30)),
            Entry::new("a.txt", EntryKind::File, 900, Time64::from_secs(20)),
            Entry::new("c.txt", EntryKind::File, 100, Time64::from_secs(10)),
        ],
        &[("a.txt", cell_place(1, 1))],
    );
    let layout = layout_of(&desktop);
    let placed = bounds(&desktop, &layout, named(&desktop, "a.txt"));
    desktop
        .apply_settings(arranged_by(IconFlow::default(), IconSort::Size))
        .expect("the order changed");
    let layout = layout_of(&desktop);
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "c.txt")),
        cell(&layout, 0)
    );
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "b.txt")),
        cell(&layout, 1)
    );
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "a.txt")),
        placed,
        "the placed icon stays put"
    );
}

/// Wait for a save of `name`, which its volume may make as `finished`, let
/// go at `drop` with a wait ending at monotonic time 100.
fn expect_save(desktop: &mut Desktop<FakeDir>, name: &str, finished: Option<&str>, drop: Point) {
    let layout = layout_of(desktop);
    desktop.offer_landing(Some(LandingOffer::saved(name, finished)));
    desktop.expect_offered(drop, &layout, Scale::ONE, &theme(), 100);
}

/// Wait for a drop of the one file `name`, its picture held where the press
/// was, let go at `drop` with a wait ending at monotonic time 100.
fn expect_drop(desktop: &mut Desktop<FakeDir>, name: &str, drop: Point) {
    let layout = layout_of(desktop);
    let picture = DragPicture {
        icon: IconAsset::new("text").expect("a valid asset id"),
        from: Some((0, 0)),
    };
    let items = DragItems::new(
        DocumentName::new(name).expect("a valid name"),
        1,
        false,
        [picture],
    )
    .expect("one file");
    desktop.offer_landing(Some(LandingOffer::of(&items)));
    desktop.expect_offered(drop, &layout, Scale::ONE, &theme(), 100);
}

/// A copy that finds its name taken tells the file it makes apart, and the
/// file still stands where the drop was let go.
#[test]
fn a_dropped_file_its_copy_names_apart_still_lands_where_it_was_let_go() {
    let mut desktop = desktop_of(vec![file("x.txt")]);
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_drop(&mut desktop, "x.txt", drop);
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("x (2).txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert!(bounds(&desktop, &layout, named(&desktop, "x (2).txt")).contains(drop));
}

/// A file a one-file drop did not name is not taken for the one it did, which
/// still stands where it was let go when it arrives after.
#[test]
fn a_file_a_one_file_drop_did_not_name_is_not_taken_for_it() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_drop(&mut desktop, "report.txt", drop);
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("other.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "other.txt")),
        cell(&layout, 0),
        "the file it did not name flows"
    );
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "and nothing is kept for it"
    );
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("report.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert!(
        bounds(&desktop, &layout, named(&desktop, "report.txt")).contains(drop),
        "the file it named stands where it was let go"
    );
}

#[test]
fn a_saved_file_lands_where_its_save_was_let_go() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_save(&mut desktop, "Letter.txt", None, drop);
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("Letter.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert!(
        bounds(&desktop, &layout, named(&desktop, "Letter.txt")).contains(drop),
        "it stands under the pointer it was let go at"
    );
    assert!(
        matches!(
            desktop.take_arrangement_edits().as_slice(),
            [ArrangementEdit::Set(set)] if set.len() == 1
        ),
        "and is kept there"
    );
}

/// A volume that keeps no file type finishes a save's name with its type, and
/// the file it makes lands as surely as one made under the name asked for.
#[test]
fn a_save_its_volume_names_otherwise_still_lands_where_it_was_let_go() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_save(&mut desktop, "Letter", Some("Letter,fff"), drop);
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("Letter,fff"))],
        layout_of,
        &mut Region::new(),
    );
    assert!(bounds(&desktop, &layout, named(&desktop, "Letter,fff")).contains(drop));
}

#[test]
fn a_file_appearing_after_the_wait_takes_the_next_free_cell() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    expect_save(&mut desktop, "Letter.txt", None, Point::new(500, 400));
    desktop.expire_landing(100);
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("Letter.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "Letter.txt")),
        cell(&layout, 1)
    );
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "nothing the user did not place is stored"
    );
}

#[test]
fn each_file_a_drop_carried_lands_as_far_from_the_drop_as_it_was_held() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let picture = |from| DragPicture {
        icon: IconAsset::new("text").expect("a valid asset id"),
        from: Some(from),
    };
    let items = DragItems::new(
        DocumentName::new("one.txt").expect("a valid name"),
        2,
        false,
        [picture((0, 0)), picture((150, 40))],
    )
    .expect("two files");
    desktop.offer_landing(Some(LandingOffer::of(&items)));
    desktop.expect_offered(Point::new(300, 300), &layout, Scale::ONE, &theme(), 100);
    desktop.apply_changes(
        vec![
            EntryChange::Upsert(file("one.txt")),
            EntryChange::Upsert(file("two.txt")),
        ],
        layout_of,
        &mut Region::new(),
    );
    let (one, two) = (
        bounds(&desktop, &layout, named(&desktop, "one.txt")),
        bounds(&desktop, &layout, named(&desktop, "two.txt")),
    );
    assert_eq!((two.left() - one.left(), two.top() - one.top()), (150, 40));
}

/// A drop that moved on from the bare desktop leaves nothing to land.
#[test]
fn a_drop_with_no_offer_standing_lands_nothing() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    desktop.offer_landing(None);
    desktop.expect_offered(Point::new(300, 300), &layout, Scale::ONE, &theme(), 100);
    desktop.apply_changes(
        vec![EntryChange::Upsert(file("one.txt"))],
        layout_of,
        &mut Region::new(),
    );
    assert_eq!(bounds(&desktop, &layout, 0), cell(&layout, 0));
    assert!(desktop.take_arrangement_edits().is_empty());
}

#[test]
fn a_record_read_after_the_listing_moves_only_the_icons_it_places() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    desktop.set_focused(false, &layout, &mut Region::new());
    let set = vec![(
        String::from("b.txt"),
        Some(Placed {
            id: FileId::NONE,
            at: cell_place(2, 1),
            order: 0,
        }),
    )];
    let damage = damage_of(|damage| {
        desktop.adopt_arrangement(
            &record_of(&[ArrangementEdit::Set(set)]),
            true,
            &layout,
            damage,
        );
    });
    let taken = cell(&layout, 2 * layout.cells_per_line() + 1);
    assert_eq!(bounds(&desktop, &layout, 1), taken);
    assert_eq!(
        bounds(&desktop, &layout, 0),
        cell(&layout, 0),
        "a.txt never moved"
    );
    assert!(
        damage.intersects(cell(&layout, 1)) && damage.intersects(taken),
        "the cell b.txt left and the place it took"
    );
    assert!(!damage.intersects(cell(&layout, 0)));
}

#[test]
fn a_place_kept_for_a_file_renamed_while_away_follows_the_file() {
    let id = FileId {
        volume: [5; 16],
        node: 7,
    };
    let mut desktop = desktop_of(vec![file("new.txt").with_id(id)]);
    let layout = layout_of(&desktop);
    let kept = vec![(
        String::from("old.txt"),
        Some(Placed {
            id,
            at: cell_place(3, 2),
            order: 0,
        }),
    )];
    desktop.adopt_arrangement(
        &record_of(&[ArrangementEdit::Set(kept)]),
        true,
        &layout,
        &mut Region::new(),
    );
    assert_eq!(
        bounds(&desktop, &layout, 0),
        cell(&layout, 3 * layout.cells_per_line() + 2)
    );
    assert_eq!(
        desktop.take_arrangement_edits(),
        [ArrangementEdit::Renamed {
            from: PlaceKey::of("old.txt"),
            to: String::from("new.txt"),
            id,
        }]
    );
}

#[test]
fn a_place_kept_for_a_file_gone_while_away_is_forgotten_once_the_record_is_read() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let kept = ArrangementEdit::Set(vec![(
        String::from("gone.txt"),
        Some(Placed {
            id: FileId::NONE,
            at: cell_place(1, 1),
            order: 0,
        }),
    )]);
    let record = record_of(core::slice::from_ref(&kept));
    desktop.adopt_arrangement(&record, false, &layout, &mut Region::new());
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "nothing is forgotten before the store has been read"
    );
    desktop.adopt_arrangement(&record, true, &layout, &mut Region::new());
    assert_eq!(
        desktop.take_arrangement_edits(),
        [ArrangementEdit::Forget(vec![PlaceKey::of("gone.txt")])]
    );
}

#[test]
fn an_icon_put_down_past_what_the_record_keeps_says_so_and_goes_back() {
    let names: Vec<String> = (0..=MAX_PLACED)
        .map(|n| alloc::format!("{n:03}.txt"))
        .collect();
    let entries = names.iter().map(|name| file(name)).collect();
    let places: Vec<(&str, Place)> = names
        .iter()
        .take(MAX_PLACED)
        .map(|name| (name.as_str(), cell_place(1, 1)))
        .collect();
    let mut desktop = placed_on(entries, &places);
    let layout = layout_of(&desktop);
    let was = bounds(&desktop, &layout, MAX_PLACED);
    let refused = carry(&mut desktop, &layout, MAX_PLACED, (120, 60));
    assert!(
        matches!(refused.action, Some(DesktopAction::Refuse(_))),
        "the record says it has no room"
    );
    assert_eq!(
        bounds(&desktop, &layout, MAX_PLACED),
        was,
        "and the icon stands where it stood"
    );
}

#[test]
fn an_arrow_moves_to_the_nearest_icon_that_way_wherever_the_icons_stand() {
    let mut desktop = placed_on(
        vec![file("a.txt"), file("b.txt"), file("c.txt")],
        &[
            ("a.txt", cell_place(0, 0)),
            ("b.txt", cell_place(3, 0)),
            ("c.txt", cell_place(1, 2)),
        ],
    );
    let layout = layout_of(&desktop);
    desktop.set_focused(true, &layout, &mut Region::new());
    desktop.key(right(), true, &layout, &[], &mut Region::new());
    assert_eq!(
        desktop.selected(),
        Some(0),
        "the first arrow starts somewhere"
    );
    desktop.key(right(), true, &layout, &[], &mut Region::new());
    assert_eq!(
        desktop.selected(),
        Some(1),
        "b.txt is the icon to the right"
    );
    desktop.key(down(), true, &layout, &[], &mut Region::new());
    assert_eq!(desktop.selected(), Some(2), "c.txt is the one below");
    desktop.key(left(), true, &layout, &[], &mut Region::new());
    assert_eq!(desktop.selected(), Some(0), "a.txt is the one to the left");
    assert!(
        damage_of(|damage| {
            desktop.key(up(), true, &layout, &[], damage);
        })
        .is_empty(),
        "with no icon that way nothing moves"
    );
    assert_eq!(desktop.selected(), Some(0));
}

#[test]
fn turning_the_grid_on_or_off_moves_no_icon() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let base = desktop.settings().clone();
    assert_eq!(
        desktop.apply_settings(DesktopSettings {
            align_to_grid: true,
            ..base
        }),
        Some(PinboardChange::default()),
        "it decides where the next icon put down settles"
    );
}

/// The seat says while the primary button is down, which is what keeps an
/// icon carried for as long as the press that took it lasts.
#[test]
fn the_seat_says_while_the_primary_button_is_held() {
    let (mut shell, mut comp) = crate::tests::headless_desktop();
    let _ = shell.apply(crate::tests::moved(300, 200), &mut comp, 0);
    assert!(!shell.router().holds_primary());
    let _ = shell.apply(crate::tests::PRIMARY_PRESS, &mut comp, 0);
    assert!(shell.router().holds_primary(), "the press is held");
    let _ = shell.apply(crate::tests::moved(340, 220), &mut comp, 0);
    assert!(shell.router().holds_primary(), "and carried");
    let _ = shell.apply(crate::tests::PRIMARY_RELEASE, &mut comp, 0);
    assert!(!shell.router().holds_primary(), "until it comes up");
}

/// Pump `events` through `shell` as one wake of the serve loop drains them,
/// and route each batch to `desktop` as the session does: every outcome first
/// follows a held press to where the pointer is now, then a press on the
/// desktop takes what it lands on.
fn routed(
    desktop: &mut Desktop<FakeDir>,
    shell: &mut DesktopShell,
    comp: &mut Compositor,
    events: &[InputEvent],
) {
    let mut source = crate::tests::MemoryInput::new(events);
    let mut outcomes = Vec::new();
    loop {
        let stopped = shell
            .pump(&mut source, comp, 0, &mut outcomes)
            .expect("an in-memory source does not fault");
        let layout = shell.desktop_layout(comp, desktop);
        let (pointer, held) = (shell.router().pointer(), shell.router().holds_primary());
        for outcome in &outcomes {
            desktop.follow_press(pointer, held, &layout, comp.scale(), &mut Region::new());
            if outcome.value == ShellOutcome::WindowManager(InputResponse::DesktopPressed) {
                desktop.press(
                    pointer,
                    &layout,
                    comp.scale(),
                    shell.session().active_theme(),
                    outcome.at_ns,
                    &[],
                    &mut Region::new(),
                );
            }
        }
        if stopped == Stopped::Empty {
            return;
        }
    }
}

/// The seat applies a whole batch before routing any of it, so the release
/// can arrive with motion not yet followed: the icon still lands where it was
/// let go, not where the batch before left it.
#[test]
fn an_icon_let_go_with_motion_still_to_follow_lands_where_it_was_let_go() {
    let (mut shell, mut comp) = crate::tests::headless_desktop();
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = shell.desktop_layout(&comp, &desktop);
    let from = bounds(&desktop, &layout, 0).center();
    let to = |dx: i32, dy: i32| crate::tests::moved(from.x + dx, from.y + dy);
    let mut wake = |events: &[InputEvent]| routed(&mut desktop, &mut shell, &mut comp, events);
    wake(&[to(0, 0), crate::tests::PRIMARY_PRESS]);
    wake(&[to(60, 40)]);
    wake(&[to(120, 80), to(200, 120), crate::tests::PRIMARY_RELEASE]);
    let put = bounds(&desktop, &layout, 0).center();
    assert_eq!(
        (put.x - from.x, put.y - from.y),
        (200, 120),
        "where it was let go"
    );
}

/// A press routed on its own, then carried and let go within one batch, is a
/// carry rather than a click.
#[test]
fn a_press_carried_and_let_go_within_one_batch_moves_its_icon() {
    let (mut shell, mut comp) = crate::tests::headless_desktop();
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = shell.desktop_layout(&comp, &desktop);
    let from = bounds(&desktop, &layout, 0).center();
    let to = |dx: i32, dy: i32| crate::tests::moved(from.x + dx, from.y + dy);
    let mut wake = |events: &[InputEvent]| routed(&mut desktop, &mut shell, &mut comp, events);
    wake(&[to(0, 0), crate::tests::PRIMARY_PRESS]);
    wake(&[to(100, 50), to(150, 90), crate::tests::PRIMARY_RELEASE]);
    let put = bounds(&desktop, &layout, 0).center();
    assert_eq!(
        (put.x - from.x, put.y - from.y),
        (150, 90),
        "it went with the press"
    );
}

/// A burst of motion over the bare desktop routes as one outcome: each sample
/// would only ask what lies under the one pointer the seat holds.
#[test]
fn a_burst_of_motion_over_the_desktop_folds_into_one_outcome() {
    let (mut shell, mut comp) = crate::tests::headless_desktop();
    let mut source = crate::tests::MemoryInput::new(&[
        crate::tests::moved(300, 200),
        crate::tests::moved(310, 205),
        crate::tests::moved(320, 210),
    ]);
    let mut outcomes = Vec::new();
    shell
        .pump(&mut source, &mut comp, 0, &mut outcomes)
        .expect("an in-memory source does not fault");
    let seen: Vec<&ShellOutcome> = outcomes.iter().map(|timed| &timed.value).collect();
    assert_eq!(
        seen,
        [&ShellOutcome::WindowManager(
            InputResponse::DesktopPointerMoved
        )]
    );
    assert_eq!(
        shell.router().pointer(),
        Point::new(320, 210),
        "at the newest"
    );
}

/// A tidy of more icons than the field has cells leaves each it could not
/// place to flow after the others, never on a cell it gave another.
#[test]
fn a_tidy_of_more_icons_than_cells_stands_none_on_another() {
    let cells = layout_of(&desktop_of(Vec::new())).visible_range(0).len();
    let total = cells + 4;
    // The largest are listed first, so they flowed onto the cells the tidy
    // gives the smallest.
    let entries = (0..total)
        .map(|n| {
            let size = u64::try_from(total - n).expect("a small count");
            Entry::new(
                format!("{n:03}.txt"),
                EntryKind::File,
                size,
                Time64::UNIX_EPOCH,
            )
        })
        .collect();
    let mut desktop = desktop_of(entries);
    let layout = layout_of(&desktop);
    desktop.command(PinboardCommand::TidyUp(TidyOrder::Size), &layout, &[]);
    let shown: Vec<Rect> = (0..total)
        .filter_map(|index| desktop.bounds_for_test(&layout, index))
        .collect();
    assert_eq!(shown.len(), cells, "every cell holds one icon and no more");
    for (at, rect) in shown.iter().enumerate() {
        assert!(
            !shown[at + 1..].contains(rect),
            "two icons stand on {rect:?}"
        );
    }
}

/// Icons kept past the edge of a narrower field, or of one at a larger scale,
/// each stand on a free cell near where they were put, never piled onto one
/// another at the edge.
#[test]
fn icons_kept_past_the_field_each_take_a_cell_of_their_own() {
    let desktop = placed_on(
        vec![file("a.txt"), file("b.txt"), file("c.txt")],
        &[
            ("a.txt", cell_place(5, 0)),
            ("b.txt", cell_place(6, 0)),
            ("c.txt", cell_place(7, 0)),
        ],
    );
    let double = Scale::from_percent(200).expect("a scale");
    for field in [
        desktop.layout(Rect::new(0, 0, 400, 600), Scale::ONE, &theme()),
        desktop.layout(work_area(), double, &theme()),
    ] {
        let shown: Vec<Rect> = (0..3)
            .map(|index| bounds(&desktop, &field, index))
            .collect();
        for (at, rect) in shown.iter().enumerate() {
            assert!(
                !shown[at + 1..].contains(rect),
                "two icons stand on {rect:?}"
            );
            assert_eq!(
                rect.intersection(&field.tile_area()),
                *rect,
                "{rect:?} stands on the field"
            );
        }
    }
}

/// Under the trailing arrangement a place is measured from the trailing edge,
/// so the icons tidied against it stay against it on a wider field.
#[test]
fn a_trailing_arrangement_keeps_its_places_against_the_trailing_edge() {
    let mut desktop = desktop_with(
        vec![file("a.txt"), file("b.txt")],
        arranged_by(IconFlow::Trailing, IconSort::Name),
    );
    let layout = layout_of(&desktop);
    desktop.command(PinboardCommand::TidyUp(TidyOrder::Name), &layout, &[]);
    let wide = desktop.layout(Rect::new(0, 0, 1200, 600), Scale::ONE, &theme());
    assert_eq!(
        [bounds(&desktop, &wide, 0), bounds(&desktop, &wide, 1)],
        [cell(&wide, 0), cell(&wide, 1)],
        "still against the trailing edge"
    );
}

/// The icon put down last stands over the one it was put on, whatever the two
/// are called and however they are sorted.
#[test]
fn the_icon_put_down_last_stands_over_the_one_it_was_put_on() {
    let mut desktop = desktop_of(vec![
        Entry::new("a.txt", EntryKind::File, 900, Time64::UNIX_EPOCH),
        Entry::new("b.txt", EntryKind::File, 100, Time64::UNIX_EPOCH),
    ]);
    let layout = layout_of(&desktop);
    carry(&mut desktop, &layout, 1, (300, 200));
    let (a, b) = (bounds(&desktop, &layout, 0), bounds(&desktop, &layout, 1));
    carry(
        &mut desktop,
        &layout,
        0,
        (b.left() - a.left(), b.top() - a.top()),
    );
    press_at(&mut desktop, &layout, b.center(), 10_000_000_000);
    assert_eq!(
        desktop.selected(),
        Some(0),
        "a.txt, put down last, is met first"
    );
    desktop
        .apply_settings(arranged_by(IconFlow::default(), IconSort::Size))
        .expect("the order changed");
    press_at(&mut desktop, &layout, b.center(), 20_000_000_000);
    assert_eq!(
        desktop.selected(),
        Some(named(&desktop, "a.txt")),
        "and stays on top through a new order"
    );
}

/// The desktop's own change goes into the record before the record is handed
/// back, so a store answer collected meanwhile cannot put the icon back.
#[test]
fn an_icon_put_down_as_the_record_arrives_stays_where_it_was_put() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let mut memory = ArrangementMemory::new();
    assert!(memory.next_load(0));
    let kept = Placed {
        id: FileId::NONE,
        at: cell_place(3, 2),
        order: 0,
    };
    let set = vec![(String::from("b.txt"), Some(kept))];
    memory.loaded(0, Some(record_of(&[ArrangementEdit::Set(set)])));
    carry(&mut desktop, &layout, 0, (300, 200));
    let put = bounds(&desktop, &layout, 0);
    assert!(
        !desktop.keep_record(&mut memory, layout_of, &mut Region::new()),
        "the record has room for it"
    );
    assert_eq!(
        bounds(&desktop, &layout, 0),
        put,
        "a.txt stands where it was put"
    );
    assert_eq!(
        bounds(&desktop, &layout, 1),
        cell(&layout, 3 * layout.cells_per_line() + 2),
        "and b.txt where the record keeps it"
    );
    assert!(
        memory.live().place_of("a.txt").is_some(),
        "which keeps a.txt too"
    );
}

/// An arrow still has somewhere to start when the icon selected is not shown:
/// the first icon that is, as with nothing selected.
#[test]
fn an_arrow_starts_from_the_first_icon_shown_when_the_selection_is_not() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    press_at(&mut desktop, &layout, centre_of(&layout, 1), 0);
    assert_eq!(desktop.selected(), Some(1));
    let (tile, margin) = (cell(&layout, 0), 2 * DESKTOP_MARGIN);
    let one = desktop.layout(
        Rect::new(0, 0, tile.width + margin, tile.height + margin),
        Scale::ONE,
        &theme(),
    );
    assert_eq!(one.visible_range(0).len(), 1, "a field of one cell");
    assert_eq!(desktop.bounds_for_test(&one, 1), None, "b.txt is not shown");
    desktop.key(down(), true, &one, &[], &mut Region::new());
    assert_eq!(desktop.selected(), Some(0), "the first icon shown");
}

/// Icons stacked exactly on one another are each reached by the arrows, in
/// the order they are drawn.
#[test]
fn the_arrows_reach_every_icon_of_a_stack_in_the_order_they_are_drawn() {
    let mut desktop = placed_on(
        vec![file("a.txt"), file("b.txt"), file("c.txt")],
        &[
            ("a.txt", cell_place(2, 1)),
            ("b.txt", cell_place(2, 1)),
            ("c.txt", cell_place(2, 1)),
        ],
    );
    let layout = layout_of(&desktop);
    let stack = bounds(&desktop, &layout, 2).center();
    press_at(&mut desktop, &layout, stack, 0);
    assert_eq!(
        desktop.selected(),
        Some(2),
        "c.txt, put down last, is on top"
    );
    let mut met = Vec::new();
    for key in [left(), left(), left(), right(), right(), right()] {
        desktop.key(key, true, &layout, &[], &mut Region::new());
        met.push(desktop.selected());
    }
    assert_eq!(met, [Some(1), Some(0), Some(0), Some(1), Some(2), Some(2)]);
}

/// An application states where each picture it shows stood, and nothing bounds
/// what it states: a drop of pictures held at the very ends of the coordinate
/// space still lands every file on the field, and keeping icons to the grid
/// settles them there rather than overflowing finding the nearest cell.
#[test]
fn a_drop_of_pictures_held_at_the_ends_of_the_coordinates_lands_on_the_field() {
    let settings = DesktopSettings {
        align_to_grid: true,
        ..DesktopSettings::default()
    };
    let mut desktop = desktop_with(Vec::new(), settings);
    let layout = layout_of(&desktop);
    let picture = |from| DragPicture {
        icon: IconAsset::new("text").expect("a valid asset id"),
        from: Some(from),
    };
    let items = DragItems::new(
        DocumentName::new("0.txt").expect("a valid name"),
        4,
        false,
        [
            picture((i32::MIN, i32::MIN)),
            picture((i32::MAX, i32::MAX)),
            picture((i32::MIN, i32::MAX)),
            picture((i32::MAX, i32::MIN)),
        ],
    )
    .expect("four files");
    desktop.offer_landing(Some(LandingOffer::of(&items)));
    desktop.expect_offered(Point::new(300, 300), &layout, Scale::ONE, &theme(), 100);
    desktop.apply_changes(
        (0..4)
            .map(|n| EntryChange::Upsert(file(&format!("{n}.txt"))))
            .collect(),
        layout_of,
        &mut Region::new(),
    );
    let area = layout.tile_area();
    for n in 0..4 {
        let shown = bounds(&desktop, &layout, named(&desktop, &format!("{n}.txt")));
        assert_eq!(
            shown.intersection(&area),
            shown,
            "{n}.txt stands on the field"
        );
    }
}

/// A resting name's halo is blurred once and kept: a repaint lays the same one
/// down again, and only a new name, scale or theme blurs one afresh.
#[test]
fn a_halo_is_blurred_again_only_for_a_new_name_scale_or_theme() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let halos = |desktop: &Desktop<FakeDir>, scale: Scale, theme: &Theme| {
        let layout = desktop.layout(work_area(), scale, theme);
        let mut surface = Surface::new(800, 600).expect("a screen-sized layer");
        desktop.render(
            &mut surface,
            &layout,
            scale,
            theme,
            &mut NoArtwork,
            work_area(),
            None,
        );
        ["a.txt", "b.txt", "c.txt"].map(|name| desktop.halo_for_test(name))
    };
    let blurred = |was: &[Option<Arc<LabelHalo>>; 3], now: &[Option<Arc<LabelHalo>>; 3]| {
        was.iter()
            .zip(now)
            .filter(|&(was, now)| match (was, now) {
                (Some(was), Some(now)) => !Arc::ptr_eq(was, now),
                (held, now) => held.is_none() && now.is_some(),
            })
            .count()
    };
    let (dark, light) = (Theme::dark(), Theme::light());
    let double = Scale::from_percent(200).expect("a scale");
    let first = halos(&desktop, Scale::ONE, &dark);
    assert!(
        first[0].is_some() && first[1].is_some(),
        "each resting name keeps one"
    );
    let again = halos(&desktop, Scale::ONE, &dark);
    assert_eq!(blurred(&first, &again), 0, "a repaint blurs none");
    let scaled = halos(&desktop, double, &dark);
    assert_eq!(blurred(&again, &scaled), 2, "a new scale blurs each again");
    let themed = halos(&desktop, double, &light);
    assert_eq!(blurred(&scaled, &themed), 2, "and so does a new theme");
    desktop.apply_changes(
        vec![
            EntryChange::Remove(String::from("b.txt")),
            EntryChange::Upsert(file("c.txt")),
        ],
        layout_of,
        &mut Region::new(),
    );
    let renamed = halos(&desktop, double, &light);
    assert_eq!(
        blurred(&themed, &renamed),
        1,
        "a new name blurs its own and no other"
    );
    assert!(renamed[1].is_none(), "and a name gone keeps none");
}
