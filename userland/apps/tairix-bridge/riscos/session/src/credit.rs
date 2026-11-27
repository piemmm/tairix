//! Credited input: the front end sends a context at most [`INPUT_WINDOW`]
//! input events beyond those the worker last said it took in, so the events
//! a worker holds for a context that reads none are bounded, and a context
//! that reads none closes no other context's window.
//!
//! The front end keeps a [`Credit`] for each context, the worker an
//! [`Inbox`]; the worker takes an event in as it hands it to the kernel,
//! and says so with [`Event::Credit`](crate::protocol::Event::Credit).

use alloc::collections::VecDeque;

use tairix_riscos_kernel::input::InputEvent;

/// The most input events sent a context beyond those it has taken in.
pub const INPUT_WINDOW: u32 = 64;

/// Why a credit or an inbox would not move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CreditError {
    /// An event sent beyond the window.
    Beyond,
    /// More taken in than was sent, or fewer than said before.
    Miscounted,
}

/// The front end's credit for one context: the events it has sent, and
/// those the worker has said it took in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Credit {
    sent: u32,
    applied: u32,
}

impl Credit {
    /// A new context's: nothing sent.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sent: 0,
            applied: 0,
        }
    }

    /// The events sent and not yet taken in.
    #[must_use]
    pub const fn outstanding(&self) -> u32 {
        self.sent.wrapping_sub(self.applied)
    }

    /// Whether another event may be sent now.
    #[must_use]
    pub const fn open(&self) -> bool {
        self.outstanding() < INPUT_WINDOW
    }

    /// An event sent.
    ///
    /// # Errors
    ///
    /// [`CreditError::Beyond`] where the window is closed, nothing moved.
    pub const fn send(&mut self) -> Result<(), CreditError> {
        if !self.open() {
            return Err(CreditError::Beyond);
        }
        self.sent = self.sent.wrapping_add(1);
        Ok(())
    }

    /// The worker has taken in `applied` events in all.
    ///
    /// # Errors
    ///
    /// [`CreditError::Miscounted`] for more than was sent or fewer than it
    /// said before, nothing moved.
    pub const fn taken(&mut self, applied: u32) -> Result<(), CreditError> {
        let more = applied.wrapping_sub(self.applied);
        if more > self.outstanding() {
            return Err(CreditError::Miscounted);
        }
        self.applied = applied;
        Ok(())
    }
}

/// The worker's inbox for one context: the events sent and not yet taken
/// in, and how many it has taken in.
#[derive(Debug)]
pub struct Inbox {
    /// Room for a whole window from the start, so that nothing the front end
    /// may send needs more.
    events: VecDeque<InputEvent>,
    applied: u32,
}

impl Inbox {
    /// A new context's: empty, none where its room is not had.
    #[must_use]
    pub fn new() -> Option<Self> {
        let mut events = VecDeque::new();
        events.try_reserve_exact(INPUT_WINDOW as usize).ok()?;
        Some(Self { events, applied: 0 })
    }

    /// `event` received.
    ///
    /// # Errors
    ///
    /// [`CreditError::Beyond`] for an event beyond the window, which the
    /// front end never sends.
    pub fn receive(&mut self, event: InputEvent) -> Result<(), CreditError> {
        if self.events.len() >= INPUT_WINDOW as usize {
            return Err(CreditError::Beyond);
        }
        self.events.push_back(event);
        Ok(())
    }

    /// The next event to hand the kernel, taken in.
    pub fn take(&mut self) -> Option<InputEvent> {
        let event = self.events.pop_front()?;
        self.applied = self.applied.wrapping_add(1);
        Some(event)
    }

    /// The events taken in, counted from the context's making.
    #[must_use]
    pub const fn applied(&self) -> u32 {
        self.applied
    }

    /// Whether an event waits.
    #[must_use]
    pub fn waiting(&self) -> bool {
        !self.events.is_empty()
    }
}

#[cfg(test)]
mod tests;
