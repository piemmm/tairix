use alloc::boxed::Box;
use alloc::vec::Vec;

use super::*;
use crate::host::{DmaHost, MockHost, MockWait};
use crate::transport::{ChainView, Direction, MockTransport};
use crate::DmaSlab;

/// The tag a request carries: its first device-read byte.
fn tag_of(chain: &ChainView<'_>) -> u8 {
    chain.device_read.first().map_or(0, |seg| seg[0])
}

/// Write `tag` into the request's answer byte, saying it wrote one byte per
/// tag unit so each answer's length is its own.
fn answer_with(chain: &mut ChainView<'_>, tag: u8) -> u32 {
    if let Some(out) = chain.device_write.first_mut() {
        out.fill(tag);
    }
    u32::from(tag)
}

/// A device on queue 0 of `size` descriptors that holds a request whose tag
/// is odd and answers every other at once.
fn device(host: &MockHost, size: u16) -> MockTransport {
    let mut t = MockTransport::new(1, size, 0, 0);
    t.reach(host);
    t.set_synchronous_notify(true);
    t.install_holding_shim(
        0,
        Box::new(|chain: &mut ChainView<'_>| {
            let tag = tag_of(chain);
            Ok(tag.is_multiple_of(2).then(|| answer_with(chain, tag)))
        }),
    );
    t
}

fn queue(t: &mut MockTransport, host: &MockHost, size: u16) -> TicketQueue {
    TicketQueue::new(SplitQueue::new(t, host, 0, size, 1).expect("setup")).expect("bookkeeping")
}

/// Requests carried in two slabs: the tag of request `at` is byte `at` of
/// `input`, and its answer lands in byte `at` of `output`.
struct Slabs {
    input: DmaSlab,
    output: DmaSlab,
}

impl Slabs {
    fn new(host: &MockHost, tags: &[u8]) -> Self {
        let mut input = host.alloc_dma_zeroed(tags.len()).expect("dma");
        input.as_bytes_mut().copy_from_slice(tags);
        let output = host.alloc_dma_zeroed(tags.len()).expect("dma");
        Self { input, output }
    }

    fn request(&self, at: usize) -> [ChainSegment; 2] {
        [
            ChainSegment {
                device_addr: self.input.device_addr_at(at, 1).expect("inside"),
                len: 1,
                direction: Direction::DeviceRead,
            },
            ChainSegment {
                device_addr: self.output.device_addr_at(at, 1).expect("inside"),
                len: 1,
                direction: Direction::DeviceWrite,
            },
        ]
    }

    fn answered(&self, at: usize) -> u8 {
        self.output.as_bytes()[at]
    }
}

#[test]
fn answers_in_any_order_each_reach_their_own_request() {
    let host = MockHost::new();
    let mut t = device(&host, 8);
    let mut q = queue(&mut t, &host, 8);
    let slabs = Slabs::new(&host, &[1, 3, 5, 7]);
    let tickets: Vec<Ticket> = (0..4)
        .map(|at| q.publish(&slabs.request(at)).expect("published"))
        .collect();
    q.kick(&mut t);
    assert_eq!(t.held(0), 4);
    assert_eq!(q.out(), 4);
    // Answered last first, each with its own tag.
    for at in (0..4).rev() {
        t.answer_held(0, at, |chain| {
            let tag = tag_of(chain);
            answer_with(chain, tag)
        })
        .expect("held");
    }
    assert_eq!(q.drain(), Ok(4));
    assert_eq!(q.out(), 0);
    for (at, ticket) in tickets.iter().enumerate() {
        let tag = [1u8, 3, 5, 7][at];
        assert_eq!(q.answer(*ticket), Ok(Some(u32::from(tag))));
        assert_eq!(slabs.answered(at), tag);
        assert_eq!(q.retire(*ticket), Ok(u32::from(tag)));
    }
}

#[test]
fn a_wait_ends_on_its_own_answers_and_leaves_the_rest_out() {
    let host = MockHost::new();
    let mut t = device(&host, 8);
    let mut q = queue(&mut t, &host, 8);
    let slabs = Slabs::new(&host, &[1, 2, 4]);
    let held = q.publish(&slabs.request(0)).expect("published");
    let answered = q.publish(&slabs.request(1)).expect("published");
    let also = q.publish(&slabs.request(2)).expect("published");
    q.kick(&mut t);
    let acks = t.ack_interrupts;
    assert_eq!(q.wait(&mut t, &host, &[answered, also], 1_000), Ok(()));
    assert_eq!(
        t.ack_interrupts,
        acks + 1,
        "the line is cleared after the wait"
    );
    assert_eq!(q.answer(answered), Ok(Some(2)));
    assert_eq!(q.answer(also), Ok(Some(4)));
    assert_eq!(
        q.answer(held),
        Ok(None),
        "the long one is still the device's"
    );
    assert_eq!(q.retire(held), Err(DriverError::Busy));
    assert_eq!(q.out(), 1);
}

