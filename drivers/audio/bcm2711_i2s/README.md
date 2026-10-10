# `tairix-drv-audio-bcm2711-i2s`

Autoloaded user-space driver for the **BCM2711's PCM/I²S block**: a digital
audio interface whose transmit FIFO a cyclic DMA channel keeps fed, composed
with the codec its sound card links it to, serving `audiochan-v1` to the
mixer. Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "brcm,bcm2835-i2s"`, which a HAT's overlay enables
beside a `simple-audio-card` naming the codec. The node supplies the register
window, the `tx` DMA request line, the PCM clock and the codec link; the
driver names no board and no address. Any codec whose driver serves
`codec-v1` composes with it — the image ships `ti,pcm5102a` and `ti,pcm5122`.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the block's registers |
| `CAP_IPC_ENDPOINT` | the DMA controller's, the clock manager's and the codec's endpoints, through the node's links |
| `CAP_IPC_BIND_PRIVILEGED` | the device-channel endpoint the mixer calls |
| `CAP_HW_EMIT` | the `audiochan` node that hands the endpoint to the mixer |
| `CAP_SHM` | the DMA buffer and the mixer's PCM ring, each mapped from its grant |
| `CAP_LOG_EMIT` | the reason it ends, when it does |

## How it streams

The endpoint offers the widest sample the codec takes whose FIFO word is laid
out as a ring of that format holds it: a 32-bit sample a word, a 24-bit one
right-justified in a word (`S24In32`), or two 16-bit samples to a word, the
first channel's in the low half. A frame is therefore copied as it is, and
any narrowing is the mixer's, dithered. The rates are the codec's within the
block's 8 kHz to 384 kHz, and the gain is the codec's.

Each sample has a slot as wide as itself, two slots a frame, in the link's
framing — I²S, left- or right-justified, DSP A or B — and either clock's
inversion, as Linux's `bcm2835-i2s` programs the block for the same link.
Where this side drives the bit clock, the clock manager runs the PCM clock at
the rate times the frame's bits; a clock it makes more than 100 ppm off is
refused, and the mixer's clock fit follows what remains. Where the codec
drives the clocks, the block follows them.

The block takes FIFO words for its two channels in turn, so a word left over
would swap them. Every stream therefore starts from a FIFO cleared with
transmit off (BCM2711 ARM Peripherals, 7.5.3): the clear completes once two
bit clocks have passed, the DMA channel fills the FIFO, transmit goes on, and
the codec comes up. A codec that drives the bit clock comes up first, since
the clear needs a running one. A stop mutes the codec first, then parks the
buffer; transmit goes off and the codec down only once the channel has
halted, so a drain's last frames are heard.

## Limitations

- **It plays.** `codec-v1` describes converters that play, so the block's
  receive side is unused.
- **One codec.** A card linking several codecs to the interface would need
  time slots its link does not describe; the driver refuses it rather than
  guess.

## Testing

Host tests check every framing against the register values Linux computes
for the same link, the FIFO clear's two-bit-clock handshake and its failure
with no bit clock, and drive the composed interface over a model of the
block, the modelled DMA channel, and a clock controller and a codec that
decode every frame they are sent: the offer, the clock's rate and tolerance,
the start and stop orders in both clock directions, a drain playing out, the
gain, a busy reconfiguration, and release.

There is no QEMU vertical for the Pi board (no firmware tree hand-off,
`plans/PI.md`), so metal acceptance is `plans/PI.md` P16.

## References

- BCM2711 ARM Peripherals, chapter 7 — the PCM/I²S block.
- Linux `sound/soc/bcm/bcm2835-i2s.c` — the framing this driver matches.
- `plans/SOUND.md` SND8g — the staged design this driver lands.
