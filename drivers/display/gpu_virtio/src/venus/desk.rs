//! The desk between the GPU's threads and the display loop, which alone keeps
//! the device: the GPU's threads queue orders, the display loop carries them
//! out on its own turn and hands each answer back with what its sender left
//! with the order, so no GPU request holds the display path and no fence
//! holds a display request.
//!
//! An order is queued with room already reserved for its answer, so the
//! display loop never refuses one for memory. The loop is roused once per
//! burst of orders: a rouse the kernel could not deliver is owed, and given
//! again by the thread that waits for answers.

use alloc::collections::VecDeque;

use tairix_abi::ipc::{SendRefusal, IPC_MEMORY_RETRY_NS};
use tairix_abi::{DriverError, Duration64};
use tairix_inline::ArrayVec;
use tairix_rt::sync::{Condvar, Mutex};
use tairix_virtio::Part;

use crate::three_d::{Blob, Context};
use crate::wire::BlobSpec;

/// The longest command stream an order carries in itself: the few commands
/// that point the renderer at the context's shared streams.
pub(crate) const PAYLOAD_MAX: usize = 128;

/// The commands of a [`Order::Submit`].
pub(crate) type Payload = ArrayVec<u8, PAYLOAD_MAX>;

/// What the GPU's threads ask of the device.
pub(crate) enum Order {
    MakeContext,
    EndContext(Context),
    MakeBlob(Context, BlobSpec),
    EndBlob(Blob),
    /// Map the blob at the part's place in the host-visible region, then the
    /// part into the driver.
    MapBlob(Blob, Part),
    /// Unmap the part from the driver, then the blob from the region.
    UnmapBlob(Blob, Part),
    /// Run `payload` in the context, fenced on its ring `ring`.
    Submit {
        context: Context,
        ring: u8,
        payload: Payload,
    },
}

/// What the device did of an order.
pub(crate) enum Answer {
    Done,
    Context(Context),
    Blob(Blob),
    /// The part, its blob mapped there by the device; its window where the
    /// driver could map it too.
    Mapped(Part),
    /// The part, out of the driver and the region, free to be released.
    Unmapped(Part),
    /// Refused, with any part the order lent.
    Refused(DriverError, Option<Part>),
}

/// What rouses the display loop.
pub(crate) trait Rouse: Send + Sync {
    /// Rouse it, answering whether a rouse is now on its way.
    fn rouse(&self) -> bool;
}

/// Rouses the display loop with a message on the port it parks on.
pub(crate) struct PortRouse(pub(crate) u64);

impl Rouse for PortRouse {
    fn rouse(&self) -> bool {
        match tairix_rt::ipc_send(self.0, &[1]) {
            Ok(()) => true,
            // A full lane already holds a rouse on its way.
            Err(errno) => SendRefusal::of(errno) == SendRefusal::Full,
        }
    }
}

struct State<T> {
    orders: VecDeque<(u64, Order, T)>,
    /// Orders the display loop has taken and not answered, by tag.
    in_service: VecDeque<(u64, T)>,
    answers: VecDeque<(T, Answer)>,
    next_tag: u64,
    /// A rouse is on its way and not yet spent.
    roused: bool,
    closed: bool,
}

/// The desk, each order carrying back a `T` its sender left with it.
pub(crate) struct Desk<T> {
    state: Mutex<State<T>>,
    answered: Condvar,
    rouse: alloc::boxed::Box<dyn Rouse>,
}

impl<T: Send> Desk<T> {
    pub(crate) fn new(rouse: alloc::boxed::Box<dyn Rouse>) -> Self {
        Self {
            state: Mutex::new(State {
                orders: VecDeque::new(),
                in_service: VecDeque::new(),
                answers: VecDeque::new(),
                next_tag: 1,
                roused: false,
                closed: false,
            }),
            answered: Condvar::new(),
            rouse,
        }
    }

