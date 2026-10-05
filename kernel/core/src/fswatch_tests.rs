use super::*;

use alloc::boxed::Box;
use alloc::string::String;

use tairix_reclaim::{FreeMemorySource, PressureBand};

use crate::test_pressure::{free_for, pressured, unpressured};

const DIR: NodeId = NodeId::from_raw(10);
const SUB: NodeId = NodeId::from_raw(11);
const FILE: NodeId = NodeId::from_raw(12);

/// A registry of the calling test's own, so no test shares tables, charges
/// or the journal budget with another.
fn registry() -> &'static WatchRegistry {
    Box::leak(Box::new(WatchRegistry::new()))
}

fn table(volume: u8) -> VolumeWatch {
    VolumeWatch::new([volume; 16], NameMatching::Exact, unpressured(), registry())
}

/// Arm a watcher on `node`, for a path that resolved under the epochs now.
fn arm(watch: &VolumeWatch, node: NodeId) -> u64 {
    watch.watch(node.raw(), watch.epochs(0)).expect("arms").0
}

/// Deliver `watcher` everything up to `upto`, under the epochs now.
fn commit(watch: &VolumeWatch, watcher: u64, upto: u64) {
    watch.commit(DIR.raw(), watcher, upto, watch.epochs(0));
}

/// The names a drain takes, as owned strings, with its position and `more`.
fn names_of(drain: Result<Option<Drain>, Errno>) -> (Vec<String>, u64, bool) {
    match drain.expect("the names copy out") {
        Some(Drain::Names { names, upto, more }) => (
            names
                .iter()
                .map(|n| String::from_utf8(n.to_vec()).expect("utf-8"))
                .collect(),
            upto,
            more,
        ),
        Some(Drain::Rescan { .. }) => panic!("expected names, got a rescan"),
        None => panic!("expected names, got no watcher"),
    }
}

fn is_rescan(drain: &Result<Option<Drain>, Errno>) -> bool {
    matches!(drain, Ok(Some(Drain::Rescan { .. })))
}

fn drain_all(watch: &VolumeWatch, watcher: u64) -> Vec<String> {
    let (names, upto, more) = names_of(watch.take(DIR.raw(), watcher, usize::MAX, false));
    assert!(!more);
    commit(watch, watcher, upto);
    names
}

fn parked(watch: &VolumeWatch, node: NodeId) -> bool {
    watch
        .nodes
        .lock()
        .get(&node.raw())
        .is_some_and(|n| n.parked)
}

fn node_journal_bytes(watch: &VolumeWatch, node: NodeId) -> usize {
    watch
        .nodes
        .lock()
        .get(&node.raw())
        .and_then(|n| n.journal.as_ref().map(|j| j.bytes))
        .expect("journal")
}

fn journal_bytes(watch: &VolumeWatch) -> usize {
    node_journal_bytes(watch, DIR)
}

#[test]
fn a_name_changed_many_times_is_one_record() {
    let watch = table(1);
    let w = arm(&watch, DIR);
    for _ in 0..10_000 {
        watch.entry_changed(DIR, b"build.log", Some(FILE));
    }
    watch.entries_changed(DIR, b"new.o", None);
    assert_eq!(drain_all(&watch, w), ["build.log", "new.o"]);
    assert!(
        drain_all(&watch, w).is_empty(),
        "drained names do not repeat"
    );
}

#[test]
fn a_change_already_pending_for_every_watcher_moves_nothing() {
    let watch = table(2);
    let w = arm(&watch, DIR);
    watch.entry_changed(DIR, b"a", None);
    let seq = watch.journal_seq(DIR.raw());
    watch.entry_changed(DIR, b"a", None);
    assert_eq!(
        watch.journal_seq(DIR.raw()),
        seq,
        "a second write to an undrained name is already covered"
    );
    drain_all(&watch, w);
    watch.entry_changed(DIR, b"a", None);
    assert!(
        watch.journal_seq(DIR.raw()) > seq,
        "once delivered, a change is recorded afresh"
    );
}

