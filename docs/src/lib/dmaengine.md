# `tairix-dmaengine`

The controller side of `dmaengine-v1`, the seam a consumer driver streams a
cyclic DMA transfer through ([DMA-engine drivers](../drivers/dma.md)).

`Controller` is the endpoint a DMA controller's driver serves under its
node's duty. It is written over the `DmaEngine` and `DmaChannel` class traits
and knows no controller: a request line counts once the kernel attests the
caller holds it, a FIFO once the kernel attests a register window covering the
access and the narrowest window covering the controller's own registers
translates it,
every buffer is carved by the endpoint after every check, and a posted wait is
answered at the first period boundary past the position it names, counted by
which period the channel has reached so coalesced interrupts stay exact. A
channel belongs to the instance that opened it; one that will not take its
reset is withdrawn with everything it may still reach kept.

`Node` is a controller driver's bring-up, over the shared one
([`tairix-drvrt`](drvrt.md)'s `DutyNode`): it reads the node's grants, finds
its duty, register window, channel mask and the windows the engines reach
peripherals and memory through, and places every carve under the memory
window. A driver maps the window, states its engines' reach, builds its engine
and hands it to `serve`, which binds every served channel's line, resets the
channels, declares the device quiesced, and only then binds the endpoint and
parks on it, on peer exits and on the lines. Every failure ends the driver
with a stable exit code (90–95) and its reason logged; every decision is
logged with a stable event id (24 200–24 210).

The endpoint is host-tested over a scripted engine and a kernel that answers
the grant questions by the kernel's own rule; each controller driver drives it
again over its own register model.
