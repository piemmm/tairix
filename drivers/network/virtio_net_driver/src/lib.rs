//! The virtio-net driver process's manifest request. The process itself is
//! the `Run` binary (`src/main.rs`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::CapabilityId;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the virtqueue slab, the device interrupt, the shared
/// frame region, the reserved device-channel endpoint, publishing its `netchan`
/// node, and its readiness beacon.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::MEM_DMA,
    CapabilityId::IRQ_BIND,
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];
