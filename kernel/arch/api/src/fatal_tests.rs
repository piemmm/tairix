extern crate std;

use std::string::{String, ToString};
use std::vec::Vec;

use core::mem::{offset_of, size_of};
use core::panic::Location;
use std::format;

use super::{
    decode_kept, enter, enter_without_atomics, file_tail, format_hex_word, kept_image,
    reset_for_tests, Cause, CauseWords, Entry, Kept, KernelFault, Processor, Reporter, StackCheck,
    StackCorruption, FATAL_RECORDS, FILE_TAIL, HEX_WORD_LEN, KEPT_BYTES,
};
use crate::backtrace::BootStackGuard;

const PORT: Reporter = Reporter { port: "aarch64" };

/// A task's stack found with its guard canary disturbed.
const OVERRUN: StackCorruption = StackCorruption {
    check: StackCheck::Guard,
    task: 0x2a,
    task_sp: 0x4410_7f00,
    base: 0x4410_0000,
    top: 0x4410_8000,
};

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
fn a_corrupt_stack_report_ends_with_its_record_naming_the_stack() {
    let mut report = String::new();
    PORT.stack_corruption(
        &mut report,
        Entry::Report,
        Processor::Cpu(3),
        &OVERRUN,
        || Some(BootStackGuard::Intact),
    );
    let mut lines = report.lines().skip_while(|line| line.is_empty());
    assert_eq!(
        lines.next(),
        Some("==================== TAIRiX KERNEL STACK CORRUPTION ====================")
    );
    assert_eq!(
        lines.next(),
        Some("[tairix-kernel] aarch64 task 0x2a failed its kernel-stack guard check on CPU 3")
    );
    assert_eq!(
        last_line(&report),
        "[ERROR] id=4012 kernel stack corrupt cpu=3 check=guard task=0x000000000000002a \
         task_sp=0x0000000044107f00 stack_base=0x0000000044100000 \
         stack_top=0x0000000044108000 boot_stack_guard=intact"
    );
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
        Entry::Nested { first: None },
        Processor::Cpu(1),
        &"unused",
        None,
        || unreachable!("a nested entry must not assess the guard"),
    );
    assert_eq!(
        report,
        "[ERROR] id=4010 kernel panic (nested — re-entered the fatal-report path) cpu=1 \
         file=null line=0 column=0 first=null\n"
    );

    let mut fault = String::new();
    PORT.fault(
        &mut fault,
        Entry::Nested { first: None },
        Processor::Cpu(1),
        &VTABLE_BRANCH,
        format_args!("unused"),
        || unreachable!("a nested entry must not assess the guard"),
    );
    assert_eq!(
        fault,
        "[ERROR] id=4011 fatal kernel fault (nested — re-entered the fatal-report path) cpu=1 \
         syndrome=0x0000000086000000 fault_addr=0x3ff0000000000000 \
         fault_pc=0x3ff0000000000000 fault_sp=0x0000000040081f40 first=null\n"
    );

    let mut stack = String::new();
    PORT.stack_corruption(
        &mut stack,
        Entry::Nested { first: None },
        Processor::Cpu(1),
        &OVERRUN,
        || unreachable!("a nested entry must not assess the guard"),
    );
    assert_eq!(
        stack,
        "[ERROR] id=4012 kernel stack corrupt (nested — re-entered the fatal-report path) cpu=1 \
         check=guard task=0x000000000000002a task_sp=0x0000000044107f00 \
         stack_base=0x0000000044100000 stack_top=0x0000000044108000 first=null\n"
    );
}

