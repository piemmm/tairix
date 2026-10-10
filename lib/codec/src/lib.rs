//! The `codec-v1` driver side (`plans/SOUND.md` SND8f): what every audio
//! codec driver runs around its [`Codec`](tairix_abi::driver::codec::Codec).
//!
//! A codec is reached through the link its digital audio interface's node
//! holds, so the server believes a request only once the kernel attests the
//! caller holds the quoted link, and reads the framing and clock sides from
//! that attested link rather than from the frame. One interface's driver
//! holds the codec at a time: the one that configured it, until it ends, when
//! the codec is stopped. [`CodecServer`] is that logic, host-testable;
//! `serve` is the freestanding loop that binds the endpoint under the node's
//! duty and parks on it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod server;

#[cfg(test)]
mod server_tests;

pub use server::{CodecServer, Record, Recorder};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::serve;
