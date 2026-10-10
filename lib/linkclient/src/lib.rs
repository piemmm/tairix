//! The consumer halves of the supplier links (`plans/SUPPLIERS.md`): what a
//! driver whose node names another driver's service speaks to it.
//!
//! A link request is believed by its supplier only once the kernel attests
//! the caller holds it, so a client quotes the request discovery gave its
//! node and nothing of its own choosing. [`DmaClient`] streams through a DMA
//! controller's channel and learns of each period boundary from a posted wait
//! its serve loop wakes on (`plans/SOUND.md` SND5, SND8); [`ClockClient`]
//! runs the clock a block is fed by (`clock-v1`); [`CodecClient`] drives the
//! codec on the far side of a digital audio interface (`codec-v1`). Each
//! reaches its supplier through [`LinkCall`], whose production form is
//! [`RtLinkCall`], so every consumer speaks each protocol one way.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod call;
mod clock;
mod codec;
mod dma;

#[cfg(test)]
mod clock_tests;
#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod dma_tests;

pub use call::{LinkCall, RtLinkCall};
pub use clock::ClockClient;
pub use codec::CodecClient;
pub use dma::DmaClient;
