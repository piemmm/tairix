//! What a launch from the library asks for and counts as, what it shows
//! running, and its record: read once, edited at once, written behind, and
//! never stored over a record that was not read.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::{Errno, ProcId, PROC_ID_LEN};
use tairix_appdata::fake::FakeService;
use tairix_proglib::personal::{key_of, FORGOTTEN};
use tairix_proglib::{
    BundlePath, Catalog, DisplayName, Edit, EntryId, LibraryCategory, LibraryEntry, Personal,
    RecordFull,
};

use super::{
    elevated_launch, launched_entries, load_personal, running_bundles, store_personal,
    LauncherMemory, MAX_UNCONFIRMED, RUN_ELEVATED_PURPOSE,
};
use crate::effects::{RETRY_MAX_NS, RETRY_NS};
use crate::elevate::Elevation;
use crate::launch::LaunchTable;
use crate::AppGroup;
use tairix_taskbar::AppIdentity;

fn service() -> FakeService {
    FakeService::for_word("desktop")
}

fn id(stem: &str) -> EntryId {
    EntryId::new(&format!("os.tairix.{stem}")).expect("id")
}

fn key(stem: &str) -> String {
    key_of(&id(stem))
}

/// A catalogued Office program `name` whose bundle is
/// `/System/Applications/<bundle>.app`, listed as `stem`.
fn entry(stem: &str, name: &str, bundle: &str) -> LibraryEntry {
    LibraryEntry::new(
        id(stem),
        DisplayName::new(name).expect("name"),
        BundlePath::new(&format!("/System/Applications/{bundle}.app")).expect("bundle"),
        LibraryCategory::Office,
        None,
    )
}

/// Write and Calc, and Sheets listed against Calc's bundle too.
fn catalog() -> Catalog {
    let mut catalog = Catalog::new();
    for entry in [
        entry("write", "Write", "write"),
        entry("calc", "Calc", "calc"),
        entry("calc.sheets", "Sheets", "calc"),
    ] {
        catalog.insert(entry).expect("fits");
    }
    catalog
}

/// A memory that has read `record`, its launcher told.
fn read(record: Personal) -> LauncherMemory {
    let mut memory = LauncherMemory::new();
    assert!(memory.next_load(0));
    memory.loaded(0, Some(record));
    let _ = memory.take_moved();
    memory
}

#[test]
fn a_fresh_account_reads_an_empty_record() {
    let loaded = load_personal(&mut service());
    assert_eq!(loaded.personal, Some(Personal::new()));
    assert!(loaded.warnings.is_empty());
}

#[test]
fn a_stored_record_reads_back_and_one_not_kept_is_said() {
    let mut host = service().with_store(&format!(
        "{} = favourite=1\n{} = favourite=one\nother.key = kept\n",
        key("calc"),
        key("chess")
    ));
    let loaded = load_personal(&mut host);
    let personal = loaded.personal.expect("read");
    assert_eq!(personal.keep(&id("calc")).favourite(), Some(1));
    assert_eq!(personal.len(), 1, "the unreadable record is left out");
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].contains(&key("chess")));
}

#[test]
fn an_unreadable_store_says_so_and_yields_no_record_to_store_over() {
    let mut host = service();
    host.refusal().set(Some(Errno::DeviceOffline));
    let loaded = load_personal(&mut host);
    assert_eq!(loaded.personal, None);
    assert!(loaded.warnings[0].contains("DeviceOffline"));
}

#[test]
fn storing_sets_and_removes_and_answers_what_the_store_holds() {
    let mut host = service().with_store(&format!(
        "{} = favourite=2\nother.key = kept\n",
        key("chess")
    ));
    let changes = vec![
        (key("calc"), Some(String::from("favourite=1"))),
        (key("chess"), None),
    ];
    assert_eq!(store_personal(&mut host, &changes), Ok(changes.clone()));
    let held = host.committed();
    assert_eq!(held.get(&key("calc")), Some("favourite=1"));
    assert_eq!(
        held.get(&key("chess")),
        None,
        "a removal leaves no tombstone it does not need"
    );
    assert_eq!(held.get("other.key"), Some("kept"));
}

