# I2C — the transfer protocol, the controllers, and HID over I2C

Binding under `AGENTS.md`. How a driver talks to a device on an I2C bus: the
transfer protocol a bus driver serves for each target, the controllers that
serve it, and the HID-over-I2C transport touchpads and touchscreens use.

## Ledger

| Id | Item | Status |
|---|---|---|
| I1 | `i2c-v1` (`lib/abi/src/driver/i2c.rs`), on shared memory: a transaction of up to 8 messages, each a read or a write of up to 4096 bytes, a repeated START between messages and a STOP at the end; the bus states its speed and which repeated STARTs it can issue; every length is checked against the channel's buffer; the controller side in `lib/i2cbus`, the client in `lib/linkclient` | done |
| I2 | The `I2c` link role (`plans/SUPPLIERS.md` SL1): one endpoint per controller node; a child's link names its address and reaches that target alone, a `ddc` link names none and reaches its dedicated bus | done |
| I3 | `lib/i2c` and its consumers — the I2C RTC drivers (ds3231, pcf8523, pcf85063a) and the PCM5122 codec — on the rebuilt protocol | done |
| I4 | The BCM2835 BSC controller on the rebuilt protocol: transfers longer than its FIFO paced by its interrupts, the repeated START before each further message issued by starting it while the write before it is still active (the behaviour Linux's `i2c-bcm2835` uses), and the bus run at its declared speed | done |
| I5 | The DesignWare I2C controller driver (`drivers/bus/i2c/designware`): standard and fast mode, `RESTART` and `STOP` per command, interrupt-paced FIFOs, abort sources mapped to errors | planned |
| I6 | Intel LPSS: the PCI functions that carry a DesignWare controller, their private reset and clock registers, SCL timing from ACPI `SSCN`/`FMCN` or the I2C specification's figures | planned |
| I7 | I2C targets from ACPI: each `I2cSerialBusV2` an `I2cTarget` link to its controller by PCI address (`plans/ACPI.md` A4) | planned |
| I8 | HID over I2C (`plans/HID.md` H6): the HID descriptor, `RESET`, `SET_POWER`, `GET_REPORT`/`SET_REPORT`, and the input register read while the device's interrupt is asserted | planned |
| I9 | The Broadcom set-top BSC (`drivers/bus/i2c/brcmstb`) for the BCM2711 HDMI blocks' DDC buses: every transaction shape native, each command's completion read after sleeping its time on the wire, the bus run at its fastest fixed clock at or below the declared speed | done |

## The protocol (I1)

- A client maps one shared buffer granted with its channel; a request names
  a list of messages, each with an address, a direction, an offset into the
  buffer and a length. The bus driver performs them in order as one
  transaction — START, then a repeated START before each further message,
  then STOP — leaves each read's bytes in its span of the buffer, and answers
  with how the transaction ended.
- Bounds, defences rather than capacities: 8 messages and 4096 bytes per
  message (one report descriptor in a single read, as HID over I2C requires),
  the whole list within the buffer.
- A bus that cannot issue a repeated START refuses a transaction of more than
  one message rather than splitting it with a STOP: a device that needs the
  repeated START would read the STOP as a new transaction. Its capabilities
  say so before any request, so a driver that needs a repeated START refuses
  the bus at bind with its reason.
- Errors are closed: no acknowledgement from the target (address or data),
  arbitration lost, a bus fault, a timeout of the bus's own deadline. A fault
  names the message it ended in; a refusal before the bus was touched is the
  request's own error.
- **Channels.** A controller serves one endpoint per node
  (`I2C_CONTROLLER_ENDPOINTS`, under the node's `I2c` duty). A client opens
  the channel its link names and is answered with the bus's facts and the
  grant of a buffer the controller made for that channel —
  `I2C_BUFFER_LEN`, two pages, a longest message and the writes before it.
  Every request quotes the link, which the kernel attests the caller holds.
  A target link (`[address]`) reaches that address alone: every message must
  name it. A bus link (`[]`) reaches every address of a bus wired to one
  consumer, as an HDMI port's DDC bus is (EDID, its segment pointer, SCDC);
  discovery gives one only for a bus with no target of its own, so it never
  reaches another driver's part. Closing the channel, or the holder's end,
  drops it and its buffer. A channel is one per holder and link.
- **The bus's facts.** Its speed, the one its node declares
  (`clock-frequency`, 100 kHz when it declares none) and the controller runs
  it at no more than, and which repeated STARTs it can issue: none; after a
  write that moved bytes, before a further write or a read that ends the
  transaction; or any.
- **The port.** `I2cPort` is one target's write-then-read, shaped once
  (`I2cTransaction::write_then_read`) for every implementor; a target channel
  (`lib/linkclient`'s `I2cTarget`) refuses at open a bus that cannot follow a
  register write with its read, so a chip driver refuses the bus at bind. A
  target that does not answer its first message is not found; one that
  answered and then refused has faulted.

## The controllers (I4–I6, I9)

- A transfer waits on the controller's interrupt; a bounded wait whose budget
  expires fails the transaction and resets the controller, so a stuck target
  cannot hold the bus. The budget is per step of progress, so a driver woken
  late finds work rather than a timeout.
- **BCM2835.** The BSC's FIFO is 16 bytes, refilled or drained at its
  threshold interrupts. A repeated START is issued by starting the next
  message while the write before it is active, and before that write's last
  byte is handed over: Linux starts it once the FIFO holds that byte, which
  only an interrupt handler can do before the controller ends the write,
  while a process may be preempted anywhere and the controller cannot finish
  a write it has not been given. Writes of any length may precede it; a read
  must end the transaction, so any other shape is refused on this
  controller. The step budget is a FIFO's worth of bytes at the bus's speed
  and one second's stall, the Linux I²C core's default; the controller's own
  stretch timeout is off, as Linux leaves it, so every stall is reported
  against the message it stalled. A fault names the earliest message the
  controller has not been seen to finish. Its divider is set from its parent
  clock — the VPU clock, described through its clock link, which the firmware
  may move — before each transaction, rounded so the bus never runs faster
  than declared.
- **Broadcom set-top (DDC).** A command moves up to 32 bytes and carries its
  own conditions: a START and the address unless it continues the message, a
  STOP unless the message continues or a repeated START follows. Every chunk
  after a message's first continues it. The BCM2711's DDC controllers have no
  interrupt in the tree, so a command's completion is read after sleeping its
  time on the wire, one-shot, never spun; an empty message is refused, since
  the controller sends an address only with a byte. Bring-up releases the
  controller from the HDMI block's automatic I²C engine.
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
