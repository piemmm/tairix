# `tairix-drv-display-gpu-virtio` — virtio-gpu display

The seat's display on a virtio GPU (virtio 1.2 §5.7, device type 16), on
either transport, and its 3D device served as a GPU where it offers Venus.
Stability tier: `experimental`.

The crate has two targets:

- `src/lib.rs` — `VirtioGpu`, the engine over the bus-agnostic split-virtqueue
  transport, host-tested against a model device that keeps the resources,
  scanouts, cursor, capsets, contexts and blobs a real one would; and, on a
  bare-metal target, `program`, the `Run` bring-up a program hands the engine
  on from.
- `src/main.rs` — the `Run` binary `devmgr` autoloads against the node, which
  serves the seat's display, and the GPU where the device takes Venus work.

## Supported hardware

A virtio GPU on the virtio-MMIO transport a `-M virt` machine presents
(aarch64, riscv64) and the virtio-PCI one a PC presents (x86_64). One signed
bundle binds both: the kernel resolves and role-tags every window before this
process sees it.

## What it does

- **The output is the device's.** The first scanout the device reports
  enabled, at the size its display information states or, where that names
  none, the preferred timing of the EDID it offers. A display event re-reads
  it, and the console follows it to a new mode.
- **A present copies no pixel.** Each frame is the backing of a 2D resource,
  carved under the node's DMA grant and granted to the presenter. A present
  has the device transfer the damaged rectangles from the backing into its
  copy and flush them. Every command is fenced, so the device answers it only
  once it has run it, and a present goes in stages, each sent once the one
  before has run: the device may run its queue in any order. The frame is
  the presenter's again the moment the call returns. A frame's first present
  transfers it whole.
- **Many requests at once.** The control queue carries many requests, each
  in a command slot of its own and answered in its own time; a stage waits
  for its own answers alone. The display path keeps a batch's room, so no 3D
  fence, however long, holds a display request back.
- **Venus.** Where the device offers 3D commands, blob resources, contexts
  initialised for a capset and the Venus capset, and the node holds the
  host-visible region, bring-up reads the capset and the engine makes Venus
  contexts, host blobs it maps into the region, and command streams fenced on
  a context's rings; each command is answered as a completion carrying its
  sender's tag. A device short of any of these draws in 2D alone, which is
  recorded with the reason.
- **The GPU it serves.** Where the device takes Venus work, the driver serves
  it to client processes on its node's channel (`gpu-v1`, `plans/GPU.md`
  GP11.2), published as a `tairix,gpu` child of the node, once the kernel
  binds the channel: for a node a DMA translation unit translates, or on a
  platform with none (GP11.3). A thread of its own
  probes the host's physical device and states it, narrowed to what the
  backend runs, through `tairix_gpu`'s model, which the runtime hosts for
  every client. Each context is a virtio-gpu context of its own, reaching
  no other's memory, so the GPU exports none, holding a
  Vulkan instance, device and queues, one shared blob its replies and
  commands cross, and a feedback page each job's last command writes its
  number into, in a slot the job holds until the number is read: a retired
  fence alone proves nothing. Memory is zeroed before
  it is answered, and host-visible memory is a blob mapped into the
  host-visible region. The display loop alone touches the device: the GPU
  side's orders reach it through a desk it serves after every display command
  it waited for, roused through a port only this process may send to, so no
  GPU request holds the display back and no 3D answer waits on a later wake.
- **Compute.** Bind sets of buffers, compute pipelines and dispatches. A
  module is folded before it is sent — specialized as the seam specialized
  it, and every value it could read unwritten defined — so the host compiles
  the one meaning the seam admitted. An indirect dispatch's counts are checked
  on the device by the pinned `indirect_counts` pass before the dispatch reads
  them; counts past the device's run nothing and lose the context, as on the
  software device. The seam's semantics for what a program reads past its
  bindings or before it wrote it need the host's second robustness level and
  zero-initialized workgroup memory: a host without either serves no GPU.
  Commands or replies longer than a context's shared areas cross a blob made
  for them.
- **Images.** Images, views, texel buffers and samplers; buffer-image,
  image-image copies and clears; and bind sets of every descriptor kind. A
  format's shader uses through an image are stated only where the host's
  robust image access reads past an image's edges as zeros, any image use only
  where the host copies the format both ways, and a float depth format only
  where the host keeps a depth copied in outside `0.0..=1.0`; a depth or
  stencil format only of a queue family running graphics work too; a 24-bit
  depth the host renders none of is its float depth and stencil. From Vulkan
  1.3, or with its extension, a depth is stated sampled only where it is also
  compared, and storage reached through a binding naming no format only where
  the format says it is; an older host's device features grant that reach.
  An image is asked of the host before it is made — cube-compatible where a
  square of one sample with six layers or more, unless the host makes no such
  image of its format, which then has no cube view — kept in the general
  layout, and cleared once bound, so it reads zero whatever its memory held.
