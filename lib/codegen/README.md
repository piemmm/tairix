# tairix-codegen

The one code generator (`lib/codegen`): an SSA IR, its verifier and
reference interpreter, a linear-scan register allocator, and backends for
x86_64, aarch64 and riscv64. The PC emulator's translator is its first
consumer; the browser's compiled tiers and the Wasm tier share it
(`plans/BROWSER.md` decision 6, `plans/PCEMU.md` PC18).

- **IR.** 32- and 64-bit integer values in blocks with parameters. Every
  operation is total: division by zero gives zero, its remainder the
  dividend; signed overflow wraps; shifts and rotates count modulo the
  width. Helpers are called through a table the function is given.
- **Verifier.** Refuses an ill-typed or ill-formed function — a value
  defined twice or not at all, a use its definition does not dominate, an
  edge giving the wrong arguments, an edge into the entry block.
- **Compile.** Dead pure instructions dropped, constants folded where the
  target encodes them, a branch — and on x86_64 and aarch64 a select — on
  its own block's comparison fused into it, values allocated by linear
  scan (spilling the one read least for its length, a call saving the
  caller-saved registers live across it), code laid out in reverse
  postorder with branches falling into the next block and relaxed to
  their reach. Output is position-independent and follows the target's C
  calling convention; each function makes a frame record so a
  frame-pointer walk passes through it.
- **Targets.** An instruction set and the extensions every core the code
  may run on has, from the architecture the kernel attests and the
  startup vector's common feature set: BMI2's register-count shifts and
  `rorx` on x86_64, Zbb's rotations and extensions on riscv64.
- **Proof.** Each backend lowers to a list of its own machine
  instructions. That list runs on a model of its machine against the
  interpreter, with and without each extension, and every instruction is
  encoded and decoded again by `lib/disasm`. `fuzz_codegen` is enrolled in
  `cargo xtask fuzz`.

`no_std` + `alloc`, `#![forbid(unsafe_code)]`: it generates code and never
runs it. The machine models and the random-function generator are behind
the `test-util` feature.

## Stability

Tier: `experimental`.
