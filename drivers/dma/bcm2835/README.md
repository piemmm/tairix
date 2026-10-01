# `tairix-drv-dma-bcm2835`

Autoloaded user-space driver for the **Broadcom legacy DMA engines**, the
controller every Raspberry Pi's SPI, PCM/I²S, PWM and SD host stream through.
Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "brcm,bcm2835-dma"`: the BCM2835, BCM2837 and
BCM2711 legacy engines, full and LITE channels alike. It binds only through
the discovery match (`BIND_KEYS`) and names no board and no address. The
BCM2711's DMA4 engines (`brcm,bcm2711-dma`) are a different register model
and are not served here.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the node's channel registers |
| `CAP_IRQ_BIND` | each served channel's interrupt line |
| `CAP_IPC_BIND_PRIVILEGED` | the node's endpoint, under its `DmaController` duty |
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
- **Blocks are held to a LITE channel's 65 532 bytes on every channel**, and a
  chain to one page of blocks, so a shape is admitted or refused whichever
  channel serves it.
- **A service late by a whole lap of the buffer is invisible** to the
  controller, whose count is by period. The consumer sees it in the service
  times its waits report.
- **The peripheral side is reached only through the node's translated
  window covering its own registers.** A tree describing no such window
  leaves the controller serving nothing a FIFO can be translated through.
- **Runtime unload** stops every channel with its process; the node's
  quarantine holds their chains and buffers until the next instance resets the
  channels and declares the device quiesced.
- **A channel that will not take its reset is withdrawn, not freed.** Its
  chain and buffer are kept while it might still reach them; at bring-up it
  ends the driver, leaving the node's memory quarantined.

## Testing

Host tests drive the engine and the endpoint against a register-level model
that fetches control blocks from simulated memory as the silicon does: the
chain walked cyclically with one interrupt per period, LITE limits, the
pause-drain-reset stop, the three error flags, a fetch from freed memory, and
a block whose memory side leaves its buffer. The endpoint's tests answer the
kernel's grant questions by the kernel's own coverage rule.

QEMU runs no cyclic DREQ-paced chain, so there is no integration test; metal
acceptance is `plans/PI.md` P13.

## References

- BCM2711 ARM Peripherals, chapter 4 (DMA Controller).
- `plans/SOUND.md` SND5 — the staged design this driver lands.
