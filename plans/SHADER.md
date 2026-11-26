# SHADER.md — the shader IR, its validator, the builder, and the sandboxed compiler

Binding under `AGENTS.md`. This plan owns **shader programs**: the intermediate
representation TAIRiX ingests, the validator that decides whether a module may
run, the Rust builder that emits the first-party set at build time, the
front end that compiles third-party source, and the sandbox the whole compiling
path runs inside.

It exists as its own plan because it is a *language and compiler* concern with
its own threat model, and `plans/GPU.md` — which consumes it — is a *device*
concern. `plans/GPU.md` owns the seam, the memory and submission model, and the
backends; it never restates what is here, and this plan never describes a
device.

Read first (§15.18): `plans/GPU.md` (the consumer, and the security position
this plan implements), `AGENTS.md` §1 (Rust only), §2.12 (roll your own),
§5.4 (validate every input, fail closed), §19.2 (W^X — and why it does *not*
apply here), §19.5 (parser sandboxing), §19.6 (fuzzing), §24.4 (validation
bounds stay fixed), §26.4 (untrusted input is hostile).

## Ledger

| # | Item | Status | Blocks |
|---|---|---|---|
| SH0 | This plan, the jump-sheet row, the §3 map entries, the `PLAN.md` section | done | — |
| SH1 | `lib/spirv`: the module model, and the total, bounded binary decoder and encoder | done | SH2 |
| SH2 | The validator: the capability allow-list, the addressing-model refusal, structural control flow, types, and resource binding | done | GP3 |
| SH8 | The lowering view: an admitted module's resolved types and explicit layouts kept and exposed, its specialization evaluated once for every backend, and stage visibility in a pipeline layout's agreement | done | GP3.2, GP3.3 |
| SH9 | Each entry point's samplings: which image each sampler samples and in which forms, followed through calls, so a layout's sample and sampler types are agreed against them | done | GP5.5 |
| SH3 | The builder: a Rust API that emits validated modules, plus `cargo xtask shaders` and the pinned first-party set | done | GP4 |
| SH10 | Sample-rate shading: `SampleRateShading` admitted, the device feature that states it, and the software device shading each sample | done | — |
| SH5 | `lib/wgsl`: the WGSL front end — lexer, parser, resolver, and SPIR-V emission | done | — |
| SH5.1 | The text: identifier properties in `lib/unicode`, the lexer, template-list discovery, and the parser's bounded syntax tree | done | — |
| SH5.2 | The meaning: declarations and scope, types and conversions, the built-in functions, const evaluation, memory layout, attributes and interfaces, statement behaviours, uniformity, and diagnostic filters | done | — |
| SH5.3 | The module: SPIR-V written through the builder for every declaration, statement and expression, WGSL's arithmetic where SPIR-V leaves it undefined, `discard` as demotion, overrides given at compile time, pointer parameters specialised; every module written admitted | done | — |
| SH4 | The compile sandbox: the `wgsl` service in `lib/sandbox`, its request/answer protocol, the caller's admission of what it answers, its capability floor, deadline and memory ceiling, and containment on crash | done | — |
| SH6 | Enforced robustness: one access model on every backend — a buffer bounded by its range, every other memory object by each level an index steps through — and `lib/spirv`'s guard pass, checked, that Venus's lowering runs | done | — |
| SH7 | The verification battery: differential execution over every admitted opcode, fuzz targets, the property models, the hostile corpus, robustness on each backend, and sandbox containment | done | GP8 |

Items are built in ledger order. SH7's harness is written *with* SH1–SH6 rather
than after them: a validator whose refusals are untested has no property worth
naming. SH8 and SH9 follow SH2, which they extend. The front end precedes the
sandbox that hosts it, and sample-rate shading precedes the front end's
emission, which needs it for WGSL's `sample_index` and sample interpolation.

## 0. Binding decisions

1. **SPIR-V is the IR, and it is adopted rather than invented.** It is an open
   Khronos standard with a published machine-readable grammar, it is binary and
   statically typed rather than a text language with an ambiguous parse, both
   Vulkan and WebGPU target it, and virtio-gpu's Venus capset forwards it
   directly to a host driver. Inventing an IR would mean writing the
   specification as well as the implementation, and would make
   `plans/GPU.md` GP5 impossible.
2. **Validation is the trust boundary, and it is first-party.** A module is
   admitted by TAIRiX's own validator or it does not run. There is no "trusted
   source" bypass, and the pinned first-party set is validated on the same path
   as a hostile one — it is cheap, and one path is the only path anyone tests.
