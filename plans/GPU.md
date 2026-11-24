# GPU.md — `lib/gpu`: the device seam, its backends, and real shader support

Binding under `AGENTS.md`. This plan owns how TAIRiX reaches a graphics
processor for **render and compute** work: the device-neutral seam its
consumers write against, the memory/submission/synchronisation model beneath
it, the per-device backends, presentation, and where the desktop's own 2D work
runs — the CPU, the display engine's planes or a GPU (§7). The shader toolchain — the IR,
its validator, the build-time builder, and the untrusted-input compiler — is
large enough and different enough in character to be its own plan
(`plans/SHADER.md`); this plan consumes it and does not restate it.

Scanout is `plans/DISPLAY.md`'s. Layer composition is
`plans/FIX-DISPLAY-ACCELERATION.md`'s, and **this plan is layered on it, not
beside it**.

Read first (§15.18): `plans/SHADER.md` (the IR and the compiler), `plans/FIX-DISPLAY-ACCELERATION.md`
(the `AcceleratedDisplay`/`AccelLayer` seam and the 2D `gpu_virtio` driver
it delivered), `plans/DISPLAY.md`, `plans/FIX-DESKTOP-SPEEDUP.md` (the software
path that stays mandatory), `plans/PI.md` (VideoCore VI), `plans/WINTERSUN.md`
(the first demanding consumer), `AGENTS.md` §1 (Rust only), §4 (no ambient
authority, OOM as a value), §5.2 (capability minimalism), §16.4 (the curated
shared-library set), §17.3 (the software path is mandatory), §19.5 (parser
sandboxing), §24 and §26 (scalability and the operating-conditions floor), §27
(complete primitives).

## Ledger

| # | Item | Status |
|---|---|---|
| GP0 | This plan, `plans/SHADER.md`, the jump-sheet rows, the §3 map entries, the `PLAN.md` section | done |
| GP1 | `drivers/display/gpu_virtio`: the 2D driver — `register`, `BIND_KEYS`, resources, scanout, and the flip-completion interrupt | done: delivered by `plans/FIX-DISPLAY-ACCELERATION.md` C |
| GP2 | The memory and submission model in `lib/gpu`: contexts as the unit of isolation, typed allocations charged to a context and a heap, buffers made for stated uses, submissions checked whole, in-order queues, timelines and fences, deadlines and the engine reset — software-complete | done |
| GP3 | `lib/gpu`: the device-neutral **render and compute** vocabulary, and the software backend — including the tile-wide SPIR-V interpreter that makes it complete rather than render-only (§1) | done |
| GP3.1 | Device-stated capabilities, features and format features, the render ones left to GP3.4; formats, images, views, buffer views and samplers; the buffer-image, image-image and clear commands; word memory, the device's own worker and the process's `JobRunner` | done |
| GP3.2 | Bind set layouts, bind sets, pipeline layouts with stage visibility, and shader modules | done |
| GP3.3 | The interpreter over `plans/SHADER.md`'s lowering view, compute pipelines and dispatch | done |
| GP3.4 | `lib/raster` evolved into the one software 3D pipeline; render pipelines, passes and draws, with the render features, format features, limits and queue capability they read | done |
| GP3.4.1 | `lib/raster`'s 3D stages, evolved in place: the converter's coverage as an integer half-plane predicate per edge its row walker and a block evaluator share, homogeneous clip-space clipping, perspective-correct, linear and flat interpolation, depth for every depth format and compare operation (its reciprocal depth the reversed-Z configuration), stencil, the four-sample lattice and the general blend; WinterSun's pictures unchanged | done |
| GP3.4.2 | The interpreter's vertex and fragment stages: the stage interface and its built-ins, quads, derivatives and helper invocations, implicit-level samples and queries, and discards; the coverage test's fragment exception gone | done |
| GP3.4.3 | Render pipelines, passes and draws on the software device: their fixed-function state and creation checks, the render features, format features, limits and the `GRAPHICS` queue; a pass's draws shaded, clipped and set up in bounded batches in parallel, its bins rasterised in parallel in place, cleared and resolved | done |
| GP3.4.4 | Indirect draws, and `tests/fuzz_gpu.rs` generating images, bind sets, passes, draws and dispatches | done |
| GP4 | The pinned first-party chrome past the desktop's passes (GD6): the compositor's per-frame chrome and the furniture painter's draws as build-time SPIR-V, each with its conformance case | done |
| GP4.1 | The compositor's per-frame chrome as pinned passes: a window's body cut to its shape and laid with the dithered over, its drop shadow at either level and easing between them, and the scaled desktop's resample — the body's rule and the shadow defined once in `lib/raster` for the compositor and the passes | done |
| GP4.2 | The furniture painter's draws as pinned passes: plates and rounded rectangles, gradients, rings, polygons and strokes, glyph runs and icons, each `lib/raster`'s own primitive | done |
| GP11 | The device channel: `gpu-v1`, a driver hosting GP2's model for every client process, the context-creation capability, per-context bounds on queued work, the watchdog's one-shot timer, and the IOMMU requirement — landing with the first hardware backend | done |
| GP11.1 | The backend seam: `lib/gpu`'s model over a backend, the software device the first; overlap decided by the model, independent of any backend's texel layout | done |
| GP5 | The Venus backend: a guest Vulkan ICD over virtio-gpu's Venus capset — arbitrary shaders on real host hardware | done |
| GP5.1 | virtio shared memory: the capability decoded, its region granted to the node's driver, and mapped and unmapped there a part at a time | done |
| GP5.2 | virtio-gpu 3D in `gpu_virtio`: a control queue of many requests in flight, capsets, Venus contexts and their fence rings, blob resources, `SUBMIT_3D`; QEMU built with virglrenderer, its Venus over lavapipe | done |
| GP11.2 | The served device, with GP5.3: `gpu-v1`, the runtime hosting the model for every client, `Device::connect`, `GPU_DEVICES`, `CAP_GPU` and `gpucheck`, per-client bounds, and the watchdog | done |
| GP5.3 | The Venus backend's core, with GP11.2: the protocol and its replies, bring-up, the device it states, memory, buffers, transfers and job submission | done |
| GP11.3 | The IOMMU requirement: the kernel binds a GPU's device channel only for a node a unit translates, or on a platform with none | done |
| GP5.4 | Compute through Venus: modules, layouts, bind sets, compute pipelines, dispatches; the pinned set held to the software device, in `gpucheck` | done |
| GP5.5 | Images, views, samplers, image copies, clears and bindings through Venus; typed sampling in the seam (`plans/SHADER.md` SH9); the image work held to the software device, in `gpucheck` | done |
| GP5.6 | Render pipelines, passes and draws through Venus; the render work held to the software device, in `gpucheck` | done |
| GP6 | Presentation: the swapchain, the accelerated layer path, fullscreen promotion, and the vsync flip | planned |
| GP7 | `plans/WINTERSUN.md` WS19: the game's 3D view (WS32) — its terrain, entity, particle and light passes — as pinned SPIR-V, offloaded | planned |
| GP8 | Untrusted-shader admission: `plans/SHADER.md`'s runtime path wired into pipeline creation | planned |
| GP9 | `drivers/display/v3d`: the Raspberry Pi VideoCore VI backend | planned |
| GP10 | The application-facing GPU library and its generated C surface — what a ported game links | planned |
| GD1 | The base presents only what it shows: nothing a composite layer covers is copied into the display's frame | done |
| GD3 | Band-free blending as an engine capability: `AccelCaps` states it, every translucent window that reads no backdrop rides a free layer, the metal comparison, and the HVS's output dither | done |
| GD4 | A frosted window as two planes, its frost and its own pixels | done |
| GD8 | The frost is a function of what lies beneath it: sampled past the window's edges, retained as a field with a margin, so a drag inside it blurs nothing | done |
| GD9 | A frost layer is its window's field, cropped by the engine: a drag writes display memory only at the corners | done |
| GD2 | The composite writes where the engine reads: a flattened layer baked by the pass that composes it, and the base composed into the display's own frame | done |
| GD5 | The screen fade as a lone layer's opacity | done |
| GD10 | Black as no layer: a solid black window over the screen is the engine's own background, nothing written, baked or held | done |
| GD6 | The desktop's frost, dithered blend, copy, fill and encode as pinned compute passes (`lib/shaders`), `lib/raster`'s arithmetic to the bit | done |
| GD7 | Copy engines as `lib/gpu`'s transfer queue | planned |
| GD11 | The desktop's compositing engine on a device: GD6's passes drawing its frost and flattened layers where the session is given hardware, `lib/raster` everywhere else | planned |

Items are built in ledger order. An item is complete — tests, docs, and a green
whole-project gate — before the next begins. Several items additionally wait on
`plans/SHADER.md`, whose ledger names in its own `Blocks` column which of these
each of its items holds back; a row here blocked by one says so. The GD items
(§7) are a track of their own beside GP2–GP11, joining it only where a row says
it is blocked.

### Milestones

Each exit criterion is a measurable artefact, not a checklist. Work does not
proceed past a milestone whose criterion is unmet.

| Milestone | Items | Exit criterion |
|---|---|---|
| **MG0 — pixels through hardware** | GP1 | `devmgr` autoloads `gpu_virtio` by discovery-match on all three bare-metal QEMU targets; a scanout resource is created, transferred and flipped; flip completion arrives as an interrupt and its waiter parks. |
| **MG1 — one API, software-complete** | GP2, GP3, GP4 | Every seam method has a software implementation; every pass of the compositor's chrome is pinned SPIR-V whose conformance case, run on the software backend, equals what the compositor draws; a consumer compiled against the seam runs unchanged with no device present. |
| **MG2 — real shaders on real hardware** | GP11, GP5, GP6, GP7 | The same pinned pipelines execute on the host GPU through Venus and produce the software backend's picture within tolerance; WinterSun's frame is drawn by the GPU and the gain is measured. |
| **MG3 — untrusted shaders** | GP8 | A shader module supplied at run time by an unprivileged process is compiled in a sandbox, admitted, executed, and cannot read another context's memory; a malicious module is refused or contained, and the fuzz corpus is clean. |
| **MG4 — native silicon** | GP9 | The Pi's V3D executes the pinned set from TAIRiX-generated ISA, conformant against the software reference. |
| **MG5 — a ported game runs** | GP10 | A third-party application, built against the published surface and linking no TAIRiX source, renders and presents. |
| **MD — the display engine carries the desktop** | GD1–GD5, GD8–GD10 | A frosted window's drag blurs nothing inside its field and writes into display memory only its corners and the strip it uncovered; every translucent window that reads no backdrop moves as a layer; what the HVS states of its blend is what its metal comparison showed. |

MG2 is the milestone that proves or kills the strategy: it is the first point at
which arbitrary shader code runs on a real GPU, and it is reachable **without
writing a single instruction-set back end**, because Venus forwards SPIR-V to
the host driver. That is why it precedes MG4 rather than following it.

## 0. Binding decisions

These are settled. A change that contradicts one stops and asks (§15.7).

1. **Shaders are a first-class, supported feature, not a refused one.** The
   goal is a GPU stack good enough to carry a modern game: programmable render
   and compute pipelines, arbitrary shader modules, and the performance that
   makes them worth having. A design that can only run a fixed set of effects
   is not that, and is refused.
2. **One API and one IR; the only variable is *when* a shader was compiled.**
   `lib/gpu` is the single device-neutral seam and **SPIR-V** is the single
   shader IR every backend ingests. A pipeline whose module was built at TAIRiX
   build time and one whose module arrived from an application at run time are
   the same object taking the same path; they differ only in who compiled the
   module and therefore in how much it must be distrusted. There is no second
   API for first-party code and no privileged path.
3. **The object model is Vulkan's, deliberately, and conformance is not
   claimed.** Devices, queues, typed allocations, descriptor/bind sets,
   pipelines, render passes, command buffers, timeline semaphores and fences:
   this is where explicit GPU APIs converged, it is what maps cleanly onto every
   real driver, and it is what makes the Venus backend a serialisation rather
   than a translation. Borrowing the model is not implementing the
   specification — TAIRiX does not ship `libvulkan` conformance until GP10 says
   what it does ship, and never claims partial conformance (§2.19).
4. **A shader compiler for a hardware backend is not a CPU JIT, and is not
   treated as one.** Its output is device code fetched by the GPU's shader cores
   from GPU-visible memory; nothing is mapped executable in a CPU address space,
   so §19.2's W^X transition and `CAP_JIT_MAP_EXEC` do not apply and are not
   required. The real attack surface is the **compiler itself** and the
   **submitted command stream**, and both are structurally contained (§0a).
   - **The software backend is the one place this could stop being true**, since
     its "device" is the CPU. It therefore **interprets** SPIR-V rather than
     generating machine code: an interpreter keeps the property that no
     untrusted module ever becomes CPU-executable, on every target including
     `wasm32`. Its speed comes from the width it interprets at — whole tiles of
     invocations at once, in loops over lanes the compiler vectorises, with a
     `lib/cpuops` family added where a measurement shows one pays — not from
     code generation. Should a measurement ever show interpretation is
     insufficient, generating CPU code is a *new* decision requiring §19.2's
     full discipline and a capability gate, and is taken explicitly or not at
     all (§2.19).
5. **The software backend is mandatory, complete, and the reference.** Every
   seam method has a software implementation on every Tier-1 target including
   `wasm32`. A consumer writes one path; the absence of hardware is a
   performance property, never a capability gap to branch on (§17.3). The
   software backend is also the **conformance oracle**: a hardware backend is
   correct when it agrees with it within a stated tolerance, which is a test
   result rather than a hope.
6. **The OS's own pixels never depend on a runtime compiler.** The compositor's
   chrome, the desktop's effects and the game's passes are pinned, build-time
   SPIR-V (GD6, GP4, GP7). A broken, absent or refused shader compiler degrades
   third-party content; it can never fail to draw the desktop.
