use tairix_abi::{Errno, WaitSourceKind};

use super::{MemberSet, Members};
use crate::loopback::MemberLedger;
use crate::session::SessionDescriptors;

const READ: u64 = 3;
const WRITE: u64 = 4;

const fn pipes(read_fd: u32, write_fd: u32) -> SessionDescriptors {
    SessionDescriptors { read_fd, write_fd }
}

#[test]
fn a_session_s_pipes_are_watched_only_in_the_directions_it_wants() {
    let ledger = MemberLedger::default();
    let mut members = Members::over(ledger.clone(), READ, WRITE);
    assert_eq!(members.sync(Some(pipes(7, 8)), true, false), Ok(()));
    assert_eq!(ledger.members(), [(WaitSourceKind::Stream, 7, READ)]);
    assert_eq!(members.sync(Some(pipes(7, 8)), true, true), Ok(()));
    assert_eq!(
        ledger.members(),
        [
            (WaitSourceKind::Stream, 7, READ),
            (WaitSourceKind::StreamRoom, 8, WRITE)
        ]
    );
    assert_eq!(members.sync(Some(pipes(7, 8)), true, false), Ok(()));
    assert_eq!(ledger.members(), [(WaitSourceKind::Stream, 7, READ)]);
    assert_eq!(members.sync(None, true, true), Ok(()));
    assert!(ledger.members().is_empty());
}

#[test]
fn a_replacement_worker_s_pipes_take_the_place_of_its_predecessor_s() {
    let ledger = MemberLedger::default();
    let mut members = Members::over(ledger.clone(), READ, WRITE);
    assert_eq!(members.sync(Some(pipes(7, 8)), true, true), Ok(()));
    assert_eq!(members.sync(Some(pipes(9, 10)), true, true), Ok(()));
    assert_eq!(
        ledger.members(),
        [
            (WaitSourceKind::Stream, 9, READ),
            (WaitSourceKind::StreamRoom, 10, WRITE)
        ]
    );
    members.release();
    assert!(ledger.members().is_empty());
}

/// The regression: a window's members outlived it, so the next session given
/// the same descriptor numbers had its read member refused, and with it the
/// write member that a request larger than a pipe waits on.
#[test]
fn members_dropped_leave_nothing_for_the_next_session_on_their_numbers() {
    let ledger = MemberLedger::default();
    let mut first = Members::over(ledger.clone(), READ, WRITE);
    assert_eq!(first.sync(Some(pipes(7, 8)), true, true), Ok(()));
    drop(first);
    assert!(ledger.members().is_empty());
    let mut next = Members::over(ledger.clone(), READ + 10, WRITE + 10);
    assert_eq!(next.sync(Some(pipes(7, 8)), true, true), Ok(()));
    assert_eq!(
        ledger.members(),
        [
            (WaitSourceKind::Stream, 7, READ + 10),
            (WaitSourceKind::StreamRoom, 8, WRITE + 10)
        ]
    );
}

/// The regression: a refused read member returned before the write member
/// was registered, so a request that did not fit the pipe never finished.
#[test]
fn a_refused_direction_leaves_the_other_as_wanted() {
    let ledger = MemberLedger::default();
    let mut elsewhere = ledger.clone();
    assert_eq!(elsewhere.add(WaitSourceKind::Stream, 7, 99), Ok(()));
    let mut members = Members::over(ledger.clone(), READ, WRITE);
    assert_eq!(
        members.sync(Some(pipes(7, 8)), true, true),
        Err(Errno::AlreadyExists)
    );
    assert!(ledger
        .members()
        .contains(&(WaitSourceKind::StreamRoom, 8, WRITE)));
    drop(members);
    assert_eq!(ledger.members(), [(WaitSourceKind::Stream, 7, 99)]);
}
