# GPIO — controllers, and the interrupts their lines carry

Binding under `AGENTS.md`. The GPIO controller drivers and the one service
they give the drivers whose device signals on a GPIO line: wait until the line
fires. Lines reach their consumers as `InterruptLine` links
(`plans/SUPPLIERS.md`).

## Ledger

| Id | Item | Status |
|---|---|---|
| G1 | The line-interrupt service (`lib/abi/src/driver/gpio.rs`, `gpioirq-v1`): a consumer's link endpoint answers `Wait` when its line fires, with the trigger and polarity its selector states; the line stays masked from the moment it fires until the consumer waits again, so a level interrupt is serviced before it re-arms | planned |
| G2 | The BCM2835-family GPIO controller driver (`drivers/gpio/bcm2835`: `brcm,bcm2711-gpio`, `brcm,bcm2835-gpio`): its bank interrupts, per-line edge and level detection, and the service | planned |
| G3 | The Intel PCH GPIO controller driver (`drivers/gpio/intel_pch`, Sunrise Point and later): communities, pad groups, ACPI pin numbering, GPI interrupts through the controller's shared interrupt; published by the ACPI bus driver (`plans/ACPI.md` A4) | planned |

## The service (G1)

- `Wait` parks the consumer until its line fires and answers the instant it
  did; the line is masked in the controller as it fires, and unmasked by the
  next `Wait`. An edge that arrives while masked is latched and answers the
  next `Wait` at once, so no edge is lost and a level line cannot storm.
- The selector names the line, the trigger (rising, falling, both, high, low)
  and nothing else. A line another consumer holds, one the controller does not
  have, or a trigger it cannot detect is refused at the first `Wait`, logged.
- The consumer's driver waits on its link through the driver runtime's one
  interrupt seam, beside a direct `Irq`, so a driver serves a device whichever
  way its interrupt is wired.
- A consumer that ends has its line masked and released; a controller that
  ends answers every outstanding `Wait` with `NotFound`, which the consumer
  reads as its device gone.

## The BCM2835 family (G2)

- One driver owns the GPIO block's registers: the event-detect enables, the
  event-status register it clears, and the per-bank interrupt lines of its
  node. The kernel's boot-time mux of the console UART's pins is the only other
  write to the block and happens before any driver loads.
- Function selection and pulls are not part of the service: a line used as an
  interrupt is set to input with the pull its node states, and nothing else on
  the block is touched.

## The Intel PCH (G3)

- The pin numbers ACPI names are the driver's own map onto communities, groups
  and pads, a table per chip generation in the driver; a pin outside the table
  is refused.
- A pad the firmware has not handed to the operating system (pad ownership,
  host-software ownership) is refused rather than taken.
- Interrupts arrive on the controller's one shared line (an `Irq` from its
  `_CRS`); the driver reads the GPI status of each enabled pad and answers its
  consumer.

## Invariants

- A GPIO line has at most one consumer, and only the controller's own driver
  touches its registers.
- A level interrupt re-arms only when its consumer asks, never on a timer.
