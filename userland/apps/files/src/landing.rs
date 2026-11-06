//! A drop onto the desktop as the file manager carries it out: each item it
//! writes announced to the desktop before its first write, then what became
//! of it, and the drop's end, sent however the job carrying it ends.
//!
//! A job that ends sends the end itself, from the thread that made the items,
//! so it reaches the desktop after every step before it. One let go of before
//! it could — cancelled, closed or stopped before it ran, or refused its start
//! — owes the end as it is dropped, as does an end the desktop could not be
//! reached with, and the loop posts it without waiting. Room for each end is
//! kept as its drop begins ([`OwedEnds`]), so owing one needs no memory.
//!
//! # No I/O
//!
//! [`DropLanding`] is the stepping alone, over a [`LandingSink`] the program
//! backs with the desktop's window channel, so when each step is sent is a
//! host test rather than an argument.

use alloc::vec::Vec;

use tairix_abi::window_ipc::{LandingName, LandingOutcome, LandingStep};
use tairix_abi::Errno;
use tairix_fscopy::op::Root;

/// What became of an item, from what its transfer left under its name: an
/// item stopped before it settled the name failed.
#[must_use]
pub const fn outcome_of(root: Option<Root>) -> LandingOutcome {
    match root {
        Some(Root::Created) => LandingOutcome::Created,
        Some(Root::Replaced) => LandingOutcome::Replaced,
        Some(Root::Merged) => LandingOutcome::Merged,
        Some(Root::Skipped) => LandingOutcome::Skipped,
        None => LandingOutcome::Failed,
    }
}

/// Where a drop's steps go.
pub trait LandingSink {
    /// Send `step`, waiting for the desktop to take it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] once the desktop has ended the drop; any other
    /// refusal, or a failure to reach it.
    fn step(&mut self, step: &LandingStep) -> Result<(), Errno>;

    /// Leave the drop's end to be sent without waiting.
    fn owe_end(&mut self);
}

/// The steps of one drop onto the desktop, or of none: a paste or a link that
/// lands anywhere else steps nothing.
pub struct DropLanding<S: LandingSink> {
    /// Where its steps go, until it ends or the desktop refuses one.
    sink: Option<S>,
    /// The item announced and not yet done.
    making: Option<u32>,
}

impl<S: LandingSink> DropLanding<S> {
    /// The steps of the drop `sink` reaches.
    #[must_use]
    pub fn to(sink: S) -> Self {
        Self {
            sink: Some(sink),
            making: None,
        }
    }

    /// No drop's: every step is nothing.
    #[must_use]
    pub fn none() -> Self {
        Self {
            sink: None,
            making: None,
        }
    }

    /// Item `item`, numbered in drag order, is about to be written as `name`.
    /// A name no file can have is not announced: that item lands as any other
    /// arrival does.
    pub fn making(&mut self, item: u32, name: &str) {
        let Ok(name) = LandingName::new(name) else {
            return;
        };
        if self.send(&LandingStep::Making { item, name }) {
            self.making = Some(item);
        }
    }

    /// The item announced is done with, as `outcome` says.
    pub fn made(&mut self, outcome: LandingOutcome) {
        if let Some(item) = self.making.take() {
            self.send(&LandingStep::Made { item, outcome });
        }
    }

    /// The drop is over. An end the desktop did not take, short of having
    /// ended the drop itself, is owed, so the drop ends there however this
    /// one went.
    pub fn end(&mut self) {
        self.making = None;
        if let Some(mut sink) = self.sink.take() {
            if !matches!(
                sink.step(&LandingStep::Ended),
                Ok(()) | Err(Errno::NotFound)
            ) {
                sink.owe_end();
            }
        }
    }

    /// Send `step`, answering whether the desktop took it. Once it has ended
    /// the drop nothing more is sent; a step it did not take for any other
    /// reason leaves that item to land as any arrival does.
    fn send(&mut self, step: &LandingStep) -> bool {
        let Some(sink) = self.sink.as_mut() else {
            return false;
        };
        match sink.step(step) {
            Ok(()) => true,
            Err(Errno::NotFound) => {
                self.sink = None;
                self.making = None;
                false
            }
            Err(_) => false,
        }
    }
}

impl<S: LandingSink> Drop for DropLanding<S> {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.as_mut() {
            sink.owe_end();
        }
    }
}

/// The ends of drops onto the desktop let go of before they could be sent,
/// for the loop to post: each drop keeps room here for its end from the
/// moment it begins, so owing it never needs memory.
#[derive(Debug, Default)]
pub struct OwedEnds {
    /// Each end owed: the window its drop came from, and its report.
    owed: Vec<(u64, u32)>,
    /// How many drops under way keep room here.
    held: usize,
}

impl OwedEnds {
    /// Keep room for the end of a drop about to begin.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] where the heap has none.
    pub fn hold(&mut self) -> Result<(), Errno> {
        let held = self.held.checked_add(1).ok_or(Errno::OutOfMemory)?;
        self.owed
            .try_reserve(held)
            .map_err(|_| Errno::OutOfMemory)?;
        self.held = held;
        Ok(())
    }

    /// A drop that kept room owes its `end`, which the room takes.
    pub fn owe(&mut self, end: (u64, u32)) {
        self.held = self.held.saturating_sub(1);
        self.owed.push(end);
    }

    /// A drop that kept room ended owing nothing: its room is free.
    pub fn release(&mut self) {
        self.held = self.held.saturating_sub(1);
    }

    /// The next end owed, for the loop to post.
    pub fn take(&mut self) -> Option<(u64, u32)> {
        self.owed.pop()
    }
}

#[cfg(test)]
#[path = "landing_tests.rs"]
mod tests;
