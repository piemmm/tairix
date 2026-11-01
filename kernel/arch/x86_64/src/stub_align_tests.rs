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
//! so a needle can never match itself. An assembly stub that forwards to a
//! naked entry is simulated through it, and each constant the simulation
//! substitutes is checked bound to the definition it assumes.

extern crate std;

use std::collections::{BTreeMap, BTreeSet};
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::fpu::{FP_FRAME_BYTES, FP_FRAME_MXCSR};
use crate::interrupts::{
    SavedRegs, ISR_FRAME_CS, ISR_FRAME_TOP, WORD_ISR_FRAME_CS, WORD_ISR_FRAME_TOP,
};
use crate::sidechannel::sidechannel_tests::{assembly_line, instructions, ASM_SOURCES};
use crate::syscall_entry::SYSCALL_ABOVE_FP_FRAME;
use tairix_fuzzseed::asm::{bindings, invocations, mnemonic, template_instructions};
use tairix_fuzzseed::sources::is_assembly;

/// Offset of the saved `CS` below the frame top.
const CS_BELOW_TOP: i64 = 32;
/// The CPU's five-word frame.
const CPU_FRAME: i64 = 40;

/// The string literal on `line`, if it is one, with its trailing `\n`.
pub(crate) fn literal(line: &str) -> Option<String> {
    let text = line.trim().trim_end_matches(',');
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    let body = body.strip_suffix("\\n").unwrap_or(body);
    Some(body.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The one Rust source among those inspected with a line holding `needle`,
/// and that line's byte offset in it.
fn only_definition(needle: &str) -> (&'static str, usize) {
    let mut found = ASM_SOURCES
        .iter()
        .filter(|(name, _)| !is_assembly(name))
        .flat_map(|(_, source)| {
            let mut at = 0;
            source.split_inclusive('\n').filter_map(move |line| {
                let start = at;
                at += line.len();
                line.contains(needle).then_some((*source, start))
            })
        });
    let definition = found
        .next()
        .unwrap_or_else(|| panic!("no `{needle}` in the inspected sources"));
    assert!(found.next().is_none(), "`{needle}` is defined twice");
    definition
}

/// The instructions of the template macro `name`, from its definition.
fn macro_instructions(name: &str) -> Vec<String> {
    let header = format!("macro_rules! {name} {{");
    let (source, at) = only_definition(&header);
    source[at..]
        .lines()
        .skip(1)
        .take_while(|line| *line != "}")
        .filter_map(literal)
        .collect()
}

/// `lines` with each splice of a template macro, a `NAME!(),` line,
/// replaced by the macro's own instructions.
fn expand<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        if let Some(instruction) = literal(line) {
            out.push(instruction);
        } else if let Some(call) = line.trim().strip_suffix("!(),") {
            out.extend(macro_instructions(call.rsplit("::").next().unwrap_or(call)));
        }
    }
    out
}

/// A constant a stub reads and the simulation substitutes: its operand, the
/// expression the stub must bind it to, and the value assumed.
type Bound = (&'static str, &'static str, usize);

const FP_FRAME: Bound = (
    "fp_frame",
    "const crate::fpu::FP_FRAME_BYTES",
    FP_FRAME_BYTES,
);
const FP_MXCSR: Bound = (
    "fp_mxcsr",
    "const crate::fpu::FP_FRAME_MXCSR",
    FP_FRAME_MXCSR,
);
const ISR_FRAME: [Bound; 2] = [
    (
        "frame_cs",
        "const crate::interrupts::ISR_FRAME_CS",
        ISR_FRAME_CS,
    ),
    (
        "frame_top",
        "const crate::interrupts::ISR_FRAME_TOP",
        ISR_FRAME_TOP,
    ),
];
const WORD_ISR_FRAME: [Bound; 2] = [
    (
        "frame_cs",
        "const crate::interrupts::WORD_ISR_FRAME_CS",
        WORD_ISR_FRAME_CS,
    ),
    (
        "frame_top",
        "const crate::interrupts::WORD_ISR_FRAME_TOP",
        WORD_ISR_FRAME_TOP,
    ),
];
const VECTOR: Bound = (
    "vector",
    "const core::mem::size_of::<SavedRegs>()",
    core::mem::size_of::<SavedRegs>(),
);

/// The constants every returning stub binds, for its frame.
fn frame_consts(frame: [Bound; 2]) -> Vec<Bound> {
    std::vec![FP_FRAME, FP_MXCSR, frame[0], frame[1]]
}

/// Whether `anchor` names an assembly definition: a label, or the macro a
/// label is emitted by.
fn is_assembly_anchor(anchor: &str) -> bool {
    anchor.starts_with(".macro ") || anchor.ends_with(':')
}

/// The one assembly source defining `anchor`, its instructions, and the
/// index of the defining line.
fn definition(anchor: &str) -> (&'static str, Vec<String>, usize) {
    let mut found = ASM_SOURCES.iter().flat_map(|(name, source)| {
        let lines = instructions(name, source);
        let at: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| *line == anchor || line.starts_with(&format!("{anchor} ")))
            .map(|(at, _)| at)
            .collect();
        at.into_iter()
            .map(move |at| (*name, lines.clone(), at))
            .collect::<Vec<_>>()
    });
    let definition = found
        .next()
        .unwrap_or_else(|| panic!("no `{anchor}` in the inspected sources"));
    assert!(found.next().is_none(), "`{anchor}` is defined twice");
    definition
}

/// What the assembly defined at `lines[at]` runs, up to its macro's end or
/// its `.size`: every line but its own first label and the symbol
/// directives that emit nothing, so any other directive stays to be judged.
fn definition_body(lines: &[String], at: usize) -> Vec<String> {
    let mut labelled = false;
    lines[at + 1..]
        .iter()
        .take_while(|line| *line != ".endm" && !line.starts_with(".size "))
        .filter(|line| {
            let symbol = [".global ", ".globl ", ".type "]
                .iter()
                .any(|directive| line.starts_with(directive));
            let own_label = !labelled && line.ends_with(':');
            labelled |= own_label;
            !symbol && !own_label
        })
        .cloned()
        .collect()
}

