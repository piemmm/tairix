# `tairix-codegen` — the code generator

`tairix_codegen` (`lib/codegen`) compiles a function of its SSA IR to
machine code for x86_64, aarch64 or riscv64. It is the one generator
TAIRiX has: the PC emulator's translator (`plans/PCEMU.md` PC18), the
browser's compiled JavaScript tiers and the Wasm tier
(`plans/BROWSER.md` decision 6) all build IR and hand it here. The crate
generates bytes and nothing else — it never maps them executable, runs
them, or touches memory they compute — so it is `no_std` + `alloc` with
`#![forbid(unsafe_code)]`, and the compiling path can run in a sandbox
holding no authority.

Stability tier: **experimental**.

## The IR

A `Function` is blocks of SSA values: 32-bit (`I32`) and 64-bit (`I64`)
integers, each defined once, by an instruction or as a block's parameter.
The entry block's parameters (`Block::ENTRY`) are the arguments, at most
six. Instructions are constants, unary and binary arithmetic and logic,
shifts and rotates, comparisons giving `0` or `1`, selects, widening and
narrowing, loads and stores of 1–8 bytes at a base plus an offset, and
calls of helper `n` of a table whose address is a value. A block ends in a
jump or a branch, each edge giving its target's parameters their values,
or a return.

Every operation is total, so no input value makes generated code trap:
division by zero gives zero and its remainder the dividend, the one signed
division that overflows wraps, and a shift or rotate counts modulo the
width. `interp::run` is the reference meaning every backend is held to.

`ir::Builder` makes a function block by block and keeps a misuse — an
instruction with no block begun, a block ended twice, more values than 32
bits number — for `finish` to refuse. `verify::verify` refuses an
ill-formed function: a value defined twice or not at all, a use its
definition does not dominate, an operand of the wrong type, a load or store
wider than its value, an edge giving its block the wrong arguments, an edge
into the entry block, a return of the wrong type, more than six arguments.
`compile` verifies first; a function the verifier admits compiles unless it
outruns a target's reach.

## Compiling

1. **Order.** The blocks the entry reaches, in reverse postorder; an
   unreachable block is never compiled.
2. **Uses.** An instruction with no effect whose value nothing reads is
   dropped, transitively, block parameters included. A constant read only
   where the target encodes it is never materialized: as an immediate; on
   x86_64 also as a first operand or a select's second choice moved into
   the result by `mov`, and as a stored value (`mov m, imm`); on aarch64
   and riscv64 a zero chosen or stored is the zero register. A branch
   whose condition is a comparison in its own block tests the comparison
   directly (`cmp; jcc`, `cmp; b.cond`, `blt`), and on x86_64 and aarch64
   so does a select (`cmp; cmovcc`, `cmp; csel`), so the boolean exists
   only if something else reads it. Loads, stores and calls are always
   kept: a load may trap.
3. **Allocation.** Linear scan (Poletto and Sarkar) over one interval per
   needed value, found by walking back from each read to the definition
   (Brandner et al.), so the work follows the liveness rather than blocks
   times values. A value live across a call prefers a register the call
   keeps; in one it does not, the call saves it to a save area around
   itself, so a call on a path seldom taken costs nothing elsewhere. With
   no register free, the interval read least for its length — this one or
   one holding a register — takes a slot, so a value every step reads
   keeps its register however long it lives. An interval ending where
   another begins does not lend it its register. Stack slots are given
   once registers are, each spilled value over its whole span, greedily by
   start — the fewest slots any assignment can use — and the save area,
   as large as the most any one call saves, follows them.
4. **Lowering.** Arguments move from the argument registers to where they
   live; each edge's arguments move to its block's parameters as one
   parallel move, cycles broken through a scratch register. A branch falls
   into whichever successor is laid out next, its comparison inverted
   where that is the second; where the edge jumped to moves values, its
   moves go after the last block.
5. **Encoding.** A fixed-width target's branches take the shortest form
   that reaches (aarch64: `b.cond`, then an inverted branch over `b`;
   riscv64: `bxx`, over `jal`, then over `auipc` and `jalr`). A function
   past a target's longest reach is refused (`CompileError::TooLarge`),
   never miscompiled.

Output is position-independent and follows the target's C calling
convention: arguments arrive as 64-bit words, an `I32` zero-extended; an
`I32` answer is returned zero-extended; helpers are called the same way.
Each function makes a frame record, so the system's frame-pointer
backtrace passes through generated code.

| Target | Convention | Allocated (caller-, callee-saved) | Scratch | Frame pointer | Never written |
|---|---|---|---|---|---|
| x86_64 | System V | RSI RDI R8 R9; RBX R12–R15 | RAX RCX RDX R10 R11 | RBP | — |
| aarch64 | AAPCS64 | X0–X14; X19–X28 | X15 X16 X17 | X29 | X18 |
| riscv64 | LP64, RV64GC | A0–A7 T4–T6; S1–S11 | T0–T3 | S0 | GP TP |

x86_64 guards division by zero and the overflowing signed division, which
the processor refuses; aarch64's division is already total; riscv64 masks
the all-ones quotient a zero divisor gives. riscv64 holds an `I32`
sign-extended, as its word instructions leave it, and converts where the
convention passes one; the others hold it zero-extended.

## Targets

A `Target` is an instruction set (`Isa`) and the extensions of it that every
core the code may run on has. `Target::of(arch, features)` makes one from
the architecture the kernel attests and the feature set common to every
core, which a process reads from its startup vector
(`tairix_rt::cpu_features`); the backend is never chosen by how the crate
was built. A target keeps only the extensions its backend lowers to, so two
equal targets compile alike, and `Target::ALL` names every target a backend
lowers differently.

| Target | Extension | Lowered to |
|---|---|---|
| x86_64 | BMI2 | A shift by a register count is `shlx`, `shrx` or `sarx`: the count stays in whichever register holds it, and the result is written to a register of its own, so no move into CL or onto the result. A rotate by a constant is `rorx`. |
| riscv64 | Zbb | A rotation is `rol`, `ror` or `rori` (`rolw`, `rorw`, `roriw` for an `I32`), and a 16-bit or signed 8-bit extension `zext.h`, `sext.b` or `sext.h`, in place of a shift each way. |

## Proof

Each backend lowers to a list of its own machine instructions, the
encoder's only input, so a backend is tested twice over:

- The list runs on a model of its machine (`model::run`) against the
  interpreter on the same inputs, comparing the answer, the memory left
  and every helper call. A model refuses what hardware leaves to chance: a
  register read before anything defined it, flags read where nothing set
  them, a misaligned stack or missing frame record at a call, a
  callee-saved register not restored, a write to a reserved register; and
  a call leaves the caller-saved registers and everything below the stack
  pointer undefined.
- Every instruction is encoded and decoded again by `lib/disasm`, its text
  compared with a rendering written from what the instruction means.

The conformance checks every backend passes are random functions that keep
many values live across calls, branch and loop (`testgen`), division and
shifts on their edges, immediates either side of each target's encodings,
offsets either side of each addressing form, and frames beyond the
immediate reach of a load. Each runs on every target of its instruction
set, with and without each extension, so an extension's lowering is held
to the interpreter as the base is. `fuzz_codegen` runs every target against
the interpreter and builds hostile functions — ill-typed, using another
function's values, jumping into the entry — that must be refused or
compiled, never a panic.

Compile cost is linear in the function's size and its liveness; a caller
bounds the functions it hands in.
