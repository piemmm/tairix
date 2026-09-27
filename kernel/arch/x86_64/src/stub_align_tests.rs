//! Host unit tests for the stack alignment the exception stubs hand their Rust
//! dispatchers.
//!
//! System V AMD64 (§3.2.2) has a callee entered with `%rsp ≡ 8 (mod 16)`, and
//! the compiler is entitled to rely on it: an aligned vector spill or a
//! 16-byte atomic on a stack slot faults on a stack eight bytes off. The stubs
//! are naked assembly no host test can run, so each is simulated here from its
//! own source, one instruction at a time. The needles live in this file, so a
//! needle can never match itself.

extern crate std;

use std::string::String;
use std::vec::Vec;

/// The string-literal instructions of the naked stub whose definition starts
/// at the line containing `anchor`, in order, whitespace-collapsed.
fn stub_instructions(src: &str, anchor: &str) -> Vec<String> {
    let mut lines = src.lines().skip_while(|line| !line.contains(anchor));
    assert!(
        lines.next().is_some(),
        "no `{anchor}` in the inspected source"
    );
    lines
        .take_while(|line| !line.contains("options("))
        .filter_map(|line| {
            let literal = line.trim().trim_end_matches(',');
            literal
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(|instruction| instruction.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .collect()
}

/// The instructions of the assembly routine at `label` in a `.s` source, in
/// order, up to its first `call`.
fn routine_instructions(src: &str, label: &str) -> Vec<String> {
    let mut lines = src.lines().skip_while(|line| line.trim() != label);
    assert!(
        lines.next().is_some(),
        "no `{label}` in the inspected source"
    );
    let mut routine = Vec::new();
    for line in lines {
        let instruction = line.split("//").next().unwrap_or("").trim();
        if instruction.is_empty() {
            continue;
        }
        let instruction = instruction.split_whitespace().collect::<Vec<_>>().join(" ");
        let calls = instruction.starts_with("call ");
        routine.push(instruction);
        if calls {
            break;
        }
    }
    routine
}

/// `%rsp` modulo 16 at the dispatcher's entry, given it modulo 16 at the
/// stub's entry, or `None` if the stub leaves it undetermined.
fn dispatcher_entry_alignment(instructions: &[String], at_stub_entry: Option<u64>) -> Option<u64> {
    let mut rsp = at_stub_entry;
    let sub = |rsp: Option<u64>, bytes: u64| rsp.map(|r| (r + 16 - bytes % 16) % 16);
    for instruction in instructions {
        let words: Vec<&str> = instruction.split(' ').collect();
        match words.as_slice() {
            ["pushq", _] => rsp = sub(rsp, 8),
            // `%gs:0` is the per-CPU syscall stack top, which
            // `validate_kernel_rsp0` refuses unless it is 16-aligned.
            ["andq", "$-16,", "%rsp"] | ["movq", "%gs:0,", "%rsp"] => rsp = Some(0),
            ["subq", bytes, "%rsp"] => {
                let bytes = bytes
                    .trim_start_matches('$')
                    .trim_end_matches(',')
                    .parse()
                    .expect("an immediate");
                rsp = sub(rsp, bytes);
            }
            ["call", ..] => return sub(rsp, 8),
            _ => {}
        }
    }
    panic!("the stub never calls its dispatcher");
}

#[test]
fn every_diverging_exception_stub_enters_its_dispatcher_aligned() {
    let stub = stub_instructions(
        include_str!("interrupts.rs"),
        "macro_rules! exception_isr_body",
    );
    // Whatever alignment the delivery left, with or without an error code.
    assert_eq!(dispatcher_entry_alignment(&stub, None), Some(8));
}

/// The CPU aligns `%rsp` to 16 before it pushes an exception frame in
/// long mode (Intel SDM Vol 3A §6.14.2), and `#PF` pushes an error code, so
/// the stub starts 16-aligned after the frame and the code.
#[test]
fn the_page_fault_stub_enters_its_dispatcher_aligned() {
    let stub = stub_instructions(include_str!("fault.rs"), "fn page_fault_isr()");
    assert_eq!(dispatcher_entry_alignment(&stub, Some(0)), Some(8));
}

/// A resumable ISR has no error code: the five-word frame leaves `%rsp` eight
/// bytes below a 16-byte boundary.
#[test]
fn every_resumable_isr_stub_enters_its_dispatcher_aligned() {
    let stub = stub_instructions(include_str!("interrupts.rs"), "macro_rules! define_isr");
    assert_eq!(dispatcher_entry_alignment(&stub, Some(8)), Some(8));
}

#[test]
fn the_syscall_stub_enters_its_dispatcher_aligned() {
    let stub = stub_instructions(include_str!("syscall_entry.rs"), "fn syscall_entry_stub()");
    assert_eq!(dispatcher_entry_alignment(&stub, None), Some(8));
}

/// Each external-IRQ vector's stub pushes its vector word and jumps to the
/// common trampoline, so the trampoline starts 16-aligned.
#[test]
fn the_external_irq_trampoline_enters_its_dispatcher_aligned() {
    let routine = routine_instructions(
        include_str!("external_irq.s"),
        "tairix_arch_x86_64_external_irq_common:",
    );
    assert_eq!(dispatcher_entry_alignment(&routine, Some(0)), Some(8));
}
