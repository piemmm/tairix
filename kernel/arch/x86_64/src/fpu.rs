//! The kernel's floating-point discipline on x86_64.
//!
//! The kernel is compiled against the SSE2 baseline, so the compiler may emit
//! SSE in any function. Kernel code therefore clobbers `xmm0`–`xmm15` (their
//! low 128 bits) and `MXCSR`, and nothing else: legacy SSE leaves the YMM and
//! ZMM upper bits alone, the floor never implies VEX, and no kernel code uses
//! x87 or MMX. Every stub that calls Rust frames exactly that set
//! (`crate::fp_frame_save`) and runs the handler under `KERNEL_MXCSR`, so
//! an interrupted context's `MXCSR` — a user's unmasked exceptions or rounding
//! mode — never governs kernel code. The extended state the kernel leaves
//! alone is saved per task only when the task parks (`crate::xstate`).

/// The architectural default `MXCSR`: round to nearest, every exception
/// masked, no flush-to-zero. The kernel runs under it and every new user
/// context starts with it.
pub const MXCSR_DEFAULT: u32 = 0x1F80;

/// The `MXCSR` every entry stub loads before it calls Rust.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) static KERNEL_MXCSR: u32 = MXCSR_DEFAULT;

/// Bytes of the SSE frame each entry saves below its GPRs: `xmm0`–`xmm15`,
/// then `MXCSR` padded to keep the frame 16-byte aligned.
pub const FP_FRAME_BYTES: usize = 16 * 16 + 16;
/// Offset of the saved `MXCSR` in the SSE frame.
pub const FP_FRAME_MXCSR: usize = 16 * 16;

/// Template text for an entry stub: reserve the SSE frame below a 16-byte
/// aligned `%rsp`, save `xmm0`–`xmm15` and the interrupted `MXCSR` into it,
/// and load `KERNEL_MXCSR`. The `naked_asm!` it is spliced into binds
/// `fp_frame`, `fp_mxcsr` and `kernel_mxcsr`.
#[doc(hidden)]
#[macro_export]
macro_rules! fp_frame_save {
    () => {
        concat!(
            "subq ${fp_frame}, %rsp\n",
            "movaps %xmm0, 0(%rsp)\n",
            "movaps %xmm1, 16(%rsp)\n",
            "movaps %xmm2, 32(%rsp)\n",
            "movaps %xmm3, 48(%rsp)\n",
            "movaps %xmm4, 64(%rsp)\n",
            "movaps %xmm5, 80(%rsp)\n",
            "movaps %xmm6, 96(%rsp)\n",
            "movaps %xmm7, 112(%rsp)\n",
            "movaps %xmm8, 128(%rsp)\n",
            "movaps %xmm9, 144(%rsp)\n",
            "movaps %xmm10, 160(%rsp)\n",
            "movaps %xmm11, 176(%rsp)\n",
            "movaps %xmm12, 192(%rsp)\n",
            "movaps %xmm13, 208(%rsp)\n",
            "movaps %xmm14, 224(%rsp)\n",
            "movaps %xmm15, 240(%rsp)\n",
            "stmxcsr {fp_mxcsr}(%rsp)\n",
            "ldmxcsr {kernel_mxcsr}(%rip)\n",
        )
    };
}

/// Template text undoing `fp_frame_save`: the interrupted context's
/// `MXCSR` and `xmm0`–`xmm15` back, and the frame released.
#[doc(hidden)]
#[macro_export]
macro_rules! fp_frame_restore {
    () => {
        concat!(
            "ldmxcsr {fp_mxcsr}(%rsp)\n",
            "movaps 0(%rsp), %xmm0\n",
            "movaps 16(%rsp), %xmm1\n",
            "movaps 32(%rsp), %xmm2\n",
            "movaps 48(%rsp), %xmm3\n",
            "movaps 64(%rsp), %xmm4\n",
            "movaps 80(%rsp), %xmm5\n",
            "movaps 96(%rsp), %xmm6\n",
            "movaps 112(%rsp), %xmm7\n",
            "movaps 128(%rsp), %xmm8\n",
            "movaps 144(%rsp), %xmm9\n",
            "movaps 160(%rsp), %xmm10\n",
            "movaps 176(%rsp), %xmm11\n",
            "movaps 192(%rsp), %xmm12\n",
            "movaps 208(%rsp), %xmm13\n",
            "movaps 224(%rsp), %xmm14\n",
            "movaps 240(%rsp), %xmm15\n",
            "addq ${fp_frame}, %rsp\n",
        )
    };
}

