# GPIO — controllers, and the interrupts their lines carry

Binding under `AGENTS.md`. The GPIO controller drivers and the one service
they give the drivers whose device signals on a GPIO line: wait until the line
fires. Lines reach their consumers as `InterruptLine` links
(`plans/SUPPLIERS.md`).

## Ledger

| Id | Item | Status |
|---|---|---|
| G1 | The line-interrupt service (`lib/abi/src/driver/irqline.rs`, `irqline-v1`): a consumer's link endpoint answers `Wait` when its line fires, with the trigger and polarity its selector states; the line stays masked from the moment it fires until the consumer waits again, so a level interrupt is serviced before it re-arms; first served by the BCM L2 controller (`plans/SOUND.md` SND19e) | done |
| G2 | The BCM2835-family GPIO controller driver (`drivers/gpio/bcm2835`: `brcm,bcm2711-gpio`): its bank interrupts, per-line edge and level detection, the service, and the header's lines lent over `gpio-header-v1` (`plans/COOLING.md` CL2) | done |
| G3 | The Intel PCH GPIO controller driver (`drivers/gpio/intel_pch`, Sunrise Point and later): communities, pad groups, ACPI pin numbering, GPI interrupts through the controller's shared interrupt; published by the ACPI bus driver (`plans/ACPI.md` A4) | planned |

## The service (G1)

- `Wait` parks the consumer until its line fires and answers the instant it
  did; the line is masked in the controller as it fires, and unmasked by the
  next `Wait`. An edge that arrives while masked is latched and answers the
  next `Wait` at once, so no edge is lost and a level line cannot storm.
- The selector is the controller's own specifier: the line, and for a
  controller whose binding names one the trigger (rising, falling, both, high,
  low), and nothing else. A line another live consumer holds, one the
  controller does not have, or a trigger it cannot detect is refused at the
  first `Wait`, logged; a line an ended consumer held is taken by its link's
  next holder.
- The consumer's driver waits on its link through `tairix_linkclient`'s
  `IrqLineClient` — a blocking wait, or a posted one its serve loop wakes on —
  beside a direct `Irq`, so a driver serves a device whichever way its
  interrupt is wired.
- A consumer that ends has its line masked and released; a controller that
  ends answers every outstanding `Wait` with `NotFound`, which the consumer
  reads as its device gone.

## The BCM2835 family (G2)

- One driver owns the GPIO block's registers: the function selects, the
  output set and clear, the event-detect enables, the event-status register
  it clears, and its node's first three interrupts — GPIO 0–27, 28–45, and 46
  up. The kernel's boot-time mux of the console UART's pins is the only other
  write to the block and happens before any driver loads.
- A line used as an interrupt is set to input; a header line a holder claims
  is driven as an output (`plans/COOLING.md` CL2). A line routed to an
  alternate function is never taken for either. Pulls are the board's wiring,
  stated by the firmware's `gpio=` lines, and are never touched.
- A level line's detection is enabled only while its consumer waits and is
  disabled as it fires. An edge line's is enabled by every wait and disabled
  when an edge fires with no wait posted, the edge latched in the driver to
  answer the next wait at once: a line that keeps firing wakes the driver at
  most once between waits.

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