/// The entry an assembly stub's closing `jmp` forwards to, if one is listed.
fn forwarded_to(body: &[String]) -> Option<&'static str> {
    let target = body.last()?.strip_prefix("jmp ")?;
    let (function, label) = (format!("fn {target}()"), format!("{target}:"));
    ENTRIES
        .iter()
        .map(|(anchor, _)| *anchor)
        .find(|anchor| *anchor == function || *anchor == label)
}

/// The instructions of the stub defined at `anchor` — through the entry it
/// forwards to, for an assembly one — with template macros expanded and each
/// `{name}` of `consts` replaced by its value, once the stub is checked to
/// bind it to the expression the constant assumes.
pub(crate) fn stub(anchor: &str, consts: &[Bound]) -> Vec<String> {
    if is_assembly_anchor(anchor) {
        let (_, lines, at) = definition(anchor);
        let mut body = definition_body(&lines, at);
        if let Some(target) = forwarded_to(&body) {
            body.extend(stub(target, consts));
        }
        return body;
    }
    let (source, at) = only_definition(anchor);
    let block = invocations(&source[at..], "naked_asm")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("`{anchor}` defines no naked block"));
    let bound = bindings(block);
    expand(block.lines())
        .into_iter()
        .map(|mut instruction| {
            for (name, expression, value) in consts {
                let operand = format!("{{{name}}}");
                if instruction.contains(&operand) {
                    assert_eq!(
                        bound.get(*name).map(String::as_str),
                        Some(*expression),
                        "{anchor}: `{name}` is bound to another constant"
                    );
                    instruction = instruction.replace(&operand, &value.to_string());
                }
            }
            instruction
        })
        .collect()
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
    /// The slot the last `pushq` of an immediate wrote.
    pushed_immediate: Option<i64>,
    /// The stack slot `%rsi` was last loaded from.
    rsi_read: Option<i64>,
    /// `%rsp` at the first `jmp`.
    rsp_at_jmp: Option<i64>,
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
            ["movq", operand, "%rsi"] if rsp_offset(operand).is_some() => {
                trace.rsi_read = at(rsp_offset(operand).expect("a slot"));
            }
            ["jmp", _] => {
                trace
                    .rsp_at_jmp
                    .get_or_insert_with(|| rsp.unwrap_or(i64::MIN));
            }
            _ => {}
        }
        if words.first() == Some(&"pushq") && words.get(1).is_some_and(|w| w.starts_with('$')) {
            trace.pushed_immediate = rsp;
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
    let body = stub("macro_rules! define_isr", &frame_consts(ISR_FRAME));
    assert_returning("define_isr!", &simulate(&body, Some(CPU_FRAME)), true);
}

