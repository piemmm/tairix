# `tairix-drv-audio-pcm5122`

Autoloaded user-space driver for the **TI PCM5122** DAC over I²C, the part on
HATs with a digital volume. Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "ti,pcm5122"`, a child of the I²C bus it answers on.
Its only reach to the part is the transfer endpoint its node's grant names,
so it cannot address another device on the bus; its codec duty comes from the
board's `simple-audio-card`.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_IPC_ENDPOINT` | the bus's transfer endpoint, through the node's grant |
| `CAP_IPC_BIND_PRIVILEGED` | the codec's endpoint, under its codec `LinkDuty` |
| `CAP_LOG_EMIT` | the record of every refusal, and why it ends |

## What it does

At bring-up it resets the part, leaves it in standby and muted, makes both
clocks inputs, feeds its PLL from the bit clock and has it ignore the absent
system clock — the consumer sequence Linux's `pcm512x` driver uses. Serving
`codec-v1` (`lib/codec`):

- **Configure** programs the framing the board's link states (I²S, left- or
  right-justified, DSP A with its one-bit offset, DSP B), the word length, 16
  to 32 bits, and the bit clock's polarity, and lets the part's dividers
  follow the rates it detects. The part has no frame clock polarity, so a
  link inverting the frame clock is refused.
  The rates are the thirteen its automatic clocking derives from a bit clock,
  8 kHz to 384 kHz.
- **Gain** is the part's digital volume, 24 dB to −103 dB in half-decibel
  steps, rounded to the step at or above the one asked so the mixer never
  has to make up the difference, plus mute.
- **Start** leaves standby and unmutes unless the gain says mute; **stop**
  mutes, waits up to 10 ms for the soft-mute ramp to finish — parked between
  reads, never spinning — and returns to standby, so the interface's clocks
  stop under a silent part.

## Limitations

- **It follows the interface's clocks only.** Driving them would need the
  system clock a HAT's oscillators supply, which no binding here describes.

## Testing

Host tests run the driver against a model of the part's register file: the
bring-up sequence, every framing and width, the gain's rounding and range,
mute while started or not, and a stop that waits out the soft mute within its
budget. Metal acceptance rides with the I²S driver's, `plans/PI.md` P16.
