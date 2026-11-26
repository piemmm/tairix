# tairix-wgsl

Stability tier: **experimental**

The WGSL front end (`plans/SHADER.md` SH5): WebGPU Shading Language source,
hostile by assumption, compiled to a SPIR-V module `lib/spirv`'s validator
admits.

## What it provides

- `compile` — source and the values its overrides take (`Options`,
  `OverrideKey`) to a `Compiled` module and the warnings and notes its
  diagnostic filters leave, or a `Failure`: the first shader-creation error as
  a `Diagnostic` (a closed `Code`, its `Span`, the line and column it begins
  at, and a message of fixed text and the source's own names), memory refused,
  or an internal invariant failed — a defect in the front end, never an answer
  about the source.
- `position` — the line and column a byte of a source begins, as a
  diagnostic reports them.

## Design notes

- The language is the W3C specification's core whole, with the extensions
  whose every construct lands on a capability the validator admits; the
  others are refused by name. What SPIR-V leaves undefined is written out as
  WGSL defines it, `discard` demotes, and override values are given at compile
  time.
- Bounded: the specification's minimum limits, and the front end's own —
  `MAX_SOURCE_BYTES`, `MAX_SYNTAX_NODES`, `MAX_OUTPUT_BYTES`,
  `MAX_EXPRESSION_NESTING`, `MAX_DIAGNOSTICS`, a fixed constant-evaluation
  budget and a fixed budget for each function's uniformity analysis — which
  hold the largest compile of every shape, however densely spelled or deeply
  nested, within a message-sized sandbox worker. A source past one is
  refused, never partly compiled.
- Deterministic: one source and one set of override values write one
  sequence of bytes.
- Not trusted: the module it writes is admitted again by its caller, and
  guarded by `lib/spirv` where a device needs it, so a front-end bug is a
  correctness problem rather than a security one. On TAIRiX it runs only in
  `lib/sandbox`'s `wgsl` worker.
- No `unsafe`.

## Tests

Unit tests hold the lexer, parser, constant evaluator and checks to the
specification's text — each shader-creation error refused with its code,
uniformity among them — and the emitter's modules, of every construct, to
admission. `tests/run.rs` compiles each construct, runs it on the software
device, and holds it to the value the specification gives, the guarded
module beside it where indexing shows. `tests/fuzz_wgsl.rs` mutates valid
modules and holds the front end total, its refusals within the source, every
compiled module admitted and guarded, every compile deterministic, and random
integer programs to a reference evaluation of WGSL's definitions.
`tests/compile_memory.rs` holds the shapes that could multiply what the
uniformity analysis keeps — long chains, switches, loops and functions of
many exits, and calls of pointers fed by one another — to twice what their
statements hold without them.
