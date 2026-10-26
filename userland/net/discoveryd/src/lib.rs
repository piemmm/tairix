//! TAIRiX link-local service discovery (`discoveryd`) — multicast DNS and
//! DNS-SD, and the computers WS-Discovery names, `plans/ZEROCONF.md`.
//!
//! Every multicast DNS and WS-Discovery packet, and every metadata response,
//! is unauthenticated input from a peer that chooses its own arrival rate, so
//! the service is two processes and the one that parses holds nothing:
//!
//! * [`front::Front`] holds the multicast DNS and WS-Discovery sockets, the
//!   group memberships, the connections a computer's metadata is fetched
//!   over, and every authority the service will ever exercise. It parses no
//!   DNS, SOAP or HTTP: it relays each datagram the network stack delivered —
//!   from a sender the stack found on-link, and within that sender's relay
//!   budget — and each metadata response to its decoder, passes it time, and
//!   supervises it.
//! * [`decoder::Decoder`] runs the `tairix_net::mdns` and `tairix_net::wsd`
//!   engines, one of each per interface, as a capability-empty sandbox worker
//!   over one pipe pair (`tairix_sandbox::supervise`). It cannot open a file,
//!   reach an endpoint, spawn, or read a clock; a decoder a crafted packet
//!   kills is reaped, logged, and replaced after a paced delay, and the front
//!   survives it.
//!
//! The channel between them is [`wire`]: fixed-shape frames, each side's
//! reader a bounds-checked field read.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod decoder;
pub mod events;
pub mod front;
pub mod grants;
pub mod query;
pub mod questions;
pub mod sessions;
pub mod wire;

#[cfg(test)]
mod wsd_fixtures;
