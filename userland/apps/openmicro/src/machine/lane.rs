//! A device run ahead of the machine on a thread of its own.
//!
//! The machine posts what it does to the device, each input stamped with the
//! cycle it lands on, and publishes how far it has run; a runner thread brings
//! the device up behind it. Wherever the machine needs the device as it stands
//! at a cycle it brings the device up itself, waiting on the runner for one
//! piece of the device's work at most, and a lane no thread was had for is
//! brought up by the machine alone.
//!
//! A device changes only by its inputs and the cycles it is brought up to, and
//! a runner takes it no further than a catch-up to any later cycle would have,
//! so which thread brought it up, and in what pieces, never shows: a machine
//! is the same machine with its runners or without them.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::ops::{Deref, DerefMut};

#[cfg(not(feature = "loom"))]
use alloc::sync::Arc;
#[cfg(not(feature = "loom"))]
use tairix_rt::sync::{Condvar, Mutex, MutexGuard};

#[cfg(feature = "loom")]
use model::{Arc, Condvar, Mutex, MutexGuard};

/// A device a [`Lane`] brings up.
pub trait Device: Send + 'static {
    /// What the machine does to it.
    type Input: Send + 'static;

    /// Bring it up to machine cycle `to`, as everything that reads it there
    /// sees it.
    fn catch_up(&mut self, to: u64);

    /// Bring it a piece of the way toward machine cycle `limit`, no further
    /// than a catch-up to any cycle from `limit` on would take it, answering
    /// whether it moved.
    fn run_ahead(&mut self, limit: u64) -> bool;

    /// Take `input`, which lands at machine cycle `at`, once caught up to it.
    fn take(&mut self, at: u64, input: Self::Input);
}

/// What starts the threads a machine's devices run ahead on.
pub trait Spawner {
    /// `body` run on a thread of its own, answered with what waits for it to
    /// end; `None` where no thread can be had.
    fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> Option<Box<dyn Joiner>>;
}

/// A thread a [`Spawner`] started.
pub trait Joiner {
    /// Wait for it to end.
    fn join(self: Box<Self>);
}

/// Machine cycles the published time leads the device by before an idle
/// runner is woken: a thousand wakes a second at most. A runner already awake
/// keeps up with every publication.
const WAKE_LEAD: u64 = 1024;

/// Inputs held for a runner before the machine brings the device up itself.
const POSTED_MOST: usize = 4096;

/// A device, and the runner that brings it up behind the machine.
pub struct Lane<D: Device> {
    shared: Arc<Shared<D>>,
    runner: Option<Box<dyn Joiner>>,
}

struct Shared<D: Device> {
    control: Mutex<Control<D::Input>>,
    /// Signalled when an idle runner has work, or is to stop.
    work: Condvar,
    device: Mutex<D>,
}

/// What the machine and the runner agree through.
struct Control<I> {
    /// The cycle the machine has run to, as far as it has said.
    published: u64,
    /// The cycle up to which a runner can do nothing more.
    exhausted: u64,
    /// Inputs not yet taken, in the order they land.
    posted: VecDeque<(u64, I)>,
    /// The machine is waiting for the device.
    wanted: bool,
    /// The runner is parked.
    idle: bool,
    stop: bool,
}

impl<I> Control<I> {
    /// Whether the runner has work it may take up.
    fn has_work(&self) -> bool {
        !self.wanted && (self.published > self.exhausted || !self.posted.is_empty())
    }

    /// Whether an idle runner is worth waking: the machine a lead ahead of it.
    fn worth_waking(&self) -> bool {
        self.idle && !self.wanted && self.published >= self.exhausted.saturating_add(WAKE_LEAD)
    }
}

impl<D: Device> Lane<D> {
    /// `device`, brought up by the machine alone until a runner is
    /// [`spread`](Self::spread) to it.
    pub fn new(device: D) -> Self {
        Self {
            shared: Arc::new(Shared {
                control: Mutex::new(Control {
                    published: 0,
                    exhausted: 0,
                    posted: VecDeque::new(),
                    wanted: false,
                    idle: false,
                    stop: false,
                }),
                work: Condvar::new(),
                device: Mutex::new(device),
            }),
            runner: None,
        }
    }

    /// Bring the device up on a thread `spawner` starts, where it can.
    pub fn spread(&mut self, spawner: &dyn Spawner) {
        if self.runner.is_some() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.runner = spawner.spawn(Box::new(move || serve(&shared)));
    }

    /// Post `input`, which lands at machine cycle `at`, no earlier than any
    /// posted before it. Without a runner, or with too many already waiting
    /// for it, the device is brought up and takes it now, so no [`Caught`] of
    /// this lane may be held across a post.
    pub fn post(&self, at: u64, input: D::Input) {
        if self.runner.is_some() {
            let mut control = self.shared.control.lock();
            if control.posted.len() < POSTED_MOST && control.posted.try_reserve(1).is_ok() {
                control.posted.push_back((at, input));
                control.published = control.published.max(at);
                self.wake(&control);
                return;
            }
        }
        self.at(at).take(at, input);
    }

    /// Say the machine has run to cycle `now`, waking an idle runner the
    /// machine has run far enough ahead of.
    pub fn publish(&self, now: u64) {
        if self.runner.is_none() {
            return;
        }
        let mut control = self.shared.control.lock();
        control.published = control.published.max(now);
        self.wake(&control);
    }

