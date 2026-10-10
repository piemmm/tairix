//! Unit tests for the routing policy.
//!
//! The policy is a pure function of state, so it is checked over the whole
//! cross-product of (role × whose room the sink is × which sink was asked
//! for) rather than over the handful of cases somebody thought of.

use alloc::vec;
use alloc::vec::Vec;

use super::{
    admit, duck_millibel, route, Admission, Roles, Room, Routing, SinkState, StreamRequest,
};
use crate::volume::DUCK_MILLIBEL;
use tairix_abi::audio::StreamRole;
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::seat::{DisplayLease, ReleaseSurface};
use tairix_abi::{Errno, ProcId};

const EVERY_ROLE: &[StreamRole] = &[
    StreamRole::Media,
    StreamRole::Communication,
    StreamRole::Notification,
    StreamRole::Accessibility,
];

const ALICE: ProcId = ProcId::from_raw([0xA1; 16]);
const BOB: ProcId = ProcId::from_raw([0xB0; 16]);

const EVERY_ROOM: [Room; 4] = [
    Room::Unclaimed,
    Room::Session(ALICE),
    Room::Session(BOB),
    Room::Withheld,
];

/// Two sinks, the first the machine default, serving the rooms asked for.
fn sinks(default_room: Room, other_room: Room) -> Vec<SinkState> {
    vec![
        SinkState {
            device_id: 10,
            is_default: true,
            room: default_room,
        },
        SinkState {
            device_id: 20,
            is_default: false,
            room: other_room,
        },
    ]
}

fn request(role: StreamRole, session: Option<ProcId>) -> StreamRequest {
    StreamRequest {
        role,
        requested_device: None,
        session,
    }
}

#[test]
fn a_seat_with_no_presenter_leaves_its_room_unclaimed() {
    assert_eq!(Room::from(DisplayLease::UNHELD), Room::Unclaimed);
    assert_eq!(
        Room::from(DisplayLease::ended(3, ReleaseSurface::Text)),
        Room::Unclaimed
    );
}

#[test]
fn a_held_seat_is_its_holders_login_or_nobodys() {
    assert_eq!(
        Room::from(DisplayLease::held(3, ALICE)),
        Room::Session(ALICE)
    );
    assert_eq!(
        Room::from(DisplayLease::held(3, ProcId::KERNEL)),
        Room::Withheld,
        "a presenter no login encloses, such as the login screen"
    );
}

/// Between one presenter and the next, the departing session no longer has
/// the room and the arriving one does not have it yet.
#[test]
fn a_handover_withholds_the_room_from_everybody() {
    assert_eq!(
        Room::from(DisplayLease::ended(3, ReleaseSurface::Handover)),
        Room::Withheld
    );
}

/// A source follows the room as a sink does — a departing session's recorder
/// does not hear the arriving user — but what it captured is never discarded
/// for being late.
#[test]
fn a_source_is_held_outside_the_room_and_never_dropped() {
    for role in EVERY_ROLE {
        assert_eq!(
            admit(
                StreamDirection::Capture,
                *role,
                Some(ALICE),
                Room::Session(ALICE)
            ),
            Admission::Mix
        );
        for room in [Room::Session(BOB), Room::Withheld] {
            assert_eq!(
                admit(StreamDirection::Capture, *role, Some(ALICE), room),
                Admission::Hold,
                "{role:?} into {room:?}"
            );
        }
        assert_eq!(
            admit(StreamDirection::Capture, *role, None, Room::Unclaimed),
            Admission::Mix
        );
    }
}

#[test]
fn an_unclaimed_room_plays_for_anybody_which_is_the_headless_case() {
    for role in EVERY_ROLE {
        for session in [None, Some(ALICE), Some(BOB)] {
            assert_eq!(
                route(
                    &request(*role, session),
                    &sinks(Room::Unclaimed, Room::Unclaimed)
                ),
                Routing::Play { device_id: 10 },
                "{role:?} from {session:?}"
            );
        }
    }
}

#[test]
fn the_session_holding_the_seat_is_mixed() {
    for role in EVERY_ROLE {
        assert_eq!(
            route(
                &request(*role, Some(ALICE)),
                &sinks(Room::Session(ALICE), Room::Unclaimed)
            ),
            Routing::Play { device_id: 10 },
            "{role:?}"
        );
    }
}

/// A departing user's music does not play into the arriving user's room, and
/// it does not silently vanish either: it holds its position and is told.
#[test]
fn a_session_without_the_room_is_paused_unless_it_is_a_notification() {
    for room in [Room::Session(ALICE), Room::Withheld] {
        for role in EVERY_ROLE {
            for session in [None, Some(BOB)] {
                let expected = if *role == StreamRole::Notification {
                    Routing::Drop
                } else {
                    Routing::Pause { device_id: 10 }
                };
                assert_eq!(
                    route(&request(*role, session), &sinks(room, Room::Unclaimed)),
                    expected,
                    "{role:?} from {session:?} into {room:?}"
                );
            }
        }
    }
}

