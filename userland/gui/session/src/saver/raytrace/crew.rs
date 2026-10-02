//! A reveal traced on a thread of its own: the seam the embedder grants the
//! thread through, the desk between it and the serve loop, and both ends of
//! the protocol over that desk — the thread's loop and the serve loop's link.
//!
//! The embedder supplies only its lock, its condition variable, the wake into
//! its serve loop and the thread itself ([`DeskLock`]); everything they carry
//! out is here. The serve loop collects on its own cadence, seconds apart once
//! only fine detail is left, and is woken only as a scene is readied, so the
//! scene's first passes are shown as they come. What the thread lays down is
//! bounded, so a loop that stops drawing holds the thread back rather than
//! letting what it traced pile up.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::DerefMut;

use tairix_parallel::JobRunner;
use tairix_wallpaper::RaytraceOptions;

use super::album::{Picture, Unkept};
use super::engine::{Engine, Handing, Traced, STRETCH_NS};
use super::MOST_WAIT_NS;

/// How long what the tracing thread laid down may wait for the serve loop
/// before the thread stops for it: two of the loop's longest waits between
/// collections, so a loop collecting late never idles the thread.
const LATE_NS: u64 = 2 * MOST_WAIT_NS;

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
    /// Move every step traced since the last call onto the end of `into`, in
    /// the order traced, and answer where the reveal stands.
    fn collect(&self, into: &mut Vec<Traced>) -> Status;

    /// Ask for a scene in another setting, dropping everything traced before.
    fn next(&self);
}

/// The embedder's lock around a reveal's [`TraceDesk`], the condition
/// variable its tracing thread parks on, and the wake into its serve loop.
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

    /// Wake the serve loop, from the tracing thread, to collect at once.
    fn nudge(&self);
}

/// The whole life of a thread tracing over `desk`: wait for its engine, then
/// prepare each scene across `runner` a slice at a time and trace it a
/// stretch at a time, every core handing each step onto the desk as it traces
/// it, nudging the serve loop as each scene is readied, until the loop leaves,
/// parked whenever there is nothing to trace; `clock` reads the monotonic
/// clock, from any of the cores, and `keeper` keeps each whole picture, once
/// it is on the desk, if the engine keeps them.
pub fn run_tracing_thread<L: DeskLock + Sync>(
    desk: &L,
    runner: &dyn JobRunner,
    clock: &(dyn Fn() -> u64 + Sync),
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
                match held.turn(clock()) {
                    Turn::Leave => return,
                    Turn::Wait => held = desk.park(held),
                    Turn::Trace(order) => break order,
                }
            }
        };
        let status = if engine.is_tracing() && !order.next {
            let handing = HandOver { desk, order };
            let until = clock().saturating_add(STRETCH_NS);
            engine.trace_stretch(runner, &handing, (clock, until))
        } else {
            order.carry_out(&mut engine, runner, &mut slice, &mut || clock())
        };
        let now = clock();
        let readied = desk.lock().deposit(order, &slice, status, now);
        if readied {
            desk.nudge();
        }
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

    fn next(&self) {
        let waiting = {
            let mut held = self.0.lock();
            held.next();
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
    /// Carry out this turn's work.
    Trace(Order),
}

/// One turn's work — a slice of a scene's preparation or a stretch of its
/// tracing — under what the loop last asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Order {
    generation: u32,
    /// Whether it begins the scene the loop asked for.
    next: bool,
}

impl Order {
    /// Begin the scene the loop asked for, if it asked, then trace one slice
    /// of `engine` across `runner` into `out`, answering where the reveal
    /// stands.
    pub(super) fn carry_out(
        self,
        engine: &mut Engine,
        runner: &dyn JobRunner,
        out: &mut Vec<Traced>,
        clock: &mut dyn FnMut() -> u64,
    ) -> Status {
        if self.next {
            engine.next();
        }
        engine.step(runner, out, clock)
    }
}

/// A stretch's cores handing their steps onto `desk`, traced under `order`.
struct HandOver<'a, L: DeskLock> {
    desk: &'a L,
    order: Order,
}