    /// Queue `order`, its answer handed back with `then`.
    ///
    /// # Errors
    ///
    /// The order and `then` again, where there is no room to queue it or to
    /// answer it.
    #[allow(
        clippy::result_large_err,
        reason = "a refused order goes back whole, so its sender keeps the part it lent"
    )]
    pub(crate) fn order(&self, order: Order, then: T) -> Result<(), (Order, T)> {
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let owed = state.orders.len() + state.in_service.len() + state.answers.len() + 1;
        let (serving, answered) = (state.in_service.len(), state.answers.len());
        let room = state.orders.try_reserve(1).is_ok()
            && state.in_service.try_reserve(owed - serving).is_ok()
            && state.answers.try_reserve(owed - answered).is_ok();
        if !room {
            return Err((order, then));
        }
        let tag = state.next_tag;
        state.next_tag += 1;
        state.orders.push_back((tag, order, then));
        let rouse = !core::mem::replace(&mut state.roused, true);
        drop(guard);
        if rouse && !self.rouse.rouse() {
            // Owed: the answering thread gives it again.
            self.state.lock().roused = false;
            self.answered.notify_all();
        }
        Ok(())
    }

    /// The display loop's turn begins: a rouse arriving from here on is for
    /// orders this turn may not see.
    pub(crate) fn begin_turn(&self) {
        self.state.lock().roused = false;
    }

    /// The next order, taken into service under its tag.
    pub(crate) fn take(&self) -> Option<(u64, Order)> {
        let mut state = self.state.lock();
        let (tag, order, then) = state.orders.pop_front()?;
        // Room was reserved as it was queued.
        state.in_service.push_back((tag, then));
        Some((tag, order))
    }

    /// Put back an order the device has no room for yet, first in line.
    pub(crate) fn put_back(&self, tag: u64, order: Order) {
        let mut state = self.state.lock();
        if let Some(at) = state.in_service.iter().position(|(held, _)| *held == tag) {
            if let Some((tag, then)) = state.in_service.remove(at) {
                // The slot it held in the queue is free again.
                state.orders.push_front((tag, order, then));
            }
        }
    }

    /// Answer the order `tag`.
    pub(crate) fn answer(&self, tag: u64, answer: Answer) {
        let mut state = self.state.lock();
        let Some(at) = state.in_service.iter().position(|(held, _)| *held == tag) else {
            return;
        };
        if let Some((_, then)) = state.in_service.remove(at) {
            // Room was reserved as it was queued.
            state.answers.push_back((then, answer));
        }
        drop(state);
        self.answered.notify_all();
    }

    /// Wait for the next answer and what its sender left with it, giving any
    /// owed rouse again meanwhile; `None` once the desk is closed and every
    /// order answered.
    pub(crate) fn next_answer(&self) -> Option<(T, Answer)> {
        let mut state = self.state.lock();
        loop {
            if let Some(answered) = state.answers.pop_front() {
                return Some(answered);
            }
            if state.closed && state.orders.is_empty() && state.in_service.is_empty() {
                return None;
            }
            if !state.roused && !state.orders.is_empty() {
                state.roused = true;
                drop(state);
                let roused = self.rouse.rouse();
                state = self.state.lock();
                if !roused {
                    state.roused = false;
                    let retry = Duration64::from_nanos(IPC_MEMORY_RETRY_NS);
                    state = self.answered.wait_timeout(state, retry).0;
                }
                continue;
            }
            state = self.answered.wait(state);
        }
    }

    /// Take no more orders; the answering thread ends once every order is
    /// answered.
    #[cfg(test)]
    pub(crate) fn close(&self) {
        self.state.lock().closed = true;
        self.answered.notify_all();
    }

    /// Whether an order waits for the display loop.
    #[cfg(test)]
    pub(crate) fn has_orders(&self) -> bool {
        !self.state.lock().orders.is_empty()
    }
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;
