//! Host unit tests for the atomicity of this port's two `eret` sequences.
//!
//! `ELR_EL1` and `SPSR_EL1` are single-copy registers holding the state an
//! `eret` consumes. An exception taken once they hold that state overwrites
//! both in hardware, and the nested handler's own return restores *its*
//! saved pair — so the interrupted sequence then `eret`s to the nested
//! handler's return address in the nested handler's PSTATE. In the trap
//! trampoline that resumes the epilogue at EL1 with the frame already
//! popped, walking `sp` one frame per turn off the kernel stack until the
//! loads fault, and then faulting recursively with `DAIF` masked: a silent,
//! unrecoverable wedge reported as a bare hard lockup. The debug watchdog's
//! Group-0/FIQ cadence is a live source of exactly that exception, because
//! the syscall/fault handler runs with `DAIF.F` clear so a wedged core can
//! be sampled (`plans/WATCHDOG.md`).
//!
//! Both sequences therefore mask every asynchronous exception before they
//! program the return state. Neither can be executed on the host, and the
//! window is a race no target test can reliably enter, so the ordering is
//! pinned here against the two sources — the assembly carve-out
//! `vectors.s` and the inline-`asm!` user entry. The needles live in this
//! file rather than in the inspected ones, so a needle can never match
//! itself and pass a test whose subject has lost its mask.

use std::string::String;
use std::vec::Vec;

/// Collapse each line's internal whitespace to single spaces so an
/// assertion names an instruction without also pinning the source's column
/// alignment. The exception path's other source pins share it.
pub(super) fn instruction_lines(src: &str) -> Vec<String> {
    src.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The index of the single line equal to `instruction`.
///
/// Requiring exactly one occurrence is what makes an ordering assertion
/// meaningful: a second copy of a pinned line elsewhere in the file would
/// leave the order it is compared in ambiguous.
pub(super) fn line_of(lines: &[String], instruction: &str) -> usize {
    let mut hits = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.as_str() == instruction);
    let Some((index, _)) = hits.next() else {
        panic!("no `{instruction}` line in the inspected source");
    };
    assert!(
        hits.next().is_none(),
        "`{instruction}` must appear exactly once in the inspected source",
    );
    index
}

/// Panic if any instruction in `lines` re-enables an asynchronous
/// exception, so the masked window reaches the `eret` intact.
fn assert_nothing_unmasks(lines: &[String]) {
    for line in lines {
        if line.starts_with("//") {
            continue;
        }
        let lowered = line.to_ascii_lowercase();
        assert!(
            !lowered.contains("daifclr") && !lowered.starts_with("msr daif,"),
            "the masked window must reach the `eret` intact, but it runs `{line}`",
        );
    }
}

#[test]
fn the_trap_epilogue_masks_asynchronous_exceptions_before_the_return_state() {
    let lines = instruction_lines(include_str!("vectors.s"));
    let handler = line_of(&lines, "bl tairix_aarch64_trap_handler");
    let mask = line_of(&lines, "msr DAIFSet, #0xf");
    let elr = line_of(&lines, "msr ELR_EL1, x2");
    let spsr = line_of(&lines, "msr SPSR_EL1, x3");
    let eret = line_of(&lines, "eret");

    assert!(
        handler < mask,
        "the mask belongs to the return path, after the handler call",
    );
    assert!(
        mask < elr && mask < spsr,
        "the return state must be programmed with exceptions already masked",
    );
    assert!(
        elr < eret && spsr < eret,
        "the return state is programmed before the `eret` consumes it",
    );
    assert_nothing_unmasks(&lines[mask..eret]);
}

#[test]
fn the_user_entry_eret_masks_asynchronous_exceptions_before_the_return_state() {
    let lines = instruction_lines(include_str!("userentry.rs"));
    let mask = line_of(&lines, "\"msr DAIFSet, #0xf\",");
    let elr = line_of(&lines, "\"msr ELR_EL1, {entry}\",");
    let spsr = line_of(&lines, "\"msr SPSR_EL1, {spsr}\",");
    let eret = line_of(&lines, "\"eret\",");

    assert!(
        mask < elr && mask < spsr,
        "the EL0 entry state must be programmed with exceptions already masked",
    );
    assert!(
        elr < eret && spsr < eret,
        "the entry state is programmed before the `eret` consumes it",
    );
    assert_nothing_unmasks(&lines[mask..eret]);
}

