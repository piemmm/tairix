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
use tairix_abi::pinboard_ipc::{PinboardDocument, PinboardRequest, PINBOARD_MAX_REQUEST};
use tairix_abi::sysinfo::{MountAvailability, VolumeClass};
use tairix_abi::window_ipc::{
    AppMenuItemId, DocumentName, DragItems, DragOwnIcon, DragOwnSource, DragPicture, IconAsset,
};
use tairix_abi::window_ipc::{LandingName, LandingOutcome, LandingStep};
use tairix_abi::{Errno, FileId, Time64};
use tairix_browse::chrome::REASON_NOT_EJECTABLE;
use tairix_browse::mount::{volume_root, MountAct};
use tairix_browse::render::{entry_parts, Tiles};
use tairix_browse::{
    AppAssociation, DesktopDrives, DirectorySource, DriveClass, Entry, EntryChange, EntryKind,
    FilerPreferences, GridView, HiddenEndings, LinkTarget, Listing, Volume, VolumeFacts,
};
use tairix_colour::Rgb;
use tairix_controls::{ActivityState, LabelHalo, MenuMark};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::{IconKind, IconRequest, NoArtwork};
use tairix_log::DiscardSink;
use tairix_proglib::{BundlePath, Catalog, DisplayName, EntryId, LibraryCategory, LibraryEntry};
use tairix_raster::Surface;
use tairix_theme::{CursorSetId, Theme};
use tairix_wallpaper::{
    Backdrop, CursorSize, DesktopSettings, IconFlow, IconSort, WallpaperChoice,
};
use tairix_wm::{Compositor, InputEvent, InputResponse, Key, NamedKey};

use crate::desktop::LandingWork;
use crate::desktop::{
    BackdropWork, Desktop, DesktopAction, DesktopActivation, DesktopOutcome, GestureRefusal,
    LandingOffer, LaunchDocument, PinboardChange, SettingsEdit, DESKTOP_MARGIN,
};
use crate::drag::{DesktopItem, DragEnd, DragStep, DragSurface, ItemsEnd, OwedReport};
use crate::launcher::Kept;
use crate::pinboard::{
    self, Arrangement, ArrangementEdit, ArrangementMemory, Over, PinboardCommand, Place, PlaceKey,
    Placed, TidyOrder, MAX_PLACED, PLACE_UNITS, REASON_DRIVE_NOT_TRASHED, REASON_SEVERAL,
};
use crate::settings::{serve_pinboard, PinboardCall};
use crate::shell::{DesktopShell, ShellOutcome, Stopped};
use tairix_abi::window_ipc::DropOperation;
use tairix_controls::{ChainChild, ChainModel, ChainRow};
use tairix_window::DragConclusion;

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
    /// Whether a watch follows it.
    follows: bool,
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

    fn follows(&self, _components: &[String]) -> bool {
        self.0.borrow().follows
    }
}

