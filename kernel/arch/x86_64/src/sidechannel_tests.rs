//! Host pins for every side-channel mechanism the port declares applied.
//!
//! The entry and return paths are naked assembly no host test can run, so
//! each mechanism is checked against the sources they are built from: a
//! profile that says `Applied` fails here while any path lacks the code.
//! The needles live in this file, never in an inspected one, so a needle
//! cannot match itself.

extern crate std;

use std::collections::BTreeSet;
use std::format;
use std::string::String;
use std::vec::Vec;

use super::SideChannel;
use crate::interrupts::stub_align_tests::{literal, stub};

/// Every source of the port that carries assembly.
const ASM_SOURCES: &[(&str, &str)] = &[
    ("apic.rs", include_str!("apic.rs")),
    ("apic_timer.rs", include_str!("apic_timer.rs")),
    ("backtrace.rs", include_str!("backtrace.rs")),
    ("context.rs", include_str!("context.rs")),
    ("context_hal.rs", include_str!("context_hal.rs")),
    ("fault.rs", include_str!("fault.rs")),
    ("fpu.rs", include_str!("fpu.rs")),
    ("gdt.rs", include_str!("gdt.rs")),
    ("interrupts.rs", include_str!("interrupts.rs")),
    ("irq.rs", include_str!("irq.rs")),
    ("irqmask.rs", include_str!("irqmask.rs")),
    ("lib.rs", include_str!("lib.rs")),
    ("msr.rs", include_str!("msr.rs")),
    ("paging.rs", include_str!("paging.rs")),
    ("percpu.rs", include_str!("percpu.rs")),
    ("pio.rs", include_str!("pio.rs")),
    ("preempt.rs", include_str!("preempt.rs")),
    ("qemu_exit.rs", include_str!("qemu_exit.rs")),
    ("reset.rs", include_str!("reset.rs")),
    ("serial.rs", include_str!("serial.rs")),
    ("sidechannel.rs", include_str!("sidechannel.rs")),
    ("smp.rs", include_str!("smp.rs")),
    ("syscall_entry.rs", include_str!("syscall_entry.rs")),
    ("takeover.rs", include_str!("takeover.rs")),
    ("tlb_shootdown.rs", include_str!("tlb_shootdown.rs")),
    ("tsc.rs", include_str!("tsc.rs")),
    ("uaccess.rs", include_str!("uaccess.rs")),
    ("userentry.rs", include_str!("userentry.rs")),
    ("xstate.rs", include_str!("xstate.rs")),
    ("ap_trampoline.s", include_str!("ap_trampoline.s")),
    ("boot.s", include_str!("boot.s")),
    ("context.s", include_str!("context.s")),
    ("external_irq.s", include_str!("external_irq.s")),
    ("takeover.s", include_str!("takeover.s")),
];

/// Whether `name` is an assembly source rather than a Rust one.
fn is_assembly(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension == "s")
}

/// The instructions of `source`, whitespace-normalised: every string
/// literal line of a Rust source, every non-comment line of an assembly one.
fn instructions(name: &str, source: &str) -> Vec<String> {
    if is_assembly(name) {
        source
            .lines()
            .filter_map(|line| {
                let code = line.split('#').next()?.split("//").next()?;
                let text = code.split_whitespace().collect::<Vec<_>>().join(" ");
                (!text.is_empty()).then_some(text)
            })
            .collect()
    } else {
        source.lines().filter_map(literal).collect()
    }
}

/// Every return-to-user instruction in the port, as `(source, index)` into
/// that source's [`instructions`].
fn user_returns() -> Vec<(&'static str, Vec<String>, usize)> {
    let mut out = Vec::new();
    for &(name, source) in ASM_SOURCES {
        let lines = instructions(name, source);
        let sites: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, i)| matches!(i.as_str(), "iretq" | "sysretq"))
            .map(|(at, _)| at)
            .collect();
        for at in sites {
            out.push((name, lines.clone(), at));
        }
    }
    out
}

#[test]
fn the_scan_finds_each_known_return_to_user() {
    let found: BTreeSet<(&str, String)> = user_returns()
        .into_iter()
        .map(|(name, lines, at)| (name, lines[at].clone()))
        .collect();
    for known in [
        ("sidechannel.rs", "iretq"),
        ("syscall_entry.rs", "sysretq"),
        ("userentry.rs", "iretq"),
    ] {
        assert!(
            found.contains(&(known.0, String::from(known.1))),
            "no `{}` found in {}",
            known.1,
            known.0
        );
    }
}

/// `verw` is the last instruction before each return that reaches ring 3:
/// unconditionally before a return that always does, and behind the saved
/// `CS` test in the template every interrupt stub returns through.
#[test]
fn every_return_to_user_clears_the_buffers_last() {
    if !SideChannel::declared_profile()
        .user_return_buffer_clear
        .is_applied()
    {
        return;
    }
    for (name, lines, at) in user_returns() {
        let clear = match lines[..at].split_last() {
            Some((label, rest)) if label.ends_with(':') => {
                let label = label.trim_end_matches(':');
                let (clear, rest) = rest.split_last().expect("an instruction before the label");
                let (jump, rest) = rest.split_last().expect("a branch to the label");
                assert_eq!(
                    jump,
                    &format!("jz {label}f"),
                    "{name}: only a return to ring 0 may skip the clear"
                );
                let (test, _) = rest.split_last().expect("the saved CS test");
                assert_eq!(
                    test, "testb $3, 8(%rsp)",
                    "{name}: the branch tests the saved CS"
                );
                clear
            }
            Some((clear, _)) => clear,
            None => panic!("{name}: a return with nothing before it"),
        };
        assert!(
            clear.starts_with("verw ") && clear.contains("{verw_sel}"),
            "{name}: `{}` returns to user without clearing the buffers first",
            lines[at]
        );
    }
}

