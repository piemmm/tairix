//! `extern "C" fn tairix_arch_aarch64_main` — the Rust side of the boot
//! trampoline.
//!
//! `boot.s` turns translation on under its early map of the image and
//! `bl`s this symbol from the high half with the device tree's physical
//! address and the length its header gives (AAPCS64). This function moves the
//! boot CPU onto the kernel root, then hands over to the binary-supplied
//! `extern "C" fn kernel_main(tree) -> !`, mirroring the riscv64 port's
//! `tairix_arch_riscv64_main` seam: each test binary (and the production
//! kernel binary) defines `kernel_main` exactly once.

extern "C" {
    /// Provided by the linked binary. Must not return.
    ///
    /// `tree` is where the kernel reads the firmware's device tree, read
    /// only ([`crate::paging::firmware_tree`]), or `0` when the loader handed
    /// over none, as QEMU's ELF loader does.
    fn kernel_main(tree: u64) -> !;
}

/// The trampoline jumps here, exactly once, on the boot CPU.
///
/// The EL1 vectors are armed first — with them FP/SIMD, which the code that
/// builds the kernel root may be lowered to — so not even that takes an
/// exception with `VBAR_EL1` still pointing wherever the firmware left it.
/// Nothing else runs before the boot CPU is on the kernel root: the
/// trampoline's early map is writable and executable throughout. Then the
/// boot CPU settles and holds the kernel's protection ([`crate::protection`]).
///
/// A kernel root that cannot be built is a malformed image; the CPU parks,
/// with nothing yet mapped to report through.
///
/// # Safety
///
/// Implicitly safe to call from the asm trampoline because `boot.s`'s
/// SAFETY-INVARIANTs hold (EL1, interrupts masked, the early map and the
/// boot stack live). Calling from anywhere else is a kernel bug.
#[no_mangle]
pub extern "C" fn tairix_arch_aarch64_main(tree_phys: u64, tree_len: u64) -> ! {
    // SAFETY: the boot CPU, a stack established, interrupts masked — the
    // contract `init_vectors` states.
    unsafe { crate::exceptions::init_vectors() };
    // SAFETY: the boot CPU, once, from the trampoline's early map with
    // interrupts masked: the contract `enter_kernel_root` states.
    let Ok(tree) = (unsafe { crate::paging::enter_kernel_root(tree_phys, tree_len) }) else {
        crate::kernel_arch::halt_current_cpu()
    };
    crate::protection::settle();
    // SAFETY: EL1 on the boot CPU, with no user page mapped yet; the boot CPU
    // offers what it settled, so it holds it, and then enables translation
    // under it.
    unsafe {
        let _ = crate::protection::hold();
        crate::paging::hold_translation_controls();
    }
    // SAFETY: `kernel_main` is provided by the linked binary and is
    // documented `-> !` (see the `extern` block).
    unsafe { kernel_main(tree) }
}