/// A shared folder holding `entries`.
fn holding(entries: Vec<Entry>) -> Rc<RefCell<Folder>> {
    Rc::new(RefCell::new(Folder {
        answer: Some(Ok(Listing::Ready(entries))),
        listings: 0,
        follows: false,
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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

/// The ground around an icon's picture and name is the desktop's: a press
/// there is on no icon, though it lies inside the icon's cell.
#[test]
fn the_ground_in_an_icons_cell_is_the_desktops() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    desktop.press(
        centre_of(&layout, 0),
        tairix_input::Modifiers::default(),
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
    desktop.press(
        ground,
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        1,
        &[],
        &mut Region::new(),
    );
    assert_eq!(desktop.selected(), None);
}

/// What the session does with pointer motion over the desktop: it follows a
/// press still carrying an icon and nothing else, answering what that owes a
/// repaint.
fn moved_to(desktop: &mut Desktop<FakeDir>, at: Point, layout: &GridView) -> Region {
    damage_of(|damage| {
        let outcome = desktop.follow_press(at, false, layout, Scale::ONE, &theme(), damage);
        assert_eq!(outcome, DesktopOutcome::ignored(), "motion asks nothing");
    })
}

/// A pointer over an icon changes nothing about it — no plate, and the resting
/// ink and halo kept — so motion over the desktop repaints nothing, with an
/// icon selected and the keyboard on the desktop or without.
#[test]
fn moving_over_an_icon_draws_the_same_pixels_and_owes_no_damage() {
    let mut desktop = desktop_of(vec![folder("Work"), file("a.txt"), bundle("Chess.app")]);
    let layout = layout_of(&desktop);
    let theme = theme();
    let paint = |desktop: &Desktop<FakeDir>| {
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
        pixels_in(&surface, work_area())
    };
    let over = [
        centre_of(&layout, 0),
        centre_of(&layout, 1),
        centre_of(&layout, 2),
        EMPTY_DESKTOP,
    ];
    for focused in [false, true] {
        if focused {
            desktop.set_focused(true, &layout, &mut Region::new());
            desktop.key(down(), true, &layout, &[], &mut Region::new());
            assert!(desktop.selected().is_some(), "an icon is selected");
        }
        let still = paint(&desktop);
        for at in over {
            assert!(
                moved_to(&mut desktop, at, &layout).is_empty(),
                "{at:?} owes a repaint"
            );
            assert!(paint(&desktop) == still, "{at:?} drew something");
        }
    }
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
fn a_press_selects_an_icon_and_a_press_on_empty_desktop_clears_it() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let mut damage = Region::new();

    desktop.press(
        centre_of(&layout, 1),
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
                    tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
            tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        tairix_input::Modifiers::default(),
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
fn a_shortcut_is_named_in_the_desktop_folder_and_spelled_from_the_root_it_names() {
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
            target: "System:/Applications/games/chess.app".to_string(),
        },
        "the link is the entry's own name inside the desktop folder, and the \
         target is the bundle directory spelled from its root, which no walk follows"
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
            target: "System:/Applications/chess.app".to_string(),
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
        tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let acted = desktop.press(
        at,
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
                tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let slow = desktop.press(
        at,
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme,
        0,
        &[],
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
        tairix_input::Modifiers::default(),
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

/// The file manager's settings `in_effect` with the files apply `text` merged
/// over them, as the session admits one from Settings.
fn files_applied(in_effect: &FilerPreferences, text: &str) -> FilerPreferences {
    let request = PinboardRequest::ApplyFiles {
        document: PinboardDocument::new(text).expect("a bounded document"),
    };
    let mut frame = [0u8; PINBOARD_MAX_REQUEST];
    let len = request.encode(&mut frame).expect("encodes");
    let called = serve_pinboard(
        &DiscardSink,
        (1000, 1000),
        (&DesktopSettings::default(), in_effect, None),
        &frame[..len],
    );
    match called {
        Ok(PinboardCall::ApplyFiles(files)) => files,
        other => panic!("not a files apply: {other:?}"),
    }
}

/// The names on the desktop follow the endings a files apply from Settings
/// hides: an application without its `.app` by default, whole once that is
/// turned off and without it again once it is back on, each over a halo
/// blurred for the name shown.
#[test]
fn the_names_shown_follow_the_endings_a_files_apply_hides() {
    let ink_of = |desktop: &Desktop<FakeDir>| {
        let (surface, layout) = painted_over(desktop, None);
        name_ink(&surface, &layout, 0, &Theme::light())
    };
    let short = ink_of(&desktop_of(vec![file("Chess")]));
    let whole = ink_of(&desktop_of(vec![file("Chess.app")]));
    assert!(short != whole, "the two names draw apart");
    let mut desktop = desktop_of(vec![bundle("Chess.app")]);
    assert!(ink_of(&desktop) == short, "hidden by default");
    let mut files = FilerPreferences::default();
    for (text, shown) in [
        ("files.hide_app_endings = false\n", &whole),
        ("files.hide_app_endings = true\n", &short),
    ] {
        let was = desktop.halo_for_test("Chess.app").expect("kept");
        files = files_applied(&files, text);
        let endings = files.presentation.endings;
        assert!(desktop.hide_endings(endings), "{text:?}");
        assert!(ink_of(&desktop) == *shown, "{text:?}");
        let now = desktop.halo_for_test("Chess.app").expect("kept");
        assert!(!Arc::ptr_eq(&was, &now), "{text:?}: blurred anew");
        assert!(!desktop.hide_endings(endings), "{text:?}: again, nothing");
    }
}

/// A press meets an icon's picture whatever its name, and its name as it is
/// shown: where a hidden ending would have stood is the desktop's ground.
#[test]
fn a_press_meets_the_name_as_it_is_shown() {
    let theme = theme();
    let mut desktop = desktop_of(vec![bundle("Chess.app")]);
    let layout = layout_of(&desktop);
    let parts = |name: &str| {
        entry_parts(
            Tiles::Large,
            (&file(name), HiddenEndings::NONE),
            cell(&layout, 0),
            Scale::ONE,
            &theme,
        )
        .expect("the tile draws")
    };
    let hits = |desktop: &mut Desktop<FakeDir>, at: Point| {
        desktop.context_press(at, &layout, Scale::ONE, &theme, &mut Region::new())
    };
    let on_picture = parts("Chess.app").picture.expect("a picture").center();
    let whole = parts("Chess.app").name.expect("the whole name is drawn");
    let on_ending = Point::new(whole.right() - 1, whole.center().y);
    assert!(
        !parts("Chess").contains(on_ending),
        "beside the short name is ground"
    );
    for (endings, on_icon) in [
        (HiddenEndings::NONE, true),
        (FilerPreferences::default().presentation.endings, false),
    ] {
        assert!(desktop.hide_endings(endings), "{endings:?}");
        assert!(hits(&mut desktop, on_picture), "{endings:?}: its picture");
        assert_eq!(
            hits(&mut desktop, on_ending),
            on_icon,
            "{endings:?}: its ending"
        );
    }
}

/// The names the desktop draws icons for.
fn names<S: DirectorySource>(desktop: &Desktop<S>) -> Vec<&str> {
    desktop.entries().iter().map(Entry::name).collect()
}

/// An item whose name marks it hidden has no icon while the file manager's
/// settings leave hidden items out, as they do by default, so a capture's
/// scratch file is never drawn while it is written; leaving them out again
/// reads nothing.
#[test]
fn a_hidden_item_has_an_icon_only_while_the_settings_show_hidden_items() {
    let folder = holding(vec![file("a.txt"), file(".keeping.webm")]);
    let mut desktop = desktop_over(&folder);
    assert_eq!(names(&desktop), ["a.txt"]);
    assert!(!desktop.show_hidden_items(false), "left out already");

    assert!(desktop.show_hidden_items(true));
    let mut shown = names(&desktop);
    shown.sort_unstable();
    assert_eq!(shown, [".keeping.webm", "a.txt"]);

    let reads = listings(&folder);
    assert!(desktop.show_hidden_items(false));
    assert_eq!(names(&desktop), ["a.txt"]);
    assert_eq!(listings(&folder), reads, "leaving them out read the folder");
}

/// A watch's report of a hidden item adds no icon, and the rename that keeps
/// it under a name of its own adds one then: a recording appears once, kept.
#[test]
fn a_reported_hidden_item_has_no_icon_until_it_is_renamed_into_view() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    assert!(!desktop.apply_changes(
        vec![EntryChange::Upsert(file(".keeping.webm"))],
        layout_of,
        &mut Region::new(),
    ));
    assert_eq!(names(&desktop), ["a.txt"]);
    assert!(desktop.apply_changes(
        vec![
            EntryChange::Remove(String::from(".keeping.webm")),
            EntryChange::Upsert(file("Recording.webm")),
        ],
        layout_of,
        &mut Region::new(),
    ));
    let mut shown = names(&desktop);
    shown.sort_unstable();
    assert_eq!(shown, ["Recording.webm", "a.txt"]);
}

/// Showing hidden items reads nothing while no folder is shown yet or a
/// listing is on its way: that listing is adopted as the setting then says.
#[test]
fn showing_hidden_items_adopts_a_listing_on_its_way_without_reading_again() {
    let worker = Rc::new(RefCell::new(Worker::default()));
    let mut desktop = Desktop::new(DeferredDir(Rc::clone(&worker)), home());
    assert!(!desktop.show_hidden_items(true), "nothing listed yet");
    assert_eq!(worker.borrow().reads, 0);
    assert!(!desktop.show_hidden_items(false));

    desktop.relist();
    assert!(!desktop.show_hidden_items(true));
    land(&worker, vec![file("a.txt"), file(".profile")]);
    assert!(desktop.resume());
    assert_eq!(desktop.entries().len(), 2, "the hidden item is shown");
    assert_eq!(worker.borrow().reads, 1, "showing hidden items read again");
}

/// Leaving hidden items out forgets none of their places: shown again, a
/// hidden item stands where the user put it.
#[test]
fn a_hidden_item_left_out_keeps_its_place_for_when_it_is_shown_again() {
    let mut desktop = placed_on(
        vec![file("a.txt"), file(".notes")],
        &[(".notes", cell_place(3, 2))],
    );
    assert_eq!(names(&desktop), ["a.txt"]);
    assert!(desktop.show_hidden_items(true));
    let layout = layout_of(&desktop);
    let put = bounds(&desktop, &layout, named(&desktop, ".notes"));
    assert_ne!(
        put,
        cell(&layout, 1),
        "it stands where it was put, not flowed"
    );

    assert!(desktop.show_hidden_items(false));
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "a place was forgotten"
    );
    assert!(desktop.show_hidden_items(true));
    assert_eq!(bounds(&desktop, &layout, named(&desktop, ".notes")), put);
}

/// The settled witness is given once a revealed desktop's icons are drawn as
/// they will stay, once for each arrangement of them, and never for a folder
/// not yet read or with a listing on its way.
#[test]
fn the_settled_witness_is_given_once_for_each_arrangement_drawn_as_kept() {
    let worker = Rc::new(RefCell::new(Worker::default()));
    let mut desktop = Desktop::new(DeferredDir(Rc::clone(&worker)), home());
    let reports = Cell::new(0);
    let report = || reports.set(reports.get() + 1);
    desktop.report_settled(true, |_| true, report);
    assert_eq!(reports.get(), 0, "nothing read yet");
    desktop.relist();
    desktop.report_settled(true, |_| true, report);
    assert_eq!(reports.get(), 0, "a listing still on its way");
    land(&worker, vec![file("a.txt")]);
    assert!(desktop.resume());
    desktop.report_settled(false, |_| true, report);
    assert_eq!(reports.get(), 0, "not yet revealed");
    desktop.report_settled(true, |_| false, report);
    assert_eq!(reports.get(), 0, "an icon's picture still coming");
    desktop.report_settled(true, |_| true, report);
    assert_eq!(reports.get(), 1);
    desktop.report_settled(true, |_| panic!("asked with nothing owed"), report);
    assert_eq!(reports.get(), 1, "once for this arrangement");
    assert!(desktop.apply_changes(
        vec![EntryChange::Upsert(file("b.txt"))],
        layout_of,
        &mut Region::new(),
    ));
    desktop.report_settled(true, |_| true, report);
    assert_eq!(reports.get(), 2, "and again for the next");
}

/// A hidden item deleted while left out gives up the place the record kept
/// for it, as an icon's file does.
#[test]
fn a_hidden_item_deleted_while_left_out_gives_up_its_place() {
    let mut desktop = placed_on(
        vec![file("a.txt"), file(".notes")],
        &[(".notes", cell_place(3, 2))],
    );
    assert!(!desktop.apply_changes(
        vec![EntryChange::Remove(String::from(".notes"))],
        layout_of,
        &mut Region::new(),
    ));
    assert_eq!(
        desktop.take_arrangement_edits(),
        [ArrangementEdit::Set(vec![(String::from(".notes"), None)])]
    );
}

/// An icon renamed to a hidden name keeps its place under that name, and
/// stands there once hidden items are shown.
#[test]
fn an_icon_renamed_to_a_hidden_name_keeps_its_place_set_aside() {
    let id = FileId {
        volume: [3; 16],
        node: 42,
    };
    let mut desktop = desktop_of(vec![file("a.txt").with_id(id), file("b.txt")]);
    let layout = layout_of(&desktop);
    carry(&mut desktop, &layout, 0, (300, 200));
    let put = bounds(&desktop, &layout, 0);
    let _ = desktop.take_arrangement_edits();
    assert!(desktop.apply_changes(
        vec![
            EntryChange::Remove(String::from("a.txt")),
            EntryChange::Upsert(file(".a.txt").with_id(id)),
        ],
        layout_of,
        &mut Region::new(),
    ));
    assert_eq!(names(&desktop), ["b.txt"]);
    assert_eq!(
        desktop.take_arrangement_edits(),
        [ArrangementEdit::Renamed {
            from: PlaceKey::of("a.txt"),
            to: String::from(".a.txt"),
            id,
        }]
    );
    assert!(desktop.show_hidden_items(true));
    assert_eq!(bounds(&desktop, &layout, named(&desktop, ".a.txt")), put);
}

/// A change of the endings shown re-lays the desktop: the icons no one placed
/// flow again, closing the gap a deleted file left, and the selection stays on
/// its icon.
#[test]
fn a_change_of_endings_flows_the_icons_again_and_keeps_the_selection() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt"), file("c.txt")]);
    let layout = layout_of(&desktop);
    press_at(&mut desktop, &layout, centre_of(&layout, 2), 0);
    desktop.apply_changes(
        vec![EntryChange::Remove(String::from("b.txt"))],
        layout_of,
        &mut Region::new(),
    );
    let c_txt = named(&desktop, "c.txt");
    assert_eq!(
        bounds(&desktop, &layout, c_txt),
        cell(&layout, 2),
        "a deleted file leaves its cell free"
    );
    assert!(desktop.hide_endings(HiddenEndings::NONE));
    let c_txt = named(&desktop, "c.txt");
    assert_eq!(
        bounds(&desktop, &layout, c_txt),
        cell(&layout, 1),
        "and the icons flow again"
    );
    assert_eq!(
        desktop.selected(),
        Some(c_txt),
        "the selection stays on c.txt"
    );
    assert!(
        !desktop.hide_endings(HiddenEndings::NONE),
        "the endings in force change nothing"
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
            tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );

    desktop
        .apply_settings(arranged_by(IconFlow::default(), IconSort::Size))
        .expect("the order changed");
    let names: Vec<&str> = desktop.entries().iter().map(Entry::name).collect();
    assert_eq!(names, ["c.txt", "b.txt", "a.txt"]);
    assert_eq!(listings(&folder), 1, "the icons shown are the folder's");
    assert_eq!(desktop.selected(), Some(2), "the selection stays on a.txt");
    let acted = desktop.press(
        centre_of(&layout, 0),
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
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
        tairix_input::Modifiers::default(),
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let clicked = desktop.press(
        at,
        tairix_input::Modifiers::default(),
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
    let model = pinboard::model(Over::Icon, &arranged_by(IconFlow::Trailing, IconSort::Size));

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
    let model = pinboard::model(DRIVE, &DesktopSettings::default());
    // A row opened for its field answers as the field commits.
    let commands: Vec<PinboardCommand> = model
        .rows()
        .iter()
        .filter_map(|row| match row.child() {
            ChainChild::Entry { id, .. } => Some(*id),
            _ => row.id(),
        })
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
    let bare = pinboard::model(Over::Backdrop, &DesktopSettings::default());
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
        PinboardCommand::ALL.len() - 4,
        "only the rows about an icon are left out"
    );

    let over_icon = pinboard::model(Over::Icon, &DesktopSettings::default());
    assert!(
        over_icon.rows()[1].drawn().is_group_break(),
        "with Open above it, New Folder starts its own group"
    );

    // `New Folder` is `ALL` position 4, so its id is 5 whichever gesture built
    // the plate — the row it sits at moved, its command did not.
    let id = AppMenuItemId::new(5).expect("a non-zero row id");
    assert_eq!(
        PinboardCommand::from_item(id),
        Some(PinboardCommand::NewFolder)
    );
}

/// Every group break the plate draws, so a divider cannot drift onto a row
/// that begins nothing.
#[test]
fn the_menu_groups_its_rows_by_what_they_are() {
    let model = pinboard::model(Over::Icon, &DesktopSettings::default());
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
    for over in [Over::Backdrop, Over::Icon, DRIVE] {
        let model = pinboard::model(over, &DesktopSettings::default());
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
                ChainChild::Entry { id, .. } => assert_eq!(
                    PinboardCommand::from_item(*id),
                    Some(PinboardCommand::Title),
                    "{label:?} answers as its field commits"
                ),
                other @ ChainChild::Info(_) => {
                    panic!("the backdrop menu hangs no window: {other:?}")
                }
            }
        }
    }
}

/// Tidy Up's three orders sit on a plate of their own, each naming its own
/// command.
#[test]
fn tidy_up_files_its_orders_on_a_plate_of_their_own() {
    let model = pinboard::model(Over::Backdrop, &DesktopSettings::default());
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
        tairix_input::Modifiers::default(),
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

/// Carry the icon at `index` by its centre `by` pixels, still held there,
/// answering where the pointer holds it.
fn carry_held(
    desktop: &mut Desktop<FakeDir>,
    layout: &GridView,
    index: usize,
    by: (i32, i32),
) -> Point {
    let from = bounds(desktop, layout, index).center();
    press_at(desktop, layout, from, 0);
    let to = Point::new(from.x + by.0, from.y + by.1);
    desktop.follow_press(to, true, layout, Scale::ONE, &theme(), &mut Region::new());
    to
}

/// Carry the icon at `index` by its centre `by` pixels and let it go there,
/// answering what putting it down asked for.
fn carry(
    desktop: &mut Desktop<FakeDir>,
    layout: &GridView,
    index: usize,
    by: (i32, i32),
) -> DesktopOutcome {
    let to = carry_held(desktop, layout, index, by);
    desktop.follow_press(to, false, layout, Scale::ONE, &theme(), &mut Region::new())
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
            &theme(),
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

/// Icons carried over a window are described for the seat by their paths, and
/// once lent to it they leave the desktop's layer until the drag ends: no
/// picture is left where they stood, nothing here follows the pointer, and a
/// drag that ends elsewhere has them stand where they stood again.
#[test]
fn icons_lent_to_the_seat_leave_the_desktop_until_the_drag_ends() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let rest = bounds(&desktop, &layout, 0);
    assert!(
        !desktop.carries_to(rest.center(), Scale::ONE),
        "no press, no drag"
    );
    let to = carry_held(&mut desktop, &layout, 0, (300, 200));
    assert!(desktop.carries_to(to, Scale::ONE));
    let cargo = desktop.carried_cargo(to, &layout).expect("described");
    let paths: Vec<&str> = cargo.items.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(paths, ["/Users/ada/Desktop/a.txt"]);
    assert_eq!(cargo.shown.count(), 1);
    assert_eq!(cargo.shown.first().as_str(), "a.txt");
    let carried = bounds(&desktop, &layout, 0);
    let lent = damage_of(|damage| desktop.lend_carry(&layout, damage));
    assert!(lent.intersects(carried), "the ground it covered");
    assert!(!lent.intersects(rest), "and no picture where it stood");
    assert_eq!(
        desktop.bounds_for_test(&layout, 0),
        None,
        "drawn nowhere on the desktop while the seat carries it"
    );
    assert_eq!(bounds(&desktop, &layout, 1), cell(&layout, 1));
    assert!(
        !desktop.carries_to(to, Scale::ONE),
        "the seat carries it now"
    );
    assert!(desktop.carried_cargo(to, &layout).is_none());
    let later = Point::new(to.x + 50, to.y + 30);
    assert!(
        damage_of(|damage| {
            desktop.follow_press(later, false, &layout, Scale::ONE, &theme(), damage);
        })
        .is_empty(),
        "nothing here follows the pointer"
    );
    let back = damage_of(|damage| desktop.restore_lent(&layout, damage));
    assert!(back.intersects(rest));
    assert_eq!(bounds(&desktop, &layout, 0), rest, "back where it stood");
    assert!(desktop.take_arrangement_edits().is_empty());
}

/// Let go over a window in the batch that reached it, a carried press is let
/// go: its icons stand where they stood.
#[test]
fn a_carry_let_go_before_the_seat_takes_it_stands_where_it_stood() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let rest = bounds(&desktop, &layout, 0);
    carry_held(&mut desktop, &layout, 0, (300, 200));
    let carried = bounds(&desktop, &layout, 0);
    let cancelled = damage_of(|damage| desktop.cancel_carry(&layout, damage));
    assert!(cancelled.intersects(carried) && cancelled.intersects(rest));
    assert_eq!(bounds(&desktop, &layout, 0), rest);
    assert!(desktop.take_arrangement_edits().is_empty());
}

/// A press on the desktop proves the seat carries none of its icons, so one
/// still lent stands where it stood again.
#[test]
fn a_press_brings_back_icons_still_lent() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let rest = bounds(&desktop, &layout, 0);
    carry_held(&mut desktop, &layout, 0, (300, 200));
    desktop.lend_carry(&layout, &mut Region::new());
    let elsewhere = cell(&layout, 1).center();
    let pressed = damage_of(|damage| {
        desktop.press(
            elsewhere,
            tairix_input::Modifiers::default(),
            &layout,
            Scale::ONE,
            &theme(),
            0,
            &[],
            damage,
        );
    });
    assert!(pressed.intersects(rest));
    assert_eq!(bounds(&desktop, &layout, 0), rest);
}

/// A press still within the drag slop is not yet a drag, so it is never
/// handed over.
#[test]
fn a_press_within_the_slop_is_not_handed_over() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let at = cell(&layout, 0).center();
    press_at(&mut desktop, &layout, at, 0);
    let still = Point::new(at.x + 1, at.y + 1);
    assert!(!desktop.carries_to(still, Scale::ONE));
    desktop.follow_press(
        still,
        true,
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );
    assert!(desktop.carried_cargo(still, &layout).is_none());
    assert!(desktop.carries_to(Point::new(at.x + 300, at.y), Scale::ONE));
}

/// Several icons are described together, the pressed one first.
#[test]
fn a_selection_is_handed_over_with_the_pressed_icon_first() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt"), file("c.txt")]);
    let layout = layout_of(&desktop);
    let first = bounds(&desktop, &layout, 0).center();
    press_at(&mut desktop, &layout, first, 0);
    let from = bounds(&desktop, &layout, 2).center();
    desktop.press(
        from,
        tairix_input::Modifiers {
            shift: true,
            ..tairix_input::Modifiers::default()
        },
        &layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    let to = Point::new(from.x + 300, from.y + 200);
    desktop.follow_press(to, true, &layout, Scale::ONE, &theme(), &mut Region::new());
    let cargo = desktop.carried_cargo(to, &layout).expect("described");
    let paths: Vec<&str> = cargo.items.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(
        paths,
        ["/Users/ada/Desktop/c.txt", "/Users/ada/Desktop/a.txt"]
    );
    assert_eq!(cargo.shown.count(), 2);
    assert_eq!(cargo.shown.first().as_str(), "c.txt");
}

/// Brought back and let go on the desktop, lent icons are put down exactly as
/// a press carried to that point would put them, and kept there.
#[test]
fn lent_icons_brought_back_are_put_down_where_let_go() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let was = bounds(&desktop, &layout, 0);
    carry_held(&mut desktop, &layout, 0, (300, 200));
    desktop.lend_carry(&layout, &mut Region::new());
    let at = Point::new(was.center().x + 237, was.center().y + 101);
    assert_eq!(
        desktop.put_down_lent(at, &layout, &mut Region::new()),
        DesktopOutcome::ignored()
    );
    let now = bounds(&desktop, &layout, 0);
    assert_eq!(
        (now.left() - was.left(), now.top() - was.top()),
        (237, 101),
        "exactly where it was let go"
    );
    assert_eq!(bounds(&desktop, &layout, 1), cell(&layout, 1));
    assert_eq!(desktop.take_arrangement_edits().len(), 1, "and kept there");
}

/// Icons brought back from a lent drag are repainted where they are put
/// down: the desktop drew them nowhere while they were lent, and the seat's
/// picture of them has gone.
#[test]
fn lent_icons_put_down_are_repainted_where_let_go() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let was = bounds(&desktop, &layout, 0);
    carry_held(&mut desktop, &layout, 0, (300, 200));
    desktop.lend_carry(&layout, &mut Region::new());
    let at = Point::new(was.center().x + 237, was.center().y + 101);
    let damage = damage_of(|damage| {
        assert_eq!(
            desktop.put_down_lent(at, &layout, damage),
            DesktopOutcome::ignored()
        );
    });
    let mut unpainted = Region::new();
    unpainted.add(bounds(&desktop, &layout, 0));
    unpainted.subtract_region(&damage);
    assert!(unpainted.is_empty(), "the icon's new place is owed a paint");
}

