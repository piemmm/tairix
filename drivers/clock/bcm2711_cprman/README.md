# `tairix-drv-clock-bcm2711-cprman`

Autoloaded user-space driver for the **BCM2711 clock manager**, which sets
every clock on the Raspberry Pi 4's SoC. Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "brcm,bcm2711-cprman"`. It binds only through the
discovery match (`BIND_KEYS`) and names no board and no address: the register
window, the clock duty and the oscillator's rate all come from the node.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the node's register window |
| `CAP_IPC_BIND_PRIVILEGED` | the node's endpoint, under its clock `LinkDuty` |
| `CAP_LOG_EMIT` | the record of every decision |

## Which clocks it serves

The PCM clock (binding id 31) and the PWM clock (30), the audio blocks'. A
consumer quotes the clock link the kernel attests it holds and asks for a
rate; see `docs/src/drivers/clock.md`. Every other clock is refused
`NotSupported`: the firmware retunes the cores', the memory's and the buses'
clocks itself, and a board's own wiring may depend on the general-purpose
ones.

A rate is made from the oscillator or PLLD's peripheral channel, the two
sources the firmware leaves fixed, by the divisor nearest it, a fractional one
through first-order MASH. MASH may never clock a generator past 25 MHz, so a
fractional divisor is used only where its whole part keeps within that, and a
faster rate is made by a whole divisor. The driver reads the PLLs and never
reprograms one.

## Limitations

- **A shared clock runs at its first holder's rate.** Both PWM blocks run
  from one clock; the second may join it at the rate it runs at and is
  refused `Busy` at any other, and nobody retunes a clock another holds.
- **Rates outside the generators' range are clamped**: a divisor runs from 2
  to 4095, and the reply states the rate made.
- **A clock the firmware left running is described, never stopped unasked.**
  It is reprogrammed only for a consumer that runs it.
- **A generator that will not finish its period is reset mid-period**, which
  may glitch its output; one that keeps running through the reset is left as
  it is and refused, since reconfiguring a running generator can lock it up.
- **Runtime unload** stops nothing: a consumer's clock stops with the
  consumer's release or its end, which this driver sees while it lives.

## Testing

Host tests drive the generators against a register-level model that asserts
the hardware's rules: the password on every write, writes to the two served
generators only, nothing reconfigured while busy, and no enable in the write
that configures. The endpoint tests run the generators behind `lib/clock`'s
endpoint through its rig, answered by the shared fake kernel, and a seeded
walk checks that a clock runs exactly while held, at its first holder's rate.

There is no QEMU vertical for the Pi board (no firmware tree hand-off,
`plans/PI.md`), so metal acceptance is `plans/PI.md` P14.

## Events

The endpoint's, recorded by `lib/clock` (`docs/src/lib/clock.md`): ids 24300
to 24308.

## References

- BCM2835 ARM Peripherals, section 6.3 — the generators the PCM and PWM
  clocks share with the general-purpose ones.
- QEMU's BCM2835 CPRMAN model (`include/hw/misc/bcm2835_cprman_internals.h`,
  `hw/misc/bcm2835_cprman.c`; GPL-2.0-or-later) — the PLL and PLL-channel
  registers, the VPU generator and its source mux, and the PCM and PWM
  generators' offsets.
- `plans/SOUND.md` SND8 — the staged design this driver lands.
