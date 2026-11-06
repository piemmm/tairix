//! Unit tests for the record of where the user put their icons
//! ([`crate::pinboard`]): kept in the session's private scope, read
//! tolerantly, written behind, and never stored over a record not read. The
//! menu's row model is tested with the desktop's commands it names.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId};
use tairix_appdata::fake::FakeService;
use tairix_proglib::personal::FORGOTTEN;
use tairix_wallpaper::IconFlow;

use super::{
    load_arrangement, Arrangement, ArrangementEdit, ArrangementFull, ArrangementMemory, Place,
    PlaceKey, Placed, MAX_PLACED,
};
use crate::launcher::{store_personal, Kept};

fn service() -> FakeService {
    FakeService::for_word("desktop")
}

/// The file numbered `node` on one volume.
fn file(node: u64) -> FileId {
    FileId {
        volume: [7; 16],
        node,
    }
}

fn placed(across: u32, down: u32, node: u64) -> Placed {
    Placed {
        id: file(node),
        at: Place {
            across,
            down,
            from: IconFlow::Leading,
        },
        order: 0,
    }
}

/// Keep `name` at `placed`.
fn put(name: &str, placed: Placed) -> ArrangementEdit {
    ArrangementEdit::Set(vec![(String::from(name), Some(placed))])
}

/// An arrangement keeping exactly `name` at `at`.
fn holding(name: &str, at: Placed) -> Arrangement {
    let mut arrangement = Arrangement::default();
    arrangement.make(&put(name, at)).expect("room");
    arrangement
}

#[test]
fn a_place_is_kept_under_its_name_and_reads_back_exactly() {
    let mut arrangement = Arrangement::default();
    let key = PlaceKey::of("notes.txt").spelled();
    assert_eq!(
        arrangement.make(&put("notes.txt", placed(4096, 8192, 9))),
        Ok(vec![key.clone()])
    );
    let value = arrangement.kept(&key).expect("kept");
    let (read, refused) = Arrangement::read([(key.as_str(), value.as_str())].into_iter());
    assert!(refused.is_empty());
    assert_eq!(read, arrangement);
    assert_eq!(read.place_of("notes.txt"), Some(placed(4096, 8192, 9)));
}

/// A key is read again in every later session and by every later build, so
/// it is the XXH64 of the name pinned here, and within the key grammar
/// whatever the name holds.
#[test]
fn a_key_is_the_same_in_every_session_and_inside_the_key_grammar() {
    let key = PlaceKey::of("Holiday Photos (2026)");
    assert_eq!(key.spelled(), "pinboard.2ff7ab664822b0ad");
    assert_ne!(key, PlaceKey::of("holiday photos (2026)"));
    assert_eq!(tairix_appconf::validate_key(&key.spelled()), Ok(()));
}

/// A drive keeps its place under the identity its volume's content carries:
/// the key its root names, spelled within the key grammar and read back
/// exactly.
#[test]
fn a_drive_keeps_its_place_under_its_volume() {
    let identity = [0xab; 16];
    let root = tairix_browse::mount::volume_root(identity);
    let key = PlaceKey::of(&root);
    assert_eq!(key, PlaceKey::of_volume(identity));
    assert!(key.is_volume() && !PlaceKey::of("USB").is_volume());
    let spelled = key.spelled();
    assert_eq!(spelled, "pinboard.volume.abababababababababababababababab");
    assert_eq!(tairix_appconf::validate_key(&spelled), Ok(()));
    let mut arrangement = Arrangement::default();
    assert_eq!(
        arrangement.make(&put(&root, placed(4096, 0, 0))),
        Ok(vec![spelled.clone()])
    );
    let value = arrangement.kept(&spelled).expect("kept");
    let (read, refused) = Arrangement::read([(spelled.as_str(), value.as_str())].into_iter());
    assert!(refused.is_empty());
    assert_eq!(read.place_of(&root), Some(placed(4096, 0, 0)));
    assert_eq!(
        PlaceKey::read("pinboard.volume.ABABABABABABABABABABABABABABABAB"),
        None
    );
}