/// The CPU aligns `%rsp` to 16 before it pushes an exception frame in
/// long mode (Intel SDM Vol 3A §6.14.2), and `#PF` pushes an error code, so
/// the stub starts 16-aligned after the frame and the code.
#[test]
fn the_page_fault_stub_frames_sse_state_and_loads_pending_state() {
    let body = stub("fn page_fault_isr()", &frame_consts(WORD_ISR_FRAME));
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

/// Each external-IRQ vector's stub, simulated from the CPU frame through the
/// trampoline it forwards to: its one pushed word is the vector the
/// dispatcher is handed, and the Rust side finds the CPU frame exactly that
/// word above the saved registers.
#[test]
fn each_external_irq_stub_frames_sse_state_and_loads_pending_state() {
    let mut consts = frame_consts(WORD_ISR_FRAME);
    consts.push(VECTOR);
    let body = stub(".macro external_irq_stub", &consts);
    let trace = simulate(&body, Some(CPU_FRAME));
    assert_returning("external_irq_stub", &trace, true);
    assert_eq!(
        trace.pushed_immediate,
        Some(-CPU_FRAME - 8),
        "the vector word sits just below the CPU frame"
    );
    assert_eq!(
        trace.rsi_read, trace.pushed_immediate,
        "the dispatcher is handed the word the stub pushed"
    );
    let pushed = trace.rsp_at_jmp.map(|at| -CPU_FRAME - at);
    assert_eq!(
        pushed.and_then(|bytes| usize::try_from(bytes).ok()),
        Some(crate::irq::EXTERNAL_VECTOR_QWORD_BYTES),
        "the dispatcher's frame arithmetic is the bytes the stub pushes"
    );
}

#[test]
fn the_syscall_stub_frames_sse_state_and_loads_pending_state() {
    let consts = std::vec![
        FP_FRAME,
        FP_MXCSR,
        (
            "frame_top",
            "const crate::fpu::FP_FRAME_BYTES + SYSCALL_ABOVE_FP_FRAME",
            FP_FRAME_BYTES + SYSCALL_ABOVE_FP_FRAME,
        ),
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

/// How the CPU reaches an entry point, which decides what it may assume of
/// the state it inherits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reached {
    /// Interrupt, exception, or trap-gate delivery, which keeps the
    /// interrupted `DF` and `AC`.
    Gate,
    /// The `syscall` instruction, whose `IA32_FMASK` clears `DF` and `AC`.
    Syscall,
    /// A boot loader's or firmware's hand-off, or a start-up IPI, after which
    /// the entry assumes nothing of the flags.
    Boot,
    /// Kernel code already under the Rust ABI — a call, a jump from entry
    /// assembly, or the return resuming it after a fault — so `DF=0`.
    Call,
}

/// Every entry point the port defines, by the line its definition starts
/// at — `fn NAME()`, `macro_rules! NAME`, `NAME:` for an assembly label, or
/// `.macro NAME` for a label a macro emits — and how it is reached.
pub(crate) const ENTRIES: &[(&str, Reached)] = &[
    ("fn syscall_entry_stub()", Reached::Syscall),
    ("macro_rules! define_isr", Reached::Gate),
    ("macro_rules! exception_isr_body", Reached::Gate),
    ("fn page_fault_isr()", Reached::Gate),
    ("fn tairix_arch_x86_64_external_irq_common()", Reached::Gate),
    ("fn tairix_arch_x86_64_isr_default()", Reached::Gate),
    (".macro external_irq_stub", Reached::Gate),
    ("_start:", Reached::Boot),
    ("pvh_start:", Reached::Boot),
    ("_ap_trampoline_start:", Reached::Boot),
    ("fn tairix_arch_x86_64_fpu_enable()", Reached::Call),
    ("fn tairix_arch_x86_64_xstate_load()", Reached::Call),
    ("fn tairix_arch_x86_64_x87_scrub()", Reached::Call),
    ("higher_half_entry:", Reached::Call),
    ("tairix_arch_x86_64_switch:", Reached::Call),
    ("_takeover_switch_stack:", Reached::Call),
    ("tairix_x86_64_guarded_user_copy:", Reached::Call),
    ("tairix_x86_64_uaccess_fixup:", Reached::Call),
];

/// The global labels nothing enters: data, and markers bounding code entered
/// elsewhere.
const NOT_ENTERED: &[&str] = &[
    "_ap_trampoline_end",
    "boot_pdpt_physmap",
    "tairix_arch_x86_64_external_irq_table",
    "tairix_x86_64_uaccess_window_begin",
    "tairix_x86_64_uaccess_window_end",
];

/// The anchors of the entry points reached as `reached`.
pub(crate) fn reached_by(reached: Reached) -> impl Iterator<Item = &'static str> {
    ENTRIES
        .iter()
        .filter(move |(_, how)| *how == reached)
        .map(|(anchor, _)| *anchor)
}

/// The label `line` defines, if it is one: the text before its first `:`,
/// non-empty and holding no space, so `movq %gs:0, %rsp` defines none.
fn label(line: &str) -> Option<&str> {
    let (name, rest) = line.split_once(':')?;
    (!name.is_empty() && !name.contains(char::is_whitespace) && rest.trim().is_empty())
        .then_some(name)
}

/// Each global label `sources` define, mapped to the anchor that names it:
/// `LABEL:`, or `.macro NAME` for one a macro emits. A global with no label,
/// set by `.set` or `=`, is entered by nothing.
fn assembly_labels(sources: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for &(name, source) in sources {
        let lines = instructions(name, source);
        let globals: BTreeSet<&str> = lines
            .iter()
            .filter_map(|line| {
                line.strip_prefix(".global ")
                    .or_else(|| line.strip_prefix(".globl "))
            })
            .flat_map(|symbols| symbols.split(',').map(str::trim))
            .collect();
        let mut emitted_by = None;
        for line in &lines {
            if let Some(header) = line.strip_prefix(".macro ") {
                emitted_by = header
                    .split_whitespace()
                    .next()
                    .map(|name| format!(".macro {name}"));
            } else if line == ".endm" {
                emitted_by = None;
            } else if let Some(name) = label(line).filter(|name| globals.contains(name)) {
                let anchor = emitted_by.clone().unwrap_or_else(|| format!("{name}:"));
                found.insert(name.to_string(), anchor);
            }
        }
    }
    found
}

/// Each naked function `sources` define, by its anchor: a naked function a
/// template macro emits is named by its macro.
fn naked_anchors(sources: &[(&str, &str)]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for &(name, source) in sources {
        if is_assembly(name) {
            continue;
        }
        let mut lines = source.lines();
        let mut template = None;
        while let Some(line) = lines.next() {
            if let Some(rest) = line.strip_prefix("macro_rules! ") {
                template = rest.split_whitespace().next();
            } else if line == "}" {
                template = None;
            }
            if line.trim() != "#[unsafe(naked)]" {
                continue;
            }
            let anchor = if let Some(template) = template {
                format!("macro_rules! {template}")
            } else {
                let signature = lines
                    .by_ref()
                    .find(|line| line.contains(" fn "))
                    .unwrap_or_else(|| panic!("{name}: a naked attribute on no function"));
                let function = signature
                    .split(" fn ")
                    .nth(1)
                    .and_then(|rest| rest.split('(').next())
                    .unwrap_or_else(|| panic!("{name}: an unnamed naked function"));
                format!("fn {function}()")
            };
            found.insert(anchor);
        }
    }
    found
}

/// Every naked function and every global label the port's sources define,
/// assembly and `global_asm!` templates alike, is in [`ENTRIES`] or
/// [`NOT_ENTERED`], so a new entry point cannot escape the tests that read
/// the inventory.
#[test]
fn every_entry_point_is_classified() {
    let labels = assembly_labels(ASM_SOURCES);
    for symbol in NOT_ENTERED {
        assert!(
            labels.contains_key(*symbol),
            "`{symbol}` is no longer defined"
        );
    }
    let mut found = naked_anchors(ASM_SOURCES);
    found.extend(
        labels
            .iter()
            .filter(|(symbol, _)| !NOT_ENTERED.contains(&symbol.as_str()))
            .map(|(_, anchor)| anchor.clone()),
    );
    let listed: BTreeSet<String> = ENTRIES
        .iter()
        .map(|(anchor, _)| (*anchor).to_string())
        .collect();
    assert_eq!(found, listed);
}

#[test]
fn the_entry_scan_reads_assembly_sources_and_global_asm_templates() {
    let stubs = "
        .macro forward vec
            .global forward_\\vec
        forward_\\vec:
            pushq $\\vec
            jmp common
        .endm
        .global table
        table:
            .quad 0
        .globl size
        .set size, 8
        local:
            nop
    ";
    let glue = r##"
        core::arch::global_asm!(r#"
            .global copy
        copy:  # a comment: with a colon
            ret
        "#);
        #[unsafe(naked)]
        pub unsafe extern "C" fn trampoline() {
            core::arch::naked_asm!("ret")
        }
    "##;
    let sources = [("stubs.s", stubs), ("glue.rs", glue)];
    let labels = assembly_labels(&sources);
    let expected: BTreeMap<String, String> = [
        ("forward_\\vec", ".macro forward"),
        ("table", "table:"),
        ("copy", "copy:"),
    ]
    .into_iter()
    .map(|(symbol, anchor)| (symbol.to_string(), anchor.to_string()))
    .collect();
    assert_eq!(labels, expected);
    assert_eq!(
        naked_anchors(&sources),
        BTreeSet::from([String::from("fn trampoline()")])
    );
}

/// Whether an assembly stub's `body` only forwards: pushes immediates, then
/// jumps. Anything else — another instruction, or a directive that emits
/// code — could clobber what its target saves.
fn only_forwards(body: &[String]) -> bool {
    let Some((last, pushes)) = body.split_last() else {
        return false;
    };
    last.starts_with("jmp ") && pushes.iter().all(|line| line.starts_with("pushq $"))
}

/// An assembly stub a gate reaches only pushes immediates and jumps to a
/// checked naked entry, so it clobbers nothing that entry saves and every
/// gate check runs over the path it starts.
#[test]
fn every_gate_entered_assembly_stub_only_forwards() {
    let mut checked = 0;
    for anchor in reached_by(Reached::Gate).filter(|anchor| is_assembly_anchor(anchor)) {
        let (_, lines, at) = definition(anchor);
        let body = definition_body(&lines, at);
        assert!(only_forwards(&body), "{anchor}: {body:?}");
        let target =
            forwarded_to(&body).unwrap_or_else(|| panic!("{anchor}: forwards nowhere listed"));
        assert!(
            reached_by(Reached::Gate).any(|entry| entry == target) && !is_assembly_anchor(target),
            "{anchor}: forwards to `{target}`, which is no checked gate entry"
        );
        checked += 1;
    }
    assert!(checked > 0, "no assembly stub is gate-entered");

    let lines =
        |text: &[&str]| -> Vec<String> { text.iter().map(|line| (*line).to_string()).collect() };
    let forwarder = lines(&[
        ".macro f v",
        ".global f_\\v",
        "f_\\v:",
        "pushq $\\v",
        "jmp common",
        ".endm",
    ]);
    assert!(only_forwards(&definition_body(&forwarder, 0)));
    for hidden in [
        lines(&[
            ".macro f v",
            "f_\\v:",
            ".byte 0x31, 0xdb",
            "pushq $\\v",
            "jmp common",
            ".endm",
        ]),
        lines(&[
            ".macro f v",
            "f_\\v:",
            ".rept 2",
            "pushq $\\v",
            ".endr",
            "jmp common",
            ".endm",
        ]),
        lines(&[
            ".macro f v",
            "f_\\v:",
            "pushq $\\v",
            "movq $0, %rbx",
            "jmp common",
            ".endm",
        ]),
    ] {
        assert!(!only_forwards(&definition_body(&hidden, 0)), "{hidden:?}");
    }
}

/// The external-IRQ stubs span the range `irq` defines, passed in as
/// operands, so the table the Rust side indexes holds one entry per vector by
/// construction.
#[test]
fn the_external_irq_stubs_span_the_range_irq_defines() {
    let source = include_str!("external_irq.s");
    let lines: Vec<String> = source.lines().filter_map(assembly_line).collect();
    let repeats: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with(".rept"))
        .collect();
    assert!(!repeats.is_empty());
    assert!(
        repeats.iter().all(|line| *line == ".rept {vector_count}"),
        "{repeats:?}"
    );
    let starts: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with(".set vec_no,") && !line.ends_with("vec_no + 1"))
        .collect();
    assert!(!starts.is_empty());
    assert!(
        starts
            .iter()
            .all(|line| *line == ".set vec_no, {first_vector}"),
        "{starts:?}"
    );
    let block = invocations(include_str!("lib.rs"), "global_asm")
        .into_iter()
        .find(|block| block.contains("include_str!(\"external_irq.s\")"))
        .expect("lib.rs assembles the stubs");
    let bound = bindings(block);
    assert_eq!(
        bound.get("first_vector").map(String::as_str),
        Some("const irq::EXTERNAL_VECTOR_FIRST")
    );
    assert_eq!(
        bound.get("vector_count").map(String::as_str),
        Some("const irq::EXTERNAL_VECTOR_COUNT")
    );
}

