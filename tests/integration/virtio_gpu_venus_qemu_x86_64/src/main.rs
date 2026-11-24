//! QEMU vertical: **3D work against QEMU's Venus GPU**
//! (`plans/GPU.md` GP5.2–GP5.6).
//!
//! The production boot discovers QEMU's OpenGL virtio GPU, with Venus, blob
//! resources and a host-visible region, as a virtio-PCI function on a `q35`
//! board, and grants the region to its node; `devmgr` autoloads the Venus
//! check (`tests/integration/venus_check_program`) in the display driver's
//! place. The check brings the device up as that driver does, makes a Venus
//! context and two host blobs, maps them into the region and into itself,
//! reads back through a blob's second mapping what its first wrote, submits a
//! fenced stream, and lets it all go. It then makes the driver's Venus
//! backend a GPU as the driver does and holds it to the software device with
//! `gpucheck`'s work, its image work, render work and four-sample pass
//! included. The host renders through Mesa's software Vulkan driver,
//! so the run needs no GPU. The board has no DMA translation unit, so the
//! kernel serves the GPU's channel.
//!
//! PASS is the check's own verdict; its failure ends the run at once with the
//! step that failed in the transcript.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel;

/// A board with no translation unit has its GPU served.
#[cfg(itest_x86_64)]
const GPU_SERVED: bool = true;

#[cfg(not(itest_x86_64))]
fn main() {}
