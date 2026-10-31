//! The `Run` entry-point binary of the `mount` tool.
//!
//! A pure-Rust program over `tairix-rt`. `main` runs the parsed command
//! against the production seams: `tairix_procinfo::IpcTransport` for the
//! mount-table queries, `RtMounter` for the image mount, the shared
//! `tairix_help::BundleHelp` for the short help, and the inherited standard
//! streams. It holds no authority of its own: the kernel decides what may be
//! mounted, and audits it.
//!
//! On the host it is an inert stub, so the host tooling still covers the
//! file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::string::String;

    use tairix_abi::imageattach::{ImageAttach, ImageAttachReply};
    use tairix_abi::{Errno, RealpathMode};
    use tairix_help::BundleHelp;
    use tairix_mount::{parse, run, MountError, Mounter};
    use tairix_procinfo::IpcTransport;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// The production [`Mounter`]: the image opened and attached through the
    /// runtime's one image-attach call, a directory resolved by the kernel.
    struct RtMounter;

    impl Mounter for RtMounter {
        fn attach(&self, image: &str, attach: &ImageAttach<'_>) -> Result<ImageAttachReply, Errno> {
            tairix_rt::image_attach_path(image.as_bytes(), attach)
        }

        fn absolute(&self, path: &str) -> Result<String, Errno> {
            tairix_rt::realpath(path, RealpathMode::Existing)
        }
    }

    /// Program entry point.
    ///
    /// Exit codes, as util-linux's `mount` gives them: `0` on success, `1`
    /// for a command line not understood, `2` for a service or terminal
    /// that failed, `32` for a mount that failed.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("mount") else {
            return tairix_help::MISUSE;
        };
        let failed = |err: MountError| {
            if err.misused() {
                tairix_help::report_misuse("mount", &err);
            } else {
                write_stderr_line(format_args!("mount: {err}"));
            }
            err.status()
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => return failed(err),
        };
        match run(
            command,
            tairix_help::user_locale(),
            &RtMounter,
            &IpcTransport,
            &BundleHelp::new("mount"),
            &Stdout,
            &Stderr,
        ) {
            Ok(()) => 0,
            Err(err) => failed(err),
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
