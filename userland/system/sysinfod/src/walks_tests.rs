extern crate std;

use super::*;
use alloc::vec;

fn caller(n: u32) -> ProcId {
    let mut id = [0; tairix_abi::PROC_ID_LEN];
    id[..4].copy_from_slice(&n.to_le_bytes());
    ProcId::from_raw(id)
}

fn key(n: u32, walk: u32) -> WalkKey {
    WalkKey {
        caller: caller(n),
        walk,
        query: SysinfoQueryId::GLOBAL_PROCESS_LIST,
    }
}

fn held(walks: &mut Walks, key: WalkKey) -> Option<std::vec::Vec<u8>> {
    match walks.list(key)? {
        Walked::Held(list) => Some(list.to_vec()),
        Walked::Unheld => None,
    }
}

/// A walk holds its list until it ends; another caller's walk of the same id
/// is another walk, as is the same id over another list.
#[test]
fn a_walk_holds_its_list_until_it_ends() {
    let mut walks = Walks::new(1 << 30);
    walks.begin(key(7, 1), Some(vec![1, 2, 3]));
    walks.begin(key(8, 1), Some(vec![9]));
    let mounts = WalkKey {
        query: SysinfoQueryId::MOUNT_LIST,
        ..key(7, 1)
    };
    assert!(walks.list(mounts).is_none());
    assert_eq!(held(&mut walks, key(7, 1)), Some(vec![1, 2, 3]));
    assert_eq!(held(&mut walks, key(8, 1)), Some(vec![9]));
    walks.end(key(7, 1));
    assert!(walks.list(key(7, 1)).is_none());
    walks.begin(key(8, 1), Some(vec![4, 5]));
    assert_eq!(held(&mut walks, key(8, 1)), Some(vec![4, 5]), "begun again");
    assert_eq!(walks.charged, WALK_CHARGE + 2);
    walks.end(key(8, 1));
    assert!(walks.callers.is_empty(), "a caller with no walk is let go");
    assert_eq!(walks.charged, 0);
}

/// A caller beginning more walks than its share lets go of its own least
/// recently used, never another caller's.
#[test]
fn a_caller_past_its_share_lets_go_of_its_own_oldest() {
    let mut walks = Walks::new(1 << 30);
    walks.begin(key(2, 1), Some(vec![0]));
    let share = u32::try_from(WALKS_PER_CALLER).unwrap();
    for walk in 1..=share {
        walks.begin(key(1, walk), Some(vec![0]));
    }
    let _ = walks.list(key(1, 1));
    walks.begin(key(1, share + 1), Some(vec![0]));
    assert!(walks.list(key(1, 2)).is_none(), "its least recently used");
    assert!(walks.list(key(1, 1)).is_some());
    assert!(walks.list(key(1, share + 1)).is_some());
    assert!(walks.list(key(2, 1)).is_some(), "another's kept");
}

/// What the walks hold is bounded: the least recently active caller's walk
/// is let go for room, and a list larger than the whole budget is read
/// afresh instead.
#[test]
fn what_the_walks_hold_is_bounded() {
    let mut walks = Walks::new(0);
    let half = MIN_HELD_BYTES / 2 - WALK_CHARGE;
    walks.begin(key(1, 1), Some(vec![0; half]));
    walks.begin(key(2, 1), Some(vec![0; half]));
    let _ = walks.list(key(1, 1));
    walks.begin(key(3, 1), Some(vec![0; half]));
    assert!(walks.list(key(2, 1)).is_none(), "least recently active");
    assert!(walks.list(key(1, 1)).is_some());
    assert!(walks.list(key(3, 1)).is_some());
    assert!(walks.charged <= MIN_HELD_BYTES);
    walks.begin(key(4, 1), Some(vec![0; MIN_HELD_BYTES + 1]));
    assert!(matches!(walks.list(key(4, 1)), Some(Walked::Unheld)));
    walks.begin(key(5, 1), None);
    assert!(matches!(walks.list(key(5, 1)), Some(Walked::Unheld)));
}

/// Walks holding nothing are charged too, so any number of callers beginning
/// them holds no more than the budget, the latest kept.
#[test]
fn a_flood_of_empty_walks_is_bounded() {
    let mut walks = Walks::new(0);
    let most = MIN_HELD_BYTES / WALK_CHARGE;
    let callers = u32::try_from(most * 3).unwrap();
    for n in 0..callers {
        walks.begin(key(n, 1), Some(vec![]));
    }
    assert!(walks.charged <= MIN_HELD_BYTES);
    assert_eq!(walks.callers.len(), most);
    assert!(walks.list(key(0, 1)).is_none(), "the earliest let go");
    assert!(walks.list(key(callers - 1, 1)).is_some(), "the latest kept");
}

/// A caller that exits never ends its walks, so they go when it is found gone,
/// their charge with them, and a live caller's stay.
#[test]
fn an_exited_callers_walks_are_let_go_of_and_a_live_callers_kept() {
    let mut walks = Walks::new(1 << 30);
    let (gone, staying) = (key(1, 1), key(2, 1));
    walks.begin(gone, Some(vec![1; 64]));
    walks.begin(staying, Some(vec![2; 64]));
    let both = walks.charged;
    walks.retain_live(caller(9), || {
        Some(LiveInstances::new(vec![caller(9), caller(2)]))
    });
    assert!(walks.list(gone).is_none(), "the exited caller's walk");
    assert_eq!(held(&mut walks, staying), Some(vec![2; 64]));
    assert!(walks.charged < both, "its charge released");
    walks.end(staying);
    assert_eq!(walks.charged, 0);
}

/// The asking caller is alive, so a census is taken only when another caller
/// holds a walk, and one that fails lets go of nothing.
#[test]
fn a_census_is_taken_only_when_another_caller_holds_a_walk() {
    let mut walks = Walks::new(1 << 30);
    let own = key(2, 1);
    walks.begin(own, Some(vec![2; 64]));
    walks.retain_live(caller(2), || panic!("nothing another caller holds"));
    walks.retain_live(caller(3), || None);
    assert_eq!(held(&mut walks, own), Some(vec![2; 64]));
}
