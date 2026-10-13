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
| `Describe(link)` | the clock's rate, zero while stopped, the highest rate it can run at, and whether another process holds it |
| `Run { link, hz }` | an exact clock's rate, as near `hz` as the controller can make it |
| `Floor { link, hz }` | a floored clock's rate, the lowest the controller makes at or above every floor stated |
| `Release(link)` | the caller's hold given up |

A controller states each clock's policy. An exact clock runs at the rate its
first holder set: another may join it at that rate and is refused `Busy` at
any other, so no consumer's stream is retimed under it, and only a sole holder
may retune it. A floored clock — one several consumers share, or a gate that
runs only at its parent's rate — runs at no less than the highest floor any
holder states, and is refused `OutOfRange` a floor it cannot meet rather than
run slower. Each policy refuses the other's request `NotSupported`. A holder
that ends has its holds dropped; the last holder's release or end returns the
clock unheld. A request naming a clock the controller does not serve is
refused `NotSupported`.

The endpoint and every rule above are written once, in `lib/clock`
([`tairix-clock`](../lib/clock.md)); a driver supplies only its hardware.

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

## BCM2711 DVP clock gates (`drivers/clock/bcm2711_dvp`)

Binds `brcm,brcm2711-dvp` and serves its two gates, binding ids 0 and 1: the
108 MHz clock each HDMI block's audio logic runs from, gated in the block's
`MISC_CONFIG` register by a bit set to close it. A gate runs only at its
parent's rate, read from the node's `fixed-clock` entry, so each is floored:
an HDMI driver states a floor no higher than that rate and holds its gate open
while its block is up. Opening or closing one gate leaves every other bit of
the register as it was, and a gate the firmware left open is never closed
unasked. The block's HDMI resets are not served: an HDMI block is brought up by
programming it, and Linux's reset of it through this block does nothing.

## Raspberry Pi firmware clocks (`drivers/clock/rpi_firmware`)

Binds `raspberrypi,firmware-clocks`: the clocks the `VideoCore` firmware owns,
which no register reaches, only a property exchange with the mailbox service.
The service admits this node's driver alone, and only for the clocks it
drives (`lib/vcmailbox`'s `LINKED_CLOCKS` and the ARM clock). The driver
serves two roles:

- **The ARM clock** is the machine's frequency mechanism
  ([CPU frequency](../architecture/cpu-frequency.md)): on a thread of its own
  the driver applies the kernel governor's targets.
- **The display's clocks** — the core clock (binding id 4) the display engine
  runs from, and the HDMI blocks' state-machine (13) and pixel-path (14)
  clocks — are served over `clock-v1`, each floored. A clock runs at the rate
  the firmware makes for the highest floor its holders state, never below the
  least the firmware accepts — nor, for the state-machine clock, below the
  120 MHz the HDMI blocks' registers need, which the firmware does not state —
  and returns to the rate it ran at before once its last holder leaves; one
  the firmware had not started rests at that least, or where it runs when the
  least is zero, since the interface cannot stop a clock and answers a zero
  rate as no such clock. The firmware rounds, so
  every reply carries its own answer. A clock the firmware reports no range
  for is not served, and the ARM clock is never reachable through a link.

