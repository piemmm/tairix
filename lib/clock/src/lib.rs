//! The `clock-v1` controller side (`plans/SOUND.md` SND8, SND19f): what
//! every clock controller driver serves around its clocks.
//!
//! [`Controller`] is the endpoint: a request counts once the kernel attests
//! the caller holds the clock link it quotes; who holds each clock is tracked
//! per process instance, and a clock's policy — exact, its first holder's
//! rate, or floored, the highest floor its holders state — is the hardware's
//! to declare through [`ClockHardware`]. It is host-testable; the freestanding
//! `open` and `serve` are a driver's bring-up from its node's grants and the
//! loop it hands its hardware to.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod controller;

#[cfg(any(test, feature = "test-support"))]
pub mod testkit;

#[cfg(test)]
mod controller_tests;

pub use controller::{Applied, ClockHardware, Controller, Halt, Policy, Record};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::{fixed_rate, open, serve};
