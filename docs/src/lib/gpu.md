# `tairix-gpu` — the GPU seam

`lib/gpu` is how TAIRiX reaches a graphics processor for render and compute
work, and what it does when there is none (`plans/GPU.md`). This page covers
the memory and submission model every backend implements and the software
device that implements it on every target; the images, views, texel buffers,
samplers and transfers of `plans/GPU.md` GP3.1; GP3.2's bind set layouts, bind
sets, pipeline layouts and shader modules; GP3.3's compute pipelines, their
dispatches and the interpreter that runs them; and GP3.4's render pipelines,
passes and draws, rasterised through `lib/raster`'s stages. Indirect draws
arrive with GP3.4.4; reaching a hardware device's driver arrives with GP11.

## A device and what it is

A `Device` is described by a `DeviceInfo`:

| Part | What it says |
|---|---|
| `heaps` | pools of memory, each with its size and whether it is the device's own |
| `memory_types` | the kinds of memory an allocation is made in, each over one heap, with its `MemoryProperties`: device-local, host-visible, host-coherent, host-cached |
| `queue_families` | sets of queues running one kind of work — `QueueCapabilities::TRANSFER`, `COMPUTE`, `GRAPHICS` — and how many queues of each a context has, at most `MAX_QUEUES_PER_FAMILY` |
| `limits` | the bounds it puts on what is asked of it: the largest allocation and the alignments a buffer and its bindings take, image sizes and layers, bind sets, push-constant bytes, binding ranges and texel-buffer elements, workgroup sizes, counts and memory, per-invocation memory, its subgroup size, the longest a submission may run, and a render pipeline's vertex attributes, buffers, offsets, strides and stage components, its colour targets, viewports, point sizes and line widths |
| `capabilities` | the shader capabilities of `lib/spirv`'s allow-list it runs |
| `features` | what it offers past the portable core: anisotropic filtering, mirror-clamp-to-edge, dual-source blending, logic operations, depth clamping, wide lines, large points, and storage writes and atomics from a vertex or fragment stage |
| `formats` | what it states each `Format` can be used for (`FormatFeatures`): sampled, filtered, stored, a colour or depth-stencil attachment, blended, multisampled, resolved, a texel buffer, a vertex attribute |

The portable core is what Vulkan 1.1 requires of every device; anything past
it is a feature a device states, and creation refuses what it has not stated.
A description with a memory type over a missing heap, an empty heap, a family
with no queues, too many or no capability, an alignment that is not a power of
two, a subgroup size that is not a power of two up to `MAX_SUBGROUP_SIZE`,
capabilities out of order, no deadline at all, more colour targets than
`MAX_PASS_ATTACHMENTS`, vertex buffers than `MAX_DRAW_VERTEX_BUFFERS` or vertex
attributes than `MAX_VERTEX_ATTRIBUTES`, or viewport bounds out of order is
refused (`Error::Malformed`).

`Device::software` makes the software device from a `SoftwareDesc`: the
machine's memory, a clock, the process's `JobRunner`, what starts its worker
(`RuntimeThreads`, `lib/rt`'s threads, or `NoThreads`) and the subgroup size
it runs at, `DEFAULT_SUBGROUP_SIZE` unless the reference is being run at the
width of a device it is compared with. It has one heap of half the machine's
memory, one memory type that is every kind at once, and one queue family
running transfers, dispatches and passes one job at a time. It states every
capability a module may declare, every feature, and every use of each format
its encoding can mean: a colour format of float or unsigned normalised
channels renders, blends and resolves, but for the shared-exponent one, which
is only sampled; an integer one renders without either; a signed normalised
one is no attachment. Its buffers align to 16 bytes, the widest vector its copies move,
and its submissions run for at most a minute.

## Contexts

A `Context` is the unit of isolation. Its allocations, buffers, timelines and
queues are its own:

- **A handle is refused wherever it does not belong.** A buffer, allocation,
  timeline or command buffer of another context, or of another device, is
  refused with `Error::Foreign` where it is named — in a command, a buffer's
  creation, or a submission. A handle carries its device's identity as well as
  its context's, so two devices whose first objects share keys still cannot
  confuse them.
- **A released object is refused** (`Error::Stale`). Objects are filed in
  `tairix-collections`' `SlotMap`, whose generations refuse a key that outlived
  its object however often its slot is reused.
- **Memory is shared only by export.** `Allocation::export` makes a token
  another context of the same device takes in with `Context::import`. The
  memory stays charged to the context that allocated it for as long as anything
  holds it.
- **Dropping the context releases everything it holds.** Its queued work is
  dropped, work already running finishes and is discarded, every thread
  waiting on one of its timelines wakes to `Error::Destroyed`, and its handles
  refuse every request from then on.

## Memory

An allocation is made in one memory type and charged to two budgets: its
context's limit (`ContextDesc::memory_limit`, by default as much as the heaps
hold) and its heap's size. It is refused past the context's limit
(`Error::OverLimit`), then past the heap (`Error::HeapFull`), and only then
attempted, refused if the memory cannot be had (`Error::OutOfMemory`). A
refused attempt takes nothing from either budget but the room its context's
table of allocations made ahead of it. Memory is zeroed when made.

A charge is given back when the memory is finally released, which may be after
its handle: a buffer, an image, an importer, a bind set or queued work can
still hold it.