/// A lent press follows its icon through a change to the folder, stays off
/// the desktop meanwhile, and puts down the icon carried and moves no other.
#[test]
fn a_lent_press_follows_its_icon_through_a_change_to_the_folder() {
    let mut desktop = desktop_of(vec![file("b.txt"), file("c.txt")]);
    let layout = layout_of(&desktop);
    let was = bounds(&desktop, &layout, 1);
    carry_held(&mut desktop, &layout, 1, (300, 200));
    desktop.lend_carry(&layout, &mut Region::new());
    assert!(desktop.apply_changes(
        vec![EntryChange::Upsert(file("a.txt"))],
        layout_of,
        &mut Region::new()
    ));
    let layout = layout_of(&desktop);
    let carried = named(&desktop, "c.txt");
    assert_eq!(carried, 2, "listed after the arrival");
    assert_eq!(
        desktop.bounds_for_test(&layout, carried),
        None,
        "still off the desktop"
    );
    let others: Vec<Rect> = [named(&desktop, "a.txt"), named(&desktop, "b.txt")]
        .iter()
        .map(|&index| bounds(&desktop, &layout, index))
        .collect();
    let at = Point::new(was.center().x + 120, was.center().y + 80);
    let _ = desktop.put_down_lent(at, &layout, &mut Region::new());
    let now = bounds(&desktop, &layout, carried);
    assert_eq!((now.left() - was.left(), now.top() - was.top()), (120, 80));
    let after: Vec<Rect> = [named(&desktop, "a.txt"), named(&desktop, "b.txt")]
        .iter()
        .map(|&index| bounds(&desktop, &layout, index))
        .collect();
    assert_eq!(after, others, "no other icon moved");
}

