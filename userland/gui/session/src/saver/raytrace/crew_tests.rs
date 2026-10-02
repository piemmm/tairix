//! Host tests of the desk between a reveal's tracing thread and the serve
//! loop: what the thread is told to do, what reaches the loop, what asking
//! for the next scene drops, when the thread is owed a signal and the loop a
//! wake, and the whole protocol between a real tracing thread and a loop.

extern crate std;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tairix_raster::Pixel;
use tairix_raytrace::Step;

use super::{
    run_tracing_thread, DeskLink, DeskLock, Keeper, Order, Status, TraceDesk, TraceLink, Turn,
    LATE_NS,
};
use crate::saver::raytrace::album::{Picture, Unkept};
use crate::saver::raytrace::engine::{Engine, Traced, PLAIN};

/// A step of the last pass at column `x`.
fn step(x: u32) -> Traced {
    Traced {
        step: Step { x, y: 0, side: 1 },
        pixel: Pixel::TRANSPARENT,
    }
}

/// The slice the desk hands the thread, which must be one.
fn slice(desk: &mut TraceDesk) -> Order {
    match desk.turn(0) {
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
        desk.turn(0),
        Turn::Trace(Order { next: false, .. })
    ));
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Preparing(0));
    assert!(into.is_empty());
}

/// Slices reach the loop whole and in the order traced, with where the
/// reveal stood after the last of them, and the loop's own empty buffer goes
/// back to the desk to be filled again.
#[test]
fn what_is_laid_down_reaches_the_loop_in_order_with_its_status() {
    let mut desk = TraceDesk::new();
    let first = slice(&mut desk);
    desk.deposit(first, &[step(0), step(1)], Status::Tracing(400), 0);
    let second = slice(&mut desk);
    desk.deposit(second, &[step(2)], Status::Whole, 0);
    let mut into = Vec::with_capacity(64);
    assert_eq!(desk.collect(&mut into), Status::Whole);
    assert_eq!(columns(&into), [0, 1, 2]);
    assert!(
        desk.ready.capacity() >= 64,
        "the loop's buffer is the desk's now"
    );
    let mut again = Vec::new();
    assert_eq!(desk.collect(&mut again), Status::Whole);
    assert!(again.is_empty(), "nothing is collected twice");
}

/// Steps the loop has collected and not yet painted stay ahead of those it
/// collects next, so a collection between paints leaves no hole.
#[test]
fn a_collection_keeps_what_the_loop_has_not_yet_painted() {
    let mut desk = TraceDesk::new();
    let first = slice(&mut desk);
    desk.deposit(first, &[step(0)], Status::Tracing(1), 0);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Tracing(1));
    let second = slice(&mut desk);
    desk.deposit(second, &[step(1), step(2)], Status::Tracing(2), 0);
    assert_eq!(desk.collect(&mut into), Status::Tracing(2));
    assert_eq!(columns(&into), [0, 1, 2]);
}

/// Once what was laid down has waited two of the loop's longest waits, the
/// thread waits rather than piling up what it traces; a collection lets it go
/// on.
#[test]
fn the_thread_waits_while_the_loop_is_behind_and_goes_on_once_it_collects() {
    let mut desk = TraceDesk::new();
    let first = slice(&mut desk);
    desk.deposit(first, &[step(0)], Status::Tracing(1), 1_000);
    let later = 1_000 + LATE_NS - 1;
    let Turn::Trace(second) = desk.turn(later) else {
        panic!("not yet behind");
    };
    desk.deposit(second, &[step(1)], Status::Tracing(2), later);
    assert_eq!(
        desk.turn(1_000 + LATE_NS),
        Turn::Wait,
        "behind by what waited longest"
    );
    assert!(desk.waiting);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Tracing(2));
    assert_eq!(columns(&into), [0, 1]);
    assert!(matches!(desk.turn(u64::MAX), Turn::Trace(_)));
    assert!(!desk.waiting);
}

/// A core hands its steps straight onto the desk, the reveal's progress the
/// furthest any core has told, until the loop asks for another scene or goes.
#[test]
fn a_core_hands_its_steps_over_until_the_loop_asks_for_more() {
    let mut desk = TraceDesk::new();
    let order = slice(&mut desk);
    desk.deposit(order, &[], Status::Tracing(0), 0);
    assert!(desk.hand(order, &[step(0), step(1)], Status::Tracing(5)));
    assert!(
        desk.hand(order, &[step(2)], Status::Tracing(3)),
        "a core behind"
    );
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Tracing(5));
    assert_eq!(columns(&into), [0, 1, 2]);
    desk.next();
    assert!(
        !desk.hand(order, &[step(3)], Status::Tracing(9)),
        "a scene the loop no longer wants"
    );
    into.clear();
    assert_eq!(desk.collect(&mut into), Status::Preparing(0));
    assert!(into.is_empty());
    let asked = slice(&mut desk);
    desk.leaving = true;
    assert!(
        !desk.hand(asked, &[step(4)], Status::Tracing(1)),
        "the loop is gone"
    );
}