/// Operands that mean one thing wherever a block reads them, and the
/// definition each is bound to.
const SHARED_OPERANDS: &[(&str, &str)] = &[
    ("smap", "const crate::protection::Protections::SMAP.bits()"),
    ("in_force", "sym crate::protection::IN_FORCE"),
    ("kernel_mxcsr", "sym crate::fpu::KERNEL_MXCSR"),
    ("verw_sel", "sym crate::sidechannel::VERW_SELECTOR"),
    (FP_FRAME.0, FP_FRAME.1),
    (FP_MXCSR.0, FP_MXCSR.1),
];

/// The template macros the port's sources define, by name.
fn template_macros() -> Vec<&'static str> {
    ASM_SOURCES
        .iter()
        .filter(|(name, _)| !is_assembly(name))
        .flat_map(|(_, source)| source.lines())
        .filter_map(|line| line.strip_prefix("macro_rules! "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect()
}

/// What `block` spells: its own text, each template macro it splices in, and
/// each assembly source it includes.
fn block_text(block: &str) -> String {
    let mut text = String::from(block);
    for name in template_macros() {
        if block.contains(&format!("{name}!()")) {
            for instruction in macro_instructions(name) {
                text.push('\n');
                text.push_str(&instruction);
            }
        }
    }
    for &(name, source) in ASM_SOURCES.iter().filter(|(name, _)| is_assembly(name)) {
        if block.contains(&format!("include_str!(\"{name}\")")) {
            text.push('\n');
            text.push_str(source);
        }
    }
    text
}

/// Each shared operand `block` reads but binds otherwise, with what it binds.
fn misbound(block: &str) -> Vec<(&'static str, Option<String>)> {
    let text = block_text(block);
    let bound = bindings(block);
    SHARED_OPERANDS
        .iter()
        .filter(|(name, _)| text.contains(&format!("{{{name}}}")))
        .filter(|(name, expected)| bound.get(*name).map(String::as_str) != Some(*expected))
        .map(|(name, _)| (*name, bound.get(*name).cloned()))
        .collect()
}

/// Every block in the port that reads a shared operand binds it to its one
/// definition, so a stub cannot test the right bit of the wrong word.
#[test]
fn every_shared_template_operand_is_bound_to_its_definition() {
    let mut read = BTreeSet::new();
    let mut blocks = 0;
    for &(name, source) in ASM_SOURCES.iter().filter(|(name, _)| !is_assembly(name)) {
        for kind in ["asm", "naked_asm", "global_asm"] {
            for block in invocations(source, kind) {
                blocks += 1;
                let text = block_text(block);
                read.extend(
                    SHARED_OPERANDS
                        .iter()
                        .filter(|(operand, _)| text.contains(&format!("{{{operand}}}")))
                        .map(|(operand, _)| *operand),
                );
                assert_eq!(
                    misbound(block),
                    Vec::<(&str, Option<String>)>::new(),
                    "{name}: `{}`",
                    block.lines().next().unwrap_or(block)
                );
            }
        }
    }
    assert!(blocks > 0, "the scan found no block");
    let all: BTreeSet<&str> = SHARED_OPERANDS.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        read, all,
        "a shared operand no scanned block reads is checked by nothing"
    );
}