/// A lent press whose icon has gone puts nothing down.
#[test]
fn a_lent_press_whose_icon_went_puts_nothing_down() {
    let mut desktop = desktop_of(vec![file("a.txt"), file("b.txt")]);
    let layout = layout_of(&desktop);
    let to = carry_held(&mut desktop, &layout, 0, (300, 200));
    desktop.lend_carry(&layout, &mut Region::new());
    assert!(desktop.apply_changes(
        vec![EntryChange::Remove(String::from("a.txt"))],
        layout_of,
        &mut Region::new()
    ));
    let layout = layout_of(&desktop);
    let rest = bounds(&desktop, &layout, 0);
    assert_eq!(
        desktop.put_down_lent(to, &layout, &mut Region::new()),
        DesktopOutcome::ignored()
    );
    assert_eq!(bounds(&desktop, &layout, 0), rest, "b.txt stays");
    assert!(desktop.take_arrangement_edits().is_empty());
}

#[test]
fn a_press_that_never_travels_is_a_click_and_moves_nothing() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let at = cell(&layout, 0).center();
    press_at(&mut desktop, &layout, at, 0);
    let still = Point::new(at.x + 1, at.y + 1);
    let held = damage_of(|damage| {
        desktop.follow_press(still, true, &layout, Scale::ONE, &theme(), damage);
    });
    assert!(held.is_empty(), "a hand holding still is not a drag");
    assert_eq!(
        desktop.follow_press(
            still,
            false,
            &layout,
            Scale::ONE,
            &theme(),
            &mut Region::new()
        ),
        DesktopOutcome::ignored()
    );
    let later = Point::new(at.x + 300, at.y + 200);
    assert!(
        damage_of(|damage| {
            desktop.follow_press(later, true, &layout, Scale::ONE, &theme(), damage);
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

/// The drop the tests' drags are concluded as: window 9's, at report 3.
const SOURCE: (u64, u32) = (9, 3);

/// A drag of `count` items led by `lead`, those shown held `from` the press.
fn items_held(lead: &str, count: u32, from: &[(i32, i32)]) -> DragItems {
    let pictures = from.iter().map(|&from| DragPicture {
        icon: IconAsset::new("text").expect("a valid asset id"),
        from: Some(from),
        own: None,
    });
    DragItems::new(
        DocumentName::new(lead).expect("a valid name"),
        count,
        false,
        pictures,
    )
    .expect("items")
}

/// Let `items` go at `drop` over the bare desktop, as [`SOURCE`]'s drop.
fn expect_drop(desktop: &mut Desktop<FakeDir>, items: &DragItems, drop: Point) {
    let layout = layout_of(desktop);
    let folder = desktop.folder_path();
    desktop.offer_landing(Some(LandingOffer::of(items, folder, true)));
    desktop.expect_drop(SOURCE, drop, &layout, Scale::ONE, &theme());
}

/// Take `step` of [`SOURCE`]'s drop, answering what it asked of the embedder.
fn take_step(desktop: &mut Desktop<FakeDir>, step: &LandingStep) -> LandingWork {
    desktop.step_landing(SOURCE, step, layout_of, &mut Region::new())
}

/// Item `item` about to be written as `name`.
fn making(item: u32, name: &str) -> LandingStep {
    LandingStep::Making {
        item,
        name: LandingName::new(name).expect("a valid name"),
    }
}

/// Item `item` done, as `outcome` says.
const fn made(item: u32, outcome: LandingOutcome) -> LandingStep {
    LandingStep::Made { item, outcome }
}

/// `names` arrive in the folder, as its watch reports them.
fn arrive(desktop: &mut Desktop<FakeDir>, names: &[&str]) {
    desktop.apply_changes(
        names
            .iter()
            .map(|name| EntryChange::Upsert(file(name)))
            .collect(),
        layout_of,
        &mut Region::new(),
    );
}

/// Make item `item` as `name`, its icon arriving while it is written.
fn make_arriving(desktop: &mut Desktop<FakeDir>, item: u32, name: &str) {
    take_step(desktop, &making(item, name));
    arrive(desktop, &[name]);
    take_step(desktop, &made(item, LandingOutcome::Created));
}

/// An arrival a drop's source did not announce is not taken for its item,
/// however like it it is named; the item announced stands where it was let
/// go from the moment it appears.
#[test]
fn a_name_its_source_did_not_announce_is_not_taken_for_its_drop() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_drop(&mut desktop, &items_held("x.txt", 1, &[(0, 0)]), drop);
    take_step(&mut desktop, &making(0, "x.txt"));
    arrive(&mut desktop, &["x (2).txt"]);
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "x (2).txt")),
        cell(&layout, 0),
        "a name it did not announce flows"
    );
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "and nothing is kept for it"
    );
    arrive(&mut desktop, &["x.txt"]);
    assert!(
        bounds(&desktop, &layout, named(&desktop, "x.txt")).contains(drop),
        "the one it announced stands where it was let go"
    );
}

/// An arrival during a drop of several items that is none of them flows as
/// any other does.
#[test]
fn an_unrelated_arrival_during_a_drop_of_several_is_not_placed() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let several = items_held("one.txt", 2, &[(0, 0), (150, 40)]);
    expect_drop(&mut desktop, &several, Point::new(500, 400));
    take_step(&mut desktop, &making(0, "one.txt"));
    arrive(&mut desktop, &["other.txt"]);
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "other.txt")),
        cell(&layout, 0)
    );
    assert!(desktop.take_arrangement_edits().is_empty());
}

/// An item listed long after it was made, after its drop ended, still stands
/// where it was let go: a drop waits for its source's steps, not a clock.
#[test]
fn an_item_made_long_after_its_drop_still_lands_where_it_was_let_go() {
    let folder = holding(Vec::new());
    folder.borrow_mut().follows = true;
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_drop(&mut desktop, &items_held("notes.txt", 1, &[(0, 0)]), drop);
    take_step(&mut desktop, &making(0, "notes.txt"));
    take_step(&mut desktop, &made(0, LandingOutcome::Created));
    take_step(&mut desktop, &LandingStep::Ended);
    assert_eq!(
        listings(&folder),
        1,
        "a folder its watch follows is not read"
    );
    arrive(&mut desktop, &["notes.txt"]);
    assert!(
        bounds(&desktop, &layout, named(&desktop, "notes.txt")).contains(drop),
        "listed after its drop ended"
    );
    assert!(
        matches!(
            desktop.take_arrangement_edits().as_slice(),
            [ArrangementEdit::Set(set)] if set.len() == 1
        ),
        "and kept there"
    );
}

/// A replaced item moves to where the drop was let go; a skipped, merged or
/// failed one moves nothing.
#[test]
fn a_replaced_item_moves_to_its_drop_and_a_skipped_one_stays() {
    let names = ["a.txt", "b.txt", "c.txt", "d.txt"];
    let mut desktop = desktop_of(names.iter().map(|name| file(name)).collect());
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_drop(&mut desktop, &items_held("a.txt", 4, &[(0, 0); 4]), drop);
    take_step(&mut desktop, &making(0, "a.txt"));
    take_step(&mut desktop, &made(0, LandingOutcome::Replaced));
    assert!(
        bounds(&desktop, &layout, named(&desktop, "a.txt")).contains(drop),
        "a replaced item moves to its drop"
    );
    for (item, name, outcome) in [
        (1, "b.txt", LandingOutcome::Skipped),
        (2, "c.txt", LandingOutcome::Merged),
        (3, "d.txt", LandingOutcome::Failed),
    ] {
        let stood = bounds(&desktop, &layout, named(&desktop, name));
        take_step(&mut desktop, &making(item, name));
        take_step(&mut desktop, &made(item, outcome));
        assert_eq!(
            bounds(&desktop, &layout, named(&desktop, name)),
            stood,
            "{name} moves nothing"
        );
    }
}

/// An item a drag carried past those it showed takes the free cell nearest
/// where the drop was let go, never an arbitrary one.
#[test]
fn an_item_past_those_shown_takes_the_free_cell_nearest_its_drop() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let far = [(-2000, -2000), (2000, 2000), (-2000, 2000), (2000, -2000)];
    expect_drop(
        &mut desktop,
        &items_held("0.txt", 5, &far),
        centre_of(&layout, 7),
    );
    for item in 0..5 {
        make_arriving(&mut desktop, item, &format!("{item}.txt"));
    }
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "4.txt")),
        cell(&layout, 7)
    );
}

#[test]
fn each_item_a_drop_carried_lands_as_far_from_the_drop_as_it_was_held() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    let two = items_held("one.txt", 2, &[(0, 0), (150, 40)]);
    expect_drop(&mut desktop, &two, Point::new(300, 300));
    make_arriving(&mut desktop, 0, "one.txt");
    make_arriving(&mut desktop, 1, "two.txt");
    let (one, two) = (
        bounds(&desktop, &layout, named(&desktop, "one.txt")),
        bounds(&desktop, &layout, named(&desktop, "two.txt")),
    );
    assert_eq!((two.left() - one.left(), two.top() - one.top()), (150, 40));
}

/// A drop that moved on from the desktop before it was let go lands nothing.
#[test]
fn a_drop_with_no_offer_standing_lands_nothing() {
    let mut desktop = desktop_of(Vec::new());
    let layout = layout_of(&desktop);
    desktop.offer_landing(None);
    desktop.expect_drop(SOURCE, Point::new(300, 300), &layout, Scale::ONE, &theme());
    make_arriving(&mut desktop, 0, "one.txt");
    assert_eq!(bounds(&desktop, &layout, 0), cell(&layout, 0));
    assert!(desktop.take_arrangement_edits().is_empty());
}

