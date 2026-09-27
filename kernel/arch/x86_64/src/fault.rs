//! x86_64 page-fault (`#PF`, vector 14) entry and the packed exception
//! syndrome every exception reports through.
//!
//! The production IDT ([`crate::interrupts`]) routes every architecturally
//! defined exception to its own entry ([`crate::exceptions`]) and the LAPIC
//! timer / external-IRQ vectors to their dedicated stubs. A page fault is the
//! one exception the kernel can *resolve* — a demand-paged file mapping, or a
//! fault inside the guarded user-copy window — so vector 14 keeps its own
//! resumable entry here rather than sharing the diverging exception tail.
//!
//! What it cannot finish goes to the callbacks in [`tairix_arch_api::fault`],
//! with the same three-tier posture as every port:
//!
//! * A **ring-3 data fault** is offered to the user-fault resolver first —
//!   the demand-paged file-mapping path. A resolved fault returns through the
//!   entry's full GPR restore and `iretq`, retrying the faulting instruction
//!   against the now-resident page.
//! * Any **other ring-3 fault** — an instruction-fetch `#PF` (a wild jump),
//!   or a data fault with no resolver installed — is the running task's own
//!   and is charged to it through the user-fault terminator, which kills the
//!   task and leaves the CPU running other work.
//! * Everything left is the **kernel's own**: the installed fatal handler
//!   gets it, or with none installed the port writes its own report and parks
//!   the CPU (never a silent reset).
//!
//! The faulting address lives in `CR2` on x86_64 (it is *not* pushed on the
//! stack), so the entry captures it before any further fault could clobber
//! it. The decode builds on the host, so its unit tests run under
//! `cargo test`; only the entry stub and the `CR2` read are gated to the
//! freestanding target.

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use tairix_arch_api::backtrace::UserRegisterFrame;

/// IDT vector the CPU raises for a page fault (`#PF`, Intel SDM Vol 3A
/// Table 6-1).
pub const PAGE_FAULT_VECTOR: u8 = 14;

/// `#PF` error-code bit `P` (bit 0): `0` = the access referenced a
/// not-present page, `1` = a page-level protection violation
/// (Intel SDM Vol 3A §4.7).
pub const PF_ERR_PRESENT: u64 = 1 << 0;

/// `#PF` error-code bit `W/R` (bit 1): `1` = the access was a write.
pub const PF_ERR_WRITE: u64 = 1 << 1;

/// `#PF` error-code bit `U/S` (bit 2): `1` = the access originated at
/// CPL 3 (user mode).
pub const PF_ERR_USER: u64 = 1 << 2;

/// `#PF` error-code bit `RSVD` (bit 3): `1` = a reserved bit was set in
/// a paging-structure entry on the translation path.
pub const PF_ERR_RESERVED: u64 = 1 << 3;

/// `#PF` error-code bit `I/D` (bit 4): `1` = the fault was an
/// instruction fetch.
pub const PF_ERR_INSTR: u64 = 1 << 4;

/// `true` iff the fault referenced a **not-present** page (error-code
/// `P` bit clear) — the cause raised when user code touches an address
/// the active page tables do not map, e.g. a use-after-unmap.
#[must_use]
pub const fn is_not_present(error_code: u64) -> bool {
    error_code & PF_ERR_PRESENT == 0
}

/// `true` iff the fault originated in user mode (error-code `U/S` bit
/// set).
#[must_use]
pub const fn is_user(error_code: u64) -> bool {
    error_code & PF_ERR_USER != 0
}

/// `true` iff the faulting access was a write (error-code `W/R` bit set).
#[must_use]
pub const fn is_write(error_code: u64) -> bool {
    error_code & PF_ERR_WRITE != 0
}

/// `true` iff a `#PF` with this error code is a **user-mode data**
/// access (read or write, not an instruction fetch) — the class the
/// dedicated `#PF` entry offers to the user-fault resolver.
/// A kernel-mode fault (`U/S` clear) is never offered: it is never file
/// backing (the kernel copy path resolves its own misses in software),
/// and an instruction fetch is never file backing either (a file mapping
/// is never executable). A write in this class is offered but never
/// *resolved* — see [`is_resolvable_user_fault`] — the resolver kills
/// the faulting task instead, so a store to a read-only mapping (or any
/// wild write) costs the task, never the CPU.
#[must_use]
pub const fn is_user_data_fault(error_code: u64) -> bool {
    is_user(error_code) && error_code & PF_ERR_INSTR == 0
}

