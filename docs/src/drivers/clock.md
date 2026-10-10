# Clock-controller drivers

A clock controller makes the clocks other devices run on. Its drivers live
under `drivers/clock/<leaf>/` and serve the `clock-v1` seam
(`tairix_abi::driver::clock`). The staged design is `plans/SOUND.md` SND8 and
the link it rests on `plans/SUPPLIERS.md` SL1; this page is the class view of
what exists.

## Who may set a clock

A controller's registers set every clock on the chip, the cores' and the
memory's among them, so only its driver maps them. A consumer never touches
them: it calls the controller's endpoint quoting the clock `LinkRequest`
discovery gave its node, and the controller believes the request only once the
kernel attests the caller holds it (`call_peer_holds`). The endpoint is one per
controller node, from the reserved `CLOCK_CONTROLLER_ENDPOINTS` block indexed
by node id, and only the holder of the node's clock `LinkDuty` may bind it.

## Discovery

Every FDT port reads the generic clock binding through the shared walk
([hardware detection](hardware-detection.md)): a node with `#clock-cells`
carries the clock `LinkDuty`, and each entry of a consumer's `clocks` becomes
a clock `LinkRequest` whose selector is the entry's specifier. An entry naming
a `fixed-clock` becomes a `FixedClockRate` fact on the consumer instead, since
no driver serves one. A consumer's driver loads only once its controller
serves.

## `clock-v1`

| Operation | Answer |
|---|---|
| `Describe(link)` | the clock's rate, zero while stopped, and whether another process holds it |
| `Run { link, hz }` | the rate the clock now runs at, as near `hz` as the controller can make it |
| `Release(link)` | the caller's hold given up; the last holder's release stops the clock |

A clock is held by every process that runs it and runs at the rate the first
set. Another may join it at that rate and is refused `Busy` at any other, so
no consumer's stream is retimed under it; once it is held by one process
alone, that process may retune it. A holder that ends has its holds dropped,
and the clock stops with its last. A request naming a clock the controller
does not serve is refused `NotSupported`.

## BCM2711 clock manager (`drivers/clock/bcm2711_cprman`)

Binds `brcm,bcm2711-cprman` and serves the PCM clock (binding id 31) and the
PWM clock (30). The rest are the firmware's: it retunes the cores', the
memory's and the buses' clocks itself, and a board's own wiring may depend on
the general-purpose ones.

- **Sources.** A rate is made from the oscillator, at the rate its `clocks`
  entry states, or from PLLD's peripheral channel, read from the PLL's
  registers: the two sources the firmware leaves fixed. PLLC follows the core
  clock and PLLA feeds the display. The driver never reprograms a PLL.
- **Divisors.** A generator divides by a 12.12 fixed-point divisor from 2 to
  4095; one with a fractional part runs through first-order MASH, so every
  period is within one source period of the exact division. MASH may never
  clock the generator past 25 MHz (BCM2711 ARM Peripherals, 5.4), and its
  shortest period is its whole part's, so a fractional divisor is a candidate
  only where the source over that whole part keeps within it; a whole divisor
  always is. The divisor chosen is the nearest candidate in average rate; at
  an equal distance a whole divisor wins, having no MASH jitter, then the
  larger, whose jitter is the smaller part of a period.
- **Changes.** A generator is stopped before its source or divisor changes
  and enabled in a write of its own, as the part requires. One that does not
  finish its period is reset; one that keeps running through the reset is left
  alone and the request refused.
- **What it leaves alone.** A clock the firmware left running is described as
  it is and never stopped unasked.