/// A drop into a folder no watch covers shows what it made once its source
/// ends it: the folder is read then, once, and each item stands where it was
/// let go.
#[test]
fn a_folder_no_watch_covers_is_read_when_its_drop_ends() {
    let folder = holding(Vec::new());
    let mut desktop = desktop_over(&folder);
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    expect_drop(&mut desktop, &items_held("notes.txt", 1, &[(0, 0)]), drop);
    take_step(&mut desktop, &making(0, "notes.txt"));
    take_step(&mut desktop, &made(0, LandingOutcome::Created));
    folder.borrow_mut().answer = Some(Ok(Listing::Ready(vec![file("notes.txt")])));
    let read = listings(&folder);
    take_step(&mut desktop, &LandingStep::Ended);
    assert_eq!(listings(&folder), read + 1, "read as the drop ends");
    assert!(bounds(&desktop, &layout, named(&desktop, "notes.txt")).contains(drop));
}

/// What a drop writes is held from its first write, pictured again once it is
/// made or once the drop ends with it unfinished, and nothing is taken after
/// the end.
#[test]
fn what_a_drop_writes_is_held_until_it_is_done_or_the_drop_ends() {
    let mut desktop = desktop_of(Vec::new());
    let two = items_held("App.app", 2, &[(0, 0), (0, 0)]);
    expect_drop(&mut desktop, &two, Point::new(300, 300));
    let path = |name: &str| Some(format!("/Users/ada/Desktop/{name}"));
    let work = take_step(&mut desktop, &making(0, "App.app"));
    assert_eq!((work.hold, work.release), (path("App.app"), None));
    let work = take_step(&mut desktop, &made(0, LandingOutcome::Created));
    assert_eq!((work.hold, work.release), (None, path("App.app")));
    take_step(&mut desktop, &making(1, "Tool.app"));
    let work = take_step(&mut desktop, &LandingStep::Ended);
    assert_eq!(
        (work.hold, work.release),
        (None, path("Tool.app")),
        "an unfinished item is let go with its drop"
    );
    assert_eq!(
        take_step(&mut desktop, &making(0, "App.app")),
        LandingWork::default(),
        "nothing is taken after the end"
    );
}

/// An item a drop writes is drawn, while its own picture is produced, by the
/// one its drag carried — an application's and a command's own icon, which
/// the session already reads for the drag — rather than its glyph while the
/// copy is held and read.
#[test]
fn a_dropped_bundle_stands_in_with_the_picture_its_drag_carried() {
    for (source, name) in [
        ("/System/Applications/Paint.app", "Paint.app"),
        ("/System/Commands/ls.app", "ls.app"),
    ] {
        let mut desktop = desktop_of(Vec::new());
        let own = DragOwnIcon::new(DragOwnSource::Bundle, source, None).expect("an own picture");
        let picture = DragPicture {
            icon: IconAsset::new(IconKind::AppBundle.asset_id()).expect("an asset id"),
            from: Some((0, 0)),
            own: Some(own),
        };
        let first = DocumentName::new(name).expect("a valid name");
        let items = DragItems::new(first, 1, false, [picture]).expect("one bundle");
        expect_drop(&mut desktop, &items, Point::new(300, 300));
        take_step(&mut desktop, &making(0, name));
        let made = bundle(name).with_id(FileId {
            volume: [1; 16],
            node: 7,
        });
        let arrived = vec![EntryChange::Upsert(made.clone())];
        desktop.apply_changes(arrived, layout_of, &mut Region::new());
        let path = format!("/Users/ada/Desktop/{name}");
        let carried = IconRequest::from_carried(IconKind::AppBundle, Some(&own));
        let mut asked = Vec::new();
        desktop.want_shown_artwork(&layout_of(&desktop), Scale::ONE, &theme(), |request, _| {
            let listed = IconRequest::listed_bundle(request.icon_kind(), &path, made.stamp());
            asked.push(request == listed.standing_in(carried));
        });
        assert_eq!(asked, [true], "{name}");
    }
}

/// A shortcut to an application is drawn by the application's own icon,
/// asked for where its activation reaches it: its target names another root,
/// which no walk through the shortcut follows.
#[test]
fn a_shortcut_to_an_application_draws_the_application_s_icon() {
    let desktop = desktop_of(vec![link(
        "Editor",
        LinkTarget::Bundle,
        "System:/Applications/Editor.app",
    )]);
    let mut asked = Vec::new();
    desktop.want_shown_artwork(&layout_of(&desktop), Scale::ONE, &theme(), |request, _| {
        let target = IconRequest::bundle(request.icon_kind(), "/System/Applications/Editor.app");
        asked.push(request == target);
    });
    assert_eq!(asked, [true]);
}

/// A save dropped on the bare desktop stands under the pointer it was let go
/// at, under the exact name the session's own open made it as.
#[test]
fn a_saved_file_stands_where_its_save_was_let_go() {
    for made in ["Letter.txt", "Letter,fff"] {
        let mut desktop = desktop_of(vec![file("a.txt")]);
        let layout = layout_of(&desktop);
        let drop = Point::new(500, 400);
        desktop.expect_save((1, 2), "Letter", drop, (&layout, Scale::ONE, &theme()));
        let path = format!("/Users/ada/Desktop/{made}");
        assert!(
            !desktop.saved((1, 2), Some(path.as_str()), layout_of, &mut Region::new()),
            "owed until it is listed"
        );
        arrive(&mut desktop, &[made]);
        assert!(
            bounds(&desktop, &layout, named(&desktop, made)).contains(drop),
            "{made} stands where it was let go"
        );
    }
}

/// A change made in Settings lays the desktop out again — its scale, here —
/// and nothing settles it there until a drag's first report over it lights
/// the drop target, which moves its revision. The drag, still on the bare
/// desktop, owes its source no second report: one would have taken the
/// folder held for the first, whose answer is the one that decides the drop.
#[test]
fn a_drag_on_the_desktop_settings_laid_out_again_is_asked_once() {
    let mut desktop = desktop_of(Vec::new());
    desktop.set_focused(false, &layout_of(&desktop), &mut Region::new());
    let mut shell = crate::tests::shell();
    let mut comp = crate::tests::compositor();
    let window = crate::tests::opaque_window(&mut comp, Point::new(100, 100), 120, 80);
    let _ = shell.handle(crate::tests::moved(150, 140), &mut comp, 0);
    let _ = shell.handle(crate::tests::PRIMARY_PRESS, &mut comp, 0);
    let items = items_held("a.txt", 1, &[(0, 0)]);
    shell
        .begin_drag(&mut comp, 7, window, items)
        .expect("carried");
    let on_desktop = |desktop: &Desktop<FakeDir>| DragSurface::Desktop {
        folder: desktop.folder_path(),
        icon: None,
        revision: desktop.revision(),
    };
    shell.drag_over(&mut comp, on_desktop(&desktop));
    let Some(OwedReport::Items(first)) = shell.take_drag_report() else {
        panic!("the first report is owed");
    };
    let scaled = Scale::from_percent(150).expect("a scale Settings offers");
    let relaid = desktop.layout(work_area(), scaled, &theme());
    let settled = desktop.revision();
    desktop.set_drop_target(shell.drag_drop_icon(), &relaid, &mut Region::new());
    assert_ne!(
        desktop.revision(),
        settled,
        "settled where Settings laid it out"
    );
    shell.drag_over(&mut comp, on_desktop(&desktop));
    assert_eq!(
        shell.take_drag_report(),
        None,
        "the folder is asked about once"
    );
    assert!(shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    let release = InputEvent::PointerReleased {
        button: tairix_wm::PointerButton::Primary,
    };
    assert_eq!(
        shell.drag_pointer(&mut comp, &release),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Desktop {
                serial: first.serial,
                operation: DropOperation::Copy,
            },
            refused: None,
            offer: None,
        }))
    );
}