impl<L: DeskLock + Sync> Handing for HandOver<'_, L> {
    fn take(&self, steps: &[Traced], status: Status) -> bool {
        self.desk.lock().hand(self.order, steps, status)
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
    /// When the oldest of what the loop has yet to collect was laid down.
    since: Option<u64>,
    status: Status,
    /// Bumped by every ask, so work done before it is recognised.
    generation: u32,
    /// Whether the loop has asked for another scene the thread has not yet
    /// begun.
    asked: bool,
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
            since: None,
            status: Status::Preparing(0),
            generation: 0,
            asked: false,
            waiting: false,
            leaving: false,
        }
    }

    /// The tracing thread's next move at `now`: begin the scene the loop
    /// asked for, wait while the scene is done or the loop is behind, else
    /// trace on.
    pub(super) fn turn(&mut self, now: u64) -> Turn {
        let turn = if self.leaving {
            Turn::Leave
        } else if core::mem::take(&mut self.asked) {
            Turn::Trace(Order {
                generation: self.generation,
                next: true,
            })
        } else if !self.status.is_working() || self.behind(now) {
            Turn::Wait
        } else {
            Turn::Trace(Order {
                generation: self.generation,
                next: false,
            })
        };
        self.waiting = turn == Turn::Wait;
        turn
    }

    /// Whether what was laid down has waited for the loop so long, at `now`,
    /// that the thread is to stop for it.
    fn behind(&self, now: u64) -> bool {
        self.since
            .is_some_and(|since| now.saturating_sub(since) >= LATE_NS)
    }

    /// Lay down what `order` traced and where the reveal then stood at `now`,
    /// unless the loop has asked for something since or the reveal failed;
    /// memory refused for it fails the reveal rather than leaving a hole in
    /// the picture. Answers whether that ended the scene's preparation, which
    /// the loop is to see at once.
    fn deposit(&mut self, order: Order, traced: &[Traced], status: Status, now: u64) -> bool {
        if order.generation != self.generation || self.status == Status::Failed {
            return false;
        }
        let preparing = matches!(self.status, Status::Preparing(_));
        if self.ready.try_reserve(traced.len()).is_err() {
            self.status = Status::Failed;
        } else {
            self.ready.extend_from_slice(traced);
            self.since.get_or_insert(now);
            self.status = status;
        }
        preparing && !matches!(self.status, Status::Preparing(_))
    }

    /// Lay down `steps`, traced under `order` by one of the thread's cores,
    /// the reveal then standing at `status`; whether that core is to trace
    /// on: not once the loop has asked for something since or gone, or the
    /// reveal stopped working — memory refused for them fails it rather than
    /// leaving a hole in the picture.
    fn hand(&mut self, order: Order, steps: &[Traced], status: Status) -> bool {
        if self.leaving || order.generation != self.generation || !self.status.is_working() {
            return false;
        }
        if self.ready.try_reserve(steps.len()).is_err() {
            self.status = Status::Failed;
            return false;
        }
        self.ready.extend_from_slice(steps);
        // Cores hand over in no order, so a count behind one already laid
        // down can come after it.
        if let (Status::Tracing(held), Status::Tracing(now)) = (self.status, status) {
            self.status = Status::Tracing(held.max(now));
        }
        true
    }

    /// Move everything laid down onto the end of `into`, and answer where the
    /// reveal stands; memory refused for it fails the reveal rather than
    /// leaving a hole in the picture.
    fn collect(&mut self, into: &mut Vec<Traced>) -> Status {
        if into.is_empty() {
            core::mem::swap(&mut self.ready, into);
        } else if into.try_reserve(self.ready.len()).is_ok() {
            into.append(&mut self.ready);
        } else {
            self.ready.clear();
            self.status = Status::Failed;
        }
        self.since = None;
        self.status
    }

    /// Ask for a scene in another setting, dropping everything traced before.
    /// Two asks the tracing thread has not yet taken are one: each begins a
    /// scene from its start.
    pub(super) fn next(&mut self) {
        self.asked = true;
        self.generation = self.generation.wrapping_add(1);
        self.ready.clear();
        self.since = None;
        self.status = Status::Preparing(0);
    }
}

#[cfg(test)]
#[path = "crew_tests.rs"]
mod tests;
