//! `TAIRiXBridge.app`: RISC OS programs on the TAIRiX desktop
//! (`plans/RISCOS-EMULATOR.md` §15.3). The parts here are host-tested;
//! `run.rs` joins them to the live windows, workers and sessions.
//!
//! - [`bar`]: a screen window's bar and its menu.
//! - [`begin`]: what a session is started with beside its memory.
//! - [`events`]: the diagnostic records.
//! - [`memory`]: how a session's memory is divided between its guest and
//!   its worker.
//! - [`provenance`]: which session a launch runs in.
//! - [`screen`]: a context's screen as its window shows it.
//! - [`input`]: what a screen's window gives its context.
//! - [`settings`]: the application's settings and the form editing them.
//! - [`tasks`]: a session's window, the Task Manager's view of it.
//! - [`worker`]: the worker's half of a session.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod bar;
pub mod begin;
pub mod events;
pub mod input;
pub mod memory;
pub mod provenance;
pub mod screen;
pub mod settings;
pub mod tasks;
pub mod worker;