/// A save whose open made nothing, or made its file elsewhere, stands
/// nothing.
#[test]
fn a_save_that_made_nothing_here_places_nothing() {
    let mut desktop = desktop_of(vec![file("a.txt")]);
    let layout = layout_of(&desktop);
    let drop = Point::new(500, 400);
    let look = (&layout, Scale::ONE, &theme());
    desktop.expect_save((1, 2), "Letter.txt", drop, look);
    assert!(!desktop.saved((1, 2), None, layout_of, &mut Region::new()));
    desktop.expect_save((1, 3), "Letter.txt", drop, look);
    let elsewhere = Some("/Users/ada/Documents/Letter.txt");
    assert!(!desktop.saved((1, 3), elsewhere, layout_of, &mut Region::new()));
    arrive(&mut desktop, &["Letter.txt"]);
    assert_eq!(
        bounds(&desktop, &layout, named(&desktop, "Letter.txt")),
        cell(&layout, 1)
    );
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
            desktop.follow_press(
                pointer,
                held,
                &layout,
                comp.scale(),
                &theme(),
                &mut Region::new(),
            );
            if outcome.value == ShellOutcome::WindowManager(InputResponse::DesktopPressed) {
                desktop.press(
                    pointer,
                    tairix_input::Modifiers::default(),
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
    let ends = [
        (i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX),
        (i32::MIN, i32::MAX),
        (i32::MAX, i32::MIN),
    ];
    expect_drop(
        &mut desktop,
        &items_held("0.txt", 4, &ends),
        Point::new(300, 300),
    );
    for n in 0..4 {
        make_arriving(&mut desktop, n, &format!("{n}.txt"));
    }
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

/// The identity the stick's content carries.
const STICK: [u8; 16] = [7; 16];

/// The menu over a drive its viewer may eject and retitle.
const DRIVE: Over<'static> = Over::Drive {
    eject_refused: None,
    title: Ok(""),
};

/// An attached volume at `target` of `class`, published under `key` with the
/// content identity `identity`, shown to the desktop's user.
fn attached(target: &str, class: VolumeClass, (key, identity): ([u8; 16], [u8; 16])) -> Volume {
    let label = target.rsplit('/').find(|part| !part.is_empty());
    Volume {
        label: String::from(label.unwrap_or(target)),
        target: String::from(target),
        medium: None,
        facts: VolumeFacts {
            filesystem: String::from("fat32"),
            title: String::new(),
            class,
            free: None,
            availability: MountAvailability::Available,
            owner: None,
            volume_id: key,
            identity,
            read_only: false,
            titled: false,
            title_writes: true,
        },
        offered: true,
    }
}

/// The stick, attached first, at `/Storage/USB`.
fn stick() -> Volume {
    attached("/Storage/USB", VolumeClass::Removable, (STICK, STICK))
}

/// The desktop's mount table read as `volumes`: whether any icon changed, and
/// what it repainted.
fn mounted<S: DirectorySource>(desktop: &mut Desktop<S>, volumes: Vec<Volume>) -> (bool, Region) {
    let mut damage = Region::new();
    let moved = desktop.adopt_mounts(Some(volumes), layout_of, &mut damage);
    (moved, damage)
}

/// The labels the desktop's icons are shown by, in byte order.
fn labels_of<S: DirectorySource>(desktop: &Desktop<S>) -> Vec<String> {
    let mut labels: Vec<String> = desktop
        .entries()
        .iter()
        .map(|entry| entry.label().unwrap_or(entry.name()).to_string())
        .collect();
    labels.sort_unstable();
    labels
}

/// Where the icon called `name` is listed.
fn position<S: DirectorySource>(desktop: &Desktop<S>, name: &str) -> usize {
    desktop
        .entries()
        .iter()
        .position(|entry| entry.name() == name)
        .unwrap_or_else(|| panic!("{name} is listed"))
}

/// The centre of where the icon listed at `index` stands, which is a cell of
/// its own rather than the `index`th once icons have been placed or kept.
fn icon_centre<S: DirectorySource>(desktop: &Desktop<S>, layout: &GridView, index: usize) -> Point {
    desktop
        .bounds_for_test(layout, index)
        .expect("a shown icon")
        .center()
}

/// What a double-click on the icon at `index`, from `at_ns`, asks for.
fn opened<S: DirectorySource>(
    desktop: &mut Desktop<S>,
    index: usize,
    at_ns: u64,
) -> Option<DesktopAction> {
    let layout = layout_of(desktop);
    let at = icon_centre(desktop, &layout, index);
    let mut press = |now_ns| {
        desktop
            .press(
                at,
                tairix_input::Modifiers::default(),
                &layout,
                Scale::ONE,
                &theme(),
                now_ns,
                &[],
                &mut Region::new(),
            )
            .action
    };
    press(at_ns);
    press(at_ns + 1)
}

/// Whether the menu `model` lets the row labelled `label` be chosen, and the
/// reason it gives where it does not; `None` where it offers no such row.
fn offered(model: &ChainModel, label: &str) -> Option<(bool, Option<String>)> {
    model
        .rows()
        .iter()
        .find(|row| row.drawn().label() == label)
        .map(|row| (row.drawn().state().enabled, row.tip().map(String::from)))
}

/// A shortcut to a volume opens the volume where the storage catalog shows it
/// while it is attached, and otherwise says it is not there, or is someone
/// else's.
#[test]
fn a_shortcut_to_a_volume_opens_it_where_storage_shows_it_or_says_it_is_not_present() {
    let shortcut = link("Holiday", LinkTarget::Volume, &volume_root(STICK));
    let mut desktop = desktop_of(vec![shortcut]);
    assert_eq!(
        opened(&mut desktop, 0, 0),
        Some(DesktopAction::Refuse(GestureRefusal::new(
            "\u{201c}Holiday\u{201d} is not present",
            "The drive it stands for is not attached.",
        )))
    );
    assert!(mounted(&mut desktop, vec![stick()]).0);
    assert_eq!(
        opened(&mut desktop, 0, 10_000_000_000),
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Storage/USB".to_string(),
        }))
    );
    let mut theirs = stick();
    theirs.offered = false;
    assert!(mounted(&mut desktop, vec![theirs]).0);
    assert_eq!(
        opened(&mut desktop, 0, 20_000_000_000),
        Some(DesktopAction::Refuse(GestureRefusal::new(
            "\u{201c}Holiday\u{201d} is not available",
            "The drive it stands for is attached for someone else.",
        )))
    );
}

/// The mount table moving repaints the icons it changed and no other: the
/// shortcut whose volume came, while the files around it stay as they were.
#[test]
fn only_the_icons_the_mount_table_changed_repaint() {
    let shortcut = link("Holiday", LinkTarget::Volume, &volume_root(STICK));
    let mut desktop = desktop_of(vec![file("a.txt"), shortcut, file("z.txt")]);
    let layout = layout_of(&desktop);
    let at = position(&desktop, "Holiday");
    let (moved, damage) = mounted(&mut desktop, vec![stick()]);
    assert!(moved);
    assert_eq!(damage.rects(), [cell(&layout, at)]);
    let (moved, damage) = mounted(&mut desktop, vec![stick()]);
    assert!(
        !moved && damage.is_empty(),
        "the same table changes nothing"
    );
}

/// The desktop draws a drive for each attached volume of the classes its
/// settings name — removable drives alone by default — and counts the view's
/// root as the system drive, opened as the root.
#[test]
fn the_desktop_draws_drives_of_the_classes_it_shows_and_the_root_as_the_system_drive() {
    let mut desktop = desktop_of(vec![file("notes.txt")]);
    let volumes = vec![
        attached("/", VolumeClass::Local, ([1; 16], [1; 16])),
        attached("/System", VolumeClass::Local, ([2; 16], [2; 16])),
        attached("/Storage/Data", VolumeClass::Local, ([3; 16], [3; 16])),
        stick(),
        attached("/Storage/Share", VolumeClass::Network, ([4; 16], [4; 16])),
        attached("/Storage/disk.adf", VolumeClass::Image, ([5; 16], [5; 16])),
    ];
    assert!(mounted(&mut desktop, volumes).0);
    assert_eq!(labels_of(&desktop), ["USB", "notes.txt"]);
    let mut everything = DesktopDrives::default();
    for class in DriveClass::ALL {
        *everything.setting_mut(class) = true;
    }
    assert!(desktop.show_drives(everything));
    assert_eq!(
        labels_of(&desktop),
        ["/", "Data", "Share", "USB", "disk.adf", "notes.txt"],
        "the system tree's own mounts are no drive"
    );
    assert!(!desktop.show_drives(everything), "nothing more to draw");
    let root = position(&desktop, &volume_root([1; 16]));
    assert_eq!(
        opened(&mut desktop, root, 0),
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/".to_string(),
        }))
    );
}

/// A volume already on the desktop by shortcut is not drawn twice, and twins
/// of one identity draw one drive: the attachment a shortcut would open.
#[test]
fn a_volume_is_drawn_once_however_it_is_shown() {
    let twin = attached("/Storage/USB_2", VolumeClass::Removable, ([9; 16], STICK));
    let shortcut = link("Holiday", LinkTarget::Volume, &volume_root(STICK));
    let mut desktop = desktop_of(vec![shortcut]);
    assert!(mounted(&mut desktop, vec![stick(), twin.clone()]).0);
    assert_eq!(labels_of(&desktop), ["Holiday"]);
    let mut bare = desktop_of(vec![]);
    assert!(mounted(&mut bare, vec![twin, stick()]).0);
    assert_eq!(labels_of(&bare), ["USB"]);
}

/// A drive carried over a window is handed over as its attached volume where
/// the storage catalog shows it, named as it is shown, so the one drop policy
/// treats it as a volume rather than as a file in the desktop folder.
#[test]
fn a_drive_is_handed_over_by_its_place_in_the_storage_catalog() {
    let mut desktop = desktop_of(vec![]);
    assert!(mounted(&mut desktop, vec![stick()]).0);
    let layout = layout_of(&desktop);
    let at = position(&desktop, &volume_root(STICK));
    let from = icon_centre(&desktop, &layout, at);
    press_at(&mut desktop, &layout, from, 0);
    let to = Point::new(from.x + 300, from.y + 200);
    desktop.follow_press(to, true, &layout, Scale::ONE, &theme(), &mut Region::new());
    let cargo = desktop.carried_cargo(to, &layout).expect("described");
    assert_eq!(
        cargo.items,
        [DesktopItem {
            path: String::from("/Storage/USB"),
            id: FileId::NONE,
        }]
    );
    assert_eq!(cargo.shown.first().as_str(), "USB", "named as it is shown");
}

/// A drive offers its eject where its viewer may make it, and refuses moving
/// it to the Trash with the reason; a fixed drive's eject is refused too.
#[test]
fn a_drive_offers_its_eject_and_refuses_the_trash_with_the_reason() {
    let mut desktop = Desktop::new(FakeDir(holding(vec![])), home()).acting_for(1000);
    desktop.relist();
    let shown = DesktopDrives::default().with(DriveClass::Fixed, true);
    let _ = desktop.show_drives(shown);
    let fixed = attached("/Storage/Data", VolumeClass::Local, ([3; 16], [3; 16]));
    assert!(mounted(&mut desktop, vec![stick(), fixed]).0);
    let layout = layout_of(&desktop);
    for (name, identity, ejects) in [
        ("USB", STICK, Ok(MountAct::eject(&stick().facts, "USB"))),
        ("Data", [3; 16], Err(REASON_NOT_EJECTABLE)),
    ] {
        let at = position(&desktop, &volume_root(identity));
        let on_icon = desktop.context_press(
            icon_centre(&desktop, &layout, at),
            &layout,
            Scale::ONE,
            &theme(),
            &mut Region::new(),
        );
        assert!(on_icon, "{name}");
        let menu = desktop.menu(true);
        let refused = ejects.as_ref().err().copied();
        assert_eq!(
            offered(&menu, "Eject"),
            Some((refused.is_none(), refused.map(String::from))),
            "{name}"
        );
        assert_eq!(
            offered(&menu, "Move to Trash"),
            Some((false, Some(String::from(REASON_DRIVE_NOT_TRASHED))))
        );
        let ejected = match ejects {
            Ok(act) => DesktopAction::Volume(act),
            Err(reason) => DesktopAction::Refuse(GestureRefusal::new(
                tairix_util::utf8::refusal_headline(name, "ejected"),
                reason,
            )),
        };
        assert_eq!(
            desktop.command(PinboardCommand::Eject, &layout, &[]).action,
            Some(ejected)
        );
        assert_eq!(
            desktop
                .command(PinboardCommand::MoveToTrash, &layout, &[])
                .action,
            Some(DesktopAction::Refuse(GestureRefusal::new(
                tairix_util::utf8::refusal_headline(name, "moved to the Trash"),
                REASON_DRIVE_NOT_TRASHED,
            )))
        );
    }
}