/// A reveal that failed stays failed: nothing laid down after the failure
/// reaches the loop to leave a hole in the picture behind it.
#[test]
fn a_failed_reveal_takes_nothing_more() {
    let mut desk = TraceDesk::new();
    let order = slice(&mut desk);
    desk.deposit(order, &[step(0)], Status::Tracing(1), 0);
    // As a collection the heap refused room for leaves it.
    desk.status = Status::Failed;
    assert!(!desk.deposit(order, &[step(1)], Status::Tracing(2), 0));
    assert!(!desk.hand(order, &[step(2)], Status::Tracing(3)));
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Failed);
    assert_eq!(columns(&into), [0]);
    assert_eq!(desk.turn(0), Turn::Wait);
}

/// With the scene whole, or refused, there is nothing to trace until the
/// loop asks for something, and the thread waits for it.
#[test]
fn a_finished_or_failed_reveal_waits_until_the_loop_asks() {
    for end in [Status::Whole, Status::Failed] {
        let mut desk = TraceDesk::new();
        let order = slice(&mut desk);
        desk.deposit(order, &[], end, 0);
        assert_eq!(desk.turn(0), Turn::Wait, "{end:?}");
        let mut into = Vec::new();
        assert_eq!(desk.collect(&mut into), end);
        assert_eq!(desk.turn(0), Turn::Wait, "collecting asks for nothing");
        desk.next();
        assert!(matches!(
            desk.turn(0),
            Turn::Trace(Order { next: true, .. })
        ));
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
    desk.deposit(before, &[step(0)], Status::Tracing(300), 0);
    let in_flight = slice(&mut desk);
    desk.next();
    desk.deposit(in_flight, &[step(1)], Status::Whole, 0);
    let mut into = Vec::new();
    assert_eq!(desk.collect(&mut into), Status::Preparing(0));
    assert!(into.is_empty(), "{:?}", columns(&into));
    let asked = slice(&mut desk);
    assert!(asked.next);
    desk.deposit(asked, &[step(2)], Status::Tracing(1), 0);
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
    assert_eq!(desk.turn(0), Turn::Leave);
    assert!(!desk.waiting);
}

/// The loop is to be woken as each scene's preparation ends, refused or
/// readied, and at no other deposit: not while a scene is prepared or traced,
/// nor for a slice the loop no longer wants.
#[test]
fn the_loop_is_woken_as_each_scene_is_readied_and_only_then() {
    let mut desk = TraceDesk::new();
    let order = slice(&mut desk);
    assert!(!desk.deposit(order, &[], Status::Preparing(400), 0));
    let order = slice(&mut desk);
    assert!(desk.deposit(order, &[], Status::Tracing(0), 0), "readied");
    let order = slice(&mut desk);
    assert!(!desk.deposit(order, &[step(0)], Status::Tracing(1), 0));
    let in_flight = slice(&mut desk);
    desk.next();
    assert!(
        !desk.deposit(in_flight, &[], Status::Tracing(2), 0),
        "a slice of the scene before"
    );
    let asked = slice(&mut desk);
    assert!(!desk.deposit(asked, &[], Status::Preparing(10), 0));
    let order = slice(&mut desk);
    assert!(desk.deposit(order, &[], Status::Failed, 0), "refused");
}

/// A thread tracing needs no signal to see what the loop did; one waiting
/// does, and is marked as waiting only while it is.
#[test]
fn only_a_waiting_thread_is_owed_a_signal() {
    let mut desk = TraceDesk::new();
    assert!(!desk.waiting, "a thread not yet turned is not parked");
    let order = slice(&mut desk);
    desk.deposit(order, &[], Status::Whole, 0);
    assert!(!desk.waiting, "tracing, not waiting");
    assert_eq!(desk.turn(0), Turn::Wait);
    assert!(desk.waiting);
    desk.next();
    assert!(desk.waiting, "still parked until it turns again");
    let _ = slice(&mut desk);
    assert!(!desk.waiting);
}

/// A desk behind the host's own lock and condition variable, as the
/// embedder's is behind the runtime's, counting the loop's wakes.
struct HostDesk {
    desk: std::sync::Mutex<TraceDesk>,
    turn: std::sync::Condvar,
    nudges: AtomicUsize,
}

impl HostDesk {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            desk: std::sync::Mutex::new(TraceDesk::new()),
            turn: std::sync::Condvar::new(),
            nudges: AtomicUsize::new(0),
        })
    }
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

    fn nudge(&self) {
        self.nudges.fetch_add(1, Ordering::SeqCst);
    }
}

