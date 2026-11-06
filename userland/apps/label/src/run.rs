//! The `Run` entry-point binary of the `label` tool — the program a shell
//! spawns to show or change a volume's title.
//!
//! A pure-Rust program over `tairix-rt`: `main` collects the inherited
//! arguments and the `LANG` locale, and runs the parsed command against the
//! production seams — the shared `tairix_procinfo::IpcTransport` for the
//! `sysinfo-v1` `MOUNT_LIST` and `VIEW_LIST` queries, `RtRetitler`, which
//! canonicalises a path through `realpath`, encodes the
//! `VolumeRetitleRequest` and issues the `volume_retitle` syscall (the kernel
//! decides who may and audits every decision), the shared
//! `tairix_help::BundleHelp`, and the inherited standard streams. It binds
//! only to its inherited descriptors and holds no ambient authority.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::string::String;

    use tairix_abi::driver::filesystem::VolumeTitle;
    use tairix_abi::volume::{VolumeRetitleRequest, VOLUME_ID_LEN, VOLUME_RETITLE_MAX_LEN};
    use tairix_abi::{Errno, RealpathMode};
    use tairix_help::BundleHelp;
    use tairix_label::{parse, run, Retitler};
    use tairix_procinfo::{Caller, IpcTransport};
    use tairix_rt::io::{write_stderr_line, Stdout};

    /// The production [`Retitler`]: encode the retitle frame and issue the
    /// `volume_retitle` syscall. It adds no authority; a refusal surfaces as
    /// the kernel's exact [`Errno`].
    struct RtRetitler;

    impl Caller for RtRetitler {
        fn user(&self) -> Option<u32> {
            tairix_rt::self_origin().ok().map(|origin| origin.uid())
        }

        fn canonical(&self, path: &str) -> Result<String, Errno> {
            tairix_rt::realpath(path, RealpathMode::Existing)
        }
    }

    impl Retitler for RtRetitler {
        fn retitle(
            &self,
            volume_id: [u8; VOLUME_ID_LEN],
            title: &VolumeTitle,
        ) -> Result<(), Errno> {
            let request = VolumeRetitleRequest {
                volume_id,
                title: *title,
            };
            let mut frame = [0u8; VOLUME_RETITLE_MAX_LEN];
            let len = request.encode(&mut frame)?;
            let ret = tairix_rt::volume_retitle(&frame[..len]);
            if ret == 0 {
                Ok(())
            } else {
                Err(Errno::from_syscall(ret))
            }
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success (or the help or version), then as
    /// `LabelError::status` gives them: `1` for a command line not understood
    /// or a title not shown or set, `2` for a failed service or terminal.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("label") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("label", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        match run(
            command,
            locale,
            &IpcTransport,
            &RtRetitler,
            &BundleHelp::new("label"),
            &Stdout,
        ) {
            Ok(()) => 0,
            Err(err) => {
                write_stderr_line(format_args!("label: {err}"));
                err.status()
            }
        }
    }

    tairix_rt::entry!(main);
}

// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry is not compiled, so this inert `main` keeps the crate building. It
// performs no I/O.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