**Host memory a context makes the device hold is charged to the same limit.**
Every object — a buffer, image, view, sampler, timeline, layout, bind set,
shader module or pipeline, and an allocation's memory itself — is charged for
the host bytes it keeps (its tables, a module's words and the validator's
tables, a pipeline's lowered program and a workspace for every participant of
its need, a set's descriptors) when it is made, and gives them back when its
last holder lets it go. An update's bytes are charged once, as they are
recorded, however often the command buffer is submitted. A submission is
charged for what its commands keep queued, for the tables its largest
dispatch or pass makes as it runs, and for the placements of the tiles its
dispatches share, so a submission past the limit is refused before anything queues. The context's own state is charged as it is made, and each of
its tables and queues — the objects of each kind, its queued jobs, each queue's
order and each timeline's pending signals — grows only by room charged to it
first: a table keeps its room, still charged, once its objects go, so the
slots a context's peak needed are its to reuse, and a queue gives back what a
burst left once it holds a quarter of it. Room made ahead of a refused request
stays, charged, for the next. The device's own room for its contexts — a slot
apiece, and a place on each engine's list for each of their queues, the list
doubling as it grows — is charged to the heap, as is a context's budget itself
for as long as anything charged to it is held. One context therefore cannot
spend the device's memory for every other. A request is asked of the limit for
all it would charge the context — its own bytes and its tables' room — before
any heap: past the limit it is refused with `Error::OverLimit`, and within it,
past the heap the device's own memory comes from, with `Error::HeapFull`.
`Context::memory_used` reports both kinds:
`MemoryUse::allocations` and `MemoryUse::objects`.

The CPU reaches host-visible memory through `Allocation::write` and
`Allocation::read`, at any byte; memory the CPU cannot reach refuses both
(`Error::NotHostVisible`), as does a range past its end (`Error::OutOfRange`).

A `Buffer` is a span of one allocation, at an offset that is a multiple of the
device's `buffer_alignment`, made for the `BufferUsage` it states:
`TRANSFER_SRC` and `TRANSFER_DST` to be copied from and into, `UNIFORM`,
`STORAGE`, `UNIFORM_TEXEL` and `STORAGE_TEXEL` to be bound, `INDIRECT` to be
read by an indirect dispatch, and `VERTEX` and `INDEX` to be read by a draw. It
holds its memory while it lives.

## Images, views and samplers

An `Image` is placed in an allocation like a buffer, at an offset that is a
multiple of the alignment and within the size `Context::image_requirements`
states — answered from the device's description alone, without its lock; the
texels' layout is the device's own, and they enter and leave only through
copies. Its `ImageDesc` is checked against the device:

| Refused | As |
|---|---|
| an empty extent, mip count, layer count or usage | `Error::Empty` |
| a usage no image declares | `Error::Usage` |
| a row with height or depth, a rectangle with depth, a box with layers | `Error::Shape` |
| a size past the device's, or more mip levels than the size halves through | `Error::TooLarge` |
| for storage use, more bytes in all than `max_storage_image_bytes`: an atomic names a texel with a 32-bit offset | `Error::TooLarge` |
| a sample count other than 1 or 4; 4 for anything but a single-level two-dimensional attachment with no storage use whose format multisamples | `Error::Samples` |
| a usage the device does not state of the format | `Error::Unsupported` |

An `ImageView` sees some mip levels, layers and aspects of an image in one
dimension — 1D, 1D array, 2D, 2D array, cube (six square layers), cube array
(whole cubes) or 3D — and in the image's format or its sRGB twin, and refuses
anything else (`Error::Shape`, `Error::Aspect`, `Error::OutOfRange`); a cube
array needs a device that runs `SampledCubeArray` (`Error::Unsupported`). A
`BufferView` is a texel buffer: a buffer's aligned range of whole texels of a
format the device states for the texel-buffer use the buffer was made for, at
most `max_texel_buffer_elements`. A `Sampler` filters, addresses, clamps its
level of detail and compares as its `SamplerDesc` says — its `CompareOp` the
one `lib/raster` defines for every depth and stencil test — anisotropy and
mirror-clamp-to-edge need their features, and unnormalised coordinates take
only their restricted form.

A view holds its image, and a texel buffer its buffer, as a buffer holds its
memory.

## Bindings and shader modules

A `BindSetLayout` is a list of `LayoutBinding`s, by ascending number and one
apiece (`Error::Bindings`). Each names its binding, the
`tairix_spirv::BindingKind` it holds — so an image binding states the
dimension, layers, samples, channel type and storage format a module declares —
how many descriptors, the `Access` its shaders may make and the `Stages` that
see it. A binding of no descriptors or stages is refused (`Error::Empty`), as is
one past the device's `max_binding_number` (`Error::OutOfRange`), a write to a
kind that is only read, and a shape no admitted module declares: a layered 3D
image, a multisampled storage image or cube, a texel buffer of another
dimension, a storage format whose channels read otherwise (`Error::Shape`); and
a layout of more descriptors, its bindings' together, than the device's
`max_bind_set_descriptors` (`Error::TooMany`), as a set of it looks each one up
under the device's lock. A layout is checked and its tables built before the
lock is taken, so a refusal costs other contexts nothing.

A `BindSet` is made whole against its layout: one `BindSetEntry` for each
binding, in order, with as many `Resource`s as the binding's count
(`Error::Bindings`); one whose descriptors alone would pass its context's limit
is refused before any is looked up. Each resource is checked once, there, so a
command naming the set checks nothing per binding:

| Binding | Takes | Refused |
|---|---|---|
| sampler | a `Sampler` | — |
| sampled image | an `ImageView` of one plane of an image made `SAMPLED`, of a format the device samples | another plane count (`Error::Aspect`), dimension, layers, samples or channel type (`Error::Shape`) |
| storage image | an `ImageView` of one level of an image made `STORAGE`, of a format the device stores | another shape, or a format other than the binding's unless it names none (`Error::Shape`) |
| combined image sampler | a view as a sampled image takes, and a `Sampler` | linear filtering of a format the device does not filter (`Error::Unsupported`), a comparison of what is not depth (`Error::Aspect`), unnormalised coordinates over more than one level or layer of a row or rectangle (`Error::Shape`) |
| uniform or storage texel buffer | a `BufferView` over a buffer made for that texel use, of a format the device offers for it | another shape or format |
| uniform or storage buffer | a range of a buffer made `UNIFORM` or `STORAGE`, aligned to the device's binding alignment and at most its range | an empty range (`Error::Empty`), misaligned (`Error::Misaligned`), past the buffer (`Error::OutOfRange`) or the device's range (`Error::TooLarge`) |

A resource of another kind is refused (`Error::Shape`), one not made for the use
too (`Error::Usage`), and a format the device does not offer for it
(`Error::Unsupported`). A bind set holds every resource it binds, and through
them their memory, until it is dropped, whatever became of their handles.

A `PipelineLayout` is up to `max_bind_sets` bind set layouts (`Error::TooMany`),
set `i` laid out by the `i`th, and its `PushConstants`: whole words
(`Error::Misaligned`) up to `max_push_constant_bytes` (`Error::TooLarge`), and
the stages that see them — some where there are bytes and none where there are
not (`Error::Empty`). It is the `tairix_spirv::Layout` an entry point is agreed
against, so an entry point's binding or push constants its stage cannot see is
a disagreement like any other.

A `ShaderModule` is a module `lib/spirv` admitted, made the context's; one
declaring a capability the device does not run is refused
(`Error::Unsupported`). `ShaderModule::module` reads it back.

## Compute pipelines

`Context::create_compute_pipeline` makes a `ComputePipeline` from a
`ComputePipelineDesc`: a module, the name of one of its compute entry points, a
pipeline layout and specialization values. The entry point is agreed against
the layout and its constants specialized, then lowered once, there, on the
calling thread and not under the device's lock. Creation refuses:

| Refused | As |
|---|---|
| no compute entry point of the name | `Error::EntryPoint` |
| a binding, access, stage or push constant the layout does not provide | `Error::Disagreement` |
| a specialization value of no constant, given twice or wider than its constant, or a workgroup size it makes zero | `Error::Specialization` |
| constants whose evaluation passes `MAX_SPECIALIZED` | `Error::TooLarge` |
| a workgroup past `max_workgroup_size` along an axis, past `max_workgroup_invocations`, or past 1024 invocations | `Error::TooLarge` |
| a program past the lowering budget, its calls inlined: 2<sup>20</sup> instructions, edge copies or switch cases, 2<sup>19</sup> blocks, 2<sup>18</sup> instances, 2<sup>23</sup> operands walked, 2<sup>22</sup> words and 2<sup>24</sup> parts of constants, 2<sup>24</sup> structure members read, 2<sup>22</sup> scalars and 2<sup>24</sup> type levels of explicit-layout plans, 2<sup>16</sup> words a value and 2<sup>28</sup> before registers are allocated, 2<sup>26</sup> steps widening live ranges | `Error::TooLarge` |
| registers and private memory past `max_invocation_memory` an invocation, values a tile shares past it, or workgroup memory past `max_workgroup_memory` | `Error::TooLarge` |
| the program's host memory, its table's room or its working memory for every participant, past the context's limit | `Error::OverLimit` |
| those the heap cannot hold, asked before any working memory is made | `Error::HeapFull`, `Error::OutOfMemory` |

A lowering that finds an invariant the validator promised broken refuses the
module (`Error::Malformed`) rather than trusting it.

## Render pipelines

`Context::create_render_pipeline` makes a `RenderPipeline` from a
`RenderPipelineDesc`: a vertex stage and, unless the pipeline renders depth and
stencil alone, a fragment stage, each an entry point of its stage in a module
with its specialization values; a pipeline layout; the vertex buffers a draw
binds, each with its stride, its step — by vertex or by instance — and its
attributes; the topology and primitive restart; rasterization, depth-stencil,
multisample and blend state; and the colour targets. Each stage is agreed,
specialized and lowered as a compute entry point is, once, without the device's
lock. Each vertex input is fed by the attribute at its location; each fragment
input component by the vertex output component at its location, of the same
type and width; each fragment output writes the target at its location.
Creation refuses of each stage what a compute pipeline's creation does — its
entry point, layout, specialization, lowering and memory — and:

| Refused | As |
|---|---|
| a vertex input no attribute feeds, or feeds as another component type or 64 bits wide; two attributes at one location; a fragment input no vertex output provides; a fragment output with no target, of another component type or 64 bits wide; a second colour output at a location but 0; a blend reading a second colour the stage does not write, or for a target past the first | `Error::Interface` |
| vertex buffers, attributes or targets past `max_vertex_input_bindings`, `max_vertex_input_attributes` or `max_color_attachments` | `Error::TooMany` |
| a stride, an attribute offset, a line width, or the vertex outputs' or fragment inputs' components past the device's | `Error::TooLarge` |
| an attribute location past `max_vertex_input_attributes` | `Error::OutOfRange` |
| a format without its use — an attribute's `VERTEX_INPUT`, a target's `COLOR_ATTACHMENT`, a blended target's `BLENDED`, a depth-stencil `DEPTH_STENCIL`, any of them `MULTISAMPLED` at four samples — or a feature the device has not stated: dual-source blending, a logic operation, depth clamping, a line wider than a pixel, a stage writing storage, images or atomics; restart of a list topology | `Error::Unsupported` |
| a sample count other than one or four | `Error::Samples` |
| a depth test or write of a format without depth, or a stencil test of one without stencil | `Error::Aspect` |
| a line width under one or not finite, a depth bias not finite, a write mask past four channels | `Error::Shape` |

A draw renders as Vulkan 1.1 states, with every rule a picture depends on
fixed:

- **Assembly.** Points, lines, line strips, triangles, triangle strips — an odd
  one's corners `i, i+2, i+1` — and fans, whose corners are `i+1, i+2, 0`; a
  primitive's first corner provokes it. Restart starts a strip or fan afresh at
  an index of all ones.
- **Points and lines** are two triangles each: a point the square of its
  `PointSize`, held to `1..=max_point_size` where points may be large and one
  otherwise, dropped when its centre is clipped, `PointCoord` from its upper
  left; a line the rectangle its width wide about its clipped segment. Both face
  the front.
- **A draw touches no pixel outside its viewport**, its scissor or its pass's
  area: a primitive is clipped in clip space to a guard band about its
  viewport, and each sample is held to the viewport's rectangle — its left and
  top edges in, its right and bottom out — as a clipped edge would hold it.
- **Triangles** are clipped in clip space, their depth clamped to the
  viewport's range in place of clipped where the pipeline clamps; culled by the
  winding of their first piece with area, the front counter-clockwise or
  clockwise as stated, the framebuffer's y axis down; and offset in depth by
  their bias, found once a primitive.
- **Interpolation** is perspective-correct, linear in the framebuffer
  (`NoPerspective`), or the provoking vertex's (`Flat`); a `Centroid` input is
  interpolated at the pixel's centre where the primitive covers it, else at the
  first sample it covers, whatever the tests make of them.
- **Coverage** is what the primitive covers within the pipeline's sample mask,
  the fragment's `SampleMask` input — before any test — then narrowed by its
  `SampleMask` output and by alpha-to-coverage: the first target's alpha's share
  of the samples, from the first, rounded to the nearest. Depth is found at each
  sample unless the fragment writes its own, then clamped to the viewport's
  range where depth is clamped and to `[0, 1]` for a normalised format.
- **Tests and writes.** With `EarlyFragmentTests`, depth and stencil are tested
  and written before shading and nothing the shader does undoes them, its
  `FragDepth` ignored. Otherwise they are tested before shading only where the
  result cannot differ — no storage writes or atomics, no `FragDepth`, and,
  where the shader can discard or change coverage, no stencil fail or depth-fail
  operation but keep — a sample that fails written then and one that passes
  tested again and written after it. Each sample's tests, blend and write are
  made in the order its primitives were drawn.
- **Colour.** A float or normalised target blends as its blend says. A logic
  operation, where the pipeline has one, takes blending's place in every target:
  it combines an integer or non-sRGB normalised target's bits, and the rest are
  written unblended. A target's write mask keeps the channels it does not name.
  A target no output of the fragment stage writes is left as it was; an output
  of fewer components than its target writes zero to the rest. A four-sample
  target resolves by its samples' average, in linear light for an sRGB format.

## Submitting work

A `CommandBuffer` records:

| Command | What it does | Rules |
|---|---|---|
| `copy_buffer` | moves regions of one buffer into another | whole words; at most `MAX_COPY_REGIONS` regions; no byte written twice, and within one memory no byte both read and written |
| `fill_buffer` | fills a span with a 32-bit word, little-endian | offset and size whole words |
| `update_buffer` | writes bytes recorded with the command | whole words, at most `MAX_UPDATE_BYTES` |
| `copy_buffer_to_image`, `copy_image_to_buffer` | moves texels between a buffer laid out in rows and slices and one aspect of a single-sampled image's level and layers | each region within both; a row at least the region's width and whole texels, as many rows a slice as the region; the depth of `Depth24PlusStencil8` never copied; no texel or byte written twice; the buffer and the image not sharing memory |
| `copy_image` | moves texels between images of one sample count, the same plane, and texels of one size | each region within both; no texel written twice, and within one image none both read and written; two images sharing memory refused |
| `clear_image` | writes one value over ranges of an image, in each aspect a range names | a float, signed or unsigned colour of the format's kind, or a depth within `0.0..=1.0` and a stencil value |
| `dispatch` | runs a compute pipeline's workgroups, so many along each axis, with its bind sets and push constants, read as words once as they are recorded and shared by every submission | one bind set for each set of the pipeline's layout, each laid out as that set (`Error::Bindings`); the push constants exactly the layout's bytes (`Error::Shape`); counts within `max_workgroup_count` (`Error::TooLarge`); at most `MAX_DISPATCH_SETS` sets and `MAX_PUSH_BYTES` bytes (`Error::TooMany`, when recorded) |
| `dispatch_indirect` | `dispatch` with its counts three little-endian words read from a buffer when it runs | the buffer made `INDIRECT` (`Error::Usage`), the offset whole words (`Error::Misaligned`), and twelve bytes within the buffer (`Error::OutOfRange`) |
| `render_pass` | loads its attachments — keeping what each held or clearing it within the pass's area — runs the draws its closure records in order, and resolves its four-sample targets | some attachment (`Error::Empty`); each a two-dimensional view of one level and layer of an image made for the attachment use (`Error::Usage`, `Error::Shape`), in a format the device states the use and sample count of (`Error::Unsupported`), a depth-stencil view of all its format's aspects (`Error::Aspect`); all of one extent (`Error::Shape`) and sample count (`Error::Samples`), none overlapping another in memory (`Error::Overlap`); a resolve from four samples to one (`Error::Samples`) of the same extent and format (`Error::Shape`) the device resolves (`Error::Unsupported`); a clear of the format's kind, its depth within `0.0..=1.0` (`Error::Shape`); an area not empty (`Error::Empty`) and within the attachments (`Error::OutOfRange`); at most `max_color_attachments` targets (`Error::TooMany`), and `MAX_PASS_ATTACHMENTS` when recorded |
| `draw`, `draw_indexed` | within a pass: instances of a range of vertices, or of the vertices a range of indices names, each index added to a base vertex, drawn with a pipeline, its bind sets, push constants and vertex buffers, a viewport, a scissor, a stencil reference and a blend constant | the pipeline's targets and depth-stencil format the pass's (`Error::Interface`), at its sample count (`Error::Samples`); sets and push constants as a dispatch's; a vertex buffer made `VERTEX` for each of the pipeline's (`Error::Bindings`, `Error::Usage`), indices made `INDEX` at a multiple of their size (`Error::Misaligned`); each buffer's offset within it, a viewport finite, wider than nought and not flat, within the device's dimensions and bounds and its depths within `0.0..=1.0`, a scissor whose far edges a `u32` holds, ranges in order and a blend constant finite (`Error::OutOfRange`); nothing a stage of it sees, nor its vertices or indices, reaching an attachment the pass writes, and nothing a stage of it writes reaching a depth-stencil attachment the pass only tests, which its loads keep and none of its draws writes (`Error::Overlap`); at most `MAX_DRAW_VERTEX_BUFFERS` vertex buffers when recorded (`Error::TooMany`) |

Recording checks only that what is named belongs to the context. Everything
else is checked when a `Submission` carries the buffer to `Queue::submit`,
**whole and before anything is queued**: every buffer and image is live and
made for the use, every range is within what it names, no region is empty,
every command is work the queue's family runs (`Error::Unsupported`), every
signal advances its timeline, the deadline is neither zero nor past the
device's longest, and no fixed bound is passed — `MAX_SUBMISSION_BUFFERS`
command buffers, `MAX_SUBMISSION_COMMANDS` commands, `MAX_SUBMISSION_POINTS`
waits and as many signals. A refusal is a `SubmitError` naming the command,
counted across the submission's command buffers, and changes nothing but the
room its queues made ahead of it: no byte is written, no signal is queued, no
fence is used. Nothing is clamped into range. A pass counts as a command and
each of its draws as another, so a refusal names the draw.

A draw's scissor is held to its pass's area, and its samples to its viewport.
Its vertex attributes and indices
past their buffers read zero, as a shader's accesses past a binding do; an
attribute's format lacking a channel its input reads gives it zero, its alpha
one.

The bounds are security bounds rather than capacities: they cap the work one
request asks of the checker and do not move to admit an oversize submission.

## Order and synchronisation

- **A queue runs its work in the order submitted**, each submission seeing
  everything the ones before it did. A submission waiting for a timeline holds
  back the ones after it on its queue.
- **A `Timeline` only advances.** The CPU sets it with `Timeline::signal`,
  which must pass where it stands and stay short of every signal queued work
  will make; a submission's signal must pass everything queued before it. A
  value is **reached only once every signal up to it has landed**, so work
  completing out of order on two queues never makes a wait see a value whose
  earlier work is still running.
- **A `Fence` is one submission's completion.** `Fence::wait` and
  `Timeline::wait` park on the timeline they wait for and are woken by what
  advances it, the context's loss or its release; nothing spins, and a
  timeline no one waits on wakes no one.
- **A submission may wait for a value no work has yet been asked to signal**:
  it runs once the value lands.
- **The device is had in turn.** Every request takes the device's model in
  the order it asked, so a thread that calls again at once never keeps
  another out. Waiters park in eight places by turn, and a turn's end wakes
  only the next turn's place, so it wakes the next locker alone until more
  than eight wait. A waiter gives its turn up as it parks and has the model
  again only in a turn of its own.
  Nothing is zeroed while the model is held, and no memory — an allocation's
  words, working memory, a pipeline's program or a job's work — is let go
  while it is, whichever holder lets it go last, but working memory shrunk in
  place, what the heap would otherwise refuse room for, and what the host
  refuses the room to set aside.

Releasing a timeline while queued work still waits on or signals it lets that
work finish first.

A dispatch's workgroups run in no order against each other; every command after
it sees everything it wrote. So with a pass: its draws' samples are tested,
blended and written in the order drawn, but the memory its shaders write is
ordered with nothing else in the pass, and a pass that reads through a binding
memory one of its shaders writes, or whose fragments store to one word, leaves
what it read and that word to the device, as racing workgroups do; every
command after it sees everything it wrote. An indirect dispatch whose counts, read when it
runs, pass the device's loses its context (`Loss::Fault`), and a count of zero
along an axis runs nothing.

## Deadlines and loss

Every submission carries a deadline, counted from when its work starts. Work
that runs past it **loses its context** (`Loss::Overran`); a lost context
refuses all work (`Error::Lost`), its waiters wake to the loss, and it only
releases what it holds. Every other context's work goes on.

A device engine running several jobs at once is reset when one overruns: the
context of every job past its deadline is lost, as is the context of every
other job that had begun (`Loss::Reset`), since what it had done cannot be
vouched for; the rest go back to wait their turn, ahead of what came after
them. Work the device finds malformed while running it loses its context too
(`Loss::Fault`), and so does work whose tables the host refuses as it starts,
past what its submission reserved (`Loss::OutOfMemory`).

## The software device

The software device runs its work on a thread of its own: a submission or a
CPU signal queues the work and wakes it, so neither runs a job before
returning, as on hardware. The worker parks holding only a weak reference to
the device, so it never keeps an idle device alive — a job it runs keeps the
device until that job ends — and it ends once the device is released. Where no thread can be started (`NoThreads`, or the host and
`wasm32` under `RuntimeThreads`) a job runs on the thread whose act made it
ready — the submission, the CPU's signal, or the completion before it — or,
when its engine is already running work, on the thread running it, which takes
the next job when it finishes.

A job counts its work — the bytes its transfers move, the words over lanes its
dispatches compute, and what locking and resolving each dispatch costs — and
reads the clock each 2<sup>20</sup> of it, stopping before its next piece of
work once its deadline has passed. Each piece of a dispatch starts from where
the job stood, and the job goes on from the most any piece that claimed
workgroups left — once one read the clock, which covers where the job stood,
the others count only their own work — so a job of many small dispatches is
checked as a job of one large one. A reading costs a call into the kernel and
this much work a thousand times that, and a job that has finished its last
piece is never stopped.

Each allocation's words — its bytes in little-endian order, the last word
padded — have a lock of their own, so the CPU's reads and writes and the work
running over them never touch them at once. Lockers have the words in turn, in
the order they asked, so a CPU thread writing again and again cannot keep a
command out, nor a command the CPU. A command locks every memory it names, a
pair in one order every locker shares. Neither the CPU nor the work holds the
device's lock while it moves bytes, and an allocation's words are made, and
let go by whichever holder goes last, outside it, as zeroing and freeing them
take as long as they are large: the charges stand for them meanwhile. Moving whole words costs no more
than moving bytes did: on the development host a 64 MiB host write, device
copy and host read each run at about 20 GB/s either way.

An image's texels lie mip level by mip level, each layer a box of rows that
start on a word, each texel its samples in turn; a depth-stencil texel holds
its depth and its stencil in eight bytes. A texel is read and written through
its format's encoding: unsigned and signed normalised, integer and float
channels, the sRGB transfer through tables made from `lib/colour`'s once a
device, and the packed 10:10:10:2, 11:11:10 and 9:9:9:5 encodings.

A pass locks every memory it reaches once, as a dispatch does: gathered from
its attachments, its draws' buffers and each distinct set they bind and sorted
once, so a pass of many draws costs its memories' logarithm. A run of draws
of one pipeline and the same sets resolves their descriptors once. It runs its
draws a flush at a time in bounded memory: up to 16 batches of 32 primitives
of one instance, each batch's distinct vertices fetched and shaded a tile at a
time and its primitives assembled, clipped, culled and set up, the batches
spread across the runner's participants; then every bin of the pass's area a
batch reaches is drawn across them. Bins are squares ruled from the
framebuffer's origin, at most 128 pixels a side and small enough that their
samples, every attachment's together, take at most 256 KiB, down to an 8×8
block. A bin is drawn in place in the attachments' memory, its primitives in
order: each piece of a primitive's clipped polygon set up as it is reached,
its coverage found a block at a time, the covered quads packed sixteen to a
fragment tile and shaded together, then each sample tested, blended and
written in the order drawn. Bins meet at multiples of their side, where every
attachment's texels start on a word as each row does, so bins drawn at once
never write one word, and a pass leaves the same bytes however many
participants draw it. The first flush
clears every bin the pass clears before drawing in it, and the last resolves
every bin, so a pass of no draws clears and resolves too. The tables a pass
makes as it runs — its gathered memories, locks and views, each run's
descriptors, its draws' own, a flush's batches and each participant's tile
outputs — are charged when it is submitted.

## The interpreter

A pipeline's entry point is lowered into a program for a *tile* of up to 64
lanes: its calls inlined, each call site a construct whose merge is the code
after the call; its blocks laid out so every construct comes before its merge
and a loop's construct before its continue construct and that before its merge,
and each block after every block that branches to it but round a loop, so a
join and a case fallen into follow all their way in; the blocks no branch from
the entry reaches left out, since they never run, and the variables only
another entry point reaches; every pointer's root resolved; values the same on
every lane computed once a tile; and registers allocated over live ranges
widened over the loops other lanes may run again while one holds a value — to a
loop's end for a value carried into it, and to its start for a value the tile
shares that is carried out of it, since such a register is written whichever
lanes run. Lowering's time grows with what it walks — every inlined
instance's operands and the tables it spans, however little a walk builds —
and its budget bounds each: each structure's member offsets are found once,
zero values share runs of zeros that together hold under twice the longest,
and a switch makes one edge for each target however many labels name it. A
vector's component, a part of a constant, a shuffle's run of one source, and a
composite of one register's own words in order are read where they lie rather
than copied — a part of a larger composite is copied out, so the whole is not
kept live as long as the part; and a copy of the whole of one register into a
fresh one of its size and class, where the first was written before and is
last read there, gives the second its slot and goes, so a chain of insertions
writes each part once.

A vertex or fragment entry point is lowered as a compute one, its workgroup of
one invocation: its located inputs and outputs, and the built-ins the fixed
function feeds and takes, become ports onto the private words they are loaded
from and stored to; the built-ins a tile's placing gives — `HelperInvocation`
among them — are given. A vertex tile shades up to 64 of a batch's distinct
vertices; a fragment tile sixteen 2×2 quads, a quad's lane bit 0 across and bit
1 down. A helper lane — a quad's pixel its primitive does not cover — takes part
in derivatives and makes no buffer store, atomic or image write; subgroup
operations leave it out and it reads zero from them. A discarded lane ends, its
tile told which, and a derivative across a lane that has ended reads its own
lane's value.

A tile runs each instruction over the span of its lanes in the block it is in,
each value a plane of words, a lane apiece. It keeps every block's waiting
lanes and runs the earliest, so lanes that part at a branch meet again at its
merge and none laps another round a loop. A workgroup of more than 32
invocations takes tiles of its own, each suspended at a barrier until all have
reached it or ended; smaller workgroups share a tile. A program with no barrier
runs a workgroup's tiles in turn, each to its end, in one tile's memory, so its
workspace holds one tile's however many a workgroup takes. Where each tile's
lanes stand is laid out once a piece, and each batch writes only its
workgroups' ids. A subgroup is the device's subgroup size of a workgroup's
invocations, in order.

**Every value is defined**, so nothing a module computes depends on the
machine, the run or another context's work:

- arithmetic wraps; division and remainder by zero give zero, and `MIN / -1`
  gives `MIN`; a shift by the width or more gives zero;
- a float is computed as a double and rounded once to its width; minimum,
  maximum and clamp take GLSL's formulas, never a choice of zero's sign the
  target makes;
- a float converted to an integer saturates, and a `NaN` gives zero;
- `OpUndef`, an undefined shuffle component, an unwritten variable and
  workgroup memory read zero, as does a component past its vector;
- a subgroup operation folds its active lanes in lane order, and a lane it
  names that is not running gives zero; a ballot is read only in the bits of
  its subgroup's invocations;
- a narrow switch selector reaches the cases its literals name as the
  selector's own width holds them;
- an access past what a pointer reaches — a binding's range, a view's extent,
  levels or layers, a variable — reads zero, one for an alpha the format
  lacks, and writes nothing, scalar by scalar; a texel written with fewer
  components than its view has writes zero to the rest;
- a border colour fills only the channels a view's format holds, in what they
  read as, the rest as a texel lacking them; a depth reference is held to
  `[0, 1]` only for a normalized format;
- a sample with a reference through a sampler without a comparison compares
  as `LessOrEqual`.

A fetch adds its offset to the integer coordinate; a projective coordinate is
divided by the component past the view's spatial axes; and a cube's level of
detail is taken from its gradients carried onto the face the direction meets,
as Vulkan's cube-map derivative transformation does.

A dispatch locks every memory it reaches and shares it between the process's
runner's participants as atomic views of its words, a shader atomic being the
word's own. It runs as a piece for each participant, each in a workspace of
its own and claiming batches of workgroups from one shared cursor until none is
left, so a costly stretch of them is shared out rather than left to one piece;
the bytes a dispatch leaves are the same however many participants run it and
in whatever order, unless its workgroups race each other: one word stored by
two, or where an atomic's returned value lands. A piece clears the registers
and block masks it runs in before its first batch, each batch its workgroups'
memory before any of their tiles starts, and each tile's start its private
words and the words its lanes share, so nothing of another job, or another
context, is ever read. Workgroups are counted in 64
bits, so a dispatch of more than four billion has an id for every one, on every
target. A dispatch's memories are found once, when its bind sets are made, each
descriptor's by its place among its set's, and a pipeline places each binding
among its set's descriptors when it is made, so neither checking a submission
nor holding it queued costs anything a descriptor, and a run resolves each
descriptor by index: a set holding every memory the dispatch locks — a lone
set, unless an indirect dispatch reads its counts from another memory — reads
its places as they are, and another's are mapped onto the dispatch's once,
both lists walked in address order.

**The working memory is had before the work starts.** A device keeps one
workspace for each participant its runner can run at once. Each of a
workspace's two parts — the words its tiles and workgroups work in, and its
blocks' lane masks — is fitted to the largest of that part any pipeline still
alive needs, rounded up to a class that keeps its four leading bits, so a part
is at most an eighth larger than that need; each pipeline's context is charged
for a workspace of its own rounded need for every participant, and a
pipeline's creation fits the pool to it or is refused. The pool counts its live
pipelines' needs in a fixed number of classes, so a pipeline comes and goes in
constant time and the device keeps no bookkeeping that grows with the
pipelines it has held. There is one set of workspaces, free or lent to one job
at a time, as a pipeline's claim pays for one set. Nothing of it is made or
zeroed under the device's lock: a pipeline's creation plans what the set lacks
of its need against the set's own parts — and, while a job holds it, what is
kept for it — and charges the heap for it, makes it with the lock let go —
each part that grows, for each workspace, or the set whole where there is
none — and installs it, swapping each part into the set where it is longer,
so pipelines planned together are each covered whichever installs first. A growth needs room for the parts it makes beside those they
replace; a set no pipeline holds is let go first where the heap would refuse
it otherwise, and a growth the heap still has no room for is refused. What the
set no longer needs shrinks in place, which takes no room, and what was let go
is dropped with the lock let go, but where the heap was about to refuse, which
lets it go at once. The set is fitted as it is lent, under the lock a pipeline
dropping takes, so one dropped just before is fitted out of it. While a job holds
the set, what a pipeline made meanwhile is kept until the set comes back and is
swapped in then — let go first if it would grow nothing — and the last
pipeline whose need covers a part of the lent set stays charged, if dropped
meanwhile, until then, whatever pipeline the set was sized by and whether or
not its own growth was yet made. A job of no dispatch is lent nothing, though its
end fits the pool to what is still held as any job's does, and a job's
deadline is counted from once its workspaces are had. The tables a
dispatch makes as it starts — the memories it locks and the descriptors it
resolves — and the placements of its tiles, which a job's dispatches share,
are charged to its context when it is submitted. Once a pipeline is dropped
its memory goes back at the next look — a pipeline planned, refused or not, or
destroyed, a job's start or end, a context's release, or a charge the heap
would otherwise refuse.

A dispatch counts its work in words over lanes — every lane between the first
and last a block runs, which is what each instruction computes, or one lane
for a value the tile shares, which is computed once — the blocks its frontier
looks through included, on the job's meter; a shared flag stops every
participant at its next count once the deadline passes, or one finds the
program broken. The clock is read only before work, and not at all once the
job is stopped, so a dispatch whose last work is done is never stopped: each
command looks before its own work, so one with none — a dispatch of no
workgroups over nothing — does not look at all, and the check before an empty
block that ends its lanes is skipped, as such a block is in no loop. A
participant running pieces in turn finds the cursor spent by its first, whose
count is the job's. A participant therefore stops within 2<sup>20</sup> words
over lanes of a deadline passing, and the charges of one step past it — an
instruction, a tile's or a batch's start, or the empty blocks that end a
tile's lanes — whether a module loops through empty blocks, runs one huge block
or fans out calls, and whether a job is one dispatch or many. Each tile's start
and each batch's is checked before it clears what it clears, and a piece clears
its registers and blocks a check's work at a time; a dispatch's merging,
locking, viewing, placing and resolving are counted a memory or a descriptor at
a time,
and a buffer copy or fill moves 2<sup>20</sup> bytes at most between two looks
at the clock, so neither a set of millions of memories or descriptors nor a
fill of the whole heap runs far past a deadline.

## Tests

- The model's tests drive engines of any depth through dispatch, completion and
  reset: a reset losing the overrunning and begun contexts and running the rest
  again first, in the order they first ran and ahead of any an earlier reset
  left, a context whose own work overran lost for that, and work that had not
  begun spared whatever its deadline, out-of-order completions holding a
  timeline until every earlier signal lands, a timeline no one waits on waking
  no one, queues served in turn, a queue's order kept with room to spare, an
  engine's list grown by doubling, memory made for a context released
  meanwhile refused and given back, work a queue's family does not run
  refused, and descriptions refused.
- The device's tests run the seam on the software device: commands landing
  where asked, handles refused across contexts and devices, export and import,
  refusals that change nothing, ranges refused rather than clamped, copies of
  part of a word, overlaps, memory limits failing closed after the attempt,
  freed memory held by queued work, order, timelines, deadlines, an overrun
  losing only its context, waits across threads, and a device with a worker
  running its work there and its worker ending with the device. The
  accounting tests charge every object to its context until its last holder
  lets it go and a table's room for as long as it is kept, refuse objects past
  a context's limit, refuse an allocation, an object, a submission, a
  pipeline or a context past both its limit and the heap for the limit, charge
  a pipeline all it keeps before its working memory is made, refuse a
  submission whose dispatch tables would pass it before anything queues,
  charge an update's bytes once however often they are
  submitted, give back a timeline's room once its signals land and a lost
  context's holds on its timelines, give back a pipeline's working memory as
  the work that last held it ends, and keep a released context's budget
  charged to the heap while memory charged to it is held. Memory whose last
  holder is a buffer, a bind set, a job, a lost context's queued work or an
  import refused for its table's room goes without the device's lock, and so
  does a pipeline's program, handed out by a release only as its last holder;
  allocations charged together each have their table's room before any memory
  is made, a refused submission leaves the commands it resolved with its
  caller, a signal wakes its waiters before the work it readied runs, and the
  worker is poked with no call while it is not waiting, is parked only while
  it is, and is not poked at all for work that is not ready. The fair lock's
  lockers have the value in the order they asked, across its counters' wrap,
  a locker waits for every turn given before its own, parked in its turn's
  place, a turn's end and a waiter parking wake the next turn's place alone,
  and only where a locker waits there, and a waiter parks with its turn and
  the value given up. The host parks no thread
  — `lib/rt`'s futex calls fail closed there — so a lock's waiters spin in
  these tests; where a turn's end wakes is checked in the lock's own state, a
  test sees each wake its thread asks of the kernel through `lib/abi-trap`'s
  seam, and a memory or program let go under a fair lock is noted on the
  thread that let it go.
- The format, texel, layout, image, sampler and image-device tests hold every
  format's texels to a round trip through its encoding, the sRGB tables to
  `lib/colour`'s transfer, the packed and shared-exponent encodings to their
  rounding and saturation, every image, view, texel-buffer and sampler
  description to the device's refusals, and every copy and clear to the texels
  it lands and the planes, layers and memory it leaves alone.
- The binding tests make a layout of every kind of binding and a bind set
  whole over it, refuse each check on each kind, hold a bind set's memory past
  every handle it was made from, agree an assembled compute shader with a
  pipeline layout — and disagree once its stage cannot see a binding or its
  push constants — and refuse a module, or a cube-array view, a device does not
  state the capability for.
- The interpreter's tests run a case for every instruction the allow-list
  admits and every `GLSL.std.450` instruction, and fail when one is admitted
  with no case; the stage tests feed a vertex batch its indices and attributes
  and take its position, take derivatives across a quad's row or column — a
  helper's lent, a discarded neighbour's its own lane's — report a discarded
  lane, choose an implicit level from a quad's derivatives, and keep a helper
  from buffers, atomics and subgroup operations. The scalar tests pin every defined
  value; the control tests pin reconvergence after a selection, a loop each
  lane leaves at its own count, an early return and an unreachable loop body;
  the image tests read, write, fetch, sample, gather — a cube's across its
  edges included — compare and query; the sampling tests move a fetch by its
  offset, divide a long projective coordinate by the right component, fill a
  border in only the channels a format holds, hold a reference to `[0, 1]` for
  a normalized format alone, take a cube's level of detail from its gradients
  on the face, and round a filtered `NaN` to the one quiet `NaN`; the
  robustness test reaches past every kind of resource and checks every byte
  about what it may write. The layout cases rejoin lanes out of module order —
  a join after both its branches, a case fallen into from one declared after
  it, a phi past a line instruction, a negative 16-bit case, and a value the
  tile shares carried out of a loop another lane runs again.
- The render pipeline tests make a pipeline and refuse each disagreement of
  its stages, targets and state, an attribute past the device's locations, a
  second colour read where its stage writes none or past the first target, and
  a stage writing storage on a device not stating it. The pass tests refuse a
  pass whose attachments or area disagree, a resolve into another extent or
  from one sample, a draw its pipeline could not run, a draw binding or reading
  as vertices or indices what its pass renders to — but only through the
  stages that see it, and a depth-stencil attachment only tested read but not
  written — and an attachment in a format the device does not render to. The
  render tests pin the pixels of coverage and shared edges, each
  interpolation qualifier across a near-plane clip, depth, depth bias, stencil,
  blending, write masks and the blend constant, points and lines, strips, fans
  and a restarted indexed strip, four samples resolved and masked, early and
  late tests counted, discards, quads of many primitives blended in order, a
  pass of many bins and flushes, a culled face and a flipped viewport, a pass
  of no draws, a target no output writes and an output of fewer components, a
  depth-fail operation written though a fragment stage shades, a viewport
  smaller than its scissor, plain and flipped, a sample mask holding early
  depth writes to its samples, and a fragment's `SampleMask` input and centroid
  before its tests; they draw a pass to the same bytes on one participant,
  several in reverse order and several threads, pass over a draw of billions of
  empty instances, restarting or not, gather the memories of thousands of
  draws' distinct sets once, resolve a large set shared by thousands of draws
  once, charge a queued pass for its own bytes, and name a refused draw among
  its pass's commands.
- The lowering tests build one edge for a switch's many labels of one target,
  share runs of zeros, refuse constants past their budget, lay out a
  structure of the most members in one walk, leave another entry point's
  variables alone, read a level-of-detail query's coordinate, and give a vertex
  and a fragment stage their ports and built-ins; the register tests widen ranges over a deep nest of loops and refuse widening
  past its steps.
- The dispatch tests stop a module looping forever, a huge straight-line block
  and a fan-out of calls at the first check past their deadline, and refuse a
  fan-out past the lowering budget; give the same bytes on one participant,
  several in reverse order and several threads; read every undefined form as
  zero in a second context after the first filled its registers and memory;
  read an indirect dispatch's counts when it runs and lose the context past the
  device's; refuse workgroups and working sets past the device's limits; give
  a dropped pipeline's working memory back before the heap refuses, refit only
  the part of a workspace that changes, and refuse a growth the heap has no
  room for with nothing changed, its need uncounted, even after its look
  shrank the set; give back all a plan was charged when its parts cannot be
  made; cover each of two pipelines planned together, growing the set or
  making it whole, in one kind of part or both, whichever installs first, keep
  a set made whole before a job took it beside growths kept meanwhile, and let
  a set no pipeline holds go before the heap refuses a new one; lend the set to
  one job at a time, fitted to what is still held, a pipeline dropped between
  a look and the lending fitted out of it, a plan refused at its limit still
  a look, make nothing while it is lent, keep
  the heap a growth
  meanwhile needs — for the workspaces lent, counting what is kept for them,
  parts or a whole set — and the last pipeline covering it charged until it
  comes back, whatever sized it, a growth still being made included, a claim
  kept paying for every part it covers; keep a growth for the
  lent set through a look, in either kind, cover growths kept together
  whichever came first, and let what would grow nothing go unshrunk; charge a
  job once for the placements its dispatches share, and count a job's work on
  from only the pieces of a dispatch that claimed its workgroups, and of those
  that read no clock once one did, only their own work; claim workgroups in turn
  from one cursor; stop a job of many small dispatches, and of dispatches of no
  workgroups over many descriptors, at its deadline, count the memories a
  dispatch views, check each tile's start and clear a piece a check's work at a
  time, read no clock once a job is stopped, and none after a job's last work —
  a dispatch of no workgroups, one of nothing, or the empty blocks that end a
  tile's lanes; hold a queued dispatch's memories only through its sets; and
  give a workgroup past four billion its own id.
- `tests/fuzz_gpu.rs` (in `cargo xtask fuzz`) records copies, fills and updates
  over buffers sharing allocations, ranges inside, across and past what they
  name, and holds every verdict and every byte to a reference.
- `tests/fuzz_shader.rs` (in `cargo xtask fuzz`) generates compute modules the
  validator admits — every scalar kind, vectors, selections and data-dependent
  loops with early breaks, calls, barriers, subgroup operations, and memory
  reached past every object — and their admitted mutants, and holds the
  interpreter total, never faulting, writing nothing outside its range, and
  giving the same bytes on one participant and several.
- `cargo xtask miri` interprets the crate's tests and the submission fuzz
  harness for the contention its threads put `lib/rt`'s lock and condition
  variable to, and for a dispatch's pieces and a pass's bins sharing memory as
  atomic views on threads of their own, which the determinism tests run at a
  size the interpreter finishes. It skips the test that asks the host for an exbibyte,
  which the interpreter cannot represent, and the deadline, lowering- and
  specialization-budget, instruction-case and many-draw pass tests whose
  extent is their assertion, and leaves out the `lowering_memory` binary, built to the lowering
  bound, and the `fuzz_shader` harness, whose runners deal their pieces on one
  thread; those run in full in the ordinary matrix. The crate forbids `unsafe`.
- Loom has nothing to model. Every hand-off is made under a lock — the
  worker's wake flag, each piece's own workspace, the lease and the turns of
  the fair lock the device's model and every memory are had through included
  — or by the runner's join, but for these, none of which publishes anything
  else. The cursors a dispatch's pieces claim workgroups and a pass's claim
  batches and bins from are counters whose every claim is its own range, each
  batch then had under its own lock and each bin's words by one piece alone.
  The worker's running flag is a set-once publication read with acquire
  ordering. The counts of the needs
  pipelines hold are added to under the device's lock and taken from under
  the lease's as a hold drops: read under the device's lock alone, a count
  stale for a hold just dropped only keeps a free set larger until the next
  look, and the set is fitted under both locks as it is lent, so a hold
  dropped before is fitted out of it and one dropped after finds it lent and
  learns, under the lease's lock, whether any hold still covers each part of
  it. A budget is taken from only under the device's lock and
  given back from any thread as a charge drops, so a stale read can only
  refuse. A dispatch's and a pass's stop and fault flags carry no data, are
  only ever set, and are read for their verdict after the join.
