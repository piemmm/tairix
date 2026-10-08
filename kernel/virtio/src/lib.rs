//! Architecture-neutral kernel-side virtio wiring (Stage 4.D Item 4).
//!
//! This crate holds the kernel virtio facilities that are independent
//! of any CPU architecture:
//!
//! * [`virtio_pci_walk`] — the ring-0 walk that maps a virtio-PCI
//!   device's register windows into a
//!   [`PciTransportWindows`](tairix_virtio::PciTransportWindows) and
//!   hands them to a caller-supplied transport builder.
//! * [`virtio_mmio_walk`] — the ring-0 walk that maps a `virt`-board
//!   virtio-MMIO slot's register window and hands it to a
//!   caller-supplied transport builder.
//!
//! Both walks are generic over the transport builder, so this crate
//! names only `lib/*` types and never the concrete
//! `drivers/bus/virtio` transports — keeping ring 0 off any
//! `drivers/bus/*` crate, since `kernel/*` depends on `lib/*` and never on a
//! driver. The production builders are
//! `tairix_drv_bus_virtio::{PciTransport, MmioTransport}::new`.
//!
//! # Why a separate crate
//!
//! Every Tier-1 freestanding target links the one host and the one
//! PCI and MMIO walk from here, without pulling in a foreign architecture
//! port.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

// Host tests need `std` for the `kernel/mem` host-test doubles. The
// crate itself stays `no_std` for the freestanding build
// (no hacks).
#[cfg(test)]
extern crate std;

pub mod kernel_host;
pub mod kernel_mmio;
pub mod virtio_mmio_walk;
pub mod virtio_pci_walk;

pub use kernel_host::KernelVirtioHost;
pub use kernel_mmio::KernelMmioMapper;
pub use virtio_mmio_walk::{
    first_virtio_slot, provision_virtio_mmio, VirtioMmioProvision, VirtioMmioWalkError, MAX_SLOTS,
};
pub use virtio_pci_walk::{provision_virtio_pci, VirtioPciWalkError, VirtioProvision};
