//! The `Run` entry-point binary of `audioctl`.
//!
//! A pure-Rust program over `tairix-rt`. It reaches the audio service over
//! its transport for the devices and their controls, and the System
//! Information API for the streams, and binds only to its inherited
//! descriptors.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::vec::Vec;

    use tairix_abi::audio::AudioDeviceDescriptor;
    use tairix_abi::driver::audio::StreamDirection;
    use tairix_abi::Errno;
    use tairix_audio::live::RtAudio;
    use tairix_audio::stream::{devices, set_control, DeviceControl};
    use tairix_audioctl::{parse, run, Sound, OWN_WORD};
    use tairix_help::BundleHelp;
    use tairix_procinfo::IpcTransport;
    use tairix_rt::io::{write_stderr_line, Stdout};

    /// The audio service over its live transport.
    struct RtSound(RtAudio);

    impl Sound for RtSound {
        fn devices(
            &mut self,
            direction: StreamDirection,
        ) -> Result<Vec<AudioDeviceDescriptor>, Errno> {
            devices(&mut self.0, direction)
        }

        fn set(&mut self, device_id: u32, control: DeviceControl) -> Result<(), Errno> {
            set_control(&mut self.0, device_id, control)
        }
    }

    /// Program entry point.
    ///
    /// Exit codes: `0` when the command completed, `1` when it was refused,
    /// failed, or was not understood, with the reason on standard error.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments(OWN_WORD) else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse(OWN_WORD, &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        match run(
            command,
            locale,
            &mut RtSound(RtAudio::new()),
            &IpcTransport,
            &BundleHelp::new(OWN_WORD),
            &Stdout,
        ) {
            Ok(()) => 0,
            Err(err) => {
                write_stderr_line(format_args!("{OWN_WORD}: {err}"));
                1
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
