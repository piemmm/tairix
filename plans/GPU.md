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
| GP3 | `lib/gpu`: the device-neutral **render and compute** vocabulary, and the software backend — including the tile-wide SPIR-V interpreter that makes it complete rather than render-only (§1) | in progress |
| GP3.1 | Device-stated capabilities, features and format features, the render ones left to GP3.4; formats, images, views, buffer views and samplers; the buffer-image, image-image and clear commands; word memory, the device's own worker and the process's `JobRunner` | done |
| GP3.2 | Bind set layouts, bind sets, pipeline layouts with stage visibility, and shader modules | done |
| GP3.3 | The interpreter over `plans/SHADER.md`'s lowering view, compute pipelines and dispatch | done |
| GP3.4 | `lib/raster` evolved into the one software 3D pipeline; render pipelines, passes and draws, with the render features, format features, limits and queue capability they read | planned |
| GP4 | The pinned first-party pipeline set: OS chrome and the game's passes as build-time SPIR-V, with the conformance suite | planned |
| GP11 | The device channel: `gpu-v1`, a driver hosting GP2's model for every client process, the context-creation capability, per-context bounds on queued work, the watchdog's one-shot timer, and the IOMMU requirement — landing with the first hardware backend | planned |
| GP5 | The Venus backend: a guest Vulkan ICD over virtio-gpu's Venus capset — arbitrary shaders on real host hardware | planned |
| GP6 | Presentation: the swapchain, the accelerated layer path, fullscreen promotion, and the vsync flip | planned |
| GP7 | `plans/WINTERSUN.md` WS19: the game's 3D view (WS32) — its terrain, entity, particle and light passes — offloaded | planned |
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
| GD6 | The desktop's frost, dithered blend, copy, fill and encode in GP4's pinned set, agreeing with `lib/raster` within a stated tolerance | blocked: GP4 |
| GD7 | Copy engines as `lib/gpu`'s transfer queue | blocked: GP3, GP11 |

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
| **MG1 — one API, software-complete** | GP2, GP3, GP4 | Every seam method has a software implementation; the compositor draws its chrome through pinned SPIR-V pipelines on the software backend; a consumer compiled against the seam runs unchanged with no device present. |
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
   SPIR-V (GP4). A broken, absent or refused shader compiler degrades
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
  draws dual-source blending, logic operations, depth clamping, wide lines and
  large points; each format's features; and its subgroup size. A statement is
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
  and a size the device states (`Context::image_requirements`); its texel
  layout is the device's own, and texels enter and leave only by copies with a
  stated buffer layout. A view selects mip levels, layers and an aspect, gives
  the image a view dimension (1D, 1D array, 2D, 2D array, cube, cube array, 3D),
  and may read an sRGB image as its unorm twin or the reverse, and nothing
  else.
- **`BufferView`** — a texel buffer: a buffer, an aligned offset, a range of
  whole texels of a format with the texel-buffer feature, at most
  `max_texel_buffer_elements`.
- **`Sampler`** — nearest or linear magnification, minification and mip
  filtering; repeat, mirrored-repeat, clamp-to-edge, clamp-to-border and (a
  feature) mirror-clamp-to-edge addressing; LOD bias and clamps; anisotropy (a
  feature) to the device's limit; an optional comparison; three border colours;
  and unnormalised coordinates under Vulkan's restrictions.
- **`BindSetLayout`, `BindSet`, `PipelineLayout`** — a layout entry is a
  binding, the `tairix_spirv::BindingKind` it holds (so an image binding states
  the dimension, sample type and storage format a module declares), a count,
  the `Access` it allows and the stages that see it; a layout holds at most the
  device's `max_bind_set_descriptors`, its bindings' together, since a set of
  it looks each one up under the device's lock, and is checked and built
  before that lock is taken. A bind set is made whole: every binding written,
  every range within its buffer, aligned and made for the use, every view of
  the shape and usage its entry states, so a draw or dispatch naming it checks
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
  disagrees with, a vertex input no attribute provides or provides as another
  component type, a fragment input no vertex output provides, a fragment
  output with no target or of another component type, blending on a format
  that does not blend, a capability, feature or format feature the device has
  not stated, a workgroup or workgroup memory past the device's after
  specialization, and a program past the lowering budget. Immutable once made.