const SIZE: (u32, u32) = (48, 27);

/// A clock that reads a millisecond later every time it is read, on any
/// thread.
#[derive(Default)]
struct Ticking(AtomicU64);

impl Ticking {
    fn read(&self) -> u64 {
        self.0.fetch_add(1_000_000, Ordering::SeqCst) + 1_000_000
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
    let mut engine = Engine::new(SIZE, seed, PLAIN).expect("an engine");
    let clock = Ticking::default();
    let mut pictures = Vec::new();
    for _ in 0..scenes {
        let mut steps = Vec::new();
        while engine
            .step(&tairix_parallel::SERIAL, &mut steps, &mut || clock.read())
            .is_working()
        {}
        let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
        paint(&steps, &mut picture);
        pictures.push(picture);
        engine.next();
    }
    pictures
}

/// The first two scenes of an engine seeded `seed`, traced by a real thread
/// on the production loop across `runner` while a loop collects on its own
/// time: each scene's picture, and the spacing of every step it showed, in
/// the order the loop collected them.
fn passed_through_the_desk(
    seed: u64,
    runner: &'static dyn tairix_parallel::JobRunner,
) -> Vec<(Vec<Pixel>, Vec<u32>)> {
    let desk = HostDesk::new();
    let thread = {
        let served = Arc::clone(&desk);
        std::thread::spawn(move || {
            let clock = Ticking::default();
            run_tracing_thread(&*served, runner, &|| clock.read(), None);
        })
    };
    let link = DeskLink::hand_over(
        Arc::clone(&desk),
        Engine::new(SIZE, seed, PLAIN).expect("an engine"),
    );
    let mut scenes = Vec::new();
    let mut drawn = Vec::new();
    for scene in 1..=2 {
        let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
        let mut sides = Vec::new();
        loop {
            let status = link.collect(&mut drawn);
            paint(&drawn, &mut picture);
            sides.extend(drawn.iter().map(|traced| traced.step.side));
            drawn.clear();
            match status {
                Status::Preparing(_) | Status::Tracing(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Status::Whole => break,
                Status::Failed => panic!("the heap refused a scene"),
            }
        }
        assert_eq!(
            desk.nudges.load(Ordering::SeqCst),
            scene,
            "one wake a scene"
        );
        scenes.push((picture, sides));
        link.next();
    }
    drop(link);
    thread
        .join()
        .expect("the thread leaves once the link is dropped");
    scenes
}

/// A real tracing thread and a loop collecting on its own time pass whole
/// reveals through the desk — the thread held back while the loop is behind,
/// stopped at each whole scene until the next is asked for, the loop woken
/// once as each is readied — every step arriving once, a pass's steps never
/// before every step of the passes before it, and the thread leaves once the
/// link is dropped; on one core or across several, the same pictures.
#[test]
fn a_tracing_thread_and_a_loop_pass_whole_reveals_through_the_desk() {
    static ACROSS: tairix_parallel::Threaded = tairix_parallel::Threaded::new(4);
    let seed = 41;
    let alone = traced_alone(seed, 2);
    for runner in [
        &tairix_parallel::SERIAL as &'static dyn tairix_parallel::JobRunner,
        &ACROSS,
    ] {
        let scenes = passed_through_the_desk(seed, runner);
        for ((picture, sides), expected) in scenes.iter().zip(&alone) {
            assert_eq!(picture, expected);
            assert_eq!(sides.len(), (SIZE.0 * SIZE.1) as usize, "each step once");
            assert!(
                sides.windows(2).all(|pair| pair[1] <= pair[0]),
                "pass by pass"
            );
        }
    }
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
    let desk = HostDesk::new();
    let kept = Arc::new(std::sync::Mutex::new(Vec::new()));
    let thread = {
        let served = Arc::clone(&desk);
        let mut keeper = Recording(Arc::clone(&kept));
        std::thread::spawn(move || {
            let clock = Ticking::default();
            run_tracing_thread(
                &*served,
                &tairix_parallel::SERIAL,
                &|| clock.read(),
                Some(&mut keeper),
            );
        })
    };
    let mut engine = Engine::new(SIZE, seed, PLAIN).expect("an engine");
    engine.keep_pictures();
    let link = DeskLink::hand_over(Arc::clone(&desk), engine);
    let mut drawn = Vec::new();
    let mut picture = alloc::vec![Pixel::TRANSPARENT; (SIZE.0 * SIZE.1) as usize];
    while link.collect(&mut drawn).is_working() {
        paint(&drawn, &mut picture);
        drawn.clear();
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
