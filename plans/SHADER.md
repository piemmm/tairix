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
| SH3 | The builder: a Rust API that emits validated modules, plus `cargo xtask shaders` and the pinned first-party set | planned | GP4 |
| SH4 | The compile sandbox: the service process, its request/answer protocol, its capability floor, and containment on crash | planned | GP8 |
| SH5 | `lib/wgsl`: the WGSL front end — lexer, parser, resolver, and SPIR-V emission | planned | GP8 |
| SH6 | Enforced robustness: the access model that makes an admitted module safe to run, and the injection pass that guarantees it | planned | GP8 |
| SH7 | The verification battery: differential execution, fuzz targets, the property model, and the hostile corpus | planned | GP8 |

Items are built in ledger order. SH7's harness is written *with* SH1–SH6 rather
than after them: a validator whose refusals are untested has no property worth
naming.

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
- each `Varying` its stage interface occupies.

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

## 3. SH3 — the builder and the pinned set

A Rust API that constructs modules programmatically: types, constants,
functions, control-flow constructs, and the instruction set the allow-list
admits. It emits only modules its own validator then admits, so a builder bug is
a build failure rather than a shipped defect.

`cargo xtask shaders` builds the first-party set — the compositor's chrome and
effects, and `plans/WINTERSUN.md`'s terrain, sprite, particle and light passes —
into committed artefacts. `--write` regenerates; the bare form **verifies and
fails closed on drift**, so it belongs in `ci`. This is the pattern
`cargo xtask c-header --write` and `cargo xtask font-atlas` already establish,
and it is what gives the desktop programmable shading with **no runtime compiler
surface whatsoever**.

The build is reproducible (§19.3): the same source yields byte-identical
modules, so the committed artefacts are reviewable and a change to one is
visible in the diff.

## 4. SH4 — the sandbox

The compiling path — SH5's front end and anything else that consumes untrusted
source — runs in a dedicated process holding exactly one shared-memory IPC
endpoint and nothing else: no filesystem capability, no network capability, no
capability to spawn. Source in, module bytes out.

- **Validation happens on the receiving side**, in the caller, after the
  sandbox returns. A compromised sandbox can therefore return anything it likes
  and still cannot get an invalid module admitted — the trust boundary is the
  validator, not the process.
- **A crash is contained**: the caller receives a typed error, the sandbox is
  replaced, and the event is logged with a stable event ID (§19.4, §19.5). A
  compiler crash never takes down the compositor or the calling application.
- **Work is bounded**: a compile carries a deadline and a memory ceiling, so a
  pathological input costs a refusal rather than the machine (§24.3, §26.4).

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

## 6. SH6 — enforced robustness

The property an admitted module must have: **every buffer, texture and shared
-memory access falls within the resource actually bound, whatever the shader
computes.** Logical addressing (§2) removes arbitrary addresses; this item
removes out-of-range indices into legitimate resources.

Where the device guarantees it in hardware, that is used. Where it does not, the
builder and the front end inject the clamp, and the validator **checks the
guarantee holds** rather than trusting that a pass ran. An access that can be
proven in range statically costs nothing; one that cannot is clamped. A module
for which neither can be established is refused.

This is the mechanism that makes an untrusted shader safe to execute, and it is
why it is its own item rather than a note inside the validator.

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
  oracle that replaces a vendor conformance suite (`plans/GPU.md` §0b).
- **The validator is tested by what it refuses**, not only by what it admits: a
  hostile corpus of malformed headers, truncated modules, id cycles, type
  mismatches, unstructured control flow, unlisted capabilities, physical
  addressing, out-of-range bindings, and oversize inputs — each producing its
  typed refusal and no module.
- **Fuzz targets** (§19.6) over the SPIR-V decoder, the validator, and the WGSL
  lexer/parser/resolver. These are the highest-value fuzz targets in the tree:
  they are the parsers standing between an application and the GPU. Crashing
  inputs enter the regression corpus with a unit test.
- **Property model** (`cargo xtask proptest`): a generated module that the
  builder produced is always admitted; any single-byte mutation of a valid
  module is either admitted as a still-valid module or refused with a typed
  error — never accepted as something different, never a panic.
- **Robustness is demonstrated, not asserted**: a module deliberately indexing
  past every resource kind reads or writes nothing outside it, on every backend.
- **Sandbox containment**: a compiler process cannot open a file, reach the
  network, or spawn; a crash yields a typed error and a replacement; an
  overrunning compile is refused at its deadline.
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

1. **Whether WGSL conformance fixtures count as authored non-Rust source.** The
   front end's test corpus is WGSL text. It is test *data* for a parser of
   third-party input, not TAIRiX source, which is why SH5 is not read as a §1
   violation — but it is a judgement call and is recorded rather than assumed.
   If the answer is that it is one, the fixtures are generated from Rust
   descriptions instead, at some cost in expressiveness.
2. **Whether a module cache is worth its risk.** Caching compiled modules across
   runs saves real time on a large application and introduces a persistent
   artefact an attacker may try to poison. If it lands, the cache is keyed on a
   hash of the validated module and revalidated on load — never trusted because
   it was cached.
