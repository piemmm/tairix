use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::fs::{DirChange, DirChangeBatch, DirEntry, DirWatchStatus, FileKind};
use tairix_abi::{Errno, FileId, Time64};
use tairix_geometry::{Rect, Region, Scale};
use tairix_theme::Theme;

use super::*;
use crate::browser::Browser;
use crate::chrome::ToolbarBand;
use crate::entry::{Entry, Occupancy};
use crate::render::{listing_damage, shown_listing};
use crate::source::{DirectorySource, Listing, Probe};
use crate::tests::focused;
use crate::vfs::NoLinks;

/// A directory whose listing a test rewrites, answered at once or, when
/// `deferred`, only once a test lets it land.
#[derive(Clone, Default)]
struct Folder {
    entries: Rc<RefCell<Vec<Entry>>>,
    deferred: Rc<RefCell<bool>>,
    landed: Rc<RefCell<bool>>,
}

impl Folder {
    fn holding(names: &[&str]) -> Self {
        let folder = Self::default();
        folder.set(names);
        folder
    }

    fn set(&self, names: &[&str]) {
        *self.entries.borrow_mut() = names.iter().map(|n| Entry::file(*n)).collect();
    }
}

impl DirectorySource for Folder {
    fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        if *self.deferred.borrow() && !*self.landed.borrow() {
            return Ok(Listing::Pending);
        }
        Ok(Listing::Ready(self.entries.borrow().clone()))
    }

    fn has_children(&mut self, _components: &[String]) -> Result<Probe, Errno> {
        Ok(Probe::Holds(tairix_icon::FolderSample::default()))
    }
}

fn names<S: DirectorySource>(browser: &Browser<S>) -> Vec<String> {
    browser
        .entries()
        .iter()
        .map(|e| e.name().to_string())
        .collect()
}

fn upsert(name: &str) -> EntryChange {
    EntryChange::Upsert(Entry::file(name))
}

fn record(name: &[u8]) -> DirEntry<'_> {
    DirEntry {
        kind: FileKind::Regular,
        size: 3,
        allocated: 4096,
        modified: Time64::from_secs(1_000),
        id: FileId::NONE,
        nlink: 1,
        name,
    }
}

