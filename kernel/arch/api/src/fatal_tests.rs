extern crate std;

use std::string::String;

use super::{
    enter, enter_without_atomics, format_hex_word, reset_for_tests, Entry, FaultWords, KernelFault,
    Processor, Reporter, HEX_WORD_LEN, KERNEL_FAULT, KERNEL_PANIC,
};
use crate::backtrace::BootStackGuard;

const PORT: Reporter = Reporter { port: "aarch64" };

/// The instruction abort a corrupted vtable slot raised in the
/// figure-determinism vertical: a branch to the `f64` bit pattern of `1.0`.
const VTABLE_BRANCH: KernelFault = KernelFault {
    syndrome: Some(0x8600_0000),
    address: Some(0x3ff0_0000_0000_0000),
    pc: 0x3ff0_0000_0000_0000,
    sp: Some(0x4008_1f40),
};

fn fault_report(entry: Entry, fault: &KernelFault, guard: Option<BootStackGuard>) -> String {
    let mut out = String::new();
    PORT.fault(
        &mut out,
        entry,
        Processor::Cpu(0),
        fault,
        format_args!("ESR_EL1 0x86000000 (EC 0x21)"),
        || guard,
    );
    out
}

fn last_line(report: &str) -> &str {
    report.lines().last().unwrap_or("")
}

#[test]
fn a_fault_report_ends_with_its_record_naming_every_word() {
    let report = fault_report(Entry::Report, &VTABLE_BRANCH, Some(BootStackGuard::Intact));
    assert_eq!(
        last_line(&report),
        "[ERROR] id=4011 fatal kernel fault cpu=0 syndrome=0x0000000086000000 \
         fault_addr=0x3ff0000000000000 fault_pc=0x3ff0000000000000 \
         fault_sp=0x0000000040081f40 boot_stack_guard=intact"
    );
    assert!(report.ends_with('\n'), "nothing trails the record");
}

/// A word the CPU did not give is recorded as absent, never as a zero or a
/// stale register that reads as a value.
#[test]
fn a_word_the_cpu_did_not_give_is_null() {
    let fiq = KernelFault {
        syndrome: None,
        address: None,
        pc: 0x4008_0000,
        sp: None,
    };
    let report = fault_report(Entry::Report, &fiq, None);
    assert_eq!(
        last_line(&report),
        "[ERROR] id=4011 fatal kernel fault cpu=0 syndrome=null fault_addr=null \
         fault_pc=0x0000000040080000 fault_sp=null"
    );
}

#[test]
fn a_fault_report_says_why_in_prose_before_its_record() {
    let report = fault_report(Entry::Report, &VTABLE_BRANCH, None);
    let mut lines = report.lines().skip_while(|line| line.is_empty());
    assert_eq!(
        lines.next(),
        Some("==================== TAIRiX KERNEL FAULT ====================")
    );
    assert_eq!(
        lines.next(),
        Some(
            "[tairix-kernel] aarch64 exception on CPU 0 with no fault handler installed: \
             ESR_EL1 0x86000000 (EC 0x21)"
        )
    );
}

#[test]
fn a_report_with_no_guard_carries_no_guard_field() {
    let report = fault_report(Entry::Report, &VTABLE_BRANCH, None);
    assert!(!report.contains("boot-stack guard"));
    assert!(!last_line(&report).contains("boot_stack_guard"));
}

/// An overrun still in progress names both the verdict and its depth, in the
/// keys the kernel's own post-mortem uses.
#[test]
fn an_overrun_names_the_verdict_and_its_depth() {
    let report = fault_report(
        Entry::Report,
        &VTABLE_BRANCH,
        Some(BootStackGuard::BelowStack {
            sp: 0x4000_0f00,
            bytes: 0x100,
        }),
    );
    assert!(last_line(&report)
        .ends_with("boot_stack_guard=sp_below_stack boot_stack_overrun_bytes=0x0000000000000100"));
    assert!(report.contains("boot-stack guard: OVERRUN - sp 0x40000f00"));
}

#[test]
fn a_disturbed_canary_is_named() {
    let report = fault_report(
        Entry::Report,
        &VTABLE_BRANCH,
        Some(BootStackGuard::Disturbed),
    );
    assert!(last_line(&report).ends_with("boot_stack_guard=disturbed"));
}

#[test]
fn a_panic_report_ends_with_its_record_naming_where() {
    let here = core::panic::Location::caller();
    let mut report = String::new();
    PORT.panic(
        &mut report,
        Entry::Report,
        Processor::Cpu(2),
        &"the grid did not place",
        Some(here),
        || None,
    );
    assert!(report.contains("[tairix-kernel] aarch64 panic on CPU 2: the grid did not place"));
    let expected = std::format!(
        "[ERROR] id=4010 kernel panic cpu=2 file={} line={} column={}",
        here.file(),
        here.line(),
        here.column()
    );
    assert_eq!(last_line(&report), expected);
}

#[test]
fn a_panic_with_no_location_says_so() {
    let mut report = String::new();
    Reporter { port: "riscv64" }.panic(
        &mut report,
        Entry::Report,
        Processor::Cpu(0),
        &"lost",
        None,
        || None,
    );
    assert!(report.contains("CPU 0 halted"));
    assert_eq!(
        last_line(&report),
        "[ERROR] id=4010 kernel panic cpu=0 file=<unknown> line=0 column=0"
    );
}

