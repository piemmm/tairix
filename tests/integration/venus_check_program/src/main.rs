//! The Venus check's `Run` program: bound to the virtio GPU in place of the
//! display driver, it brings the device up as that driver does, drives the
//! engine's 3D work against the device, then makes the driver's Venus backend
//! a GPU as the driver does and holds it to the software device with
//! `gpucheck`'s work, records the verdict, and serves the seat's display.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
extern crate alloc;

#[cfg(freestanding)]
mod program {
    use alloc::format;
    use alloc::sync::Arc;

    use tairix_abi::driver::{DeviceWake, MemoryWindow, RegionCaching};
    use tairix_abi::DriverError;
    use tairix_abi::WindowError;
    use tairix_display::service::serve;
    use tairix_drv_display_gpu_virtio::program::{
        host_visible_region, run, start_gpu, Drive, Engine, Host,
    };
    use tairix_drv_display_gpu_virtio::wire::BlobSpec;
    use tairix_drv_display_gpu_virtio::{Done, Stream};
    use tairix_drvrt::serve::log_event;
    use tairix_gpu::Device;
    use tairix_gpucheck::{compare, reference, work};
    use tairix_inline::ArrayVec;
    use tairix_log::{Field, FieldValue, Level};
    use tairix_test_venus_check::{FAILED, PASSED};
    use tairix_virtio::{SharedMemory, Transport};

    /// How long one command may take: past a context's making, which starts
    /// a process of the host's.
    const BUDGET_NS: u64 = 30_000_000_000;

    /// Bytes of each blob: two pages.
    const BLOB_BYTES: u64 = 8192;

    /// Where the check failed, and why.
    struct Failed {
        step: &'static str,
        why: DriverError,
    }

    /// A result, its failure named for the step that met it.
    trait At<T> {
        fn at(self, step: &'static str) -> Result<T, Failed>;
    }

    impl<T, E: Into<DriverError>> At<T> for Result<T, E> {
        fn at(self, step: &'static str) -> Result<T, Failed> {
            self.map_err(|why| Failed {
                step,
                why: why.into(),
            })
        }
    }

    /// What the device did of the command tagged `tag`.
    fn answered<T: Transport>(gpu: &mut Engine<'_, T>, tag: u64) -> Result<Done, DriverError> {
        gpu.wait_for(tag, BUDGET_NS)?;
        let completion = gpu.completion().ok_or(DriverError::DeviceOffline)?;
        if completion.tag != tag {
            return Err(DriverError::DeviceFault);
        }
        completion.outcome
    }

    /// The caching the device mapped the blob of the command tagged `tag`
    /// with.
    fn mapped<T: Transport>(
        gpu: &mut Engine<'_, T>,
        tag: u64,
    ) -> Result<RegionCaching, DriverError> {
        match answered(gpu, tag)? {
            Done::Mapped(caching) => Ok(caching),
            Done::Done => Err(DriverError::DeviceFault),
        }
    }

    /// Byte `at` of the pattern seeded `seed`.
    fn pattern(seed: u8, at: usize) -> u8 {
        u8::try_from(at % 251).unwrap_or(0).wrapping_mul(31) ^ seed
    }

    /// Fill `window` with the pattern seeded `seed`.
    fn fill(window: &MemoryWindow, seed: u8) -> Result<(), DriverError> {
        let mut chunk = [0u8; 256];
        for start in (0..window.len()).step_by(chunk.len()) {
            let len = chunk.len().min(window.len() - start);
            for (at, byte) in chunk[..len].iter_mut().enumerate() {
                *byte = pattern(seed, start + at);
            }
            window
                .write(start, &chunk[..len])
                .map_err(WindowError::as_driver_error)?;
        }
        Ok(())
    }