3. **The compiling path runs in a minimum-capability sandbox** (§19.5). Source
   in, module out, over one shared-memory endpoint: no filesystem, no network,
   no spawn, no device. This is the single largest deviation from how every
   other graphics stack is built, and it is the point.
4. **Compiled output is device code, never CPU code.** Nothing this plan
   produces is mapped executable in a CPU address space, so §19.2's W^X
   transition and `CAP_JIT_MAP_EXEC` do not apply and are not requested. A
   shader compiler is not a JIT in the sense that rule governs.
5. **The first-party set is authored in Rust and built ahead of time.** TAIRiX
   writes no shading language of its own (§1): the pinned modules are emitted by
   a Rust builder API (SH3), committed, and verified on drift. The WGSL front
   end (SH5) exists to consume *third-party* source, not to author TAIRiX.
6. **Safety is enforced, not assumed.** An admitted module cannot address memory
   outside the resources bound to it — guaranteed by the logical addressing
   model (§2) plus the robustness pass (SH6), not by trusting the author or the
   front end.
7. **Refusal is the default and is total.** An unrecognised opcode, an
   unlisted capability, a malformed header, a type mismatch, an unstructured
   control-flow graph, or a module exceeding a bound yields **no module** and a
   typed error naming what failed — never a best-effort subset, never a repair,
   never a guess (§5.4).
8. **Bounds are security bounds** (§24.4). Maximum module bytes, instruction
   count, type-graph depth, binding count, and nesting depth are fixed and do
   not move to accommodate a caller. A module that outgrows one is refused.

## 1. SH1 — `lib/spirv`, the module

`Module::decode` reads a module in either byte order and refuses it, naming the
word at fault, or answers the checked model: every instruction recognised by the
grammar and holding exactly its operands; every id nonzero, below the bound and
defined once; every enumerated operand a value its kind has, with the parameters
it takes; every string terminated, zero-padded UTF-8; every constant's and switch
case's literal as wide as the numeric type defined before it; every extended
instruction an instruction of an imported `GLSL.std.450`, with exactly its
operands. The decoder is total and bounded — `MAX_MODULE_BYTES` (16 MiB) and
`MAX_INSTRUCTIONS` (2²⁰) are fixed — and allocates in proportion to the input's
length only: ids resolve through a sorted table of what the module defines,
never one sized by its bound.

The grammar is generated by `cargo xtask spirv` from the Khronos grammar
vendored byte-identical in `lib/spirv/assets/` (SPIRV-Headers
`vulkan-sdk-1.4.363.0`, SPIR-V 1.6 revision 7), pinned by SHA-256 and checked
for drift in `ci`. It recognises the core and the Khronos multi-vendor
extensions; a provisional entry, a single vendor's, or one reachable only
through unrecognised capabilities is refused as unknown. The generator fails
rather than emit tables for a grammar shape the decoder does not read.

The model is shared by the validator (SH2), the builder (SH3), and the backends
that lower it (`plans/GPU.md`); a second parse anywhere is the duplication §2.2
forbids. The encoder frames and packs only — what it writes is decoded before
anything relies on it, so there is one judge of what a module may hold — and
`Encoder::rewriting` with `copy` re-encodes a module to exactly its bytes.

## 2. SH2 — the validator

The controls, in descending order of how much they buy:

- **The logical addressing model only.** A module declaring `Physical32` or
  `Physical64` addressing is refused outright. Logical addressing means the
  module has no raw pointers and no pointer arithmetic: every memory reference
  is to a declared variable or a declared binding, resolvable statically. This
  single refusal removes the entire class of "shader computed an address"
  attacks, and it costs nothing — no graphics workload needs physical
  addressing.
- **A capability allow-list.** SPIR-V modules declare the `OpCapability` set
  they use, and most of the exotic surface in a driver compiler sits behind one.
  TAIRiX admits a stated list and refuses everything else, so an unimplemented
  or dangerous feature is a refusal rather than an untested code path.
- **Structured control flow.** Merge and continue constructs must be
  well-formed and properly nested; an unstructured or irreducible graph is
  refused. Unstructured control flow is where a large share of real driver
  compiler bugs live, and shaders do not need it.
- **Type and id consistency.** Every id defined once before use, every type
  well-formed, every instruction's operands of the declared types, no cycles in
  the type graph.
- **Resource-binding agreement.** Every declared binding matches the pipeline's
  bind-set layout in type, count and access. A module that references a binding
  the pipeline does not provide is refused at pipeline creation — not at draw
  time, where it would be a per-frame cost and a worse failure (§2.16).
- **Entry points and execution model.** The declared entry point exists, matches
  the pipeline's stage, and its interface is complete.

