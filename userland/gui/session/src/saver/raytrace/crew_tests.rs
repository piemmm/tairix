//! Host tests of the desk between a reveal's tracing thread and the serve
//! loop: what the thread is told to do, what reaches the loop, what a request
//! drops, when the thread is owed a signal, and the whole protocol between a
//! real tracing thread and a loop.

extern crate std;

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_raytrace::Block;

use super::{
    run_tracing_thread, DeskLink, DeskLock, Order, Request, Status, TraceDesk, TraceLink, Turn,
    QUEUED_SLICES,
};
use crate::saver::raytrace::engine::{Engine, Traced};

/// A one-pixel step at column `x`.
fn step(x: u32) -> Traced {
    Traced {
        block: Block {
            x,
            y: 0,
            width: 1,
            height: 1,
        },
        pixel: Pixel::TRANSPARENT,
    }
}

/// The slice the desk hands the thread, which must be one.
fn slice(desk: &mut TraceDesk) -> Order {
    match desk.turn() {
        Turn::Trace(order) => order,
        other => panic!("expected a slice, got {other:?}"),
    }
}

fn columns(steps: &[Traced]) -> Vec<u32> {
    steps.iter().map(|traced| traced.block.x).collect()
}

#[test]
fn a_fresh_desk_asks_for_a_slice_and_has_nothing_to_collect() {
    let mut desk = TraceDesk::new();
    assert!(matches!(
        desk.turn(),
        Turn::Trace(Order { request: None, .. })
    ));
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Working);
    assert!(into.is_empty());
}

/// Slices reach the loop whole and in the order traced, with where the
/// reveal stood after the last of them, and the loop's own buffer goes back
/// to the desk to be filled again.
#[test]
fn what_is_laid_down_reaches_the_loop_in_order_with_its_status() {
    let mut desk = TraceDesk::new();
    let first = slice(&mut desk);
    desk.deposit(first, &[step(0), step(1)], Status::Working);
    let second = slice(&mut desk);
    desk.deposit(second, &[step(2)], Status::Whole);
    let mut into = Vec::with_capacity(64);
    into.push(step(9));
    assert_eq!(desk.collect(&mut into), Status::Whole);
    assert_eq!(columns(&into), [0, 1, 2], "a stale step is never collected");
    let mut again = Vec::new();
    assert_eq!(desk.collect(&mut again), Status::Whole);
    assert!(again.is_empty(), "nothing is collected twice");
}

/// Once the loop is two of its frames behind, the thread waits rather than
/// piling up what it traced; a collection lets it go on.
#[test]
fn the_thread_waits_while_the_loop_is_behind_and_goes_on_once_it_collects() {
    let mut desk = TraceDesk::new();
    for column in 0..QUEUED_SLICES {
        let order = slice(&mut desk);
        desk.deposit(
            order,
            &[step(u32::try_from(column).expect("few"))],
            Status::Working,
        );
    }
    assert_eq!(desk.turn(), Turn::Wait);
    assert!(desk.waiting);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Working);
    assert_eq!(into.len(), usize::try_from(QUEUED_SLICES).expect("few"));
    let _ = slice(&mut desk);
    assert!(!desk.waiting);
}

/// With the scene whole, or refused, there is nothing to trace until the
/// loop asks for something, and the thread waits for it.
#[test]
fn a_finished_or_failed_reveal_waits_until_the_loop_asks() {
    for end in [Status::Whole, Status::Failed] {
        let mut desk = TraceDesk::new();
        let order = slice(&mut desk);
        desk.deposit(order, &[], end);
        assert_eq!(desk.turn(), Turn::Wait, "{end:?}");
        let mut into = Vec::new();
        assert_eq!(desk.collect(&mut into), end);
        assert_eq!(desk.turn(), Turn::Wait, "collecting asks for nothing");
        desk.request(Request::Next);
        assert!(matches!(
            desk.turn(),
            Turn::Trace(Order {
                request: Some(Request::Next),
                ..
            })
        ));
        assert_eq!(
            desk.collect(&mut into),
            Status::Working,
            "the next scene is under way"
        );
    }
}

/// A request drops what was traced before it, and a slice the thread was
/// tracing when the loop asked is dropped when it is laid down.
#[test]
fn a_request_drops_everything_traced_before_it() {
    let mut desk = TraceDesk::new();
    let before = slice(&mut desk);
    desk.deposit(before, &[step(0)], Status::Working);
    let in_flight = slice(&mut desk);
    desk.request(Request::Again);
    desk.deposit(in_flight, &[step(1)], Status::Whole);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Working);
    assert!(into.is_empty(), "{:?}", columns(&into));
    let asked = slice(&mut desk);
    assert_eq!(asked.request, Some(Request::Again));
    desk.deposit(asked, &[step(2)], Status::Working);
    assert_eq!(desk.collect(&mut into), Status::Working);
    assert_eq!(columns(&into), [2]);
}

