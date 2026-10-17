//! A [`Courier`] for the host tests: it carries each check the moment it is
//! handed one, as a worker does on a machine that grants no thread.

use alloc::collections::VecDeque;

use tairix_greeter::{Courier, Ticket};

use crate::accounts::SessionTransport;
use crate::verify::{exchange, Check, Checked};

/// Carries every check over `transport` at once, keeping the answers for
/// [`Courier::receive`] in the order they were given.
pub(crate) struct Inline<T> {
    pub(crate) transport: T,
    carried: VecDeque<Checked>,
}

impl<T> Inline<T> {
    pub(crate) const fn new(transport: T) -> Self {
        Self {
            transport,
            carried: VecDeque::new(),
        }
    }
}

impl<T: SessionTransport> Courier for Inline<T> {
    type Answer = Checked;

    fn send(&mut self, ticket: Ticket, account: &str, secret: &str) -> Result<(), Checked> {
        let mut check = Check::new(ticket, account, secret)?;
        let answer = exchange(&mut self.transport, &mut check);
        self.carried.push_back(answer);
        Ok(())
    }

    fn receive(&mut self) -> Option<Checked> {
        self.carried.pop_front()
    }
}