#[test]
fn a_covered_change_wakes_no_one_and_leaves_the_member_parked() {
    let watch = table(17);
    let w = arm(&watch, DIR);
    watch.entries_changed(DIR, b"lock", None);
    let pacing = Pacing {
        watcher: w,
        observed: watch.journal_seq(DIR.raw()),
        ..Pacing::default()
    };
    assert_eq!(watch.dir_state(DIR.raw(), &pacing, 0, 0), MemberState::Idle);
    assert!(parked(&watch, DIR));
    // The consumer is holding the report back; the name churns meanwhile.
    for _ in 0..100 {
        watch.entries_changed(DIR, b"lock", None);
    }
    assert!(
        parked(&watch, DIR),
        "a change already pending woke the parked member"
    );
}

#[test]
fn a_change_during_a_drain_is_recorded_again() {
    // The drain takes `a`, reads it, and has not committed yet when `a`
    // changes once more: that change must not be mistaken for covered, or
    // the drain would commit past it holding the older state.
    let watch = table(3);
    let w = arm(&watch, DIR);
    watch.entry_changed(DIR, b"a", None);
    let (taken, upto, _) = names_of(watch.take(DIR.raw(), w, usize::MAX, false));
    assert_eq!(taken, ["a"]);
    watch.entry_changed(DIR, b"a", None);
    commit(&watch, w, upto);
    assert_eq!(
        drain_all(&watch, w),
        ["a"],
        "the racing change is delivered"
    );
}

#[test]
fn a_new_watcher_starts_where_the_journal_stands() {
    let watch = table(4);
    let first = arm(&watch, DIR);
    watch.entries_changed(DIR, b"before", None);
    let second = arm(&watch, DIR);
    // A change to a name still pending for the first watcher is new to the
    // second, so it may not be dismissed as covered.
    watch.entries_changed(DIR, b"before", None);
    watch.entries_changed(DIR, b"after", None);
    assert_eq!(drain_all(&watch, first), ["before", "after"]);
    assert_eq!(drain_all(&watch, second), ["before", "after"]);
}

#[test]
fn a_journal_past_its_bound_asks_the_lagging_watcher_to_rescan() {
    let watch = table(5);
    let lagging = arm(&watch, DIR);
    let keeping_up = arm(&watch, DIR);
    let name_len = 200;
    let fits = JOURNAL_BYTES / name_cost(name_len);
    for i in 0..fits + 4 {
        let mut name = alloc::vec![b'x'; name_len];
        name[..8].copy_from_slice(&(i as u64).to_le_bytes());
        watch.entries_changed(DIR, &name, None);
        if i == fits / 2 {
            drain_all(&watch, keeping_up);
        }
    }
    assert!(is_rescan(&watch.take(
        DIR.raw(),
        lagging,
        usize::MAX,
        false
    )));
    let (names, _, _) = names_of(watch.take(DIR.raw(), keeping_up, usize::MAX, false));
    assert!(
        !names.is_empty() && names.len() <= fits,
        "the precise suffix survives"
    );
}

/// A source whose machine is small enough that the share every journal
/// together may hold is a few names.
struct TinyMachine;

impl FreeMemorySource for TinyMachine {
    fn free_bytes(&self) -> usize {
        1 << 20
    }

    fn total_bytes(&self) -> usize {
        2 << 20
    }
}

fn tiny_machine() -> &'static MemoryPressure {
    static SOURCE: TinyMachine = TinyMachine;
    Box::leak(Box::new(MemoryPressure::over(&SOURCE)))
}

#[test]
fn a_journal_holds_no_more_than_its_share_of_a_small_machine() {
    let watch = VolumeWatch::new([18; 16], NameMatching::Exact, tiny_machine(), registry());
    let w = arm(&watch, DIR);
    let (_, share) = watch.journal_limits();
    for i in 0..1_000u32 {
        watch.entries_changed(DIR, alloc::format!("{i:08}").as_bytes(), None);
    }
    assert!(journal_bytes(&watch) <= share);
    assert!(
        is_rescan(&watch.take(DIR.raw(), w, usize::MAX, false)),
        "the watcher still owed the names it gave up rescans"
    );
}

/// A machine whose journals together may hold 128 KiB.
struct SmallMachine;

impl FreeMemorySource for SmallMachine {
    fn free_bytes(&self) -> usize {
        32 << 20
    }

    fn total_bytes(&self) -> usize {
        64 << 20
    }
}

