//! The display-service protocol engine (`plans/DISPLAY.md` D7b,
//! `plans/FIX-DISPLAY-ACCELERATION.md`).
//!
//! One crate hosts **both halves** of the `DISPLAY_ENDPOINT` protocol over
//! injected seams, so what a request means, what is validated and what is
//! refused have exactly one definition:
//!
//! * [`server::DisplayServer`] — the engine a display driver's `Run` binary
//!   hosts: decode → caller-authority check ([`server::PeerFacts`]) →
//!   validation → the driver's
//!   [`Display`](tairix_abi::driver::display::Display), which allocates the
//!   buffers its device reads.
//! * [`client::RemoteDisplay`] — the [`Presenter`] a compositor presents
//!   through: it maps the frames the driver granted, copies only changed
//!   pixels into them, and names them by id.
//!
//! The wire format lives in `tairix_abi::display_ipc`. Behind the `service`
//! feature, the `service` module is the one serve loop every display driver's
//! `Run` binary runs.
//!
//! The crate also hosts [`framebuffer::Framebuffer`] — the generic
//! linear-surface engine — [`winframe`], the pixel/byte boundary of a
//! **window** frame, which is the scan-out path's channel-order decision and
//! so is defined beside it, and [`sleep::DisplaySleep`], how a presenter
//! switches its display off for energy and wakes it again.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(any(test, feature = "test-support"))]
extern crate alloc;

pub mod client;
pub mod events;
pub mod framebuffer;
pub mod memory;
pub mod presenter;
pub mod region;
#[cfg(feature = "rt")]
pub mod rt;
pub mod scanout;
pub mod server;
#[cfg(feature = "service")]
pub mod service;
pub mod sleep;
#[cfg(any(test, feature = "test-support"))]
pub mod testkit;
pub mod winframe;

pub use client::{BufferMapper, DisplayClient, DisplayTransport, RemoteDisplay};
pub use framebuffer::{Framebuffer, FramebufferConfig};
pub use memory::{copy_rows, frames_budget, BufferIds, BufferMemory, DeviceBuffer, SharedBuffer};
pub use presenter::{Base, CursorPlane, DrawInPlace, LayerPresenter, Presenter};
pub use region::{FrameRegion, ShmMapper};
#[cfg(feature = "rt")]
pub use rt::{
    establish_display, FramesRefused, RtBufferMapper, RtBufferMemory, RtDeviceBuffer,
    RtDeviceMemory, RtDisplay, RtDisplayTransport, RtMapping, RtSharedBuffer, RtShmMapper,
    RtShmRegion,
};
pub use scanout::{damage_list, scanout_len, sub_screen_damage, ChannelOrder};
// The runner contract and the calling-thread runner a window-frame conversion
// names, re-exported so a program that converts its own frame does not take a
// second dependency to spell one constant (one definition, as `tairix_wm`
// re-exports the pointer vocabulary it consumes).
pub use server::{
    Delivery, DisplayServer, Latch, PeerFacts, DISPLAY_REPLY_MAX, PRESENT_LATCH_DEADLINE_NS,
};
pub use sleep::{DisplaySleep, SwitchedOff};
pub use tairix_parallel::{JobRunner, SERIAL};

use tairix_abi::{DriverError, Errno};

/// Map a typed [`Errno`] a display-service reply carried back onto the
/// [`DriverError`] the `Display` trait's callers expect — the inverse
/// boundary conversion for [`client::RemoteDisplay`], defined once.
///
/// The seat refusals keep their meaning ([`Errno::SeatRevoked`] is the
/// distinct "you lost the seat" signal a compositor tears down on;
/// [`Errno::SeatNotOwner`] and every other authority refusal surface as
/// `PermissionDenied`, exactly as the kernel-side present gate reports
/// them). Conditions with no driver-level equivalent fail closed as
/// `DeviceFault`: the remote display is the device, and an
/// uninterpretable refusal from it must read as "unhealthy", never as
/// success.
#[must_use]
pub fn driver_error_from_errno(err: Errno) -> DriverError {
    match err {
        Errno::BufferTooSmall => DriverError::BufferTooSmall,
        Errno::BadMagic => DriverError::BadMagic,
        Errno::AbiVersionUnsupported => DriverError::AbiVersionUnsupported,
        Errno::LengthOutOfRange => DriverError::LengthOutOfRange,
        Errno::OutOfRange => DriverError::OutOfRange,
        Errno::PermissionDenied | Errno::SeatNotOwner => DriverError::PermissionDenied,
        Errno::NotFound => DriverError::NotFound,
        Errno::SignatureInvalid => DriverError::SignatureInvalid,
        Errno::NotImplemented => DriverError::NotImplemented,
        Errno::WouldBlock | Errno::Busy => DriverError::Busy,
        Errno::NotSupported => DriverError::Unsupported,
        Errno::OutOfMemory => DriverError::OutOfMemory,
        Errno::NoSpace => DriverError::NoSpace,
        Errno::SeatRevoked => DriverError::SeatRevoked,
        Errno::EndpointStalled => DriverError::EndpointStalled,
        Errno::Stale => DriverError::Stale,
        // `Errno::DeviceFault` maps to itself; every other condition has
        // no driver-level equivalent and fails closed as a fault too.
        _ => DriverError::DeviceFault,
    }
}

#[cfg(test)]
mod tests;
