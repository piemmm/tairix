//! QEMU integration test: the x86_64 boot stack's poison guard.
//!
//! The boot stack is in use before paging is on, so the guard is poison
//! while the trampoline runs, and read-only once the kernel root is loaded.
//! Only a real boot can show that the linker reserved it, that the stub's
//! fill survived the `.bss` clear that spans it, and that the port's
//! post-mortem handle names that same reservation. The checks themselves
//! are `tairix_itest_bootguard`'s, shared with the sibling ports; the
//! judgement they exercise is unit-tested on the host.
//!
//! Any failure is reported to QEMU as a distinct, non-zero exit code.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::fmt::Write as _;
    use core::panic::PanicInfo;

    use tairix_arch_api::CpuStateCapture as _;
    use tairix_arch_x86_64::backtrace::Backtracer;
    use tairix_arch_x86_64::{qemu_exit, serial};
    use tairix_itest_bootguard::check;

    extern "C" {
        /// Lowest address of the boot stack's poison guard, through the
        /// kernel window (`linker.ld` derives it from `boot.s`).
        static boot_stack_guard_bottom_high: u8;
        /// The boot stack's lowest byte — one past the guard's last.
        static boot_stack_bottom_high: u8;
    }

    #[panic_handler]
    fn bootguard_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        tairix_arch_x86_64::panic::handle_panic_via_serial(info)
    }

    #[no_mangle]
    pub extern "C" fn kernel_main(_multiboot_info: u64) -> ! {
        let mut com1 = serial::Serial::init(serial::COM1_BASE);
        let bt = Backtracer::new();
        let sp = bt.capture().sp;
        // SAFETY: taking the address of the extern guard symbols is a
        // link-time constant; they are never dereferenced here.
        let low = core::ptr::addr_of!(boot_stack_guard_bottom_high) as u64;
        let bottom = core::ptr::addr_of!(boot_stack_bottom_high) as u64;
        // The kernel root maps the guard read-only, so the check writes it
        // through the direct map's alias of the same bytes.
        let writable = tairix_arch_x86_64::paging::physmap_virt(
            bottom - tairix_arch_x86_64::paging::KERNEL_VMA_BASE,
        );
        // SAFETY: `boot.s` reserves `[low, bottom)` as the BSP's boot-stack
        // guard and nothing else claims it, and `writable` is its stack-bottom
        // byte through the direct map, which the floor covers; this kernel is
        // the only thing running, so the checks' write-and-restore is
        // unraced.
        match unsafe { check(bt.boot_stack_guard(), low, bottom, sp, writable) } {
            Ok(()) => {
                let _ = writeln!(com1, "[bootguard] x86_64 boot-stack guard verified");
                qemu_exit::exit_success()
            }
            Err(defect) => {
                let _ = writeln!(
                    com1,
                    "[bootguard] x86_64 FAILED ({}): {}",
                    defect.code(),
                    defect.describe()
                );
                qemu_exit::exit_failure()
            }
        }
    }
}

// Host stub. The crate is *only* meaningful on the bare-metal x86_64
// target; on the host a no-op `main` keeps `cargo build` / `cargo test`
// against the host triple working for IDE indexing and `cargo xtask ci`.
#[cfg(not(itest_x86_64))]
fn main() {}