/// A report that died inside itself leaves the bare record as the only line
/// about the machine's death, so that record names the cause the first entry
/// was taken for: a fault's four words, a panic's place, or a stack's check
/// and bounds.
#[test]
fn a_bare_record_names_the_cause_the_first_entry_was_taken_for() {
    let mut after_a_fault = String::new();
    PORT.panic(
        &mut after_a_fault,
        Entry::Nested {
            first: Some(Cause::Fault(VTABLE_BRANCH)),
        },
        Processor::Cpu(0),
        &"unused",
        None,
        || None,
    );
    assert!(
        after_a_fault.ends_with(
            " first=fault first_syndrome=0x0000000086000000 \
             first_fault_addr=0x3ff0000000000000 first_fault_pc=0x3ff0000000000000 \
             first_fault_sp=0x0000000040081f40\n"
        ),
        "{after_a_fault}"
    );

    let raised = Location::caller();
    let mut after_a_panic = String::new();
    PORT.fault(
        &mut after_a_panic,
        Entry::Nested {
            first: Some(Cause::panic(Some(raised))),
        },
        Processor::Cpu(0),
        &VTABLE_BRANCH,
        format_args!("unused"),
        || None,
    );
    assert!(
        after_a_panic.ends_with(&format!(
            " first=panic first_file={} first_line={} first_column={}\n",
            raised.file(),
            raised.line(),
            raised.column()
        )),
        "{after_a_panic}"
    );

    let mut after_a_stack = String::new();
    PORT.panic(
        &mut after_a_stack,
        Entry::Nested {
            first: Some(Cause::Stack(StackCorruption {
                check: StackCheck::Suspension,
                ..OVERRUN
            })),
        },
        Processor::Cpu(0),
        &"unused",
        None,
        || None,
    );
    assert!(
        after_a_stack.ends_with(
            " first=stack first_check=suspension first_task=0x000000000000002a \
             first_task_sp=0x0000000044107f00 first_stack_base=0x0000000044100000 \
             first_stack_top=0x0000000044108000\n"
        ),
        "{after_a_stack}"
    );
}

/// A path longer than the tail keeps its end, where the file's name is, cut
/// where a character begins.
#[test]
fn a_long_path_keeps_its_end_whole_characters_only() {
    let short = "src/fatal.rs";
    let (bytes, len) = file_tail(short);
    assert_eq!(&bytes[..usize::from(len)], short.as_bytes());

    let long = format!("{}é/src/fatal.rs", "d".repeat(FILE_TAIL));
    let (bytes, len) = file_tail(&long);
    let kept = core::str::from_utf8(&bytes[..usize::from(len)]).expect("whole characters");
    assert!(long.ends_with(kept));
    assert!(kept.len() <= FILE_TAIL);
    assert!(kept.ends_with("/src/fatal.rs"));
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
    PORT.stack_corruption(
        &mut report,
        Entry::Silent,
        Processor::Cpu(1),
        &OVERRUN,
        || unreachable!("a silent entry must not assess the guard"),
    );
    assert!(report.is_empty());
}

/// The one test that drives the process-wide latch, so no two race on it.
#[test]
fn the_latch_grants_one_report_then_one_nested_record_then_silence() {
    let fault = Cause::Fault(VTABLE_BRANCH);
    let panic = Cause::panic(Some(Location::caller()));
    reset_for_tests();
    assert_eq!(enter(&fault), Entry::Report);
    assert_eq!(
        enter(&panic),
        Entry::Nested { first: Some(fault) },
        "the second entry is told what the first was taken for"
    );
    assert_eq!(enter(&panic), Entry::Silent);
    assert_eq!(enter(&fault), Entry::Silent);

    // The atomics-free entry reads and advances the same count and keeps the
    // same cause, so a boot that mixes the two still reports once.
    reset_for_tests();
    assert_eq!(enter_without_atomics(&panic), Entry::Report);
    assert_eq!(enter(&fault), Entry::Nested { first: Some(panic) });
    assert_eq!(enter_without_atomics(&fault), Entry::Silent);

    // A word the CPU did not give is still absent when the cause is read back.
    reset_for_tests();
    let fiq = Cause::Fault(KernelFault {
        syndrome: None,
        address: None,
        pc: 0x4008_0000,
        sp: None,
    });
    assert_eq!(enter(&fiq), Entry::Report);
    assert_eq!(enter(&fault), Entry::Nested { first: Some(fiq) });

    // A corrupt stack is kept with the check that found it.
    for check in [StackCheck::Guard, StackCheck::Suspension] {
        reset_for_tests();
        let stack = Cause::Stack(StackCorruption { check, ..OVERRUN });
        assert_eq!(enter(&stack), Entry::Report);
        assert_eq!(enter(&fault), Entry::Nested { first: Some(stack) });
    }

    // The count saturates rather than wrapping back to a first entry.
    super::ENTRIES.store(u32::MAX, core::sync::atomic::Ordering::Relaxed);
    assert_eq!(enter(&fault), Entry::Silent);
    assert_eq!(enter_without_atomics(&fault), Entry::Silent);
    reset_for_tests();
}

