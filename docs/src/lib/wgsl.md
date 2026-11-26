# `tairix-wgsl` — the WGSL front end

`lib/wgsl` compiles WebGPU Shading Language source to a SPIR-V module
`lib/spirv`'s validator admits (`plans/SHADER.md` SH5). Source is untrusted
input — an application supplies it, and it may have crossed a network — so the
front end runs only in `lib/sandbox`'s `wgsl` worker, and its caller believes
nothing it answers until the validator has admitted the module again. Its bugs
are therefore a correctness problem, not a security one.

Stability tier: **experimental**.

## Compiling

`compile(source, &Options)` answers a `Compiled` — the module's bytes and the
warnings and notes the source's diagnostic filters leave, at most
`MAX_DIAGNOSTICS` — or a `Failure`:

| `Failure` | Means |
|---|---|
| `Error(Diagnostic)` | the first shader-creation error: its `Code`, its `Span`, the line and column it begins at, and a message of fixed text and the source's own names |
| `OutOfMemory` | the allocator refused memory the compile needed |
| `Internal` | one of the front end's own invariants failed: a defect in it, never an answer about the source |

`Options::overrides` gives each override its value, named by an `OverrideKey`
— its declared name or its `@id` — as the WebGPU API gives them: a number,
converted to the override's type. An override given no value takes its
default; one with neither is an error naming it. `position` answers the line
and column, each from one, that a byte of a source begins at: a line ends at
each line break WGSL names, and a column counts code points.

A compile is **deterministic**: one source and one set of override values
write one sequence of bytes and one set of diagnostics.

## The language

The specification's core, whole, and the extensions whose every construct
lands on a capability the validator admits. `enable f16` and
`dual_source_blending` are offered; `clip_distances`, `subgroups`,
`subgroup_size_control`, `primitive_index` and `atomic_vec2u_min_max` are
refused by name, as are the language extensions that need them or a device
feature the portable layout does not assume.

What SPIR-V leaves undefined is written out as WGSL defines it — an integer
divided by zero is its dividend, a shift takes its amount modulo the width, a
float converted to an integer saturates — so every backend computes one
answer. `discard` demotes: the invocation runs on as a helper with its
storage writes masked, so its quad's derivatives stay defined, and ends where
its entry point returns. Override values are constants of the module, so an
override may size an array or set a workgroup size.

## Bounds

The specification's minimum limits — 1,023 structure members, 127 levels of
brace-enclosed statements, 255 function parameters, 1,023 case selectors —
and the front end's own: `MAX_SOURCE_BYTES`, and `MAX_SYNTAX_NODES`
expressions and statements however densely the source spells them;
`MAX_EXPRESSION_NESTING`; a fixed constant-evaluation budget; a fixed budget
of nodes and edges for each function's uniformity analysis, which bounds how
deep its loops and selections nest over how many variables; and a written
module of at most `MAX_OUTPUT_BYTES`. A compile of the largest source of every
shape that costs memory — dense spellings and deep nests among them — holds,
with its request, within the sandbox worker's message-sized memory, which
`lib/sandbox`'s `wgsl_memory` test measures. A source past a bound is refused
as too large, never partly compiled.

The uniformity analysis joins a variable only at the values it can hold where
control meets, so a long `else if` chain, a switch of many clauses, or a loop
or function of many exits costs what its statements assign, not its variables
times its branches or exits; and a call's pointer arguments reach what their
callee's tags say feeds them as the graph is walked, so a call holds its
arguments, not their square.

## Tests

The lexer, parser, constant evaluator and checks are held to the
specification's text, each shader-creation error refused with its code; the
emitter's modules of every construct are admitted. `tests/run.rs` runs each
construct on the software device and holds it to the value the specification
gives, guarded where indexing shows. `tests/fuzz_wgsl.rs` mutates valid
modules: nothing panics, a refusal lies within the source at the place it
names, every compiled module is admitted and guarded, every compile is
deterministic, and random integer programs — SPIR-V's undefined cases among
them — run to exactly a reference evaluation of WGSL's definitions.
`tests/compile_memory.rs` holds a chain of branches, a switch of clauses, a
loop of exits, a function of returns and calls of pointers fed by one another,
each over two thousand variables or every pointer a function takes, to twice
what compiling the same statements holds without that shape.

`cargo xtask miri` interprets the library's tests and `tests/run.rs`'s, each
dealt across the host's cores, and the fuzz harness at a handful of rounds. It
leaves out three whose extent is their assertion — a hundred thousand
dependencies ordered, and twenty thousand arms, operators and indexes parsed,
without recursing — which were unfinished after forty-five minutes
interpreted, and `tests/compile_memory.rs`, whose programs' size is its
assertion and one of which was unfinished after twenty-five minutes
interpreted; the ordinary matrix runs them in full. The crate forbids `unsafe`;
loom has nothing to model, since nothing here is shared between threads.
