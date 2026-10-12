//! The `i2c-v1` controller side (`plans/I2C.md` I1, I2): what every I²C
//! controller driver serves around its bus.
//!
//! [`Controller`] is the endpoint: a channel is opened over a link the kernel
//! attests the caller holds and granted a buffer of its own; a transaction
//! runs only when every message addresses what its link reaches and the bus
//! can issue the repeated STARTs it needs. It is host-testable; the
//! freestanding `open` and `serve` are a driver's bring-up from its node's
//! grants and the loop it hands its bus to.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod budget;
mod controller;

#[cfg(test)]
mod controller_tests;

pub use budget::{step_budget_ns, wire_ns, STALL_ALLOWANCE_NS};
pub use controller::{BufferHost, Controller, I2cBus, Record};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::{bringup_failed, declared_speed, open, serve};
