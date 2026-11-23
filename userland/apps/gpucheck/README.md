# `gpucheck`

Stability: experimental.

The `gpucheck` command runs the same transfers on each GPU a driver serves
and on the software device, and states where their answers differ
(`plans/GPU.md` GP11.2). It is the first holder of `CAP_GPU`.

The work updates words into host-visible memory, fills beside them, copies
both through memory the device keeps for itself and back, and submits a
fill past its buffer, which every device must refuse. Two devices agree when
they leave the same bytes and refuse that fill for the same reason.

It finds the served GPUs through the System Information API's `GPU_DEVICES`
and reaches each with `Device::connect`.

Capabilities: `CAP_GPU`, `CAP_SHM`, `CAP_CONSOLE_WRITE`, `CAP_FS_ACCESS`.
