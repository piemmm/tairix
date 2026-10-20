# `tairix-drv-display-gpu-virtio` — virtio-gpu display

The seat's display on a virtio GPU's 2D device (virtio 1.2 §5.7, device type
16), on either transport. Stability tier: `experimental`.

The crate has two targets:

- `src/lib.rs` — `VirtioGpu`, the engine over the bus-agnostic split-virtqueue
  transport, host-tested against a model device that keeps the resources,
  scanouts and cursor a real one would.
- `src/main.rs` — the `Run` binary `devmgr` autoloads against the node.

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
  copy and flush them, in one batch the device answers on its interrupt, so
  the frame is the presenter's again the moment the call returns. A frame's
  first present transfers it whole.
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
- **Memory the device may read is never given back.** A buffer whose detach
  the device has not answered stays held, and a resource an unanswered batch
  may have made — a frame's, the console's, the pointer's — is unreferenced
  before its memory is used again or goes back; everything is held for the
  kernel to reclaim, should the device refuse its reset.

## Limitations

- 2D only: no blob resources, no layer compositor, one scanout.
- A panic report reaches the console's memory; the screen shows it only while
  this driver still runs.

## Required capabilities

- `CAP_MMIO_MAP` — the transport's windows.
- `CAP_MEM_DMA` — the frames and the console the device reads.
- `CAP_IRQ_BIND` — the interrupt every command is answered on.
- `CAP_SHM` — granting the frames to the presenter.
- `CAP_IPC_BIND_PRIVILEGED` — the seat's display endpoint.
- `CAP_LOG_EMIT` — the decisions it records.

It runs in user space and never requests `CAP_DRV_KERNEL`.

## Events

| Event | Id |
|---|---|
| The console could not be hosted at the output's mode | 24920 |
| What the seat shows could not be shown | 24921 |
| The console's newest rows could not be shown | 24922 |
| The console is shown on the device | 24923 |
