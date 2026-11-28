//! TAIRiX Bridge's session (`plans/RISCOS-EMULATOR.md` §15): what crosses
//! between the front end and a session's worker.
//!
//! - [`protocol`]: the commands the front end sends and the events the
//!   worker sends, each decoded whole against fixed bounds.
//! - [`ring`]: the screens' damage ring in the spawn share, which both sides
//!   keep in step.
//! - [`credit`]: each context's credited input.
//! - [`worker`]: the worker: the kernel and every context of the session,
//!   scheduled in turns, paced to the port's clock, speaking the protocol.
//! - [`front`]: the front end's model of the session, which believes an
//!   event only where the worker could have sent it and reports a session
//!   that ended with the contexts that were running.
//! - [`host`]: the front end's side of one worker, driven without blocking.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

/// Every request the unit tests make, metered per thread, so a test can
/// refuse a call the room it asks for.
#[cfg(test)]
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

pub mod credit;
pub mod front;
pub mod host;
pub mod protocol;
pub mod ring;
pub mod worker;
