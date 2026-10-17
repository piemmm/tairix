//! The [`Verifier`] every embedder asks through: it hands each offer to a
//! [`Courier`] that carries it to the authority off the embedder's loop, and
//! never waits for the answer.

use crate::surface::{Ticket, Tickets, Verifier};

/// What carries an offered secret to an authority and brings the answer back:
/// a worker thread on a running system.
pub trait Courier {
    /// What comes back about one offer.
    type Answer;

    /// Take the offer `ticket` names — `secret` for `account` — to be carried,
    /// or answer it at once when it cannot be: no memory to hold it, or more
    /// than the authority's protocol carries.
    ///
    /// `secret` is the caller's to erase once this returns, so a courier that
    /// carries it later holds its own copy, erased on every path.
    ///
    /// # Errors
    ///
    /// The answer to an offer that was never carried.
    fn send(&mut self, ticket: Ticket, account: &str, secret: &str) -> Result<(), Self::Answer>;

    /// The oldest answer brought back and not yet taken.
    fn receive(&mut self) -> Option<Self::Answer>;
}

/// A [`Verifier`] asking through `C`, issuing every offer a ticket from one
/// book for its whole life.
pub struct Asking<C: Courier> {
    courier: C,
    tickets: Tickets,
    /// The answer to an offer that could not be carried, owed before anything
    /// the courier brings back. A newer one replaces it: a surface only ever
    /// waits on the last offer it made.
    unasked: Option<C::Answer>,
}

impl<C: Courier> Asking<C> {
    /// A verifier asking through `courier`.
    pub fn new(courier: C) -> Self {
        Self {
            courier,
            tickets: Tickets::default(),
            unasked: None,
        }
    }

    /// The oldest answer that has landed, taken exactly once.
    pub fn landed(&mut self) -> Option<C::Answer> {
        self.unasked.take().or_else(|| self.courier.receive())
    }

    /// The courier asked through.
    pub const fn courier(&self) -> &C {
        &self.courier
    }
}

impl<C: Courier> Verifier for Asking<C> {
    fn ask(&mut self, account: &str, secret: &str) -> Ticket {
        let ticket = self.tickets.issue();
        if let Err(answer) = self.courier.send(ticket, account, secret) {
            self.unasked = Some(answer);
        }
        ticket
    }
}