fn batch(status: DirWatchStatus, more: bool, changes: &[DirChange<'_>]) -> Vec<u8> {
    let mut out = vec![0u8; 2048];
    let count = u32::try_from(changes.len()).expect("small");
    let mut at = DirChangeBatch::encode_header(&mut out, status, more, count).expect("header");
    for change in changes {
        at += change.encode_into(&mut out[at..]).expect("record");
    }
    out.truncate(at);
    out
}

#[test]
fn a_batch_decodes_to_the_entries_a_listing_would_hold() {
    let bytes = batch(
        DirWatchStatus::Changes,
        true,
        &[
            DirChange::Present(record(b"moo.txt")),
            DirChange::Absent(b"gone"),
        ],
    );
    let (update, more) = decode_batch("/Users/me", &bytes, &mut NoLinks).expect("decodes");
    assert!(more);
    let expected = Entry::new(
        "moo.txt",
        crate::entry::EntryKind::File,
        3,
        Time64::from_secs(1_000),
    );
    assert_eq!(
        update,
        WatchUpdate::Changes(vec![
            EntryChange::Upsert(expected),
            EntryChange::Remove(String::from("gone"))
        ])
    );
    for (status, want) in [
        (DirWatchStatus::Rescan, WatchUpdate::Rescan),
        (DirWatchStatus::Gone, WatchUpdate::Gone),
        (DirWatchStatus::Changes, WatchUpdate::Quiet),
    ] {
        assert_eq!(
            decode_batch("/", &batch(status, false, &[]), &mut NoLinks),
            Ok((want, false))
        );
    }
    assert!(decode_batch("/", &[0u8; 3], &mut NoLinks).is_err());
}

#[test]
fn updates_fold_with_gone_and_rescan_outranking_changes() {
    let mut update = WatchUpdate::Quiet;
    update.absorb(WatchUpdate::Changes(vec![upsert("a")]));
    update.absorb(WatchUpdate::Changes(vec![upsert("b")]));
    assert_eq!(update, WatchUpdate::Changes(vec![upsert("a"), upsert("b")]));
    update.absorb(WatchUpdate::Rescan);
    update.absorb(WatchUpdate::Changes(vec![upsert("c")]));
    assert_eq!(update, WatchUpdate::Rescan);
    update.absorb(WatchUpdate::Gone);
    update.absorb(WatchUpdate::Rescan);
    assert_eq!(update, WatchUpdate::Gone);
    let mut flood = WatchUpdate::Changes(vec![upsert("x"); PENDING_CHANGES_MAX]);
    flood.absorb(WatchUpdate::Changes(vec![upsert("y")]));
    assert_eq!(
        flood,
        WatchUpdate::Rescan,
        "past the bound a rescan is cheaper"
    );
}

#[test]
fn changes_merge_into_listing_order_keeping_focus_and_selection_by_name() {
    let folder = Folder::holding(&["b", "d", "f", "h"]);
    let mut browser = Browser::open_root(folder).expect("open");
    browser.select(2).expect("select");
    assert_eq!(focused(&browser).map(Entry::name), Some("f"));
    browser.toggle_selection(3).expect("toggle");
    let moved = browser
        .apply_changes(vec![
            upsert("a"),
            EntryChange::Remove(String::from("d")),
            upsert("g"),
            EntryChange::Remove(String::from("never-there")),
        ])
        .expect("merged");
    assert!(moved.is_some());
    assert_eq!(names(&browser), ["a", "b", "f", "g", "h"]);
    assert_eq!(
        focused(&browser).map(Entry::name),
        Some("h"),
        "the focus follows its entry"
    );
    let selected: Vec<usize> = browser.selection().iter().collect();
    assert_eq!(selected, [2, 4], "the selection follows its entries");
}

#[test]
fn a_removed_focus_rests_where_it_was_and_leaves_the_selection() {
    let folder = Folder::holding(&["a", "b", "c"]);
    let mut browser = Browser::open_root(folder).expect("open");
    browser.select(1).expect("select");
    browser
        .apply_changes(vec![EntryChange::Remove(String::from("b"))])
        .expect("merged");
    assert_eq!(names(&browser), ["a", "c"]);
    assert_eq!(focused(&browser).map(Entry::name), Some("c"));
    assert!(
        browser
            .selection()
            .iter()
            .all(|i| browser.entries()[i].name() != "b"),
        "a removed entry never passes its selection on"
    );
}

/// A focus a removal left resting on a neighbour is not a choice, so no verb
/// acts on the neighbour until the user picks it.
#[test]
fn a_focus_a_removal_moved_is_nothing_to_act_on() {
    let folder = Folder::holding(&["a", "b", "c"]);
    let mut browser = Browser::open_root(folder).expect("open");
    browser.select(1).expect("select");
    assert_eq!(browser.chosen_index(), Some(1));
    browser
        .apply_changes(vec![EntryChange::Remove(String::from("b"))])
        .expect("merged");
    assert_eq!(focused(&browser).map(Entry::name), Some("c"));
    assert_eq!(browser.chosen_index(), None);
    assert!(browser.chosen_entry().is_none());
    assert!(matches!(
        browser.activate_selected(crate::BundleIntent::Launch),
        Err(crate::BrowseError::NoSuchEntry)
    ));
    browser.select(1).expect("select");
    assert_eq!(browser.chosen_entry().map(Entry::name), Some("c"));
}

#[test]
fn a_change_that_alters_nothing_shown_reports_no_move() {
    let folder = Folder::holding(&["a", "b"]);
    let mut browser = Browser::open_root(folder).expect("open");
    assert_eq!(browser.apply_changes(vec![upsert("a")]), Ok(None));
    assert_eq!(browser.apply_changes(Vec::new()), Ok(None));
}

#[test]
fn a_changed_folder_keeps_its_occupancy_until_probed_again() {
    let folder = Folder::default();
    *folder.entries.borrow_mut() = vec![Entry::directory("src"), Entry::directory("docs")];
    let mut browser = Browser::open_root(folder).expect("open");
    assert!(browser.resolve_occupancy(0..2));
    let probed = browser.entries()[1].occupancy();
    assert_eq!(
        probed,
        Occupancy::NonEmpty(tairix_icon::FolderSample::default())
    );
    let mut fresh = Entry::directory("src");
    fresh = Entry::new(
        fresh.name().to_string(),
        fresh.kind(),
        0,
        Time64::from_secs(99),
    );
    browser
        .apply_changes(vec![EntryChange::Upsert(fresh)])
        .expect("merged");
    let src = &browser.entries()[1];
    assert_eq!(
        src.occupancy(),
        Occupancy::NonEmpty(tairix_icon::FolderSample::default()),
        "no blink back to plain"
    );
    assert!(src.needs_occupancy_probe(), "but it is asked again");
    assert!(
        !browser.entries()[0].needs_occupancy_probe(),
        "an unchanged folder is not"
    );
    assert!(
        !browser.resolve_occupancy(0..2),
        "an answer that reads as before moves nothing to repaint"
    );
    assert!(!browser.entries()[1].needs_occupancy_probe());
}

#[test]
fn a_reload_keeps_focus_selection_and_unchanged_answers() {
    let folder = Folder::holding(&["a", "b", "c"]);
    let mut browser = Browser::open_root(folder.clone()).expect("open");
    browser.select(2).expect("select");
    folder.set(&["0", "a", "b", "c"]);
    browser.refresh().expect("reload");
    assert_eq!(names(&browser), ["0", "a", "b", "c"]);
    assert_eq!(focused(&browser).map(Entry::name), Some("c"));
}

#[test]
fn a_new_folders_focus_waits_for_its_listing_to_land() {
    let folder = Folder::holding(&["a"]);
    let mut browser = Browser::open_root(folder.clone()).expect("open");
    *folder.deferred.borrow_mut() = true;
    folder.set(&["a", "new"]);
    browser.create_entry("new", |_| Ok(())).expect("creates");
    assert_eq!(browser.focus_pending(), Some("new"));
    assert_eq!(
        focused(&browser).map(Entry::name),
        Some("a"),
        "not yet: the listing is pending"
    );
    *folder.landed.borrow_mut() = true;
    assert!(browser.resume().expect("lands"));
    assert_eq!(focused(&browser).map(Entry::name), Some("new"));
    assert_eq!(browser.focus_pending(), None);
}

#[test]
fn a_change_out_of_view_repaints_only_the_scrollbar() {
    let many: Vec<String> = (0..200).map(|i| alloc::format!("f{i:03}")).collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut browser = Browser::open_root(Folder::holding(&names)).expect("open");
    let (scale, theme) = (Scale::default(), Theme::dark());
    let viewport = Rect::new(0, 0, 400, 300);
    let band = ToolbarBand::Hidden;
    let before = shown_listing(&browser, scale, &theme, viewport, band);
    browser.apply_changes(vec![upsert("zzz")]).expect("merged");
    let mut damage = Region::new();
    assert!(listing_damage(
        &before,
        &browser,
        scale,
        &theme,
        viewport,
        band,
        &mut damage
    ));
    let bar = crate::render::scrollbar_bounds(scale, &theme, viewport, band).expect("a bar");
    assert!(
        damage.rects().iter().all(|&r| r == bar),
        "only the bar moved: {:?}",
        damage.rects()
    );

    let before = shown_listing(&browser, scale, &theme, viewport, band);
    browser
        .apply_changes(vec![EntryChange::Remove(String::from("f001"))])
        .expect("merged");
    let mut damage = Region::new();
    assert!(listing_damage(
        &before,
        &browser,
        scale,
        &theme,
        viewport,
        band,
        &mut damage
    ));
    assert!(damage.rects().len() > 1, "rows shifted beneath the removal");

    let before = shown_listing(&browser, scale, &theme, viewport, band);
    browser.apply_changes(vec![upsert("f150")]).expect("merged");
    let mut damage = Region::new();
    assert!(
        !listing_damage(
            &before,
            &browser,
            scale,
            &theme,
            viewport,
            band,
            &mut damage
        ),
        "an unchanged entry out of view repaints nothing"
    );
}

fn at(path: &[&str]) -> Vec<String> {
    path.iter().map(|c| String::from(*c)).collect()
}

fn changed(name: &str) -> WatchUpdate {
    WatchUpdate::Changes(vec![upsert(name)])
}

fn installed(watches: &mut Watches<u8, u32>, client: u8, path: &[&str], handle: u32) {
    watches.offer(client, &at(path), handle);
    assert_eq!(watches.took(client, &at(path)).join, Some(handle));
}

fn took(join: Option<u32>, release: [Option<u32>; 2]) -> Took<u32> {
    Took { join, release }
}

#[test]
fn a_listing_installs_its_watch_only_where_the_consumer_landed() {
    let mut watches = Watches::new();
    watches.offer(0, &at(&["Users", "me"]), 7);
    assert_eq!(
        watches.took(0, &at(&["Users"])),
        took(None, [Some(7), None]),
        "an offer for somewhere the consumer is not is stale, and let go"
    );
    assert_eq!(
        watches.took(0, &at(&["Users", "me"])),
        took(None, [None, None])
    );
    watches.offer(0, &at(&["Users"]), 8);
    assert_eq!(
        watches.took(0, &at(&["Users"])),
        took(Some(8), [None, None])
    );
    assert_eq!(
        watches.took(0, &at(&["Users"])),
        took(None, [None, None]),
        "a reload with no new watch keeps the one in place"
    );
    watches.offer(0, &at(&["Apps"]), 9);
    assert_eq!(
        watches.took(0, &at(&["Apps"])),
        took(Some(9), [Some(8), None]),
        "the folder left behind is let go"
    );
    assert_eq!(
        watches.took(0, &at(&["Users"])),
        took(None, [None, Some(9)]),
        "moving somewhere unwatched releases the old folder's watch"
    );
}

#[test]
fn drains_are_coalesced_and_their_updates_fold_until_adopted() {
    let mut watches = Watches::new();
    assert!(!watches.want_drain(0), "nothing is watched yet");
    installed(&mut watches, 0, &["Users"], 1);
    assert!(watches.want_drain(0));
    assert!(!watches.want_drain(0), "one drain is already wanted");
    let (whose, location, handle) = watches.next_drain().expect("a drain");
    assert_eq!((whose, location.clone(), handle), (0, at(&["Users"]), 1));
    assert!(watches.next_drain().is_none());
    assert!(watches.deliver(0, &location, changed("a")));
    assert!(watches.deliver(0, &location, changed("b")));
    assert!(!watches.deliver(0, &location, WatchUpdate::Quiet));
    assert_eq!(
        watches.take_update(0),
        WatchUpdate::Changes(vec![upsert("a"), upsert("b")])
    );
    assert_eq!(
        watches.take_update(0),
        WatchUpdate::Quiet,
        "handed over once"
    );
}

#[test]
fn a_drain_of_a_location_the_consumer_left_is_dropped() {
    let mut watches = Watches::new();
    installed(&mut watches, 0, &["Users"], 1);
    watches.want_drain(0);
    let (_, location, _) = watches.next_drain().expect("a drain");
    installed(&mut watches, 0, &["Apps"], 2);
    assert!(!watches.deliver(0, &location, changed("stale")));
    assert_eq!(watches.take_update(0), WatchUpdate::Quiet);
}

#[test]
fn closing_or_leaving_hands_the_watch_back_to_release() {
    let mut watches = Watches::new();
    installed(&mut watches, 0, &["Users"], 1);
    installed(&mut watches, 1, &["Apps"], 2);
    watches.want_drain(0);
    assert_eq!(watches.unwatch(0), Some(1));
    assert!(
        watches.next_drain().is_none(),
        "an unwatched consumer drains nothing"
    );
    assert_eq!(watches.forget(1), [Some(2), None]);
    assert_eq!(watches.forget(1), [None, None]);
}

#[test]
fn a_stopping_desk_records_nothing() {
    let mut watches = Watches::new();
    installed(&mut watches, 0, &["Users"], 1);
    watches.stop();
    assert!(!watches.want_drain(0));
    assert_eq!(watches.offer(0, &at(&["Apps"]), 2), Some(2));
    assert_eq!(watches.took(0, &at(&["Users"])).join, None);
    assert!(watches.next_drain().is_none());
}

#[test]
fn merging_into_a_bare_listing_answers_where_each_entry_went() {
    let mut entries: Vec<Entry> = ["b", "d"].iter().map(|n| Entry::file(*n)).collect();
    let (placed, moved) = merge_changes(
        &mut entries,
        vec![
            upsert("c"),
            EntryChange::Remove(String::from("b")),
            upsert("a"),
        ],
        crate::sort::SortMode::default_order(),
    )
    .expect("merged");
    assert!(moved);
    let shown: Vec<&str> = entries.iter().map(Entry::name).collect();
    assert_eq!(shown, ["a", "c", "d"]);
    assert_eq!(
        [placed.place(0), placed.place(1), placed.place(2)],
        [None, Some(2), None]
    );
}

/// Whatever a batch holds and whatever order the listing sorts in, a merge
/// lands what sorting the changed folder afresh would and places every entry
/// where its name now sits — through inserts made in place and through a
/// rebuild alike.
#[test]
fn a_merge_lands_what_a_fresh_sort_would() {
    use crate::sort::{sort_entries, SortDirection, SortKey, SortMode};
    let sized = |name: &str, size: u64| {
        Entry::new(
            name,
            crate::entry::EntryKind::File,
            size,
            Time64::UNIX_EPOCH,
        )
    };
    let remove = |name: &str| EntryChange::Remove(String::from(name));
    let listing = || -> Vec<Entry> {
        (0..40u64)
            .map(|i| sized(&alloc::format!("n{i:02}"), i % 7))
            .collect()
    };
    let flood: Vec<EntryChange> = (0..12u64)
        .map(|i| EntryChange::Upsert(sized(&alloc::format!("m{i:02}"), i % 5)))
        .chain([remove("n03"), EntryChange::Upsert(sized("n04", 9))])
        .collect();
    assert!(flood.len() > INSERTS_IN_PLACE, "the flood is rebuilt");
    let cases: [Vec<EntryChange>; 8] = [
        vec![EntryChange::Upsert(sized("n05", 99))],
        vec![remove("n00"), EntryChange::Upsert(sized("aa", 3))],
        vec![
            EntryChange::Upsert(sized("n39", 0)),
            remove("n20"),
            EntryChange::Upsert(sized("zz", 6)),
            remove("absent"),
        ],
        vec![EntryChange::Upsert(sized("n10", 3))],
        vec![
            EntryChange::Upsert(sized("n01", 5)),
            EntryChange::Upsert(sized("n01", 1)),
            remove("n02"),
            EntryChange::Upsert(sized("n02", 2)),
        ],
        vec![remove("n07"), remove("n08"), remove("n31")],
        (11..17)
            .map(|i| remove(&alloc::format!("n{i:02}")))
            .chain([EntryChange::Upsert(sized("ab", 4))])
            .collect(),
        flood,
    ];
    for key in [SortKey::Name, SortKey::Size] {
        for direction in [SortDirection::Ascending, SortDirection::Descending] {
            let mode = SortMode { key, direction };
            for changes in &cases {
                let mut before = listing();
                sort_entries(&mut before, mode);
                let mut wanted: BTreeMap<String, Entry> = before
                    .iter()
                    .map(|entry| (String::from(entry.name()), entry.clone()))
                    .collect();
                for change in changes {
                    match change.clone() {
                        EntryChange::Upsert(entry) => {
                            wanted.insert(String::from(entry.name()), entry);
                        }
                        EntryChange::Remove(name) => {
                            wanted.remove(&name);
                        }
                    }
                }
                let mut wanted: Vec<Entry> = wanted.into_values().collect();
                sort_entries(&mut wanted, mode);

                let mut merged = before.clone();
                let (placed, _) =
                    merge_changes(&mut merged, changes.clone(), mode).expect("merged");
                assert_eq!(merged, wanted, "{mode:?} {changes:?}");
                for (at, was) in before.iter().enumerate() {
                    let now = merged.iter().position(|entry| entry.name() == was.name());
                    assert_eq!(placed.place(at), now, "{mode:?} {changes:?} at {at}");
                }
                assert_eq!(placed.place(before.len()), None);
            }
        }
    }
}

/// A filter never turns away a name it holds, whatever its length, so a merge
/// cannot miss the entry a change named; and it does turn away most others.
#[test]
fn a_name_filter_holds_every_name_it_was_given() {
    let names: Vec<String> = (0..500)
        .map(|i| "x".repeat(i % 90) + &alloc::format!("{i}"))
        .collect();
    let filter = NameFilter::of(names.iter().map(String::as_str)).expect("room");
    assert!(names.iter().all(|name| filter.may_hold(name)));
    let strangers = (0..2000)
        .filter(|i| !filter.may_hold(&alloc::format!("other-{i}")))
        .count();
    assert!(
        strangers > 1900,
        "only {strangers} of 2000 were turned away"
    );
    let empty = NameFilter::of(core::iter::empty()).expect("room");
    assert!(!empty.may_hold(""), "an empty batch names nothing");
}

/// What a merge moved is what it answers for: a change of one name in a large
/// folder is placed through that one name, not a table of the whole folder.
#[test]
fn a_merges_placement_is_the_size_of_its_change() {
    let mut entries: Vec<Entry> = (0..10_000)
        .map(|i| Entry::file(alloc::format!("f{i:05}")))
        .collect();
    let (placed, moved) = merge_changes(
        &mut entries,
        vec![upsert("f00000a")],
        crate::sort::SortMode::default_order(),
    )
    .expect("merged");
    assert!(moved);
    let Placed::Merged { gone, arrived } = &placed.placed else {
        panic!("a merge is placed by what it moved");
    };
    assert!(gone.is_empty());
    assert_eq!(arrived, &[1]);
    assert_eq!(placed.place(0), Some(0));
    assert_eq!(placed.place(1), Some(2));
    assert_eq!(placed.place(9_999), Some(10_000));
}

#[test]
fn a_reload_reads_through_the_watch_already_held_and_drops_what_it_supersedes() {
    let mut watches = Watches::new();
    installed(&mut watches, 0, &["Users"], 1);
    watches.want_drain(0);
    let (_, location, _) = watches.next_drain().expect("a drain");
    assert!(watches.deliver(0, &location, changed("old")));
    assert_eq!(watches.relisting(0, &at(&["Users"])), Some(1));
    assert_eq!(
        watches.take_update(0),
        WatchUpdate::Quiet,
        "the read about to begin supersedes what was drained before it"
    );
    assert_eq!(
        watches.relisting(0, &at(&["Apps"])),
        None,
        "elsewhere arms afresh"
    );
    assert_eq!(watches.offer(0, &at(&["Apps"]), 2), None);
    assert_eq!(
        watches.relisting(0, &at(&["Apps"])),
        Some(2),
        "an offer is held too"
    );
    assert_eq!(
        watches.offer(0, &at(&["Apps"]), 3),
        Some(2),
        "a displaced offer is handed back to let go of"
    );
    watches.stop();
    assert_eq!(watches.offer(0, &at(&["Apps"]), 4), Some(4));
}

/// A watch that reported its directory gone is spent: a reload arms afresh
/// rather than reading through it, and the gone waits for a reload that finds
/// nothing either.
#[test]
fn a_reload_never_reads_through_a_watch_that_reported_gone() {
    let mut watches = Watches::new();
    installed(&mut watches, 0, &["Users", "old"], 1);
    watches.want_drain(0);
    let (_, location, _) = watches.next_drain().expect("a drain");
    assert!(watches.deliver(0, &location, WatchUpdate::Gone));
    assert_eq!(watches.relisting(0, &at(&["Users", "old"])), None);
    assert_eq!(watches.take_update(0), WatchUpdate::Gone, "still owed");
    assert!(watches.deliver(0, &location, WatchUpdate::Gone));
    assert_eq!(watches.offer(0, &at(&["Users", "old"]), 2), None);
    assert_eq!(
        watches.took(0, &at(&["Users", "old"])),
        took(Some(2), [Some(1), None]),
        "the fresh watch replaces the spent one"
    );
    assert_eq!(watches.take_update(0), WatchUpdate::Quiet);
}

/// With no reader, a listing read on the caller is armed and taken at once:
/// its watch joins, a reload reads through it, and moving away lets it go.
#[test]
fn a_listing_read_here_follows_its_folder_as_a_waited_one_does() {
    let mut watches: Watches<u8, u32> = Watches::new();
    let listing = || Ok(vec![Entry::file("a")]);
    let (listed, joined) = watches.read_here(0, &at(&["Users"]), |reuse| {
        assert_eq!(reuse, None, "nothing held to read through yet");
        (listing(), Some(1))
    });
    assert_eq!(listed, listing());
    assert_eq!(joined, took(Some(1), [None, None]));
    assert!(watches.follows(0, &at(&["Users"])));
    let (_, reloaded) = watches.read_here(0, &at(&["Users"]), |reuse| {
        assert_eq!(reuse, Some(1), "a reload reads through the watch held");
        (listing(), None)
    });
    assert_eq!(reloaded, took(None, [None, None]), "and keeps it");
    let (_, moved) = watches.read_here(0, &at(&["Apps"]), |_| (listing(), Some(2)));
    assert_eq!(
        moved,
        took(Some(2), [Some(1), None]),
        "the folder left is let go"
    );
    assert!(!watches.follows(0, &at(&["Users"])));
    assert!(watches.follows(0, &at(&["Apps"])));
}

/// A watch follows only the folder it is on, and only until it reports that
/// folder gone.
#[test]
fn a_watch_follows_its_own_folder_until_it_is_gone() {
    let mut watches: Watches<u8, u32> = Watches::new();
    assert!(
        !watches.follows(0, &at(&["Users"])),
        "nothing is watched yet"
    );
    installed(&mut watches, 0, &["Users"], 1);
    assert!(watches.follows(0, &at(&["Users"])));
    assert!(!watches.follows(0, &at(&["Apps"])), "nor another folder");
    assert!(
        !watches.follows(1, &at(&["Users"])),
        "nor for another consumer"
    );
    assert!(watches.want_drain(0));
    let (client, location, _) = watches.next_drain().expect("the drain wanted");
    watches.deliver(client, &location, WatchUpdate::Gone);
    assert!(
        !watches.follows(0, &at(&["Users"])),
        "a gone folder reports nothing"
    );
}

/// A watch a read here armed but the desk will not hold is handed back to be
/// let go of: once the desk has stopped, and when an offer for somewhere else
/// was still pending.
#[test]
fn a_read_here_hands_back_what_it_displaced() {
    let mut watches: Watches<u8, u32> = Watches::new();
    watches.offer(0, &at(&["Apps"]), 4);
    let (_, joined) = watches.read_here(0, &at(&["Users"]), |_| {
        (Ok(vec![Entry::file("a")]), Some(6))
    });
    assert_eq!(
        joined,
        took(Some(6), [Some(4), None]),
        "the stale offer goes"
    );
    watches.stop();
    let (listed, refused) = watches.read_here(1, &at(&["Users"]), |_| {
        (Ok(vec![Entry::file("a")]), Some(7))
    });
    assert!(listed.is_ok());
    assert_eq!(
        refused,
        took(None, [Some(7), None]),
        "a stopped desk keeps none"
    );
}

/// A read here that is refused, or listed without a watch, installs nothing,
/// and a listing whose watch would not arm still lists.
#[test]
fn a_refused_read_here_installs_no_watch() {
    let mut watches: Watches<u8, u32> = Watches::new();
    let (listed, joined) = watches.read_here(0, &at(&["Gone"]), |_| (Err(Errno::NotFound), None));
    assert_eq!(
        (listed, joined),
        (Err(Errno::NotFound), took(None, [None, None]))
    );
    assert!(!watches.follows(0, &at(&["Gone"])));
    let (listed, joined) =
        watches.read_here(0, &at(&["Users"]), |_| (Ok(vec![Entry::file("a")]), None));
    assert!(listed.is_ok());
    assert_eq!(joined, took(None, [None, None]));
    assert!(!watches.follows(0, &at(&["Users"])));
}

/// With no reader, every drain wanted is run on the caller, its update landed
/// for the consumer, and only a change owes the loop a look.
#[test]
fn drains_here_land_what_each_wanted_watch_reported() {
    let mut watches: Watches<u8, u32> = Watches::new();
    installed(&mut watches, 0, &["Users"], 1);
    installed(&mut watches, 1, &["Apps"], 2);
    assert!(!watches.drain_here(|_| unreachable!("nothing was wanted")));
    assert!(watches.want_drain(0) && watches.want_drain(1));
    let mut drained = Vec::new();
    let landed = watches.drain_here(|&handle| {
        drained.push(handle);
        if handle == 1 {
            changed("a")
        } else {
            WatchUpdate::Quiet
        }
    });
    assert!(landed);
    assert_eq!(drained, [1, 2]);
    assert_eq!(watches.take_update(0), changed("a"));
    assert_eq!(watches.take_update(1), WatchUpdate::Quiet);
    assert!(watches.want_drain(1));
    assert!(
        !watches.drain_here(|_| WatchUpdate::Quiet),
        "a quiet drain owes nothing"
    );
}

/// Stopping lets every offer go, so a reader parked on one leaves.
#[test]
fn stopping_lets_every_offer_go() {
    let mut watches = Watches::new();
    assert_eq!(watches.offer(0, &at(&["Apps"]), 1), None);
    watches.stop();
    assert_eq!(watches.took(0, &at(&["Apps"])).join, None);
    assert_eq!(watches.forget(0), [None, None]);
}

#[test]
fn a_cleared_selection_stays_cleared_and_a_removed_one_passes_to_nothing() {
    let folder = Folder::holding(&["a", "b", "c"]);
    let mut browser = Browser::open_root(folder).expect("open");
    browser.select(1).expect("select");
    browser.clear_selection();
    browser.apply_changes(vec![upsert("d")]).expect("merged");
    assert!(
        browser.selection().is_empty(),
        "a merge refilled a cleared selection"
    );
    browser.select(1).expect("select");
    browser
        .apply_changes(vec![EntryChange::Remove(String::from("b"))])
        .expect("merged");
    assert!(
        browser.selection().is_empty(),
        "the neighbour of a removed entry was selected in its place"
    );
}

#[test]
fn the_anchor_follows_a_surviving_member_when_its_own_entry_goes() {
    let folder = Folder::holding(&["a", "b", "c", "d"]);
    let mut browser = Browser::open_root(folder).expect("open");
    browser.select(1).expect("select");
    browser.toggle_selection(3).expect("toggle");
    browser
        .apply_changes(vec![EntryChange::Remove(String::from("d"))])
        .expect("merged");
    let anchor = browser
        .selection()
        .anchor()
        .expect("an anchor while selected");
    assert_eq!(browser.entries()[anchor].name(), "b");
}

#[test]
fn choosing_something_else_lets_a_pending_focus_go() {
    let folder = Folder::holding(&["a", "z"]);
    let mut browser = Browser::open_root(folder.clone()).expect("open");
    *folder.deferred.borrow_mut() = true;
    folder.set(&["a", "new", "z"]);
    browser.create_entry("new", |_| Ok(())).expect("creates");
    assert_eq!(browser.focus_pending(), Some("new"));
    browser.select(1).expect("the user picks z");
    assert_eq!(browser.focus_pending(), None);
    *folder.landed.borrow_mut() = true;
    assert!(browser.resume().expect("lands"));
    assert_eq!(
        focused(&browser).map(Entry::name),
        Some("z"),
        "the user's choice stands"
    );
}

#[test]
fn a_focus_whose_name_never_showed_is_let_go_at_the_next_listing() {
    let folder = Folder::holding(&["a"]);
    let mut browser = Browser::open_root(folder.clone()).expect("open");
    *folder.deferred.borrow_mut() = true;
    browser
        .create_entry("gone-again", |_| Ok(()))
        .expect("creates");
    *folder.landed.borrow_mut() = true;
    assert!(browser.resume().expect("lands"));
    assert_eq!(browser.focus_pending(), None);
    assert_eq!(focused(&browser).map(Entry::name), Some("a"));
}

/// A tree of folders a test removes from, answered at once or, when `later`,
/// a request at a time: each asked target waits until it is asked again.
#[derive(Clone, Default)]
struct Tree {
    dirs: Rc<RefCell<BTreeMap<Vec<String>, Vec<Entry>>>>,
    later: Rc<RefCell<bool>>,
    asked: Rc<RefCell<Vec<Vec<String>>>>,
}

impl Tree {
    fn holding(paths: &[&[&str]]) -> Self {
        let tree = Self::default();
        for path in paths {
            tree.dirs
                .borrow_mut()
                .insert(at(path), vec![Entry::file("x")]);
        }
        tree
    }

    fn remove(&self, path: &[&str]) {
        self.dirs.borrow_mut().remove(&at(path));
    }
}

impl DirectorySource for Tree {
    fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
        if *self.later.borrow() {
            let mut asked = self.asked.borrow_mut();
            if let Some(waited) = asked.iter().position(|was| was.as_slice() == components) {
                asked.remove(waited);
            } else {
                asked.push(components.to_vec());
                return Ok(Listing::Pending);
            }
        }
        self.dirs
            .borrow()
            .get(components)
            .cloned()
            .map(Listing::Ready)
            .ok_or(Errno::NotFound)
    }

    fn has_children(&mut self, _components: &[String]) -> Result<Probe, Errno> {
        Ok(Probe::Holds(tairix_icon::FolderSample::default()))
    }
}

