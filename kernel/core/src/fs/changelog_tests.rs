use super::*;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemWrite, NameMatching, NodeKind,
    NodeSecurity,
};
use tairix_reclaim::{CacheBudget, ReclaimOwner};

use crate::fs::memfs::{RwMockFs, ADMIN_RECORD};
use crate::fs::CachedFs;
use crate::fswatch::{Drain, WatchRegistry};
use crate::test_pressure::unpressured;
use crate::test_sink::TestSink;

const DIR: NodeId = NodeId::from_raw(10);
const DIR2: NodeId = NodeId::from_raw(11);
const SUB: NodeId = NodeId::from_raw(12);
const FILE: NodeId = NodeId::from_raw(13);

/// A claimed table of the calling test's own, and the table it claims.
fn claimed() -> (Claim, Arc<VolumeWatch>) {
    let registry: &'static WatchRegistry = crate::forever(Box::new(WatchRegistry::new()));
    let claim = registry
        .claim([0x5C; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    let table = Arc::clone(claim.table());
    (claim, table)
}

/// Arm a watcher on `node`, under the mount table's first epoch.
fn arm(table: &VolumeWatch, node: NodeId) -> u64 {
    table.watch(node.raw(), 0).expect("arms").0
}

/// What `node`'s journal holds for `watcher`, drained.
fn drained(table: &VolumeWatch, node: NodeId, watcher: u64) -> Vec<String> {
    match table
        .take(node.raw(), watcher, usize::MAX, false)
        .expect("copies out")
    {
        Some(Drain::Names { names, upto, .. }) => {
            table.commit(node.raw(), watcher, upto, 0);
            names
                .iter()
                .map(|n| String::from_utf8(n.to_vec()).expect("utf-8"))
                .collect()
        }
        Some(Drain::Rescan { .. }) => alloc::vec![String::from("<rescan>")],
        None => alloc::vec![String::from("<unwatched>")],
    }
}

#[test]
fn a_change_inside_a_subfolder_reaches_the_subfolders_entry() {
    let (claim, table) = claimed();
    let parent = arm(&table, DIR);
    let inner = arm(&table, SUB);
    let mut log = ChangeLog::new(claim);
    log.resolved(SUB, DIR, b"sub");
    log.added(SUB, b"new.txt", Some(FILE));
    assert_eq!(drained(&table, SUB, inner), ["new.txt"]);
    assert_eq!(drained(&table, DIR, parent), ["sub"]);
}

#[test]
fn a_metadata_change_is_attributed_to_the_entry_it_was_made_through() {
    let (claim, table) = claimed();
    let w = arm(&table, DIR);
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"notes.txt");
    log.changed(FILE);
    assert_eq!(drained(&table, DIR, w), ["notes.txt"]);
}

#[test]
fn an_unbound_name_is_never_attributed() {
    let (claim, table) = claimed();
    let w = arm(&table, DIR);
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"gone.txt");
    log.removed(DIR, b"gone.txt", Some(FILE));
    assert_eq!(drained(&table, DIR, w), ["gone.txt"]);
    log.changed(FILE);
    assert!(drained(&table, DIR, w).is_empty(), "the removal unbound it");
}

#[test]
fn a_rename_rebinds_the_moved_entry_and_reports_both_names() {
    let (claim, table) = claimed();
    let src = arm(&table, DIR);
    let dst = arm(&table, DIR2);
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"draft");
    log.renamed((DIR, b"draft"), (DIR2, b"final"), Some(FILE), None);
    assert_eq!(drained(&table, DIR, src), ["draft"]);
    assert_eq!(drained(&table, DIR2, dst), ["final"]);
    log.changed(FILE);
    assert_eq!(
        drained(&table, DIR2, dst),
        ["final"],
        "it is bound at its new name"
    );
    assert!(drained(&table, DIR, src).is_empty());
}