#[test]
fn a_late_answer_is_its_own_requests_and_holds_no_other_back() {
    let host = MockHost::new();
    let mut t = device(&host, 8);
    let mut q = queue(&mut t, &host, 8);
    let slabs = Slabs::new(&host, &[9, 6]);
    let late = q.publish(&slabs.request(0)).expect("published");
    q.kick(&mut t);
    host.script_waits([MockWait::Silent]);
    assert_eq!(
        q.wait(&mut t, &host, &[late], 1_000),
        Err(DriverError::DeviceOffline)
    );
    assert_eq!(q.answer(late), Ok(None), "still the device's");
    let next = q
        .publish(&slabs.request(1))
        .expect("not held back by the late one");
    q.kick(&mut t);
    assert_eq!(q.wait(&mut t, &host, &[next], 1_000), Ok(()));
    assert_eq!(q.retire(next), Ok(6));
    t.answer_held(0, 0, |chain| answer_with(chain, 9))
        .expect("held");
    assert_eq!(q.drain(), Ok(1));
    assert_eq!(q.retire(late), Ok(9));
    assert_eq!(slabs.answered(0), 9);
}

#[test]
fn a_retired_ticket_names_nothing_however_its_record_is_reused() {
    let host = MockHost::new();
    let mut t = device(&host, 4);
    let mut q = queue(&mut t, &host, 4);
    let slabs = Slabs::new(&host, &[2, 4]);
    let first = q.publish(&slabs.request(0)).expect("published");
    q.kick(&mut t);
    assert_eq!(q.drain(), Ok(1));
    assert_eq!(q.retire(first), Ok(2));
    assert_eq!(q.retire(first), Err(DriverError::NotFound));
    assert_eq!(q.answer(first), Err(DriverError::NotFound));
    let second = q.publish(&slabs.request(1)).expect("published");
    assert_ne!(second, first);
    q.kick(&mut t);
    assert_eq!(q.drain(), Ok(1));
    assert_eq!(q.answer(first), Err(DriverError::NotFound));
    assert_eq!(
        q.wait(&mut t, &host, &[first], 1_000),
        Err(DriverError::NotFound)
    );
    assert_eq!(q.retire(second), Ok(4));
}

#[test]
fn nothing_is_published_that_the_queue_cannot_hold() {
    let host = MockHost::new();
    let mut t = device(&host, 4);
    let mut q = queue(&mut t, &host, 4);
    let slabs = Slabs::new(&host, &[1, 3, 5]);
    let one = q.publish(&slabs.request(0)).expect("two descriptors");
    let two = q.publish(&slabs.request(1)).expect("two more");
    assert_eq!(q.free_descriptors(), 0);
    assert_eq!(q.publish(&slabs.request(2)), Err(DriverError::Busy));
    assert_eq!(q.publish(&[]), Err(DriverError::DeviceFault));
    let long = [slabs.request(0)[0]; 5];
    assert_eq!(q.publish(&long), Err(DriverError::DeviceFault));
    q.kick(&mut t);
    for _ in 0..2 {
        t.answer_held(0, 0, |chain| {
            let tag = tag_of(chain);
            answer_with(chain, tag)
        })
        .expect("held");
    }
    assert_eq!(q.drain(), Ok(2));
    assert_eq!((q.retire(one), q.retire(two)), (Ok(1), Ok(3)));
    assert_eq!(q.free_descriptors(), 4);
}

#[test]
fn answers_never_retired_bound_what_may_be_published() {
    let host = MockHost::new();
    let mut t = device(&host, 4);
    let mut q = queue(&mut t, &host, 4);
    let slabs = Slabs::new(&host, &[2]);
    let one = [slabs.request(0)[1]];
    let tickets: Vec<Ticket> = (0..4)
        .map(|_| q.publish(&one).expect("one descriptor each"))
        .collect();
    q.kick(&mut t);
    assert_eq!(q.drain(), Ok(4));
    assert_eq!(q.free_descriptors(), 4, "every chain is back");
    assert_eq!(
        q.publish(&one),
        Err(DriverError::Busy),
        "four answers are yet to be taken"
    );
    q.retire(tickets[0]).expect("answered");
    q.publish(&one).expect("room again");
}

#[test]
fn an_answer_for_no_request_is_refused_and_the_ones_before_it_kept() {
    let host = MockHost::new();
    let mut t = device(&host, 8);
    let mut q = queue(&mut t, &host, 8);
    let slabs = Slabs::new(&host, &[2]);
    let ticket = q.publish(&slabs.request(0)).expect("published");
    q.kick(&mut t);
    t.publish_raw_used(0, 7, 1).expect("programmed");
    assert_eq!(q.drain(), Err(DriverError::DeviceFault));
    assert_eq!(q.answer(ticket), Ok(Some(2)));
    assert_eq!(q.drain(), Ok(0), "the bogus answer was skipped");
}

#[test]
fn a_ticket_named_twice_is_waited_for_once() {
    let host = MockHost::new();
    let mut t = device(&host, 4);
    let mut q = queue(&mut t, &host, 4);
    let slabs = Slabs::new(&host, &[2]);
    let ticket = q.publish(&slabs.request(0)).expect("published");
    q.kick(&mut t);
    assert_eq!(q.wait(&mut t, &host, &[ticket, ticket], 1_000), Ok(()));
    assert_eq!(
        q.wait(&mut t, &host, &[ticket], 1_000),
        Ok(()),
        "an answered ticket costs no wait"
    );
    assert_eq!(q.retire(ticket), Ok(2));
}
