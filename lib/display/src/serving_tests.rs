//! The serve loop's turn: each wake acted on, and a display whose calls can
//! take its event sources' wakes serviced after every call that drove it.

use alloc::collections::VecDeque;

use tairix_log::{Event, EventId, Sink};

use super::*;
use crate::events::{FIRST_PRESENT, LATCH_OVERDUE, LATCH_UNDELIVERED};
use crate::serving::{Rendezvous, Serving, Woke};

/// The kernel's side of the loop, scripted: the requests waiting and the
/// lease announced, and what was answered, announced and recorded.
#[derive(Default)]
struct MockRendezvous {
    requests: VecDeque<(u64, Vec<u8>)>,
    lease: Option<DisplayLease>,
    replies: Vec<u64>,
    followed: usize,
    records: RefCell<Vec<EventId>>,
}

impl Sink for MockRendezvous {
    fn write_event(&self, event: &Event<'_>) {
        self.records.borrow_mut().push(event.id);
    }
}

impl Rendezvous for MockRendezvous {
    fn take_request(&mut self, request: &mut [u8]) -> Option<(usize, u64)> {
        let (ticket, bytes) = self.requests.pop_front()?;
        request[..bytes.len()].copy_from_slice(&bytes);
        Some((bytes.len(), ticket))
    }

    fn reply(&mut self, ticket: u64, _reply: &[u8]) {
        self.replies.push(ticket);
    }

    fn lease(&mut self) -> Option<DisplayLease> {
        self.lease
    }

    fn follow(&mut self, _output: Option<OutputMode>) {
        self.followed += 1;
    }
}

/// A serve loop over what a rig set up.
struct Looped {
    serving: Serving<StepClock, MockRendezvous>,
    display: MockDisplay,
    seat: MockSeat,
    delivery: MockDelivery,
}

impl Looped {
    fn over(rig: Rig, rendezvous: MockRendezvous) -> Self {
        let Rig {
            server,
            display,
            seat,
            delivery,
            ..
        } = rig;
        Self {
            serving: Serving::new(server, rendezvous),
            display,
            seat,
            delivery,
        }
    }

    fn on(&mut self, woke: Woke) {
        self.serving
            .on(&mut self.display, &mut self.seat, &mut self.delivery, woke);
    }

    fn rendezvous(&self) -> &MockRendezvous {
        self.serving.rendezvous()
    }
}

fn lease_ended() -> MockRendezvous {
    MockRendezvous {
        lease: Some(DisplayLease::ended(1, ReleaseSurface::Text)),
        ..MockRendezvous::default()
    }
}

fn presenting(buffer: BufferId) -> MockRendezvous {
    let present = DisplayRequest::Present {
        seat_id: SEAT,
        buffer,
        damage: DamageList::new(&[full()]).expect("one rect"),
    };
    MockRendezvous {
        requests: [(TICKET, present.to_le_bytes().to_vec())].into(),
        ..MockRendezvous::default()
    }
}

/// A rig whose display queues presents, one queued past its deadline.
fn overdue(takes_wakes: bool) -> Rig {
    let mut rig = Rig::new();
    rig.display.takes_wakes = takes_wakes;
    rig.display.queues = true;
    let frames = rig.frames();
    assert!(rig.present(frames[0], &[full()]).is_ok());
    rig.clock.advance(PRESENT_LATCH_DEADLINE_NS);
    rig
}

#[test]
fn a_lease_s_release_is_followed_by_a_service_where_the_display_s_calls_take_its_wakes() {
    let mut rig = Rig::new();
    rig.display.takes_wakes = true;
    let mut looped = Looped::over(rig, lease_ended());
    looped.on(Woke::Lease);
    assert_eq!(looped.display.powered, [DisplayPower::On]);
    assert_eq!(
        looped.display.serviced,
        [(0, 1, 0)],
        "once, after the release lit the display"
    );
    assert_eq!(looped.rendezvous().followed, 1);
}

#[test]
fn an_expired_present_is_followed_by_a_service_where_the_display_s_calls_take_its_wakes() {
    let mut looped = Looped::over(overdue(true), MockRendezvous::default());
    looped.on(Woke::Due);
    assert_eq!(
        looped.display.serviced,
        [(1, 1, 1)],
        "after it was abandoned"
    );
    assert_eq!(*looped.rendezvous().records.borrow(), [LATCH_OVERDUE]);
}

#[test]
fn a_request_is_answered_then_followed_by_a_service_where_the_display_s_calls_take_its_wakes() {
    let mut rig = Rig::new();
    rig.display.takes_wakes = true;
    let frames = rig.frames();
    let mut looped = Looped::over(rig, presenting(frames[0]));
    looped.on(Woke::Request);
    assert_eq!(looped.rendezvous().replies, [TICKET]);
    assert_eq!(
        looped.display.serviced,
        [(1, 1, 0)],
        "after it was lit and shown"
    );
    assert_eq!(*looped.rendezvous().records.borrow(), [FIRST_PRESENT]);
}

#[test]
fn a_display_whose_calls_take_no_wakes_is_serviced_on_its_sources_wakes_alone() {
    let mut looped = Looped::over(overdue(false), lease_ended());
    looped.on(Woke::Due);
    looped.on(Woke::Lease);
    assert_eq!(looped.display.abandoned, 1);
    assert!(looped.display.serviced.is_empty());
    looped.on(Woke::Events);
    assert_eq!(looped.display.serviced.len(), 1);
    assert_eq!(looped.rendezvous().followed, 1);
}

#[test]
fn a_wake_that_drove_nothing_owes_no_service() {
    let mut rig = Rig::new();
    rig.display.takes_wakes = true;
    let mut looped = Looped::over(rig, MockRendezvous::default());
    // No latch owed, no lease that reads, and a request its poster withdrew.
    looped.on(Woke::Room);
    looped.on(Woke::Lease);
    looped.on(Woke::Request);
    assert!(looped.display.serviced.is_empty());
    assert!(looped.rendezvous().replies.is_empty());
    looped.on(Woke::Events);
    assert_eq!(looped.display.serviced.len(), 1, "a source's wake, once");
}

#[test]
fn latches_refused_for_good_are_recorded_once_their_count_doubles() {
    let mut rig = Rig::new();
    rig.display.queues = true;
    let frames = rig.frames();
    rig.delivery.refuse_post = Some(Errno::NoSpace);
    assert!(rig.present(frames[0], &[full()]).is_ok());
    assert_eq!(rig.latch(), Latch::Undelivered(Errno::NoSpace));
    let mut looped = Looped::over(rig, MockRendezvous::default());
    looped.serving.report_undelivered();
    looped.serving.report_undelivered();
    assert_eq!(*looped.rendezvous().records.borrow(), [LATCH_UNDELIVERED]);
}