#[test]
fn a_block_binding_a_shared_operand_elsewhere_is_caught() {
    let smep = "core::arch::naked_asm!(
        crate::clear_entry_flags!(),
        \"iretq\",
        smap = const $crate::protection::Protections::SMEP.bits(),
        in_force = sym crate::protection::IN_FORCE,
    )";
    assert_eq!(
        misbound(smep),
        [(
            "smap",
            Some(String::from(
                "const crate::protection::Protections::SMEP.bits()"
            ))
        )]
    );
    let unbound = "core::arch::asm!(\"verw {verw_sel}(%rip)\", options(att_syntax))";
    assert_eq!(misbound(unbound), [("verw_sel", None)]);
    let right = "core::arch::asm!(\"verw {verw_sel}(%rip)\", verw_sel = sym crate::sidechannel::VERW_SELECTOR)";
    assert_eq!(misbound(right), []);
}

/// Every stub entered by interrupt, exception, or trap-gate delivery clears
/// the direction flag before it calls into Rust: delivery keeps the
/// interrupted `DF`, and the dispatcher's `rep movs`/`rep stos` read it.
#[test]
fn every_asynchronous_entry_stub_clears_the_direction_flag_before_rust() {
    for anchor in reached_by(Reached::Gate) {
        let body = stub(anchor, &[]);
        let cld = body.iter().position(|i| i == "cld");
        let call = body.iter().position(|i| i.starts_with("call "));
        assert!(cld.is_some(), "{anchor}: no `cld` on entry");
        assert!(
            cld < call,
            "{anchor}: `cld` must precede the first call into Rust"
        );
    }
}

/// Whether `instruction` is a string instruction, which reads `DF`.
fn is_string_op(instruction: &str) -> bool {
    let mnemonic = instruction.split(' ').next().unwrap_or("");
    mnemonic.starts_with("rep")
        || ["movs", "stos", "lods", "cmps", "scas"].iter().any(|op| {
            mnemonic
                .strip_prefix(op)
                .is_some_and(|width| ["b", "w", "l", "q"].contains(&width))
        })
}

/// Every stub entered by gate delivery clears `AC` before its first call into
/// Rust whenever SMAP is in force: delivery keeps the interrupted `AC`, which
/// user code may set, and `clac` is defined only where the CPU offers SMAP, so
/// the stub asks the set in force first.
#[test]
fn every_asynchronous_entry_stub_clears_the_alignment_check_flag_under_smap() {
    for anchor in reached_by(Reached::Gate) {
        let body = stub(anchor, &[]);
        let call = body
            .iter()
            .position(|i| i.starts_with("call "))
            .unwrap_or_else(|| panic!("{anchor}: no call into Rust"));
        let clac = body
            .iter()
            .position(|i| i == "clac")
            .unwrap_or_else(|| panic!("{anchor}: no `clac` on entry"));
        assert!(
            clac < call,
            "{anchor}: `clac` must precede the first call into Rust"
        );
        // What `{smap}` and `{in_force}` are bound to is pinned by
        // `every_shared_template_operand_is_bound_to_its_definition`.
        assert_eq!(
            body[clac - 2..=clac + 1],
            ["testb ${smap}, {in_force}(%rip)", "jz 4f", "clac", "4:"],
            "{anchor}: `clac` must run exactly when SMAP is in force"
        );
    }
}

/// Whether `path[at]` leaves the direction flag clear: a `cld`, or the
/// `popfl` of a whole `RFLAGS` value with it clear.
fn clears_direction_flag(path: &[String], at: usize) -> bool {
    path[at] == "cld" || (path[at] == "popfl" && at > 0 && path[at - 1] == "pushl $0x2")
}