/// A retitle of a fixed drive the kernel refused the user for want of the
/// authority is asked of an administrator through the desktop's own prompt,
/// once and then awaited: a second meanwhile is refused as busy, a failed run
/// is stated, and a declined or a done one says nothing.
#[test]
fn a_drive_retitle_refused_for_want_of_authority_asks_an_administrator() {
    use tairix_abi::window_ipc::{AuthorisedOutcome, SystemTask};
    use tairix_browse::mount::{MountDone, ADMINISTRATOR_BUSY, ADMINISTRATOR_FAILED};
    let mut desktop = Desktop::new(FakeDir(holding(vec![])), home()).acting_for(1000);
    desktop.relist();
    let _ = desktop.show_drives(DesktopDrives::default().with(DriveClass::Fixed, true));
    let mut fixed = attached("/Storage/Data", VolumeClass::Local, ([3; 16], [3; 16]));
    fixed.facts.titled = true;
    let facts = fixed.facts.clone();
    assert!(mounted(&mut desktop, vec![fixed]).0);
    let retitle = || {
        MountAct::retitle(&facts, "Data", Some(1000), "Archive")
            .expect("the user may ask")
            .expect("a change")
    };
    let refused = |act| MountDone {
        act,
        answer: Err(Errno::PermissionDenied),
    };

    desktop.volume_done(refused(retitle()));
    assert_eq!(desktop.take_refusal(), None, "asked, not refused");
    let Some(SystemTask::Retitle(words)) = desktop.take_administrator_ask() else {
        panic!("an administrator is asked to retitle it");
    };
    assert_eq!(
        words.argv().iter().collect::<Vec<_>>(),
        [volume_root([3; 16]).as_str(), "Archive", "Data"]
    );
    assert_eq!(desktop.take_administrator_ask(), None, "asked once");

    desktop.volume_done(refused(retitle()));
    let busy = retitle().refused(ADMINISTRATOR_BUSY);
    assert_eq!(
        desktop.take_refusal(),
        Some(GestureRefusal::new(busy.headline, busy.reason)),
        "one question at a time"
    );

    desktop.administrator_answered(AuthorisedOutcome::Ran { exit_code: 1 });
    let failed = retitle().refused(ADMINISTRATOR_FAILED);
    assert_eq!(
        desktop.take_refusal(),
        Some(GestureRefusal::new(failed.headline, failed.reason))
    );
    for outcome in [
        AuthorisedOutcome::Declined,
        AuthorisedOutcome::Ran { exit_code: 0 },
    ] {
        desktop.volume_done(refused(retitle()));
        assert!(desktop.take_administrator_ask().is_some());
        desktop.administrator_answered(outcome);
        assert_eq!(desktop.take_refusal(), None, "{outcome:?} says nothing");
    }
    // Another refusal is the user's to read, and owes no question.
    desktop.volume_done(MountDone {
        act: retitle(),
        answer: Err(Errno::Busy),
    });
    assert!(desktop.take_refusal().is_some());
    assert_eq!(desktop.take_administrator_ask(), None);
}

/// A drive offers its title where its viewer may retitle the volume: its row
/// opens a field on the title, whose commit retitles it through the one check;
/// otherwise the row is refused with the reason, and so is a commit.
#[test]
fn a_drive_offers_its_title_where_its_viewer_may_retitle_it() {
    use tairix_browse::places::REASON_UNTITLED_FORMAT;
    let mut desktop = Desktop::new(FakeDir(holding(vec![])), home()).acting_for(1000);
    desktop.relist();
    let shown = DesktopDrives::default().with(DriveClass::Fixed, true);
    let _ = desktop.show_drives(shown);
    let mut titled = stick();
    titled.facts.titled = true;
    titled.facts.title = String::from("Holiday");
    let fixed = attached("/Storage/Data", VolumeClass::Local, ([3; 16], [3; 16]));
    assert!(mounted(&mut desktop, vec![titled, fixed]).0);
    let layout = layout_of(&desktop);
    for (identity, retitles) in [(STICK, true), ([3; 16], false)] {
        let at = position(&desktop, &volume_root(identity));
        assert!(desktop.context_press(
            icon_centre(&desktop, &layout, at),
            &layout,
            Scale::ONE,
            &theme(),
            &mut Region::new(),
        ));
        let menu = desktop.menu(true);
        if !retitles {
            assert_eq!(
                offered(&menu, "Title"),
                Some((false, Some(String::from(REASON_UNTITLED_FORMAT))))
            );
            assert_eq!(
                desktop.retitle_selection("Data 2").action,
                Some(DesktopAction::Refuse(GestureRefusal::new(
                    tairix_util::utf8::refusal_headline("Data", "retitled"),
                    REASON_UNTITLED_FORMAT,
                )))
            );
            continue;
        }
        let row = menu
            .rows()
            .iter()
            .find(|row| row.drawn().label() == "Title")
            .expect("the Title row");
        let ChainChild::Entry {
            id,
            initial,
            selected,
        } = row.child()
        else {
            panic!("the Title row opens a field");
        };
        assert_eq!((initial.as_str(), selected.clone()), ("Holiday", 7..7));
        assert_eq!(
            PinboardCommand::from_item(*id),
            Some(PinboardCommand::Title)
        );
        assert_eq!(row.id(), None, "choosing the row opens its field");
        let Some(DesktopAction::Volume(MountAct::Retitle {
            volume_id, titling, ..
        })) = desktop.retitle_selection("Photos").action
        else {
            panic!("a retitle");
        };
        assert_eq!(
            (volume_id, titling.title.as_str(), titling.retitler),
            (STICK, "Photos", tairix_browse::Retitler::AtTheSeat)
        );
        assert_eq!(
            desktop.retitle_selection("Holiday").action,
            None,
            "the title it records asks nothing"
        );
    }
}

/// A drive keeps its place under its volume while the volume is away: the
/// record's first read forgets no place a drive not attached keeps, the drive
/// stands there once its volume returns, and its going forgets nothing.
#[test]
fn a_drive_keeps_its_place_while_its_volume_is_away() {
    let mut desktop = desktop_of(vec![file("notes.txt")]);
    let place = Placed {
        id: FileId::NONE,
        at: Place {
            across: 0,
            down: PLACE_UNITS,
            from: IconFlow::Leading,
        },
        order: 0,
    };
    let mut live = Arrangement::default();
    live.make(&ArrangementEdit::Set(vec![(
        volume_root(STICK),
        Some(place),
    )]))
    .expect("room");
    let layout = layout_of(&desktop);
    desktop.adopt_arrangement(&live, true, &layout, &mut Region::new());
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "nothing forgotten"
    );
    assert!(mounted(&mut desktop, vec![stick()]).0);
    let drive = position(&desktop, &volume_root(STICK));
    assert_eq!(
        desktop.bounds_for_test(&layout, drive),
        Some(cell(&layout, 1)),
        "standing where it was put"
    );
    assert!(mounted(&mut desktop, Vec::new()).0);
    assert!(
        desktop.take_arrangement_edits().is_empty(),
        "nor as it goes"
    );
}

/// The mount table is read one read at a time, with one more owed at most: a
/// failed read leaves the one owed, and a refused one is owed again.
#[test]
fn the_mount_table_is_read_once_at_a_time_with_one_more_owed() {
    let mut desktop = desktop_of(vec![]);
    assert!(!desktop.take_mount_read(), "nothing moved");
    desktop.mounts_moved();
    desktop.mounts_moved();
    assert!(desktop.take_mount_read());
    desktop.mounts_moved();
    assert!(!desktop.take_mount_read(), "one under way");
    assert!(!desktop.adopt_mounts(None, layout_of, &mut Region::new()));
    assert!(desktop.take_mount_read(), "the one owed");
    desktop.mount_read_refused();
    assert!(desktop.take_mount_read(), "a refused read is owed again");
    assert!(!desktop.take_mount_read());
}

/// A shortcut spelled from the root it names, which no walk follows, is
/// reached at its target: a bundle launched by its view path, a folder opened
/// where it is.
#[test]
fn a_shortcut_spelled_from_its_root_is_reached_at_its_target() {
    let mut desktop = desktop_of(vec![
        link(
            "Chess",
            LinkTarget::Bundle,
            "System:/Applications/chess.app",
        ),
        link("Work", LinkTarget::Directory, "Users:/ada/Documents/Work"),
    ]);
    let (chess, work) = (position(&desktop, "Chess"), position(&desktop, "Work"));
    assert_eq!(
        opened(&mut desktop, chess, 0),
        Some(DesktopAction::Activate(DesktopActivation::Launch {
            run_path: "/System/Applications/chess.app/Run".to_string(),
            label: "chess".to_string(),
            document: None,
        }))
    );
    assert_eq!(
        opened(&mut desktop, work, 10_000_000_000),
        Some(DesktopAction::Activate(DesktopActivation::OpenFolder {
            path: "/Users/ada/Documents/Work".to_string(),
        }))
    );
}

/// The icons a desktop has selected, low first.
fn chosen(desktop: &Desktop<FakeDir>) -> Vec<usize> {
    desktop.selection().iter().collect()
}