#[test]
fn a_removal_a_policy_would_undo_is_masked() {
    let mut host = service().with_policy(&format!("{} = favourite=1\n", key("calc")));
    assert_eq!(
        load_personal(&mut host)
            .personal
            .expect("read")
            .keep(&id("calc"))
            .favourite(),
        Some(1),
        "the administrator seeded a favourite"
    );
    assert_eq!(
        store_personal(&mut host, &[(key("calc"), None)]),
        Ok(vec![(key("calc"), Some(String::from(FORGOTTEN)))])
    );
    assert_eq!(host.committed().get(&key("calc")), Some(FORGOTTEN));
    let next = load_personal(&mut host).personal.expect("read");
    assert_eq!(
        next.keep(&id("calc")).favourite(),
        None,
        "what was removed stays removed"
    );
}

#[test]
fn a_refused_write_is_withdrawn_so_no_later_write_lands_it() {
    let mut host = service();
    let refusal = host.commit_refusal();
    refusal.set(Some(Errno::DeviceOffline));
    assert_eq!(
        store_personal(
            &mut host,
            &[(key("calc"), Some(String::from("favourite=1")))]
        ),
        Err(Errno::DeviceOffline)
    );
    refusal.set(None);
    assert!(store_personal(
        &mut host,
        &[(key("chess"), Some(String::from("favourite=2")))]
    )
    .is_ok());
    assert_eq!(
        host.committed().get(&key("calc")),
        None,
        "what was refused never lands"
    );
    assert_eq!(host.committed().get(&key("chess")), Some("favourite=2"));
}

#[test]
fn a_mask_the_store_refuses_answers_what_shows() {
    let mut host = service()
        .with_policy(&format!("{} = favourite=1\n", key("calc")))
        .refusing_commits_after(1, Errno::NoSpace);
    assert_eq!(
        store_personal(&mut host, &[(key("calc"), None)]),
        Ok(vec![(key("calc"), Some(String::from("favourite=1")))]),
        "the removal landed, its mask did not, and the policy's record shows"
    );
}

#[test]
fn a_refused_store_keeps_nothing() {
    let mut host = service();
    host.refusal().set(Some(Errno::NoSpace));
    assert_eq!(
        store_personal(
            &mut host,
            &[(key("calc"), Some(String::from("favourite=1")))]
        ),
        Err(Errno::NoSpace)
    );
    host.refusal().set(None);
    assert_eq!(host.committed().get(&key("calc")), None);
}

#[test]
fn an_edit_shows_at_once_but_nothing_is_stored_before_the_record_is_read() {
    let mut memory = LauncherMemory::new();
    assert!(!memory.is_read());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    assert!(memory.take_moved());
    assert_eq!(memory.shelf(None).favourites, [id("calc")], "shown at once");
    assert_eq!(
        memory.next_store(),
        None,
        "never stored over a record not read"
    );
    assert!(!memory.writing());

    assert!(memory.next_load(0));
    assert!(!memory.next_load(0), "one read at a time");
    let mut held = Personal::new();
    held.apply(&Edit::Favourite(id("chess"))).expect("room");
    memory.loaded(0, Some(held));
    assert!(memory.take_moved());
    assert_eq!(
        memory.shelf(None).favourites,
        [id("chess"), id("calc")],
        "the edit replays over what was read"
    );
    assert!(
        !memory.next_load(u64::MAX),
        "a read record is never read again"
    );
    let ask = memory.next_store().expect("owed now");
    assert_eq!(
        ask.changes,
        [(key("calc"), Some(String::from("favourite=2")))]
    );
    assert!(memory.writing());
}

