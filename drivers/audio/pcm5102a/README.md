# `tairix-drv-audio-pcm5102a`

Autoloaded user-space driver for the **TI PCM5102A** DAC, the part on the
simplest I²S HATs. Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "ti,pcm5102a"`. The part has no control port, so the
driver binds with nothing but the codec duty discovery gives its node from
the board's `simple-audio-card`.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_IPC_BIND_PRIVILEGED` | the codec's endpoint, under its codec `LinkDuty` |
| `CAP_LOG_EMIT` | the record of every refusal |

## What it says

It serves `codec-v1` (`lib/codec`) with what the part accepts: 8 kHz to
384 kHz, 16-, 24- or 32-bit samples, I²S or left-justified framing — the
format pin picks, and the board's link states which it strapped — and no
gain, which is how its interface's driver learns to leave the gain to the
mixer. It only ever follows the interface's clocks, in their normal sense,
making its own system clock from the bit clock, so a link asking it to drive
them or invert one is refused.

## Limitations

- **Nothing is set**: framing, filter, de-emphasis and mute are the board's
  pins. A start or a stop changes nothing on the part.

## Testing

Host tests check the facts and the refusals. Metal acceptance rides with the
I²S driver's, `plans/PI.md` P16.