7. **No ambient access to the device.** A GPU context is created through a
   capability-gated request, holds a context-local address space, and can reach
   no memory it was not given (§2). A compromised client reaches its own
   resources and nothing else. Every submission is validated before it touches
   the ring (§5.4). The gate guards a device a driver serves to every process
   (GP11); the software device is the process's own CPU, reaches nothing
   another process holds, and is created with no capability, as decision 5
   requires.
8. **Where an IOMMU exists, the GPU is behind it.** A device with unrestricted
   DMA to physical memory defeats every isolation property above, and a
   user-space driver holding such a device is a full-memory compromise waiting
   for one bug. TAIRiX requires IOMMU/SMMU containment where the platform
   provides it, and where it genuinely does not (some SoCs), that is recorded as
   a stated, per-platform security limitation in the driver's `README.md` — not
   discovered later.
9. **Nothing here is a second display path.** Rendering produces a surface;
   presenting it goes through the one existing display path and the compositor
   (§17.3, §2.2). A game does not seize the framebuffer, and neither does the
   compositor get a private channel to the device.
10. **An engine's blend is accepted where it bands.** A window whose content is
    translucent rides a free layer on every layer engine, whether or not its
    blend dithers: an undithered 8-bit blend steps a smooth gradient the
    software path would dither, and a drag that costs nothing is worth the
    plateaus. Where the engine can dither, its driver turns that on, as the
    Pi 4's HVS does; what it states of its blend is what the metal comparison
    showed (GD3).
11. **The frost is defined for speed, not held to its first arithmetic.** The
    desktop's frost may change where a cheaper definition looks the same: it
    may differ a little from what `lib/raster` drew before, and `lib/raster`
    changes with it rather than a GPU being held bit-identical to it (GD6).
    What stays fixed is that the software path and every backend draw the one
    definition, agreeing within a stated tolerance.

## 0a. The security position, stated honestly

Graphics stacks are a CVE farm for three reasons, and TAIRiX can structurally
remove all three. This is the strongest argument for doing this work here rather
than adopting someone else's stack, and it is the claim the plan is judged on.

| Why it goes wrong elsewhere | What TAIRiX does |
|---|---|
| The shader compiler runs in the application's (or the kernel's) address space with its full authority | It runs in a §19.5 minimum-capability sandbox: one shared-memory endpoint, no filesystem, no network, no spawn. A compiler bug yields control of a process that can do nothing. (`plans/SHADER.md`) |
| Compiler output is trusted because the compiler produced it | Output is **data**, validated on the way in — module structure, resource references, and bounds — and re-validated at submission. A backend never executes a module it did not itself validate. |
| A shader can address memory outside its context | Contexts have their own GPU address spaces; a submission naming a resource the context does not own is refused, not clamped (§5.4). |
| A hung or runaway shader wedges the display | A per-submission deadline, a reset path that recovers the device, and a contained client — the compositor survives a client's hang and reports it (§2.24, §26.5). |
| The device DMAs anywhere | Behind the IOMMU where one exists (decision 8). |

The residual risks are named rather than hidden: a bug in the **validator** (it
is the trust boundary, so it is fuzzed and property-tested as the first-class
security surface it is), a hardware erratum a driver must work around, and a
platform with no IOMMU.

## 0b. Why not simply adopt OpenGL

Because it costs more and buys less, for reasons that survived the challenge
that "Android and Java manage with a GL interface, so the C argument is weak".

**That challenge is correct and the C argument is withdrawn.** `GLES31` is a
Java class over JNI, WebGL is a JavaScript binding, `wgpu` is Rust: the language
a specification is written in says nothing about the language of a binding or an
implementation, and §1/§15.11 forbid *authoring* C, not implementing a spec that
was specified in C. That reason is deleted rather than softened.

What remains, and is sufficient:

1. **No GPU is tied to OpenGL.** None has been since roughly the GeForce 3 era.
   Hardware is tied to its command stream, its register layout, its shader ISA
   and its memory model; GL is itself a thick translation layer over those, and
   every implementation translates. Adopting GL therefore means writing the
   per-chip translation that is needed **anyway**, plus a thirty-year
   compatibility state machine on top of it. It is strictly more work, not less.
2. **The state machine is the part that aged worst**, and is exactly what the
   explicit model in decision 3 exists to avoid: implicit synchronisation,
   driver-guessed hazard tracking, and hundreds of setters whose interactions
   are the bug surface.
3. **Its value is compatibility with software TAIRiX does not yet run**, and
   when TAIRiX does want to run it (GP10, MG5), the API that software targets is
   Vulkan or a translation onto Vulkan — which is the model already chosen.
4. **Partial conformance is a misleading claim** (§2.19). "OpenGL" means the
   conformance suite, and a subset shipped under the name misleads every
   consumer.

**The cost of not adopting a standard is real and is accepted with its eyes
open**: no external conformance suite to run, no corpus of open implementations
written in TAIRiX's vocabulary, and no vendor documentation phrased in its
terms. Three things mitigate it and are why the trade is taken. The object model
*is* the industry's (decision 3), so vendor documentation and open drivers
remain readable. The IR *is* Khronos SPIR-V (`plans/SHADER.md`), so the largest
single piece of specification work is adopted rather than invented. And the
software backend is a differential oracle (decision 5) whose every
implementation-defined result — derivatives, level of detail, filter weights,
transcendental precision, coverage patterns — is a stated choice compared at a
stated tolerance (§1.3), and everything else exact, which for TAIRiX's purposes
is a stronger check than a per-test-tolerance conformance suite.

## 1. GP3 — what `lib/gpu` is

An explicit render **and compute** vocabulary at the altitude modern APIs
settled on: resources created up front, state gathered into immutable objects,
work recorded into command buffers and submitted with explicit synchronisation.
GP2 built what every kind of work shares (§2); GP3 adds what drawing and
computing need, each checked on the same submission path and each with its
software implementation. Presentation is GP6's.

**The portable core is what Vulkan 1.1 requires of every device.** Every
device provides it; anything past it is a feature a device states. The
software device states everything in this section, so a consumer that picks a
device stating what its modules and pipelines need — the software device always
does — never branches on a gap (§0 decision 5), and creation refuses what a
device has not stated.

### 1.1 The vocabulary

- **`DeviceInfo`** states, beyond GP2's heaps, types, families and limits: the
  shader capabilities of `plans/SHADER.md`'s allow-list it runs; its optional
  features — anisotropic filtering and mirror-clamp-to-edge, and with GP3.4's
  draws dual-source blending, logic operations, depth clamping, wide lines,
  large points, storage writes and atomics from a vertex or a fragment stage,
  and indirect records naming a first instance, which Vulkan 1.1 does not
  require; each format's features; its subgroup size; and the most records an
  indirect draw reads, one where the device draws no more. A statement is
  added with the creation-time check that reads it, never ahead of it. Each limit is either a fixed bound (§24.4)
  or, for the software device, a capacity derived from discovered memory.
- **`Format`** — the 8-, 16- and 32-bit-per-channel unorm, snorm, uint, sint
  and float colour formats, their sRGB and BGRA orders, the packed 10:10:10:2,
  11:11:10 and 9:9:9:5 formats, and the depth and stencil formats: `Depth16Unorm`,
  `Depth32Float`, `Stencil8`, `Depth32FloatStencil8`, and `Depth24PlusStencil8`,
  the depth-stencil format every device has, its depth at least 24 bits of the
  device's choosing. A format's features — sampled, filtered, storage, colour
  attachment, depth-stencil, multisampled, uniform and storage texel buffer,
  with storage atomics once the binding model names an atomic access, and with
  GP3.4's passes blended, resolved and vertex input —
  are the device's statement; the ones Vulkan 1.1 mandates every device
  states.
