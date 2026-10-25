# Interrupt-line drivers

A device whose interrupt reaches the root controller (the GIC, the PLIC) gets
it as an `Irq` grant it binds and waits on. A device wired behind a
second-level controller — an L2 aggregator, a GPIO block — has no line the root
controller knows, so its driver reaches the line through the controller's
driver: an `InterruptLine` link (`plans/SUPPLIERS.md` SL4) served over
`irqline-v1` (`tairix_abi::driver::irqline`).

## Discovery

The shared device-tree walk finds a node's interrupt parent as the Devicetree
Specification v0.4 §2.4 defines it, and `interrupts-extended` names it per specifier
and wins over `interrupts`. A specifier on the root controller stays an `Irq`
grant at its position. One on any other node marked `interrupt-controller`
becomes a link to that node: the endpoint its node id names in
`INTERRUPT_LINE_ENDPOINTS`, the specifier's cells as the selector, its position
as the link's index, and its `interrupt-names` entry as the name when it fits
eight bytes. Every such controller carries the role's duty. A controller no
consumer may use links nothing; the kernel's own controllers take only their
root lines.

## The service

- **Wait.** A consumer names the link discovery gave it, and the controller
  believes it only once the kernel attests the caller holds it. The wait
  unmasks the line and is answered when it fires; the line is masked from then
  until the consumer waits again, so a level line is serviced before it can
  fire again and an edge arriving meanwhile answers the next wait at once.
- **One consumer a line.** Another live holder of a link to the same line is
  refused `Busy`; a line an ended consumer held is taken by its link's next
  holder. A consumer that ends has its lines masked and released.
- **The consumer side** is `tairix_linkclient::IrqLineClient`: a blocking
  `wait`, or a `post_wait` its serve loop wakes on and `reap_wait` collects,
  beside any direct `Irq` it binds.
- **The supplier side** is `tairix_drvrt::irqline::IrqLines`, the one
  definition of these rules every controller's driver keeps its lines by,
  over its own registers (`LineHardware`).

The BCM2835-family GPIO controller serves the same service for its lines;
see [GPIO-controller drivers](./gpio.md).

## `drivers/irq/brcmstb_l2`

The Broadcom second-level controllers, edge layout (`brcm,l2-intc`,
`brcm,bcm2711-l2-intc` and their kin) and level layout
(`brcm,bcm7271-l2-intc`), the layout picked by the bind key the node matched.
At bring-up every line is masked and every latched edge cleared. The
controller's own interrupt is one line of the root controller; on it the
driver reads the unmasked status, masks and acknowledges each line that fired,
and answers its wait. On the BCM2711 the always-on controller carries each
HDMI block's hotplug, CEC and wake-up lines.

## Status

The protocol, the walk's links and duties, the client and the driver exist,
host-proven against a register model of both layouts. QEMU's boards carry no
such controller, so the metal acceptance is the HDMI hotplug run
(`plans/SOUND.md` SND19q).