/// One user's watches on directories it never drains — enough of them, busy
/// enough, to fill every journal's budget first come first served — hold only
/// their equal shares, so another's journal still names its changes.
#[test]
fn a_watcher_that_never_drains_crowds_out_no_other_journal() {
    static SOURCE: SmallMachine = SmallMachine;
    let gauge: &'static MemoryPressure = Box::leak(Box::new(MemoryPressure::over(&SOURCE)));
    let registry = registry();
    let volume = [0xE8; 16];
    let claim = registry
        .claim(volume, NameMatching::Exact, gauge)
        .expect("claims");
    let table = claim.table();
    let on = |node: u64| FileId { volume, node };
    let (budget, _) = table.journal_limits();
    let mut hogs = Vec::new();
    // Names across ever more directories, stopping the moment every
    // journal's budget is full, if equal shares ever let it be.
    'fill: for node in 100..164 {
        hogs.push(
            ArmedWatch::arm(
                registry,
                on(node),
                0,
                ProcessId(0xF5_0010),
                64,
                Resolved::default(),
            )
            .expect("arms"),
        );
        for i in 0..64u32 {
            if registry.journal_bytes.load(Ordering::Relaxed) >= budget {
                break 'fill;
            }
            table.entries_changed(
                NodeId::from_raw(node),
                alloc::format!("{i:08}").as_bytes(),
                None,
            );
        }
    }
    let other = ArmedWatch::arm(
        registry,
        on(SUB.raw()),
        0,
        ProcessId(0xF5_0011),
        4,
        Resolved::default(),
    )
    .expect("arms");
    let (_, share) = table.journal_limits();
    for hog in &hogs {
        assert!(node_journal_bytes(table, NodeId::from_raw(hog.dir().node)) <= share);
    }
    table.entries_changed(SUB, b"mine", None);
    let Ok(Some(Drain::Names { names, .. })) = other.take(usize::MAX, false) else {
        panic!("the other journal still names its change");
    };
    assert_eq!(names.iter().collect::<Vec<_>>(), [b"mine".as_slice()]);
}

#[test]
fn a_journal_drained_empty_returns_its_memory() {
    let registry = registry();
    let watch = VolumeWatch::new([19; 16], NameMatching::Exact, unpressured(), registry);
    let w = arm(&watch, DIR);
    let armed = registry.journal_bytes.load(Ordering::Relaxed);
    assert_eq!(registry.journal_count.load(Ordering::Relaxed), 1);
    for i in 0..200u32 {
        watch.entries_changed(DIR, alloc::format!("burst-{i}").as_bytes(), None);
    }
    assert!(registry.journal_bytes.load(Ordering::Relaxed) > armed);
    drain_all(&watch, w);
    assert_eq!(registry.journal_bytes.load(Ordering::Relaxed), armed);
    watch.unwatch(DIR.raw(), w);
    assert_eq!(
        registry.journal_bytes.load(Ordering::Relaxed),
        0,
        "a released journal leaves nothing counted"
    );
    assert_eq!(registry.journal_count.load(Ordering::Relaxed), 0);
}

#[test]
fn a_name_folding_volume_always_rescans() {
    let watch = VolumeWatch::new(
        [6; 16],
        NameMatching::AsciiCaseInsensitive,
        unpressured(),
        registry(),
    );
    let w = arm(&watch, DIR);
    watch.entries_changed(DIR, b"README", None);
    assert!(is_rescan(&watch.take(DIR.raw(), w, usize::MAX, false)));
}

#[test]
fn memory_pressure_shrinks_a_journal_as_it_does_filesystem_metadata() {
    let (source, gauge) = pressured(free_for(PressureBand::Normal));
    let watch = VolumeWatch::new([7; 16], NameMatching::Exact, gauge, registry());
    let w = arm(&watch, DIR);
    source.set_free(free_for(PressureBand::Mild));
    gauge.sample();
    watch.entries_changed(DIR, b"kept", None);
    assert_eq!(
        drain_all(&watch, w),
        ["kept"],
        "a rescan is the dearer read, so tightening memory keeps names"
    );
    source.set_free(free_for(PressureBand::Severe));
    gauge.sample();
    watch.entries_changed(DIR, b"dropped", None);
    assert!(is_rescan(&watch.take(DIR.raw(), w, usize::MAX, false)));
    assert_eq!(
        journal_bytes(&watch),
        0,
        "under severe pressure a journal holds no names"
    );
}

