# tairix-dmaengine

The `dmaengine-v1` controller side (`plans/SOUND.md` SND5, SND19a). Stability
tier: `experimental`.

- `Controller` is the endpoint a DMA controller's driver serves under its
  node's duty, written over the `DmaEngine` and `DmaChannel` class traits and
  nothing of any one controller: attested request lines and FIFOs, buffers it
  carves itself, waits answered by period, channels owned by the instance that
  opened them and withdrawn when they refuse a reset.
- `Node` and `serve` are the freestanding bring-up and loop: the node's grants,
  register window, duty, mask and windows read once, then the lines bound, the
  channels reset, the device declared quiesced and the endpoint served.

Every DMA controller driver links it, so the protocol's rules are written
once: `drivers/dma/bcm2835`.
