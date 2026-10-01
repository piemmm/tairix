//! A reveal traced on a thread of its own: the seam the embedder grants the
//! thread through, the desk between it and the serve loop, and both ends of
//! the protocol over that desk — the thread's loop and the serve loop's link.
//!
//! The embedder supplies only its lock, its condition variable and the thread
//! itself ([`DeskLock`]); everything they carry out is here. The serve loop
//! collects on its own frame rather than being woken: it is an animation, so
//! a wake could only bring it to wait for that frame anyway. What the thread
//! lays down is bounded, so a loop that stops drawing holds the thread back
//! rather than letting what it traced pile up.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::DerefMut;

use tairix_parallel::JobRunner;
use tairix_wallpaper::RaytraceOptions;

use super::album::{Picture, Unkept};
use super::engine::{Engine, Traced, SLICE_NS};
use tairix_theme::motion::SceneClock;

/// How many slices may wait for the serve loop before the tracing thread
/// stops for it: two of the loop's frames' worth, so a frame it spends
/// elsewhere never idles the thread.
const QUEUED_SLICES: u64 = 2 * SceneClock::FRAME_NS / SLICE_NS;

// A queue of none would hold the thread back from its first slice for good.
const _: () = assert!(QUEUED_SLICES > 0);

/// Where a reveal stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Status {
    /// The scene is being prepared, this many thousandths of the way.
    Preparing(u16),
    /// The scene is being traced, this many thousandths of its pixels done.
    Tracing(u16),
    /// Every pixel of the scene has been traced.
    Whole,
    /// The heap would not hold the scene, or what was traced of it.
    Failed,
}

impl Status {
    /// Whether more of the picture is to come.
    #[must_use]
    pub const fn is_working(self) -> bool {
        matches!(self, Self::Preparing(_) | Self::Tracing(_))
    }
}

/// What the serve loop asks of a reveal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// A scene in another setting.
    Next,
    /// The current scene again from its first step: the window let the
    /// picture go.
    Again,
}

/// The threads a reveal may be traced on, away from the serve loop.
pub trait TraceHost {
    /// Trace `engine`'s reveals on threads of their own as `options` ask: one
    /// when the processor use is idle, and one for each core under
    /// performance; and keep each whole picture there when they ask for that.
    ///
    /// # Errors
    ///
    /// `engine`, back, when the machine grants no thread; the serve loop then
    /// traces it itself, and keeps nothing.
    #[allow(
        clippy::result_large_err,
        reason = "the engine comes back at most once a screensaver start, and only to be traced \
                  on the loop; boxing it would trade that one move for an allocation that cannot \
                  fail gracefully"
    )]
    fn launch(
        &self,
        engine: Engine,
        options: RaytraceOptions,
    ) -> Result<Box<dyn TraceLink>, Engine>;
}

/// Where a reveal's whole pictures go, on the thread that traced them.
pub trait Keeper {
    /// Keep `picture`, or say why it could not be kept.
    fn keep(&mut self, picture: Result<Picture, Unkept>);
}

/// The serve loop's end of a reveal traced on other threads. Dropping it
/// sends them away.
pub trait TraceLink {
    /// Move every step traced since the last call into `into`, in the order
    /// traced, and answer where the reveal stands.
    fn collect(&self, into: &mut Vec<Traced>) -> Status;

    /// Ask for `request`, dropping everything traced before it.
    fn request(&self, request: Request);
}

/// The embedder's lock around a reveal's [`TraceDesk`], and the condition
/// variable its tracing thread parks on.
pub trait DeskLock {
    /// Proof the desk is held; dropping it releases the desk.
    type Guard<'a>: DerefMut<Target = TraceDesk>
    where
        Self: 'a;

    /// Take the desk, waiting while another thread holds it.
    fn lock(&self) -> Self::Guard<'_>;

    /// Give the desk up until [`signal`](Self::signal)led, or woken for no
    /// reason, and take it back.
    fn park<'a>(&'a self, held: Self::Guard<'a>) -> Self::Guard<'a>;

    /// Wake the tracing thread, if it is parked.
    fn signal(&self);
}

/// The whole life of a thread tracing over `desk`: wait for its engine, then
/// trace it across `runner` a slice at a time, laying each down on the desk,
/// until the serve loop leaves, parked whenever there is nothing to trace;
/// `clock` reads the monotonic clock, to pace the slices, and `keeper` keeps
/// each whole picture, once it is on the desk, if the engine keeps them.
pub fn run_tracing_thread(
    desk: &impl DeskLock,
    runner: &dyn JobRunner,
    clock: &mut dyn FnMut() -> u64,
    mut keeper: Option<&mut dyn Keeper>,
) {
    let mut engine = {
        let mut held = desk.lock();
        loop {
            if held.leaving {
                return;
            }
            if let Some(engine) = held.engine.take() {
                break engine;
            }
            held = desk.park(held);
        }
    };
    let mut slice = Vec::new();
    loop {
        let order = {
            let mut held = desk.lock();
            loop {
                match held.turn() {
                    Turn::Leave => return,
                    Turn::Wait => held = desk.park(held),
                    Turn::Trace(order) => break order,
                }
            }
        };
        let status = order.carry_out(&mut engine, runner, &mut slice, clock);
        desk.lock().deposit(order, &slice, status);
        slice.clear();
        if let Some(finished) = engine.take_finished() {
            if let Some(keeper) = keeper.as_mut() {
                keeper.keep(finished);
            }
        }
    }
}

