# `tairix-i2c`

**Stability tier: experimental.** The public surface is `Device` and its
five register operations, the `MAX_BLOCK_LEN` bound, and the `mock` part
double behind the `mock-bus` feature.

I²C register-transaction **protocol** (`lib/*`, not a driver crate): the
write-then-read composition every register-addressed part needs, over the
`I2cPort` seam (`tairix_abi::driver::i2c`, re-exported here so a chip driver
names the whole vocabulary through one crate). Each chip driver contributes
only its own register map and quirks — the split `lib/usb` and `lib/virtio`
already make between a bus protocol and the devices on it.

A register read is **one** transaction: the pointer write and the read-back
are joined by a repeated START, so no other transfer can be interleaved
between them and return some other register's contents — a wrong clock rather
than an error. `Device` never splits them, and a host test asserts the shape
rather than merely the result.

## What a chip driver holds

A `Device` is one `I2cPort`, and a port carries **no address**. On a real bus
it is a target channel (`tairix_linkclient::I2cTarget`) opened over the link
discovery gave the chip's node, and the controller holds every message of it
to the one address that link names — so a chip driver has no field in which
it could name a neighbour, however it is compromised (`plans/I2C.md`).

## Testing

`mock::MockPart` is the shared register-file double: a seedable register
file, the chip's pointer auto-increment, a programmable fault, a transfer
counter, and the bounds a real target channel holds a transfer to. It is the single definition every chip driver's host tests use, so
no driver carries a private copy of the scaffold.

```toml
[dev-dependencies]
tairix-i2c = { path = "../../../lib/i2c", features = ["mock-bus"] }
```

## References

- I²C-bus specification and user manual, NXP UM10204.