    fn wake(&self, control: &Control<D::Input>) {
        if control.worth_waking() {
            self.shared.work.notify_one();
        }
    }

    /// The device brought up to machine cycle `now`, every input landing by
    /// then taken, held until the [`Caught`] is let go.
    #[must_use]
    pub fn at(&self, now: u64) -> Caught<'_, D> {
        let shared = &*self.shared;
        if self.runner.is_none() {
            let mut device = shared.device.lock();
            device.catch_up(now);
            return Caught { device };
        }
        {
            let mut control = shared.control.lock();
            control.published = control.published.max(now);
            control.wanted = true;
        }
        let mut device = shared.device.lock();
        loop {
            let next = shared.control.lock().posted.pop_front();
            let Some((at, input)) = next else {
                break;
            };
            device.catch_up(at);
            device.take(at, input);
        }
        device.catch_up(now);
        let mut control = shared.control.lock();
        control.wanted = false;
        control.exhausted = control.exhausted.max(now);
        drop(control);
        Caught { device }
    }

    /// Put `device` in place as it stands at machine cycle `now`, the machine's
    /// time starting again from there, as a restored snapshot's does; what
    /// was posted for the device it replaces is dropped with it.
    pub fn replace(&self, now: u64, device: D) {
        let shared = &*self.shared;
        shared.control.lock().wanted = true;
        let mut held = shared.device.lock();
        *held = device;
        let mut control = shared.control.lock();
        control.posted.clear();
        control.published = now;
        control.exhausted = now;
        control.wanted = false;
    }
}

impl<D: Device> Drop for Lane<D> {
    fn drop(&mut self) {
        let Some(runner) = self.runner.take() else {
            return;
        };
        {
            let mut control = self.shared.control.lock();
            control.stop = true;
            self.shared.work.notify_all();
        }
        runner.join();
    }
}

/// A device brought up to the cycle it was asked for, held for the machine.
pub struct Caught<'a, D> {
    device: MutexGuard<'a, D>,
}

impl<D> Deref for Caught<'_, D> {
    type Target = D;

    fn deref(&self) -> &D {
        &self.device
    }
}

impl<D> DerefMut for Caught<'_, D> {
    fn deref_mut(&mut self) -> &mut D {
        &mut self.device
    }
}

/// A runner: bring the device up behind the machine until told to stop,
/// giving way whenever the machine wants it.
fn serve<D: Device>(shared: &Shared<D>) {
    loop {
        {
            let mut control = shared.control.lock();
            loop {
                if control.stop {
                    return;
                }
                if control.has_work() {
                    break;
                }
                control.idle = true;
                control = shared.work.wait(control);
                control.idle = false;
            }
        }
        let mut device = shared.device.lock();
        loop {
            // Read with the device held: a replaced device's time starts again,
            // and no limit from before may reach it.
            let (next, limit) = {
                let mut control = shared.control.lock();
                if control.wanted || control.stop {
                    break;
                }
                (control.posted.pop_front(), control.published)
            };
            if let Some((at, input)) = next {
                device.catch_up(at);
                device.take(at, input);
            } else if !device.run_ahead(limit) {
                let mut control = shared.control.lock();
                control.exhausted = control.exhausted.max(limit);
                break;
            }
        }
    }
}

/// loom's locks in `lib/rt`'s shape, so the models drive the lane itself.
#[cfg(feature = "loom")]
mod model {
    extern crate std;

    use core::ops::{Deref, DerefMut};
    use std::sync::PoisonError;

    pub(super) use loom::sync::Arc;

    pub(super) struct Mutex<T>(loom::sync::Mutex<T>);

    pub(super) struct MutexGuard<'a, T>(loom::sync::MutexGuard<'a, T>);

    impl<T> Mutex<T> {
        pub(super) fn new(value: T) -> Self {
            Self(loom::sync::Mutex::new(value))
        }

        pub(super) fn lock(&self) -> MutexGuard<'_, T> {
            MutexGuard(self.0.lock().unwrap_or_else(PoisonError::into_inner))
        }
    }

    impl<T> Deref for MutexGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &T {
            &self.0
        }
    }

    impl<T> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.0
        }
    }

    pub(super) struct Condvar(loom::sync::Condvar);

    impl Condvar {
        pub(super) fn new() -> Self {
            Self(loom::sync::Condvar::new())
        }

        pub(super) fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
            MutexGuard(self.0.wait(guard.0).unwrap_or_else(PoisonError::into_inner))
        }

        pub(super) fn notify_one(&self) {
            self.0.notify_one();
        }

        pub(super) fn notify_all(&self) {
            self.0.notify_all();
        }
    }
}

/// Host threads, for the tests that run devices ahead.
#[cfg(test)]
pub(crate) mod host {
    extern crate std;

    use alloc::boxed::Box;
    use std::thread::JoinHandle;

    use super::{Joiner, Spawner};

    /// Starts each runner on a host thread.
    pub(crate) struct HostThreads;

    struct HostJoin(JoinHandle<()>);

    impl Joiner for HostJoin {
        fn join(self: Box<Self>) {
            assert!(self.0.join().is_ok(), "the runner ended cleanly");
        }
    }

    impl Spawner for HostThreads {
        fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> Option<Box<dyn Joiner>> {
            Some(Box::new(HostJoin(std::thread::spawn(body))))
        }
    }
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod tests;