#[test]
fn names_every_watcher_took_are_released() {
    let watch = table(8);
    let a = arm(&watch, DIR);
    let b = arm(&watch, DIR);
    watch.entries_changed(DIR, b"one", None);
    watch.entries_changed(DIR, b"two", None);
    drain_all(&watch, a);
    assert!(journal_bytes(&watch) > 0, "b has not taken them");
    drain_all(&watch, b);
    assert_eq!(journal_bytes(&watch), 0);
}

#[test]
fn a_drain_fills_its_budget_and_continues() {
    let watch = table(9);
    let w = arm(&watch, DIR);
    for name in [b"aa".as_slice(), b"bb", b"cc"] {
        watch.entries_changed(DIR, name, None);
    }
    let one = DirChange::len_for(true, 2);
    let (first, upto, more) = names_of(watch.take(DIR.raw(), w, one * 2, false));
    assert_eq!(first, ["aa", "bb"]);
    assert!(more);
    commit(&watch, w, upto);
    assert_eq!(drain_all(&watch, w), ["cc"]);
}

#[test]
fn a_drain_asked_to_rescan_takes_everything_recorded() {
    let watch = table(20);
    let w = arm(&watch, DIR);
    watch.entries_changed(DIR, b"secret-plan.txt", None);
    let Ok(Some(drain)) = watch.take(DIR.raw(), w, usize::MAX, true) else {
        panic!("the watcher is armed");
    };
    assert!(
        matches!(drain, Drain::Rescan { .. }),
        "no name is handed out"
    );
    commit(&watch, w, drain.upto());
    assert!(drain_all(&watch, w).is_empty(), "the recorded name is gone");
}

#[test]
fn a_relocated_directory_makes_its_watchers_rescan() {
    let watch = table(10);
    let w = arm(&watch, DIR);
    watch.entries_changed(DIR, b"a", None);
    watch.node_relocated(DIR);
    assert!(is_rescan(&watch.take(DIR.raw(), w, usize::MAX, false)));
}

#[test]
fn an_entry_inside_a_subfolder_updates_the_subfolders_own_entry() {
    let watch = table(11);
    let w = arm(&watch, DIR);
    watch.entries_changed(SUB, b"inner.txt", Some((DIR, b"sub")));
    assert_eq!(drain_all(&watch, w), ["sub"]);
}

#[test]
fn a_member_is_idle_then_ready_then_held_by_its_latency() {
    let watch = table(12);
    let w = arm(&watch, DIR);
    let observed = watch.journal_seq(DIR.raw());
    let latency = 100;
    let state = |observed, reported_at, now| {
        let pacing = Pacing {
            watcher: w,
            observed,
            epochs: Epochs::default(),
            latency_ns: latency,
            reported_at,
        };
        watch.dir_state(DIR.raw(), &pacing, 0, now)
    };
    assert_eq!(state(observed, None, 1_000), MemberState::Idle);
    assert!(parked(&watch, DIR), "an idle scan leaves the node parked");
    watch.entries_changed(DIR, b"x", None);
    assert!(!parked(&watch, DIR), "the change consumed the park");
    // A member that never reported reports its first change at once ...
    assert_eq!(state(observed, None, 0), MemberState::Ready);
    // ... and holds the next until its latency has run.
    let reported = watch.journal_seq(DIR.raw());
    watch.entries_changed(DIR, b"y", None);
    assert_eq!(state(reported, Some(1_000), 1_050), MemberState::Due(1_100));
    assert_eq!(state(reported, Some(1_000), 1_100), MemberState::Ready);
    // ... as does one whose previous report is older than its latency.
    assert_eq!(state(reported, Some(500), 1_050), MemberState::Ready);
}

