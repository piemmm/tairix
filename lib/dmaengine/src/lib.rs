//! The `dmaengine-v1` controller side (`plans/SOUND.md` SND5, SND19a): what
//! every DMA controller driver serves around its
//! [`DmaEngine`](tairix_abi::driver::dmaengine::DmaEngine).
//!
//! A control block holds bus addresses and nothing between a controller and
//! RAM checks them, so the controller's driver is the only process that
//! writes one. [`Controller`] is the endpoint its consumers call: it believes
//! a request line or a FIFO only once the kernel attests the caller holds it,
//! carves every buffer itself, and answers waits by which period a channel
//! has reached. It is written over the class traits alone and is
//! host-testable; `serve` is the freestanding bring-up and loop a driver hands
//! its engine to.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod controller;

#[cfg(test)]
mod controller_tests;

pub use controller::{split_windows, Buffer, Controller, ControllerHost, Record};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::{bringup_failed, serve, Node};

use tairix_abi::driver::dmaengine::DMA_MAX_CHANNELS;

/// The channels every per-channel table holds.
const CHANNEL_SLOTS: usize = DMA_MAX_CHANNELS as usize;
