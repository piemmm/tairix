//! The `Run` program of the virtio-gpu display driver: autoloaded by
//! `devmgr` against a virtio GPU on either transport, it brings the device
//! up, hosts the kernel console where its node does, and serves the seat's
//! display for the life of the process: its presenter's frames carved under
//! the node's DMA grant, every command answered on the device's interrupt.
//! Where the device takes Venus work it serves the GPU too, on its node's
//! channel.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::DeviceWake;
    use tairix_display::service::serve;
    use tairix_drv_display_gpu_virtio::program::{run, serve_gpu, Drive, Engine, Host};
    use tairix_inline::ArrayVec;
    use tairix_virtio::Transport;

    /// The seat's display, served for the life of the process, and the GPU
    /// beside it where the device takes Venus work.
    struct Serve;

    impl Drive for Serve {
        fn drive<'h, T: Transport>(
            self,
            mut gpu: Engine<'h, T>,
            host: &'h Host,
            wakes: &[DeviceWake],
        ) -> i32 {
            let mut all = ArrayVec::<DeviceWake, 4>::new();
            let _ = all.try_extend_from_slice(wakes);
            if let Ok(port) = serve_gpu(&mut gpu, host) {
                let _ = all.try_push(DeviceWake::Port(port));
            }
            serve(&mut gpu, &all)
        }
    }

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        run(Serve)
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(freestanding))]
fn main() {}
