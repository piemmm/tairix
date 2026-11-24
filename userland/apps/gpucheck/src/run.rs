//! The `Run` entry-point binary of the `gpucheck` tool.
//!
//! It lists the GPUs drivers serve, runs [`tairix_gpucheck::work`] on each
//! and on the software device, and states whether they agree. Every refusal
//! states its reason, and the step it met, on `stderr`.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
extern crate alloc;

#[cfg(all(freestanding, feature = "program"))]
mod program {
    use alloc::format;
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use tairix_abi::gpu::GpuDeviceDescriptor;
    use tairix_gpu::Device;
    use tairix_gpucheck::{compare, reference, work, Difference, USAGE};
    use tairix_help::HelpOnly;
    use tairix_procinfo::{for_each_gpu_device, IpcTransport, WalkStep};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};

    const NAME: &str = "gpucheck";

    /// The region each GPU's bytes cross through: room for the work's
    /// largest transfer and its largest module, which crosses whole.
    const REGION: usize = 1 << 20;

    fn fail(reason: &str) -> i32 {
        write_stderr_line(format!("{NAME}: {reason}"));
        1
    }

    fn say(line: &str) -> bool {
        Stdout.write_all(line.as_bytes()).is_ok() && Stdout.write_all(b"\n").is_ok()
    }

    /// Program entry point: `0` once every served GPU agreed with the
    /// software device, `1` where one differed, none was served, or the
    /// command line was not understood.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments(NAME) else {
            return tairix_help::MISUSE;
        };
        match tairix_help::parse_help_only(&arguments) {
            Ok(HelpOnly::Run) => {}
            Ok(HelpOnly::Help) => return tairix_help::print_own_short_help(NAME, Some(USAGE)),
            Err(err) => {
                tairix_help::report_misuse(NAME, &err);
                return tairix_help::MISUSE;
            }
        }
        let mut served: Vec<GpuDeviceDescriptor> = Vec::new();
        let listed = for_each_gpu_device(&IpcTransport, |device| {
            served
                .try_reserve(1)
                .map_err(|_| tairix_abi::Errno::OutOfMemory)?;
            served.push(*device);
            Ok(WalkStep::Continue)
        });
        if listed.is_err() {
            return fail("the served GPUs could not be listed");
        }
        if served.is_empty() {
            return fail("no GPU is served");
        }
        let Ok(reference) = reference(Arc::new(tairix_rt::ClockDelay)) else {
            return fail("the software device could not be made");
        };
        let expected = match work(&reference) {
            Ok(expected) => expected,
            Err(refused) => return fail(&format!("the software device refused {refused}")),
        };
        drop(reference);
        let mut agreed = true;
        for gpu in &served {
            let line = match Device::connect(gpu.endpoint, REGION) {
                Err(why) => {
                    agreed = false;
                    format!("gpu {}: could not be reached: {why}", gpu.node)
                }
                Ok(device) => match work(&device)
                    .map_err(Difference::Refused)
                    .and_then(|answered| compare(&answered, &expected))
                {
                    Ok(()) => format!("gpu {}: agrees with the software device", gpu.node),
                    Err(difference) => {
                        agreed = false;
                        format!("gpu {}: {difference}", gpu.node)
                    }
                },
            };
            if !say(&line) {
                return fail("standard output refused the answer");
            }
        }
        i32::from(!agreed)
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
