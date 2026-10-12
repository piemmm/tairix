# `tairix-drv-clock-bcm2711-dvp`

Autoloaded user-space driver for the **BCM2711 DVP clock gates**, which gate
the 108 MHz clock each of the Raspberry Pi 4's HDMI blocks runs its audio
logic from. Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "brcm,brcm2711-dvp"`. It binds only through the
discovery match (`BIND_KEYS`) and names no board and no address: the register
window, the clock duty and the gated clock's rate all come from the node.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the node's register window |
| `CAP_IPC_BIND_PRIVILEGED` | the node's endpoint, under its clock `LinkDuty` |
| `CAP_LOG_EMIT` | the record of every decision |

## Which clocks it serves

Gate 0, HDMI0's, and gate 1, HDMI1's, each named by the one cell of a
consumer's `clocks` entry. A gate runs only at its parent's rate, so each is a
floored clock: a consumer states the least rate it needs, no higher than the
parent's, and the gate stays open while any holder holds it. See
`docs/src/drivers/clock.md`.

## Limitations

- **A floor above the parent's rate is refused**, never met with less.
- **The HDMI resets the block also holds are not served.** An HDMI block is
  brought up by programming it; Linux's reset of it through this block does
  nothing.
- **A gate the firmware left open is described, never closed unasked.**
- **Runtime unload** closes nothing: a gate closes with its last holder's
  release or end, which this driver sees while it lives.

## Testing

Host tests drive the gates against a register-level model that asserts the
hardware's rules — only the gate register is written, and only its two gate
bits change — alone and behind `lib/clock`'s endpoint through its rig.

There is no QEMU vertical for the Pi board, so metal acceptance is
`plans/PI.md` P14.

## Events

The endpoint's, recorded by `lib/clock` (`docs/src/lib/clock.md`): ids 24300
to 24308.

## References

- Linux `drivers/clk/bcm/clk-bcm2711-dvp.c` — the gate register and bits.
- `plans/SOUND.md` SND19f — the staged design this driver lands.
