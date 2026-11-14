//! `plans/BEEB.md` BB30: boot the aarch64 desktop, launch `OpenMicro.app` from
//! the program library, and pass once a window of it is shown and it ends
//! cleanly at the Quit the host sends after reading its machine's BASIC prompt
//! off the screen and suspending and resuming the machine.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink};
    use tairix_itest_desktop_witness::{window_shown, ShownWindows};
    use tairix_itest_finisher::fail_point;
    use tairix_itest_witness::{field_str, names_bundle};
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_openmicro_qemu_aarch64::{EXIT_SYSCALL, OPENMICRO_APP_NAME};

    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;

    // SAFETY: `HEAP` is a static arena of `HEAP_BYTES` bytes that nothing else
    // references, handed to the one allocator that owns it for the run.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// The emulator ended before its window was shown.
    const FAIL_EXITED_UNSHOWN: NonZeroU16 = fail_point!(1);

    /// No emulator task is known yet: task 0 is the kernel's own context,
    /// which no user-space syscall names.
    const NO_TASK: u64 = 0;

    struct Witnesses {
        launched: AtomicBool,
        /// The emulator's own task, its first syscall's after the launch: its
        /// machine workers run the same binary under the same name.
        app: AtomicU64,
        windows: ShownWindows<1>,
    }

    impl Witnesses {
        const fn new() -> Self {
            Self {
                launched: AtomicBool::new(false),
                app: AtomicU64::new(NO_TASK),
                windows: ShownWindows::new(),
            }
        }

        fn note_bundle_loaded(&self, event: &Event<'_>) {
            if field_str(event, "bundle").is_some_and(|bundle| {
                names_bundle(
                    bundle,
                    tairix_abi::SYSTEM_APPLICATION_STORE,
                    OPENMICRO_APP_NAME,
                )
            }) {
                self.launched.store(true, Ordering::Release);
                self.windows.arm();
            }
        }

        fn note_syscall(&self, event: &Event<'_>) {
            if !self.launched.load(Ordering::Acquire)
                || field_str(event, "comm") != Some(OPENMICRO_APP_NAME)
            {
                return;
            }
            let Some(task) = field_str(event, "task")
                .and_then(|task| u64::from_str_radix(task, 16).ok())
                .filter(|&task| task != NO_TASK)
            else {
                return;
            };
            let app =
                match self
                    .app
                    .compare_exchange(NO_TASK, task, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => task,
                    Err(captured) => captured,
                };
            if task != app || field_str(event, "sc") != Some(EXIT_SYSCALL) {
                return;
            }
            if self.windows.count() >= 1 {
                qemu_exit::exit_success();
            }
            qemu_exit::exit_failure(FAIL_EXITED_UNSHOWN);
        }
    }

    static WITNESSES: Witnesses = Witnesses::new();

    struct AuditObserver;

    impl Sink for AuditObserver {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                WITNESSES.note_bundle_loaded(event);
            } else if event.id.0 == tairix_kernel_syscall::AuditEvent::SyscallInvoked.id().0 {
                WITNESSES.note_syscall(event);
            }
        }
    }

    static AUDIT_SINK: AuditObserver = AuditObserver;

    struct DiagnosticObserver;

    impl Sink for DiagnosticObserver {
        fn write_event(&self, event: &Event<'_>) {
            if let Some(window) = window_shown(event) {
                WITNESSES.windows.note(window);
            }
            SerialSink::new().write_event(event);
        }
    }

    static DIAGNOSTIC_SINK: DiagnosticObserver = DiagnosticObserver;

    #[panic_handler]
    fn tairix_openmicro_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &DIAGNOSTIC_SINK,
            &AUDIT_SINK,
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

#[cfg(not(itest_aarch64))]
fn main() {}
