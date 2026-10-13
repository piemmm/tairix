//! The **driver side** of the `netchan-v1` NIC device-channel contract
//! (`plans/NETWORK.md` §2.3, N4c/N4d).
//!
//! The network stack (`userland/net/netstack`) owns the shared frame-ring
//! region and is the channel's *client*; a link-layer driver process owns
//! the device (MMIO/DMA/IRQ) and is its *server*. This crate is that server,
//! written once over the [`Net`](tairix_abi::driver::net::Net) trait so every
//! NIC driver process — whatever silicon it drives — shares one control
//! plane instead of re-deriving it.
//!
//! Two layers:
//!
//! * [`NetChannelServer`] — the pure, host-testable per-request handler:
//!   attach state, geometry validation, and the ring/service logic. No I/O,
//!   so the whole control plane is exercised on the host against a mock
//!   device. [`Drain`] is its interrupt-path counterpart: the whole
//!   mask/unmask/stop policy as a state machine over
//!   [`ServiceReport`](tairix_abi::driver::net_ring::ServiceReport)s.
//! * `serve` — the freestanding process loop the driver binary hands its
//!   opened device to: claim a reserved device-channel endpoint, publish the
//!   [`NETCHAN_NODE_COMPATIBLE`](tairix_abi::driver::net_channel::NETCHAN_NODE_COMPATIBLE)
//!   hardware-tree node carrying it, and park on
//!   a wait set over {call endpoint, device interrupt} for the life of the
//!   driver. Compiled only for the bare-metal targets a driver binary is
//!   built for; the host build carries just the pure handler.
//!
//! # Fail closed
//!
//! Every reply is a fully-encoded `netchan-v1` frame carrying a typed
//! [`Errno`](tairix_abi::Errno) — a service call before attach, a region too
//! small for the agreed geometry, or a device fault is never a panic and
//! never a partially-applied action. Every set-up refusal in `serve` ends
//! the driver through `tairix_drvrt::exit` rather than degrading.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

mod server;
pub use server::{Drain, DrainAction, DrainStep, Drained, Masked, NetChannelServer};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::serve;