/// Each way the CPU first enters the kernel — the multiboot2 `_start`, the PVH
/// `pvh_start`, an application processor's start-up trampoline — clears the
/// direction flag before its first call into Rust or string instruction, since
/// no boot protocol defines it. Read in file order, which the entries'
/// straight-line jumps follow.
#[test]
fn the_boot_entries_clear_the_direction_flag_before_rust() {
    for entry in reached_by(Reached::Boot) {
        let (file, lines, from) = definition(entry);
        let path = &lines[from..];
        let cleared = (0..path.len()).find(|&at| clears_direction_flag(path, at));
        let first_reader = path
            .iter()
            .position(|i| i.starts_with("call") || is_string_op(i))
            .unwrap_or_else(|| panic!("{file}: `{entry}` reaches no call"));
        assert!(
            cleared.is_some_and(|cleared| cleared < first_reader),
            "{file}: `{entry}` reaches `{}` before clearing the direction flag",
            path[first_reader]
        );
    }
}

/// The boot CPU's entry writes `RFLAGS` whole once it has a stack, ahead of
/// any call or string instruction: neither boot protocol defines it beyond
/// `VM` and `IF`, and an inherited `AC` would leave every user page open to
/// the kernel under SMAP. Nothing else in the entry loads `RFLAGS`.
#[test]
fn the_boot_entry_writes_rflags_whole() {
    let (_, source) = ASM_SOURCES
        .iter()
        .find(|(name, _)| *name == "boot.s")
        .unwrap_or_else(|| panic!("boot.s is not inspected"));
    let lines = instructions("boot.s", source);
    let from = lines
        .iter()
        .position(|line| line == "boot_common:")
        .unwrap_or_else(|| panic!("boot.s: no `boot_common:`"));
    let path = &lines[from..];
    let stack = path
        .iter()
        .position(|i| i == "movl $boot_stack_top, %esp")
        .unwrap_or_else(|| panic!("boot.s: no boot stack"));
    let write = path
        .windows(2)
        .position(|pair| pair == ["pushl $0x2", "popfl"])
        .unwrap_or_else(|| panic!("boot.s: `RFLAGS` is not written whole"));
    let first_reader = path
        .iter()
        .position(|i| i.starts_with("call") || is_string_op(i))
        .unwrap_or_else(|| panic!("boot.s: `boot_common` reaches no call"));
    assert!(
        stack < write,
        "boot.s: `RFLAGS` is written before there is a stack"
    );
    assert!(
        write < first_reader,
        "boot.s: `{}` runs first",
        path[first_reader]
    );
    assert_eq!(
        lines.iter().filter(|i| i.starts_with("popf")).count(),
        1,
        "boot.s loads `RFLAGS` once"
    );
}

/// The boot path leaves nothing on the stack it replaces `%rsp` over when it
/// re-addresses the stack through the kernel window.
#[test]
fn the_boot_path_leaves_nothing_on_the_stack_it_readdresses() {
    let (_, source) = ASM_SOURCES
        .iter()
        .find(|(name, _)| *name == "boot.s")
        .unwrap_or_else(|| panic!("boot.s is not inspected"));
    let lines = instructions("boot.s", source);
    let from = lines
        .iter()
        .position(|line| line == "boot_common:")
        .unwrap_or_else(|| panic!("boot.s: no `boot_common:`"));
    let to = lines
        .iter()
        .position(|line| line == "movabsq $boot_stack_top_high, %rsp")
        .unwrap_or_else(|| panic!("boot.s: the stack is never re-addressed"));
    let path = &lines[from..to];
    let count = |prefix: &str| path.iter().filter(|i| i.starts_with(prefix)).count();
    assert_eq!(count("push"), count("pop"), "boot.s: a pushed word is live");
    assert_eq!(count("call"), 0, "boot.s: a return address is live");
}

/// On the start-up root an AP runs only the FPU enable and its APIC mode
/// before its entry, which is what lets the docs say what runs there.
#[test]
fn an_ap_runs_only_its_fpu_enable_and_apic_mode_before_its_entry() {
    let (_, source) = ASM_SOURCES
        .iter()
        .find(|(name, _)| *name == "ap_trampoline.s")
        .unwrap_or_else(|| panic!("ap_trampoline.s is not inspected"));
    let lines = instructions("ap_trampoline.s", source);
    let called: Vec<&str> = lines
        .windows(2)
        .filter(|pair| pair[1] == "call *%rax")
        .filter_map(|pair| pair[0].strip_prefix("movabsq $")?.strip_suffix(", %rax"))
        .collect();
    assert_eq!(
        called,
        [
            "tairix_arch_x86_64_fpu_enable",
            "tairix_arch_x86_64_ap_apic_mode",
            "tairix_arch_x86_64_ap_entry",
        ]
    );
    assert_eq!(
        lines.iter().filter(|i| i.starts_with("call")).count(),
        called.len(),
        "ap_trampoline.s: a call is not one of the three"
    );
}

/// Each boot entry establishes `CR0` rather than amending it: INIT leaves an
/// application processor's caches disabled, and multiboot2 leaves the boot
/// CPU's other bits undefined, so a value read back would carry either into
/// the kernel.
#[test]
fn every_boot_entry_writes_cr0_whole() {
    let files: BTreeSet<&str> = reached_by(Reached::Boot)
        .map(|entry| definition(entry).0)
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    for file in files {
        let (_, source) = ASM_SOURCES
            .iter()
            .find(|(name, _)| *name == file)
            .unwrap_or_else(|| panic!("{file} is not inspected"));
        let lines = instructions(file, source);
        assert!(
            !lines.iter().any(|i| i.starts_with("movl %cr0,")),
            "{file} reads CR0 back"
        );
        let writes: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, i)| i.ends_with(", %cr0"))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(writes.len(), 1, "{file} writes CR0 once");
        let value = &lines[writes[0] - 1];
        assert!(
            value.starts_with("movl $") && value.ends_with(", %eax"),
            "{file}: CR0 comes from `{value}`, not a whole value"
        );
    }
}

/// One way through a boot entry's arming of `EFER`, and what it did.
#[derive(Clone, Debug, Default)]
struct ArmingPath {
    /// What the last execute-disable test found: offered or not.
    nx: Option<bool>,
    /// The Intel vendor words matched so far.
    intel: u32,
    /// Whether the last XD Bit Disable clear found the control set.
    xd_was_set: bool,
    /// Whether the instruction naming `EFER.NXE` ran.
    arms_nxe: bool,
    /// Each MSR written, by its operand, with the Intel words and the XD
    /// finding at the time.
    written: Vec<(String, u32, bool)>,
    /// Whether it stopped at a halt rather than writing `EFER`.
    halted: bool,
}

