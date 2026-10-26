//! The `Run` binary of the ARXFS driver: the process `devmgr` loads
//! for a volume node of the format, serving that volume to the kernel
//! (`plans/FSSERVE.md` FSS5). On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    /// Program entry point, which `tairix-rt`'s `_start` calls and whose
    /// status it exits with: serve the volume until it leaves.
    fn main() -> i32 {
        tairix_fsserve::program::run::<tairix_drv_fs_arxfs::Served>()
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
