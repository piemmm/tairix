//! U-mode fixture proving first entry to user mode leaks no kernel register
//! state (`plans/OPEN-DEFECTS.md` D360).
//!
//! It owns its own naked `_start` — it links no runtime — because a runtime
//! prologue would overwrite the entry register state before anything could
//! observe it. `_start` captures every integer register the instant control
//! reaches user mode, then a checker exits 0 only if every register the kernel
//! did not hand the task is zero: a kernel pointer or another task's value left
//! in one is the leak this catches. The registers the kernel *does* set — the
//! stack pointer, the first-argument register, and the thread pointer — carry
//! the task's own entry values and are not checked here.
//!
//! On the host, and on any non-freestanding target, it is an inert stub.

#![cfg_attr(entry_hygiene, no_std)]
#![cfg_attr(entry_hygiene, no_main)]
#![deny(missing_docs)]

#[cfg(entry_hygiene)]
mod program {
    /// The `exit` syscall number, from the one ABI definition.
    const EXIT: u64 = tairix_abi::SyscallNumber::EXIT.as_u16() as u64;

    /// Exit code when a captured register that must be zero was not — the code
    /// is the offending register's index in the capture, plus one so it is
    /// never the success code.
    fn fail(index: usize) -> ! {
        exit((index + 1) as i32)
    }

    /// Exit through the raw `abi-v1` `exit` syscall. No runtime, so this is the
    /// only way out.
    #[cfg(entry_hygiene_x86_64)]
    fn exit(code: i32) -> ! {
        // SAFETY: `exit` is unprivileged and diverges; the number is in `rax`
        // and the code in `rdi`, the System V syscall ABI.
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rax") EXIT,
                in("rdi") code as i64,
                options(noreturn, nostack),
            );
        }
    }
    #[cfg(entry_hygiene_aarch64)]
    fn exit(code: i32) -> ! {
        // SAFETY: `exit` is unprivileged and diverges; the number is in `x8`
        // and the code in `x0`.
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") EXIT,
                in("x0") code as i64,
                options(noreturn, nostack),
            );
        }
    }
    #[cfg(entry_hygiene_riscv64)]
    fn exit(code: i32) -> ! {
        // SAFETY: `exit` is unprivileged and diverges; the number is in `a7`
        // and the code in `a0`.
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a7") EXIT,
                in("a0") code as i64,
                options(noreturn, nostack),
            );
        }
    }

    // x86_64: `_start` pushes every GPR (rsp changes, but is not checked) and
    // hands the block to `check`. `rdi` holds the entry argument, so its
    // captured value is ignored.
    #[cfg(entry_hygiene_x86_64)]
    core::arch::global_asm!(
        ".globl _start",
        "_start:",
        "push r15", "push r14", "push r13", "push r12", "push r11", "push r10",
        "push r9", "push r8", "push rbp", "push rsi", "push rdi", "push rdx",
        "push rcx", "push rbx", "push rax",
        "mov rdi, rsp",
        "call {check}",
        check = sym check,
    );

    /// The order the x86_64 `_start` pushed the registers in (ascending
    /// address from `rsp`): the check skips `rdi`, the entry argument.
    #[cfg(entry_hygiene_x86_64)]
    const NAMES: &[&str] = &[
        "rax", "rbx", "rcx", "rdx", "rdi", "rsi", "rbp", "r8", "r9", "r10", "r11", "r12", "r13",
        "r14", "r15",
    ];
    /// The register the kernel loads with the entry argument, skipped.
    #[cfg(entry_hygiene_x86_64)]
    const ARG: &str = "rdi";

    #[cfg(entry_hygiene_aarch64)]
    core::arch::global_asm!(
        ".globl _start",
        "_start:",
        "sub sp, sp, #256",
        "stp x0, x1, [sp, #0]",
        "stp x2, x3, [sp, #16]",
        "stp x4, x5, [sp, #32]",
        "stp x6, x7, [sp, #48]",
        "stp x8, x9, [sp, #64]",
        "stp x10, x11, [sp, #80]",
        "stp x12, x13, [sp, #96]",
        "stp x14, x15, [sp, #112]",
        "stp x16, x17, [sp, #128]",
        "stp x18, x19, [sp, #144]",
        "stp x20, x21, [sp, #160]",
        "stp x22, x23, [sp, #176]",
        "stp x24, x25, [sp, #192]",
        "stp x26, x27, [sp, #208]",
        "stp x28, x29, [sp, #224]",
        "str x30, [sp, #240]",
        "mov x0, sp",
        "bl {check}",
        check = sym check,
    );
    #[cfg(entry_hygiene_aarch64)]
    const NAMES: &[&str] = &[
        "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13",
        "x14", "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26",
        "x27", "x28", "x29", "x30",
    ];
    #[cfg(entry_hygiene_aarch64)]
    const ARG: &str = "x0";

    #[cfg(entry_hygiene_riscv64)]
    core::arch::global_asm!(
        ".globl _start",
        "_start:",
        "addi sp, sp, -256",
        "sd ra, 0(sp)", "sd gp, 8(sp)", "sd tp, 16(sp)", "sd t0, 24(sp)",
        "sd t1, 32(sp)", "sd t2, 40(sp)", "sd s0, 48(sp)", "sd s1, 56(sp)",
        "sd a0, 64(sp)", "sd a1, 72(sp)", "sd a2, 80(sp)", "sd a3, 88(sp)",
        "sd a4, 96(sp)", "sd a5, 104(sp)", "sd a6, 112(sp)", "sd a7, 120(sp)",
        "sd s2, 128(sp)", "sd s3, 136(sp)", "sd s4, 144(sp)", "sd s5, 152(sp)",
        "sd s6, 160(sp)", "sd s7, 168(sp)", "sd s8, 176(sp)", "sd s9, 184(sp)",
        "sd s10, 192(sp)", "sd s11, 200(sp)", "sd t3, 208(sp)", "sd t4, 216(sp)",
        "sd t5, 224(sp)", "sd t6, 232(sp)",
        "mv a0, sp",
        "call {check}",
        check = sym check,
    );
    #[cfg(entry_hygiene_riscv64)]
    const NAMES: &[&str] = &[
        "ra", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4", "a5", "a6",
        "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4", "t5", "t6",
    ];
    /// riscv64 loads the argument into `a0` and the thread pointer into `tp`;
    /// both carry the task's own entry values and are skipped.
    #[cfg(entry_hygiene_riscv64)]
    const ARG: &str = "a0";
    #[cfg(entry_hygiene_riscv64)]
    const TP: &str = "tp";

    /// Verify every captured register the kernel did not set is zero.
    ///
    /// # Safety
    ///
    /// `regs` must point at the `NAMES.len()`-word block `_start` captured.
    unsafe extern "C" fn check(regs: *const u64) -> ! {
        for (index, name) in NAMES.iter().enumerate() {
            if *name == ARG {
                continue;
            }
            #[cfg(entry_hygiene_riscv64)]
            if *name == TP {
                continue;
            }
            // SAFETY: the caller's contract sizes the block to `NAMES.len()`.
            if unsafe { *regs.add(index) } != 0 {
                fail(index);
            }
        }
        exit(0)
    }

    /// A U-mode program has no unwinding runtime; a panic can only halt.
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
        exit(126)
    }
}

// --- Host stub ----------------------------------------------------------
#[cfg(not(entry_hygiene))]
fn main() {}