/// `true` iff a `#PF` with this error code may actually be *resolved* by
/// making a page resident: a **user-mode, not-present, read data**
/// access — the only shape a demand-paged file-mapping fault can take.
/// Everything else in the offered class is fatal to the task:
///
/// * a protection violation (`P` set) cannot be fixed by making a page
///   resident;
/// * a write can never be made valid — a file mapping is read-only, and
///   resolving a write fault as "already resident, retry" would
///   re-execute the store into an endless fault storm instead of
///   killing the task.
#[must_use]
pub const fn is_resolvable_user_fault(error_code: u64) -> bool {
    is_user(error_code)
        && is_not_present(error_code)
        && error_code & (PF_ERR_WRITE | PF_ERR_INSTR) == 0
}

/// Bit position of the vector field in the packed exception syndrome
/// ([`exception_syndrome`]).
const SYNDROME_VECTOR_SHIFT: u32 = 32;

/// Bit set in a packed exception syndrome when the exception was taken
/// from ring 3 rather than from kernel mode.
const SYNDROME_FROM_USER: u64 = 1 << 40;

/// Pack an x86_64 exception into the neutral fault syndrome word.
///
/// x86_64 has no single cause register: the cause is the *vector*, and
/// only some vectors push an error code. The two are folded into one word
/// so the neutral fault record every port reports can carry both — the
/// error code in bits `0..32` and the vector in bits `32..40`, with bit `40`
/// set when the exception came from ring 3.
///
/// The error code occupies the low half deliberately: it keeps
/// [`is_not_present`] / [`is_user`] / [`is_write`] valid decoders of a
/// `#PF` syndrome, so a handler that only ever provokes page faults reads
/// the same bits it always did. Those decoders are meaningful **only** when
/// [`syndrome_vector`] reports [`PAGE_FAULT_VECTOR`]; for any other vector
/// the error code's bits carry that vector's own meaning (a selector for
/// `#TS`/`#NP`/`#SS`/`#GP`, zero for `#DF` and `#AC`) or nothing at all.
#[must_use]
pub const fn exception_syndrome(vector: u8, error_code: u64, from_user: bool) -> u64 {
    // A hardware error code is 32 bits wide (Intel SDM Vol 3A §6.13), so
    // the low half holds it losslessly; mask rather than trust the caller.
    let code = error_code & 0xFFFF_FFFF;
    let user = if from_user { SYNDROME_FROM_USER } else { 0 };
    code | ((vector as u64) << SYNDROME_VECTOR_SHIFT) | user
}

/// The IDT vector a packed [`exception_syndrome`] names.
#[must_use]
pub const fn syndrome_vector(syndrome: u64) -> u8 {
    #[allow(clippy::cast_possible_truncation)]
    // SAFETY-INVARIANT: the field is 8 bits wide by construction
    // (`exception_syndrome` shifts a `u8` into `32..40`), so the mask makes
    // the narrowing lossless.
    let vector = ((syndrome >> SYNDROME_VECTOR_SHIFT) & 0xFF) as u8;
    vector
}

/// The hardware error code a packed [`exception_syndrome`] carries, or `0`
/// for a vector that pushes none.
#[must_use]
pub const fn syndrome_error_code(syndrome: u64) -> u64 {
    syndrome & 0xFFFF_FFFF
}

/// `true` when a packed [`exception_syndrome`] records an exception taken
/// from ring 3.
#[must_use]
pub const fn syndrome_from_user(syndrome: u64) -> bool {
    syndrome & SYNDROME_FROM_USER != 0
}

/// A kernel fault's words under the names x86_64 gives them, for the port's
/// own report.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
pub(crate) struct Decoded<'a>(pub(crate) &'a tairix_arch_api::fatal::KernelFault);

