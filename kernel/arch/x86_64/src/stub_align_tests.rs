//! Host unit tests for the entry stubs, each simulated from its own source one
//! instruction at a time.
//!
//! The stubs are naked assembly no host test can run, and the offsets they
//! read the interrupted frame at are literals the layout constants must agree
//! with. The simulation tracks `%rsp` against the frame top — `RSP0` for an
//! entry from ring 3, 16-byte aligned for any entry — and checks what the
//! kernel relies on: the System V alignment at each Rust call (the compiler
//! may spill SSE registers with `movaps`), the alignment of every SSE slot,
//! that the SSE frame is restored as it was saved, that the kernel `MXCSR` is
//! loaded before Rust runs, that each exit reads the saved `CS` and the area
//! where they are, and that a pending extended-state load precedes the SSE
//! restore. Shared template macros are expanded from their own definitions,
//! so a needle can never match itself.

extern crate std;

use std::collections::BTreeMap;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::fpu::{FP_FRAME_BYTES, FP_FRAME_MXCSR};
use crate::interrupts::{ISR_FRAME_CS, ISR_FRAME_TOP, WORD_ISR_FRAME_CS, WORD_ISR_FRAME_TOP};
use crate::syscall_entry::SYSCALL_ABOVE_FP_FRAME;

/// Every source a stub or a template macro it splices in lives in.
const SOURCES: &[&str] = &[
    include_str!("interrupts.rs"),
    include_str!("fault.rs"),
    include_str!("irq.rs"),
    include_str!("syscall_entry.rs"),
    include_str!("fpu.rs"),
];

/// Offset of the saved `CS` below the frame top.
const CS_BELOW_TOP: i64 = 32;
/// The CPU's five-word frame.
const CPU_FRAME: i64 = 40;

