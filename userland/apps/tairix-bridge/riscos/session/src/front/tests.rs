//! The front end's model: an honest session's events believed whole, every
//! event a worker could not have sent refused with nothing changed, the
//! relays parked on and waited for in place told apart, launches and quits
//! kept within the worker's bound, the room a launch's events need held as it
//! is asked, and the contexts running reported by their lines.

use alloc::vec::Vec;

use tairix_fuzzseed::meter::refusing_after;
use tairix_riscos_kernel::relay::Side;

use super::*;
use crate::protocol::{Ending, Error, WAITING_MOST};
use crate::ring::RING_BYTES;

const SCREEN: Screen = Screen {
    width: 640,
    height: 480,
    x_eig: 1,
    y_eig: 1,
    border: 0,
    blanked: false,
};

/// A model with `lines` launched, each made the context its order gives,
/// from 1, and each told its screen.
fn launched(lines: &[&[u8]]) -> Front {
    let mut front = Front::new().expect("room");
    for (made, line) in (1..).zip(lines) {
        let serial = front.launch(line).expect("carried");
        let events = [
            Event::Launched {
                serial,
                context: made,
            },
            Event::Screen {
                context: made,
                screen: SCREEN,
            },
        ];
        for event in events {
            assert_eq!(front.believe(&event), Ok(None));
        }
    }
    front
}

#[test]
fn an_honest_session_is_believed_whole() {
    let mut front = launched(&[b"BASIC"]);
    assert!(front.input(1) && front.input(1));
    assert_eq!(
        front.believe(&Event::Credit {
            context: 1,
            applied: 2
        }),
        Ok(None)
    );
    let damage = Event::Damage {
        context: 1,
        first: 0,
        lines: 10,
        offset: 0,
    };
    let placed = front
        .believe(&damage)
        .expect("believed")
        .expect("in the ring");
    assert_eq!(placed.bytes, 10 * 640 * 4);
    assert!(front.release(placed.released()));
    let relay = Event::Relay {
        serial: 7,
        context: 1,
        parked: true,
        side: Side::Files,
        request: b"read",
    };
    assert_eq!(front.believe(&relay), Ok(None));
    assert!(front.answer(7));
    assert!(!front.answer(7), "answered once");
    let ended = Event::Ended {
        context: 1,
        ending: Ending::Exit { code: 0 },
    };
    assert_eq!(front.believe(&ended), Ok(None));
    assert_eq!(front.running().count(), 0);
}

#[test]
fn the_contexts_running_are_reported_by_their_lines() {
    let mut front = launched(&[b"BASIC", b"Run HostFS::Disc.$.prog"]);
    let ended = Event::Ended {
        context: 1,
        ending: Ending::Quit,
    };
    assert_eq!(front.believe(&ended), Ok(None));
    let running: Vec<_> = front.running().collect();
    assert_eq!(running, [(2, &b"Run HostFS::Disc.$.prog"[..])]);
    let why = Why::Unbelievable(Unbelievable("a context parked on a second relay"));
    assert_eq!(why.reason(), "a context parked on a second relay");
    assert_eq!(
        Why::Worker(WorkerEnd::OutOfMemory).reason(),
        "out of memory"
    );
}

#[test]
fn a_launch_carries_only_a_line_a_launch_carries() {
    let mut front = Front::new().expect("room");
    assert_eq!(front.launch(b"Echo\rHello"), None);
    let serial = front.launch(b"Echo Hello").expect("carried");
    let refused = Event::Refused {
        serial,
        error: Some(Error {
            number: 0x1E6,
            text: b"",
        }),
    };
    assert_eq!(front.believe(&refused), Ok(None));
    assert_eq!(front.running().count(), 0);
}

/// A relay context 1 is parked on, unanswered.
const UNANSWERED: Event<'static> = Event::Relay {
    serial: 3,
    context: 1,
    parked: true,
    side: Side::Files,
    request: b"held",
};

