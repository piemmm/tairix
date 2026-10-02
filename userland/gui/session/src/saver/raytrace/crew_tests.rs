//! Host tests of the desk between a reveal's tracing thread and the serve
//! loop: what the thread is told to do, what reaches the loop, what asking
//! for the next scene drops, when the thread is owed a signal, and the whole
//! protocol between a real tracing thread and a loop.

extern crate std;

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_raytrace::Step;

use super::{
    run_tracing_thread, DeskLink, DeskLock, Keeper, Order, Status, TraceDesk, TraceLink, Turn,
    QUEUED_SLICES,
};
use crate::saver::raytrace::album::{Picture, Unkept};
use crate::saver::raytrace::engine::{Engine, Traced};

/// A step of the last pass at column `x`.
fn step(x: u32) -> Traced {
    Traced {
        step: Step { x, y: 0, side: 1 },
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
    steps.iter().map(|traced| traced.step.x).collect()
}

#[test]
fn a_fresh_desk_asks_for_a_slice_and_has_nothing_to_collect() {
    let mut desk = TraceDesk::new();
    assert!(matches!(
        desk.turn(),
        Turn::Trace(Order { next: false, .. })
    ));
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Preparing(0));
    assert!(into.is_empty());
}

/// Slices reach the loop whole and in the order traced, with where the
/// reveal stood after the last of them, and the loop's own buffer goes back
/// to the desk to be filled again.
#[test]
fn what_is_laid_down_reaches_the_loop_in_order_with_its_status() {
    let mut desk = TraceDesk::new();
    let first = slice(&mut desk);
    desk.deposit(first, &[step(0), step(1)], Status::Tracing(400));
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
            Status::Tracing(1),
        );
    }
    assert_eq!(desk.turn(), Turn::Wait);
    assert!(desk.waiting);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Tracing(1));
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
        desk.next();
        assert!(matches!(desk.turn(), Turn::Trace(Order { next: true, .. })));
        assert_eq!(
            desk.collect(&mut into),
            Status::Preparing(0),
            "the next scene is under way"
        );
    }
}

/// Asking for the next scene drops what was traced before it — a slice the
/// thread was tracing when the loop asked is dropped when it is laid down —
/// and the next scene stands as one being prepared, wherever the last stood.
#[test]
fn asking_for_the_next_scene_drops_everything_traced_before_it() {
    let mut desk = TraceDesk::new();
    let before = slice(&mut desk);
    desk.deposit(before, &[step(0)], Status::Tracing(300));
    let in_flight = slice(&mut desk);
    desk.next();
    desk.deposit(in_flight, &[step(1)], Status::Whole);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Preparing(0));
    assert!(into.is_empty(), "{:?}", columns(&into));
    let asked = slice(&mut desk);
    assert!(asked.next);
    desk.deposit(asked, &[step(2)], Status::Tracing(1));
    assert_eq!(desk.collect(&mut into), Status::Tracing(1));
    assert_eq!(columns(&into), [2]);
}

/// Each ask begins a scene from its start, so two the thread has not yet
/// taken are one.
#[test]
fn two_asks_not_yet_taken_are_one() {
    let mut desk = TraceDesk::new();
    desk.next();
    desk.next();
    assert!(slice(&mut desk).next);
    assert!(!slice(&mut desk).next, "taken once");
}

#[test]
fn a_departed_loop_sends_the_thread_away_whatever_is_pending() {
    let mut desk = TraceDesk::new();
    desk.next();
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
    desk.next();
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

/// Lay each of `steps`' colours at its own pixel of `picture`.
fn paint(steps: &[Traced], picture: &mut [Pixel]) {
    for traced in steps {
        picture[(traced.step.y * SIZE.0 + traced.step.x) as usize] = traced.pixel;
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
        while engine
            .step(&tairix_parallel::SERIAL, &mut steps, &mut clock)
            .is_working()
        {}
        let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
        paint(&steps, &mut picture);
        pictures.push(picture);
        engine.next();
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
            run_tracing_thread(&*served, &tairix_parallel::SERIAL, &mut ticking(), None);
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
                Status::Preparing(_) | Status::Tracing(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Status::Whole => break,
                Status::Failed => panic!("the heap refused a scene"),
            }
        }
        pictures.push(picture);
        link.next();
    }
    drop(link);
    thread
        .join()
        .expect("the thread leaves once the link is dropped");
    assert_eq!(pictures, traced_alone(seed, 2));
}

/// A keeper that records what it was handed, for the loop to look at.
struct Recording(Arc<std::sync::Mutex<Vec<Result<Picture, Unkept>>>>);

impl Keeper for Recording {
    fn keep(&mut self, picture: Result<Picture, Unkept>) {
        self.0.lock().expect("an unpoisoned record").push(picture);
    }
}

/// A thread whose engine keeps pictures hands each whole one to its keeper
/// once, after laying its last steps down, so the loop has the picture on
/// screen before the keeping begins.
#[test]
fn a_tracing_thread_hands_each_whole_picture_to_its_keeper_once() {
    let seed = 43;
    let desk = Arc::new(HostDesk {
        desk: std::sync::Mutex::new(TraceDesk::new()),
        turn: std::sync::Condvar::new(),
    });
    let kept = Arc::new(std::sync::Mutex::new(Vec::new()));
    let thread = {
        let served = Arc::clone(&desk);
        let mut keeper = Recording(Arc::clone(&kept));
        std::thread::spawn(move || {
            run_tracing_thread(
                &*served,
                &tairix_parallel::SERIAL,
                &mut ticking(),
                Some(&mut keeper),
            );
        })
    };
    let mut engine = Engine::new(SIZE, seed).expect("an engine");
    engine.keep_pictures();
    let link = DeskLink::hand_over(Arc::clone(&desk), engine);
    let mut drawn = Vec::new();
    let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
    while link.collect(&mut drawn).is_working() {
        paint(&drawn, &mut picture);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    paint(&drawn, &mut picture);
    drop(link);
    thread
        .join()
        .expect("the thread leaves once the link is dropped");
    let kept = kept.lock().expect("an unpoisoned record");
    assert_eq!(kept.len(), 1, "one picture for one scene");
    let Some(Ok(whole)) = kept.first() else {
        panic!("a picture, not {kept:?}");
    };
    assert_eq!(whole.pixels, picture);
    assert_eq!(whole.pixels, traced_alone(seed, 1)[0]);
}