#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
impl core::fmt::Display for Decoded<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let fault = self.0;
        if let Some(syndrome) = fault.syndrome {
            let ring = if syndrome_from_user(syndrome) { 3 } else { 0 };
            write!(
                f,
                "vector {}, error code {:#x}, ring {ring}, ",
                syndrome_vector(syndrome),
                syndrome_error_code(syndrome)
            )?;
        }
        if let Some(cr2) = fault.address {
            write!(f, "CR2 {cr2:#x}, ")?;
        }
        write!(f, "RIP {:#x}", fault.pc)
    }
}

// --- Freestanding dedicated `#PF` entry ----------------------------

/// Linear address of the dedicated `#PF` ISR stub, for
/// [`crate::percpu::install_vector`].
///
/// Only meaningful on the freestanding target — the symbol is the
/// `#[unsafe(naked)]` stub below.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
pub fn page_fault_isr_addr() -> u64 {
    page_fault_isr as *const () as usize as u64
}

/// Dedicated `#PF` (vector 14) ISR stub — **resumable**.
///
/// On entry the CPU has pushed the hardware error code and the 5-word
/// [`crate::interrupts::InterruptStackFrame`] on the destination stack
/// (RSP0 for a ring-3 fault), so `%rsp` points at the error code and
/// `[%rsp + 8]` at the faulting `rip`. The stub saves the 15
/// architectural GPRs in the [`crate::interrupts::SavedRegs`] order
/// (the same order `define_isr!` pins), marshals `(error_code, CR2,
/// rip, &frame.rip)` into the `SysV` argument registers, and calls
/// [`tairix_arch_x86_64_page_fault_dispatch`]. The dispatcher *returns
/// only when the fault was dealt with* — a resolved ring-3 demand-paging
/// fault (the frame's untouched `RIP` re-runs the faulting instruction,
/// which now succeeds) or a kernel-mode fault inside the guarded
/// user-copy window (the dispatcher rewrote the frame's `RIP` to the
/// copy's fix-up, so the `iretq` resumes there and the copy reports the
/// fault as an error). The stub then restores the GPRs, drops the
/// hardware error code, and `iretq`s. Any other fault never returns
/// from the dispatcher (the fatal path diverges), so a stale resume is
/// impossible.
///
/// `CR2` is read after the GPR saves (a register is needed to hold it)
/// but before any access that could itself fault: pushes to the
/// always-mapped per-CPU kernel stack cannot raise `#PF`.
///
/// Stack alignment: long mode 16-aligns `%rsp` before it pushes any
/// exception frame (Intel SDM Vol 3A §6.14.2), so after the error code +
/// 5-word frame (48 bytes) `%rsp` is 16-aligned on entry, and after the 15
/// GPR pushes (120 bytes) it is ≡ 8 (mod 16). The `subq $8` re-aligns it so
/// the `call` lands the `SysV` callee with `%rsp ≡ 8 (mod 16)` after its
/// return-address push — the System V AMD64 §3.2.2 entry state.
///
/// # Safety
///
/// Only the CPU's IDT may invoke this symbol (installed via
/// [`crate::percpu::install_vector`] on [`PAGE_FAULT_VECTOR`]). Calling
/// it directly from Rust is undefined behaviour because it expects the
/// CPU-pushed error code + interrupt frame on the stack, not a return
/// address.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn page_fault_isr() {
    core::arch::naked_asm!(
        "pushq %rax",
        "pushq %rcx",
        "pushq %rdx",
        "pushq %rbx",
        "pushq %rbp",
        "pushq %rsi",
        "pushq %rdi",
        "pushq %r8",
        "pushq %r9",
        "pushq %r10",
        "pushq %r11",
        "pushq %r12",
        "pushq %r13",
        "pushq %r14",
        "pushq %r15",
        // %rdi <- error code (above the 15 saved GPRs = 120 bytes),
        // %rsi <- CR2 (faulting linear address), %rdx <- faulting rip,
        // %rcx <- address of the frame's RIP slot, so the dispatcher can
        // redirect a kernel-mode fault inside the guarded user-copy
        // window to the copy's fix-up (the `iretq` below then resumes
        // there).
        "movq 120(%rsp), %rdi",
        "mov %cr2, %rsi",
        "movq 128(%rsp), %rdx",
        "leaq 128(%rsp), %rcx",
        // %r8 <- &SavedRegs (the base of the 15-GPR block, = %rsp before
        // the alignment pad), %r9 <- the interrupted %rsp from the CPU iret
        // frame (at 152(%rsp): error 120, rip 128, cs 136, rflags 144,
        // rsp 152), so the dispatcher can build the faulting register frame.
        "movq %rsp, %r8",
        "movq 152(%rsp), %r9",
        "subq $8, %rsp",
        "call {dispatch}",
        "addq $8, %rsp",
        // The dispatcher returned: the fault is resolved. Restore the
        // interrupted GPRs, drop the hardware error code, and retry the
        // faulting instruction.
        "popq %r15",
        "popq %r14",
        "popq %r13",
        "popq %r12",
        "popq %r11",
        "popq %r10",
        "popq %r9",
        "popq %r8",
        "popq %rdi",
        "popq %rsi",
        "popq %rbp",
        "popq %rbx",
        "popq %rdx",
        "popq %rcx",
        "popq %rax",
        "addq $8, %rsp",
        "iretq",
        dispatch = sym tairix_arch_x86_64_page_fault_dispatch,
        options(att_syntax),
    )
}