#[test]
fn nothing_is_kept_while_nothing_is_watched() {
    let (claim, table) = claimed();
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"early");
    let w = arm(&table, DIR);
    log.changed(FILE);
    assert!(
        drained(&table, DIR, w).is_empty(),
        "a binding seen before the watch is not trusted"
    );
}

#[test]
fn the_trail_keeps_only_the_most_recent_bindings() {
    let (claim, table) = claimed();
    let w = arm(&table, DIR);
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"oldest");
    for node in 100..100 + TRAIL_LEN as u64 {
        log.resolved(NodeId::from_raw(node), DIR2, b"filler");
    }
    log.changed(FILE);
    assert!(
        drained(&table, DIR, w).is_empty(),
        "the oldest binding was displaced"
    );
}

/// A cached exact-name volume with a watch on its root.
fn watched_volume() -> (CachedFs<RwMockFs>, Arc<VolumeWatch>, NodeId, u64) {
    let (claim, table) = claimed();
    let sink: &'static TestSink = crate::forever(Box::new(TestSink::new()));
    let fs = CachedFs::new(
        RwMockFs::new().with_exact_names(),
        CacheBudget::from_backing(16 << 20),
        ReclaimOwner::FilesystemVolume { volume: 1 },
        unpressured(),
        sink,
    )
    .with_watch(claim);
    let root = fs.root();
    let watcher = arm(&table, root);
    (fs, table, root, watcher)
}

