//! x86_64 implementation of the Arch HAL "enter user mode" surface
//! ([`tairix_arch_api::EnterUser`]).
//!
//! Dropping a freshly built process image into ring 3 is the `iretq`
//! sequence: build the interrupt-return frame the CPU pops on `iretq`
//! (`SS`, user `RSP`, `RFLAGS`, `CS`, `RIP` — from the top of the
//! kernel stack down) from the ring-3 GDT selectors
//! ([`crate::gdt::USER_CS_INDEX`] / [`crate::gdt::USER_DS_INDEX`], both
//! at RPL 3), place the first-argument value in `rdi` (the System V
//! AMD64 first integer register), and `iretq`. This is the one
//! definition of that sequence; the CC2/CC3 QEMU
//! verticals reach it through the HAL rather than copying the `asm!`
//! block.
//!
//! # The thread pointer
//!
//! x86_64's psABI thread pointer is the **`FS` base**, and unlike aarch64's
//! `TPIDR_EL0` and riscv64's `tp` it is not a user-writable register: with
//! `CR4.FSGSBASE` off, ring 3 cannot program it and `wrmsr IA32_FS_BASE` is a
//! CPL-0 instruction. The kernel therefore *owns* each thread's value: it is
//! programmed here at entry and reprogrammed by
//! [`crate::userentry::set_user_thread_pointer`] before every switch into the
//! thread, out of the thread's own switch-in hook (`plans/THREADS.md`
//! decision 7). The syscall
//! entry stub never touches `FS`, so nothing else has to save or restore it.
//!
//! # Interrupt and GS state on entry
//!
//! `RFLAGS` is built with `IF` **set**, so ring 3 runs with interrupts
//! enabled and is therefore preemptible: the periodic LAPIC-timer IRQ the
//! production boot arms (`crate::preempt::init_local_preempt`) is taken in
//! user mode and drives the ring-3 preempt point
//! (`plans/PI.md` D2b-2b-A P-1c) — the x86_64 analogue of aarch64's
//! preemptible-EL0 `SPSR` and riscv64's U-mode supervisor-timer rule. The
//! *kernel* stays non-preemptible: it never executes `sti`, so it always
//! runs with `IF == 0`, and a maskable timer IRQ is only ever *taken* once
//! this `iretq` lands in ring 3 (the dispatcher gates the preempt point on
//! the interrupted `CS` regardless). Only the LAPIC timer is unmasked at
//! boot; device IRQs stay masked at the IO-APIC until a driver binds, so
//! enabling `IF` in ring 3 admits no other interrupt source yet.
//!
//! `iretq` does **not** swap `GS`. The production syscall entry stub
//! (`crate::syscall_entry::syscall_entry_stub`) `swapgs`es on entry and
//! again on exit, so during normal ring-0 execution the per-CPU
//! [`SyscallTls`](crate::syscall_entry::SyscallTls) block lives in
//! `IA32_KERNEL_GS_BASE` (programmed by
//! `syscall_entry::init_local_syscalls`, freestanding-only)
//! while the active `GS` base holds the user value. Entering ring 3
//! therefore leaves `IA32_KERNEL_GS_BASE` untouched and already correct:
//! the next `syscall`'s `swapgs` recovers the kernel TLS exactly as it
//! does for a ring-3 program that the kernel never re-entered. This
//! port adds no `swapgs` of its own.

use tairix_arch_api::{EnterUser, UserEntry};

/// x86_64 implementation of the Arch HAL "enter user mode" surface.
///
/// Zero-sized: the `iretq` transition needs no per-instance state.
#[derive(Debug, Default, Clone, Copy)]
pub struct UserMode;