/// One the worker waits for in place, unanswered.
const IN_PLACE: Event<'static> = Event::Relay {
    serial: 6,
    context: BOOT,
    parked: false,
    side: Side::Files,
    request: b"roots",
};

/// Events a worker could not have sent after [`launched`], [`UNANSWERED`]
/// and [`IN_PLACE`], with why each is refused.
const UNBELIEVABLE: [(Event<'static>, &str); 15] = [
    (
        Event::Credit {
            context: 9,
            applied: 0,
        },
        "a credit for a context not running",
    ),
    (
        Event::Credit {
            context: 1,
            applied: 1,
        },
        "a credit for input never sent",
    ),
    (
        Event::Launched {
            serial: 5,
            context: 3,
        },
        "a launch made that was never asked",
    ),
    (
        Event::Launched {
            serial: 0,
            context: 1,
        },
        "a context numbered as one made before",
    ),
    (
        Event::Refused {
            serial: 5,
            error: None,
        },
        "a launch refused that was never asked",
    ),
    (
        Event::Screen {
            context: 9,
            screen: SCREEN,
        },
        "a screen for a context not running",
    ),
    (
        Event::Damage {
            context: 9,
            first: 0,
            lines: 1,
            offset: 0,
        },
        "damage to a context not running",
    ),
    (
        Event::Damage {
            context: 1,
            first: 470,
            lines: 20,
            offset: 0,
        },
        "damage below its screen",
    ),
    (
        Event::Damage {
            context: 1,
            first: 0,
            lines: 1,
            offset: 4,
        },
        "damage where the worker could not have put it",
    ),
    (
        Event::Relay {
            serial: 4,
            context: 9,
            parked: true,
            side: Side::Files,
            request: b"",
        },
        "a relay for a context not running",
    ),
    (
        Event::Relay {
            serial: 4,
            context: BOOT,
            parked: true,
            side: Side::Files,
            request: b"",
        },
        "the session's own relay parked",
    ),
    (
        Event::Relay {
            serial: 3,
            context: 1,
            parked: false,
            side: Side::Fonts,
            request: b"",
        },
        "a relay as one unanswered",
    ),
    (
        Event::Relay {
            serial: 4,
            context: 1,
            parked: true,
            side: Side::Fonts,
            request: b"",
        },
        "a context parked on a second relay",
    ),
    (
        Event::Relay {
            serial: 4,
            context: 1,
            parked: false,
            side: Side::Fonts,
            request: b"",
        },
        "a second relay waited for in place",
    ),
    (
        Event::Ended {
            context: 9,
            ending: Ending::Quit,
        },
        "an end of a context not running",
    ),
];

#[test]
fn what_a_worker_could_not_have_sent_is_refused_and_changes_nothing() {
    for (event, why) in UNBELIEVABLE {
        let made = || {
            let mut front = launched(&[b"BASIC"]);
            for relay in [UNANSWERED, IN_PLACE] {
                assert_eq!(front.believe(&relay), Ok(None));
            }
            front
        };
        let (mut front, twin) = (made(), made());
        assert_eq!(front.believe(&event), Err(Unbelievable(why)), "{event:?}");
        assert_eq!(front, twin, "{event:?} changed nothing");
    }
}

#[test]
fn a_context_waits_for_one_relay_in_place_while_it_is_parked_on_another() {
    let mut front = launched(&[b"BASIC"]);
    assert_eq!(front.believe(&UNANSWERED), Ok(None));
    let in_place = Event::Relay {
        serial: 4,
        context: 1,
        parked: false,
        side: Side::Files,
        request: b"close",
    };
    assert_eq!(front.believe(&in_place), Ok(None), "its end closing a file");
    let ended = Event::Ended {
        context: 1,
        ending: Ending::Quit,
    };
    assert_eq!(front.believe(&ended), Ok(None));
    assert!(
        front.answer(4) && front.answer(3),
        "both still the front end's to answer"
    );
}

#[test]
fn launches_and_quits_waiting_are_kept_within_what_the_worker_keeps() {
    let mut front = launched(&[b"BASIC"]);
    assert!(front.quit(1));
    assert!(!front.quit(1), "asked once");
    assert!(!front.quit(9), "not running");
    let asked: Vec<_> = (0..WAITING_MOST)
        .map_while(|_| front.launch(b"BASIC"))
        .collect();
    assert_eq!(asked.len(), WAITING_MOST - 1, "the quit waiting among them");
    assert_eq!(front.launch(b"BASIC"), None);
    let ended = Event::Ended {
        context: 1,
        ending: Ending::Quit,
    };
    assert_eq!(front.believe(&ended), Ok(None));
    assert!(
        front.launch(b"BASIC").is_some(),
        "the quit answered by the end"
    );
}

#[test]
fn damage_to_a_screen_never_described_is_refused() {
    let mut front = Front::new().expect("room");
    let serial = front.launch(b"BASIC").expect("carried");
    let launched = Event::Launched { serial, context: 1 };
    assert_eq!(front.believe(&launched), Ok(None));
    let damage = Event::Damage {
        context: 1,
        first: 0,
        lines: 1,
        offset: 0,
    };
    assert_eq!(
        front.believe(&damage),
        Err(Unbelievable("damage to a screen never described"))
    );
}

#[test]
fn the_ring_is_believed_only_while_its_records_are_given_back() {
    let mut front = launched(&[b"BASIC"]);
    let full = 480 * 640 * 4;
    let mut offset = 0;
    while offset + full <= RING_BYTES {
        let damage = Event::Damage {
            context: 1,
            first: 0,
            lines: 480,
            offset,
        };
        assert!(front.believe(&damage).expect("room").is_some());
        offset += full;
    }
    let beyond = Event::Damage {
        context: 1,
        first: 0,
        lines: 480,
        offset: 0,
    };
    assert!(front.believe(&beyond).is_err(), "none given back yet");
    assert!(front.release(full));
    let wrapped = front
        .believe(&beyond)
        .expect("room again")
        .expect("in the ring");
    assert!(wrapped.skipped > 0, "it starts again at the start");
    assert!(!front.release(RING_BYTES + 1), "more than it holds");
    assert!(front.release(RING_BYTES), "the whole ring, as it holds");
}

#[test]
fn the_session_s_own_relay_is_believed_before_any_launch() {
    let mut front = Front::new().expect("room");
    assert_eq!(front.believe(&IN_PLACE), Ok(None));
    let again = Event::Relay {
        serial: 7,
        context: BOOT,
        parked: false,
        side: Side::Files,
        request: b"roots",
    };
    assert_eq!(
        front.believe(&again),
        Err(Unbelievable("a second relay waited for in place"))
    );
    assert!(front.answer(6));
    assert_eq!(front.believe(&again), Ok(None));
}

#[test]
fn the_room_a_launch_s_events_need_is_held_as_it_is_asked() {
    assert!(
        refusing_after(0, Front::new).is_none(),
        "no room for the session's own relay"
    );
    let mut front = Front::new().expect("room");
    assert_eq!(
        refusing_after(0, || front.launch(b"BASIC")),
        None,
        "no room to keep it"
    );
    // More than one node of a map holds, so room held short of their need shows.
    let contexts = 1..=20;
    let mut events: Vec<_> = contexts
        .clone()
        .map(|context| Event::Launched {
            serial: front.launch(b"BASIC").expect("carried"),
            context,
        })
        .collect();
    let relay = |serial, context, parked| Event::Relay {
        serial,
        context,
        parked,
        side: Side::Files,
        request: b"read",
    };
    events.extend(contexts.map(|context| relay(context, context, true)));
    events.push(relay(0, 1, false));
    let believed = refusing_after(0, || {
        events
            .iter()
            .take_while(|event| matches!(front.believe(event), Ok(None)))
            .count()
    });
    assert_eq!(believed, events.len(), "{:?}", events.get(believed));
}