/// A processor the port could not map to a dense id is named by its hardware
/// identity under that identity's own key, so it never reads as a `cpu`.
#[test]
fn an_unmapped_processor_is_named_by_its_hardware_identity() {
    let mut report = String::new();
    Reporter { port: "riscv64" }.panic(
        &mut report,
        Entry::Report,
        Processor::Hardware { key: "hart", id: 3 },
        &"lost",
        None,
        || None,
    );
    assert!(report.contains("riscv64 panic on hart 3: lost"));
    assert!(report.contains("hart 3 halted"));
    assert!(last_line(&report).starts_with("[ERROR] id=4010 kernel panic hart=3 file="));
    assert!(!last_line(&report).contains("cpu="));
}

/// A re-entered report writes one bare record under its cause's id — which
/// the harness ends a run on — and never touches the machinery that failed.
#[test]
fn a_nested_entry_writes_only_the_bare_record() {
    let mut report = String::new();
    PORT.panic(
        &mut report,
        Entry::Nested,
        Processor::Cpu(1),
        &"unused",
        None,
        || unreachable!("a nested entry must not assess the guard"),
    );
    assert_eq!(
        report,
        "[ERROR] id=4010 kernel panic (nested — re-entered the fatal-report path) cpu=1\n"
    );

    let mut fault = String::new();
    PORT.fault(
        &mut fault,
        Entry::Nested,
        Processor::Cpu(1),
        &VTABLE_BRANCH,
        format_args!("unused"),
        || unreachable!("a nested entry must not assess the guard"),
    );
    assert_eq!(
        fault,
        "[ERROR] id=4011 fatal kernel fault (nested — re-entered the fatal-report path) cpu=1\n"
    );
}

#[test]
fn a_silent_entry_writes_nothing() {
    let mut report = String::new();
    PORT.panic(
        &mut report,
        Entry::Silent,
        Processor::Cpu(1),
        &"unused",
        None,
        || unreachable!("a silent entry must not assess the guard"),
    );
    PORT.fault(
        &mut report,
        Entry::Silent,
        Processor::Cpu(1),
        &VTABLE_BRANCH,
        format_args!("unused"),
        || unreachable!("a silent entry must not assess the guard"),
    );
    assert!(report.is_empty());
}

/// The one test that drives the process-wide latch, so no two race on it.
#[test]
fn the_latch_grants_one_report_then_one_nested_record_then_silence() {
    reset_for_tests();
    assert_eq!(enter(), Entry::Report);
    assert_eq!(enter(), Entry::Nested);
    assert_eq!(enter(), Entry::Silent);
    assert_eq!(enter(), Entry::Silent);

    // The atomics-free entry reads and advances the same count, so a boot that
    // mixes the two still reports once.
    reset_for_tests();
    assert_eq!(enter_without_atomics(), Entry::Report);
    assert_eq!(enter(), Entry::Nested);
    assert_eq!(enter_without_atomics(), Entry::Silent);

    // The count saturates rather than wrapping back to a first entry.
    super::ENTRIES.store(u32::MAX, core::sync::atomic::Ordering::Relaxed);
    assert_eq!(enter(), Entry::Silent);
    assert_eq!(enter_without_atomics(), Entry::Silent);
    reset_for_tests();
}

#[test]
fn the_two_records_are_distinct() {
    assert_ne!(KERNEL_PANIC.id, KERNEL_FAULT.id);
    assert_ne!(KERNEL_PANIC.message, KERNEL_FAULT.message);
    assert_ne!(KERNEL_PANIC.nested, KERNEL_FAULT.nested);
}

#[test]
fn a_fault_displays_as_the_records_fields() {
    assert_eq!(
        std::format!("{VTABLE_BRANCH}"),
        "syndrome=0x0000000086000000 fault_addr=0x3ff0000000000000 \
         fault_pc=0x3ff0000000000000 fault_sp=0x0000000040081f40"
    );
    let bare = KernelFault {
        syndrome: None,
        address: None,
        pc: 1,
        sp: None,
    };
    assert_eq!(
        std::format!("{bare}"),
        "syndrome=null fault_addr=null fault_pc=0x0000000000000001 fault_sp=null"
    );
}

#[test]
fn the_fields_name_each_word_under_its_record_key() {
    let mut words = FaultWords::new();
    let keys: std::vec::Vec<_> = VTABLE_BRANCH
        .fields(&mut words)
        .iter()
        .map(|field| field.key)
        .collect();
    assert_eq!(keys, ["syndrome", "fault_addr", "fault_pc", "fault_sp"]);
}

#[test]
fn a_word_is_spelled_fixed_width_lowercase_hex() {
    for (value, spelled) in [
        (0, "0x0000000000000000"),
        (0xdead_beef, "0x00000000deadbeef"),
        (0xffff_8000_0000_1111, "0xffff800000001111"),
        (u64::MAX, "0xffffffffffffffff"),
    ] {
        let mut buf = [0u8; HEX_WORD_LEN];
        assert_eq!(format_hex_word(value, &mut buf), spelled);
    }
}
