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
  GP11.2), published as a `tairix,gpu` child of the node. A thread of its own
  probes the host's physical device and states it, narrowed to what the
  backend runs, through `tairix_gpu`'s model, which the runtime hosts for
  every client. Each context is a virtio-gpu context of its own holding a
  Vulkan instance, device and queues, one shared blob its replies and
  commands cross, and a feedback page each job's last command writes its
  number into: a retired fence alone proves nothing. Memory is zeroed before
  it is answered, and host-visible memory is a blob mapped into the
  host-visible region. The display loop alone touches the device: the GPU
  side's orders reach it through a desk it drains, roused through a port only
  this process may send to, so no GPU request holds the display back.
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
- The GPU it serves runs transfers alone — copies, fills and updates; compute,
  images and drawing are `plans/GPU.md` GP5.4–GP5.6. A virtio-MMIO device has
  no host-visible region, so Venus is served over PCI only.
- The renderer cannot stop a job once started, so a job still running ten
  seconds after it began loses its context.
- The GPU's channel binds whether or not an IOMMU translates the node until
  `plans/GPU.md` GP11.3. A context reaches host memory alone — its blobs and
  the host's own Vulkan objects — and never names guest memory, so no client
  can have the device read or write the guest's.
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