#[test]
fn a_place_it_cannot_read_is_left_out_and_named() {
    let good = PlaceKey::of("a").spelled();
    let value = holding("a", placed(1, 2, 3)).kept(&good).expect("kept");
    let (read, refused) = Arrangement::read(
        [
            (good.as_str(), value.as_str()),
            ("pinboard.0123456789abcdef", "nonsense"),
            ("pinboard.short", value.as_str()),
            ("pinboard.0123456789ABCDEF", value.as_str()),
            ("pinboard.0123456789abcdee", FORGOTTEN),
            ("library.os.tairix.calc", "favourite=1"),
        ]
        .into_iter(),
    );
    assert_eq!(read.places().count(), 1, "the one place it can read");
    assert_eq!(
        refused,
        [
            "pinboard.0123456789abcdef",
            "pinboard.short",
            "pinboard.0123456789ABCDEF"
        ],
        "a forgotten place and another record's keys are not refusals"
    );
}

#[test]
fn the_record_keeps_at_most_its_bound_and_refuses_the_next_set_whole() {
    let mut arrangement = Arrangement::default();
    let all: Vec<(String, Option<Placed>)> = (0..MAX_PLACED)
        .map(|n| (alloc::format!("{n}.txt"), Some(placed(1, 1, 1))))
        .collect();
    arrangement
        .make(&ArrangementEdit::Set(all))
        .expect("room for the bound");
    let before = arrangement.clone();
    assert_eq!(
        arrangement.make(&ArrangementEdit::Set(vec![
            (String::from("0.txt"), None),
            (String::from("extra.txt"), Some(placed(2, 2, 2))),
        ])),
        Ok(vec![
            PlaceKey::of("0.txt").spelled(),
            PlaceKey::of("extra.txt").spelled()
        ]),
        "a place given up makes room in the same set"
    );
    let mut full = before.clone();
    assert_eq!(
        full.make(&put("extra.txt", placed(2, 2, 2))),
        Err(ArrangementFull)
    );
    assert_eq!(full, before, "a refused set changes nothing");
}

#[test]
fn a_rename_moves_the_place_to_the_new_name() {
    let mut arrangement = holding("old.txt", placed(5, 6, 1));
    let from = PlaceKey::of("old.txt");
    assert_eq!(
        arrangement.make(&ArrangementEdit::Renamed {
            from,
            to: String::from("new.txt"),
            id: file(2),
        }),
        Ok(vec![from.spelled(), PlaceKey::of("new.txt").spelled()])
    );
    assert_eq!(arrangement.place_of("old.txt"), None);
    assert_eq!(arrangement.place_of("new.txt"), Some(placed(5, 6, 2)));
    assert_eq!(
        arrangement.make(&ArrangementEdit::Renamed {
            from: PlaceKey::of("missing.txt"),
            to: String::from("other.txt"),
            id: file(3),
        }),
        Ok(Vec::new()),
        "a place not kept moves nowhere"
    );
}

#[test]
fn an_edit_answers_only_the_keys_it_changed() {
    let mut arrangement = holding("a.txt", placed(1, 1, 1));
    assert_eq!(
        arrangement.make(&put("a.txt", placed(1, 1, 1))),
        Ok(Vec::new()),
        "the same place again changes nothing"
    );
    let key = PlaceKey::of("a.txt");
    assert_eq!(
        arrangement.make(&ArrangementEdit::Forget(vec![key, PlaceKey::of("b.txt")])),
        Ok(vec![key.spelled()])
    );
    assert_eq!(arrangement.places().count(), 0);
}

#[test]
fn a_store_answer_is_taken_and_one_it_cannot_read_is_refused() {
    let mut arrangement = Arrangement::default();
    let key = PlaceKey::of("a.txt").spelled();
    let value = holding("a.txt", placed(3, 4, 5)).kept(&key).expect("kept");
    assert!(arrangement.take(&key, Some(&value)));
    assert_eq!(arrangement.place_of("a.txt"), Some(placed(3, 4, 5)));
    assert!(
        !arrangement.take(&key, Some("zz")),
        "a value it cannot read"
    );
    assert!(arrangement.take(&key, Some(FORGOTTEN)), "a forgotten place");
    assert_eq!(arrangement.place_of("a.txt"), None);
    assert!(
        arrangement.take("library.os.tairix.calc", Some("favourite=1")),
        "another record's key changes nothing"
    );
    assert_eq!(arrangement.places().count(), 0);
}