**Termination is explicitly *not* claimed.** A validator cannot decide halting,
so a module may loop forever; that is contained by `plans/GPU.md` GP2's
submission deadline and device reset, which turns a hang into a lost context
rather than a wedged machine. Stating this plainly is the point — a plan that
implied the validator prevented it would be lying about its own guarantee.

The validator is `lib/spirv/src/validate/`. `Admitted::new` answers either an
`Admitted` module or a `Refusal`. An `Admitted` module carries the capabilities
it declares, with those they imply, and an `EntryPoint` for each entry point. A
`Refusal` names the word at fault and the `Rule` broken.
`docs/src/lib/spirv.md` tabulates each check against its rule.

An `EntryPoint` states what a pipeline must provide:
- its stage and workgroup size;
- each `Binding`: set, binding, `BindingKind`, count and `Access`;
- its push-constant bytes;
- each `Varying` its stage interface occupies, with where a fragment input
  is interpolated, and whether what it reads makes it shade each sample;
- the module-scope variables its call tree names, so a module of several
  entry points is lowered one entry point at a time.

`Admitted::held_bytes` states the host memory an admitted module keeps, which
`lib/gpu` charges to the context that made the module.

`EntryPoint::agree` is the resource-binding agreement, decided against a
pipeline `Layout` when the pipeline is created (`plans/GPU.md` GP3, GP8).

Structured control flow is judged over the dominator and post-dominator trees
of the structured graph, each construct a difference of dominator subtrees.
Every pointer access is charged to the memory object it is rooted in, and that
charge is folded through calls.

### 2.1 The environment

The validator admits SPIR-V's own rules plus the Vulkan environment's
standalone rules, because TAIRiX's object model is Vulkan's and the Venus
backend hands an admitted module to a host Vulkan driver unchanged. Where the
Vulkan runtime rules make a form depend on a device feature, the portable form
is the only one admitted, so a module that passes runs on every backend.

**Versions 1.3 to 1.6.** 1.3 is the Vulkan 1.1 baseline: the storage-buffer
storage class, 16-bit storage and the subgroup operations are core there, so
no back-porting extension is needed. Admitted extensions are those that only
restate that core (`SPV_KHR_storage_buffer_storage_class`,
`SPV_KHR_16bit_storage`) and `SPV_KHR_terminate_invocation`, which brings
`OpTerminateInvocation` to 1.3–1.5. Addressing is `Logical` and the memory
model `GLSL450`.

**The capability allow-list.** A declared capability outside it is refused,
and so is every instruction and enumerant no admitted capability, version or
extension enables.

| Capability | Why it is admitted |
|---|---|
| `Shader`, and the `Matrix` it implies | The graphics and compute model: vertex, fragment and compute entry points, structured control flow, descriptor-bound resources. |
| `Int64`, `Float64` | 64-bit arithmetic: the seed-pure generators' 64-bit mixing and double-precision accumulation, so a compute port agrees with its CPU reference. |
| `Int16`, `Float16` | Half-width arithmetic for bandwidth-bound passes: particles, blur. |
| `StorageBuffer16BitAccess`, `UniformAndStorageBuffer16BitAccess`, `StoragePushConstant16`, `StorageInputOutput16` | 16-bit data in buffers, push constants and the stage interface, without widening it in memory. |
| `Sampled1D`, `Image1D` | One-dimensional lookups: the theme's ramps and palettes. |
| `SampledBuffer`, `ImageBuffer` | Texel buffers: large linear tables such as terrain heights and material weights. |
| `SampledCubeArray`, `ImageCubeArray` | Cube-map arrays: point-light shadows in the light pass. |
| `SampleRateShading` | WGSL's `sample_index` built-in and `sample` interpolation: a fragment stage shading each sample (SH10). |
| `ImageQuery` | A pass sizing its sampling from the bound image: the frost, blurs. |
| `ImageGatherExtended` | Gathers at computed offsets: filtered shadows. |
| `StorageImageExtendedFormats` | One- and two-channel storage images: the frost field, height maps. |
| `StorageImageReadWithoutFormat`, `StorageImageWriteWithoutFormat` | One compute pass for every pixel format a display offers. |
| `GroupNonUniform`, `GroupNonUniformVote`, `GroupNonUniformBallot`, `GroupNonUniformArithmetic` | Subgroup reductions: exposure histograms, particle compaction. |

Kernel and OpenCL, physical addressing and variable pointers, the Vulkan memory
model, descriptor indexing, 8-bit types, geometry and tessellation, mesh and
ray tracing stay refused until a consumer states its reason. Which admitted
capabilities a device supports is the backend's to state at pipeline creation
(`plans/GPU.md` GP3); the admitted module names the ones it declares.