#[test]
fn every_mutation_of_a_cached_volume_is_reported() {
    let (mut fs, table, root, w) = watched_volume();
    let a = fs
        .create(root, b"a.txt", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_eq!(drained(&table, root, w), ["a.txt"]);
    fs.write_at(a, 0, b"moo").expect("write");
    assert_eq!(drained(&table, root, w), ["a.txt"], "attributed by node");
    fs.truncate(a, 1).expect("truncate");
    assert_eq!(drained(&table, root, w), ["a.txt"]);
    fs.rename(root, b"a.txt", root, b"b.txt").expect("rename");
    assert_eq!(drained(&table, root, w), ["a.txt", "b.txt"]);
    fs.create_link(root, b"link", b"b.txt", &ADMIN_RECORD)
        .expect("symlink");
    assert_eq!(drained(&table, root, w), ["link"]);
    let node = fs.lookup(root, b"b.txt").expect("resolves");
    fs.link(root, b"hard", node).expect("link");
    assert_eq!(
        drained(&table, root, w),
        ["b.txt", "hard"],
        "the name it was reached through shows the new link count"
    );
    let sec = fs.security(node).expect("security");
    fs.set_security(node, sec).expect("chmod");
    assert_eq!(
        drained(&table, root, w),
        ["hard"],
        "attributed through the last lookup"
    );
    fs.lookup(root, b"b.txt").expect("resolves");
    fs.set_attr(node, b"user.tag", b"1").expect("xattr");
    assert_eq!(drained(&table, root, w), ["b.txt"]);
    fs.remove(root, b"b.txt").expect("unlink");
    assert_eq!(drained(&table, root, w), ["b.txt"]);
}

#[test]
fn a_watched_directorys_removal_makes_its_watchers_rescan() {
    let (mut fs, table, root, _) = watched_volume();
    let sub = fs
        .create(root, b"sub", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let inner = arm(&table, sub);
    fs.remove(root, b"sub").expect("rmdir");
    assert_eq!(drained(&table, sub, inner), ["<rescan>"]);
}

#[test]
fn a_subfolders_new_entry_reaches_its_parents_watcher_through_the_walk() {
    let (mut fs, table, root, w) = watched_volume();
    fs.create(root, b"sub", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    drained(&table, root, w);
    // The walk to `sub` is what an operation inside it always makes first.
    let sub = fs.lookup(root, b"sub").expect("resolves");
    fs.create(sub, b"inner.txt", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_eq!(drained(&table, root, w), ["sub"]);
}

#[test]
fn a_refused_mutation_reports_nothing() {
    let (mut fs, table, root, w) = watched_volume();
    fs.create(root, b"a.txt", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    drained(&table, root, w);
    assert!(fs
        .create(root, b"a.txt", NodeKind::RegularFile, &ADMIN_RECORD)
        .is_err());
    assert!(fs.remove(root, b"never-there").is_err());
    assert!(drained(&table, root, w).is_empty());
}

/// A watched directory moved keeps its watchers' journal: what it lists did
/// not change, and nothing about it needs re-reading.
///
/// Regression: a move revalidated the moved directory's journal, so its
/// watchers rescanned a directory that had not changed.
#[test]
fn a_moved_directory_keeps_its_watchers_journal() {
    let (mut fs, table, root, w) = watched_volume();
    let sub = fs
        .create(root, b"sub", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let inner = arm(&table, sub);
    drained(&table, root, w);
    fs.rename(root, b"sub", root, b"moved").expect("rename");
    assert!(
        drained(&table, sub, inner).is_empty(),
        "nothing in it changed"
    );
    assert_eq!(drained(&table, root, w), ["sub", "moved"]);
}

/// A directory's security changing makes its own watchers rescan, since who
/// may list it may have changed, and no other watcher.
///
/// Regression: any directory's security change anywhere made every watcher
/// on the machine rescan, while the watched directory's own kept its names.
#[test]
fn a_watched_directorys_security_change_makes_only_its_watchers_rescan() {
    let (mut fs, table, root, w) = watched_volume();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let inner = arm(&table, dir);
    drained(&table, root, w);
    let sec = fs.security(file).expect("security");
    fs.set_security(file, sec).expect("chmod");
    assert_eq!(drained(&table, root, w), ["file"]);
    assert!(drained(&table, dir, inner).is_empty());
    let sec = fs.security(dir).expect("security");
    fs.set_security(dir, sec).expect("chmod");
    assert_eq!(drained(&table, dir, inner), ["<rescan>"]);
    assert_eq!(drained(&table, root, w), ["dir"], "its entry changed too");
}

/// A new directory is born with its creator's record, so making one costs no
/// watcher a rescan.
#[test]
fn a_new_directory_born_with_its_record_costs_no_rescan() {
    let (mut fs, table, root, w) = watched_volume();
    fs.create(
        root,
        b"made",
        NodeKind::Directory,
        &NodeSecurity::new(0o700, 1000, 1000),
    )
    .expect("mkdir");
    assert_eq!(drained(&table, root, w), ["made"]);
}

/// A pinned node's change reaches its name however many walks came since it
/// was pinned: a descriptor held open reports every write it makes.
///
/// Regression: a node-keyed write was attributed only through the trail, so
/// a long-open descriptor's appends went unreported once other walks had
/// displaced its binding.
#[test]
fn a_pinned_nodes_change_reaches_its_name_however_many_walks_came_since() {
    let (claim, table) = claimed();
    let w = arm(&table, DIR);
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"notes.txt");
    log.pinned(FILE, FILE);
    for node in 100..100 + 2 * TRAIL_LEN as u64 {
        log.resolved(NodeId::from_raw(node), DIR2, b"filler");
    }
    log.changed(FILE);
    assert_eq!(drained(&table, DIR, w), ["notes.txt"]);
}

#[test]
fn a_pinned_node_keeps_its_place_through_a_rename() {
    let (claim, table) = claimed();
    let src = arm(&table, DIR);
    let dst = arm(&table, DIR2);
    let mut log = ChangeLog::new(claim);
    log.resolved(FILE, DIR, b"draft");
    log.pinned(FILE, FILE);
    log.renamed((DIR, b"draft"), (DIR2, b"final"), Some(FILE), None);
    drained(&table, DIR, src);
    drained(&table, DIR2, dst);
    for node in 100..100 + 2 * TRAIL_LEN as u64 {
        log.resolved(NodeId::from_raw(node), SUB, b"filler");
    }
    log.changed(FILE);
    assert_eq!(drained(&table, DIR2, dst), ["final"]);
    assert!(drained(&table, DIR, src).is_empty());
}

/// A pinned node that lost its name, or its last hold, reports through no
/// name; and a pin the latest walk did not reach is given none.
#[test]
fn a_pinned_place_ends_with_its_name_or_its_last_hold() {
    let (claim, table) = claimed();
    let w = arm(&table, DIR);
    let mut log = ChangeLog::new(claim);
    let filled = |log: &mut ChangeLog| {
        for node in 100..100 + 2 * TRAIL_LEN as u64 {
            log.resolved(NodeId::from_raw(node), DIR2, b"filler");
        }
    };

    log.resolved(FILE, DIR, b"gone.txt");
    log.pinned(FILE, FILE);
    log.removed(DIR, b"gone.txt", Some(FILE));
    drained(&table, DIR, w);
    log.changed(FILE);
    assert!(
        drained(&table, DIR, w).is_empty(),
        "the removal unplaced it"
    );

    log.resolved(SUB, DIR, b"kept");
    log.pinned(SUB, SUB);
    log.unpinned(SUB);
    filled(&mut log);
    log.changed(SUB);
    assert!(
        drained(&table, DIR, w).is_empty(),
        "the last unpin forgot it"
    );

    log.resolved(SUB, DIR, b"other");
    log.pinned(FILE, FILE);
    filled(&mut log);
    log.changed(FILE);
    assert!(drained(&table, DIR, w).is_empty(), "never placed");
}

/// A write through a pinned node of a cached volume is reported through the
/// name the node bears now, however long ago it was opened.
#[test]
fn a_write_through_a_pinned_node_reaches_its_current_name() {
    let (mut fs, table, root, w) = watched_volume();
    fs.create(root, b"a.txt", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let found = fs.lookup(root, b"a.txt").expect("resolves");
    let pinned = fs.pin(found).expect("pin");
    fs.rename(root, b"a.txt", root, b"b.txt").expect("rename");
    for n in 0..2 * TRAIL_LEN {
        let name = alloc::format!("f{n}");
        fs.create(root, name.as_bytes(), NodeKind::RegularFile, &ADMIN_RECORD)
            .expect("create");
    }
    drained(&table, root, w);
    fs.write_at(pinned, 0, b"moo").expect("write");
    assert_eq!(drained(&table, root, w), ["b.txt"]);
    fs.unpin(pinned).expect("unpin");
}

/// The trail's slots are reserved once, so no name it held is ever left
/// behind in an allocation it outgrew.
#[test]
fn the_trail_reserves_its_slots_once() {
    let mut trail = Trail::new();
    trail.record(1, 0, b"first");
    let (at, capacity) = (trail.slots.as_ptr(), trail.slots.capacity());
    assert!(capacity >= TRAIL_LEN);
    for node in 2..2 * TRAIL_LEN as u64 {
        trail.record(node, 0, b"another");
    }
    assert_eq!(trail.slots.as_ptr(), at);
    assert_eq!(trail.slots.capacity(), capacity);
}

/// A full trail gives up the binding resolved longest ago, not whichever slot
/// comes next, so the walk an operation just made survives it.
#[test]
fn a_full_trail_forgets_the_binding_resolved_longest_ago() {
    let mut trail = Trail::new();
    for node in 1..=TRAIL_LEN as u64 {
        trail.record(node, 0, b"name");
    }
    trail.record(1, 0, b"name");
    trail.record(100, 0, b"new");
    assert!(
        trail.binding_of(1).is_some(),
        "resolved again, it is recent"
    );
    assert!(trail.binding_of(2).is_none(), "the oldest went");
    assert!(trail.binding_of(100).is_some());
}