#[test]
fn the_records_are_distinct() {
    for (at, record) in FATAL_RECORDS.iter().enumerate() {
        for other in &FATAL_RECORDS[at + 1..] {
            assert_ne!(record.id, other.id);
            assert_ne!(record.message, other.message);
            assert_ne!(record.nested, other.nested);
        }
    }
}

#[test]
fn a_cause_displays_its_kind_then_its_record_fields() {
    assert_eq!(
        Cause::Stack(OVERRUN).to_string(),
        "stack check=guard task=0x000000000000002a task_sp=0x0000000044107f00 \
         stack_base=0x0000000044100000 stack_top=0x0000000044108000"
    );
    assert_eq!(
        Cause::Fault(VTABLE_BRANCH).to_string(),
        format!("fault {VTABLE_BRANCH}")
    );
    let raised = Location::caller();
    assert_eq!(
        Cause::panic(Some(raised)).to_string(),
        format!(
            "panic file={} line={} column={}",
            raised.file(),
            raised.line(),
            raised.column()
        )
    );
    assert_eq!(
        Cause::panic(None).to_string(),
        "panic file=null line=0 column=0"
    );
}

/// `image` as a machine of the other byte order holds it.
fn byte_swapped(image: &[u8]) -> Vec<u8> {
    let mut swapped = image.to_vec();
    let words = offset_of!(Kept, words)..offset_of!(Kept, file);
    let (words, _) = swapped[words].as_chunks_mut::<{ size_of::<u64>() }>();
    for word in words {
        word.reverse();
    }
    swapped
}

/// A report that never reached the console leaves its cause in memory, so
/// what a debugger reads there decodes to what the latch kept, in either byte
/// order.
#[test]
fn a_kept_cause_decodes_from_the_bytes_a_debugger_reads() {
    let little_endian = 1u16.to_ne_bytes() == 1u16.to_le_bytes();
    let causes = [
        Cause::Fault(VTABLE_BRANCH),
        Cause::Fault(KernelFault {
            syndrome: None,
            address: None,
            pc: 0x4008_0000,
            sp: None,
        }),
        Cause::panic(Some(Location::caller())),
        Cause::panic(None),
        Cause::Stack(OVERRUN),
        Cause::Stack(StackCorruption {
            check: StackCheck::Suspension,
            ..OVERRUN
        }),
    ];
    for cause in causes {
        let kept = Kept::new();
        kept.keep(&cause);
        assert_eq!(kept.kept(), Some(cause));
        let image = kept_image(&cause);
        assert_eq!(decode_kept(&image, little_endian), Some(cause));
        assert_eq!(
            decode_kept(&byte_swapped(&image), !little_endian),
            Some(cause)
        );
        assert_eq!(decode_kept(&image[..KEPT_BYTES - 1], little_endian), None);
    }
    // A latch that has kept nothing holds zeros.
    assert_eq!(decode_kept(&[0; KEPT_BYTES], little_endian), None);
}

/// A kind no keep writes decodes to nothing rather than to a guessed cause.
#[test]
fn an_unknown_kept_kind_decodes_to_nothing() {
    let mut image = [0; KEPT_BYTES];
    image[offset_of!(Kept, kind)] = u8::MAX;
    assert_eq!(decode_kept(&image, true), None);
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
    let mut words = CauseWords::new();
    let keys: Vec<_> = VTABLE_BRANCH
        .fields(&mut words)
        .iter()
        .map(|field| field.key)
        .collect();
    assert_eq!(keys, ["syndrome", "fault_addr", "fault_pc", "fault_sp"]);

    let keys: Vec<_> = OVERRUN
        .fields(&mut words)
        .iter()
        .map(|field| field.key)
        .collect();
    assert_eq!(
        keys,
        ["check", "task", "task_sp", "stack_base", "stack_top"]
    );
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