- **Commands** — buffer-to-image, image-to-buffer and image-to-image copies and
  image clears; `dispatch` and `dispatch_indirect`; and `render_pass`, a `Pass`
  of colour attachments with optional resolves, an optional depth-stencil
  attachment, a load and store action each, and an area. Its draws — `draw`,
  `draw_indexed` and their indirect forms — are each a self-describing call
  naming its pipeline, bind sets, push constants, vertex and index buffers,
  viewport (of either height's sign), scissor, stencil reference and blend
  constant. A pass is recorded by a closure, so none is left open. Each draw
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
  draw binding a view or buffer that overlaps an attachment the pass writes is
  refused (a depth-stencil attachment the pass only tests may be sampled). That
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
  device found malformed; a draw runs in bounded batches whatever it counts.
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
  index naming an inactive or absent lane reads zero; a depth-reference sample
  through a sampler with no comparison compares as `LessOrEqual`, and a plain
  sample through one with a comparison ignores it; `OpUnreachable` ends its
  lane. A participant clears the registers it runs in before each piece of a
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
  `lib/gpu`'s software backend drives those stages: it shades, clips, sets up
  and bins a pass's draws in bounded batches in parallel, then rasterises its
  bins in parallel, each bin's primitives in order and its colour, depth and
  stencil samples in a buffer of its own, loaded and stored as the pass's
  actions say and resolved there. WinterSun's renderer is a configuration of
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
    fail or depth-fail operation but keep, writing only after shading. A pixel
    that fails an early test stays in its quad as a helper. Quads of several
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
shading, sparse resources, and sample counts other than 1 and 4, among them —
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

**What GP11 adds**, landing with the first hardware backend (GP5 or GP9),
since without one there is no device for it to serve:

- **`gpu-v1`**, the device channel: the wire form of GP2's requests, decoded and
  checked by the driver, which hosts GP2's model for every client process with
  each client's contexts out of every other's reach.
- **The context-creation capability** (§9 decision 2, signed off): the driver
  binds its channel endpoint restricted-sender on it, so the kernel refuses a
  caller without it at dispatch; it is granted to the first principal that
  renders on hardware, and lands with that holder.
- **Per-context bounds on queued work**, so a client cannot fill the driver's
  memory with submissions waiting on a value nobody signals (§24.3, §26.2).
- **Lowering charged to its client.** A pipeline's lowering holds up to its
  fixed budgets' worth of transient memory, outside the device's lock and
  charged to no context: in one process that is the caller's own memory, but a
  driver lowering for every client charges it to the requesting context, or
  bounds a client's concurrent lowerings, so no client spends the driver's
  memory for every other (§26.2).
- **The watchdog**: a one-shot timer at the earliest running deadline, tickless
  (§17.1), whose expiry resets the engine through GP2's reset.
- **Device address spaces and the IOMMU requirement** (decision 8): a context's
  resident memory mapped into its own device address space, and a device that
  is not behind an IOMMU where the platform has one serves no context; where a
  platform has none, the driver's `README.md` states the limitation.

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

**Build time (GP4) — the pinned set.** The OS's chrome and the game's passes
are first-party SPIR-V modules **built in Rust** by `plans/SHADER.md`'s builder,
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

**GD6 — the desktop's passes on a GPU.** The box blur's passes, the dithered
premultiplied *over*, copy, fill and the channel-order encode join GP4's pinned
set. They need not be bit-identical to `lib/raster`: they agree with it within a
tolerance the conformance cases state, and where a GPU's natural arithmetic
draws the frost faster and looks the same, `lib/raster` adopts that definition
rather than the GPU emulating the CPU's (§0 decision 11). A frosted window then
renders its frost and its flattened layer on whatever backend the device has,
and on the CPU where it has none.

**GD7 — copy engines.** The rectangle copies and fills between device-visible
images that remain after GD1 and GD2 go to the device's copy engine — the
BCM2711's DMA4 2D mode, NVIDIA's copy engine, AMD's SDMA, Intel's blitter — as
`lib/gpu`'s transfer queue.

## 8. Refused by name

- **OpenGL, OpenGL ES, or a GL-shaped compatibility layer**, for §0b.
- **Implementing Vulkan or WebGPU *conformantly* as a specification**, or
  claiming partial conformance under either name (§2.19). The object model is
  borrowed; the conformance claim is not made.
- **An external graphics crate, or Mesa in any form** (§1, §2.12).
- **A GPU-only code path with no software equivalent**, and any consumer branch
  of the form "if accelerated, draw it this way" (§17.3, §2.2).
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
    implicit-level samples and queries, discards and the `InterpolateAt` family
    — gain their interpreter cases, and the coverage test's fragment exception
    goes; `lib/raster`'s row walker and block evaluator agree on every
    sample under a property test, and WinterSun's pictures are unchanged
    through the evolved stages; points, lines and fans, a near-plane clip of
    each interpolation qualifier, centroid, per-sample depth, `SampleMask`,
    alpha-to-coverage, `EarlyFragmentTests`, overlapping quads in one tile and
    every resolve each pin their pixels; a pass larger than its bin budget
    renders whole; `tests/fuzz_gpu.rs` generates images, bind sets, passes,
    draws and dispatches as it does copies.
- **GP4**: the pinned modules are regenerated reproducibly, verify-mode fails on
  drift and runs in `ci`, and every pipeline's conformance case pins its
  software output.
- **GP5**: the pinned set executes through Venus and matches the software
  reference within its stated tolerance; a 2D-only device degrades to software
  rendering with accelerated transfer and flip, and *that* path is tested, not
  assumed.
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