#[test]
fn an_unreadable_store_says_so_and_yields_no_record_to_store_over() {
    let mut host = service();
    host.refusal().set(Some(Errno::DeviceOffline));
    let loaded = load_arrangement(&mut host);
    assert_eq!(loaded.arrangement, None);
    assert!(loaded.warnings[0].contains("DeviceOffline"));
}

/// What was shown when a session put an icon somewhere is what the next one
/// reads back.
#[test]
fn a_place_put_survives_into_the_next_session() {
    let mut host = service();
    let mut memory = ArrangementMemory::new();
    assert!(memory.next_load(0));
    memory.loaded(0, load_arrangement(&mut host).arrangement);
    memory.apply(put("a.txt", placed(10, 20, 1))).expect("room");
    let ask = memory.next_store().expect("owed");
    let result = store_personal(&mut host, &ask.changes);
    assert!(memory.stored(ask.number, result).is_empty());
    let next = load_arrangement(&mut host).arrangement.expect("read");
    assert_eq!(next, *memory.live());
}

/// A place machine policy would show through stays forgotten once the user
/// gives it up.
#[test]
fn a_place_forgotten_over_a_policy_stays_forgotten() {
    let key = PlaceKey::of("a.txt").spelled();
    let value = holding("a.txt", placed(1, 2, 3)).kept(&key).expect("kept");
    let mut host = service().with_policy(&alloc::format!("{key} = {value}\n"));
    let mut memory = ArrangementMemory::new();
    assert!(memory.next_load(0));
    memory.loaded(0, load_arrangement(&mut host).arrangement);
    assert!(
        memory.live().place_of("a.txt").is_some(),
        "the policy's place shows"
    );
    memory
        .apply(ArrangementEdit::Set(vec![(String::from("a.txt"), None)]))
        .expect("room");
    let ask = memory.next_store().expect("owed");
    let result = store_personal(&mut host, &ask.changes);
    assert!(memory.stored(ask.number, result).is_empty());
    let next = load_arrangement(&mut host).arrangement.expect("read");
    assert_eq!(
        next.place_of("a.txt"),
        None,
        "what was given up stays given up"
    );
}

#[test]
fn nothing_is_stored_over_a_record_not_read() {
    let mut memory = ArrangementMemory::new();
    memory.apply(put("a.txt", placed(1, 1, 1))).expect("room");
    assert!(memory.live().place_of("a.txt").is_some(), "shown at once");
    assert_eq!(
        memory.next_store(),
        None,
        "never stored over a record not read"
    );
}

/// A place keeps the edge it is measured from and its turn among the others
/// through the store, and one naming an edge there is none of is left out.
#[test]
fn a_place_keeps_its_edge_and_turn_and_an_unknown_edge_is_left_out() {
    let key = PlaceKey::of("a.txt").spelled();
    let kept = Placed {
        id: file(1),
        at: Place {
            across: 5,
            down: 6,
            from: IconFlow::Trailing,
        },
        order: 9,
    };
    let value = holding("a.txt", kept).kept(&key).expect("kept");
    let (read, refused) = Arrangement::read([(key.as_str(), value.as_str())].into_iter());
    assert!(refused.is_empty());
    assert_eq!(read.place_of("a.txt"), Some(kept));
    // The edge is the ninth byte: the seventeenth and eighteenth digits.
    let mut unknown = value;
    unknown.replace_range(16..18, "02");
    let (read, refused) = Arrangement::read([(key.as_str(), unknown.as_str())].into_iter());
    assert_eq!(read.places().count(), 0);
    assert_eq!(refused, [key.as_str()]);
}
