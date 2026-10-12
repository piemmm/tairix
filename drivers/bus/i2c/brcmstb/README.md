# `tairix-drv-bus-i2c-brcmstb`

Autoloaded user-space driver for the **Broadcom set-top BSC** I²C controllers
that are the BCM2711 HDMI blocks' DDC buses: the bus each HDMI port reads its
sink's EDID, its segment pointer and SCDC over. Stability tier:
`experimental`.

## Supported hardware

Device-tree `compatible = "brcm,bcm2711-hdmi-i2c"`, with its `bsc` and
`auto-i2c` register windows. The driver binds only through the discovery
match (`BIND_KEYS`) and names no board.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the controller's and the automatic I²C engine's windows |
| `CAP_IPC_BIND_PRIVILEGED` | the node's `i2c-v1` endpoint, under its I²C duty |
| `CAP_SHM` | each channel's buffer, made and granted to its client |
| `CAP_LOG_EMIT` | the record of every decision |

It binds no interrupt and holds no clock link: the tree wires the controller
neither.

## What it serves

`i2c-v1` through the shared endpoint (`lib/i2cbus`, `plans/I2C.md`). A command
moves up to 32 bytes through eight data registers and carries its own framing
— a START and the address unless it continues the message before it, a STOP
unless a further command continues the message or a repeated START follows —
so the bus issues a repeated START before any message. Every chunk after a
message's first continues it, so a message longer than one command is never
re-addressed part way (Linux's `i2c-brcmstb` re-addresses the last chunk of
any message but the last, which only a write longer than 32 bytes before a
further message exposes).

The bus runs at the fastest of the controller's eight fixed clocks — 375,
390, 187.5 and 200 kHz, each also halved — at or below the speed its node
declares: 97.5 kHz on the Pi 4. A node declaring less than the slowest is
refused at bring-up.

## Bring-up

The HDMI block's automatic I²C engine owns the controller until released, so
the driver writes the engine's release bit before touching the controller,
then idles it and selects its four-byte data registers and its clock.

## Waiting on the bus

The tree wires the controller no interrupt (Linux drives it by polling), so
each command's completion is read after sleeping the command's time on the
wire, on a one-shot timer, and again each such interval while a target
stretches the clock. A command that shows no completion within its time on
the wire and a one-second stall, the Linux I²C core's default, gives the
transaction up and idles the controller. A refusal names the message it ended
in, since each command belongs to one.

## Limitations

- **No empty message.** The controller sends an address only with a byte
  after it, so an empty write is refused before the bus is touched.
- **10-bit addressing is not supported.**
- **Not exercisable under QEMU.** No emulated machine presents the node, so
  the controller is proven against a simulated one — commands run against an
  E-DDC EEPROM behind its segment pointer, absent targets, and targets that
  stretch or hold the clock, each command showing done only after its time on
  the wire — and the live bus is the on-metal acceptance item
  (`plans/PI.md`).

## Events

The endpoint's records are `lib/i2cbus`'s (`docs/src/lib/i2cbus.md`).

## References

- Linux `drivers/i2c/busses/i2c-brcmstb.c`.
- VESA Enhanced Display Data Channel (E-DDC) Standard.
- `plans/I2C.md` I9, `plans/SOUND.md` SND19i.
