//! The machine worker (`plans/BEEB.md` §4): the machine run in a
//! capability-empty sandbox, driven by the front end over a session. The
//! owner sends a request and the worker answers it with one reply; nothing
//! is unsolicited. One share holds the picture: the worker writes the rows
//! that changed before it replies and the owner reads them before it asks
//! again, so the two never touch it at once.

mod client;
pub mod protocol;
mod service;

pub use client::{Busy, MachineClient};
pub use service::MachineService;
