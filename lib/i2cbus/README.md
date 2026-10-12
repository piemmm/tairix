# tairix-i2cbus

The `i2c-v1` controller side (`plans/I2C.md` I1, I2). Stability tier:
`experimental`.

- `Controller` is the endpoint an I²C controller's driver serves under its
  node's duty, written over the `I2cBus` trait and nothing of any one
  controller: a channel opened over a link the kernel attests the caller
  holds, a buffer made for each channel and granted to it, every message held
  to the address its link reaches and every transaction to the repeated
  STARTs the bus can issue.
- `serve` is the freestanding loop a driver hands its bus to, each channel's
  buffer a shared region granted to its client.

Every I²C controller driver links it, so the protocol's rules are written
once: `drivers/bus/i2c/bcm2835`.