/// A press on the backdrop with `modifiers`, dragged out to `to` and, unless
/// `held`, let go there; answers the damage the drag reported.
fn band_to(
    desktop: &mut Desktop<FakeDir>,
    layout: &GridView,
    modifiers: tairix_input::Modifiers,
    to: Point,
    held: bool,
) -> Region {
    desktop.press(
        EMPTY_DESKTOP,
        modifiers,
        layout,
        Scale::ONE,
        &theme(),
        0,
        &[],
        &mut Region::new(),
    );
    damage_of(|damage| {
        desktop.follow_press(to, true, layout, Scale::ONE, &theme(), damage);
        if !held {
            desktop.follow_press(to, false, layout, Scale::ONE, &theme(), damage);
        }
    })
}

/// The far corner of the cell `index` stands on.
fn far_corner(layout: &GridView, index: usize) -> Point {
    let tile = cell(layout, index);
    Point::new(tile.right() - 1, tile.bottom() - 1)
}

const CTRL: tairix_input::Modifiers = tairix_input::Modifiers {
    shift: false,
    ctrl: true,
    alt: false,
    meta: false,
};

const SHIFT: tairix_input::Modifiers = tairix_input::Modifiers {
    shift: true,
    ctrl: false,
    alt: false,
    meta: false,
};

/// A band dragged from the backdrop selects every icon it touches and none it
/// does not, keeps them once let go, and is drawn only while it is held.
#[test]
fn a_band_dragged_over_icons_selects_those_it_touches() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c"), file("d")]);
    let layout = layout_of(&desktop);
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        far_corner(&layout, 1),
        true,
    );
    assert_eq!(chosen(&desktop), [0, 1]);
    assert!(desktop.band().is_some(), "drawn while held");
    desktop.follow_press(
        far_corner(&layout, 1),
        false,
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );
    assert_eq!(chosen(&desktop), [0, 1], "kept once let go");
    assert_eq!(desktop.band(), None);
    assert_eq!(
        desktop.selected(),
        None,
        "no one icon is chosen among several"
    );
}

/// A press that never travels past the drag slop draws no band and selects
/// nothing.
#[test]
fn a_band_short_of_the_drag_slop_selects_nothing() {
    let mut desktop = desktop_of(vec![file("a"), file("b")]);
    let layout = layout_of(&desktop);
    let near = Point::new(EMPTY_DESKTOP.x + 1, EMPTY_DESKTOP.y + 1);
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        near,
        true,
    );
    assert_eq!(desktop.band(), None);
    assert!(chosen(&desktop).is_empty());
}

/// With Control held the band adds to what the press kept, and shrinking it
/// releases only what it took, never what was kept.
#[test]
fn a_band_with_control_adds_to_the_selection_the_press_kept() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c"), file("d")]);
    let layout = layout_of(&desktop);
    press_at(&mut desktop, &layout, centre_of(&layout, 3), 0);
    band_to(&mut desktop, &layout, CTRL, far_corner(&layout, 1), true);
    assert_eq!(chosen(&desktop), [0, 1, 3]);
    desktop.follow_press(
        far_corner(&layout, 0),
        true,
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );
    assert_eq!(chosen(&desktop), [0, 3], "shrunk off b, d kept");
}

/// Escape while a band is held takes back what it selected.
#[test]
fn escape_takes_back_what_the_band_selected() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c"), file("d")]);
    let layout = layout_of(&desktop);
    press_at(&mut desktop, &layout, centre_of(&layout, 3), 0);
    band_to(&mut desktop, &layout, CTRL, far_corner(&layout, 1), true);
    desktop.key(
        Key::Named(NamedKey::Escape),
        true,
        &layout,
        &[],
        &mut Region::new(),
    );
    assert_eq!(chosen(&desktop), [3]);
    assert_eq!(desktop.band(), None);
}

/// A step of the band repaints the icons whose mark it changed and the band's
/// own edges, never an icon it left as it was.
#[test]
fn a_band_repaints_only_what_it_changed() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c"), file("d")]);
    let layout = layout_of(&desktop);
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        far_corner(&layout, 0),
        true,
    );
    let step = damage_of(|damage| {
        desktop.follow_press(
            far_corner(&layout, 1),
            true,
            &layout,
            Scale::ONE,
            &theme(),
            damage,
        );
    });
    assert!(step.intersects(cell(&layout, 1)), "b was marked");
    assert!(!step.intersects(cell(&layout, 3)), "d was not touched");
}

/// Control toggles an icon in the selection and Shift adds one, each leaving
/// the rest selected.
#[test]
fn control_toggles_and_shift_adds_an_icon() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c")]);
    let layout = layout_of(&desktop);
    let press = |desktop: &mut Desktop<FakeDir>, index, modifiers| {
        desktop.press(
            centre_of(&layout, index),
            modifiers,
            &layout,
            Scale::ONE,
            &theme(),
            0,
            &[],
            &mut Region::new(),
        );
        desktop.follow_press(
            centre_of(&layout, index),
            false,
            &layout,
            Scale::ONE,
            &theme(),
            &mut Region::new(),
        );
    };
    press(&mut desktop, 0, tairix_input::Modifiers::default());
    press(&mut desktop, 2, SHIFT);
    assert_eq!(chosen(&desktop), [0, 2]);
    press(&mut desktop, 0, CTRL);
    assert_eq!(chosen(&desktop), [2]);
}

/// A press inside a selection of several keeps it, and a release that did not
/// travel collapses it to the icon pressed.
#[test]
fn a_still_press_inside_several_collapses_them_to_the_icon() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c"), file("d")]);
    let layout = layout_of(&desktop);
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        far_corner(&layout, 2),
        false,
    );
    assert_eq!(chosen(&desktop), [0, 1, 2]);
    press_at(&mut desktop, &layout, centre_of(&layout, 1), 10_000_000_000);
    assert_eq!(chosen(&desktop), [0, 1, 2], "kept while held");
    desktop.follow_press(
        centre_of(&layout, 1),
        false,
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );
    assert_eq!(chosen(&desktop), [1]);
}

/// Dragging one of several selected icons carries every one of them by the
/// same distance.
#[test]
fn dragging_one_of_several_carries_them_all() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c")]);
    let layout = layout_of(&desktop);
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        far_corner(&layout, 1),
        false,
    );
    let (a, b, c) = (
        bounds(&desktop, &layout, 0),
        bounds(&desktop, &layout, 1),
        bounds(&desktop, &layout, 2),
    );
    carry(&mut desktop, &layout, 0, (300, 40));
    let moved = |was: Rect, now: Rect| (now.left() - was.left(), now.top() - was.top());
    let shift = moved(a, bounds(&desktop, &layout, 0));
    assert_ne!(shift, (0, 0), "a moved");
    assert_eq!(
        moved(b, bounds(&desktop, &layout, 1)),
        shift,
        "b moved with it"
    );
    assert_eq!(bounds(&desktop, &layout, 2), c, "c, not selected, stayed");
    assert_eq!(chosen(&desktop), [0, 1], "the selection rides along");
}

/// A secondary press inside a selection of several keeps it, and the menu's
/// *Open* says why it does not open them.
#[test]
fn a_menu_over_several_icons_says_why_it_opens_none() {
    let mut desktop = desktop_of(vec![file("a"), file("b"), file("c")]);
    let layout = layout_of(&desktop);
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        far_corner(&layout, 1),
        false,
    );
    let on_icon = desktop.context_press(
        centre_of(&layout, 1),
        &layout,
        Scale::ONE,
        &theme(),
        &mut Region::new(),
    );
    assert!(on_icon);
    assert_eq!(chosen(&desktop), [0, 1]);
    let menu = desktop.menu(true);
    assert_eq!(
        offered(&menu, "Open"),
        Some((false, Some(String::from(REASON_SEVERAL))))
    );
    let opened = desktop.command(PinboardCommand::Open, &layout, &[]).action;
    assert!(
        matches!(&opened, Some(DesktopAction::Refuse(refusal)) if refusal.detail() == REASON_SEVERAL),
        "{opened:?}"
    );
}

/// The band is drawn where it lies, over the backdrop, and confined to the
/// area repainted.
#[test]
fn the_band_is_drawn_where_it_lies() {
    let mut desktop = desktop_of(vec![file("a")]);
    let layout = layout_of(&desktop);
    let to = Point::new(400, 300);
    let draw = |desktop: &Desktop<FakeDir>, area: Rect| {
        let mut surface = Surface::new(800, 600).expect("a surface");
        desktop.render(
            &mut surface,
            &layout,
            Scale::ONE,
            &theme(),
            &mut NoArtwork,
            area,
            None,
        );
        surface
    };
    let bare = draw(&desktop, work_area());
    band_to(
        &mut desktop,
        &layout,
        tairix_input::Modifiers::default(),
        to,
        true,
    );
    let banded = draw(&desktop, work_area());
    let inside = Rect::new(350, 250, 10, 10);
    assert_ne!(
        pixels_in(&bare, inside),
        pixels_in(&banded, inside),
        "the band tints its inside"
    );
    let clipped = draw(&desktop, Rect::new(0, 0, 300, 600));
    assert_eq!(
        pixels_in(&clipped, inside),
        pixels_in(&bare, inside),
        "nothing outside the area"
    );
}

/// A document that would not open, and a folder or shortcut the desktop could
/// not make, are refused naming them and saying why.
#[test]
fn an_unopened_document_and_an_unmade_item_are_refused_saying_why() {
    let unopened = GestureRefusal::unopened("plan.txt", Errno::PermissionDenied);
    assert_eq!(
        unopened.headline(),
        tairix_util::utf8::refusal_headline("plan.txt", "opened")
    );
    assert!(
        unopened.detail().contains("permission denied"),
        "{}",
        unopened.detail()
    );
    let unmade = GestureRefusal::unmade("/Users/ann/Desktop/New folder", Errno::AlreadyExists);
    assert_eq!(
        unmade.headline(),
        tairix_util::utf8::refusal_headline("New folder", "made")
    );
    assert!(unmade.line().starts_with("desktop: "));
}
