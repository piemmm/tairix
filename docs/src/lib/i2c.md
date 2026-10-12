# `tairix-i2c`

`lib/i2c` is the I²C **register-transaction protocol**: the write-then-read
composition every register-addressed part needs, over the `I2cPort` seam. It
names no device, board, or chip, so it lives in `lib/*` as shared code. Each
chip driver (`drivers/rtc/ds3231`, `drivers/rtc/pcf8523`,
`drivers/rtc/pcf85063a`, `drivers/audio/pcm5122`) composes it, exactly as the
bus-agnostic xHCI protocol lives in [`tairix-usb`](./usb.md) rather than the
xHCI driver.

## What it provides

- **`Device`** — one register-addressed part, bound to one `I2cPort`, with
  `read`, `read_one`, `write`, `write_one`, and `update_one` (the
  read-modify-write every status-flag clear needs, which writes only when the
  value actually changed so a control register the chip owns bits in is never
  clobbered from a driver's stale idea of it).
- **`MAX_BLOCK_LEN`** — the longest register block one write carries, 32
  bytes, an `SMBus` block: it bounds the frame the write builds with its
  pointer byte. A read is bounded by the port.
- **`mock::MockPart`** (behind the `mock-bus` feature) — the shared
  register-file part double: a seedable register file, the chip's pointer
  auto-increment, a programmable fault, a transfer counter, and the bounds a
  real target channel holds a transfer to. Every chip driver's host tests
  drive this one definition.

It re-exports `I2cPort` from `lib/abi`, so a chip driver names the whole
vocabulary through one crate.

## One transaction, not two

A register read is a *write-then-read*: the pointer write and the read-back
are one transaction joined by a repeated START, so no other transfer can be
interleaved between them and return some other register's contents — a wrong
clock rather than an error. `Device` never splits them, and a host test
asserts the transaction's shape rather than merely its result.

## What a chip driver holds

A `Device` is one `I2cPort`, and **a port carries no address**. On a real bus
it is an `I2cTarget` ([`tairix-linkclient`](./linkclient.md)), a channel
opened over the target link discovery gave the chip's node; the controller
holds every message of it to the one address that link names, so a chip
driver cannot reach a neighbour however it is compromised. The protocol is
[`i2c-v1`](./i2cbus.md), and `plans/I2C.md` carries its design.

## Stability

**Experimental.** The public surface is `Device` and its five register
operations, `MAX_BLOCK_LEN`, and the `mock` module.

## References

- I²C-bus specification and user manual, NXP UM10204.
