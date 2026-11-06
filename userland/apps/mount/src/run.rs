//! The `Run` entry-point binary of the `mount` tool.
//!
//! A pure-Rust program over `tairix-rt`. `main` runs the parsed command
//! against the production seams: `tairix_procinfo::IpcTransport` for the
//! mount-table queries, `RtMounter` for the image mount and the answers for
//! held writes, the shared `tairix_help::BundleHelp` for the short help, and
//! the inherited standard streams. The kernel decides what may be mounted and
//! who may answer, and audits it; the mount authority the manifest asks for
//! serves the answers alone, since an image's target is bound under its
//! owner's rules whoever attaches.
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
    use tairix_abi::volume::{VolumeReplayRequest, VOLUME_REPLAY_LEN};
    use tairix_abi::{Errno, RealpathMode};
    use tairix_help::BundleHelp;
    use tairix_mount::{parse, run, MountError, Mounter};
    use tairix_procinfo::{Caller, IpcTransport};
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// The production [`Mounter`]: the image opened and attached through the
    /// runtime's one image-attach call, an answer for held writes encoded and
    /// carried by the `volume_replay` syscall, a path resolved by the kernel.
    /// It adds no authority; every refusal is the kernel's own [`Errno`].
    struct RtMounter;

    impl Caller for RtMounter {
        fn user(&self) -> Option<u32> {
            tairix_rt::self_origin().ok().map(|origin| origin.uid())
        }

        fn canonical(&self, path: &str) -> Result<String, Errno> {
            tairix_rt::realpath(path, RealpathMode::Existing)
        }
    }

    impl Mounter for RtMounter {
        fn attach(&self, image: &str, attach: &ImageAttach<'_>) -> Result<ImageAttachReply, Errno> {
            tairix_rt::image_attach_path(image.as_bytes(), attach)
        }

        fn replay(&self, request: VolumeReplayRequest) -> Result<(), Errno> {
            let mut frame = [0u8; VOLUME_REPLAY_LEN];
            let len = request.encode(&mut frame)?;
            Errno::syscall_result(tairix_rt::volume_replay(&frame[..len])).map(|_| ())
        }
    }

    /// Program entry point.
    ///
    /// Exit codes, as util-linux's `mount` gives them: `0` on success, `1`
    /// for a command line not understood or an answer the caller may not
    /// give, `2` for a service or terminal that failed, `32` for a mount or
    /// an answer that failed.
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
