# tairix-gpu

Stability tier: **experimental**

The device-neutral GPU seam (`plans/GPU.md`): the memory and submission model
every backend implements, and the software device that implements it on the CPU
on every target, a SPIR-V interpreter running its compute and render pipelines
and `lib/raster`'s stages rasterising its passes.

## What it provides

- `Device` — a device and its `DeviceInfo`: memory heaps, the memory types over
  them, queue families, limits, the shader capabilities it runs, its features
  past the portable core, and what it states each format can be used for.
  `Device::software` makes the software device from a `SoftwareDesc`.
- `Context` — the unit of isolation. A handle of another context or device is
  refused wherever it is named; memory is shared only by an `Export` another
  context imports, and stays charged to the context that allocated it. The
  host memory its own state, its objects, its tables' room and its queued work
  keep on the device is charged to its limit too, and `Context::memory_used`
  reports both as a `MemoryUse`.
- `Allocation` — typed memory, charged to its context's limit and its heap's
  size, refused past the limit, then past the heap, and then if the memory
  cannot be had. The CPU reads and writes host-visible memory through it.
- `Buffer` — a span of an allocation made for stated uses.
- `Format`, `Image`, `ImageView`, `BufferView`, `Sampler` — typed texels in a
  layout of the device's own, seen through views of a dimension, levels, layers
  and aspect, texel buffers over buffers, and how an image is sampled.
- `BindSetLayout`, `BindSet`, `PipelineLayout`, `ShaderModule` — the bindings
  a set holds and the stages that see them, a set made whole against its layout
  and holding what it binds, the sets and push constants an entry point is
  agreed against, and a module `lib/spirv` admitted whose capabilities the
  device runs.
- `ComputePipeline` — a compute entry point agreed against its layout,
  specialized and lowered once, at creation, which refuses what the device
  cannot run.
- `RenderPipeline` — a vertex and an optional fragment stage, each lowered
  once, their interfaces matched with the vertex buffers, each other and the
  colour targets, and the fixed-function state a draw runs them through, all
  checked against what the device states.
- `CommandBuffer`, `Queue`, `Submission` — copies, fills, updates, copies
  between buffers and images and between images, clears, dispatches direct
  and indirect, and render passes and the draws recorded into them, checked
  whole when submitted: a refusal names the command — a pass's draw counting as
  one — and changes nothing but the room its queues made ahead of it, and no
  range is clamped. A queue runs its work in the order submitted.
- `Timeline`, `Fence` — a value is reached only once every signal up to it has
  landed; a wait parks and is woken by what advances it.
- Deadlines — every submission carries one, and one that overruns loses its own
  context alone.

## Design notes

- The software device runs its work on a worker thread of its own, so
  submitting and signalling return at once; where no thread can be started,
  a job runs on the thread whose act made it ready. Its memory is words under
  a lock of their own, so the CPU's reads and writes and the work over them
  never meet, and neither holds the device's lock while it moves bytes. Their
  lockers have them in turn, in the order they asked, as the device's own
  model is had, so no thread keeps another out, and a turn's end wakes the
  next locker's place alone. Memory and working memory are made outside the
  device's lock, and let go outside it by whichever holder goes last, but for
  working memory shrunk in place, what the heap would otherwise refuse room
  for, and what the host refuses the room to set aside.
- A refused request makes no change but the room a table or queue made ahead
  of it, which stays, charged, for the next: every check a submission needs is
  made, and everything it is charged for taken, before anything is queued. The
  tables a dispatch makes as it starts, and the placements a job's dispatches
  share, are charged then and made when they run, and a host that refuses
  them loses the context (`Loss::OutOfMemory`).
- The interpreter runs a tile of 64 lanes at a time over a program lowered
  from `lib/spirv`'s lowering view, keeping its lanes' blocks in structured
  order so they reconverge at every merge. Every value is defined and every
  access past what it reaches reads zero and writes nothing, so a dispatch
  whose workgroups do not race leaves the same bytes on any number of cores,
  and nothing of another context is ever read. Its working memory is reserved
  when a pipeline is made — had then, or, while a job holds the workspaces,
  kept of the heap until they come back — a dispatch counts its work and stops
  within a fixed amount of it of its deadline, and lowering is held to a fixed
  budget.
- A pass is drawn a flush of bounded batches at a time: the batches' vertices
  shaded and their primitives set up across the runner's participants, then the
  bins they reach drawn across them, in place in the attachments' memory, each
  bin's primitives in order and its quads shaded sixteen to a tile. Bins meet
  where every attachment's texels start on a word, so they never share one and
  a pass leaves the same bytes on any number of cores. A pass
  refuses a draw that binds, or reads as vertices or indices, what it renders
  to, through a table of the byte spans each bind set's descriptors reach made
  when the set is made, so the check costs a draw a set's logarithm, not its
  descriptors.
- No `unsafe`. The crate is enrolled in `cargo xtask miri` for the contention
  its threads put `lib/rt`'s lock and condition variable to; the tests it
  leaves out, and why, are listed in `docs/src/lib/gpu.md`.

## Tests

The model's own tests drive engines of any depth through dispatch, completion
and reset; the device's tests run the seam on the software device, threads and
its worker included; the format, texel, image and sampler tests hold every
encoding, refusal, copy and clear to what it should land; the binding tests
hold every bind set check and a pipeline layout's agreement; the interpreter's
tests run every instruction a shader may use and pin its defined values,
reconvergence, images, robustness, and its vertex and fragment stages' ports,
quads, helpers and discards; the render pipeline and pass tests hold every
creation and submission check, and the render tests pin the pixels each rule
of a draw gives; the dispatch tests hold deadlines,
determinism across cores, isolation between contexts, indirect counts, limits
and working memory; `tests/fuzz_gpu.rs` holds the submission path to a
byte-array reference, and `tests/fuzz_shader.rs` holds the interpreter total,
contained and deterministic over generated modules and their mutants.