/// A folder whose parent went with it is left for the nearest folder above
/// that is still there, not for the parent that is gone too.
#[test]
fn a_climb_passes_every_ancestor_that_went_too() {
    let tree = Tree::holding(&[&[], &["a"], &["a", "b"], &["a", "b", "c"]]);
    let mut browser = Browser::open_root(tree.clone()).expect("open");
    browser.navigate_to(at(&["a", "b", "c"])).expect("navigate");
    tree.remove(&["a", "b", "c"]);
    tree.remove(&["a", "b"]);
    assert_eq!(browser.climb(), Ok(true));
    assert_eq!(browser.components(), at(&["a"]));
}

/// Where each listing answers later, a refusal that lands climbs on from the
/// folder that was refused rather than asking for it again.
#[test]
fn a_climb_carries_on_from_a_refusal_that_lands_later() {
    let tree = Tree::holding(&[&[], &["a"], &["a", "b"], &["a", "b", "c"]]);
    let mut browser = Browser::open_root(tree.clone()).expect("open");
    browser.navigate_to(at(&["a", "b", "c"])).expect("navigate");
    tree.remove(&["a", "b", "c"]);
    tree.remove(&["a", "b"]);
    *tree.later.borrow_mut() = true;
    assert_eq!(browser.climb(), Ok(true));
    assert!(browser.is_listing(), "the parent is asked for");
    assert!(browser.resume().is_err(), "and refused when it answers");
    assert_eq!(browser.climb(), Ok(true));
    assert_eq!(browser.resume(), Ok(true));
    assert_eq!(browser.components(), at(&["a"]));
}

/// With every folder above gone too there is nowhere to climb to.
#[test]
fn a_climb_with_nothing_left_above_says_so() {
    let tree = Tree::holding(&[&[], &["a"]]);
    let mut browser = Browser::open_root(tree.clone()).expect("open");
    browser.navigate_to(at(&["a"])).expect("navigate");
    tree.dirs.borrow_mut().clear();
    assert_eq!(browser.climb(), Ok(false));
}
