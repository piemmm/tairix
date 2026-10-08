//! The run when the `test-hooks` feature is off: park the CPU, so the binary
//! links but proves nothing.

/// Park for good.
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    park()
}

#[panic_handler]
fn panic_stub(_info: &core::panic::PanicInfo<'_>) -> ! {
    park()
}

fn park() -> ! {
    loop {
        // SAFETY: `wfe` is a well-defined parked-CPU hint on aarch64.
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