- **Drawing.** Render pipelines, passes and draws — direct, indexed and
  indirect — on a host drawing with the software device's meaning: Vulkan
  1.3, where rendering without render pass objects and binding vertex buffers
  at their sizes are core; `maintenance5`, for a point no stage sizes a pixel
  wide and an index buffer bound at its size; full 32-bit indices,
  independent blending and a clamped depth bias; and strict lines, or
  rectangular ones through the line rasterization extension. A host short of
  any states no drawing. A format is stated with four samples only at the
  standard sample positions, and where the host makes an image of four
  samples of it at the widest side stated; an sRGB format is never stated
  resolved, as a host may average its samples encoded. Vertex and index
  buffers bind at their sizes in whole indices, an empty range binding
  nothing, so a read past one reads nought; each draw's scissor is held to the
  pixels its viewport covers, so a point or line stays within its viewport as
  a triangle does. Where the host draws no first instance from a record, an
  indirect draw's records are checked on the device by the pinned
  `indirect_records` pass, each check ordered after the last by a barrier,
  and a pass whose records outrun their slot renders as several rendering
  instances, each copying its records only once the one before has drawn, the
  last alone resolving.
- **The pointer is the device's cursor.** A pointer image is copied into the
  driver's own 64 × 64 image, transferred, and shown on the cursor queue;
  moving it presents nothing.
- **The console it hosts.** Where discovery marked the node `ConsoleHost`,
  the kernel console is painted in memory the kernel carves under the node's
  DMA grant (`console_adopt`), scanned out as a resource of its own while the
  seat shows text, and the rows it paints are transferred on its damage wake.
  One the device would not make is hosted again when the output moves.
- **Power.** Switching off scans out nothing; switching on shows the last
  frame again, a present taken while off included.
- **The host-visible region it is granted.** A virtio-PCI device's
  host-visible region (shared-memory id `1`), which host-visible blobs are
  mapped into, reaches the node as a memory region the driver maps a part at
  a time; bring-up records its size. A virtio-MMIO device's is not decoded.
- **Memory the device may read is never given back.** A buffer whose detach
  the device has not answered stays held, and a resource an unanswered batch
  may have made — a frame's, the console's, the pointer's — is unreferenced
  before its memory is used again or goes back; everything is held for the
  kernel to reclaim, should the device refuse its reset.

## Limitations

- No layer compositor, one scanout.
- A binding of the GPU it serves holds one descriptor: the device states no
  arrays of them. It states no subgroup operation: a Vulkan
  host forms a module's subgroups as it chooses, so the seam's cannot be had
  on it. A virtio-MMIO device has no host-visible region, so Venus is served
  over PCI only.
- The renderer cannot stop a job once started, so a job still running ten
  seconds after it began loses its context.
- On a platform with no DMA translation unit, such as a QEMU board without
  one, the GPU is served with nothing but this driver confining the device's
  DMA: the platform's own limitation (`plans/GPU.md` decision 8). A context
  still reaches host memory alone — its blobs and the host's own Vulkan
  objects — and never names guest memory, so no client can have the device
  read or write the guest's. On a platform declaring a unit the kernel binds
  the GPU's channel only for a node one translates, and otherwise this
  driver drives the display without serving the GPU (`24928`).
- A panic report reaches the console's memory; the screen shows it only while
  this driver still runs.

## Required capabilities

- `CAP_MMIO_MAP` — the transport's windows and the host-visible region.
- `CAP_MEM_DMA` — the frames and the console the device reads.
- `CAP_IRQ_BIND` — the interrupt every command is answered on.
- `CAP_SHM` — granting the frames to the presenter.
- `CAP_IPC_BIND_PRIVILEGED` — the seat's display endpoint.
- `CAP_LOG_EMIT` — the decisions it records.
- `CAP_HW_EMIT` — publishing the GPU's channel as a child of its node.

It runs in user space and never requests `CAP_DRV_KERNEL`.

## Events

| Event | Id |
|---|---|
| The console could not be hosted at the output's mode | 24920 |
| What the seat shows could not be shown | 24921 |
| The console's newest rows could not be shown | 24922 |
| The console is shown on the device | 24923 |
| The host-visible region the node was granted | 24924 |
| The device takes Venus work | 24925 |
| The device takes no Venus work, and why | 24926 |
| The GPU is served on its node's channel | 24927 |
| The GPU is not served, and why | 24928 |
| The GPU's service ended, and why | 24929 |
