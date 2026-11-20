//! The machine worker (`plans/BEEB.md` §4): the machine run in a
//! capability-empty sandbox, driven by the front end over a session. The
//! owner sends a request and the worker answers it with one reply; nothing
//! is unsolicited. One share holds the picture: the worker writes the rows
//! that changed before it replies and the owner reads them before it asks
//! again, so the two never touch it at once.

mod client;
mod link;
pub mod protocol;
mod service;

/// The threads a machine worker is granted: its own, and one each for the
/// SID and the drives a Commodore 64 runs ahead of itself.
pub const WORKER_THREADS: u64 = 3;

pub use client::{Busy, MachineClient};
pub use link::Link;
#[cfg(not(freestanding))]
pub(crate) use service::powered_on;
pub use service::MachineService;
