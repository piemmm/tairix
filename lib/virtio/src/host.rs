//! Host seam for virtio drivers.
//!
//! The production hosts are `tairix_kernel_virtio::KernelVirtioHost` and the
//! user-space driver runtime's `RtDriverHost`; this crate ships the seam and,
//! for tests only, `MockHost`, the deterministic host every virtio driver's
//! unit tests run on, so one driver source serves every bus and the tests
//! alike.

// `VirtioHost` moved into `lib/abi` at Stage 4.D Item 0-tail; the
// trait is re-exported here so existing `use crate::VirtioHost`
// import sites (in this crate and in every consuming virtio driver
// crate) keep working unchanged.
pub use tairix_abi::driver::{CompletionSignal, DmaHost, VirtioHost};

#[cfg(any(test, feature = "mock"))]
mod mock;
#[cfg(any(test, feature = "mock"))]
pub(crate) use mock::MockMemory;
#[cfg(any(test, feature = "mock"))]
pub use mock::{MockHost, MockWait};