/// The psABI thread pointer must be saved on entry and restored on the way
/// out, at the same frame offset, so several threads of one process do not
/// share one thread-local storage base (`plans/THREADS.md` decision 7).
#[test]
fn the_trap_frame_carries_the_thread_pointer_across_a_context_switch() {
    let lines = instruction_lines(include_str!("vectors.s"));
    let handler = line_of(&lines, "bl tairix_aarch64_trap_handler");
    let save = line_of(&lines, "mrs x2, TPIDR_EL0");
    let store = line_of(&lines, "str x2, [sp, #800]");
    let load = line_of(&lines, "ldr x2, [sp, #800]");
    let restore = line_of(&lines, "msr TPIDR_EL0, x2");
    let eret = line_of(&lines, "eret");

    assert!(
        save < handler && store < handler,
        "the thread pointer belongs to the entry path, before the handler call",
    );
    assert_eq!(save + 1, store, "the read is stored straight away");
    assert!(
        handler < load,
        "the reload belongs to the return path, after the handler call",
    );
    assert_eq!(
        load + 1,
        restore,
        "the loaded word is written straight back"
    );
    assert!(
        restore < eret,
        "the thread pointer is restored before the `eret` resumes the thread",
    );
}

/// A freshly entered thread gets its own thread pointer rather than whatever
/// the previous occupant of the CPU left in the register.
#[test]
fn the_user_entry_seeds_the_thread_pointer() {
    let lines = instruction_lines(include_str!("userentry.rs"));
    let mask = line_of(&lines, "\"msr DAIFSet, #0xf\",");
    let tls = line_of(&lines, "\"msr TPIDR_EL0, {tls}\",");
    let eret = line_of(&lines, "\"eret\",");

    assert!(mask < tls, "programmed with exceptions already masked");
    assert!(tls < eret, "seeded before the `eret` consumes it");
}

/// The handler never runs under the interrupted code's rounding mode or
/// flush-to-zero: the entry resets `FPCR` after saving it, before the call,
/// and the return restores the saved value.
#[test]
fn the_trap_handler_runs_under_the_kernel_fp_environment() {
    let lines = instruction_lines(include_str!("vectors.s"));
    let read = line_of(&lines, "mrs x2, FPCR");
    let reset = line_of(&lines, "msr FPCR, xzr");
    let handler = line_of(&lines, "bl tairix_aarch64_trap_handler");
    let restore = line_of(&lines, "msr FPCR, x2");
    assert!(
        read < reset && reset < handler,
        "reset after the save, before Rust"
    );
    assert!(
        handler < restore,
        "the interrupted value comes back on return"
    );
}

/// Every CPU enters the kernel's FP environment when it enables FP: the trap
/// control is written whole, so no firmware-left SVE or SME enable survives,
/// and `FPCR` is reset only once FP no longer traps.
#[test]
fn enabling_fp_writes_the_trap_control_whole_and_resets_fpcr() {
    let lines = instruction_lines(include_str!("kernel_arch.rs"));
    let cpacr = line_of(&lines, "\"msr CPACR_EL1, {cpacr}\",");
    let fpcr = line_of(&lines, "\"msr FPCR, xzr\",");
    assert!(cpacr < fpcr, "FPCR is written once FP no longer traps");
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("mrs") && line.contains("CPACR_EL1")),
        "the trap control is written whole, never merged with what it held"
    );
}

/// Nothing the kernel left in a register reaches a new process: every
/// general-purpose register but `x0` and every vector register is zeroed,
/// with the floating-point control and status, before the `eret`.
#[test]
fn the_user_entry_leaves_no_kernel_register_state() {
    let lines = instruction_lines(include_str!("userentry.rs"));
    let mask = line_of(&lines, "\"msr DAIFSet, #0xf\",");
    let eret = line_of(&lines, "\"eret\",");
    let mut needles: Vec<String> = (1..=30)
        .map(|i| std::format!("\"mov x{i}, xzr\","))
        .collect();
    needles.extend((0..32).map(|i| std::format!("\"movi v{i}.2d, #0\",")));
    needles.push("\"msr FPCR, xzr\",".into());
    needles.push("\"msr FPSR, xzr\",".into());
    for needle in &needles {
        let at = line_of(&lines, needle);
        assert!(
            mask < at && at < eret,
            "{needle} runs inside the entry sequence"
        );
    }
}