#[test]
fn every_return_to_user_is_followed_by_a_speculation_trap() {
    if !SideChannel::declared_profile()
        .user_return_speculation_barrier
        .is_applied()
    {
        return;
    }
    for (name, lines, at) in user_returns() {
        assert_eq!(
            lines.get(at + 1).map(String::as_str),
            Some("int3"),
            "{name}: straight-line speculation runs on past `{}`",
            lines[at]
        );
    }
}

/// Each general-purpose register spelling, with the register it names.
const GPRS: &[(&str, &str)] = &[
    ("rax", "rax"),
    ("eax", "rax"),
    ("rbx", "rbx"),
    ("ebx", "rbx"),
    ("rcx", "rcx"),
    ("ecx", "rcx"),
    ("rdx", "rdx"),
    ("edx", "rdx"),
    ("rsi", "rsi"),
    ("esi", "rsi"),
    ("rdi", "rdi"),
    ("edi", "rdi"),
    ("rbp", "rbp"),
    ("ebp", "rbp"),
    ("r8", "r8"),
    ("r8d", "r8"),
    ("r9", "r9"),
    ("r9d", "r9"),
    ("r10", "r10"),
    ("r10d", "r10"),
    ("r11", "r11"),
    ("r11d", "r11"),
    ("r12", "r12"),
    ("r12d", "r12"),
    ("r13", "r13"),
    ("r13d", "r13"),
    ("r14", "r14"),
    ("r14d", "r14"),
    ("r15", "r15"),
    ("r15d", "r15"),
];

/// The System V AMD64 integer argument registers.
const SYSV_ARGUMENTS: [&str; 6] = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];

/// The general-purpose register an AT&T operand names, if it names one.
fn gpr(operand: &str) -> Option<&'static str> {
    let name = operand.trim_end_matches(',').strip_prefix('%')?;
    GPRS.iter()
        .find(|(spelling, _)| *spelling == name)
        .map(|(_, register)| *register)
}

/// Between a stub's last register save and its dispatcher call: the
/// registers it zeroes, and the ones it loads an argument into.
fn zeroed_and_passed(body: &[String]) -> (BTreeSet<&'static str>, BTreeSet<&'static str>) {
    let call = body
        .iter()
        .position(|i| i.starts_with("call {"))
        .expect("a dispatcher call");
    let last_save = body[..call]
        .iter()
        .rposition(|i| i.starts_with("pushq "))
        .expect("register saves before the call");
    let mut zeroed = BTreeSet::new();
    let mut passed = BTreeSet::new();
    for instruction in &body[last_save + 1..call] {
        let words: Vec<&str> = instruction.split(' ').collect();
        match words.as_slice() {
            ["xorl", a, b] if gpr(a).is_some() && gpr(a) == gpr(b) => {
                zeroed.extend(gpr(a));
            }
            ["movq" | "mov" | "leaq", _, destination] => passed.extend(gpr(destination)),
            _ => {}
        }
    }
    (zeroed, passed)
}

/// Once a stub has saved the interrupted registers, every one it does not
/// pass an argument in is zeroed before Rust runs, and it passes arguments
/// only in argument registers — so no value the user chose rides into the
/// kernel in a register a speculative gadget could consume.
#[test]
fn every_entry_stub_zeroes_what_it_saved_before_rust_runs() {
    if !SideChannel::declared_profile()
        .user_entry_hardening
        .is_applied()
    {
        return;
    }
    let all: BTreeSet<&str> = GPRS.iter().map(|(_, register)| *register).collect();
    for anchor in [
        "fn syscall_entry_stub()",
        "macro_rules! define_isr",
        "macro_rules! exception_isr_body",
        "fn tairix_arch_x86_64_isr_default()",
        "fn tairix_arch_x86_64_external_irq_common()",
        "fn page_fault_isr()",
    ] {
        let (zeroed, passed) = zeroed_and_passed(&stub(anchor, &[]));
        assert!(
            passed.iter().all(|r| SYSV_ARGUMENTS.contains(r)),
            "{anchor}: passes {passed:?}, not only argument registers"
        );
        assert!(
            zeroed.is_disjoint(&passed),
            "{anchor}: zeroes an argument it passes"
        );
        let covered: BTreeSet<&str> = zeroed.union(&passed).copied().collect();
        assert_eq!(
            covered,
            all,
            "{anchor}: leaves {:?} holding interrupted values",
            all.difference(&covered).collect::<Vec<_>>()
        );
    }
}

/// The body of the function whose header contains `header`.
fn function_body<'a>(source: &'a str, header: &str) -> &'a str {
    let start = source
        .find(header)
        .unwrap_or_else(|| panic!("no `{header}`"));
    let rest = &source[start..];
    &rest[..rest.find("\n}\n").expect("the function's closing brace")]
}

/// Each GS swap whose execution depends on the interrupted privilege opens
/// with a fence, so no gs-relative access runs under a mispredicted test.
#[test]
fn every_privilege_dependent_gs_swap_is_fenced() {
    if !SideChannel::declared_profile()
        .user_entry_hardening
        .is_applied()
    {
        return;
    }
    for (source, header) in [
        (
            include_str!("preempt.rs"),
            "unsafe fn preempt_ring3_if_pending(",
        ),
        (include_str!("fault.rs"), "unsafe fn with_ring3_context<"),
    ] {
        let body = function_body(source, header);
        let swap = body
            .find("asm!(\"swapgs\"")
            .unwrap_or_else(|| panic!("{header}: no swap"));
        assert!(
            body[swap..].starts_with("asm!(\"swapgs\", \"lfence\""),
            "{header}: the privilege-dependent swap is not fenced"
        );
    }
}