/// Template text for an interrupt stub's exit, spliced in after its Rust
/// returns and before `fp_frame_restore`: when the frame returns to ring 3
/// and the task's extended state is pending, load it. The `naked_asm!` binds
/// `frame_cs` and `frame_top`, the saved `CS` and the frame's top — the
/// task's area — as offsets from `%rsp` at this point.
#[doc(hidden)]
#[macro_export]
macro_rules! xstate_ring3_exit {
    () => {
        concat!(
            "testb $3, {frame_cs}(%rsp)\n",
            "jz 2f\n",
            "cmpb $0, {frame_top}(%rsp)\n",
            "je 2f\n",
            "leaq {frame_top}(%rsp), %rdi\n",
            "call tairix_arch_x86_64_xstate_load\n",
            "2:\n",
        )
    };
}

// Signed, so each mask is the sign-extended immediate `andq`/`orq` encode.

/// `CR0.MP`: `WAIT` honours `CR0.TS`.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const CR0_MP: i64 = 1 << 1;
/// `CR0.EM`: x87 and SSE instructions raise `#UD`/`#NM` when set.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const CR0_EM: i64 = 1 << 2;
/// `CR0.TS`: the next FPU instruction raises `#NM` when set.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const CR0_TS: i64 = 1 << 3;
/// `CR0.NE`: x87 errors are reported as `#MF`, not through the legacy IRQ.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const CR0_NE: i64 = 1 << 5;
/// `CR4.OSFXSR`: SSE instructions and `FXSAVE`/`FXRSTOR` are enabled.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const CR4_OSFXSR: i64 = 1 << 9;
/// `CR4.OSXMMEXCPT`: an unmasked SIMD exception is delivered as `#XM`.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const CR4_OSXMMEXCPT: i64 = 1 << 10;

/// Make the FPU and SSE usable on the calling CPU and give it the kernel's
/// floating-point environment: x87 initialised, `MXCSR` at `KERNEL_MXCSR`.
///
/// Called by the boot CPU's entry (`boot.s`) and each application
/// processor's (`ap_trampoline.s`) before their first Rust instruction.
/// Clobbers `rax` and the flags; touches no stack but its return address.
///
/// # Safety
///
/// Only the entry assembly may call it, at CPL 0 with a usable stack.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn tairix_arch_x86_64_fpu_enable() {
    core::arch::naked_asm!(
        "movq %cr0, %rax",
        "andq ${cr0_clear}, %rax",
        "orq ${cr0_set}, %rax",
        "movq %rax, %cr0",
        "movq %cr4, %rax",
        "orq ${cr4_set}, %rax",
        "movq %rax, %cr4",
        "fninit",
        "ldmxcsr {mxcsr}(%rip)",
        "ret",
        cr0_clear = const !(CR0_EM | CR0_TS),
        cr0_set = const CR0_MP | CR0_NE,
        cr4_set = const CR4_OSFXSR | CR4_OSXMMEXCPT,
        mxcsr = sym KERNEL_MXCSR,
        options(att_syntax),
    )
}

const _: () = assert!(FP_FRAME_BYTES.is_multiple_of(16) && FP_FRAME_MXCSR + 4 <= FP_FRAME_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    /// Masking every exception means an operation the kernel's own code gets
    /// wrong produces its IEEE 754 default result instead of a `#XM`.
    #[test]
    fn the_kernel_runs_with_every_exception_masked_and_round_to_nearest() {
        const EXCEPTION_MASKS: u32 = 0x3F << 7;
        const ROUNDING: u32 = 0b11 << 13;
        const FLUSH_TO_ZERO: u32 = 1 << 15;
        const DENORMALS_ARE_ZERO: u32 = 1 << 6;
        assert_eq!(MXCSR_DEFAULT & EXCEPTION_MASKS, EXCEPTION_MASKS);
        assert_eq!(
            MXCSR_DEFAULT & (ROUNDING | FLUSH_TO_ZERO | DENORMALS_ARE_ZERO),
            0
        );
        assert_eq!(MXCSR_DEFAULT & 0x3F, 0, "no sticky flag is set");
    }

    #[test]
    fn the_enable_clears_what_it_sets_nowhere() {
        assert_eq!((CR0_EM | CR0_TS) & (CR0_MP | CR0_NE), 0);
        assert_eq!(CR4_OSFXSR | CR4_OSXMMEXCPT, 0x600);
    }
}
