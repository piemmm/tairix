//! The Venus vertical on a board declaring a virtio-iommu its PCI host
//! bypasses (`plans/GPU.md` GP11.3): no unit translates the GPU on a platform
//! that has one, so the kernel refuses the GPU's channel to its driver,
//! audited, and the driver drives the device without serving it. PASS once
//! the refusal is audited for the untranslated node and the check reports the
//! GPU unserved after the device's 3D work ran.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel;

/// No unit translates the GPU on a platform declaring one, so its channel is
/// refused.
#[cfg(itest_x86_64)]
const GPU_SERVED: bool = false;

#[cfg(not(itest_x86_64))]
fn main() {}