/// A scene asked for is one from its start already, so `Next` stands over
/// `Again` whichever came first; two of the same are one.
#[test]
fn of_two_requests_not_yet_taken_next_stands() {
    for (first, second, standing) in [
        (Request::Next, Request::Again, Request::Next),
        (Request::Again, Request::Next, Request::Next),
        (Request::Again, Request::Again, Request::Again),
        (Request::Next, Request::Next, Request::Next),
    ] {
        let mut desk = TraceDesk::new();
        desk.request(first);
        desk.request(second);
        assert_eq!(slice(&mut desk).request, Some(standing));
        assert_eq!(slice(&mut desk).request, None, "taken once");
    }
}

#[test]
fn a_departed_loop_sends_the_thread_away_whatever_is_pending() {
    let mut desk = TraceDesk::new();
    desk.request(Request::Next);
    desk.leaving = true;
    assert_eq!(desk.turn(), Turn::Leave);
    assert!(!desk.waiting);
}

/// A thread tracing needs no signal to see what the loop did; one waiting
/// does, and is marked as waiting only while it is.
#[test]
fn only_a_waiting_thread_is_owed_a_signal() {
    let mut desk = TraceDesk::new();
    assert!(!desk.waiting, "a thread not yet turned is not parked");
    let order = slice(&mut desk);
    desk.deposit(order, &[], Status::Whole);
    assert!(!desk.waiting, "tracing, not waiting");
    assert_eq!(desk.turn(), Turn::Wait);
    assert!(desk.waiting);
    desk.request(Request::Next);
    assert!(desk.waiting, "still parked until it turns again");
    let _ = slice(&mut desk);
    assert!(!desk.waiting);
}

/// A desk behind the host's own lock and condition variable, as the
/// embedder's is behind the runtime's.
struct HostDesk {
    desk: std::sync::Mutex<TraceDesk>,
    turn: std::sync::Condvar,
}

impl DeskLock for HostDesk {
    type Guard<'a> = std::sync::MutexGuard<'a, TraceDesk>;

    fn lock(&self) -> Self::Guard<'_> {
        self.desk.lock().expect("an unpoisoned desk")
    }

    fn park<'a>(&'a self, held: Self::Guard<'a>) -> Self::Guard<'a> {
        self.turn.wait(held).expect("an unpoisoned desk")
    }

    fn signal(&self) {
        self.turn.notify_one();
    }
}

const SIZE: (u32, u32) = (48, 27);

/// A clock that reads a millisecond later every time it is read.
fn ticking() -> impl FnMut() -> u64 {
    let mut now = 0u64;
    move || {
        now += 1_000_000;
        now
    }
}

/// Paint `steps` over `picture` as the saver paints them.
fn paint(steps: &[Traced], picture: &mut [Pixel]) {
    for traced in steps {
        let block = traced.block;
        for y in block.y..block.y + block.height {
            for x in block.x..block.x + block.width {
                picture[(y * SIZE.0 + x) as usize] = traced.pixel;
            }
        }
    }
}

/// The pictures of the first `scenes` reveals of an engine seeded `seed`,
/// traced alone on this thread.
fn traced_alone(seed: u64, scenes: usize) -> Vec<Vec<Pixel>> {
    let mut engine = Engine::new(SIZE, seed).expect("an engine");
    let mut clock = ticking();
    let mut pictures = Vec::new();
    for _ in 0..scenes {
        let mut steps = Vec::new();
        while engine.step(&tairix_parallel::SERIAL, &mut steps, &mut clock) == Status::Working {}
        let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
        paint(&steps, &mut picture);
        pictures.push(picture);
        engine.apply(Request::Next);
    }
    pictures
}

/// A real tracing thread on the production loop and a loop collecting on its
/// own time pass whole reveals through the desk — the thread held back while
/// the loop is behind, stopped at each whole scene until the next is asked
/// for — every step arriving once and in order, and the thread leaves once the
/// link is dropped.
#[test]
fn a_tracing_thread_and_a_loop_pass_whole_reveals_through_the_desk() {
    let seed = 41;
    let desk = Arc::new(HostDesk {
        desk: std::sync::Mutex::new(TraceDesk::new()),
        turn: std::sync::Condvar::new(),
    });
    let thread = {
        let served = Arc::clone(&desk);
        std::thread::spawn(move || {
            run_tracing_thread(&*served, &tairix_parallel::SERIAL, &mut ticking());
        })
    };
    let link = DeskLink::hand_over(
        Arc::clone(&desk),
        Engine::new(SIZE, seed).expect("an engine"),
    );
    let mut pictures = Vec::new();
    let mut drawn = Vec::new();
    for _ in 0..2 {
        let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
        loop {
            let status = link.collect(&mut drawn);
            paint(&drawn, &mut picture);
            match status {
                Status::Working => std::thread::sleep(std::time::Duration::from_millis(1)),
                Status::Whole => break,
                Status::Failed => panic!("the heap refused a scene"),
            }
        }
        pictures.push(picture);
        link.request(Request::Next);
    }
    drop(link);
    thread
        .join()
        .expect("the thread leaves once the link is dropped");
    assert_eq!(pictures, traced_alone(seed, 2));
}
