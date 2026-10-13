//! The **driver side** of the `audiochan-v1` audio device-channel contract
//! (`plans/SOUND.md` SND4).
//!
//! The mixer service (`userland/system/audiod`) owns the shared PCM regions
//! and is the channel's *client*; an audio driver process owns the device
//! (MMIO/DMA/IRQ) and is its *server*. This crate is that server, written
//! once over the [`Audio`](tairix_abi::driver::audio::Audio) trait so every
//! audio driver process — whatever silicon it drives — shares one control
//! plane instead of re-deriving it. It is separate from `lib/audio` for the
//! reason `lib/netchan` is separate from `lib/net`: a driver process must not
//! link the mixer.
//!
//! Three layers:
//!
//! * [`AudioChannelServer`] — the pure, host-testable per-request handler:
//!   per-endpoint configuration and attach state, geometry validation, and
//!   the service logic.
//! * [`Dispatcher`] — the serve loop's work without its syscalls: a call
//!   answered or a device event serviced over an injected [`ChannelIo`], so
//!   the interrupt path and the region lifecycle are host-tested too.
//! * `serve` — the freestanding process loop the driver binary hands its
//!   opened device to: claim a reserved device-channel endpoint, publish the
//!   [`AUDIOCHAN_NODE_COMPATIBLE`](tairix_abi::driver::audio_channel::AUDIOCHAN_NODE_COMPATIBLE)
//!   hardware-tree node carrying it, and park on a wait set over the call
//!   endpoint and the device's event sources — its interrupt line, or the
//!   ports a bus driver reports its progress on — for the life of the driver.
//!   `Channel` is that channel without the loop, for a driver whose own
//!   loop serves another protocol of the device beside it. Compiled only for
//!   the bare-metal targets a driver binary is built for; the host build
//!   carries the server and the dispatcher.
//!
//! # Nothing spins
//!
//! Between doorbells the driver parks on its device's event sources. A period
//! boundary wakes it, it moves that period between the shared ring and the
//! device, and it sends one notify carrying the clock pair the mixer's linear
//! fit is built from. There is no audio tick and no poll loop: the device's
//! own period events are the only timer in the stack.
//!
//! # Fail closed
//!
//! Every reply is a fully-encoded `audiochan-v1` frame carrying a typed
//! [`Errno`](tairix_abi::Errno) — a service call before attach, a region too
//! small for the agreed geometry, an endpoint index the device does not
//! present, or a device fault is never a panic and never a partially-applied
//! action. Every set-up refusal in `serve` ends the driver through
//! `tairix_drvrt::exit` rather than degrading.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

pub mod cyclic;
mod dispatch;
mod server;
pub use dispatch::{ChannelIo, Dispatcher, MappedRegion};
pub use server::{AudioChannelServer, Serviced};

#[cfg(test)]
mod mock_audio;

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::{serve, Channel};
