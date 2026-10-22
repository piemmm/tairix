//! The `Raytrace.app` bundle's `Run` entry point: the scene, served over the
//! saver protocol in the drawing sandbox.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    fn main() -> i32 {
        tairix_saver::run(tairix_saver_raytrace::Raytrace::new)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