impl UserMode {
    /// Construct the x86_64 enter-user handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// The single, `'static` [`UserMode`] the kernel borrows as this port's
/// `&'static dyn EnterUser` handle.
///
/// A process's [`UserMode`] handle is carried alongside its address space so a
/// thread created later is entered through the same transition its first
/// thread was, with no per-arch producer of its own
/// (`plans/THREADS.md` decision 9). The type is zero-sized, so one shared
/// instance serves every CPU.
pub static USER_MODE: UserMode = UserMode::new();

impl EnterUser for UserMode {
    unsafe fn enter_user(&self, regs: UserEntry) -> ! {
        // SAFETY: the caller's `EnterUser::enter_user` contract
        // guarantees `regs.entry` is a ring-3-executable VA and
        // `regs.stack_pointer` a ring-3-writable stack top in the active
        // address space, and that the syscall/exception entry path is
        // installed.
        // SAFETY (thread pointer): `IA32_FS_BASE` is a per-thread scratch
        // base the kernel itself never dereferences, so any value is safe;
        // this runs at CPL 0 on the CPU about to enter the thread.
        unsafe { set_user_thread_pointer(regs.tls_base) };
        unsafe { enter_ring3(regs.entry, regs.stack_pointer, regs.arg0) }
    }
}

/// Ring-3 user code selector — `(USER_CS_INDEX << 3) | RPL 3`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const USER_CS: u64 = ((crate::gdt::USER_CS_INDEX << 3) | 3) as u64;
/// Ring-3 stack/data selector — `(USER_DS_INDEX << 3) | RPL 3`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const USER_SS: u64 = ((crate::gdt::USER_DS_INDEX << 3) | 3) as u64;
/// `RFLAGS` for the `iretq` frame: bit 1 is the architecturally
/// reserved-one bit; `IF` (bit 9) is **set** so ring 3 runs with
/// interrupts enabled and the periodic LAPIC timer can preempt a runaway
/// user task (`plans/PI.md` D2b-2b-A P-1c). The kernel itself never sets
/// `IF` (it issues no `sti`), so it stays non-preemptible; this only makes
/// *user* mode interruptible (parity with aarch64's preemptible-EL0 `SPSR`
/// and riscv64's U-mode supervisor-timer rule).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const USER_RFLAGS: u64 = (1 << 1) | (1 << 9);

/// The `IA32_FS_BASE` MSR (Intel SDM Vol 4 §2.1): the base the `fs:` segment
/// prefix adds, and the x86_64 psABI thread pointer.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const IA32_FS_BASE: u32 = 0xC000_0100;

/// Program the calling CPU's user thread pointer (`IA32_FS_BASE`) to
/// `tls_base`.
///
/// Called at user entry and again from a thread's switch-in hook, because the
/// register is privileged: ring 3 cannot maintain it itself and the kernel
/// never saves it in a trap frame, so the value has to be (re)installed by the
/// side that knows which thread is about to run.
///
/// # Safety
///
/// The caller must run at CPL 0 on the CPU that is about to execute the thread
/// `tls_base` belongs to. The value itself needs no guarantee: the kernel never
/// dereferences `FS`, so a nonsensical base can only fault that thread's own
/// accesses.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn set_user_thread_pointer(tls_base: u64) {
    // SAFETY: `IA32_FS_BASE` is unconditionally present in long mode and
    // accepts any canonical base; the write is privileged (CPL 0, which this
    // function's contract requires). A non-canonical value would `#GP` here
    // rather than corrupt anything, and only a value the calling thread chose
    // for itself can reach this.
    unsafe { crate::msr::write(IA32_FS_BASE, tls_base) }
}

/// Host substitute: there is no `IA32_FS_BASE` to program off the bare-metal
/// target. Never linked into a kernel image and never reached on the host (the
/// `threads_qemu_x86_64` vertical proves each thread presents its own thread
/// pointer).
///
/// # Safety
///
/// Carries the same contract as the bare-metal definition above, so the two
/// `cfg` arms present one `unsafe` API. The host body is inert.
#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
pub unsafe fn set_user_thread_pointer(tls_base: u64) {
    let _ = tls_base;
}

