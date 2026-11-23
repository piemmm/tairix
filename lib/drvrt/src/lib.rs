//! TAIRiX user-space driver runtime host (`lib/drvrt`).
//!
//! A first-party driver that runs in **user space** (the microkernel goal)
//! gets its `register(host: &dyn DriverHost)` entry the same
//! [`DriverHost`](tairix_abi::DriverHost) surface an in-kernel driver gets —
//! but the concrete host can no longer reach the kernel's frame allocator or
//! identity map directly. Instead it maps a **granted** device resource over
//! the `abi-v1` syscall surface (`plans/PI.md` P10 chunk 5d-0): the kernel
//! mints the driver one unforgeable handle per [`HwResource`](tairix_abi::hwtree::HwResource) its matched
//! hardware-tree node requested (and *no more*), and
//! the driver maps a register window with the `mmio_map` syscall and carves a
//! coherent DMA buffer with the `dma_alloc` syscall, passing those handles.
//!
//! [`RtDriverHost`] is that host. It implements
//! [`DriverHost`](tairix_abi::DriverHost),
//! [`MmioMapper`](tairix_abi::MmioMapper),
//! and [`VirtioHost`](tairix_abi::driver::virtio::VirtioHost) over a small
//! table of kernel-issued grants, resolving a driver's requested
//! `(phys_base, len)` window to the grant that covers it and mapping it once.
//!
//! # Not a privileged path
//!
//! The host adds **no** authority. It only translates a driver's request into
//! the grant handle the kernel already minted and issues the syscall; every
//! capability check and bounds validation happens kernel-side, on the far side
//! of the trap. A forged or another task's handle resolves
//! to nothing kernel-side and the syscall is refused. The host re-checks the
//! load-time capability bitmap up front purely so a missing grant fails fast
//! without a round trip (the kernel re-checks regardless).
//!
//! # Fail-closed
//!
//! The crate is `no_std`, and the grant table is a fixed-capacity array sized
//! by [`MAX_GRANTS`]. The one allocation is the record a DMA carve's free goes
//! through, shared by every slab the host carves, so a slab reaches nothing of
//! the host however the host is moved, borrowed or dropped. Every error path
//! denies — a missing capability, an unmappable request, a window no grant
//! covers, a record the heap refuses, or a kernel refusal returns an error,
//! never a fabricated pointer or a panic.
//!
//! # Testing seam
//!
//! The syscalls the host issues live behind the [`GrantSyscalls`] trait, and a
//! carve's free behind [`DmaRelease`], so the resolution and translation logic
//! is exercised on the host without a kernel. Production driver processes use
//! [`RtGrantSyscalls`] and [`RtDmaRelease`], which forward to `tairix_rt`'s
//! wrappers — the one syscall trap.
//!
//! A link supplier's own tests answer it through `testkit`'s fake kernel,
//! exposed by the `test-support` feature.

#![cfg_attr(not(any(test, feature = "test-support")), no_std)]
// `Arc::try_new`, so a carve's release record the heap refuses fails the
// carve rather than aborting.
#![feature(allocator_api)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod exit;
mod host;
pub mod irqline;
mod supplier;
mod syscalls;

pub use host::{GrantedResource, RtDriverHost, MAX_GRANTS};
pub use supplier::{refusal, Recorder, RtSupplier, Supplier, SupplierHost};
pub use syscalls::{DmaRelease, GrantSyscalls, RtDmaRelease, RtGrantSyscalls};

#[cfg(target_os = "none")]
pub mod serve;
#[cfg(any(target_os = "none", test))]
mod tree;

#[cfg(any(test, feature = "test-support"))]
pub mod testkit;

#[cfg(test)]
mod irqline_tests;
#[cfg(test)]
mod testkit_tests;
#[cfg(test)]
mod tests;