#[test]
fn edits_made_while_a_write_is_in_flight_gather_into_the_next() {
    let mut memory = read(Personal::new());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    let first = memory.next_store().expect("owed");
    memory.apply(Edit::Favourite(id("chess"))).expect("room");
    memory
        .apply(Edit::Launched(id("write"), Some(100)))
        .expect("room");
    assert_eq!(memory.next_store(), None, "one write in flight at a time");

    // The store lands exactly what each ask carried.
    assert!(memory
        .stored(first.number, Ok(first.changes.clone()))
        .is_empty());
    let second = memory.next_store().expect("the edits made meanwhile");
    assert!(second.number > first.number);
    let keys: Vec<String> = second.changes.iter().map(|(key, _)| key.clone()).collect();
    assert_eq!(keys, [key("chess"), key("write")], "one write for both");

    assert!(memory
        .stored(second.number, Ok(second.changes.clone()))
        .is_empty());
    assert!(!memory.writing());
    let shelf = memory.shelf(Some(100));
    assert_eq!(shelf.favourites, [id("calc"), id("chess")]);
    assert!(shelf.weights.contains_key(&id("write")));
}

#[test]
fn a_refused_write_drops_its_edits_so_what_is_shown_is_what_returns() {
    let mut memory = read(Personal::new());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    let _ = memory.take_moved();
    let ask = memory.next_store().expect("owed");
    assert!(memory.stored(ask.number, Err(Errno::NoSpace)).is_empty());
    assert!(memory.take_moved(), "the launcher is told");
    assert!(
        memory.shelf(None).favourites.is_empty(),
        "what the next start would restore"
    );
    assert!(!memory.writing());
}

#[test]
fn an_answer_to_another_write_changes_nothing() {
    let mut memory = read(Personal::new());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    let ask = memory.next_store().expect("owed");
    let before = memory.clone();
    assert!(memory
        .stored(ask.number + 1, Err(Errno::NoSpace))
        .is_empty());
    assert_eq!(memory, before);
}

#[test]
fn what_the_store_answers_is_what_is_kept() {
    // Another session of the account placed a favourite first; the store's
    // answer, not the ask, is the record.
    let mut memory = read(Personal::new());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    let ask = memory.next_store().expect("owed");
    let answered = vec![(key("calc"), Some(String::from("favourite=2")))];
    assert!(memory.stored(ask.number, Ok(answered)).is_empty());
    assert_eq!(memory.live().keep(&id("calc")).favourite(), Some(2));
}

#[test]
fn an_answer_the_record_cannot_read_is_said_and_keeps_nothing() {
    let mut memory = read(Personal::new());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    let ask = memory.next_store().expect("owed");
    let answered = vec![(key("calc"), Some(String::from("favourite=first")))];
    assert_eq!(memory.stored(ask.number, Ok(answered)), [key("calc")]);
    assert_eq!(memory.live().keep(&id("calc")).favourite(), None);
}

#[test]
fn a_failed_read_is_tried_again_after_a_wait_that_doubles_to_its_bound() {
    let mut memory = LauncherMemory::new();
    let mut now = 0;
    let mut wait = RETRY_NS;
    assert!(memory.next_load(now));
    for _ in 0..40 {
        memory.loaded(now, None);
        assert!(!memory.is_read());
        assert_eq!(
            memory.park_deadline_ns(now, u64::MAX),
            wait,
            "the loop wakes when the read is due"
        );
        assert!(!memory.next_load(now + wait - 1));
        now += wait;
        assert!(memory.next_load(now));
        wait = (wait * 2).min(RETRY_MAX_NS);
    }
    assert_eq!(wait, RETRY_MAX_NS, "the wait is bounded");
    memory.loaded(now, Some(Personal::new()));
    assert!(memory.is_read());
    assert_eq!(
        memory.park_deadline_ns(now, 5),
        5,
        "a read record owes no wake"
    );
}

#[test]
fn the_edits_held_unconfirmed_are_bounded() {
    let mut memory = LauncherMemory::new();
    for n in 0..MAX_UNCONFIRMED {
        memory
            .apply(Edit::Launched(id(&format!("p{n}")), Some(5)))
            .expect("held");
    }
    let before = memory.clone();
    assert_eq!(memory.apply(Edit::Favourite(id("calc"))), Err(RecordFull));
    assert_eq!(memory, before, "a refused edit changes nothing");
}