/// Drop to ring 3 at `entry` with stack pointer `sp` and `rdi` set, and no
/// other register holding anything the kernel left there.
///
/// The transition runs on this CPU's `RSP0`, found through its TLS slot by
/// its LAPIC id so it does not depend on the GS convention of the caller:
/// the header of the task's extended-state area directly above is zeroed —
/// and under XSAVE its image's own header too, which the stack beneath it
/// left dirty and the first XRSTOR from the area would fault on — every
/// enabled state component is loaded from its initial state
/// ([`crate::xstate::INIT_IMAGE`]) — which also clears `xmm0`–`xmm15`, the
/// x87 file and the upper halves, and sets `MXCSR` to its default — and every
/// GPR but `rdi` is zeroed before the `iretq`. Kernel pointers left in a
/// register would hand a new process the kernel's layout. This CPU's
/// registers then hold no area's state, so its owner is cleared first.
///
/// # Safety
///
/// See [`EnterUser::enter_user`]: `entry` must be a valid
/// ring-3-executable virtual address, `sp` a valid ring-3-writable
/// stack top, the GDT must carry the user code/data descriptors at
/// [`crate::gdt::USER_CS_INDEX`] / [`crate::gdt::USER_DS_INDEX`], and
/// the TSS `RSP0` plus the syscall/exception entry path must be
/// installed, with `RSP0` the entering task's. Diverges via `iretq`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
unsafe fn enter_ring3(entry: u64, sp: u64, arg0: u64) -> ! {
    use crate::xstate::{
        Flavour, HEADER_BYTES, INIT_IMAGE, XSAVE_HEADER_BYTES, XSAVE_HEADER_OFFSET,
    };
    const _: () = assert!(
        HEADER_BYTES == 8 * 8 && XSAVE_HEADER_BYTES == 8 * 8,
        "each header is zeroed in eight stores"
    );

    let cpu = crate::preempt::cpu_id_for_lapic(crate::apic::local_apic_id());
    let tls = usize::try_from(cpu)
        .ok()
        .and_then(crate::syscall_entry::syscall_tls_ptr);
    let (Some(tls), Some(config)) = (tls, crate::xstate::published()) else {
        // SAFETY-INVARIANT: a CPU admits a user task only once it has
        // registered its entry stack and the boot CPU has published the
        // extended-state layout.
        crate::panic::refuse("user mode entered on a CPU with no entry stack or extended state");
    };
    // SAFETY: `tls` is this CPU's registered slot, named by its own LAPIC
    // id, which nothing else writes while this CPU runs kernel code.
    let rsp0 = unsafe {
        (*tls).xstate_owner = 0;
        (*tls).kernel_rsp0
    };
    let (xcr0_lo, xcr0_hi) = crate::msr::halves(config.xcr0());
    let fxsave = u64::from(config.flavour() == Flavour::Fxsave);
    let scrub = u64::from(config.scrubs_x87_pointers());
    // SAFETY: the sanctioned assembly carve-out (no Rust spelling for
    // `iretq` or the interrupt-return frame). `RSP0` is this CPU's validated,
    // mapped entry stack, and nothing above the frames being abandoned is
    // live: the task's own kernel frames end there, and the boot context
    // that may call this never returns. Under XSAVE the task's image holds at
    // least the legacy half and its 64-byte header, so the header stores stay
    // inside its own area. XRSTOR's `EDX:EAX` is the enabled `XCR0`, and
    // `INIT_IMAGE` is 64-byte aligned with a zero header; FXRSTOR64 reads its
    // legacy half. The x87 scrub, called on the entry stack just below the
    // area, reads only its own constant and writes no memory. The five `push`es
    // build the long-mode `iretq`
    // frame in the order the CPU pops it (SDM Vol 3A §6.14.3). The caller's
    // safety contract guarantees the mapped entry/stack and the installed
    // selectors/TSS. `options(noreturn)` matches the divergence, which is
    // also what lets the block zero registers it names no operand for.
    unsafe {
        core::arch::asm!(
            "cli",
            "mov rsp, {rsp0}",
            "mov qword ptr [rsp], 0",
            "mov qword ptr [rsp + 8], 0",
            "mov qword ptr [rsp + 16], 0",
            "mov qword ptr [rsp + 24], 0",
            "mov qword ptr [rsp + 32], 0",
            "mov qword ptr [rsp + 40], 0",
            "mov qword ptr [rsp + 48], 0",
            "mov qword ptr [rsp + 56], 0",
            "test {scrub}, {scrub}",
            "jz 4f",
            "call {x87_scrub}",
            "4:",
            "test {fxsave}, {fxsave}",
            "jnz 2f",
            "mov qword ptr [rsp + {xheader}], 0",
            "mov qword ptr [rsp + {xheader} + 8], 0",
            "mov qword ptr [rsp + {xheader} + 16], 0",
            "mov qword ptr [rsp + {xheader} + 24], 0",
            "mov qword ptr [rsp + {xheader} + 32], 0",
            "mov qword ptr [rsp + {xheader} + 40], 0",
            "mov qword ptr [rsp + {xheader} + 48], 0",
            "mov qword ptr [rsp + {xheader} + 56], 0",
            "xrstor64 [{image}]",
            "jmp 3f",
            "2:",
            "fxrstor64 [{image}]",
            "3:",
            "push {ss}",
            "push {sp}",
            "push {rflags}",
            "push {cs}",
            "push {entry}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor esi, esi",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            rsp0 = in(reg) rsp0,
            fxsave = in(reg) fxsave,
            scrub = in(reg) scrub,
            x87_scrub = sym crate::xstate::tairix_arch_x86_64_x87_scrub,
            xheader = const XSAVE_HEADER_OFFSET,
            image = in(reg) &raw const INIT_IMAGE,
            sp = in(reg) sp,
            entry = in(reg) entry,
            ss = const USER_SS,
            rflags = const USER_RFLAGS,
            cs = const USER_CS,
            in("eax") xcr0_lo,
            in("edx") xcr0_hi,
            in("rdi") arg0,
            options(noreturn),
        );
    }
}

/// Host substitute: the `iretq` transition is meaningful only on the
/// bare-metal x86_64 target, so the host build cannot perform it. It is
/// never linked into a kernel image and never reached on the host (the
/// QEMU verticals exercise the real transition).
///
/// # Safety
///
/// Never call on the host; see [`EnterUser::enter_user`] for the
/// bare-metal contract.
#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
unsafe fn enter_ring3(_entry: u64, _sp: u64, _arg0: u64) -> ! {
    unreachable!("enter_ring3 is only meaningful on the bare-metal x86_64 target")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_mode_handle_is_object_safe() {
        let port = UserMode::new();
        let _: &dyn EnterUser = &port;
    }
}
