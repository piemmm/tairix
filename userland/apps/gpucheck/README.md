# `gpucheck`

Stability: experimental.

The `gpucheck` command runs the same work on each GPU a driver serves and on
the software device, and states where their answers differ (`plans/GPU.md`
GP11.2, GP5.4–GP5.6). It is the first holder of `CAP_GPU`.

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
lose its context. Every value is exact on any conforming device, and each
step runs on the first queue family stating its kind of work. Two devices
agree when they leave the same bytes and words, refuse that fill for the
same reason, and answer those draws and that dispatch alike.

It finds the served GPUs through the System Information API's `GPU_DEVICES`
and reaches each with `Device::connect`.

Capabilities: `CAP_GPU`, `CAP_SHM`, `CAP_CONSOLE_WRITE`, `CAP_FS_ACCESS`.