#[test]
fn an_edit_survives_into_the_next_session() {
    let mut host = service();
    let mut memory = LauncherMemory::new();
    assert!(memory.next_load(0));
    memory.loaded(0, load_personal(&mut host).personal);
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    memory
        .apply(Edit::Launched(id("chess"), Some(1_000)))
        .expect("room");
    let ask = memory.next_store().expect("owed");
    let result = store_personal(&mut host, &ask.changes);
    assert!(memory.stored(ask.number, result).is_empty());
    let next = load_personal(&mut host).personal.expect("read");
    assert_eq!(
        next,
        *memory.live(),
        "the next start restores what was shown"
    );
}

#[test]
fn running_as_an_administrator_asks_to_start_the_programs_own_entry_binary() {
    // The account's overlay named the terminal's entry "Calculator"; the
    // prompt names what will run, as its signed manifest says.
    let attested = AppIdentity {
        name: String::from("Terminal"),
        version: String::from("1.0"),
        purpose: None,
        author: None,
    };
    let request = elevated_launch(&entry("calc", "Calculator", "terminal"), &attested);
    assert_eq!(request.program, "/System/Applications/terminal.app/Run");
    assert_eq!(request.elevation, Elevation::Launch);
    assert_eq!(
        request.requester, "Terminal",
        "the prompt names the bundle's attested name, never the entry's"
    );
    assert_eq!(
        request.bundle.as_deref(),
        tairix_appstore::bundle_of_entry(&request.program),
        "and pictures the bundle it starts"
    );
    assert_eq!(request.purpose, RUN_ELEVATED_PURPOSE);
    assert_eq!(request.window, None, "the desktop's own request");
}

#[test]
fn a_launch_counts_for_every_entry_naming_its_bundle_and_nothing_else() {
    let used = [
        "/System/Applications/calc.app/Run",
        "/System/Commands/ls.app/Run",
        "/System/Applications/write.app",
        "/System/Applications/write.app/Run",
    ]
    .map(String::from);
    assert_eq!(
        launched_entries(&catalog(), &used),
        [id("calc"), id("calc.sheets"), id("write")]
    );
    assert!(launched_entries(&catalog(), &[]).is_empty());
}

#[test]
fn what_runs_is_each_attested_slot_and_each_child_launched_from_a_bundle() {
    let slot = |tag: u8, bundle: Option<&str>| AppGroup {
        owner: ProcId::from_raw([tag; PROC_ID_LEN]),
        bundle: bundle.map(String::from),
        windows: Vec::new(),
    };
    let strip = [
        slot(1, Some("/System/Applications/write.app")),
        slot(2, None),
    ];
    let mut launched = LaunchTable::new();
    launched.record(7, "Calc", "/System/Applications/calc.app/Run");
    launched.record(8, "Write", "/System/Applications/write.app/Run");
    launched.record(9, "a program", "/System/Commands/ls");
    assert_eq!(
        running_bundles(&strip, &launched),
        BTreeSet::from([
            String::from("/System/Applications/calc.app"),
            String::from("/System/Applications/write.app"),
        ])
    );
}

#[test]
fn a_write_the_worker_could_not_take_keeps_its_edits_for_the_next() {
    let mut memory = read(Personal::new());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    let ask = memory.next_store().expect("owed");
    assert!(memory.next_store().is_none(), "one write at a time");
    memory.unasked(ask.number);
    let again = memory.next_store().expect("still owed");
    assert_eq!(again.changes, ask.changes, "nothing was dropped");
    assert_ne!(again.number, ask.number);
    memory.unasked(ask.number);
    assert!(
        memory.next_store().is_none(),
        "an answer to another ask changes nothing"
    );
}

#[test]
fn edits_made_before_the_record_is_read_are_held_unconfirmed() {
    let mut memory = LauncherMemory::new();
    assert!(!memory.holds_unconfirmed());
    memory.apply(Edit::Favourite(id("calc"))).expect("room");
    assert!(memory.holds_unconfirmed());
    assert!(
        !memory.writing(),
        "nothing is stored over a record not read"
    );
}