/// The serve loop's end of a reveal traced by a thread running
/// [`run_tracing_thread`] over `L`'s desk.
pub struct DeskLink<L: DeskLock>(Arc<L>);

impl<L: DeskLock> DeskLink<L> {
    /// Hand `engine` to the thread tracing over `desk`, and link the serve
    /// loop to it.
    #[must_use]
    pub fn hand_over(desk: Arc<L>, engine: Engine) -> Self {
        desk.lock().engine = Some(engine);
        desk.signal();
        Self(desk)
    }
}

impl<L: DeskLock> TraceLink for DeskLink<L> {
    fn collect(&self, into: &mut Vec<Traced>) -> Status {
        let (status, waiting) = {
            let mut held = self.0.lock();
            (held.collect(into), held.waiting)
        };
        if waiting {
            self.0.signal();
        }
        status
    }

    fn request(&self, request: Request) {
        let waiting = {
            let mut held = self.0.lock();
            held.request(request);
            held.waiting
        };
        if waiting {
            self.0.signal();
        }
    }
}

impl<L: DeskLock> Drop for DeskLink<L> {
    fn drop(&mut self) {
        self.0.lock().leaving = true;
        self.0.signal();
    }
}

/// What the tracing thread does next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Turn {
    /// The serve loop has gone.
    Leave,
    /// Nothing to do until the loop collects or asks.
    Wait,
    /// Carry out this slice.
    Trace(Order),
}

/// One slice of work, under what the loop last asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Order {
    generation: u32,
    request: Option<Request>,
}

impl Order {
    /// Take up what the loop asked, if anything, then trace one slice of
    /// `engine` across `runner` into `out`, answering where the reveal stands.
    pub(super) fn carry_out(
        self,
        engine: &mut Engine,
        runner: &dyn JobRunner,
        out: &mut Vec<Traced>,
        clock: &mut dyn FnMut() -> u64,
    ) -> Status {
        if let Some(request) = self.request {
            engine.apply(request);
        }
        engine.step(runner, out, clock)
    }
}

/// What stands between a reveal's tracing thread and the serve loop: the
/// engine on its way to the thread, what has been traced and not yet
/// collected, where the reveal stands, and what the loop has asked since.
///
/// The embedder holds it behind a [`DeskLock`].
pub struct TraceDesk {
    engine: Option<Engine>,
    ready: Vec<Traced>,
    /// How many slices `ready` holds.
    slices: u64,
    status: Status,
    /// Bumped by every request, so a slice traced before it is recognised.
    generation: u32,
    asked: Option<Request>,
    /// Whether the thread's last turn was to wait, and so whether the loop
    /// owes it a signal when it collects or asks.
    waiting: bool,
    leaving: bool,
}

impl Default for TraceDesk {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceDesk {
    /// A desk for a reveal just begun, its engine yet to be handed over.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            engine: None,
            ready: Vec::new(),
            slices: 0,
            status: Status::Preparing(0),
            generation: 0,
            asked: None,
            waiting: false,
            leaving: false,
        }
    }

    /// The tracing thread's next move: trace whatever the loop asked for,
    /// wait while the scene is done or the loop is behind, else trace on.
    pub(super) fn turn(&mut self) -> Turn {
        let turn = if self.leaving {
            Turn::Leave
        } else if let Some(request) = self.asked.take() {
            Turn::Trace(Order {
                generation: self.generation,
                request: Some(request),
            })
        } else if !self.status.is_working() || self.slices >= QUEUED_SLICES {
            Turn::Wait
        } else {
            Turn::Trace(Order {
                generation: self.generation,
                request: None,
            })
        };
        self.waiting = turn == Turn::Wait;
        turn
    }

    /// Lay down what `order` traced and where the reveal then stood, unless
    /// the loop has asked for something since; memory refused for it fails
    /// the reveal rather than leaving a hole in the picture.
    fn deposit(&mut self, order: Order, traced: &[Traced], status: Status) {
        if order.generation != self.generation {
            return;
        }
        if self.ready.try_reserve(traced.len()).is_err() {
            self.status = Status::Failed;
            return;
        }
        self.ready.extend_from_slice(traced);
        self.slices = self.slices.saturating_add(1);
        self.status = status;
    }

    /// Move everything laid down into `into`, and answer where the reveal
    /// stands.
    fn collect(&mut self, into: &mut Vec<Traced>) -> Status {
        into.clear();
        core::mem::swap(&mut self.ready, into);
        self.slices = 0;
        self.status
    }

    /// Ask for `request`, dropping everything traced before it. A scene asked
    /// for is already one from its start, so of two requests the tracing
    /// thread has not yet taken, `Next` stands.
    pub(super) fn request(&mut self, request: Request) {
        self.asked = Some(match (self.asked, request) {
            (Some(Request::Next), _) | (_, Request::Next) => Request::Next,
            (_, Request::Again) => Request::Again,
        });
        self.generation = self.generation.wrapping_add(1);
        self.ready.clear();
        self.slices = 0;
        // A scene asked for is prepared afresh; the same one again goes on
        // standing where it stood until the thread reports its restart.
        if request == Request::Next || !self.status.is_working() {
            self.status = Status::Preparing(0);
        }
    }
}

#[cfg(test)]
#[path = "crew_tests.rs"]
mod tests;