/// Rust dispatcher the dedicated `#PF` stub calls.
///
/// A **kernel-mode** fault whose `rip` lies inside the guarded
/// user-copy window ([`crate::uaccess`]) is redirected to the copy's
/// fix-up by rewriting the interrupt frame's `RIP` slot (`rip_slot`)
/// and returning: the stub's restore + `iretq` resume at the fix-up,
/// which reports the fault to the copy's caller as an error. Every
/// other kernel-mode fault stays on the fatal path.
///
/// A ring-3 data fault ([`is_user_data_fault`], read or write) is offered to
/// the user-fault resolver first, with the error-code `W/R` verdict: a `true`
/// return means the faulting page is now resident (reads only — a write is
/// never resolved, the resolver kills the faulting task instead), and this
/// function returns so the stub restores the GPRs and `iretq`s into a retry
/// of the faulting instruction.
///
/// Every **other** ring-3 fault goes to the user-fault terminator, which kills
/// the task and never returns for it: an instruction-fetch `#PF` is never file
/// backing (a file mapping is never executable), so a wild jump is
/// unrecoverable but is still one task's mistake — parking the CPU for it
/// would turn a process fault into a machine-wide denial of service. A ring-3
/// data fault with no resolver installed reaches the terminator on the same
/// grounds.
///
/// Only the kernel's own failures are fatal, and those **never return**: the
/// installed fatal handler reports them, or with none installed the port's
/// own report does. A ring-3 fault reaches it only when the resolver or
/// terminator could not attribute the fault to a running task at all.
///
/// `interrupted_rsp` is the `RSP` the CPU pushed: in 64-bit mode it pushes the
/// interrupted stack pointer for every delivery, so it is the user's for a
/// ring-3 fault and the faulting kernel code's otherwise.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[no_mangle]
extern "C" fn tairix_arch_x86_64_page_fault_dispatch(
    error_code: u64,
    faulting_addr: u64,
    rip: u64,
    rip_slot: *mut u64,
    saved: *const crate::interrupts::SavedRegs,
    interrupted_rsp: u64,
) {
    let from_user = is_user(error_code);
    if from_user {
        // A *data* access may be demand-paged, so it goes to the resolver,
        // whose verdict is then final: a `false` means the fault was not
        // attributable to a running task at all, which is the kernel's own
        // failure and not a second thing to charge the task for. Every
        // other ring-3 fault is charged straight to the task, which the
        // terminator kills without returning.
        let resolver = if is_user_data_fault(error_code) {
            tairix_arch_api::fault::user_fault_resolver()
        } else {
            None
        };
        // SAFETY: `saved` is the stub-provided address of the live 15-GPR
        // `SavedRegs` block on this kernel stack, and the gate above proved
        // the fault came from ring 3, so the callback runs on this task's
        // own trap control flow under the in-handler GS convention.
        unsafe {
            match resolver {
                Some(resolve) => {
                    if with_ring3_context(saved, rip, interrupted_rsp, |regs| {
                        resolve(faulting_addr, is_write(error_code), regs)
                    }) {
                        return;
                    }
                }
                None => {
                    if let Some(terminate) = tairix_arch_api::fault::user_fault_terminator() {
                        let _ = with_ring3_context(saved, rip, interrupted_rsp, |regs| {
                            terminate(rip, regs)
                        });
                    }
                }
            }
        }
    } else if let Some(fixup) = crate::uaccess::kernel_fixup_for(rip) {
        // A kernel-mode page fault inside the guarded user-copy window: the
        // validated copy's software proof was violated underneath it.
        // Rewrite the frame's RIP so the stub's `iretq` resumes at the
        // copy's fix-up and the copy returns an error to its caller instead
        // of taking the CPU down.
        // SAFETY: `rip_slot` is the stub-provided address of the live
        // interrupt frame's RIP word; the fix-up address is a real
        // instruction in this image.
        unsafe {
            *rip_slot = fixup;
        }
        return;
    }
    let fault = tairix_arch_api::fatal::KernelFault {
        syndrome: Some(exception_syndrome(PAGE_FAULT_VECTOR, error_code, from_user)),
        address: Some(faulting_addr),
        pc: rip,
        sp: crate::exceptions::interrupted_kernel_sp(PAGE_FAULT_VECTOR, from_user, interrupted_rsp),
    };
    if let Some(handler) = tairix_arch_api::fault::fault_handler() {
        handler(fault);
    }
    crate::panic::report_unclaimed_fault(&fault)
}