#[test]
fn every_epoch_a_path_resolves_against_readies_its_members_paced() {
    let watch = table(21);
    let w = arm(&watch, DIR);
    let reported_under = |epochs| Pacing {
        watcher: w,
        observed: watch.journal_seq(DIR.raw()),
        epochs,
        latency_ns: 100,
        reported_at: Some(1_000),
    };
    let baseline = reported_under(watch.epochs(0));
    assert_eq!(
        watch.dir_state(DIR.raw(), &baseline, 0, 2_000),
        MemberState::Idle
    );
    assert_eq!(
        watch.dir_state(DIR.raw(), &baseline, 1, 1_050),
        MemberState::Due(1_100),
        "a moved mount table is a change, paced like any other"
    );
    watch.paths_moved();
    assert_eq!(
        watch.dir_state(DIR.raw(), &baseline, 0, 2_000),
        MemberState::Ready
    );
    watch.access_moved();
    assert_eq!(
        watch.dir_state(DIR.raw(), &baseline, 0, 2_000),
        MemberState::Ready
    );
}

/// Until the watcher's drain re-resolves its path, the directory may no
/// longer be its to follow, so a reported move is not followed by a report of
/// every later change.
#[test]
fn a_reported_move_holds_its_member_until_the_watcher_drains() {
    let watch = table(22);
    let w = arm(&watch, DIR);
    let reported_under = |epochs| Pacing {
        watcher: w,
        observed: watch.journal_seq(DIR.raw()),
        epochs,
        latency_ns: 0,
        reported_at: Some(1_000),
    };
    watch.paths_moved();
    let reported = reported_under(watch.epochs(0));
    watch.entries_changed(DIR, b"later", None);
    watch.access_moved();
    assert_eq!(
        watch.dir_state(DIR.raw(), &reported, 0, 2_000),
        MemberState::Idle
    );
    assert!(!parked(&watch, DIR), "a held member waits on no change");
    commit(&watch, w, 0);
    assert_eq!(
        watch.dir_state(DIR.raw(), &reported, 0, 2_000),
        MemberState::Ready,
        "drained under the epochs now, the change it held reports"
    );
    drain_all(&watch, w);
    assert_eq!(
        watch.dir_state(DIR.raw(), &reported_under(watch.epochs(0)), 0, 2_000),
        MemberState::Idle,
        "a member that reported the new epochs is idle again"
    );
}

/// A move that lands while a member is parked on the epochs it replaced wakes
/// it, and further moves before it parks again cost nothing.
#[test]
fn an_epoch_move_wakes_a_parked_member_once() {
    let watch = table(23);
    let w = arm(&watch, DIR);
    let at = Pacing {
        watcher: w,
        observed: watch.journal_seq(DIR.raw()),
        epochs: watch.epochs(0),
        latency_ns: 0,
        reported_at: None,
    };
    let epoch_parked = || *watch.registry.epoch_park.lock();
    assert!(!epoch_parked());
    assert_eq!(watch.dir_state(DIR.raw(), &at, 0, 0), MemberState::Idle);
    assert!(epoch_parked(), "an idle member waits on the epochs");
    watch.paths_moved();
    assert!(!epoch_parked(), "the move took the park");
    watch.paths_moved();
    assert!(!epoch_parked());
    assert_eq!(watch.dir_state(DIR.raw(), &at, 0, 0), MemberState::Ready);
}

/// Every epoch is the machine's, so a move on one volume reaches a watcher
/// whose directory is on another: the moved directory may be its ancestor.
#[test]
fn a_move_on_another_volume_reaches_a_watcher_here() {
    let registry = registry();
    let here = VolumeWatch::new([24; 16], NameMatching::Exact, unpressured(), registry);
    let there = VolumeWatch::new([25; 16], NameMatching::Exact, unpressured(), registry);
    let w = arm(&here, DIR);
    let at = Pacing {
        watcher: w,
        observed: here.journal_seq(DIR.raw()),
        epochs: here.epochs(0),
        latency_ns: 0,
        reported_at: None,
    };
    there.access_moved();
    assert_eq!(here.dir_state(DIR.raw(), &at, 0, 0), MemberState::Ready);
}

