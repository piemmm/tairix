//! The Venus vertical behind an Intel VT-d unit translating the GPU
//! (`plans/GPU.md` GP11.3): every DMA the device makes crosses its node's
//! domain and every message it raises is remapped, and the kernel serves its
//! channel, so the check passes as it does on a board with no unit.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel;

/// A unit translates the GPU, so it is served.
#[cfg(itest_x86_64)]
const GPU_SERVED: bool = true;

#[cfg(not(itest_x86_64))]
fn main() {}