/// Run `call` on the faulting ring-3 register frame under the in-handler
/// GS convention — the one bracket every user-fault callback is invoked
/// through, from the dedicated `#PF` entry and from the ring-3 tail of
/// every other exception vector ([`crate::exceptions`]).
///
/// An interrupt gate taken from ring 3 does *not* swap GS, and every
/// user-fault callback may reschedule (park on filesystem I/O, or suspend a
/// killed task with an exit action), which requires the kernel GS base — so
/// the pair brackets exactly one call. A callback that suspends the task
/// never returns here and the park machinery owns the convention from that
/// point, exactly as on the LAPIC-timer preemption path; otherwise the user
/// GS is restored, so the `iretq` or the fatal tail proceeds under the same
/// GS it would have without a callback.
///
/// The register frame is built from the saved GPR block, the faulting
/// `rip`, and the interrupt frame's user `rsp`, and lives on this kernel
/// stack for the duration of the call, so the callback can record a
/// post-mortem crash record with a backtrace.
///
/// # Safety
///
/// * `saved` must point to the live 15-GPR
///   [`crate::interrupts::SavedRegs`] block the exception stub persisted on
///   this kernel stack.
/// * The exception must have been taken from ring 3, so each `swapgs` is
///   balanced against the gate that performed none.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn with_ring3_context<R>(
    saved: *const crate::interrupts::SavedRegs,
    rip: u64,
    user_rsp: u64,
    call: impl FnOnce(*const UserRegisterFrame) -> R,
) -> R {
    // SAFETY: `swapgs` is privileged and runs in ring 0 here; it touches
    // only the GS-base/`KERNEL_GS_BASE` swap, no memory or flags.
    unsafe {
        core::arch::asm!("swapgs", options(nomem, nostack, preserves_flags));
    }
    // SAFETY: the caller guarantees `saved` addresses the live saved block.
    let frame = unsafe { user_register_frame(saved, rip, user_rsp) };
    let out = call(&raw const frame);
    // SAFETY: as above — the matching swap restoring the user GS.
    unsafe {
        core::arch::asm!("swapgs", options(nomem, nostack, preserves_flags));
    }
    out
}

