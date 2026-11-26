# GPU channel (`gpu-v1`)

`gpu-v1` is how a GPU's driver serves its device to every process that may
use it (`lib/abi/src/gpu.rs`, `plans/GPU.md` GP11.2). The driver runs
[`lib/gpu`](../lib/gpu.md)'s `serve` on the channel; a program reaches it with
`Device::connect` and gets the same `Device` the software one is, so nothing
but what the device states tells the two apart. The frame lives in `lib/abi`;
the bodies are `lib/gpu`'s encoding of the seam's own descriptions, and both
ends link that one encoding, so they cannot drift. A program speaks `gpu-v1`
through `lib/gpu`, as an application speaks Vulkan through its loader rather
than to its driver.

## The channel

Each served GPU has one device channel in the per-node block
`GPU_CHANNEL_ENDPOINTS`, which only the driver bound to that node may bind,
and only where a DMA translation unit translates the node or the platform's
firmware declares none (`plans/GPU.md` GP11.3). The kernel decides it from
the path it recorded for the node's DMA when it loaded the driver, so a
platform declaring a unit — through a malformed table too, whatever the
administrator chose for the functions it would have described — refuses the
bind for a node no unit translates, audited (`DEVICE_CHANNEL_UNCONFINED`,
4270), and the driver drives the device without serving it. The driver never
learns how its node is translated. The driver binds the channel
restricted-sender on `CAP_GPU`, so the kernel refuses a caller without it at
dispatch, and publishes it as a child node carrying `tairix,gpu`. The System Information query
[`GPU_DEVICES`](sysinfo.md) lists every such channel with the node it serves.
Every session holds `CAP_GPU`: a context reaches only what it was given and is
charged for what it makes.

A driver whose device is kept by a thread other than the ones serving it binds
the port `desk_endpoint_for(pid)` — its own `pid` under a fixed tag no pid
reaches — for its serving threads to rouse that thread, and admits only its own
process to it.

## Frames

| Frame   | Header                                                              |
|---------|---------------------------------------------------------------------|
| Request | magic `GPU1` (4), version `1` (2), op (2)                           |
| Reply   | status (4), op (2), reserved `0` (2)                                |

A body is at most `GPU_MAX_BODY` (256) bytes. A reply names the op it
answers, so no reply reads as another's. Its status is `0` for success, a
negated `Errno` for a frame refused before the device was asked (malformed, no
region open, past a bound), or a positive `GpuError` for a device's refusal,
whose detail — a loss, a refused command's index, a disagreement, the sets
and bindings of the image and sampler a refused sampling names — is the body.
A frame naming no op at all is answered with the shared status reply.
`ImageRequirements`, like every request that makes an object, names its
context, whose device answers.

The ops are one for each method of the seam, numbered from 1 in this order:
`Open`, `Close`, `Info`, `CreateContext`, `DestroyContext`, `Status`,
`MemoryUsed`, `Release`, `Allocate`, `Write`, `Read`, `Export`, `Import`,
`DropExport`, `CreateBuffer`, `CreateTimeline`, `TimelineValue`, `Signal`,
`Wait`, `Reached`, `Upload`, `Submit`, `Discard`, `CreateBindSetLayout`,
`CreateBindSet`, `CreatePipelineLayout`, `CreateShaderModule`,
`ImageRequirements`, `CreateImage`, `CreateView`, `CreateBufferView`,
`CreateSampler`, `CreateComputePipeline`, `CreateRenderPipeline`, `Attach`,
`Detach`, `ReadInto` and `WriteFrom`.

## The region

Anything longer than a body — the device's description, a module's words,
bytes written to or read from host-visible memory, commands, a submission's
waits and signals — crosses in one region the client creates and grants to
the channel, between `GPU_REGION_MIN` (128 KiB) and `GPU_REGION_MAX`
(16 MiB), named at `Open`. A body names its bytes there by a `RegionSpan`.
The client may change the region at any time, so the server copies a span out
before it decodes it, and checks every span against the region's length. Bytes
written to or read from host-visible memory are no description, so `Write` and
`Read` lend the span to the copy itself: one copy in the server, not one into
a buffer of its own and another out of it.

- **Commands** cross in region-sized `Upload`s staged for their context, the
  last of a command buffer marked; `Submit` checks and queues everything
  uploaded since the last submission, and `Discard` lets it go. A pass longer
  than the region crosses as its first draws and then the rest. Staged
  commands are charged to the context, so a submission is bounded by the
  seam's bounds and the context's limit, never by the region.
- **A module** crosses whole and is admitted again on arrival, so the region
  must hold the largest module the client makes a pipeline of.
- **What the client derived** while recording — an overlap, a push range — the
  server derives again as the commands arrive; a copy out of an image the
  context does not hold then is refused `Stale` there, since the bytes its
  regions write are the image's to decide.

## Clients, contexts and bounds

A client is the kernel-attested process calling. `Open` maps its region and
watches its exit; `Close`, or the exit, destroys every context it holds and
drops every export token it made. A context is named by the id the server
answered and is found among the caller's own before anything else is read, so
another client's id reads as one that never was (`Stale`). Each client holds:

- at most `MAX_CLIENT_CONTEXTS` (64) contexts and `MAX_CLIENT_EXPORTS` (4096)
  export tokens;
- contexts limited to at most half the device's memory each, the limit a
  context that asks for none is given, so no context holds the device's
  memory from every other;
- at most the kernel's per-sender depth of calls in service, waits among them.

## Waits

A `Wait` is held by the serve loop, not a worker, and answered when its
timeline reaches its value, when its context is lost or destroyed, or when its
timeout passes. The loop's own wait is bounded by the soonest timeout and the
device's soonest job deadline.

## Attachments

`Attach` maps a further region of the client's own, which the client granted
to the channel, for transfers straight into and out of it: a window's frames
among them, which a frame drawn on the device then reaches with one copy, made
here. It answers the attachment's id; `ReadInto` copies host-visible memory
into an attachment and `WriteFrom` an attachment's bytes into host-visible
memory, each range checked against the allocation and the attachment and
refused past either, never clamped. A client holds at most
`MAX_CLIENT_ATTACHMENTS` (8) attachments and `GPU_ATTACHED_MAX` (1 GiB) of
them in all, each a containment bound on what it makes its server map; a grant
that is not there is refused `Stale`, another process's `Foreign`, and one
shorter than what is attached `OutOfRange`. `Detach`, `Close` and the client's
exit let them go, a mapping going once no copy holds it.

## Exports

`Export` answers a random 16-byte token naming an allocation's memory.
`Import` spends it, whatever its answer, filing that memory in a context of
whichever client presents it; `DropExport` lets it go unspent. The token is
the authority, so a client shares memory only by handing the token to whom it
chooses, and it cannot be guessed.

## The serve loop

Requests run on up to `MAX_SERVE_WORKERS` (8) workers, so a slow request — a
context made, memory made, a pipeline lowered — holds back no other client.
Waits, exits, completions and the watchdog stay on the loop. A device that
cannot stop a job once started has one past its deadline answered as overran:
its context is lost and its driver told.