/// `lines` as the assembler reads them for the GAS macro `name`: its
/// definition taken out, and each call replaced by the body the inspected
/// sources define, its arguments and its own `\@` put in as GAS puts them.
fn expanded(lines: &[String], name: &str) -> Vec<String> {
    let header = format!(".macro {name}");
    let (_, defining, at) = definition(&header);
    let params: Vec<String> = defining[at]
        .split_whitespace()
        .skip(2)
        .map(|param| param.trim_end_matches(',').to_string())
        .collect();
    let body: Vec<&String> = defining[at + 1..]
        .iter()
        .take_while(|line| *line != ".endm")
        .collect();
    let mut out = Vec::new();
    let mut expansions = 0;
    let mut defined = false;
    for line in lines {
        if line.starts_with(&header) {
            defined = true;
            continue;
        }
        if defined {
            defined = line != ".endm";
            continue;
        }
        let Some(arguments) = line
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(' '))
        else {
            out.push(line.clone());
            continue;
        };
        let arguments: Vec<&str> = arguments.split(',').map(str::trim).collect();
        assert_eq!(arguments.len(), params.len(), "`{line}` against `{header}`");
        for instruction in &body {
            let mut instruction = instruction.replace("\\@", &expansions.to_string());
            for (param, argument) in params.iter().zip(&arguments) {
                instruction = instruction.replace(&format!("\\{param}"), argument);
            }
            out.push(instruction);
        }
        expansions += 1;
    }
    out
}

/// Every path from `file`'s execute-disable test to its `EFER` write or a
/// halt, following both ways of each conditional jump.
fn arming_paths(file: &str, lines: &[String]) -> Vec<ArmingPath> {
    const NX_TEST: &str = "btl ${EXT1_EDX_NX}, %edx";
    const XD_CLEAR: &str = "btrl ${XD_DISABLE_HIGH}, %edx";
    let label = |name: &str| {
        let name = format!("{name}:");
        lines
            .iter()
            .position(|line| *line == name)
            .unwrap_or_else(|| panic!("{file}: no label `{name}`"))
    };
    let start = lines
        .iter()
        .position(|i| i == "movl $0x80000001, %eax")
        .unwrap_or_else(|| panic!("{file}: no execute-disable test"));
    let mut paths = Vec::new();
    let mut pending = std::vec![(start, ArmingPath::default(), None::<String>)];
    while let Some((at, mut path, mut msr)) = pending.pop() {
        let mut at = at;
        loop {
            assert!(
                paths.len() + pending.len() < 64,
                "{file}: the arming does not end"
            );
            let line = lines
                .get(at)
                .unwrap_or_else(|| panic!("{file}: the arming runs off the source"));
            if line.contains("{EFER_NXE}") {
                path.arms_nxe = true;
            }
            let (mnemonic, operands) = line.split_once(' ').unwrap_or((line, ""));
            match mnemonic {
                "hlt" => {
                    path.halted = true;
                    paths.push(path);
                    break;
                }
                "cpuid" => msr = None,
                "wrmsr" => {
                    let written = msr
                        .clone()
                        .unwrap_or_else(|| panic!("{file}: a write to no named MSR"));
                    path.written
                        .push((written.clone(), path.intel, path.xd_was_set));
                    if written == "{IA32_EFER}" {
                        paths.push(path);
                        break;
                    }
                }
                "movl" if operands.ends_with(", %ecx") => {
                    msr = operands
                        .strip_prefix('$')
                        .and_then(|value| value.strip_suffix(", %ecx"))
                        .filter(|value| value.starts_with('{'))
                        .map(String::from);
                }
                "jmp" => {
                    at = label(operands);
                    continue;
                }
                "jc" | "jnc" | "jne" => {
                    let previous = lines[at - 1].as_str();
                    let mut taken = path.clone();
                    let mut through = path;
                    if previous == NX_TEST {
                        taken.nx = Some(mnemonic == "jc");
                        through.nx = Some(mnemonic == "jnc");
                    } else if previous.starts_with("cmpl ${INTEL_") && mnemonic == "jne" {
                        through.intel += 1;
                    } else if previous == XD_CLEAR && mnemonic == "jnc" {
                        taken.xd_was_set = false;
                        through.xd_was_set = true;
                    } else {
                        panic!("{file}: `{line}` follows `{previous}`, which no rule reads");
                    }
                    pending.push((label(operands), taken, msr.clone()));
                    path = through;
                }
                _ => {}
            }
            at += 1;
        }
    }
    paths
}

