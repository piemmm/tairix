# `tairix-drv-dma-bcm2835`

Autoloaded user-space driver for the **Broadcom DMA engines**: the legacy
engines every Raspberry Pi's SPI, PCM/I²S, PWM and SD host stream through, and
the BCM2711's DMA4 engines its HDMI audio streams through.
Stability tier: `experimental`.

## Supported hardware

- `compatible = "brcm,bcm2835-dma"`: the BCM2835, BCM2837 and BCM2711 legacy
  engines, full and LITE channels alike, 32-bit addresses.
- `compatible = "brcm,bcm2711-dma"`: the BCM2711's DMA4 engines, 40-bit
  addresses within a 36-bit interface.

It binds only through the discovery match (`BIND_KEYS`), names no board and no
address, and learns which engines its node holds from the bind key the node
matched. Both read the one request-cell binding.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the node's channel registers |
| `CAP_IRQ_BIND` | each served channel's interrupt line |
| `CAP_IPC_BIND_PRIVILEGED` | the node's endpoint, under its DMA `LinkDuty` |
| `CAP_MEM_DMA` | the control-block chains and the buffers it carves |
| `CAP_SHM` | the buffers, carved as shared regions and granted to consumers |
| `CAP_LOG_EMIT` | the record of every decision |

## Who may write a control block

Only this process. A consumer driver quotes the request line and the FIFO the
kernel attests it holds; the driver carves the buffer, builds every block from
attested facts, and hands back a mapping. See `docs/src/drivers/dma.md`.

## Limitations

- **No pause, no memory-to-memory, no one-shot scatter-gather.** The seam is
  cyclic peripheral transfers; the others arrive with a consumer that needs
  them.
- **Blocks are held to a LITE channel's 65 532 bytes on every legacy
  channel**, a DMA4 block to a gibibyte, and a chain to one page of blocks, so
  a shape is admitted or refused whichever channel of a node serves it.
- **A service late by a whole lap of the buffer is invisible** to the
  controller, whose count is by period. The consumer sees it in the service
  times its waits report.
- **The peripheral side is reached only through the node's translated
  window covering its own registers.** A tree describing no such window
  leaves the controller serving nothing a FIFO can be translated through.
- **Runtime unload** stops every channel with its process; the node's
  quarantine holds their chains and buffers until the next instance resets the
  channels and declares the device quiesced.
- **A driver that ends on its own** resets every channel still running before
  its chains go; a chain whose channel refuses the reset is withheld for the
  quarantine instead.
- **A channel that will not take its reset is withdrawn, not freed.** Its
  chain and buffer are kept while it might still reach them, and it takes no
  request but Stop and Close (`DeviceFault`); at bring-up it ends the driver,
  leaving the node's memory quarantined.

## Testing

Host tests drive each engine against its own register-level model, both
fetching control blocks from one simulated memory as the silicon does: the
chain walked cyclically with one interrupt per period, the block limits, the
pause-drain-reset stop, the error flags, a fetch from freed memory, and a
block whose memory side leaves its buffer; the DMA4 model also holds every
block to its information-word fields, the shifted block names, a 40-bit
buffer and the AXI protection. The endpoint's tests answer the kernel's grant
questions by the kernel's own coverage rule.

QEMU runs no cyclic DREQ-paced chain, so there is no integration test; metal
acceptance is `plans/PI.md` P13.

## References

- BCM2711 ARM Peripherals, chapter 4 (DMA Controller).
- The Raspberry Pi kernel's `bcm2835-dma` driver, for the DMA4 engines.
- `plans/SOUND.md` SND5, SND19c — the staged design this driver lands.