/// A retired table revalidates every journal, which its members report once;
/// the retirement itself is never reported again.
#[test]
fn a_retired_tables_members_report_once() {
    let registry = registry();
    let volume = [0xE9; 16];
    let old = registry
        .claim(volume, NameMatching::Exact, unpressured())
        .expect("claims");
    let table = Arc::clone(old.table());
    let armed = ArmedWatch::arm(
        registry,
        FileId {
            volume,
            node: DIR.raw(),
        },
        0,
        ProcessId(0xF5_0012),
        4,
        Resolved::default(),
    )
    .expect("arms");
    let at = armed.position().expect("armed");
    let pacing = Pacing {
        watcher: armed.watcher(),
        observed: at.at,
        epochs: at.epochs,
        latency_ns: 0,
        reported_at: None,
    };
    drop(old);
    let _new = registry
        .claim(volume, NameMatching::AsciiCaseInsensitive, unpressured())
        .expect("claims afresh");
    assert!(table.retired());
    assert_eq!(
        table.dir_state(DIR.raw(), &pacing, 0, 0),
        MemberState::Ready
    );
    let reported = Pacing {
        observed: table.journal_seq(DIR.raw()),
        reported_at: Some(0),
        ..pacing
    };
    assert_eq!(
        table.dir_state(DIR.raw(), &reported, 0, 0),
        MemberState::Idle,
        "retirement is an edge, not a level"
    );
}

#[test]
fn a_member_of_a_closed_watch_never_reports() {
    let watch = table(13);
    let keep = arm(&watch, DIR);
    let closed = arm(&watch, DIR);
    watch.unwatch(DIR.raw(), closed);
    watch.entries_changed(DIR, b"x", None);
    let member = |watcher| Pacing {
        watcher,
        ..Pacing::default()
    };
    assert_eq!(
        watch.dir_state(DIR.raw(), &member(closed), 0, 0),
        MemberState::Idle
    );
    assert_eq!(
        watch.dir_state(DIR.raw(), &member(keep), 0, 0),
        MemberState::Ready
    );
}

#[test]
fn file_members_follow_their_nodes_generation() {
    let watch = table(14);
    let dir_base = watch.file_member_add(DIR.raw()).expect("adds");
    let file_base = watch.file_member_add(FILE.raw()).expect("adds");
    watch.entry_changed(DIR, b"f", Some(FILE));
    assert_eq!(
        watch.generation(DIR.raw()),
        dir_base,
        "a write is not a rotation"
    );
    assert!(watch.file_ready(FILE.raw(), file_base));
    watch.entries_changed(DIR, b"f", None);
    assert!(watch.file_ready(DIR.raw(), dir_base), "a create is");
    watch.node_changed(FILE);
    assert_eq!(watch.generation(FILE.raw()), file_base.wrapping_add(2));
    watch.file_member_remove(DIR.raw());
    watch.file_member_remove(FILE.raw());
    assert!(!watch.active(), "an unwatched volume is idle again");
}

#[test]
fn an_unwatched_volume_records_nothing() {
    let watch = table(15);
    watch.entries_changed(DIR, b"x", None);
    assert!(!watch.active());
    assert!(watch.nodes.lock().is_empty());
}

#[test]
fn the_last_watcher_releases_the_journal_and_node() {
    let watch = table(16);
    let w = arm(&watch, DIR);
    assert!(watch.active());
    watch.unwatch(DIR.raw(), w);
    assert!(!watch.active());
    assert!(watch.nodes.lock().get(&DIR.raw()).is_none());
}

fn dir_on(volume: u8) -> FileId {
    FileId {
        volume: [volume; 16],
        node: DIR.raw(),
    }
}