/// Each boot entry sets `EFER.NXE` only where the CPU offers execute-disable,
/// which is otherwise a reserved bit whose setting faults with no table to
/// report the fault. An Intel part's firmware can withhold it, so that
/// control is cleared first, only on an Intel part and only where set. An AP
/// still without it halts unpaged; the boot CPU goes on to its Rust entry,
/// which refuses it with a report. Every constant is a named operand.
#[test]
fn every_boot_entry_arms_execute_disable_only_where_it_is_offered() {
    for file in ["boot.s", "ap_trampoline.s"] {
        let (_, source) = ASM_SOURCES
            .iter()
            .find(|(name, _)| *name == file)
            .unwrap_or_else(|| panic!("{file} is not inspected"));
        let lines = expanded(&instructions(file, source), "nx_probe");
        assert!(
            !lines
                .iter()
                .any(|i| i.contains("0xC0000080") || i.contains("<< 11") || i.contains("0x1A0")),
            "{file}: an MSR or `EFER` bit is spelled rather than passed in"
        );
        let paths = arming_paths(file, &lines);
        for path in &paths {
            assert!(
                !path.arms_nxe || path.nx == Some(true),
                "{file}: NXE is armed where execute-disable is not offered: {path:?}"
            );
            for (msr, intel, xd_was_set) in &path.written {
                if msr == "{IA32_MISC_ENABLE}" {
                    assert!(
                        *intel == 3 && *xd_was_set,
                        "{file}: the XD control is written on a path that is not an Intel part's \
                         with the control set: {path:?}"
                    );
                }
            }
            let order: Vec<&str> = path.written.iter().map(|(msr, ..)| msr.as_str()).collect();
            assert!(
                matches!(
                    order.as_slice(),
                    [] | ["{IA32_EFER}" | "{IA32_MISC_ENABLE}"]
                        | ["{IA32_MISC_ENABLE}", "{IA32_EFER}"]
                ),
                "{file}: MSRs written out of order: {order:?}"
            );
            if file == "ap_trampoline.s" {
                assert_eq!(
                    path.halted, !path.arms_nxe,
                    "{file}: an AP enters long mode without execute-disable, or halts with it: \
                     {path:?}"
                );
            } else {
                assert!(
                    !path.halted,
                    "{file}: the boot CPU halts unreported: {path:?}"
                );
            }
        }
        assert!(
            paths.iter().any(|path| path.arms_nxe),
            "{file}: no path arms NXE"
        );
        assert!(
            paths.iter().any(|path| path.nx == Some(false)),
            "{file}: no path finds execute-disable withheld"
        );
        assert!(
            paths.iter().any(|path| path
                .written
                .iter()
                .any(|(msr, ..)| msr == "{IA32_MISC_ENABLE}")),
            "{file}: no path clears XD Bit Disable"
        );
    }
}

/// Mnemonics that leave what `preserves_flags` covers as it was — the status
/// flags of `RFLAGS`, the x87 status word and `MXCSR`'s exception flags —
/// whatever their operands: with [`leaves_status_flags`], the only ones a block
/// telling the compiler it preserves them may run.
const LEAVES_STATUS_FLAGS: &[&str] = &[
    "clflush",
    "cli",
    "fxsave64",
    "hlt",
    "in",
    "invlpg",
    "lea",
    "lfence",
    "lgdt",
    "lidt",
    "ltr",
    "mfence",
    "out",
    "pop",
    "push",
    "pushfq",
    "rdmsr",
    "rdtsc",
    "retfq",
    "sti",
    "swapgs",
    "wrmsr",
    "xsave64",
    "xsaveopt64",
    "xsetbv",
];

/// Whether `instruction` names a control or debug register as an operand.
fn names_control_or_debug_register(instruction: &str) -> bool {
    instruction
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|word| word.trim_start_matches('%').to_ascii_lowercase())
        .any(|word| {
            ["cr", "dr"].iter().any(|prefix| {
                word.strip_prefix(prefix).is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
                })
            })
        })
}

/// Whether `instruction` leaves what `preserves_flags` covers as it was: a
/// move to or from a control or debug register leaves the status flags
/// undefined.
fn leaves_status_flags(instruction: &str) -> bool {
    match mnemonic(instruction) {
        None => true,
        Some("mov") => !names_control_or_debug_register(instruction),
        Some(op) => LEAVES_STATUS_FLAGS.contains(&op),
    }
}

#[test]
fn a_move_to_or_from_a_control_or_debug_register_writes_the_flags() {
    for writes in [
        "mov cr3, {p}",
        "mov {t}, cr3",
        "mov {v}, cr8",
        "mov dr7, {t}",
        "movq %cr2, %rsi",
        "mov CR3, {p}",
        "ldmxcsr [{p}]",
    ] {
        assert!(!leaves_status_flags(writes), "{writes}");
    }
    for leaves in [
        "mov {t}, rsp",
        "mov ds, ax",
        "mov ax, {kds:x}",
        "mov {tls}, gs:[{off}]",
        "mov {cr3}, rax",
        "2:",
    ] {
        assert!(leaves_status_flags(leaves), "{leaves}");
    }
}

#[test]
fn the_flag_scan_reads_templates_and_not_operand_registers() {
    let source = r##"
        core::arch::global_asm!(r#"
            // a lone " and a (
            orl %eax, %eax
        "#);
        core::arch::asm!("mov {t}, cr4", "or {t}, {bit}", "mov cr4, {t}",
            t = out(reg) _, options(nostack, preserves_flags));
        unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") v) };
    "##;
    let blocks = invocations(source, "asm");
    assert_eq!(blocks.len(), 2, "`global_asm!` is not an inline block");
    let global = invocations(source, "global_asm");
    assert_eq!(global.len(), 1);
    assert!(global[0].ends_with("\"#)"), "cut at its own parenthesis");
    let template = template_instructions(blocks[0]);
    assert_eq!(template, ["mov {t}, cr4", "or {t}, {bit}", "mov cr4, {t}"]);
    assert_eq!(mnemonic(&template[1]), Some("or"));
    assert!(!leaves_status_flags(&template[1]));
    assert_eq!(template_instructions(blocks[1]), ["in al, dx"]);
}

/// A block telling the compiler it preserves the status flags runs nothing that
/// writes them, or the compiler may keep a comparison's outcome in `RFLAGS`
/// across it and branch on the block's leftovers.
#[test]
fn no_block_claiming_to_preserve_the_flags_writes_them() {
    let mut claims = 0usize;
    for &(name, source) in ASM_SOURCES.iter().filter(|(name, _)| !is_assembly(name)) {
        for block in invocations(source, "asm")
            .into_iter()
            .filter(|block| block.contains("preserves_flags"))
        {
            claims += 1;
            for instruction in template_instructions(block) {
                assert!(
                    leaves_status_flags(&instruction),
                    "{name}: `{instruction}` may write the flags its block claims to preserve"
                );
            }
        }
    }
    assert!(claims > 0, "the scan found no block to check");
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
