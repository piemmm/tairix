# I2C — the transfer protocol, the controllers, and HID over I2C

Binding under `AGENTS.md`. How a driver talks to a device on an I2C bus: the
transfer protocol a bus driver serves for each target, the controllers that
serve it, and the HID-over-I2C transport touchpads and touchscreens use.

## Ledger

| Id | Item | Status |
|---|---|---|
| I1 | The transfer protocol (`lib/abi/src/driver/i2c.rs`, `lib/abi/src/i2c_ipc.rs`), rebuilt on shared memory: a transaction of up to 8 messages, each a read or a write of up to 4096 bytes, a repeated START between messages and a STOP at the end; the bus states its speed and whether it can issue a repeated START; every length is checked against the client's buffer | planned |
| I2 | The target channel: one endpoint per target, granted as an `I2cTarget` link (`plans/SUPPLIERS.md` SL1); the bus driver addresses only that target | planned |
| I3 | `lib/i2c` and the I2C RTC drivers (ds3231, pcf8523, pcf85063a) on the rebuilt protocol | planned |
| I4 | The BCM2835 BSC controller on the rebuilt protocol: transfers longer than its FIFO paced by its interrupts, and the repeated START of a write-then-read, issued by starting the read while the write is still active (the behaviour Linux's `i2c-bcm2835` uses) | planned |
| I5 | The DesignWare I2C controller driver (`drivers/bus/i2c/designware`): standard and fast mode, `RESTART` and `STOP` per command, interrupt-paced FIFOs, abort sources mapped to errors | planned |
| I6 | Intel LPSS: the PCI functions that carry a DesignWare controller, their private reset and clock registers, SCL timing from ACPI `SSCN`/`FMCN` or the I2C specification's figures | planned |
| I7 | I2C targets from ACPI: each `I2cSerialBusV2` an `I2cTarget` link to its controller by PCI address (`plans/ACPI.md` A4) | planned |
| I8 | HID over I2C (`plans/HID.md` H6): the HID descriptor, `RESET`, `SET_POWER`, `GET_REPORT`/`SET_REPORT`, and the input register read while the device's interrupt is asserted | planned |

## The protocol (I1)

- A client maps one shared buffer granted with its target channel; a request
  names a list of messages, each with a direction, an offset into the buffer
  and a length. The bus driver performs them in order as one transaction —
  START, then a repeated START before each further message, then STOP — and
  answers with the bytes each read message received or the error that ended
  the transaction.
- Bounds, defences rather than capacities: 8 messages and 4096 bytes per
  message (one report descriptor in a single read, as HID over I2C requires),
  the whole list within the buffer.
- A bus that cannot issue a repeated START refuses a transaction of more than
  one message rather than splitting it with a STOP: a device that needs the
  repeated START would read the STOP as a new transaction. Its capabilities
  say so before any request, so a driver that needs a repeated START refuses
  the bus at bind with its reason.
- Errors are closed: no acknowledgement from the target (address or data),
  arbitration lost, a bus fault, a timeout of the bus's own deadline.

## The controllers (I4–I6)

- A transfer waits on the controller's interrupt; a bounded wait whose budget
  expires fails the transaction and resets the controller, so a stuck target
  cannot hold the bus.
- **BCM2835.** The BSC's FIFO is 16 bytes, refilled or drained at its
  threshold interrupts. The repeated START works only for a write that fits
  the FIFO followed by a read, which is the register-read shape every target
  here uses; any other multi-message transaction is refused on this
  controller.
- **DesignWare.** Each command word carries its own `RESTART` and `STOP`, so
  every transaction shape is native. The controller's component type is
  checked before use.
- **Intel LPSS.** The function's BAR holds the DesignWare registers and the
  LPSS private registers above them; the driver releases the private reset
  and enables the clock before the controller, and binds the LPSS I2C device
  ids of each supported chip generation.

## HID over I2C (I8)

- The HID descriptor (30 bytes) is read from the register its node's
  `HidDescriptorAddress` property names (`plans/SUPPLIERS.md` SL5); its
  version must be 1.00 and its lengths within the protocol's bounds.
- The device is powered on (`SET_POWER`), reset (`RESET`, completed by a
  zero-length input report), and its report descriptor read whole; the engine
  of `plans/HID.md` H3 runs on top.
- Input reports are read from the input register while the interrupt line is
  asserted, the leading length deciding the report; a length past
  `wMaxInputLength` is refused.
- A device that faults is powered down and its held state released.

## Invariants

- A target channel reaches exactly one address.
- A transaction is never split into separate transactions to fit a
  controller.
- Every wait for a controller is interrupt-driven with a bounded budget.
