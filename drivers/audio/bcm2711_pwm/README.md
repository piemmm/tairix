# `tairix-drv-audio-bcm2711-pwm`

Autoloaded user-space driver for the **Raspberry Pi 4's 3.5 mm jack**: the
BCM2711 PWM block whose two channels the board wires to the jack, fed by a
cyclic DMA channel and serving `audiochan-v1` to the mixer. Stability tier:
`experimental`.

## Supported hardware

Device-tree `compatible = "tairix,bcm2711-pwm-audio"`, which the image's
overlay gives the jack's PWM block ahead of `brcm,bcm2835-pwm`: a PWM block is
a general part, and only the board knows which one drives its jack. The node
supplies the register window, the DMA request line (`dmas`) and the PWM clock
(`clocks`); the driver names no board and no address.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the PWM block's registers |
| `CAP_IPC_ENDPOINT` | the DMA controller's and the clock manager's endpoints, through the node's links |
| `CAP_IPC_BIND_PRIVILEGED` | the device-channel endpoint the mixer calls |
| `CAP_HW_EMIT` | the `audiochan` node that hands the endpoint to the mixer |
| `CAP_SHM` | the DMA buffer and the mixer's PCM ring, each mapped from its grant |
| `CAP_LOG_EMIT` | the reason it ends, when it does |

## How it sounds

The jack runs at **375 kHz**, each PWM period 250 cycles of a 93.75 MHz clock
the clock manager makes from PLLD by a whole divisor, and so without MASH
jitter. A period's duty therefore carries about eight bits. Each sample is
shaped onto those levels by third-order error feedback, `(1 - z⁻¹)³`, with one
level of triangular dither inside the loop. The quantisation noise falls away
below 20 kHz and rises towards 187.5 kHz, where nothing is heard and the
board's filter takes it.

Measured by the crate's own tests, as a windowed spectrum of the duty words
against the exact duty asked for:

| | 20 Hz – 20 kHz noise |
|---|---|
| a −6 dBFS tone | −90.8 dBFS |
| a −60 dBFS tone | −91.0 dBFS |
| rounding onto the same levels, unshaped | −60.8 dBFS |

That is near fifteen bits in the audible band, against the eight a period
holds, and the floor does not move with the signal. Dither costs about five
dB of it; it stays because it keeps the error uncorrelated with the music
whatever the music is. These are the figures of the digital stream. What the
jack plays also passes the PWM pad's edges and the board's analogue stage,
which bound the result first and are measured on metal (`plans/PI.md`).

The mixer resamples to 375 kHz with its one resampler, so the driver has no
second one; it only shapes.

## Silence, start and stop

The PWM block repeats its last word when its FIFO runs dry, so between
streams the jack is **parked**: the DMA buffer is filled with silence and the
channel stopped at its next boundary, once silence has reached the FIFO, and
the jack holds silence with nothing running. At bring-up the jack is ramped
from the PWM's idle low to silence over about 44 ms, so the first sound starts
from where it stays. A stream starts and stops against a jack already at
silence.

## Limitations

- **No jack detection**: the board wires none, so the endpoint reports its
  jack as unknown.
- **No hardware gain**: the PWM has none, so the mixer applies the gain.
- **One rate.** The jack runs at the rate its clock gives; the mixer converts.
- **A period the mixer has not supplied when it falls due plays as silence,
  counted lost**, because a cyclic buffer left alone would replay a lap-old
  period.
- **Which channel is which side** follows the board's wiring as the Pi
  documentation states it (the first PWM channel, GPIO 40, is the right side);
  metal acceptance checks it.

## Testing

Host tests measure the shaper as above, check that full-scale input never
reaches a rail and that a quiet tone leaves no harmonic above the noise, and
drive the jack over the modelled DMA channel: its facts, the bring-up ramp,
the buffer parked at silence, and shaped frames in the FIFO's order. The
stream itself — staging, each boundary's refill, a dry ring's counted
silence, a drain, parking and a faulted channel — is the shared cyclic
engine's, tested in `tairix-audiochan`.

There is no QEMU vertical for the Pi board (no firmware tree hand-off,
`plans/PI.md`), so metal acceptance is `plans/PI.md` P15.

## References

- BCM2835 ARM Peripherals, chapter 9 — the PWM block.
- `plans/SOUND.md` SND8e — the staged design this driver lands.
