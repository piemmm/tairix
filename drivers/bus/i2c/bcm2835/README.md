# `tairix-drv-bus-i2c-bcm2835`

Autoloaded user-space I²C controller driver for the **Broadcom Serial
Controller** (BSC), the I²C master every Raspberry Pi generation exposes.
Stability tier: `experimental`.

## Supported hardware

Device-tree `compatible = "brcm,bcm2835-i2c"`. Later Pi parts (BCM2711,
BCM2712) present a register-compatible controller and carry the same string,
so one driver covers the family. The driver binds only through the discovery
match (`BIND_KEYS`); it names no board and no address.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the controller's register window, from the matched node's grant |
| `CAP_IRQ_BIND` | the controller's interrupt line, which every transaction parks on |
| `CAP_IPC_BIND_PRIVILEGED` | the node's `i2c-v1` endpoint, under its I²C duty |
| `CAP_SHM` | each channel's buffer, made and granted to its client |
| `CAP_IPC_ENDPOINT` | the VPU clock, described over the node's clock link |
| `CAP_LOG_EMIT` | the record of every decision |

It holds no capability that would let it reach a device other than the one it
was matched to.

## What it serves

`i2c-v1` through the shared endpoint (`lib/i2cbus`, `plans/I2C.md`): a channel
per link, a buffer per channel, every message held to the address its link
reaches. The bus states its speed — the node's `clock-frequency`, 100 kHz when
it declares none — and that it issues a repeated START after a write that
moved bytes: between writes, and before a read that ends the transaction.

## Repeated START by chaining

The BSC has no repeated-START command. A message started while the one before
it is active follows it with a repeated START instead of a STOP, which is how
Linux's `i2c-bcm2835` issues one. The chain must be in place before the
message before it can end, so the driver withholds each write's last byte
until the next message is started: the controller cannot finish a write it
has not been given, however late this process runs. A read must end the
transaction, so any other shape is refused before the bus is touched.

## Interrupt-paced, bounded by progress

The FIFO is refilled at its write threshold and drained at its read threshold
and at completion, the transaction parking on the interrupt in between. Its
budget is per step of progress: a FIFO's worth of bytes at the bus's speed and
the longest a target may hold the bus still, one second, the Linux I²C core's
default. While the controller waits on this driver it holds the clock, so a
late wake finds work rather than a timeout; only a bus that makes no progress
for a whole step is abandoned, its controller reset, and the transaction
reported timed out. The controller's own clock-stretch timeout is off, as
Linux leaves it, so every stall is reported against the message it stalled.

## Bus speed

The divider is set from the VPU clock before every transaction, the rate
described through the node's clock link: the firmware may move that clock, and
the bus must not follow it past its declared speed. It rounds up, then up to
even as the register reads it, so the bus never runs faster than declared; a
speed the divider cannot reach from the clock's rate refuses the transaction
before the bus is touched.

## What a fault names

A fault names the earliest message the controller has not been seen to finish.
The controller gives no sign of moving to the next message until that message
asks for its bytes or delivers one, so a read's address refused straight after
a write is reported against the write.

## Limitations

- **A read must end the transaction.** The chaining that issues a repeated
  START cannot follow a read.
- **10-bit addressing is not supported.** No part this class reaches uses it,
  and the address type refuses the escape prefix rather than mis-addressing
  the bus.
- **Not exercisable under QEMU.** No emulated machine presents a
  `brcm,bcm2835-i2c` node, so the controller is proven against a simulated BSC
  — the FIFO a real queue, each status bit as the silicon sets it, messages
  chained as the silicon chains them, and an eager mode that moves every byte
  the instant it can, which is what a driver preempted at the worst moment
  faces — and the live bus is the on-metal acceptance item (`plans/PI.md`).

## Events

The endpoint's records are `lib/i2cbus`'s (`docs/src/lib/i2cbus.md`).

## References

- BCM2835 ARM Peripherals, §3 (Broadcom Serial Controller).
- Linux `drivers/i2c/busses/i2c-bcm2835.c`.
- I²C-bus specification and user manual, NXP UM10204.
- `plans/I2C.md` I1, I4.
