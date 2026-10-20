//! integration test: the compositor's per-pixel kernels on `aarch64`.
//!
//! The host suite runs the portable blend, blur and scan and the SSE2
//! candidates, and holds both to `tairix_raster::accel::CONFORMANCE_DIGEST`; it cannot
//! execute NEON. Here the families resolve against a set naming Advanced SIMD,
//! which every core this target runs on has, and each must choose its
//! candidate by priority — so the NEON candidates passed their own
//! self-verify — and then fold the fixed workload to the same constant.
//!
//! Any other outcome is a closed failure reported to QEMU.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{enable_fp_el1, handle_panic_via_serial, qemu_exit, SERIAL_SINK};
    use tairix_cpuops::{CpuFeature, CpuFeatureSet, DecisionReason};
    use tairix_itest_finisher::fail_point;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
    use tairix_raster::accel::{conformance_digest, resolve, CONFORMANCE_DIGEST};

    const TEST_START: EventId = EventId(4550);
    const TEST_PASS: EventId = EventId(4551);
    const TEST_FAIL: EventId = EventId(4552);

    /// Failure finisher code: a family fell back to its portable baseline.
    const FAIL_SELECTED: NonZeroU16 = fail_point!(1);
    /// Failure finisher code: the digest differed from the reference.
    const FAIL_DIGEST: NonZeroU16 = fail_point!(2);

    /// The global allocator's arena, in the NOLOAD `.heap` section so the boot
    /// trampoline does not spend time zeroing it.
    #[link_section = ".heap"]
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    fn note(id: EventId, level: Level, message: &'static str) {
        log(
            &SERIAL_SINK,
            &Event {
                level,
                id,
                message,
                fields: &[],
            },
        );
    }

    #[panic_handler]
    fn raster_kernels_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Entry point for the freestanding kernel.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        // The candidates are Advanced SIMD, which EL1 traps until this.
        // SAFETY: boot CPU, called once, before any FP/SIMD instruction.
        unsafe { enable_fp_el1() };

        note(
            TEST_START,
            Level::Info,
            "raster kernels: resolving on aarch64",
        );
        let decisions = resolve(CpuFeatureSet::new().with(CpuFeature::Asimd));
        for decision in [decisions.blend, decisions.blur, decisions.scan] {
            if !decision.is_some_and(|decision| decision.reason == DecisionReason::Priority) {
                note(
                    TEST_FAIL,
                    Level::Error,
                    "raster kernels: a family fell back to its portable baseline or resolved first",
                );
                qemu_exit::exit_failure(FAIL_SELECTED);
            }
        }

        let produced = conformance_digest();
        log(
            &SERIAL_SINK,
            &Event {
                level: if produced == CONFORMANCE_DIGEST {
                    Level::Info
                } else {
                    Level::Error
                },
                id: if produced == CONFORMANCE_DIGEST {
                    TEST_PASS
                } else {
                    TEST_FAIL
                },
                message: "raster kernels: conformance digest",
                fields: &[
                    Field {
                        key: "produced",
                        value: FieldValue::UnsignedInt(produced),
                    },
                    Field {
                        key: "expected",
                        value: FieldValue::UnsignedInt(CONFORMANCE_DIGEST),
                    },
                ],
            },
        );
        if produced == CONFORMANCE_DIGEST {
            qemu_exit::exit_success();
        }
        qemu_exit::exit_failure(FAIL_DIGEST);
    }
}

// Host stub. The crate is only meaningful on the bare-metal target; on the
// host a no-op `main` keeps `cargo build` and `cargo test` green without
// the crate having to be special-cased.
#[cfg(not(itest_aarch64))]
fn main() {}