/// The string literal on `line`, if it is one, with its trailing `\n`.
fn literal(line: &str) -> Option<String> {
    let text = line.trim().trim_end_matches(',');
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    let body = body.strip_suffix("\\n").unwrap_or(body);
    Some(body.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The instructions of the template macro `name`, from its definition.
fn macro_instructions(name: &str) -> Vec<String> {
    let header = format!("macro_rules! {name} {{");
    let source = SOURCES
        .iter()
        .find(|s| s.contains(&header))
        .unwrap_or_else(|| panic!("no `{header}` in the inspected sources"));
    source
        .lines()
        .skip_while(|line| !line.contains(&header))
        .skip(1)
        .take_while(|line| *line != "}")
        .filter_map(literal)
        .collect()
}

/// The instructions of the naked stub whose definition starts at the line
/// containing `anchor`, with template macros expanded and each `{name}` in
/// `consts` replaced by its value.
fn stub(anchor: &str, consts: &[(&str, usize)]) -> Vec<String> {
    let source = SOURCES
        .iter()
        .find(|s| s.contains(anchor))
        .unwrap_or_else(|| panic!("no `{anchor}` in the inspected sources"));
    let mut lines = source.lines().skip_while(|line| !line.contains(anchor));
    assert!(lines.next().is_some());
    let mut out = Vec::new();
    for line in lines.take_while(|line| !line.contains("options(")) {
        let trimmed = line.trim();
        if let Some(instruction) = literal(line) {
            out.push(instruction);
        } else if let Some(call) = trimmed.strip_suffix("!(),") {
            let name = call.rsplit("::").next().unwrap_or(call);
            out.extend(macro_instructions(name));
        }
    }
    out.into_iter()
        .map(|mut instruction| {
            for (name, value) in consts {
                instruction = instruction.replace(&format!("{{{name}}}"), &value.to_string());
            }
            instruction
        })
        .collect()
}

/// The constants every returning stub binds.
fn frame_consts(frame_cs: usize, frame_top: usize) -> Vec<(&'static str, usize)> {
    std::vec![
        ("fp_frame", FP_FRAME_BYTES),
        ("fp_mxcsr", FP_FRAME_MXCSR),
        ("frame_cs", frame_cs),
        ("frame_top", frame_top),
        (
            "vector",
            core::mem::size_of::<crate::interrupts::SavedRegs>()
        ),
    ]
}

/// A stack operand `K(%rsp)` in `operand`, as `K`.
fn rsp_offset(operand: &str) -> Option<i64> {
    let offset = operand.trim_end_matches(',').strip_suffix("(%rsp)")?;
    Some(if offset.is_empty() {
        0
    } else {
        offset.parse().ok()?
    })
}

/// What one simulated stub did.
#[derive(Default)]
struct Trace {
    /// `%rsp` below the frame top at each Rust call, in order.
    calls: Vec<(String, Option<i64>)>,
    /// Where each `xmmN` was saved, and where it was restored from.
    saved: BTreeMap<String, i64>,
    restored: BTreeMap<String, i64>,
    /// Stack slot the interrupted `MXCSR` was saved to and reloaded from.
    mxcsr_saved: Option<i64>,
    mxcsr_restored: Option<i64>,
    /// Index of the kernel `MXCSR` load, the extended-state load and the
    /// first SSE restore.
    kernel_mxcsr_at: Option<usize>,
    xstate_load_at: Option<usize>,
    first_restore_at: Option<usize>,
    /// Slots read by `testb`/`cmpb`/`leaq` on the way out.
    cs_read: Option<i64>,
    pending_read: Option<i64>,
    area_lea: Option<i64>,
    /// `%rsp` at the `iretq`.
    iret_at: Option<i64>,
}

/// Run `instructions` from `entry` bytes below the frame top (`None` for an
/// entry on a stack the stub has not yet pivoted to).
fn simulate(instructions: &[String], entry: Option<i64>) -> Trace {
    let mut rsp = entry.map(|below| -below);
    let mut aligned = entry.map(|below| (-below).rem_euclid(16));
    let mut trace = Trace::default();
    let mut last_lea: Option<i64> = None;
    for (index, instruction) in instructions.iter().enumerate() {
        let words: Vec<&str> = instruction.split(' ').collect();
        let at = |offset: i64| rsp.map(|r| r + offset);
        let slot_aligned = |offset: i64| aligned.map(|a| (a + offset).rem_euclid(16) == 0);
        match words.as_slice() {
            ["pushq", _] | ["subq", "$8,", "%rsp"] => {
                rsp = rsp.map(|r| r - 8);
                aligned = aligned.map(|a| (a - 8).rem_euclid(16));
            }
            ["popq", "%rsp"] => rsp = None,
            ["popq", _] | ["addq", "$8,", "%rsp"] => {
                rsp = rsp.map(|r| r + 8);
                aligned = aligned.map(|a| (a + 8).rem_euclid(16));
            }
            [op @ ("subq" | "addq"), bytes, "%rsp"] => {
                let magnitude: i64 = bytes
                    .trim_start_matches('$')
                    .trim_end_matches(',')
                    .parse()
                    .expect("an immediate");
                let n = if *op == "subq" { -magnitude } else { magnitude };
                rsp = rsp.map(|r| r + n);
                aligned = aligned.map(|a| (a + n).rem_euclid(16));
            }
            ["andq", "$-16,", "%rsp"] => {
                rsp = None;
                aligned = Some(0);
            }
            // `%gs:0` is `RSP0`, the frame top of the entry about to be built.
            ["movq", "%gs:0,", "%rsp"] => {
                rsp = Some(0);
                aligned = Some(0);
            }
            ["movaps", src, dst] => {
                let (register, offset, saving) = match (rsp_offset(dst), rsp_offset(src)) {
                    (Some(offset), None) => (src.trim_end_matches(','), offset, true),
                    (None, Some(offset)) => (*dst, offset, false),
                    _ => panic!("`{instruction}` moves no stack slot"),
                };
                assert_eq!(
                    slot_aligned(offset),
                    Some(true),
                    "`{instruction}` is misaligned"
                );
                let slot = at(offset).expect("a tracked stack");
                let map = if saving {
                    &mut trace.saved
                } else {
                    trace.first_restore_at.get_or_insert(index);
                    &mut trace.restored
                };
                assert!(
                    map.insert(register.to_string(), slot).is_none(),
                    "`{register}` twice"
                );
            }
            ["stmxcsr", operand] => trace.mxcsr_saved = at(rsp_offset(operand).expect("a slot")),
            ["ldmxcsr", "{kernel_mxcsr}(%rip)"] => {
                trace.kernel_mxcsr_at.get_or_insert(index);
            }
            ["ldmxcsr", operand] => {
                trace.first_restore_at.get_or_insert(index);
                trace.mxcsr_restored = at(rsp_offset(operand).expect("a slot"));
            }
            ["testb", "$3,", operand] => trace.cs_read = at(rsp_offset(operand).expect("a slot")),
            ["cmpb", "$0,", operand] => {
                trace.pending_read = at(rsp_offset(operand).expect("a slot"));
            }
            ["leaq", operand, "%rdi"] => last_lea = at(rsp_offset(operand).expect("a slot")),
            ["call", "tairix_arch_x86_64_xstate_load"] => {
                trace.xstate_load_at = Some(index);
                trace.area_lea = last_lea;
            }
            ["call", target] => trace.calls.push((
                (*target).to_string(),
                aligned.map(|a| (a - 8).rem_euclid(16)),
            )),
            ["iretq"] => trace.iret_at = rsp,
            _ => {}
        }
    }
    trace
}

/// The checks every stub that returns to its interrupted context must pass.
fn assert_returning(name: &str, trace: &Trace, reads_cs: bool) {
    assert_eq!(
        trace.calls,
        [("{dispatch}".to_string(), Some(8))],
        "{name}: one dispatcher call, entered with %rsp ≡ 8 (mod 16)"
    );
    assert_eq!(trace.saved.len(), 16, "{name}: every xmm register saved");
    assert_eq!(trace.saved, trace.restored, "{name}: restored as saved");
    assert!(trace.mxcsr_saved.is_some() && trace.mxcsr_saved == trace.mxcsr_restored);
    let kernel_mxcsr = trace.kernel_mxcsr_at.expect("kernel MXCSR loaded");
    let restore = trace.first_restore_at.expect("the SSE frame is restored");
    assert!(kernel_mxcsr < restore, "{name}: kernel MXCSR before Rust");
    let load = trace.xstate_load_at.expect("an extended-state load");
    assert!(load < restore, "{name}: the load precedes the SSE restore");
    assert_eq!(
        trace.pending_read,
        Some(0),
        "{name}: the pending flag at the frame top"
    );
    assert_eq!(trace.area_lea, Some(0), "{name}: the area is the frame top");
    if reads_cs {
        assert_eq!(trace.cs_read, Some(-CS_BELOW_TOP), "{name}: the saved CS");
        assert_eq!(
            trace.iret_at,
            Some(-CPU_FRAME),
            "{name}: iretq pops the CPU frame"
        );
    }
}

/// A resumable ISR has no error code: the five-word frame leaves `%rsp` eight
/// bytes below a 16-byte boundary.
#[test]
fn a_resumable_isr_stub_frames_sse_state_and_loads_pending_state() {
    let body = stub(
        "macro_rules! define_isr",
        &frame_consts(ISR_FRAME_CS, ISR_FRAME_TOP),
    );
    assert_returning("define_isr!", &simulate(&body, Some(CPU_FRAME)), true);
}

/// The CPU aligns `%rsp` to 16 before it pushes an exception frame in
/// long mode (Intel SDM Vol 3A §6.14.2), and `#PF` pushes an error code, so
/// the stub starts 16-aligned after the frame and the code.
#[test]
fn the_page_fault_stub_frames_sse_state_and_loads_pending_state() {
    let body = stub(
        "fn page_fault_isr()",
        &frame_consts(WORD_ISR_FRAME_CS, WORD_ISR_FRAME_TOP),
    );
    let trace = simulate(&body, Some(CPU_FRAME + 8));
    assert_returning("page_fault_isr", &trace, true);
    // Its argument reads: the error code, the frame's `rip` and its address,
    // and the interrupted `rsp`, before the SSE frame moves `%rsp`.
    let reads = [
        ("movq 120(%rsp), %rdi", -CPU_FRAME - 8),
        ("movq 128(%rsp), %rdx", -CPU_FRAME),
        ("leaq 128(%rsp), %rcx", -CPU_FRAME),
        ("movq 152(%rsp), %r9", -16),
    ];
    let after_gprs = -(CPU_FRAME + 8) - 15 * 8;
    for (instruction, slot) in reads {
        let offset: i64 = instruction
            .split(' ')
            .nth(1)
            .and_then(rsp_offset)
            .expect("a stack read");
        assert!(
            body.iter().any(|i| i == instruction),
            "`{instruction}` is in the stub"
        );
        assert_eq!(after_gprs + offset, slot, "`{instruction}`");
    }
}

/// Each external-IRQ vector's stub pushes its vector word and jumps to the
/// common trampoline, so the trampoline starts 16-aligned.
#[test]
fn the_external_irq_trampoline_frames_sse_state_and_loads_pending_state() {
    let body = stub(
        "fn tairix_arch_x86_64_external_irq_common()",
        &frame_consts(WORD_ISR_FRAME_CS, WORD_ISR_FRAME_TOP),
    );
    assert_returning(
        "external_irq_common",
        &simulate(&body, Some(CPU_FRAME + 8)),
        true,
    );
    // The vector word it hands the dispatcher sits just below the CPU frame.
    let vector = i64::try_from(core::mem::size_of::<crate::interrupts::SavedRegs>()).unwrap();
    assert!(body
        .iter()
        .any(|i| i == &format!("movq {vector}(%rsp), %rsi")));
    assert_eq!(-(CPU_FRAME + 8) - 15 * 8 + vector, -CPU_FRAME - 8);
}

#[test]
fn the_syscall_stub_frames_sse_state_and_loads_pending_state() {
    let consts = std::vec![
        ("fp_frame", FP_FRAME_BYTES),
        ("fp_mxcsr", FP_FRAME_MXCSR),
        ("frame_top", FP_FRAME_BYTES + SYSCALL_ABOVE_FP_FRAME),
    ];
    let body = stub("fn syscall_entry_stub()", &consts);
    assert_returning("syscall_entry_stub", &simulate(&body, None), false);
}

/// A diverging stub owes nothing back, but its Rust still runs aligned and
/// under the kernel `MXCSR`, whatever the delivery left.
#[test]
fn every_diverging_stub_enters_rust_aligned_under_the_kernel_mxcsr() {
    for (anchor, entries) in [
        ("macro_rules! exception_isr_body", &[None][..]),
        (
            "fn tairix_arch_x86_64_isr_default()",
            &[Some(CPU_FRAME)][..],
        ),
    ] {
        let body = stub(anchor, &[]);
        for &entry in entries {
            let trace = simulate(&body, entry);
            assert_eq!(trace.calls.len(), 1, "{anchor}");
            assert_eq!(trace.calls[0].1, Some(8), "{anchor}");
            assert!(trace.kernel_mxcsr_at.is_some(), "{anchor}");
            assert!(trace.saved.is_empty(), "{anchor}: nothing to restore");
        }
    }
}

/// The frame is sixteen 16-byte slots and the `MXCSR` word above them, as the
/// template macros lay it out.
#[test]
fn the_sse_frame_is_the_size_its_templates_fill() {
    let save = macro_instructions("fp_frame_save");
    assert_eq!(save[0], "subq ${fp_frame}, %rsp");
    for register in 0..16 {
        assert!(save.contains(&format!("movaps %xmm{register}, {}(%rsp)", register * 16)));
    }
    assert!(save.contains(&"stmxcsr {fp_mxcsr}(%rsp)".to_string()));
    assert_eq!(FP_FRAME_MXCSR, 16 * 16);
    assert_eq!(FP_FRAME_BYTES, FP_FRAME_MXCSR + 16);
}