/// Build the faulting user register frame from the saved GPR block, the
/// faulting `rip`, and the interrupted user `rsp`.
///
/// `pc` is `rip`, `sp` is the user `rsp` from the CPU iret frame, and the
/// frame pointer is `rbp`; the System V AMD64 frame layout
/// ([`crate::backtrace::Backtracer::LAYOUT`]) drives the crash-path fp
/// walk, so the frame is marked `fp_valid`.
///
/// # Safety
///
/// `saved` must point to the live 15-GPR [`crate::interrupts::SavedRegs`]
/// block the `#PF` stub persisted on this kernel stack; it is read once
/// here and outlives the read.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
unsafe fn user_register_frame(
    saved: *const crate::interrupts::SavedRegs,
    rip: u64,
    user_rsp: u64,
) -> UserRegisterFrame {
    use tairix_arch_api::backtrace::RegisterSnapshot;
    // SAFETY: the caller guarantees `saved` addresses the live saved block.
    let s = unsafe { &*saved };
    let snapshot = RegisterSnapshot::new(rip, user_rsp, s.rbp)
        .with("rax", s.rax)
        .with("rbx", s.rbx)
        .with("rcx", s.rcx)
        .with("rdx", s.rdx)
        .with("rsi", s.rsi)
        .with("rdi", s.rdi)
        .with("rbp", s.rbp)
        .with("r8", s.r8)
        .with("r9", s.r9)
        .with("r10", s.r10)
        .with("r11", s.r11)
        .with("r12", s.r12)
        .with("r13", s.r13)
        .with("r14", s.r14)
        .with("r15", s.r15);
    UserRegisterFrame::new(snapshot, crate::backtrace::Backtracer::LAYOUT, true)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use tairix_arch_api::fatal::KernelFault;

    use super::*;

    #[test]
    fn syndrome_round_trips_the_vector_error_code_and_privilege() {
        let syndrome = exception_syndrome(13, 0x1234_5678, true);
        assert_eq!(syndrome_vector(syndrome), 13);
        assert_eq!(syndrome_error_code(syndrome), 0x1234_5678);
        assert!(syndrome_from_user(syndrome));

        let kernel = exception_syndrome(6, 0, false);
        assert_eq!(syndrome_vector(kernel), 6);
        assert_eq!(syndrome_error_code(kernel), 0);
        assert!(!syndrome_from_user(kernel));
    }

    #[test]
    fn a_page_fault_syndrome_still_decodes_through_the_error_code_helpers() {
        // The error code occupies the low half precisely so a handler that
        // provokes only page faults keeps reading the same bits.
        let code = PF_ERR_USER | PF_ERR_WRITE;
        let syndrome = exception_syndrome(PAGE_FAULT_VECTOR, code, true);
        assert_eq!(syndrome_vector(syndrome), PAGE_FAULT_VECTOR);
        assert!(is_not_present(syndrome));
        assert!(is_user(syndrome));
        assert!(is_write(syndrome));
        assert!(is_resolvable_user_fault(exception_syndrome(
            PAGE_FAULT_VECTOR,
            PF_ERR_USER,
            true
        )));
    }

    #[test]
    fn a_wider_than_32_bit_error_code_cannot_reach_the_vector_field() {
        // Fail closed on a malformed input rather than corrupt the vector
        // a reader decodes the record by.
        let syndrome = exception_syndrome(8, u64::MAX, false);
        assert_eq!(syndrome_vector(syndrome), 8);
        assert_eq!(syndrome_error_code(syndrome), 0xFFFF_FFFF);
        assert!(!syndrome_from_user(syndrome));
    }

    #[test]
    fn page_fault_vector_matches_intel_sdm() {
        // Intel SDM Vol 3A Table 6-1: #PF is vector 14.
        assert_eq!(PAGE_FAULT_VECTOR, 14);
    }

    #[test]
    fn error_code_bits_match_intel_sdm() {
        // Intel SDM Vol 3A §4.7 Figure 4-12.
        assert_eq!(PF_ERR_PRESENT, 1);
        assert_eq!(PF_ERR_WRITE, 2);
        assert_eq!(PF_ERR_USER, 4);
        assert_eq!(PF_ERR_RESERVED, 8);
        assert_eq!(PF_ERR_INSTR, 16);
    }

    #[test]
    fn not_present_is_the_cleared_present_bit() {
        // A bare not-present supervisor read is error code 0.
        assert!(is_not_present(0));
        // A not-present user write keeps P clear but sets W and U/S.
        assert!(is_not_present(PF_ERR_WRITE | PF_ERR_USER));
        // A protection violation has P set, so it is *not* not-present.
        assert!(!is_not_present(PF_ERR_PRESENT));
    }

    #[test]
    fn only_user_not_present_read_data_faults_are_resolvable() {
        // The demand-paged file-mapping shape: ring 3, not-present, read,
        // data access.
        assert!(is_resolvable_user_fault(PF_ERR_USER));
        // A kernel-mode fault is never offered.
        assert!(!is_resolvable_user_fault(0));
        // A protection violation cannot be fixed by residency.
        assert!(!is_resolvable_user_fault(PF_ERR_USER | PF_ERR_PRESENT));
        // A write to a read-only file mapping must kill the task, not
        // retry forever against a resident page.
        assert!(!is_resolvable_user_fault(PF_ERR_USER | PF_ERR_WRITE));
        // An instruction fetch is never file backing.
        assert!(!is_resolvable_user_fault(PF_ERR_USER | PF_ERR_INSTR));
    }

    #[test]
    fn user_data_faults_are_offered_reads_and_writes_alike() {
        // Regression (the M1 file-map vertical's `store` role): the offer
        // gate admits ring-3 reads *and* writes — a write is offered so
        // the resolver kills the faulting task; before this gate existed a
        // user store to a read-only mapping fell to the fatal path and
        // could halt the whole CPU.
        assert!(is_user_data_fault(PF_ERR_USER));
        assert!(is_user_data_fault(PF_ERR_USER | PF_ERR_WRITE));
        assert!(is_user_data_fault(
            PF_ERR_USER | PF_ERR_PRESENT | PF_ERR_WRITE
        ));
        // Kernel-mode faults and instruction fetches are never offered.
        assert!(!is_user_data_fault(0));
        assert!(!is_user_data_fault(PF_ERR_WRITE));
        assert!(!is_user_data_fault(PF_ERR_USER | PF_ERR_INSTR));
    }

    /// The terminator, not the resolver, owns a ring-3 instruction-fetch
    /// `#PF`: a wild jump is never file backing, so offering it to the
    /// resolver would leave it on the fatal path and park the CPU for one
    /// task's mistake.
    #[test]
    fn an_instruction_fetch_is_a_user_fault_the_resolver_never_sees() {
        let wild_jump = PF_ERR_USER | PF_ERR_INSTR;
        assert!(is_user(wild_jump));
        assert!(!is_user_data_fault(wild_jump));
        assert!(!is_resolvable_user_fault(wild_jump));
        // Kernel-mode instruction fetches stay the kernel's own.
        assert!(!is_user(PF_ERR_INSTR));
    }

    #[test]
    fn a_page_fault_is_decoded_with_the_address_cr2_gave() {
        let fault = KernelFault {
            syndrome: Some(exception_syndrome(PAGE_FAULT_VECTOR, PF_ERR_WRITE, false)),
            address: Some(0x1_0000_0000),
            pc: 0xffff_8000_0010_0000,
            sp: Some(0xffff_8000_0020_0000),
        };
        assert_eq!(
            std::format!("{}", Decoded(&fault)),
            "vector 14, error code 0x2, ring 0, CR2 0x100000000, RIP 0xffff800000100000"
        );
    }

    /// No vector but `#PF` supplies an address, so none is named: `CR2` would
    /// be whichever page fault happened last.
    #[test]
    fn an_exception_with_no_address_names_none() {
        let fault = KernelFault {
            syndrome: Some(exception_syndrome(6, 0, true)),
            address: None,
            pc: 0x40_1000,
            sp: None,
        };
        assert_eq!(
            std::format!("{}", Decoded(&fault)),
            "vector 6, error code 0x0, ring 3, RIP 0x401000"
        );
    }

    #[test]
    fn user_and_write_decode_independently() {
        assert!(is_user(PF_ERR_USER));
        assert!(!is_user(PF_ERR_WRITE));
        assert!(is_write(PF_ERR_WRITE));
        assert!(!is_write(PF_ERR_USER));
        // A user-mode not-present write sets both U/S and W.
        let code = PF_ERR_USER | PF_ERR_WRITE;
        assert!(is_user(code) && is_write(code) && is_not_present(code));
    }
}