    /// Whether `window` holds the pattern seeded `seed`.
    fn holds(window: &MemoryWindow, seed: u8) -> Result<bool, DriverError> {
        let mut chunk = [0u8; 256];
        for start in (0..window.len()).step_by(chunk.len()) {
            let len = chunk.len().min(window.len() - start);
            window
                .read(start, &mut chunk[..len])
                .map_err(WindowError::as_driver_error)?;
            if chunk[..len]
                .iter()
                .enumerate()
                .any(|(at, &byte)| byte != pattern(seed, start + at))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Make a Venus context and two host blobs, map both into the
    /// host-visible region, write each through the driver's mapping, move the
    /// first to another part and read back through its new mapping what the
    /// old one wrote, submit a fenced stream, and let it all go, answering the
    /// caching the device mapped the first blob with.
    fn check<T: Transport>(gpu: &mut Engine<'_, T>, host: &Host) -> Result<RegionCaching, Failed> {
        gpu.venus()
            .ok_or(DriverError::Unsupported)
            .at("the device takes Venus work")?;
        let region = host_visible_region(host)
            .ok_or(DriverError::NotFound)
            .at("the node holds a host-visible region")?;
        let mut shared = SharedMemory::new(region).at("hold the host-visible region")?;
        let spec = BlobSpec {
            blob_id: 0,
            size: BLOB_BYTES,
            mappable: true,
            shareable: false,
        };
        let context = gpu.create_context(b"venus-check", 1).at("make a context")?;
        answered(gpu, 1).at("make a context")?;
        let first = gpu.create_blob(context, spec, 2).at("make a blob")?;
        answered(gpu, 2).at("make a blob")?;
        let second = gpu.create_blob(context, spec, 3).at("make a second blob")?;
        answered(gpu, 3).at("make a second blob")?;
        let mut here = shared.reserve(BLOB_BYTES).at("place the blob")?;
        gpu.map_blob(first, here.offset(), 4).at("map the blob")?;
        let caching = mapped(gpu, 4).at("map the blob")?;
        let mut beside = shared.reserve(BLOB_BYTES).at("place the second blob")?;
        gpu.map_blob(second, beside.offset(), 5)
            .at("map the second blob")?;
        let beside_caching = mapped(gpu, 5).at("map the second blob")?;
        let window = shared
            .map(&mut here, host, caching)
            .at("map the blob's part into the driver")?;
        fill(window, 0x11).at("write the blob")?;
        let window = shared
            .map(&mut beside, host, beside_caching)
            .at("map the second blob's part into the driver")?;
        fill(window, 0x77).at("write the second blob")?;
        let window = here
            .window()
            .ok_or(DriverError::NotFound)
            .at("the blob's part is mapped")?;
        if !holds(window, 0x11).at("read the blob")? {
            return Err(DriverError::DeviceFault).at("each blob is memory of its own");
        }
        // The blob moved: what its first mapping wrote, its second reads.
        shared.unmap(&mut here, host).at("unmap the blob's part")?;
        gpu.unmap_blob(first, 6)
            .at("take the blob out of the region")?;
        answered(gpu, 6).at("take the blob out of the region")?;
        let mut there = shared.reserve(BLOB_BYTES).at("place the blob again")?;
        shared
            .release(here)
            .map_err(|_| DriverError::Busy)
            .at("give the blob's first part back")?;
        gpu.map_blob(first, there.offset(), 7)
            .at("map the blob again")?;
        let there_caching = mapped(gpu, 7).at("map the blob again")?;
        let window = shared
            .map(&mut there, host, there_caching)
            .at("map the blob's new part into the driver")?;
        if !holds(window, 0x11).at("read the blob again")? {
            return Err(DriverError::DeviceFault)
                .at("the blob's second mapping reads what its first wrote");
        }
        gpu.submit_3d(context, 0, Stream::EMPTY, 8)
            .at("submit a fenced stream")?;
        answered(gpu, 8).at("submit a fenced stream")?;
        for (mut part, blob, tag) in [(there, first, 9), (beside, second, 10)] {
            shared.unmap(&mut part, host).at("unmap a part")?;
            gpu.unmap_blob(blob, tag)
                .at("take a blob out of the region")?;
            answered(gpu, tag).at("take a blob out of the region")?;
            shared
                .release(part)
                .map_err(|_| DriverError::Busy)
                .at("give a part back")?;
        }
        for (blob, tag) in [(first, 11), (second, 12)] {
            gpu.destroy_blob(blob, tag).at("destroy a blob")?;
            answered(gpu, tag).at("destroy a blob")?;
        }
        gpu.destroy_context(context, 13).at("destroy the context")?;
        answered(gpu, 13).at("destroy the context")?;
        Ok(caching)
    }

    /// Record the check's failure at `failed`'s step.
    fn record(failed: &Failed) {
        log_event(
            Level::Error,
            FAILED,
            "venus check failed",
            &[
                Field {
                    key: "step",
                    value: FieldValue::Str(failed.step),
                },
                Field {
                    key: "reason",
                    value: FieldValue::Error(failed.why.as_errno()),
                },
            ],
        );
    }

    /// Hold the driver's Venus backend, made a GPU as the driver makes one,
    /// to the software device, and record the verdict; `caching` is how the
    /// device mapped the first blob.
    fn held_to_reference(made: Result<Device, DriverError>, caching: RegionCaching) {
        let device = match made {
            Ok(device) => device,
            Err(why) => {
                return record(&Failed {
                    step: "make the backend a GPU",
                    why,
                })
            }
        };
        let expected =
            reference(Arc::new(tairix_rt::ClockDelay)).and_then(|reference| work(&reference));
        let agreed = match &expected {
            Ok(expected) => {
                compare(&device, expected).map_err(|difference| format!("{difference}"))
            }
            Err(why) => Err(format!("the software device refused the work: {why}")),
        };
        match agreed {
            Ok(()) => log_event(
                Level::Info,
                PASSED,
                "venus check: contexts, blobs and their mappings made and let go, and the backend agreed with the software device",
                &[Field {
                    key: "caching",
                    value: FieldValue::UnsignedInt(u64::from(caching as u32)),
                }],
            ),
            Err(why) => log_event(
                Level::Error,
                FAILED,
                "venus check failed",
                &[
                    Field {
                        key: "step",
                        value: FieldValue::Str("the backend agrees with the software device"),
                    },
                    Field {
                        key: "difference",
                        value: FieldValue::Str(&why),
                    },
                ],
            ),
        }
    }

    /// The check, then the seat's display served for the life of the process.
    struct VenusCheck;

    impl Drive for VenusCheck {
        fn drive<'h, T: Transport>(
            self,
            mut gpu: Engine<'h, T>,
            host: &'h Host,
            wakes: &[DeviceWake],
        ) -> i32 {
            // Room for the node's wakes and the desk's.
            let mut all = ArrayVec::<DeviceWake, 4>::new();
            let _ = all.try_extend_from_slice(wakes);
            match check(&mut gpu, host) {
                Ok(caching) => {
                    let then = move |made, _| held_to_reference(made, caching);
                    match start_gpu(&mut gpu, host, then) {
                        Some(port) => {
                            let _ = all.try_push(DeviceWake::Port(port));
                        }
                        None => record(&Failed {
                            step: "start the backend",
                            why: DriverError::Unsupported,
                        }),
                    }
                }
                Err(failed) => record(&failed),
            }
            serve(&mut gpu, &all)
        }
    }

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        run(VenusCheck)
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(freestanding))]
fn main() {}
