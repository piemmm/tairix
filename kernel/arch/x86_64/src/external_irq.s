// x86_64 external-IRQ ISR stubs (Stage 4.D Item 2-tail.2).
//
// Reserves architectural vectors 0x30..=0xFE for external IRQs. Each vector
// points to its own tiny stub that pushes the vector number as an immediate
// and jumps to the shared trampoline, `tairix_arch_x86_64_external_irq_common`
// in `irq.rs`, which saves and restores the interrupted context around the
// Rust dispatcher.
//
// SAFETY-INVARIANTs:
//
//   1. None of the targeted vectors (0x30..=0xFE) push a hardware
//      error code, so the synthetic-vector qword sits at a known
//      offset and the per-vector immediate fits in a sign-extended
//      i32 (every value is ≤ 0xFE).
//   2. The stub addresses are published through the
//      `tairix_arch_x86_64_external_irq_table` data array (one
//      `.quad` per vector, indexed by `vector - EXTERNAL_VECTOR_FIRST`).
//      Rust references it via `extern "C"` so the IDT installer in
//      `kernel/tairix-kernel::boot` can take the address of each stub
//      without having to know its name.
//   3. The IDT entries are interrupt gates at DPL 0, so the stubs and the
//      trampoline run with interrupts disabled.

.section .text

// --- Per-vector stubs ---------------------------------------------
//
// Generated through `.altmacro` + `.rept` so (no
// duplication) is satisfied. The macro produces one labelled stub
// per vector in [EXTERNAL_VECTOR_FIRST, EXTERNAL_VECTOR_LAST]
// (0x30..=0xFE inclusive — 207 vectors).
//
// Each stub is exactly two instructions: push the vector as an
// immediate, then jmp to the shared trampoline. The `push imm8`
// encoding the GNU assembler emits for values in 0..=0x7F is two
// bytes; values 0x80..=0xFE use `push imm32` and are five bytes.
// The size difference is irrelevant because Rust never assumes a
// fixed stride — the per-vector addresses are published through the
// `tairix_arch_x86_64_external_irq_table` data array below.

.altmacro

.macro external_irq_stub vec
    .global tairix_arch_x86_64_external_irq_\vec
    .type   tairix_arch_x86_64_external_irq_\vec, @function
tairix_arch_x86_64_external_irq_\vec:
    pushq   $\vec
    jmp     tairix_arch_x86_64_external_irq_common
    .size tairix_arch_x86_64_external_irq_\vec, . - tairix_arch_x86_64_external_irq_\vec
.endm

.macro external_irq_table_entry vec
    .quad tairix_arch_x86_64_external_irq_\vec
.endm

.set    vec_no, 0x30
.rept   (0xFF - 0x30)
    external_irq_stub %vec_no
    .set vec_no, vec_no + 1
.endr

// --- Vector table -------------------------------------------------
//
// One `.quad` per vector, in ascending vector order, indexed by
// `vector - EXTERNAL_VECTOR_FIRST`. Published in `.rodata` (the
// addresses never change after link time) so the Rust side can read
// it as `extern "C" static EXTERNAL_VECTOR_TABLE: [usize; EXTERNAL_VECTOR_COUNT]`.
//
// The label-substitution happens through the same `.altmacro` `%vec_no`
// expansion the per-vector stubs use, but `%var` only expands inside
// macro bodies — emitting `.quad` directly with `%vec_no` would leave
// the literal `%vec_no` text in the assembled stream and fail with
// "expected relocatable expression". Wrapping the `.quad` in a single-
// argument macro (`external_irq_table_entry`) puts the label inside a
// macro body, which is the form `.altmacro` is documented to handle.

.section .rodata
.global tairix_arch_x86_64_external_irq_table
.type   tairix_arch_x86_64_external_irq_table, @object

tairix_arch_x86_64_external_irq_table:
.set    vec_no, 0x30
.rept   (0xFF - 0x30)
    external_irq_table_entry %vec_no
    .set vec_no, vec_no + 1
.endr

.size tairix_arch_x86_64_external_irq_table, . - tairix_arch_x86_64_external_irq_table