#[test]
fn arming_needs_a_claimed_table_and_counts_against_the_limit() {
    let registry = registry();
    let process = ProcessId(0xF5_0001);
    let at = Resolved::default();
    assert_eq!(
        ArmedWatch::arm(registry, dir_on(0xE0), 0, process, 8, at).map(|_| ()),
        Err(Errno::NotImplemented)
    );
    let claim = registry
        .claim([0xE1; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    let first = ArmedWatch::arm(registry, dir_on(0xE1), 0, process, 2, at).expect("first");
    let second = ArmedWatch::arm(registry, dir_on(0xE1), 0, process, 2, at).expect("second");
    assert_eq!(registry.usage(process), 2);
    assert_eq!(
        ArmedWatch::arm(registry, dir_on(0xE1), 0, process, 2, at).map(|_| ()),
        Err(Errno::LimitExceeded)
    );
    drop(first);
    assert_eq!(registry.usage(process), 1);
    drop(second);
    assert_eq!(registry.usage(process), 0);
    assert!(
        !claim.table().active(),
        "dropping the watches released the journal"
    );
}

#[test]
fn a_spent_watch_stops_reporting_but_stays_charged() {
    let registry = registry();
    let process = ProcessId(0xF5_0002);
    let _claim = registry
        .claim([0xE2; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    let armed =
        ArmedWatch::arm(registry, dir_on(0xE2), 0, process, 4, Resolved::default()).expect("arms");
    assert!(armed.position().is_some());
    armed.spend();
    assert!(matches!(armed.take(usize::MAX, false), Ok(None)));
    assert!(armed.position().is_none(), "a spent watch takes no member");
    assert_eq!(registry.usage(process), 1);
    drop(armed);
    assert_eq!(registry.usage(process), 0);
}

#[test]
fn a_returning_volume_is_watched_through_the_table_it_left() {
    let registry = registry();
    let process = ProcessId(0xF5_0003);
    let claim = registry
        .claim([0xE3; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    let table = Arc::clone(claim.table());
    let armed =
        ArmedWatch::arm(registry, dir_on(0xE3), 0, process, 4, Resolved::default()).expect("arms");
    let file = FILE.raw();
    let (_, baseline) = registry
        .file_member_add(FileId {
            volume: [0xE3; 16],
            node: file,
        })
        .expect("adds");
    // The volume leaves: everything watching it hears so.
    claim.reference().release();
    let Ok(Some(drain)) = armed.take(usize::MAX, false) else {
        panic!("the watch is armed");
    };
    assert!(matches!(drain, Drain::Rescan { .. }));
    armed.commit(drain.upto(), Resolved::default());
    assert!(
        table.file_ready(file, baseline),
        "a File member sees the departure"
    );
    drop(claim);
    // ... and returns, through the same table, so the watch carries on.
    let back = registry
        .claim([0xE3; 16], NameMatching::Exact, unpressured())
        .expect("reclaims");
    assert!(Arc::ptr_eq(back.table(), &table));
    back.table().entries_changed(DIR, b"after", None);
    let (names, _, _) = names_of(armed.take(usize::MAX, false));
    assert_eq!(names, ["after"]);
    table.file_member_remove(file);
}

#[test]
fn a_second_volume_with_the_same_id_gets_no_table() {
    let registry = registry();
    let first = registry
        .claim([0xE4; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    assert!(
        registry
            .claim([0xE4; 16], NameMatching::Exact, unpressured())
            .is_none(),
        "a duplicate must not report into the first volume's watchers"
    );
    assert!(registry
        .volume([0xE4; 16])
        .is_some_and(|t| Arc::ptr_eq(&t, first.table())));
    drop(first);
    assert!(registry
        .claim([0xE4; 16], NameMatching::Exact, unpressured())
        .is_some());
}

#[test]
fn a_volume_whose_names_now_match_differently_retires_its_old_table() {
    let registry = registry();
    let old = registry
        .claim([0xE5; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    let old_table = Arc::clone(old.table());
    drop(old);
    let new = registry
        .claim(
            [0xE5; 16],
            NameMatching::AsciiCaseInsensitive,
            unpressured(),
        )
        .expect("claims afresh");
    assert!(!Arc::ptr_eq(new.table(), &old_table));
    assert!(old_table.retired());
    assert!(!new.table().retired());
}

#[test]
fn a_table_nothing_holds_is_dropped_from_the_registry() {
    let registry = registry();
    drop(
        registry
            .claim([0xE6; 16], NameMatching::Exact, unpressured())
            .expect("claims"),
    );
    assert!(registry.volume([0xE6; 16]).is_some());
    let _other = registry
        .claim([0xE7; 16], NameMatching::Exact, unpressured())
        .expect("claims");
    assert!(
        registry.volume([0xE6; 16]).is_none(),
        "an unclaimed, unwatched table is swept"
    );
}

#[test]
fn changed_names_iterate_in_recorded_order() {
    let all: [&[u8]; 3] = [b"b", b"a", b"long-name"];
    let bytes = all.iter().map(|n| n.len()).sum();
    let names = ChangedNames::with_exact(all.into_iter(), 3, bytes).expect("fits");
    let got: Vec<&[u8]> = names.iter().collect();
    assert_eq!(got, all);
    assert_eq!(names.len(), 3);
}
