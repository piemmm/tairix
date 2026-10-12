//! The `encoder-v1` driver side (`plans/SOUND.md` SND19l): what every display
//! encoder driver runs around its [`Encoder`](tairix_abi::driver::encoder::Encoder).
//!
//! An encoder is reached through the link its display engine's node holds, so
//! the server believes a request only once the kernel attests the caller holds
//! the quoted link. One engine holds the encoder at a time. The mode set's
//! phases are held to their order from the phase the hardware was found in,
//! and a holder that ends leaves the phase as it stands: only an engine can
//! order the encoder against its timing generator. [`EncoderServer`] is that
//! logic, host-testable; `serve` is the freestanding loop that binds the
//! endpoint under the node's duty and parks on it and the encoder's event
//! sources.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod server;

#[cfg(test)]
mod server_tests;

pub use server::{EncoderServer, Record};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::{serve, serve_with_audio};