/// Nobody plays into a withheld room — not even the session that just left
/// it, nor the one about to arrive.
#[test]
fn a_withheld_room_mixes_no_session() {
    for session in [Some(ALICE), Some(BOB)] {
        assert_eq!(
            route(
                &request(StreamRole::Media, session),
                &sinks(Room::Withheld, Room::Unclaimed)
            ),
            Routing::Pause { device_id: 10 }
        );
    }
}

#[test]
fn a_named_sink_is_used_and_arbitrated_on_its_own_room() {
    let request = StreamRequest {
        role: StreamRole::Media,
        requested_device: Some(20),
        session: Some(BOB),
    };
    // The default is Alice's, but the named sink is free.
    assert_eq!(
        route(&request, &sinks(Room::Session(ALICE), Room::Unclaimed)),
        Routing::Play { device_id: 20 }
    );
    // And when the named sink is Alice's too, Bob waits on it rather than
    // falling back to one they may use.
    assert_eq!(
        route(&request, &sinks(Room::Unclaimed, Room::Session(ALICE))),
        Routing::Pause { device_id: 20 }
    );
}

#[test]
fn a_named_sink_that_does_not_exist_is_refused_rather_than_substituted() {
    let request = StreamRequest {
        role: StreamRole::Media,
        requested_device: Some(99),
        session: Some(ALICE),
    };
    assert_eq!(
        route(&request, &sinks(Room::Unclaimed, Room::Unclaimed)),
        Routing::Refuse(Errno::NotFound)
    );
}

/// A machine that has not been told which sink to use is a different answer
/// from one that has no sinks, and neither is a sink picked arbitrarily.
#[test]
fn a_machine_with_no_default_refuses_rather_than_guessing() {
    let request = request(StreamRole::Media, Some(ALICE));
    let undecided = vec![SinkState {
        device_id: 10,
        is_default: false,
        room: Room::Unclaimed,
    }];
    assert_eq!(
        route(&request, &undecided),
        Routing::Refuse(Errno::DeviceOffline)
    );
    assert_eq!(route(&request, &[]), Routing::Refuse(Errno::DeviceOffline));
}

/// The whole cross-product, so no combination is decided by accident.
#[test]
fn the_policy_is_total_over_every_role_room_and_request() {
    for role in EVERY_ROLE {
        for room in EVERY_ROOM {
            for session in [None, Some(ALICE), Some(BOB)] {
                for requested in [None, Some(10), Some(20), Some(99)] {
                    let request = StreamRequest {
                        role: *role,
                        requested_device: requested,
                        session,
                    };
                    let decided = route(&request, &sinks(room, room));
                    let device_id = requested.unwrap_or(10);
                    let expected = match (requested, room, session) {
                        (Some(99), _, _) => Routing::Refuse(Errno::NotFound),
                        (_, Room::Unclaimed, _) => Routing::Play { device_id },
                        (_, Room::Session(held), Some(mine)) if held == mine => {
                            Routing::Play { device_id }
                        }
                        _ if *role == StreamRole::Notification => Routing::Drop,
                        _ => Routing::Pause { device_id },
                    };
                    assert_eq!(
                        decided, expected,
                        "{role:?} from {session:?}, sink {room:?}, asking for {requested:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn media_steps_aside_for_speech_and_nothing_else_ducks() {
    let live = |roles: &[StreamRole]| roles.iter().copied().collect::<Roles>();
    assert_eq!(
        duck_millibel(StreamRole::Media, live(&[StreamRole::Communication])),
        DUCK_MILLIBEL
    );
    assert_eq!(
        duck_millibel(StreamRole::Media, live(&[StreamRole::Accessibility])),
        DUCK_MILLIBEL
    );
    assert_eq!(
        duck_millibel(StreamRole::Media, live(&[StreamRole::Media])),
        0
    );
    assert_eq!(
        duck_millibel(StreamRole::Media, live(&[StreamRole::Notification])),
        0
    );
    assert_eq!(duck_millibel(StreamRole::Media, Roles::default()), 0);
    for role in EVERY_ROLE {
        if *role == StreamRole::Media {
            continue;
        }
        assert_eq!(
            duck_millibel(*role, live(&[StreamRole::Communication])),
            0,
            "{role:?} must not duck under speech"
        );
    }
}

#[test]
fn the_role_set_holds_each_role_once_and_only_those_inserted() {
    let mut set = Roles::default();
    for role in EVERY_ROLE {
        assert!(!set.contains(*role));
    }
    set.insert(StreamRole::Notification);
    set.insert(StreamRole::Notification);
    assert!(set.contains(StreamRole::Notification));
    assert!(EVERY_ROLE
        .iter()
        .filter(|role| **role != StreamRole::Notification)
        .all(|role| !set.contains(*role)));
    assert_eq!(EVERY_ROLE.iter().copied().collect::<Roles>(), {
        let mut all = Roles::default();
        for role in EVERY_ROLE {
            all.insert(*role);
        }
        all
    });
}
