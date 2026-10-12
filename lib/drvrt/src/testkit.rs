//! The kernel, as a link supplier's host tests stand it in.
//!
//! [`FakeKernel`] answers a supplier's questions by the kernel's own rules: a
//! grant counts only where it covers the record quoted, a call is in service
//! until it is answered or retired, an ended process cannot be watched, and a
//! supplier that answers one call twice fails the test.
//! [`Records`] keeps what the supplier recorded.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId};

use crate::{Recorder, SupplierHost};

#[derive(Default)]
struct State {
    in_service: BTreeMap<u64, ProcId>,
    last_ticket: u64,
    grants: Vec<(ProcId, HwResource)>,
    ended: Vec<ProcId>,
    watched: Vec<ProcId>,
    answers: BTreeMap<u64, Vec<u8>>,
}

/// A shared handle on one fake kernel; clones see the same state.
#[derive(Clone, Default)]
pub struct FakeKernel(Rc<RefCell<State>>);

impl FakeKernel {
    /// A ticket no call is ever in service under: what a call whose caller
    /// the kernel can no longer name is served as.
    pub const OUT_OF_SERVICE: u64 = u64::MAX;

    /// A kernel with no call in service and nothing granted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant `holder` `grant`, as loading it for the node requesting it does.
    pub fn grant(&self, holder: ProcId, grant: HwResource) {
        self.0.borrow_mut().grants.push((holder, grant));
    }

    /// Withdraw `holder`'s grant of `grant`.
    pub fn revoke(&self, holder: ProcId, grant: &HwResource) {
        self.0
            .borrow_mut()
            .grants
            .retain(|(granted, held)| (*granted, held) != (holder, grant));
    }

    /// Put a call from `caller` in service, answering its ticket.
    #[must_use]
    pub fn call(&self, caller: ProcId) -> u64 {
        let mut state = self.0.borrow_mut();
        state.last_ticket += 1;
        let ticket = state.last_ticket;
        state.in_service.insert(ticket, caller);
        ticket
    }

    /// Take the call `ticket` out of service unanswered, as its caller's end
    /// does.
    pub fn retire(&self, ticket: u64) {
        self.0.borrow_mut().in_service.remove(&ticket);
    }

    /// End `peer`: watching it is refused from now on.
    pub fn end(&self, peer: ProcId) {
        let mut state = self.0.borrow_mut();
        state.ended.push(peer);
        state.watched.retain(|watched| *watched != peer);
    }

    /// The answer the call `ticket` got.
    #[must_use]
    pub fn answer(&self, ticket: u64) -> Option<Vec<u8>> {
        self.0.borrow().answers.get(&ticket).cloned()
    }

    /// How many calls have been answered.
    #[must_use]
    pub fn answered(&self) -> usize {
        self.0.borrow().answers.len()
    }

    /// Every watched peer, in the order first watched.
    #[must_use]
    pub fn watched(&self) -> Vec<ProcId> {
        self.0.borrow().watched.clone()
    }
}

impl SupplierHost for FakeKernel {
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
        self.0
            .borrow()
            .in_service
            .get(&ticket)
            .copied()
            .ok_or(Errno::NotFound)
    }

    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno> {
        let caller = self.caller(ticket)?;
        Ok(self
            .0
            .borrow()
            .grants
            .iter()
            .any(|(holder, grant)| *holder == caller && grant.covers(record)))
    }

    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
        let mut state = self.0.borrow_mut();
        assert!(
            !state.answers.contains_key(&ticket),
            "call {ticket} answered twice"
        );
        state.in_service.remove(&ticket).ok_or(Errno::NotFound)?;
        state.answers.insert(ticket, frame.to_vec());
        Ok(())
    }

    fn watch(&mut self, peer: ProcId) -> Result<(), Errno> {
        let mut state = self.0.borrow_mut();
        if state.ended.contains(&peer) {
            return Err(Errno::NotFound);
        }
        if !state.watched.contains(&peer) {
            state.watched.push(peer);
        }
        Ok(())
    }

    fn unwatch(&mut self, peer: ProcId) {
        self.0
            .borrow_mut()
            .watched
            .retain(|watched| *watched != peer);
    }
}

/// The decisions a supplier under test recorded, oldest first; clones share
/// them.
#[derive(Clone)]
pub struct Records<R>(Rc<RefCell<Vec<R>>>);

impl<R> Default for Records<R> {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(Vec::new())))
    }
}

impl<R: Clone> Records<R> {
    /// Every decision recorded, oldest first.
    #[must_use]
    pub fn all(&self) -> Vec<R> {
        self.0.borrow().clone()
    }
}

impl<R> Recorder<R> for Records<R> {
    fn record(&mut self, record: R) {
        self.0.borrow_mut().push(record);
    }
}
