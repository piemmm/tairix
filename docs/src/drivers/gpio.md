# GPIO-controller drivers

A GPIO controller's lines reach two kinds of holder. A device the hardware
tree links to a line waits on it as an interrupt over `irqline-v1` (see
[Interrupt-line drivers](./irq.md)). A system service holding `CAP_GPIO`
drives one of the lines the board leads to its expansion header over
`gpio-header-v1` (`tairix_abi::driver::gpio`, `plans/COOLING.md` CL2). A line
has one holder at a time, of either kind.

## The header

- **Which lines are lent is the board's fact.** The architecture port states
  it as the controller node's `GpioHeaderLines` property, a bit per line. On a
  Raspberry Pi with the SoC's GPIO block that is GPIO 2–27; GPIO 0 and 1 carry
  the HAT identification EEPROM, which the firmware reads. A controller with
  no such fact lends nothing and binds no header endpoint.
- **The endpoint.** The driver binds its node's `GPIO_HEADER_ENDPOINTS` id
  restricted-sender, requiring `CAP_GPIO`, then publishes a
  `tairix,gpio-header` child node carrying it once it serves, so a service
  finds the header in the tree only when it can be called.
- **Requests.** `Lines` lists each lent line as free, routed to a peripheral
  (an alternate function in its function select), held as an interrupt, or
  driven as an output by the caller or another holder. `Claim` takes a free
  line as an output with a polarity and an initial state, the level set before
  the direction so the line never glitches. `Drive` switches the caller's line
  and `Release` returns it to an input. A holder that ends has every line it
  held returned to an input.
- **What is never lent.** A line routed to a peripheral — the serial console's
  pins, an I²C or I²S bus the firmware set up — is refused `Busy`, and a line
  outside the stated header `NotFound`.

## `drivers/gpio/bcm2835`

The BCM2711 block (fifty-eight lines) and the BCM2835 block (fifty-four),
`brcm,bcm2711-gpio` and `brcm,bcm2835-gpio`. One driver owns the block's
function selects, output set and clear, event-detect enables and status, and
the node's first three interrupts: GPIO 0–27, 28–45, and 46 up, the groups
Linux's `pinctrl-bcm2835` names. It quiesces every detection at bring-up.

- An interrupt consumer's selector is the binding's line and trigger (rising,
  falling, both, high, low). A level line's detection is enabled only while
  its consumer waits and is disabled as it fires; an edge line's stays enabled
  while held, and an edge with no wait posted answers the next wait at once.
- A line on a group whose interrupt the node does not carry is refused
  `NotSupported`.
- Pulls are the board's wiring, set by the firmware's `gpio=` lines, and are
  never touched.

QEMU does not model the BCM2711; host tests drive the controller over a
register model of the block.
