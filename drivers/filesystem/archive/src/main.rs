//! The `Run` binary of the archive driver: the process `devmgr` loads for an
//! archive's volume node, serving its members to the kernel
//! (`plans/ARCHIVES.md`). On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    /// Program entry point, which `tairix-rt`'s `_start` calls and whose
    /// status it exits with: serve the archive until it leaves.
    fn main() -> i32 {
        tairix_fsserve::program::run_archive::<tairix_drv_fs_archive::Served>()
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
