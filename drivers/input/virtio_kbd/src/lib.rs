//! The virtio-input keyboard/pointer driver process's manifest request. The
//! process itself is the `Run` binary (`src/main.rs`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::CapabilityId;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the virtqueue slab, the device interrupt, and
/// injecting decoded input into the focus arbiter.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::MEM_DMA,
    CapabilityId::IRQ_BIND,
    CapabilityId::INPUT_INJECT,
];