- **`Image`, `ImageView`** — an image is typed, sized and usage-declared: 1D,
  2D or 3D, mip levels, array layers, 1 or 4 samples, and its `ImageUsage`.
  Like a buffer it is placed in an allocation at an offset the device aligns
  and a size the device states (`Context::image_requirements`, asked of the
  context's device); its texel layout is the device's own, texels enter and
  leave only by copies with a stated buffer layout, and it reads zero once
  placed, whatever its memory held. A view selects mip levels, layers and an
  aspect, gives the image a view dimension (1D, 1D array, 2D, 2D array, cube,
  cube array, 3D), and may read an sRGB image as its unorm twin or the
  reverse, and nothing else; its format offers one of the uses its image was
  made for that a view serves.
- **`BufferView`** — a texel buffer: a buffer, an aligned offset, a range of
  whole texels of a format with the texel-buffer feature, at most
  `max_texel_buffer_elements`.
- **`Sampler`** — nearest or linear magnification, minification and mip
  filtering; repeat, mirrored-repeat, clamp-to-edge, clamp-to-border and (a
  feature) mirror-clamp-to-edge addressing; LOD bias and clamps; anisotropy (a
  feature) to the device's limit; an optional comparison; three border colours;
  and unnormalised coordinates under Vulkan's restrictions.
- **`BindSetLayout`, `BindSet`, `PipelineLayout`** — a layout entry is a
  binding, the `DescriptorType` it holds, a count, the `Access` it allows and
  the stages that see it. A descriptor type is the `tairix_spirv::BindingKind`
  a module declares (an image binding's dimension, component and storage
  format) and how a sampler reads it, which no module states: an image sampled
  through a sampler states its texels as float or depth, filterable or not, or
  signed or unsigned integer, and a sampler its type — filtering, nearest,
  integer, comparing, nearest comparing, or unnormalised — each consistent with
  the other in a combined binding. A layout holds at most the
  device's `max_bind_set_descriptors`, its bindings' together, since a set of
  it looks each one up under the device's lock, and is checked and built
  before that lock is taken. A bind set is made whole: every binding written,
  every range within its buffer, aligned and made for the use, every view of
  the shape and usage its entry states and of a format its sample type reads,
  every sampler of its entry's type, so a draw or dispatch naming it checks
  nothing per binding; one whose descriptors alone pass its context's limit is
  refused before any is looked up, and it keeps each descriptor's memory by
  its place among the set's own. A pipeline layout is up to
  `max_bind_sets` set layouts and a push-constant size, each with the stages
  that see it: the `tairix_spirv::Layout` an entry point is agreed against,
  stage visibility included (`plans/SHADER.md` SH8).
- **`ShaderModule`** — an `Admitted` module (`plans/SHADER.md`).
- **`ComputePipeline`, `RenderPipeline`** — an entry point of a module, its
  layout and its specialization values, and for drawing the fixed-function
  state: vertex buffers and attributes, topology (points, lines, line strips,
  triangles, triangle strips, triangle fans, strip restart), culling and front
  face, depth bias and (a feature) depth clamping, depth and stencil, sample
  count, sample mask and alpha-to-coverage, per-target blending — dual-source
  factors (a feature) included — write masks, and (a feature) a logic
  operation. Creation refuses an entry point of another stage, a layout it
  disagrees with, a sampling its layout's types do not take — a sampler whose
  type does not sample the image's texels, a comparing sampler read through or
  a reading one compared with, an unnormalised one at a derived level, offset,
  biased, projected or gathered (`Error::Sampling`, `plans/SHADER.md` SH9) —
  a vertex input no attribute provides or provides as another
  component type, a fragment input no vertex output provides, a fragment
  output with no target or of another component type, a second colour a
  blend reads that its fragment stage does not write or for a target past the
  first, blending on a format that does not blend, a capability, feature or
  format feature the device has
  not stated, a workgroup or workgroup memory past the device's after
  specialization, and a program past the lowering budget. Immutable once made.
- **Commands** — buffer-to-image, image-to-buffer and image-to-image copies and
  image clears; `dispatch` and `dispatch_indirect`; and `render_pass`, a `Pass`
  of colour attachments with optional resolves, an optional depth-stencil
  attachment, each loaded — kept or cleared — and an area. Its draws — `draw`,
  `draw_indexed` and their indirect forms — are each a self-describing call
  naming its pipeline, bind sets, push constants, vertex and index buffers,
  viewport (of either height's sign), scissor, stencil reference and blend
  constant; an indirect one names its records too — a count of them, a stride
  apart in a buffer, each Vulkan's four words for a draw of vertices and five
  for an indexed draw. A pass is recorded by a closure, so none is left open. Each draw
  counts as one command and its pass as one more, in recording order, which is
  how a refusal names it; every list a command carries has its fixed bound.
- **Queues** state `TRANSFER` and `COMPUTE`, and `GRAPHICS` with GP3.4's
  passes and draws; a command on a queue whose family does not run it is
  refused.

### 1.2 What GP2's model now means

- **Between commands, every command sees everything the commands before it
  did**, and there are no barriers to record or forget. A dispatch's workgroups
  are unordered. **Within a pass** a draw sees an earlier draw's attachment
  writes only through depth, stencil and blending, and the storage writes of a
  pass's draws are unordered within it and seen by every command after it; a
  draw whose stages see a view or buffer meeting an attachment the pass
  writes, or that reads a byte of one as vertices, indices or records, is
  refused (a depth-stencil attachment the pass only tests may be read, not
  written). Two views of one image meet where they share a plane, level and
  layer; an image meets anything else where its placement does, as no backend
  says where in it its texels lie. That
  is what lets a backend batch a pass's draws and keep its attachments on chip.
  A hardware backend derives the dependencies it needs from each command's
  resources and each entry point's static `Access`, which the validator states.
- **A submission is checked whole.** Every object a command names is live, the
  context's and made for the use; every copy region within its subresource and
  its buffer, in whole words for buffers; a pass's area within every
  attachment, its attachments of one extent and sample count and of the formats
  its pipelines' targets name, each resolve from four samples to one of the
  same format; viewports and scissors finite and within the device's bounds;
  push constants exactly the layout's size; every bound set the one its
  pipeline's layout names. A refusal names the command and changes nothing but
  the room its queues made ahead of it.
- **Indirect arguments are read once, when the command runs**, into the job,
  and computed with wrapping or checked arithmetic. A dispatch past the
  device's workgroup counts loses its context (`Loss::Fault`), as work the
  device found malformed; a draw runs in bounded batches whatever it counts,
  each record read as its pass reaches it, its vertex and instance numbers
  wrapping and an index's place not, and a record naming a first instance a
  device does not state is malformed too.
  A hardware backend (GP5, GP9) sanitises indirect records on the device before
  the command reads them.
- **A bind set holds the objects it binds** — each buffer, view and sampler,
  and through them their memory — until its last holder, handle, bind set or
  queued work, lets it go. Dropping a handle removes its name: naming it is then
  `Stale`, while a bind set still reaches it. A submission captures everything
  it reaches when it is checked.
- **A module is validated in the process that admits it.** `Admitted::new` is
  the only way to one; a module that crosses a channel (GP11) or leaves a cache
  is decoded and validated again on the receiving side, and a lowering that
  finds a validator invariant broken refuses the module rather than trusting it.
- **Robustness is one semantics on every backend**: a buffer access past the
  bound range reads zero and writes nothing; an image access past the view's
  extent, levels or layers reads zero in every channel the format holds and one
  for an alpha it lacks, and writes nothing; a vertex attribute past its buffer
  reads zero with a missing alpha one, and an index past its buffer reads zero.
  Venus requires the host's second robustness level; V3D's lowering injects it
  (`plans/SHADER.md` SH6).

### 1.3 The software backend

- **A worker of its own.** Jobs run on a thread the device owns; submitting and
  signalling queue work and wake it, so they return at once as on hardware, and
  an interactive thread never runs a frame (§28). Where the machine grants no
  thread, the calling thread runs the work instead. Each phase of a command is
  one run of the process's `JobRunner` (`lib/parallel`), of as many pieces as
  the runner is wide, each claiming batches of tiles, primitives or bins from a
  shared cursor.
- **Memory.** An allocation's words have a lock of their own, and a command
  holds the locks of every memory it names, in one order every holder shares.
  Transfers move whole words through plain views at copy speed; a dispatch or a
  draw shares what its shaders write between cores as atomic views of the same
  words (`AtomicU32::from_mut_slice`), so no `unsafe` is needed and a shader
  atomic is the word's own. CPU access waits for the command holding a memory,
  and lockers have a memory in turn, in the order they asked, so neither the
  CPU nor the work keeps the other out. An allocation is made outside the
  device's lock, under charges taken first, and let go outside it by whichever
  holder goes last.
- **Working memory is had before work starts.** A pipeline's per-participant
  working set — registers at its tile width, private and workgroup storage, and
  its blocks' lane masks — is reserved from the device when the pipeline is
  created and refused there. The device keeps one set of workspaces, one for
  each participant its runner can run at once, each part fitted to the largest
  of that part a live pipeline needs, rounded up to a class keeping its four
  leading bits, at most an eighth more; each pipeline is charged to its context
  for a workspace of its own rounded need for every participant, as if it had
  the set alone, so the set, once fitted, holds no more than its pipelines'
  contexts pay for. The live needs are counted in a fixed number of classes,
  so a pipeline comes and goes in constant time and the pool keeps nothing
  that grows with the pipelines it has held. Nothing of the set is made or zeroed under the
  device's lock: a pipeline's creation plans what the set lacks of its need
  against the set's own parts — and, while a job holds it, what is kept for
  it — and charges the heap, makes it with the lock let go — the parts that
  grow, or the set whole where there is none — and installs it, swapping each
  part in where it is longer, so pipelines planned together are each covered
  whichever installs first. A set no pipeline holds is let go first where the
  heap would refuse a growth otherwise; a growth the heap still has no room
  for is refused. What no pipeline needs shrinks in place, which takes no
  room, at the next look — a pipeline planned, refused or not, or destroyed, a
  job's start or end, a context's release, or a charge the heap would otherwise
  refuse — and what is let go is dropped with the lock let go, but where the
  heap was about to refuse, which lets it go at once. The set is lent to one
  job at a time, fitted as it is: what a pipeline
  made meanwhile is kept until the set comes back, and the last pipeline whose
  need covers a part of the lent set stays charged, if dropped meanwhile,
  until then. A pipeline's table room and host memory are charged as it is
  planned, with its working memory, so nothing is made only to be refused
  after. A job of no dispatch is lent nothing, and a deadline is counted from
  once the workspaces are had. The device's model and every memory are had by
  their lockers in turn, in the order they asked, so no thread that locks
  again at once keeps another out, and a turn's end wakes the next locker's
  place of eight alone. The device's own room for its contexts — a slot apiece
  and a place on each engine's list for each queue, the list doubling as it
  grows — is charged to the heap, as is a context's budget for as long as
  anything charged to it is held. A draw's vertex, primitive and bin buffers
  are fixed-size, reserved once per device and flushed in primitive order when
  full. What a job allocates as it runs — a dispatch's lock, view and
  descriptor tables, which borrow the memories they lock and so cannot be made
  ahead without `unsafe`, and the placements of the tiles its dispatches
  share — is reserved against its context when it is submitted. A pipeline
  places each of its bindings among its set's descriptors, and their total,
  when it is made, so neither a submission nor a run searches a layout; a run
  resolves its descriptors into one table, each by index — a set holding every
  memory the dispatch locks reads its own places, another's are mapped onto
  the dispatch's once — and lays its tiles' lanes out once a piece, a batch
  writing only its workgroups' ids. A program with no barrier runs a
  workgroup's tiles in turn in one tile's memory, so its workspace holds one
  tile's however many a workgroup takes.
- **Lowering.** A pipeline's entry point is lowered once, at creation, from
  `plans/SHADER.md`'s lowering view and its specialization step, in time that
  grows with what it walks — every inlined instance's operands and tables —
  under a fixed lowering budget that is a security bound like the validator's
  and bounds each — no step scans what a hostile module can make long: calls
  inlined — each call site a construct whose merge is
  the code after the call, so lanes that return early wait there — the blocks
  no branch from the entry reaches left out, and the variables only another
  entry point reaches, every pointer's root resolved
  statically, values the same on every lane found and
  computed once per tile, and registers allocated over live ranges taken from
  definitions and uses and widened over the loops other lanes may run again
  while one holds a value: to a loop's end for a value carried into it, and to
  its start for a tile-shared value carried out of it, since a shared register
  is written whichever lanes run. Each block is laid out after every block
  that branches to it other than round a loop, so a live range is a span of
  the layout. A vector's component, a part of a constant, a shuffle's run of
  one source, and a composite of one register's own words in order are read
  where they lie, an edge's moves ordered by the register a source lies
  within; a whole copy into a fresh register of its source's size and class
  gives it the source's slot where the source's range began before the copy
  and ends there, and goes. The budget bounds instructions, edge copies and switch cases
  each to what a program holds as they are built, and blocks, instances, the
  operands every inlined instance walks, constant words and parts, structure
  member reads, explicit-layout plans' scalars and type levels, value and
  virtual words, and the steps widening live ranges over loops.
- **Execution.** A program runs a *tile* of up to 64 lanes, each instruction
  over its active span of lanes, its values planes of 32-bit words (a 64-bit
  value's two planes hold each lane's word pair together). The tile keeps a
  thread frontier — its blocks in an order where every construct precedes its
  merge and each loop lays out its loop construct, then its continue construct,
  then its merge, each block with the mask of lanes waiting there — and runs
  the earliest, so lanes reconverge at every merge and none laps an iteration.
  A workgroup larger than a tile runs as several, each suspended at a barrier
  until all arrive; small workgroups share one. A fragment tile is sixteen 2×2
  quads, so a derivative reads its neighbours.
- **Subgroups** are the device's stated size, a power of two up to 64: a
  tile's lanes in order, chunked; a small workgroup's own lanes; a vertex
  batch's consecutive vertices. Helper invocations take part in derivatives and
  in no subgroup operation; absent lanes are inactive.
- **Every value is defined.** Arithmetic wraps; division by zero gives zero and
  `MIN / -1` gives `MIN`; a shift by the width or more gives zero; a dynamic
  component index past its vector reads zero and writes nothing; float to
  integer saturates and a `NaN` is zero; `OpUndef`, an undefined shuffle
  component, an uninitialised variable, workgroup memory and an unwritten
  output read as zero; subgroup operations fold in active lanes only, and an
  index naming an inactive or absent lane reads zero; a sampling reaches the
  interpreter only through a sampler of a type its layout states takes it
  (§1.1); `OpUnreachable` ends its lane. A participant clears the registers it runs in before each piece of a
  job, so nothing of another context is ever read. Stores, atomics and image writes are
  made for active lanes only, never for a helper. Lowering, interpretation and
  the fixed-function stages are total over every bit pattern: no input panics
  them.
- **Memory is robust** (§1.2): a pointer is a memory object — a binding's
  range, a variable — and an offset; every access is checked whole, scalar by
  scalar, against the object's extent, whatever its indexes said, and an
  invalid pointer stays invalid. `OpArrayLength` saturates at zero.
- **Bounded and contained.** A job counts the work it does — the words each
  instruction, edge copy, tile start and frontier scan touches over its lanes,
  what locking and resolving a dispatch costs, samples, primitives, bytes — on
  one meter its dispatches' participants count on from, and reads the clock at
  each fixed amount of it; a shared flag stops every participant at its next
  count once the deadline passes, and the context is lost (`Loss::Overran`). A
  participant stops within that amount and one step's charges of the deadline
  — an instruction, a tile's or a batch's start, or the empty blocks that end a
  tile's lanes — however the module's work is shaped and however many
  dispatches a job holds; each command looks before its own work, and none
  once the job is stopped.
- **Rasterisation is `lib/raster`'s** (user ruling, 2026-10-29). `lib/raster`'s
  mesh and view are evolved in place into the one software 3D pipeline: its
  coverage rule — the converter's crossings rounded to its grid and its tie
  rule — stated as an integer half-plane predicate per edge that its row walker
  and a block evaluator share, proved one by test; homogeneous clip-space
  clipping keeping its guard band and inside-end cuts; perspective-correct,
  linear and flat interpolation; a depth stage for every depth format and
  compare operation, of which its reciprocal depth is the reversed-Z
  configuration; stencil; the four-sample lattice; and the general blend.
  `lib/gpu`'s software backend drives those stages: it shades, clips and sets
  up a pass's draws in bounded batches in parallel, then rasterises its bins in
  parallel, each bin's primitives in order, in place in the attachments'
  memory — bins meet where every attachment's texels start on a word, so they
  never share one — cleared on the first flush and resolved on the last. A
  pass's loads keep or clear; it has no discard loads or store operations,
  whose contents a device may choose and which on a device drawing in place
  would save nothing: a tiling backend that gains by them adds them with their
  meaning fixed. WinterSun's renderer is a configuration of
  the same stages until WS19 routes it through `lib/gpu`.
- **The rules a picture depends on are stated.**
  - A point is the square of side `PointSize`, clamped to `1..=max_point_size`
    (one where the vertex stage writes none), centred on its vertex, discarded
    when its centre is clipped, `PointCoord` from its upper left. A line is the
    rectangle one pixel wide (or the stated width, a feature) about its
    segment, interpolated along it. Both are two triangles through the one
    coverage rule, and both face front.
  - Clipping weights a `NoPerspective` output linearly in framebuffer space,
    takes a flat output from the original primitive's provoking vertex (its
    first), computes depth bias once per original primitive, and drops a
    primitive with a non-finite position.
  - Four samples sit at the standard positions; depth is evaluated at each
    unless the fragment writes its own; a `Centroid` input is interpolated at
    the pixel centre if covered and at the first covered sample otherwise; the
    output `SampleMask` and alpha-to-coverage, by a stated pattern, narrow the
    coverage; an sRGB attachment resolves by averaging in linear light; an
    integer attachment does not resolve.
  - With `EarlyFragmentTests`, depth and stencil are tested and written in
    primitive order before shading, `FragDepth` is ignored, and nothing the
    shader does undoes them. Without it, they are tested before shading only
    where the result cannot differ — no storage or image writes or atomics, no
    `FragDepth`, and where the shader can discard or change coverage, no stencil
    fail or depth-fail operation but keep: a failing sample's outcome written
    then, a passing one tested again and written after shading. A pixel that
    fails an early test stays in its quad as a helper. The pipeline's sample
    mask applies before any test, and the `SampleMask` input and centroid are
    the coverage before them. A primitive clipped to its guard band has its
    samples held to its viewport's rectangle, so a draw touches no pixel outside
    it. Quads of several
    primitives share a tile; every per-sample test, blend and write is made in
    primitive order.
  - Derivatives are fine, taken per quad row and column. The level of detail,
    its rounding and the anisotropic footprint follow stated formulas; these,
    filter weights, transcendental precision and coverage patterns are the
    implementation-defined results a hardware backend is compared with at a
    stated tolerance (§0b).
- **Isolation is of memory, not of time.** The software device is its
  process's own CPU, and one context's work can hold another's back until its
  deadline; a process serving mutually distrusting clients gives each a device
  of its own.

### 1.4 Outside the seam

What neither Vulkan 1.1 requires nor this section names as a stated feature —
geometry and tessellation stages, mesh and ray-tracing pipelines, per-sample
shading and the `InterpolateAt` family, whose capability Vulkan grants only
with it, sparse resources, and sample counts other than 1 and 4, among them —
is outside it until a consumer states its reason, and is revisited when §11's
second open decision sets GP10's target.

## 2. GP2 and GP11 — memory, submission, isolation

This is where the security properties live. GP2 built the model in `lib/gpu`,
complete on the software device; GP11 serves it from a driver to every client
process. The design is `docs/src/lib/gpu.md`'s.

**What GP2 guarantees.**

- **A context is the unit of isolation.** Its allocations, buffers, timelines
  and queues are its own, filed in generational tables (`lib/collections`'
  `SlotMap`) so a released object's handle is refused. A handle of another
  context or another device is refused wherever it is named. Memory is shared
  only by an export another context imports, and stays charged to its
  allocator.
- **Memory is typed and accounted.** A device enumerates its heaps and the
  memory types over them; an allocation is charged to its context's limit and
  its heap's size, refused past the limit, then past the heap, then if the
  memory cannot be had, and given back when its last holder lets it go. Every
  request asks the limit for all it would charge the context, its tables'
  room included, before any heap. The host memory a
  context makes the device hold is charged to the same limit: its own state
  and every object when it is made, an update's bytes once when recorded, a
  submission for what it keeps queued and the tables its largest dispatch
  makes as it runs, and the room each of its tables and queues — objects of
  each kind, its jobs, a queue's order, a timeline's pending signals — grows
  to, charged before it grows and kept charged as long as it is kept, so an
  over-limit request is refused with nothing changed but the room a table or
  queue made ahead of it, and one context cannot
  spend the device's memory for every other; a table the host still refuses at
  run time loses the context as `Loss::OutOfMemory`, never as a fault.
- **Submission is checked whole, then queued.** Every buffer named is the
  context's, live, and made for the use; every range is within what it names;
  no copy writes a byte twice or, within one memory, reads what it writes; every
  signal advances its timeline; the deadline is within the device's; no fixed
  bound — command buffers, commands, points, copy regions, update bytes — is
  passed. A refusal names the command and changes nothing but the room its
  queues made ahead of it; nothing is clamped
  into range (§5.4, §23.1).
- **Order is explicit.** A queue runs its work in the order submitted.
  Timelines order work across queues and with the CPU, and a value is reached
  only once every signal up to it has landed; a fence is one submission's
  completion. A wait parks on the timeline it waits for.
- **Deadlines and reset.** Every submission carries a deadline counted from its
  start. An overrun loses its own context; an engine reset loses the context of
  every job past its deadline and of every other job that had begun, and puts
  the rest back first in their turn. A lost context only releases what it
  holds; every other context goes on. Nothing retries forever (§2.1, §26.5).
- **Capacities scale, bounds stay fixed** (§24). The software heap is half the
  discovered memory and the tables grow on demand; the submission bounds are
  security bounds and do not move (§24.4).

**What GP11 adds** lands with the first hardware backend (GP5), since without
one there is no device for it to serve: the backend seam first (GP11.1), the
served device with GP5.3 (GP11.2), the IOMMU requirement last (GP11.3).

**GP11.1 — the backend seam.** The model — contexts, tables, charges, the
checks, order, timelines, deadlines and loss — is generic over a *backend*:
what memory is to the device, what each object the model files is to it, and
how a ready job runs on an engine. The software device is the first backend;
a hardware backend is its driver's own (§2.22).

- A backend states its `DeviceInfo` and its engines' depths; makes memory,
  zeroed, and reads and writes the host-visible kinds; states an image's
  requirements; makes the device's form of each object the model files that
  it needs one of, from the model's checked description — images, bind sets
  and pipelines for the software device, and views, texel buffers, samplers,
  layouts and modules as a hardware backend needs them (GP5.4, GP5.5); runs a
  ready job and answers its outcome; and resets an engine.
- **No backend's work is done under the device's lock** — a lowering, a host
  round trip. What the model checks and charges is had under it, what the
  backend makes is made with it let go and filed under it; memory and working
  memory are charged before they are made, as GP2 already makes memory, so
  neither is made only to be refused after.
- **Overlap is the model's, whatever a backend's texel layout.** Two views of
  one image meet where their aspects, levels and layers do; an image meets
  anything else where its placement — its offset and the size its requirements
  state — meets the other's bytes. Every backend therefore refuses the same
  submissions, and no refusal depends on the software device's layout.
- What only the software device has — word memory and its locks, its texel
  layout, lowering and its programs, working memory and its pool, its worker —
  is the software backend's own, behind the seam. No public surface changes.

**GP11.2 — the served device**, landing with GP5.3:

- **One handle type for every device.** Every public handle reaches its device
  through one object-safe host: the model over a backend in this process, or a
  channel to a served device. Each operation the model takes in phases — an
  allocation, a bind set, an image, a pipeline — is one host call, and two
  handles are of one device when they share a host.
- **The backend seam is public.** A driver implements `Hardware` for its own
  device (§2.22) and makes a `Device` of it: a backend sees each context made
  and destroyed, makes memory in a context, runs each ready job as a `Job` it
  answers, and is told of each context lost. What a backend makes it may wait
  on the device for; the model never holds its lock across one. The forms of
  bind sets of buffers and compute pipelines joined the seam with GP5.4,
  those of images, views, texel buffers and samplers with GP5.5, and those of
  render pipelines and passes with GP5.6; `Device::hardware` takes a
  description stating any queue, feature or use of a format.
- **`gpu-v1`** (`lib/abi/src/gpu.rs`): one request for each method of the
  seam, framed as every channel is — a magic, `GPU_VERSION_V1` and an op, a
  status word answering — its bulk bytes (a module's words, an update's bytes,
  a description, what a host-visible memory is read or written) in one region
  the client creates and grants at `Open`, of a size the channel bounds. The
  runtime copies a request's bytes out of the region before decoding them, so
  a client changing them under it is reading nothing back. A command buffer
  uploads in region-sized parts to a submission the runtime stages for its
  context, charged to the context, so a submission is bounded by the seam's
  bounds and the context's limit alone, never by the region: the smallest
  region holds any one command but a pass, and a pass longer than the region
  crosses as its first draws and then the rest. A module crosses whole, so the
  region a client grants — at most `lib/spirv`'s largest module — is what
  bounds one. Its decoders are total and fuzzed, and judge nothing the model
  judges — a flag set crosses as its bits — so a served device refuses what
  the device itself refuses, for the same reason; what the client derived
  while recording — an overlap, a push range — the runtime derives again.
- **The runtime** (`lib/gpu`'s `serve`) hosts one device for every client
  process. A client names its contexts by the ids the runtime answered it and
  its objects by their keys within them; the runtime finds the context among
  the caller's own, the caller its kernel-attested `proc_id`, before anything
  else, and the model's tables check every key, so no forged name reaches
  another client's context. A module is admitted again where it arrives
  (§1.2). A client's exit, watched from its `Open`, destroys every context it
  held. An export is a random token the runtime answers, naming the memory
  until the export is let go, so one client imports another's memory only
  through a token that client handed it.
- **The serve loop never waits on a backend.** A request whose backend work
  may wait — a context made, memory made, a module lowered, a pipeline made —
  runs on one of a bounded set of workers, as many as the processors to a fixed
  most; waits, exits, completions and the watchdog stay on the loop, so one
  client's slow request holds back no other client.
- **A wait is a deferred reply**: the runtime keeps the ticket and answers when
  the value lands, the context is lost or destroyed — its client's `Close`
  among the ways — or the timeout passes.
- **`Device::connect`** is the client: the same `Device`, its handles
  forwarding to the served device, so a consumer tells a served device from the
  software one only by what it states (§0 decision 5). An update's bytes are
  the client's own memory until its submission arrives, so a served device
  charges them then rather than when they are recorded. **`GPU_DEVICES`**, a
  System Information query, lists each served device's channel, ungated as
  `AUDIO_DEVICES` is: which GPUs a machine has is no principal's secret, and the
  channel refuses a caller without `CAP_GPU`. `sysinfod` reads it from the
  hardware tree: a node publishing `tairix,gpu` whose endpoint is the GPU
  channel block's for the node it sits under, as the kernel's bind rule
  guarantees.
- **`CAP_GPU`** (§9 decision 2): the driver binds its device channel
  restricted-sender on it, so the kernel refuses a caller without it at
  dispatch. Every session holds it, as `CAP_DISPLAY`: a context reaches only
  what it was given and is charged for what it makes, so using the GPU is no
  more an administrator's act than drawing a window. Its first holder is
  `gpucheck` (`userland/apps/gpucheck`), run from a text session as
  `blendcheck` is, which runs the same work on each served device and on the
  software device and answers where they differ.
- **Per-client bounds**: at most `MAX_CLIENT_CONTEXTS` contexts a client, a
  fixed containment bound; each context's limit at most half the device's
  memory, the runtime's ceiling on what a client asks, so no context alone
  holds the device's memory from every other; everything a context queues
  charged to that limit (§2); and a client's requests in flight bounded by the
  kernel's per-sender lane (§24.3, §26.2).
- **Checking charged to its client.** A module's admission and whatever a
  backend makes of it before a pipeline is filed are charged to the requesting
  context, so no client spends the driver's memory for every other (§26.2).
- **The watchdog** is the serve loop's own deadline: its wait is bounded by the
  earliest running job's, tickless (§17.1). A device that meters its own work
  ends an overrun itself through GP2's reset; one that cannot stop a job once
  started — every hardware device here — has it answered overran at its
  deadline, its context lost and its backend told, and the job's own answer,
  when the device gives it, finds nothing to complete.

**GP11.3 — the IOMMU requirement** (decision 8): a device that no IOMMU
confines, on a platform with one, serves no context; where a platform has
none, the driver's `README.md` states the limitation. The kernel enforces it,
not the driver, whose bugs are the threat.

- **The rule is the GPU channel's bind.** `call_create` binds an id of
  `GPU_CHANNEL_ENDPOINTS` only for the driver loaded for that node, and only
  where the DMA path the kernel recorded for the node when it admitted the
  driver (`LoadedDriver::dma`) is translated, or is untranslated on a
  platform whose firmware declares no unit. Every other device channel keeps
  the node rule alone: their clients send data their driver places, where a
  GPU's clients run programs of their own on the device.
- **A platform declares a unit** where the hardware tree names an `Iommu`
  node, whatever became of it, or a firmware table describing units was
  malformed. A stranded node — behind a unit that translates nothing — is
  refused with an untranslated one, and `iommu.malformed=unconfined`, which
  publishes a malformed table's functions untranslated so the machine can
  boot from PCI storage, serves no GPU.
- **A refusal is audited** (`DEVICE_CHANNEL_UNCONFINED`, `4270`: the
  endpoint, the node and its path), leaves no endpoint and no grant, and
  answers the driver `PermissionDenied`. The driver records the refusal as
  the kernel gave it and goes on driving the display; it never learns how
  its node is translated (`plans/IOMMU.md` decision 5).

## 3. GP1 / GP5 / GP9 — the backends

**GP1 is the 2D driver `plans/FIX-DISPLAY-ACCELERATION.md` C delivered.**
`drivers/display/gpu_virtio` is a user-space driver on `lib/virtio` +
`lib/drvrt` + `lib/dma-barrier`, bound by discovery-match (§18.3), holding only
the register window, DMA constraint and interrupt its matched node requested.
The 3D backends below extend it; it serves the display path whether or not
anything above it follows.

**GP5 is the strategic one: Venus.** virtio-gpu's Venus capset carries
serialised Vulkan commands with SPIR-V modules to the host, which replays them
on the host's real driver. The guest side is therefore a Vulkan ICD — large but
**mechanical**, with no shader compiler and no instruction-set back end. That is
what makes "arbitrary shaders on real hardware" reachable on QEMU without years
of per-chip work, and it is why decision 3 chose Vulkan's object model: the
backend is a serialisation of the seam, not a translation of it.

GP5's pieces, in the order they land:

- **GP5.1 — virtio shared memory.** virtio's shared-memory capability
  (`cfg_type` 8: an id, a BAR, a 64-bit offset and length) is decoded where the
  others are (`lib/pci`'s `virtio_shared_memory`) and each region granted to
  the node's driver as a `HwResourceKind::MemoryRegion`, whose parts
  `mmio_map` maps with the caching the device states for each
  (`RegionCaching`: write-back, write-combining, or uncached as device
  memory), since a cached alias of memory the host keeps uncached loses
  writes. A region is granted only where a cacheable mapping is safe: on page
  boundaries, wholly within a
  prefetchable memory BAR holding nothing else the function names — no
  configuration structure, MSI-X table or pending bits — and with an id no
  other region claims; one refused is recorded and the node published
  without it. The driver maps and unmaps parts as it needs them (`mmio_map`,
  `mmio_unmap`; the driver host's `MemoryMapper`), so a region larger than its
  device window is used a part at a time, and `lib/virtio`'s `SharedMemory`
  frees a part's pages for another only once nothing maps them. virtio-gpu's
  host-visible region (id 1) is where a host-visible blob lies.
- **GP5.2 — virtio-gpu 3D.** The control queue is a `TicketQueue`
  (`lib/virtio`): many requests at once, each in a command slot of its own,
  each answered in its own time and attributed by its chain, so a late answer
  is its own request's. The display path keeps a batch's descriptors and
  slots, which no 3D work can take, and waits for its own answers alone, so
  no fence holds a display request back; a display request left unanswered
  holds back later display requests only. Bring-up negotiates 3D commands,
  blob resources and contexts initialised for a capset, reads at most sixteen
  capsets, and takes Venus work where the Venus capset (at most 1 KiB) is
  listed and the node holds the host-visible region — else the device draws
  in 2D alone, which is a first-class outcome and recorded with its reason.
  Every 3D command — a context made or destroyed, a host blob made or
  destroyed, mapped or unmapped, a `SUBMIT_3D` — is published and returns;
  its answer is a `Completion` carrying its sender's tag, in the order sent
  among those answered together. An object is named only once its making is
  answered; a refusal leaves nothing behind; an answer without its fence
  leaves the object unusable and its id never reused. Control commands are
  fenced on the device's own timeline and a submission on its context's ring
  (`0`–`63`, ring `0` the CPU timeline). The host never answers a ring fence
  on a context that is gone, so a context's destruction is fenced on the
  device's timeline and refused while a submission to it is out. A blob is
  host memory placed at a page offset of the host-visible region, and its
  map's answer states the caching its part is mapped with. QEMU's device is
  `virtio-gpu-gl-pci` with Venus, blobs and the 64 MiB host-visible region
  under `egl-headless`, rendering through lavapipe, so a vertical needs no GPU
  and draws the same on every host. `tools/ci/install-qemu.sh` builds QEMU
  with OpenGL against virglrenderer pinned by release commit, built with its
  render server, which every Venus context runs in and distro packages do not
  all ship. The Venus vertical binds a fixture driver in the display driver's
  place that shares the driver's `Run` bring-up (`program`).
- **GP5.3 — the Venus backend's core**, with GP11.2: the protocol's encoder and its
  reply stream, at the versions the capset states; bring-up — an instance, the
  host's physical device, and for each context a device and its queues in a
  virtio-gpu context of its own, so the host keeps contexts apart as GP2 does
  and a lost context is let go by destroying its virtio-gpu context once its
  submissions are answered, which the engine requires (GP5.2); the
  `DeviceInfo` it states, the host's statement narrowed to what the seam names
  and held to its fixed bounds; memory, device-local and host-visible, the
  latter a blob in the host-visible region; buffers; copies, fills and
  updates; and each job one command buffer, each command after a barrier
  making everything before it visible to it, submitted with the fence that
  answers its completion.
  - **The wire** is what the renderer's decoders read: four-byte granules, a
    handle a 64-bit id the guest chooses, unique and non-zero within its
    context, a pointer or array a 64-bit marker or count, and a structure its
    type, then its extensions, then its members. Every command the encoder
    writes is held byte for byte to that grammar; any other stream is fatal to
    its context on the host.
  - **Each context** is a virtio-gpu context with its own instance — the
    renderer allows one — its device, and one queue for each queue the device
    states, each bound to a ring of its own. Beside them it holds two blobs:
    shared memory holding the reply stream the renderer writes each reply
    into and the stream a job's commands travel through, executed in place
    and in parts, so a job is not bounded by what one request carries; and
    the feedback memory, host-visible and coherent, into which each job's
    last command writes its number, in a slot the job holds until that number
    is read — jobs on different queues end in any order — and a device-local
    zero-fill in a slot of its own, one fill at a time. A fence the render
    server retired proves nothing — it retires the fences of a worker that
    died — so a job is done only once its number is read back. Every object a
    context makes is made in an exchange of its own and named only once
    answered: a failed creation followed by a command naming its object is
    fatal to the context on the host. A lost context sends nothing more and
    answers the work and the exchange it holds at once, refused, since the
    model counts a job against its engine until it is answered and an
    exchange's caller waits untimed. Control orders and waited exchanges wait for the device's
    answer untimed, as Linux's virtio-gpu driver does: a device that stops
    answering its control queue has failed whole, and only jobs, whose length
    a client chooses, carry the watchdog.
  - **The region.** A part goes back to be handed out again only once the
    engine knows the device maps nothing into it, and memory a blob may still
    name is never freed: a map or unmap refused, or answered without saying
    what it did, leaves the part and the memory kept rather than risk one
    context's memory showing through another's.
  - **Memory** is one allocation the host's buffer requirements size, with
    one buffer over all of it, made for every use the seam names; a seam
    buffer is a range of it. A host-visible memory type is stated only where
    it is coherent, since the renderer maps nothing and flushes nothing. Memory
    is answered only once it is zeroed — host-visible memory through the
    driver's mapping, device-local memory by a fill — so no context reads what
    another left.
  - **What it states** is the host's statement narrowed: one queue family
    running transfers and dispatches — one running graphics work too where
    the host has one, since only such a family copies and clears depth and
    stencil, and without it no depth or stencil format is stated; drawing where
    the host draws with the software device's meaning (GP5.6); and of each
    format the uses its images, texel buffers, samplers, vertex buffers,
    attachments and resolves make where the host serves them (GP5.5, GP5.6),
    so the model refuses what the backend cannot run before it reaches the
    backend.
  - **The driver's threads.** The display path keeps the device: its loop
    alone publishes to the control queue and takes its answers. The GPU
    service and its workers reach the device through a desk the display loop
    serves last in each service of the device — after every display command
    it waited for, since such a wait takes the interrupt a 3D answer raised —
    rousing it on a port only the driver's own process may send to
    (`desk_endpoint_for`), and an answer thread hands each answer to the work
    waiting on it, so no GPU request holds the display path back and no fence
    holds a display request. That thread also sends a context's waiting work,
    so nothing waiting on the desk holds a lock it takes: an exchange holds
    the shared memory only while copying.
- **GP5.4 — compute**: bind sets of buffers, compute pipelines and dispatches,
  direct and indirect. The seam carries their forms (`Hardware::set`,
  `Hardware::compute`, `JobCommand::Dispatch`, `Counts`); a description
  stating a format or a render queue is still refused.
  - **One meaning on the host.** A module is folded before it is sent
    (`Admitted::fold`, `plans/SHADER.md` SH8): every specialization constant
    the value the seam evaluated, and every value it could read before writing
    it defined — an `OpUndef` its null, every private, workgroup and function
    variable without an initializer given its null — so the host compiler
    receives no specialization to re-evaluate and no undefined value to
    resolve its own way.
  - **What a program reads past its bindings or before it wrote it** has the
    software device's one meaning on the host, so the host must offer both:
    `robustBufferAccess` with `robustBufferAccess2`, a binding's size a whole
    number of the granularity the host bounds robust accesses at (the
    device's stated size alignments); and zero-initialized workgroup memory,
    without which one program's leftover workgroup memory shows through to
    the next. A host without either serves no GPU. The validator admits a
    workgroup variable's initializer only as its null.
  - **What it states.** A shader capability where the host runs it, the
    feature it needs enabled when the device is made: the core and chained
    features of 16-bit storage, 16-bit floats, 64-bit and 16-bit integers and
    the image ones. No subgroup capability: a Vulkan host decides which
    invocations form a subgroup and, for a SPIR-V 1.6 module, how many, so the
    seam's subgroup — the device's subgroup size of a workgroup's invocations,
    in order — cannot be had on it, and what a module computes would depend on
    the machine. The SPIR-V its Vulkan takes: 1.3 at 1.1, 1.5 at 1.2, 1.6 from
    1.3; the instance asks for 1.3 and the driver requires 1.1. No arrays of
    descriptors: an array indexed per invocation needs decorations the
    validator does not admit, so a binding holds one.
  - **Forms.** A bind set is a descriptor set layout and a pool holding the
    one set, the layout let go once the set is written; a compute pipeline is
    its module, its sets' layouts and its pipeline layout, the module and the
    sets' layouts let go once it is made.
  - **An indirect dispatch's counts** are copied into the job's slot of the
    feedback buffer and held to the device's by the pinned `indirect_counts`
    pass, then dispatched from there: counts past them are zeroed and the
    slot's fault word set, so nothing runs and the job is answered
    `Loss::Fault`, losing the context, as the software device judges it.
  - **Exchanges.** One whose commands or replies outrun the shared blob's
    areas — a host's extension list, a long module — crosses a blob made for
    it. Every refusal but the device's comes before anything is sent, so what
    was made before it is let go; a release that cannot be queued loses its
    context, whose end frees everything on the host.
  - **`gpucheck`** runs, besides the transfers, every pass of the pinned set
    on its sample (`Pinned::sample`) and an indirect dispatch past the
    device's counts on each served GPU and on the software device, and holds
    the two to the bit; the Venus vertical runs it through the host's
    lavapipe.
- **GP5.5 — images, views, texel buffers and samplers**, their copies,
  clears and bindings, through the seam's forms (`Hardware::image`, `place`,
  `view`, `buffer_view` and `sampler`; `JobCommand::BufferImage`, `CopyImage`
  and `ClearImage`; `SetResource`'s image kinds).
  - **Typed sampling.** A layout states how each image is read and how each
    sampler reads (§1.1), and pipeline creation agrees a module's samplings
    with it (`plans/SHADER.md` SH9). Vulkan's samplers carry no sample type,
    and a host filtering an integer texel, or comparing through a sampler made
    to read, has no meaning the software device shares: the seam refuses both
    before any backend is asked.
  - **What it states** of each format: a shader's use of an image only where
    the host's robust image access reads past an image's edges as zeros, and
    any use of an image only where the host copies the format both ways, as
    every image is cleared once placed. A float depth format only where the
    host's unrestricted depth range keeps a depth copied in outside
    `0.0..=1.0`, as the software device keeps it. `Depth24PlusStencil8` is
    the host's 24-bit depth where it renders one, else its float depth and
    stencil, whose depth no copy reads. From Vulkan 1.3, or with its
    extension (enabled before 1.3), each format's own word: a depth sampled
    only where it is also compared, and storage reached through a binding
    naming no format only where the format offers it, a module then reaching
    it so with no device feature; an older host compares every depth it
    samples, and its device features grant that reach to every format the
    seam stores, all of which they cover. A texel buffer's two uses are stated
    together or not at all, as the one buffer it views is made for both. Anisotropic
    filtering and mirror-clamp-to-edge where the host offers them, each
    enabled. A 2D image's side is held within the host's cube's, a
    sampler's level-of-detail
    bias is the host's in whole steps up to `MAX_LOD_BIAS`, a storage binding's
    alignment is a texel buffer's offset alignment too, and a storage image is
    bounded by the largest allocation.
  - **Images.** The host's image format properties are asked first, so an
    image past what the host makes of its format, kind, use and flags is
    refused as unsupported before one is made, asked as it is to be made, its
    format list included. An image is made cube-compatible where it is a
    square of one sample with six layers or more; a host making no such image
    of its format has it made without, and refuses only its cube views. It is
    made with a mutable format — from Vulkan 1.2 with the list of it and its
    sRGB twin — where it has a twin. Its requirements are asked of its
    context's device, their memory types named as stated and their bytes
    whole runs of the host's buffer-image granularity — from Vulkan 1.3 of its
    description, in the exchange that makes it, and before then of the image
    once made, since a query naming an image whose making failed ends the
    context. **An image reads zero once placed**:
    bound, then in the context's utility turn taken from the undefined layout
    to the general one it is kept in for every use and cleared, answered only
    once the device has. Every command follows a full memory barrier, so the
    general layout needs no transition.
  - **Views** are made for the uses of their image that their format offers,
    so a storage image seen as its sRGB twin is a sampled view.
  - **Samplers.** One reads its border as floats and, where it clamps to its
    border, a second reads it as integers, both made in one exchange; a
    binding names the one its texels read as.
  - **Texel buffers** are views of their memory's one buffer, at the seam
    buffer's offset plus the view's.
  - **Copies and clears** name the host's regions in texels, a colour as the
    floats, signed or unsigned words its format reads, and a depth and
    stencil as they are.
  - **`gpucheck`** adds the image work — copies between buffers and images
    and between images; clears of colour, depth and stencil; and a dispatch
    that fetches a uniform texel buffer, writes a storage texel buffer and a
    storage image, samples an integer image and compares a depth image, every
    value exact on any conforming device — on each served GPU stating the
    uses it makes, held to the software device to the bit; the Venus vertical
    runs it through the host's lavapipe.
- **GP5.6 — render pipelines, passes and draws**, direct and indirect, through
  dynamic rendering. The seam carries their forms (`Hardware::render`,
  `JobCommand::Pass`), and `Device::hardware` takes a description stating
  any queue, feature or use of a format. The seam refuses what no host could
  run with one meaning: a vertex attribute off its alignment — its channel's
  bytes, or a packed format's texel's — in its offset, its buffer's stride or
  the binding's offset (`Error::Misaligned`), as Vulkan reads one there or
  nowhere; and a blend reading a second colour beside another target, the
  one target every host blends so (`Error::Interface`). A draw's scissor is
  its own within the pass's area, an empty one at the area's corner.
  - **What it states.** A family that draws only on a host of Vulkan 1.3 —
    where rendering without render pass objects and binding vertex buffers
    at their sizes are core, an older host's extensions for both bringing
    dependencies of their own — that gives a draw the software device's
    meaning:
    - lines as rectangles: the host's strict lines, else its line
      rasterization's rectangular lines, enabled and asked for by every
      pipeline;
    - a point whose size no stage writes one pixel wide, and an index buffer
      bound at its size (`maintenance5`);
    - every index a full word holds drawn (`fullDrawIndexUint32`);
    - each target blended as its own (`independentBlend`);
    - a depth bias clamped (`depthBiasClamp`);
    - four samples only at the standard positions (`standardSampleLocations`).

    Each feature is stated where the host offers it, and enabled there:
    dual-source blending, logic operations, depth clamping, wide lines, large
    points, stores from vertex and fragment stages, and indirect draws' first
    instances. Of each format, the uses a pass makes are stated where the
    host's optimal tiling offers them: a colour target, blended, a
    depth-stencil target, and a resolve into it. Four samples are stated
    where the host makes an image of four samples of the format at the widest
    side stated, for every use the seam's image of it may have: its
    attachment use, both transfers, sampling where offered, and its sRGB
    twin's views. The widest side is held to the host's framebuffer, an
    attachment being an image a rendering instance may cover whole. A vertex
    attribute is stated where the buffer features offer it. No sRGB format is
    stated resolved: before `maintenance10`, which the Venus protocol carries
    none of, a host may average its samples encoded or decoded as it chooses,
    and the seam averages them decoded. Without `multiDrawIndirect` an
    indirect draw reads one record. The viewport bounds are the host's
    rounded inward, the largest point and the widest line the host's in whole
    pixels.
  - **Render pipelines.** A render pipeline is its two modules, folded as a
    compute one's are, the layout's set layouts and pipeline layout, and one
    graphics pipeline naming its targets' formats. The viewport, scissor,
    stencil reference and blend constants are dynamic. The modules and set
    layouts are let go once the pipeline is made. A target is written in the
    channels its write mask names that the fragment stage writes, so a
    channel no output writes keeps what it held, as on the software device.
    Vulkan defines no value for such a channel, so the seam refuses a
    pipeline whose blend factors or alpha-to-coverage would read one
    (`Error::Interface`) rather than give it a meaning a host could differ
    on.
  - **Passes.** A pass is one rendering instance in the general layout every
    image is kept in. Each colour target is loaded or cleared as the pass
    says and stored, and averaged into its resolve view as the pass ends; the
    depth and stencil of its one view are each loaded or cleared. Each draw's
    scissor, within the pass's area, is narrowed to the pixels its viewport
    covers: the software device holds every sample to the viewport, and a
    host clips a triangle there but draws a point's square or a line's
    rectangle whole. A draw binds its vertex buffers and indices at their
    sizes (`vkCmdBindVertexBuffers2`, `vkCmdBindIndexBuffer2`), indices in
    whole ones and an empty range at its buffer's start, so a robust read
    past one reads nought, as on the software device. Between consecutive
    draws only what changes is bound again.
  - **Indirect draws.** Where the device states first instances, a draw
    reads its records where they lie. Where it does not, each draw's records
    are copied into the job's feedback slot before the pass and checked there
    by the pinned `indirect_records` pass, one check a draw, each after a
    barrier since each reads and writes the slot's fault word. A record
    naming a first instance is zeroed and the fault word set, so the job
    answers `Loss::Fault`, as the software device judges it. The 256 records a
    slot holds bound the device's indirect count. A pass whose records outrun
    the slot runs as successive rendering instances, each copying its records
    only after a barrier past the draws before that read the slot, loading
    what the last stored, only the last resolving. An instance whose draws
    read no record copies and checks nothing.
  - **What a host may compute differently**, each within Vulkan's bounds:
    coverage at sub-pixel precision; interpolation; a value converted to a
    normalised target, blended into one or resolved, which only nought and
    one convert to alike everywhere; the samples alpha-to-coverage keeps; a
    depth bias's least step; a line's width and a point's size, to the host's
    granularity; whether a point centred outside the clip volume is drawn
    where its square reaches the viewport (the host's
    `pointClippingBehavior`), and the samples near where a line leaves the
    clip volume, which Vulkan lets a host clip there or not; and which
    samples of a pixel a viewport's edge falls within a point or line covers.
    Everything else in a draw has the one meaning above.
  - **`gpucheck`** adds the render work, and every value it reads is exact
    on any conforming device: triangles halving rectangles on the pixel grid
    or on half pixels between the standard sample positions, points and a
    line centred on pixel rows, varyings flat, tags from the fragment's
    coordinate, unsigned normalised targets given nought and one alone, and
    four samples averaged into a float target. Instanced strips, depth-tested and stencilled — a
    culled mirrored one among them — then an indexed strip with a restart
    under a scissor, and an indirect strip added into the colour through an
    offset viewport, render an unsigned normalised target and an integer one
    within an area a row short of them; a pass keeping that colour draws two
    of its channels alone, a list and an indexed fan with a restart; points
    and a line draw into a float target; four samples, some masked, resolve;
    and last, in the same context, a record naming its first instance draws,
    a device stating no first instances having to lose that context. Each
    step runs on the first queue family stating its work. It runs on each
    served GPU stating a family that draws and the uses it makes, held to
    the software device to the bit; the Venus vertical runs it through the
    host's lavapipe, and its software device is held to a model of Vulkan's
    rules in `gpucheck`'s tests.

The alternatives are named and refused for this role. **virgl** carries a
Gallium/TGSI-flavoured stream and would make the guest speak a GL-shaped
dialect — the compatibility tail of §0b with none of its compatibility. **drm
native context** forwards a real driver's uAPI, which means implementing that
driver's uAPI anyway.

**Where the device advertises only 2D**, which is the common QEMU
configuration, the device still accelerates transfer, scanout and flips and
rendering stays software. That is a first-class outcome with its own tests, not
a degraded one.

**GP9 is the first real instruction-set back end.** The Pi's VideoCore VI is
documented and has an open driver to read; a SPIR-V→V3D compiler and command
builder is the genuinely large piece of work in this plan, and it is sequenced
last because Venus proves everything above it first. Its value is measured at
GP5, not assumed.

## 4. GP6 — presentation

A rendered surface is presented through the **one** existing display path: the
compositor's accelerated layer seam
(`plans/FIX-DISPLAY-ACCELERATION.md`), including the scanout-sized fullscreen
promotion `plans/WINTERSUN.md` P3 needs. The swapchain is the seam's client-side
view of that: acquire, render, present, with the flip's completion arriving as
the driver's interrupt.

The division is worth stating because the two are easy to conflate: **the layer
path places and blends finished pictures; `lib/gpu` computes the pictures.** A
game's cost is not in placing images — terrain is a depth-tested perspective
mesh under a per-pixel weighted material blend, lighting is an accumulation
pass, weather is thousands of sprites — none of which a stack of hardware
planes expresses. Using the layer
path for that would mean composing a scene from hundreds of planes no hardware
has.

## 5. GP4 / GP8 — the two compile times

The same pipeline object, reached two ways.

**Build time (GP4) — the pinned set.** The OS's chrome (GD6, GP4) and the
game's passes (GP7) are first-party SPIR-V modules **built in Rust** by `plans/SHADER.md`'s builder,
emitted by an xtask, committed to the tree, and verified on drift — the pattern
`cargo xtask c-header --write` and `cargo xtask font-atlas` already establish.
The consequences are the point: the desktop carries no runtime compiler surface
at all, the modules are reviewable artefacts with pinned conformance cases, and
a change to a shader either produces identical output or fails the gate.
Authoring them in Rust rather than a shading language is also what keeps §1
intact — TAIRiX writes no third language.

**Run time (GP8) — untrusted admission.** A module supplied by an application
is decoded and validated in `plans/SHADER.md`'s sandbox, and only a validated
module becomes a `ShaderModule`. This is the path a third-party game uses and
the one the threat model in §0a is written against.

A consumer cannot tell the two apart at the seam, which is the property that
keeps this one API rather than two.

### 5.1 GP4 — the chrome's passes

GP4 pins the chrome GD6 left to the CPU: first the compositor's per-frame
chrome (GP4.1), then the furniture painter's draws (GP4.2); the game's passes
are GP7's (User ruling, 2026-11-20).

- **Compute passes, not fixed-function draws.** Every chrome pass reads the
  pixel it lays over, because the compositor rounds its over at the frame's
  ordered dither, which no fixed-function blend can. So each is a compute pass
  over the rectangle it lays, written with `lib/shaders`' kit as GD6's are.
- **One definition, two engines.** The rule a pass draws lives in
  `lib/raster`: the compositor draws through it, and the pass's conformance
  case holds the pass to it bit for bit on the software device. The window
  manager keeps only its policy — which windows cast at which level, the ease,
  the theme's reach and radii.
- **GP4.1's passes**, each over one rectangle of the frame, its rectangle and
  dither in its push:
  - `body`: one window's client pixels within their drawable extent, the
    backdrop plate past it on the client's rows, the four furniture strips,
    the client's rounded cut and the window's opacity, described in a
    storage buffer of words (more than portable push constants hold). A
    pixel is what
    `tairix_raster::Body` lays there, through `Pixel::laid_over`: a client
    pixel the cut leaves partly uncovered gives way to the furniture, and a
    source whose alpha is nought lays nothing on every row, the cut rows' per
    column path included.
  - `shadow`: one caster's drop shadow, `tairix_raster::drop_shadow` — the
    biweight kernel's running sum and the blurred corner notches each
    `DropShadow` builds, laid out once in a table buffer for as long as the
    theme and scale stand, and the caster's bounds and silhouette, its
    opacity and how far it has risen between the two levels in the push. A
    pixel is the general evaluation; the CPU's straight-edge and solid fast
    paths are proven equal to it. A level the lift gives no share is not
    evaluated, nor any pixel the silhouette covers.
  - `resample`: the desktop drawn at one size resampled into a window of the
    output, as `Surface::resample_window_into` filters it. The plans are
    `lib/raster`'s, computed on the CPU because they take 128-bit
    arithmetic, and handed to the pass as taps; a pair of identity plans is a
    copy, as on the CPU.
- **Thirty-two bits, by stated bounds.** The passes compute in 32-bit words
  so every backend, the V3D included, runs them unchanged. A corner radius of
  at most `MAX_PASS_RADIUS` keeps the supersampled distances' squares in a
  word, a caster's bounds within `MAX_PASS_COORDINATE` of the origin keep its
  offsets in one, and the resampler's taps sum to at most 1.25 in magnitude,
  which keeps both its accumulations below 2³¹. A layout past a bound is
  refused when it is described, never wrapped.
- **GP4.2's passes** are the furniture painter's primitives, each held to its
  `lib/raster` call bit for bit; a rectangle fill, a plain or faded picture and
  an overwrite are already GD6's `fill`, `over` and `copy`:
  - `plate`: a rounded rectangle laid over what is there (`fill_round_rect`)
    or replacing it (`set_round_rect`), square at a radius of nought.
  - `wash`: a straight colour, ramped from a top colour to a bottom one down
    its rows, washed over a rectangle at a weight per pixel taken from up to
    two planes, rounding once at the frame's dither: a vertical gradient
    (`fill_vertical_gradient`), a masked wash (`wash_region`) and a washed
    shape (`wash_polygon_subpixel`). A mask is a caller's function, so it
    reaches a device as the plane its caller fills.
  - `ring`: a ring washed solid or lit by the key light (`wash_ring`), and
    laid as a plate's rim (`frame_ring`); the bevel's normal length is an
    integer square root taken a bit pair at a time in 32-bit words.
  - `coverage`: a scan-converted shape's alpha a pixel, written to a plane,
    from the converter's own pieces of each row
    (`CoverageRows::pieces`): an edge's crossing of a row boundary takes
    64-bit products, so the CPU clips the edges to the rows and the pass walks
    each pixel's cells. A stroke is its quads (`stroke_quads`), each covered
    and laid in turn.
  - `mask`: an ink laid through a coverage plane at nearest (`cover_span`):
    a glyph, a tinted mask (`blit_tinted`), and a covered shape
    (`fill_polygon_subpixel`, `fill_polygon`).
  - `picture`: a picture laid cut to a rounded rectangle (`blit_rounded`) or
    toned toward grey and weakened (`blit_desaturated`).
  - A plane is one word a pixel, its value in the low byte, so no two
    invocations write one word. A shape's window is at most `MAX_PASS_SHAPE`
    pixels a side, a piece runs at most that far across its row, and a row
    holds at most `MAX_ROW_PIECES` pieces, which keep the walk's products and
    its summed cover in a signed word.

## 6. Layering and what stays true

- `lib/gpu` depends only on `lib/*` (§17.4) and reaches a device through the
  existing display/driver IPC seam.
- It carries no board or SoC name (§2.20). Device specifics live in the driver
  leaf, which may know its hardware because that is its job; V3D's quirks belong
  in the V3D driver and nowhere above it.
- Drivers stay user-space and capability-bound (§4, §18.3).
- The software backend is never removed, on any target.
- The headless build (§17.3) is unaffected: nothing here is required to boot,
  log in, or run a text session.

## 7. GD — the desktop's 2D work

Where a desktop frame's pixel work runs: on the CPU (`lib/raster`, the
reference and the fallback on every target), on the display engine's planes
(`plans/FIX-DISPLAY-ACCELERATION.md`), or on a GPU through `lib/gpu`. The order
is binding, as `plans/FIX-DESKTOP-SPEEDUP.md`'s is: stop moving bytes nothing
shows; then let the display engine blend what it can blend without changing the
picture; then compute what remains on a GPU where one exists. Every item evolves
the layer seam or the compositor in place.

### 7.1 Where a frosted window's drag goes

A Switchboard-shaped window — 928 × 560, the frosted ground's 43 px blur,
rounded and casting — dragged 6 px a sample, sixty samples back and forth over
a wallpaper and an opaque window at 1920 × 1080, measured on the host through
the real compositor, one core, SSE2:

| per sample | composited | layered |
|---|---|---|
| time, median | 0.64 ms | 0.027 ms |
| time, mean | 0.74 ms | 0.15 ms |
| blurred, mean | 5 356 px | 5 356 px |
| blurred, the drag's first move | 135 364 px | 135 364 px |
| encoded, into the display's own frame | 543 914 px | 6 126 px |
| copied into the display's frame | 0 px | 0 px |

Laid on the engine, the window's own pixels move as a free layer over its
frost's layer (GD4), the engine blends the glass, and the base presents only
the strip the frost uncovered (GD1). The frost is laid from its field (GD8): a
sample inside the field blurs nothing, the first move blurs the field's margin
— about the border every sample blurred while the frost replicated at its
window's edges, 125 924 px — and 7 of the 60 samples blur at all. On the engine
the frost layer is the field itself, cropped (GD9): a sample inside the field
writes the frost's corners into display memory and nothing else, and the mean
is the samples that cross the margin, each rewriting the hidden frost buffer
from the field. The frame and the base are composed straight into the
display's own memory (GD2), so nothing is copied into it: sent from the
compositor's own frame instead, the composited drag copies 543 867 px a sample
and its median is 0.74 ms. The same window unblurred is a free layer alone and
a sample composites nothing.

### 7.2 What each engine can take

- **The Pi 4's HVS** places, crops, scales and blends planes as it scans out,
  writing no memory. It cannot blur. It blends wider than 8 bits a channel and
  has an output dither, `SCALER_DISPDITHER` at 0x14: per channel, five bits
  every eighth from bit 0, the mode in bits 1:0 (1 carries the rounding error
  along the line, 2 adds a ±½ LSB triangular noise restarted each frame, 3
  both) and the output's depth in bits 4:2 (0 RGB888, 1 RGB666, 2 RGB565, 3
  RGB555, 5 36-bit, 7 30-bit), as a Raspberry Pi engineer described it
  (Raspberry Pi forums, topic 324770). Undithered, its blend steps a smooth
  gradient into the plateaus `plans/FIX-DESKTOP-SPEEDUP.md` B.5 removed;
  dithered to RGB888 with mode 3 it matches the desktop's own, as `blendcheck`
  shows on a Pi 4 panel.
- **The Pi 4's V3D 4.2** is programmable, reached through GP2–GP4 and GP9. Its
  published peak — 8 QPUs × 4 lanes × 500 MHz — issues fewer integer operations
  than the four A72 cores' NEON units, 4 × 8 16-bit lanes × 1.5 GHz, so for the
  desktop's integer blend and box blur it offloads the CPU rather than outruns
  it.
- **The BCM2711's DMA4 engines** copy rectangles in 2D mode (BCM2711 ARM
  Peripherals, chapter 4) at no better bandwidth than a core: an offload, worth
  measuring only once GD1 and GD2 leave a copy worth moving.
- **Other display engines** — Intel's universal planes, AMD's DCN planes,
  NVIDIA's window channels — are layer engines of the same shape, each a
  `drivers/display` leaf stating its own `AccelCaps`, band-free blending
  included. Their 3D and compute engines are many times the CPU and are where
  GD6 pays; their copy engines are GD7's.

### 7.3 The items

**GD1 — the base presents only what it shows.** A composite layer — a
flattened one or a frost — is opaque across its whole rectangle, so the base
beneath it cannot be seen: a layered present names as the base's damage the
composed region less every composite layer's rectangle, and the display copies
only that. The back buffer still
holds the full composite, which is what the layer is baked from. A free layer
hides nothing, because its content may hold fully transparent pixels. The base
pixels left stale beneath a layer are composed again before they can show: a
layer that moves, joins the base or is given back marks where it lay.

**GD2 — the composite writes where the engine reads.** A flattened footprint is
written into the buffer its layer shows next by the pass that composes it; only
what that buffer missed while the other was shown is encoded from the back
buffer. The base is composed into the display's own back frame —
`Presenter::present_drawn` for a frame, `BaseFrame::Drawn` beneath layers — with
what that frame missed encoded from the back buffer, so the compositor's own
frame serves only a display with no frame of its own, which is remembered
rather than asked every frame. Both halves keep the back buffer as the one
source every frost and catch-up reads.
- **A frame is planned before it is composed.** The plan names every output
  pixel the frame encodes, which is what the display is handed to bring its
  frame up to date with; the composite then runs into whichever frame the
  display holds.
- **Nothing a composite layer covers is drawn into the base**, the catch-up
  included, since the base beneath it is never seen.
- **A frame no display drew is composed into the back buffer alone** and owed,
  so the next frame catches it up from there; the compositor's own frame,
  left behind, is brought level whole before it is next sent.

**GD3 — the engine states whether its blend bands.** `AccelCaps` states whether
the engine's blend of a translucent layer reaches the panel deeper than 8 bits
a channel or dithered — what the software path's per-pixel dither guarantees.
The compositor hands the engine translucency either way (§0 decision 10): a
window whose content is partially or wholly transparent but which reads no
backdrop rides a free layer, its content blended by the engine, a frosted
window's over its frost (GD4). A window-wide opacity stays flattened
everywhere: the one surface that carries one, the library popup's fade, is
frosted too. The statement is reported — the Switchboard's graphics facts,
`sysinfo`, `blendcheck` — and never claims more than the metal showed.
- **The metal comparison** is `blendcheck` (`userland/apps/blendcheck`), run
  from a text session. One screen, one grey ramp, three panes of dark glass:
  the production dithered blend, the same blend undithered (what banding looks
  like), and the engine blending the same glass as a layer of its own, with
  what the driver states printed beside it.
- **The HVS dithers its blend.** Each channel the engine starts has its dither
  set to RGB888 with mode 3 while the channel is stopped, never under a running
  one, which has been seen to lose the picture; the other channels' fields and
  HDMI1's mux in the same register are left alone. Undithered, the HVS's pane
  matched the undithered one; dithered, it matches the dithered one, so the HVS
  states band-free blending.

**GD4 — a frosted window as two planes.** A frosted window at full opacity is
its own pixels on a free layer above a frost layer: the composite of
everything beneath its rectangle with its frost applied, opaque. The frost
layer is a composite zone like a flattened footprint — its pixels are
composed to `Depth::Frost`, stopping at the window's frost — so zones never
overlap and a zone crossing a frosted window of the base takes its whole
window to the base. A move recomposes the frost where it lay and where it
lies, laid from its retained field (GD8); the window's pixels only move, and the
engine blends them over the frost, banded where the engine bands (§0
decision 10). A change of the window's own pixels re-bakes its free layer
alone, and a change beneath it its frost alone. A frosted window given one
layer, or refused its frost's buffers, is flattened — the refusal kept until
its rectangle changes — and the two planes cost a second pair of buffers the
size of its rectangle. A window's layers are had together or not at all, so its
pixels never go on a layer without their frost.

**GD8 — the frost is a function of what lies beneath it.** A frost samples the
backdrop past its window's rectangle, replicating only at the screen's edges,
and reads the layers beneath composed without frosts of their own: a frosted
window beneath contributes its pixels over what lies under it, unblurred. A
frost is then the same at a screen pixel wherever its window lies and whatever
any other frost does, so what is retained is the blur itself — a *field* over
the window's rectangle — and the shape-weighted mix is taken from it as the
frost is composed.
- **A move inside the field blurs nothing.** The frost is copied from the
  field and only its corners are mixed, over the layers composed beneath them;
  nothing else beneath the glass is composed. A window's first move gives its
  field a margin of the blur's radius about its rectangle — about the border
  one sample blurred before — and a move past that shifts the field in place
  and blurs the slab entered, so a drag pays one slab per margin crossed
  rather than its border every sample. A narrower margin would cost the same
  priming per line for less travel, and one held only ahead of the motion
  thrashes on a pointer's jitter.
- **A change beneath stales only its reach.** A change to the layers beneath
  a frost stales the field and recomposes the frost within the frost's reach of
  it, and nothing of any other window's frost. Recomputing one frost no longer
  drops the frosts above it.
- **An unretained frost is computed where it is drawn.** A window the ration
  leaves without a field is frosted where the frame recomposes it, its
  backdrop composed into the frost plane over the frost's reach.
- **At half resolution.** The blur is taken on cells two pixels square,
  anchored at the screen's top-left, blurred by half the radius and spread
  back bilinearly (`plans/FIX-DESKTOP-SPEEDUP.md` D.5), so a frost reads, and a
  change reaches into it, `2·⌊radius/2⌋ + 2` pixels. A field at the window
  radius costs two fifths of the full-resolution blur it replaced, and a
  margin slab a third to a half.
- **What it changes.** The edge of a frost now shows the backdrop past the
  window rather than its own edge replicated; under the dark theme's glass
  that differs by at most 9 levels, and only within the reach of the edge.
  Treating a frosted window beneath as unblurred differs by its blur's residue
  seen through two panes and blurred again — a few levels where any is visible.
  Against the full-resolution blur, on photographs, the window radius keeps
  98% of pixels within a level and is at most 12 off at the hardest edges; the
  chrome radius, 7, keeps two thirds within a level and is at most 26 off. A
  window translucent as a whole with no blur, which nothing in the desktop
  draws, retains nothing: a blur of radius zero is no longer the backdrop as
  composed, so it composes through the blend path.

**GD9 — a frost layer is its window's field, cropped.** On a layer engine, a
frost layer's buffers are the size of its window's field and hold it in
screen order — the blur, and at the window's corners the shape's mix over what
lies beneath them — and the engine shows the window's rectangle of it (a
layer's source crop, which `AccelLayer` already carries).
- **Baked from the field, not the frame.** Only the corners are composed in
  the back buffer; everything else is encoded from the field. A frosted
  window's own zone therefore lays its frost's corners alone, and no composite
  zone is encoded into the heap frame, which the base never presents there.
- **What a drag writes.** A move inside the field writes the corners it left
  and arrived at, and the base the strip it uncovered. A shift of the field
  past its margin moves the field's rectangle, which rewrites the buffer the
  engine is not showing, from the field. Wherever the field is blurred again,
  both buffers are written there.
- **A frost layer needs a retained field.** The frame settles the ration before
  it plans its layers and fits the field of every window it lays on a frost
  layer before it bakes one; a frosted window the ration leaves without a
  field, or whose field cannot be had, is flattened — the one layer of its
  footprint over what lies beneath it — until its rectangle changes.

**GD5 — the screen fade as a lone layer's opacity.** On an engine that states
`per_layer_opacity`, a fade is a stack of one layer shown at the reveal's opacity
over the engine's black: the base, holding every window and the pointer; a
promoted window the pointer draws nothing over; or a desktop the engine scales.
A multi-layer stack flattens into the base at the fade's first step, and every
step after it hands the engine an opacity and moves no pixel, where the software
reveal re-encodes the whole screen into display memory. The software reveal is
not dithered (`Pixel::dimmed`), so the engine's is no worse; the pointer fades
with it because a fade takes the pointer off the cursor plane into the frame.
- **The base carries an opacity.** `Base` is the base layer — its pixels, a
  `BaseFrame` copied in or drawn in place, and the opacity it is laid at — and a
  base with nothing new names the frame on screen again at the new opacity.
- **Frames hold one strength.** A layered stack is encoded at full strength and
  the software path at the reveal; the frames are encoded again, whole, only
  where the strength they hold changes, so a fade the engine stops taking is
  finished in software at its strength and nothing is dimmed twice.
- **Only an opaque base's refusal means no frames.** A drawn base the display
  refuses as unsupported without drawing it marks the display as holding no
  frame of its own only when the base was opaque; at an opacity, the refusal
  may be of the opacity, which ends the stack instead.

**GD10 — black as no layer.** A solid window holds no pixels, only a colour,
so covering the screen in it costs no memory and composes as one fill a row.
Where the topmost window is solid opaque black over the whole screen, unshaped,
undecorated, with nothing of the pointer drawn over it, the layered present
hands the engine no base and no layer: an empty stack shows the engine's own
background, opaque black, which is part of every engine's contract. It needs no
layer room, scaling or opacity, so it is taken on any engine, ahead of the fade
and the scaled desktop, whose black it equals.
- **Everything beneath is held.** Every layer is given back and the base's
  damage is held exactly as beneath a promoted window, and replayed when the
  black goes.
- **An unchanged stack presents once.** While a hidden-base stack — the black
  or a promoted window — stands unchanged at the same reveal, a frame whose only
  changes are beneath it presents nothing. A sink that has seen none of it, a
  new one or a display another presenter drew into, is owed it again
  (`owe_output`).
- **The pointer's plane stands.** On the HVS an empty stack is the pointer's
  plane alone over the channel's background, so a hardware pointer stays over
  the black; an output coming up while it stands is given it.

**GD6 — the desktop's passes on a GPU.** The frost's passes — `frost_reduce`,
`frost_blur` along each axis, `frost_spread` and `frost_mix` — and the
compositor's dithered premultiplied `over`, `copy`, `fill` and channel-order
`encode` are the first members of the pinned set, in `lib/shaders`
(`plans/SHADER.md` SH3). They are compute passes, so they need neither GP3.4's
draws nor the rest of GP4, and land ahead of both (User ruling, 2026-11-10).
Each is `lib/raster`'s own integer arithmetic, so the tolerance its conformance
cases state is zero: run on the software device, the frost equals
`halved_reference` and, mixed back, `Surface::frost_halved_from`, the over
`blend_span`, and the encode `ChannelOrder::encode_run`, pixel for pixel. The
dither's biases and the rounding point are read from `lib/raster` as each module
is built. Where a GPU's natural arithmetic draws the frost faster and looks the
same, `lib/raster` adopts that definition rather than the GPU emulating the
CPU's (§0 decision 11), and the stated tolerance moves with it.

**GD11 — the desktop on a device.** The compositor's pixel work goes through a
compositing engine of which there are two, drawing one definition: `lib/raster`,
on every target, and GD6's passes on a `lib/gpu` device, its surfaces device
memory the display scans (GP6). The session picks the engine once, from the
device it was given — exactly as it reads the display's planes once
(`plans/FIX-DISPLAY-ACCELERATION.md` B) — and never per draw, so the compositor
writes one path against the engine and both engines draw every frame whole.
GD6's conformance cases hold them to the bit. The `lib/gpu` software device is
never the compositing engine: interpreting a pass the CPU already draws natively
would only be slower, so a session given no hardware composes through
`lib/raster` (User-delegated ruling, 2026-11-10). It waits on a device to serve
it: the device channel (GP11) and a hardware backend (GP5).

## 8. Refused by name

- **OpenGL, OpenGL ES, or a GL-shaped compatibility layer**, for §0b.
- **Implementing Vulkan or WebGPU *conformantly* as a specification**, or
  claiming partial conformance under either name (§2.19). The object model is
  borrowed; the conformance claim is not made.
- **An external graphics crate, or Mesa in any form** (§1, §2.12).
- **A GPU-only code path with no software equivalent**, and any consumer branch
  of the form "if accelerated, draw it this way" (§17.3, §2.2). An engine
  chosen once by the device a session was given — the display's layer engine,
  GD11's compositing engine — is not such a branch: each engine draws the whole
  of one definition.
- **A private game or compositor back-channel to the device** (§17.3).
- **A second rasteriser or blend path.** The software backend's 3D stages —
  clipping, coverage, interpolation, depth, stencil, multisampling and
  blending — are `lib/raster`'s, evolved in place into the one software 3D
  pipeline (§1.3; §2.2).
- **Executing a shader module the validator did not admit**, in any mode,
  including a "trusted first-party" bypass. The pinned set is validated too — it
  is cheap and it keeps one path.
- **Clamping an out-of-range submission into range** instead of refusing it.
- **A hand-picked constant as a device capacity** (§24.1), or widening a
  validation bound to admit an oversize module (§24.4).

## 9. Charter amendments this work requires

Each needs sign-off before the item that depends on it; none is assumed.

1. **§16.4 — a new curated shared-library class.** If third-party applications
   reach the GPU (GP10), they must link an OS-provided GPU library
   *dynamically*, so one security update covers every consumer. §16.4's set is
   closed and lists no graphics-device class. Until GP10 the seam is statically
   linked by first-party consumers and no amendment is needed; GP10 cannot land
   without one. **Blocks GP10.**
2. **A new capability gating GPU context creation. Signed off.** Against
   §5.2's three tests: it guards a class of resources rather than one object
   (all device-memory allocation, submission, and DMA-capable mapping); no
   existing capability expresses it — `CAP_SHM` grants shared memory, not
   device submission, and `CAP_DRV_LOAD` is the driver's, not the client's; and
   it arrives with its enforcement point and a live holder rather than ahead of
   them, which is GP11's driver endpoint and its first client. The software
   device needs none (decision 7). **Lands with GP11.**
3. **§3 map entries** for `lib/gpu`'s changed description and for
   `plans/SHADER.md`'s crates. Prose within a section; no gate implication.

## 10. Verification

- **GD1**: a frosted window dragged over a layered stack presents as the base's
  damage only what no flattened layer covers, counted on a mock engine; the
  engine's screen equals the software composite of the same scene after the
  window has crossed, left, and been given back its layer.
- **GD3**: the capability crosses the wire and is refused where malformed; a
  translucent window rides a free layer whatever the engine states, and the
  engine's screen then matches the software composite within one level. The
  HVS sets a channel's dither only while the channel is stopped, and leaves
  the other fields of its register as it found them.
- **GD4**: a frosted window is an opaque frost layer beneath a free layer of
  its own pixels; its drag bakes the frost again and only moves its pixels,
  blending none of them in software; its own change reaches its pixels and not
  its frost, a change beneath reaches its frost and not its pixels; given one
  layer it is flattened; a frost crossing a frosted base window takes its whole
  window to the base; every screen matches the software composite within one
  level.
- **GD8**: every frost equals a naive reference — the unfrosted composite
  beneath, blurred at half resolution with replication at the screen's edges
  and mixed at the shape's coverage — whether its field was retained, shifted,
  staled or never kept; a drag inside the field blurs no pixel and composes
  beneath the glass only its corners; a drag past it blurs only the slab
  entered; a change beneath
  recomposes only its reach and no other frost; the Switchboard-shaped drag of
  §7.1 is measured against GD4's.
- **GD9**: a drag inside the field writes the frost layer only at the corners
  it left and arrived at and composes nothing but the corners and the
  uncovered strip; a shift rewrites the hidden buffer; a change beneath reaches
  the frost layer where the field is blurred again; an unretained frosted
  window is flattened; no composite zone is encoded into the frame; the
  engine's screen matches the software composite within one level throughout.
- **GD2**: a frame drawn into a display's own memory shows what the same frame
  sent from the compositor's does, through frosts, the pointer, a fade, a
  refusal, a smaller desktop and a removal, and a base drawn beneath layers
  what one copied in does; a display drawn into is sent no frame; a display
  with no frame of its own is asked to draw once, and sent the whole output as
  it stands; a flattened layer's composed pixels are in the buffer it shows
  next before its bake; a base with nothing new draws and encodes nothing.
- **GD5**: a fade presents no pixel after its first step on an engine with
  per-layer opacity, and each step's screen matches the software reveal within
  one level.
- **GD6**: every committed pass is admitted and needs exactly the bindings and
  push words its pass states; each, run on the software device, equals the
  software path to the bit (`lib/shaders/tests/conformance.rs`); `cargo xtask
  shaders` fails `ci` on a module drifted from its source.
- **GD10**: a solid black cover presents an empty stack, gives every layer
  back and holds what changes beneath it, presenting nothing more until it goes
  or a sink is owed it; any other colour, a short or translucent cover, a
  window above it or a pointer drawn over it is not blank; a solid window
  composes what the general blend does and fills over everything beneath it;
  the display ABI carries an empty stack and the HVS shows it as the pointer
  over black, to an output coming up too.
- **GP1**: binds by discovery-match under `devmgr` on aarch64 `virt`, riscv64
  `virt` and x86_64 q35; resource create/transfer/flush/flip; flip completion
  parks on the interrupt; the display seam drives it end to end.
- **GP2**: a handle of another context or device is refused wherever it is
  named; a submission with one bad command is refused whole, names it, and
  changes no byte, signal or fence; a range past what it names is refused, not
  clamped; memory is reached from another context only once imported; limits
  fail closed after the attempt; a value is reached only once every signal up
  to it has landed; an overrun loses its own context alone, and an engine reset
  loses the overrunning and begun contexts and puts the rest back first; the
  submission path agrees with a byte-array reference under fuzzing.
- **GP11**: a client reaches only its own contexts; a caller without the
  capability is refused at dispatch; queued work per context is bounded; the
  watchdog's timer resets an engine while every other context survives; a
  device not behind an IOMMU where the platform has one serves no context.
  - **GP11.1**: the software device's tests pass through the seam unchanged
    but where an overlap is now decided by placement and subresource, a view
    of another level or layer of an attachment's image admitted and an image
    placed over it refused; over the software backend and a test backend of
    another texel layout, what a binding reaches decides the same and an image
    is placed by each backend's own requirements; no object's making holds the
    device's lock.
  - **GP11.2**: a client names no context or object of another client however
    it forges an id; a refused request changes nothing; a waiting client wakes
    when its value lands, its context is lost or destroyed, or its timeout
    passes; a client's exit releases everything it held; every decoder is
    fuzzed and total; `Device::connect` runs the seam's tests against a
    served device and agrees with the software device on every one.
  - **GP11.3**: the kernel binds a GPU's channel for a translated node, and
    for an untranslated one where the firmware declares no unit; where it
    declares one — through a malformed table too — an untranslated or
    stranded node is refused, audited, with no endpoint and no grant left,
    while another device's channel on it binds as before. Live, the Venus
    vertical is served on `q35` with no unit and behind VT-d translating the
    GPU, and refused beside a virtio-iommu its PCI host bypasses, the
    device's 3D work still running.
- **GP3**: every seam method has a software implementation and a host test; a
  program compiled against the seam runs unchanged with no device present.
  - **GP3.1**: every format round-trips its texels; images, views and copies
    are refused past every bound; GP2's copy throughput is measured before and
    after the word memory.
  - **GP3.2**: a bind set of every kind is made whole and each of its checks
    refuses; a bind set holds its memory past every handle; a pipeline layout
    is what an entry point is agreed against, stage visibility included.
  - **GP3.3**: every admitted instruction and `GLSL.std.450` instruction a
    compute shader runs has an interpreter test, and a test fails if one is
    added to the allow-list without one; the lapping-loop CFG and an early return out of a loop
    reconverge; two contexts on one device, the second reading every undefined
    form after the first ran, read zero; one module run on one and on several
    cores gives the same bytes; an access past every resource kind, through a
    range shorter than its block and a view narrower than its image, reads zero
    and writes nothing; a module looping forever, a huge straight-line block and
    a fan-out of calls each stop within a stated bound past the deadline, as
    does a job of many small dispatches; lanes rejoining out of module order —
    a join after both its branches, a case fallen into from one declared after
    it, a tile-shared value carried out of a loop another lane runs again —
    keep every value; the host memory a context makes the device hold is
    charged to its limit and refused past it; and a harness lowers and runs the
    fuzzed admitted modules over random inputs, held total and contained.
  - **GP3.4**: the instructions only a fragment shader runs — derivatives,
    implicit-level samples and queries, and discards — gain their interpreter
    cases, and the coverage test's fragment exception goes; `lib/raster`'s row walker and block evaluator agree on every
    sample under a property test, and WinterSun's pictures are unchanged
    through the evolved stages; points, lines and fans, a near-plane clip of
    each interpolation qualifier, centroid, per-sample depth, `SampleMask`,
    alpha-to-coverage, `EarlyFragmentTests`, overlapping quads in one tile and
    every resolve each pin their pixels; a pass of many bins and flushes
    renders whole; every indirect record draws what the direct draw it names
    draws; `tests/fuzz_gpu.rs` generates images, bind sets, passes, draws and
    dispatches as it does copies, an admitted one held to a twin of several
    participants drawing each record directly.
- **GP4**: the pinned modules are regenerated reproducibly, verify-mode fails on
  drift and runs in `ci`, and every pass's conformance case pins its software
  output: the body over random layouts — decorated and plain, cut and square,
  presented short of the client, translucent and opaque, partly off the frame
  — against `Body`; the shadow at either level and easing between them, square
  and rounded, beside, beneath and across its caster, against the CPU's rows;
  the resample reducing and enlarging along each axis and at one to one,
  against `resample_window_into`; plates over and replacing, gradients and
  masked washes, rings solid, lit and framed, shapes of several self-crossing
  contours under both fill rules covered against `CoverageRows::row` and laid
  as fills, strokes and design-grid fills or washed through a mask, tinted
  masks, and pictures rounded and toned, each against its `Surface` call; and
  a layout past a bound is refused.
- **GP5**: the pinned set executes through Venus and matches the software
  reference within its stated tolerance; a 2D-only device degrades to software
  rendering with accelerated transfer and flip, and *that* path is tested, not
  assumed.
  - **GP5.1**: a shared-memory region is granted only to its node's driver and
    only where it is memory a page mapping confines, a part past it is
    refused, a part is reused only once unmapped, and what one mapping writes
    another reads; QEMU's x86_64 virtio GPU's host-visible region reaches its
    driver whole. The region holds memory only once a blob is mapped into it,
    so reading it back on QEMU is GP5.2's.
  - **GP5.2**: requests answered out of order each reach their own waiter; a
    long fence holds no display request back; contexts, blobs and their
    mappings are made and let go against QEMU's Venus device, and what one
    mapping of a host-visible blob writes another reads.
  - **GP5.3**: the encoder's output matches the protocol byte for byte for
    every command it writes; a served device's copies, fills and updates
    through Venus equal the software device's; a lost context's virtio-gpu
    context is destroyed and every other context goes on.
  - **GP5.4**: every compute command and reply held to the renderer's
    grammar; a host's statement narrowed to the capabilities it runs and
    refused short of either robustness level or zero-initialized workgroup
    memory; a dispatch run by a renderer of the test's own on the software
    device leaving what the pass leaves; an indirect dispatch past the
    device's counts losing its context alone with nothing run past them; a
    module longer than the exchange area crossing whole; a pipeline or set
    the host refuses, or whose making is refused part way, leaving nothing
    behind; an exchange waiting on the host holding back no part of a job;
    and every pass of the pinned set agreeing with the software device to the
    bit, in `gpucheck`, through QEMU's Venus.
  - **GP5.5**: every image command and reply held to the renderer's grammar;
    a host's statement narrowed to the formats it copies both ways and reads
    robustly, a float depth only with its unrestricted range, a 24-bit depth
    stood in for, a depth compared and storage reached unformatted as the
    host's version says, and the sampler features enabled where offered; on a
    renderer of the test's own that keeps texels in a layout of its own and
    refuses what Vulkan's valid usage refuses, an image made as described,
    reading zero whatever its memory held, copied, cleared and bound to
    dispatches leaving what the software device leaves; an image past what
    the host makes refused with nothing left behind; a pairing the layout's
    sampling types refuse refused at pipeline creation; and the image work
    agreeing with the software device to the bit, in `gpucheck`, through
    QEMU's Venus.
  - **GP5.6**: every render command and reply held to the renderer's
    grammar; a host's statement narrowed to one of Vulkan 1.3 drawing with
    the software device's meaning, four samples only at the standard
    positions and where the host makes the widest image of them, no sRGB
    format resolved, and the drawing features enabled where offered; on a
    renderer of the test's own that refuses what Vulkan's valid usage
    refuses — two commands reaching one buffer's bytes, either writing, with
    no barrier between them among it — every kind of draw leaving what the
    software device leaves, ranges and scissors at their edges bound as the
    host's valid usage has them, a scissor held to its viewport, each target written only in the channels its fragment stage
    writes and only within its pass, four samples resolving alike, a host's
    own lines drawn as the rectangles asked for, and records the host draws no
    first instance of checked on the device — a record naming one losing its
    context, and a pass whose checked records outrun the slot running as
    several rendering instances that leave the same; and the render work
    agreeing with the software device to the bit, in `gpucheck`, through
    QEMU's Venus.
- **GP8**: a hostile module is refused or contained; a compromised compiler
  sandbox can reach nothing; the fuzz corpus is clean (`plans/SHADER.md`).
- **Differential**: a frame through the seam matches the direct software path
  within tolerance, so the seam itself is proven not to change the picture.
- **`miri`**: `lib/gpu` and every driver here are enrolled from their first
  item. `lib/gpu` itself forbids `unsafe`; the drivers will carry it for DMA
  and MMIO.
- **`loom`**: mandatory for each lock-free producer/consumer protocol whose
  correctness is an ordering claim — the hardware submission/completion rings
  (GP11) (§19.11). The software device has none. Its stop and fault flags
  carry no data, are only ever set and are read for their verdict after the
  runner's join, and its work cursor hands each claim a range of its own. Its
  worker's running flag is a set-once publication read with acquire
  ordering. The counts of the needs pipelines hold are added to under the
  device's lock and taken from under the lease's, and the set is fitted to
  them under both as it is lent, so a stale count only keeps a free set
  larger until the next look. A budget is taken from only under the device's
  lock and given back from any thread, so a stale read can only refuse.
  Every other hand-off is made under a lock or through `lib/parallel`'s join.
- **Performance is measured, not claimed** (§2.16): a stated scene at a stated
  resolution, software versus accelerated, recorded. A quoted measurement says
  whether the scene contained the frosted chrome that
  `plans/FIX-DISPLAY-ACCELERATION.md` explains still forces a software
  composite.
- **The floor** (§26.7): the resident GPU-resource budget is bounded and derived
  from discovered memory; a small machine with a large working set degrades in
  performance, never in correctness, and never panics.

## 11. Open decisions

1. **GP9's value.** A first-party V3D back end is large, and how much of the
   desktop and the game are GPU-bound on that board is a measurement from GP5,
   not a preference. The ledger order stands: §7.2's peak figures say the V3D
   offloads the Pi 4's desktop rather than speeding it, so the board's desktop
   speed is the GD track's.
2. **What GP10 actually publishes.** A Vulkan-shaped C surface generated from
   Rust definitions (§9) is the obvious answer and is enormous; a smaller
   TAIRiX-native surface is tractable but is a porting burden on every game. The
   decision needs MG2's measurements and a stated target for what "a modern
   game" means concretely, and is deliberately left open rather than guessed.
3. **Whether compute is exposed to applications at GP10 or held to first-party
   use.** Compute is in the seam from GP3 because the renderer needs it;
   publishing it to third parties widens the attack surface and is a separate
   decision.
