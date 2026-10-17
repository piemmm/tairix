//! QEMU integration test: the world generator agrees with the reference
//! digest on `riscv64`.
//!
//! ## What this proves that a host test cannot
//!
//! `WinterSun`'s world is a pure function of its seed, and the realm server
//! and every connected client generate it independently. If two of them
//! disagree by one sub-unit, a player walks through a hill on one machine
//! and around it on another, and no amount of netcode recovers from that.
//!
//! The arithmetic is written to be target-independent — IEEE-754 basic
//! operations, `lib/util::mathf` rather than a platform libm, quantised
//! integer outputs, total orders on every sort — but "written to be" is
//! not evidence. This vertical is the evidence: the whole pipeline runs
//! under a different compiler backend on a different ISA, and the digest
//! it folds must equal
//! `tairix_wintersun_world::digest::REFERENCE_DIGEST` — the same
//! constant the host suite and the sibling `aarch64`, `x86_64` and
//! `wasm32` verticals assert. Agreement between targets follows from each
//! agreeing with the constant, so no target can pass by having never run.
//!
//! Any other outcome is a closed failure reported to QEMU.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;

    use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit, SERIAL_SINK};
    use tairix_itest_finisher::fail_point;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
    use tairix_wintersun_world::digest::{self, REFERENCE_DIGEST};

    /// Stable audit-event ids for the QEMU transcript.
    const TEST_START: EventId = EventId(4420);
    const TEST_PASS: EventId = EventId(4421);
    const TEST_FAIL: EventId = EventId(4422);
    const TEST_PROGRESS: EventId = EventId(4423);

    /// Failure finisher code: the realm could not be solved at all.
    const FAIL_GENERATE: NonZeroU16 = fail_point!(1);
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

    fn note(id: EventId, message: &'static str) {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Info,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// Report a digest mismatch with both values, so a failure names the
    /// divergence rather than merely announcing one.
    fn report(id: EventId, level: Level, produced: u64) {
        log(
            &SERIAL_SINK,
            &Event {
                level,
                id,
                message: "world determinism: reference realm digest",
                fields: &[
                    Field {
                        key: "produced",
                        value: FieldValue::UnsignedInt(produced),
                    },
                    Field {
                        key: "expected",
                        value: FieldValue::UnsignedInt(REFERENCE_DIGEST),
                    },
                ],
            },
        );
    }

    #[panic_handler]
    fn world_determinism_qemu_riscv64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Entry point for the freestanding kernel.
    #[no_mangle]
    pub extern "C" fn kernel_main(_hartid: u64, _dtb: u64) -> ! {
        note(
            TEST_START,
            "world determinism: generating the reference realm",
        );

        // Each stage said as it is folded, so the runner reads a guest working
        // through the fold rather than a hung one.
        let mut folding = digest::Folding::new(digest::reference_params());
        let produced = loop {
            match folding.step() {
                Ok(Some(produced)) => break produced,
                Ok(None) => note(TEST_PROGRESS, "world determinism: a stage folded"),
                Err(_) => {
                    note(TEST_FAIL, "world determinism: the realm did not solve");
                    qemu_exit::exit_failure(FAIL_GENERATE);
                }
            }
        };

        if produced == REFERENCE_DIGEST {
            report(TEST_PASS, Level::Info, produced);
            note(
                TEST_PASS,
                "world determinism: riscv64 agrees with the reference",
            );
            qemu_exit::exit_success();
        }
        report(TEST_FAIL, Level::Error, produced);
        qemu_exit::exit_failure(FAIL_DIGEST);
    }
}

// Host stub. The crate is only meaningful on the bare-metal target; on the
// host a no-op `main` keeps `cargo build` and `cargo test` green without
// the crate having to be special-cased.
#[cfg(not(itest_riscv64))]
fn main() {}