**Refused although SPIR-V admits them**, each for the reason given:

- `BufferBlock`, decoration groups and the `Simple` memory model — deprecated;
  each has one current form.
- An array whose length is a specialization constant — so every size the
  validator and the pipeline check is final: specialization changes values,
  never shapes.
- Each of these needs a device feature not every device has:
  - `LocalSizeId`;
  - a `Workgroup` variable's initializer;
  - a non-constant image `Offset` outside a gather;
  - the `Offsets` image operand;
  - an index into an array of descriptors that is not a constant;
  - bit-field and bit-count instructions on anything but 32-bit integers;
  - atomics on anything but 32-bit integers;
  - subgroup operations on anything but Booleans, 32-bit integers, and 32-
    or 64-bit floats.
- A constant index past an array's fixed length — such an access has no
  defined result.
- A texel offset outside −8 to 7, or one a specialization could move — every
  device's limits take that range, and no wider one.
- Two resource variables of one entry point at one set and binding — one
  binding names one resource, so agreement with a layout is decided once.
- `OpPtrEqual` and `OpPtrNotEqual` — a logical pointer has no identity to
  compare.
- `OpDecorateString` and `OpMemberDecorateString`, and the reflection
  decorations `UserSemantic` and `CounterBuffer` — they serve a front end's
  reflection, and no driver reads them.
- `SequentiallyConsistent` memory semantics, and the `SubgroupMemory` and
  `CrossWorkgroupMemory` bits. The Vulkan environment treats the first as
  `AcquireRelease` and ignores the other two, so each would promise what no
  device does.
- A structure with no members in a stage interface — every interface value
  must occupy a location, so that the location bound also bounds the
  interface analysis.
- `Sample` beside `Centroid` or `Flat` on one input — each pair names two
  places to interpolate, or none.

**Admitted though its capability is not declared:** a `ClipDistance` or
`CullDistance` member of a built-in block, as glslang declares in every
`gl_PerVertex`. Such a member is latent: reading or writing it, or the whole
block, is refused.

**Bounds** (fixed; §24.4). The specification's universal limits are the
validator's: 65,535 characters in a string, 16,383 members in a structure and
pairs in a switch, 255 indexes in an access chain, extraction or insertion,
255 function parameters and call arguments, 255 composite nesting levels, 1,023
control-flow nesting levels, 65,535 global and 524,287 local variables, 255
execution modes per entry point. TAIRiX adds 64 entry points per module (the
interface analysis walks the module once for each), a call depth of 255 (the
interpreter's frame stack), 1,024 resource bindings per entry point (past any
device's per-stage limit without descriptor indexing), and 64 interface
locations each for a stage's inputs and its outputs (no device's limits reach
further).

### 2.2 SH8 — the lowering view

What a module *means* is decided once, here, and every backend lowers from it:
a second reading of the types, the layouts or the constants in a backend would
be the duplication §2.2 forbids, and two readings can disagree.

- **Types and layouts.** `Admitted` keeps the validator's resolved type table
  and exposes it: each type's shape, each structure member's `Offset`,
  `MatrixStride` and majorness and built-in, each array's `ArrayStride`, and
  each value's type.
