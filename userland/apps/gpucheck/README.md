# `gpucheck`

Stability: experimental.

The `gpucheck` command runs the same work on each GPU a driver serves and on
the software device, and states where their answers differ (`plans/GPU.md`
GP11.2, GP5.4–GP5.6, GP8). It is the first holder of `CAP_GPU`.

The work updates words into host-visible memory, fills beside them, copies
both through memory the device keeps for itself and back, and submits a
fill past its buffer, which every device must refuse; then runs every pass
of the pinned set on its sample; then, on a device stating the uses it
makes of its formats, the image work: copies between buffers and images and
between images, clears, and a dispatch reading and writing images, samplers
and texel buffers; then, on one that draws, the render work: strips, a list,
a fan, points and a line, depth-tested, stencilled, culled, blended and
masked, into unsigned normalised, integer and float targets, four samples
resolved, and a draw from a record naming its first instance, which a device
stating no first instances must answer by losing its context; then
dispatches a fill indirectly with counts past the device's own, which must
lose its context; and last runs the differential corpus (`plans/SHADER.md`
SH7): every instruction the validator admits, written with the builder into
kernels — and a draw's fragment stage — whose inputs are words of their
buffer, each word they leave held to the software device's within the
precision Vulkan states for the instruction that computed it. Every value is
one every conforming device defines alike, and each step runs on the first
queue family stating its kind of work. Two devices agree when they leave the
same bytes and words, refuse that fill for the same reason, answer those
draws and that dispatch alike, and leave every word of the corpus within
what it is held to. A device stating less than a step or a kernel uses is not
held to it.

`gpucheck --shader FILE [--words N]` runs a kernel of the caller's own
instead: WGSL, compiled in `lib/sandbox`'s `wgsl` worker, or SPIR-V, told
apart by the file's first word, admitted before any device is given it and
refused by name where it is not. Its one compute entry point runs over one
storage buffer of `N` words, word `i` starting as `i`, beside a neighbour
context's memory holding a random marker; each GPU agrees where its buffer
reads as the software device's, or where both lost the kernel to its
deadline and went on serving. The CPU waits on each submission its deadline
twice over, since the deadline runs only once the device starts the work and
its end is told only after; a submission answered neither way in that time
is a kernel the device never answered.

It finds the served GPUs through the System Information API's `GPU_DEVICES`
and reaches each with `Device::connect`.

Capabilities: `CAP_GPU`, `CAP_SHM`, `CAP_CONSOLE_WRITE`, `CAP_FS_ACCESS`,
`CAP_SANDBOX_SPAWN` (the WGSL compile worker) and `CAP_LOG_EMIT`.