- **Specialization.** `Admitted::specialize` takes the values a pipeline gives
  its specialization constants and answers every constant of the module
  evaluated — `OpConstantNull` and `OpUndef` as zero, composites as their
  constituents' ids so a shared constituent is held once, and each admitted
  `OpSpecConstantOp` operation computed total over every bit pattern — with
  each entry point's final workgroup size. A value for an id no constant
  carries, two for one id, one wider than its constant (a Boolean's past one),
  and a zero workgroup component — an entry point's, or the `WorkgroupSize`
  built-in's whether or not an entry point reads it — are refused; every
  constant and constituent
  evaluation keeps counts against `MAX_SPECIALIZED`, a null composite's spelled
  out where an insertion changes one among them, and it lists nothing on the
  way, so its time and memory are bounded whatever a module of fixed size
  declares.
- **Stage visibility.** A `Layout`'s bindings and push constants carry the
  stages that see them, and `EntryPoint::agree` refuses a use from a stage that
  cannot: a binding's as `Disagreement::Stage`, push constants as
  `Disagreement::PushConstants`.
- **Folding.** A backend that hands a host compiler a module hands it the one
  meaning decided here: `Admitted::fold` answers the module with every
  specialization constant the value evaluated and its `SpecId` gone, an
  `OpUndef` its type's null, and every private, workgroup and function
  variable without an initializer given its null, admitted again. A host
  compiler therefore re-evaluates no specialization and resolves no undefined
  value its own way. A workgroup variable's initializer is admitted only as
  its null, which hosts zero-initializing workgroup memory accept.

### 2.3 SH9 — samplings

A binding's declaration says what an image holds, never how a sampler reads
it: whether its texels are filtered, compared or read as integers is the
pairing's. The validator records each pairing so a layout's sample and sampler
types (`plans/GPU.md` §1.1) are agreed against what a module does, once, at
pipeline creation.

- **A sampling** is an image's set and binding, its sampler's — a combined
  image sampler's its own — and the forms it is sampled in: compared or read,
  at an implicit level, projected, biased, offset or gathered. Every sampling
  instruction records one, `OpImageQueryLod` included.
- **Through calls.** An image, sampler or sampled image a function receives as
  a parameter is recorded against the parameter, and each call maps it to the
  caller's own roots; an argument rooted at no variable is refused as
  `OpaqueUse`, so every sampling names bindings.
- **Bounded and exposed.** `EntryPoint::samplings` answers one per pairing,
  sorted, its forms joined. A function or an entry point of more than
  `MAX_SAMPLINGS` pairings, the most bindings an entry point may reach, is
  refused as `Limit::Samplings`. Following samplings through calls grows
  with calls and entry points rather than with the module, so every sampling
  a call maps or an entry point gathers is charged to `MAX_SAMPLING_WORK`
  for the module, refused past it as `Limit::SamplingWork`, and a list being
  gathered is merged whenever it reaches twice its bound.

## 3. SH3 — the builder and the pinned set

`lib/spirv`'s `Builder` writes a module from Rust: its sections gathered as
they are written and laid out in SPIR-V's order, scalar, vector, matrix,
pointer and function types and every constant interned, aggregates fresh, and
its functions written a block at a time through structured constructs
(`if_then`, `if_else`, `while_loop`) that open, merge and close their own
blocks. `finish` decodes what it wrote and answers it only as the validator
admits it, so a builder bug is a build failure rather than a shipped defect.
Misuse — an instruction with no block open, a block left unterminated —
is a typed refusal, and no section grows infallibly. The table generator
names every `GLSL.std.450` instruction (`GlslStd450`) for it.

The pinned set lives in `lib/shaders`: each module's source beside its
committed SPIR-V under `lib/shaders/spv/`, and the bindings and push words
its consumer binds. Its first members are the desktop's passes
(`plans/GPU.md` GD6); the compositor's per-frame chrome and the furniture
painter's draws join with GP4, the game's passes with GP7. `cargo xtask shaders` builds every module of the set and fails closed in
`ci` where a committed one has drifted, is missing, or names no module;
`--write` commits what the sources build. That is the pattern
`cargo xtask c-header --write` and `cargo xtask font-atlas` established, and
it is what gives the desktop programmable shading with **no runtime compiler
surface whatsoever**.

The build is reproducible (§19.3): one source builds one sequence of bytes,
which the fuzz harness checks of every random program it has the builder
write, so the committed artefacts are reviewable and a change to one is
visible in the diff.

## 4. SH4 — the sandbox

The compiling path — SH5's front end and anything else that consumes untrusted
source — runs in a dedicated process holding exactly one shared-memory IPC
endpoint and nothing else: no filesystem capability, no network capability, no
capability to spawn. Source in, module bytes out.

- **One service on the one seam.** `lib/sandbox`'s `wgsl` module is the
  service and its caller: a program that compiles WGSL serves it from its own
  binary, started again as a `SPAWN_FLAG_SANDBOX` worker as every
  `lib/sandbox` service is, so the compiler shares no address space with the
  program that asked. A request carries the source and the override values; an
  answer carries the module's words and its diagnostics, or the error.
- **Validation happens on the receiving side**, in the caller, after the
  sandbox returns. A compromised sandbox can therefore return anything it likes
  and still cannot get an invalid module admitted — the trust boundary is the
  validator, not the process. The caller decodes the words and admits them
  (`Admitted::new`), and believes a diagnostic only within the source's lines,
  of a known code, and printable; anything else retires the worker.
- **A crash is contained**: the caller receives a typed error, the sandbox is
  replaced, and the event is logged with a stable event ID (§19.4, §19.5). A
  compiler crash never takes down the compositor or the calling application.
- **Work is bounded**: a compile carries a deadline and a memory ceiling, so a
  pathological input costs a refusal rather than the machine (§24.3, §26.4).
  The ceiling is the seam's message-sized worker; the front end's own bounds
  keep its peak below it, which a test measures at the largest source.

## 5. SH5 — `lib/wgsl`

WGSL is the source language third-party content is authored in, and it is the
right one to accept: it has a real grammar specification, it is small next to
GLSL, and it was designed from the outset for hostile input. Lexer, parser,
resolver, and SPIR-V emission, all first-party Rust (§2.12), all inside the
sandbox.

The front end is **not** trusted to produce safe output — SH2 and SH6 run on
what it emits, exactly as they would on a module an application supplied
directly. That is what makes the front end's inevitable bugs a correctness
problem rather than a security one.

**The language** is the W3C WebGPU Shading Language's core, whole, as its
Candidate Recommendation states it, with the extensions whose every construct
lands on an admitted capability:

- **Enable-extensions offered:** `f16` and `dual_source_blending` (`Index` on
  the first location's outputs). Refused as unsupported, naming the extension:
  `clip_distances` (`ClipDistance` is not admitted), `subgroups` and
  `subgroup_size_control` (their shuffle and quad operations are not),
  `primitive_index` (`PrimitiveId` needs `Geometry`) and `atomic_vec2u_min_max`
  (64-bit atomics).
- **Language extensions offered:** `readonly_and_readwrite_storage_textures`,
  `packed_4x8_integer_dot_product`, `unrestricted_pointer_parameters`,
  `pointer_composite_access`, `texture_and_sampler_let`,
  `texture_formats_tier1`, `linear_indexing`, `immediate_address_space`,
  `fragment_depth` and `swizzle_assignment`. Refused: `uniform_buffer_standard_layout`
  (a device feature; only the portable layout is admitted), `subgroup_id` and
  `subgroup_uniformity` (they need `subgroups`), `buffer_view` (untyped
  pointers) and `atomic_vec2u_min_max`.

**Mapping decisions**, each the one reading every backend sees:

- **SPIR-V 1.3**, the portable baseline, with `SPV_KHR_terminate_invocation`
  declared only by a module that discards.
- **Arithmetic SPIR-V leaves undefined is written out as WGSL defines it**:
  an integer divided by zero is its dividend and the most negative divided by
  −1 is itself, a remainder in either case is zero; a shift takes its amount
  modulo the bit width; a float converted to an integer saturates, NaN to
  zero; `extractBits` and `insertBits` clamp their offset and count;
  `countLeadingZeros`, `countTrailingZeros` and the packed 4×8 dot products are
  written from admitted instructions.
- **`discard` demotes.** A discarded invocation keeps running as a helper so
  its quad's derivatives stay defined; a module-private flag masks every
  storage write and atomic it would make, and the entry point terminates the
  invocation where it returns.
- **Override values are given at compile time**: each takes the value the
  request names or its default, and the module carries constants, not
  specialization constants, so an override may size an array, set a workgroup
  size or call a built-in in its initializer. An override with neither is a
  compile error naming it.
- **Pointer parameters past the core's** (`unrestricted_pointer_parameters`)
  are specialised: each function is written once per root variable and access
  shape its callers pass, the indexes passed as values, so every pointer a call
  carries is a whole memory object as logical addressing requires. The
  variants are counted against a fixed bound.
- **A type used both in a buffer and in workgroup memory** is written twice —
  laid out, and plain, since workgroup memory takes no explicit layout — and a
  value crossing between them is rebuilt member by member.
- **`texture_external`** is a 2D image of float texels: TAIRiX has no
  multi-planar external texture, and every built-in WGSL defines on one reads
  a single plane identically.

**Bounds** (§24.4) are the specification's minimum limits — 1,023 structure
members, 15 levels of composite nesting, 127 of brace-enclosed statements, 255
function parameters, 1,023 case selectors, 2,047 elements in an array
constructor — and the front end's own: `MAX_SOURCE_BYTES` (256 KiB),
`MAX_SYNTAX_NODES` (131,072) expressions and statements, 255 levels of
expression nesting, a fixed budget of constant-evaluation work, a fixed budget
of nodes and edges for each function's uniformity analysis, and the module it
writes at most `MAX_OUTPUT_BYTES` (1 MiB). The front end's bounds are set
by the worker, not by the language: a compile of the largest source of every
shape that costs memory — statements, functions, constant arrays, nested
control flow, the densest spellings the grammar allows, loops and selections
nested to the brace bound over every variable, chains, switches, loops and
functions whose every branch or exit could join every variable, and calls
whose every pointer takes every other's — holds, with its request, no more
than the two frames a message-sized worker allows beside its baseline, which
`lib/sandbox`'s `wgsl_memory` test measures. A source past any bound is
refused as too large, never partly compiled.

**The answer** is a module the builder admitted, with the warnings and notes
the source's diagnostic filters leave (at most `MAX_DIAGNOSTICS`), or the first
error: a closed `Code`, the line and column it begins at, and a message built
from fixed text and the source's own names. Compilation is deterministic: one
source and one set of override values write one sequence of bytes.

## 6. SH6 — enforced robustness

The property every execution of an admitted module has: **every buffer, texture
and shared-memory access falls within the resource actually bound, whatever the
shader computes.** Logical addressing (§2) removes arbitrary addresses; this
item removes out-of-range indices into legitimate resources.

There is one semantics on every backend, `plans/GPU.md` §1.2's: a read past the
bound reads zero and a write past it does nothing — a predicated store and a
select to zero, never a clamp. What *the bound* is depends on the memory:

- **A buffer a binding describes** — storage, uniform, texel — is bounded by
  the range the binding was given, scalar by scalar, whatever its indexes said:
  an explicitly laid out byte is that byte on every device.
- **Every other memory object** — push constants, private, function, workgroup,
  and the stage interface — is bounded by **each level an index steps
  through**: an index past its array's, matrix's or vector's length reads
  zero and writes nothing, even where it would have landed inside the
  variable. These objects have no laid-out bytes for "inside" to mean.
- **A dynamic component of a value** (`OpVectorExtractDynamic`,
  `OpVectorInsertDynamic`) is bounded by its vector: past it an extraction is
  zero and an insertion changes nothing.

Where the device guarantees it in hardware, that is used: Venus requires the
host's second robustness level for buffers and images, and Vulkan's
robustness covers nothing else. Where it does not, the backend's own lowering
runs `lib/spirv`'s guard pass (`Admitted::guarded`): every dynamic index into a
level it bounds is compared with what it reaches of that level, the access made
through the index clamped to the last element it reaches, a load's value
selected to zero and a store or atomic predicated on the comparison — a block
split around it, its merge taking the block's original terminator. A composite
load before 1.4, whose `OpSelect` takes no composite, is predicated too, and a
vector before 1.4 is selected by the comparison splatted. A loop header split
keeps its merge instruction at its head, its body moved to a block of its own,
so its back edge still returns to it; an `OpPhi` naming a split block names
its last segment. The pass's output is admitted again, and an independent
check confirms the guarantee rather than trusting that a pass ran: only that
check makes a `Guarded` module, and it holds every dynamic index into a bounded
level, and every dynamic vector component, to be a clamp to the last element
it reaches. An access proven in range statically — a constant index, which the
validator already holds within what it reaches — costs nothing. The software
device implements the same model in its interpreter, so a guarded module and
its source run alike there. Admission stays device-independent: the validator
requires nothing of a module for this.

Two readings make the model exact rather than approximate:

- **An index is a signed count.** An access chain reads every index as signed,
  so an index `w` bits wide reaches no more than its positive values: a level's
  reach is its length or 2^(w−1), whichever is less. The validator refuses a
  constant index past its reach, and the pass widens a 16-bit index to 32 bits,
  sign-extended, before comparing it, so the count it compares is the count the
  device reads.
- **The pass may compute with data a module only moves.** A module holding
  16-bit values in push constants or its interface needs only the storage
  capability to load them, but the select or merge the pass writes computes
  with them, so it declares the arithmetic capability the module lacks. Venus
  then holds the guarded module, not its source, to what the host states, and
  refuses its pipeline as unsupported where the host does not state that
  capability.

This is the mechanism that makes an untrusted shader safe to execute, and it is
why it is its own item rather than a note inside the validator.

## 6a. SH10 — sample-rate shading

WGSL's `sample_index` built-in and its `sample` interpolation are core
language, and each needs SPIR-V's `SampleRateShading`. It is admitted with the
`SampleId` built-in and the `Sample` decoration, and its consumer states it: a
device runs a module declaring it only where its `DeviceInfo` states sample
shading (`plans/GPU.md` §1.1). The software device states it and shades such a
pipeline's fragments once per covered sample — interpolated at the sample's
own position, its `SampleId` the sample, its coverage that sample alone, its
outputs written to that sample — so a four-sample pass resolves exactly what
its samples hold. Venus states it where the host offers the feature.

## 7. Refused by name

- **GLSL, HLSL, or any other shading language front end.** One source language
  is enough, and each additional one is a large untrusted-input parser for a
  compatibility benefit nothing currently claims (§2.3).
- **An external shader compiler** — `glslang`, Tint, `naga`, SPIRV-Tools, or
  `rust-gpu`. All are C++ or large external Rust crates in the trusted computing
  base of the most security-sensitive parser in the system (§1, §2.12).
- **Executing an unvalidated module**, in any mode, including a first-party or
  "already checked" fast path.
- **Repairing a malformed module** rather than refusing it.
- **Physical addressing**, and every capability outside the allow-list.
- **Running the compiler in the calling process** to save an IPC round trip.
- **Widening a validation bound** to admit an oversize module (§24.4).
- **Claiming a termination or total-correctness guarantee** the validator does
  not have (§2.19).

## 8. Verification

- **Differential execution is the headline test.** A module executed by the
  software backend and by a hardware backend produces the same result within a
  stated tolerance, over a corpus covering every admitted opcode. This is the
  oracle that replaces a vendor conformance suite (`plans/GPU.md` §0b). The
  corpus is written with the builder and run by `gpucheck`; a host test holds
  its coverage to `admitted_body_ops` and `admitted_glsl`, so a newly admitted
  instruction the corpus does not run fails the build. It is families of
  kernels — numbers, structure, atomics, barriers, subgroups, images, and a
  draw's fragment stage — whose inputs are words of their buffer, never
  constants, so a device computes each instruction rather than folding it.
  Each word a kernel leaves is held to the software device's bit for bit,
  within its units in the last place, or within an absolute error, as Vulkan
  states the instruction that computed it; every input is one every
  conforming device answers alike — no tie a rounding direction decides, no
  NaN or subnormal, no integer past its definition, every index in range — and
  a host test holds the chosen inputs to that. A device that does not state a
  kernel's capabilities, formats or features is not held to it; the Venus
  verticals require every family to run but those needing a subgroup
  capability, which no Vulkan host offers the seam.
- **The front end is tested by what it compiles and what it refuses**: each
  construct compiled and admitted, run on the software device against the value
  the specification gives; each shader-creation error the specification names
  refused with its code; a compiled module's bytes identical across runs.
  WGSL fixtures are test data for a parser of third-party input, held as string
  literals in Rust tests rather than as source files of a third language.
- **Tests write modules as assembly text**, through `lib/spirv`'s `test-support`
  assembler, which reads each operand as the kind the grammar gives its
  position; no build enables it.
- **The validator is tested by what it refuses**, not only by what it admits: a
  hostile corpus of malformed headers, truncated modules, id cycles, type
  mismatches, unstructured control flow, unlisted capabilities, physical
  addressing, out-of-range bindings, and oversize inputs — each producing its
  typed refusal and no module.
- **Fuzz targets** (§19.6) over the SPIR-V decoder, the validator, and the WGSL
  lexer/parser/resolver. These are the highest-value fuzz targets in the tree:
  they are the parsers standing between an application and the GPU. Crashing
  inputs enter the regression corpus with a unit test.
- **Property model**: a generated module that the builder produced is always
  admitted — `tests/fuzz_spirv.rs` has the builder write random structured
  programs and holds each to admission and to building the same bytes twice;
  any single-byte mutation of a valid module is either admitted as a
  still-valid module or refused with a typed error — never accepted as
  something different, never a panic.
- **Robustness is demonstrated, not asserted**: a module deliberately indexing
  past every resource kind reads or writes nothing outside it, on every backend
  — the software device on the host and in `gpucheck`, Venus in `gpucheck` —
  and the guard pass's output, run on the software device, agrees with its
  input there.
- **Sandbox containment**: a compiler process cannot open a file, reach the
  network, or spawn; a crash yields a typed error and a replacement; an
  overrunning compile is refused at its deadline. `sandbox_qemu_aarch64`'s
  probe worker shows the first for every sandboxed worker, the network leg
  included.
- **Reproducibility**: `cargo xtask shaders` is byte-reproducible and its verify
  mode fails on drift in `ci`.
- **`miri`**: `lib/spirv` and `lib/wgsl` are enrolled from their first item. The
  present design carries no `unsafe` — the decoder works over bounds-checked
  slices — and if that holds to completion it is stated in the completion report
  rather than left silent (§19.11).
- **`loom`**: not applicable to either crate — neither holds shared mutable
  state, and the sandbox boundary is an IPC round trip, not a lock-free
  protocol. Stated so the absence is an answer rather than silence (§19.11).

## 9. Open decisions

1. **Whether a module cache is worth its risk.** Caching compiled modules across
   runs saves real time on a large application and introduces a persistent
   artefact an attacker may try to poison. If it lands, the cache is keyed on a
   hash of the validated module and revalidated on load — never trusted because
   it was cached.
