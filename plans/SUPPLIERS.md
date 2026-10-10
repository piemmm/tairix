# SUPPLIERS — links between hardware-tree nodes

Binding under `AGENTS.md`. How a device that depends on another device's
service is wired to it: the I2C target behind a controller, the GPIO line an
interrupt arrives on, the controller a firmware description names as a
device's bus. One kernel-mediated mechanism serves every such dependency,
whoever emitted the two nodes.

## Ledger

| Id | Item | Status |
|---|---|---|
| SL1 | The link: a consumer node names a supplier node, a role and a selector; the supplier's node carries the role's duty to serve one endpoint, the consumer's a request naming that endpoint and the selector, and the supplier asks the kernel whether a caller holds the request it quotes; the DMA request line, the clock and the codec are its roles, and `BusChild` moves to it with the I2C target (`plans/I2C.md` I2) | in progress |
| SL2 | Resolution and ordering: a supplier named by node id or by stable address (PCI segment:bus:device.function) resolves when it appears; the device manager holds a consumer until each supplier it names serves, links closing a cycle excepted, and unbinds it before one that goes; the node-id half is done, the stable address arrives with the ACPI bus driver (`plans/ACPI.md` A4) | in progress |
| SL3 | Authority: an emitter links within its own subtree; only a platform describer — the architecture port, or the ACPI bus driver under `CAP_HW_DESCRIBE` — links across subtrees; the supplier's driver validates every selector it serves | planned |
| SL4 | Device-tree discovery: `interrupt-parent` and `interrupts-extended` resolved to any interrupt controller — a GIC (or PLIC) line stays an `Irq`, any other controller's specifier becomes an interrupt-line link to it; I2C children become I2C-target links; the `status` property honoured (D168, done) | in progress |
| SL5 | Device facts: a node property carrying a closed key and a 64-bit value; the USB interface number is the first key, and the HID descriptor register (`hid-descr-addr`, or the ACPI `_DSM` that states it) lands with the driver that reads it (`plans/HID.md` H6) | in progress |

## The link (SL1)

- **On the supplier** the role's duty: the one endpoint, from the role's
  block of node-indexed ids, its driver binds and serves. **On each consumer**
  the role's request: that endpoint, the selector, and where in the
  consumer's description it was named. Discovery writes both from the one
  description, so neither side can name an endpoint the other was not given.
- **The supplier is the gate.** A request a consumer quotes on a call is
  believed only once the kernel confirms the in-service caller holds it
  (`call_peer_holds`, asked only by the holder of the endpoint's duty and only
  about the role's own requests). A record per consumer on the supplier's node
  would not do: a node carries a fixed sixteen resources, a clock or GPIO
  controller serves more consumers than that on a larger SoC, and a consumer a
  bus driver publishes later would have to amend a supplier node already
  published.
- **Roles** are a closed set, each a protocol the supplier serves and a
  meaning for the selector: `Dma` (a request line, `plans/SOUND.md` SND5),
  `Clock` (the consumer's `clocks` specifier, `clock-v1`), `Codec` (the DAI
  format, which side drives the bit and frame clocks and which runs inverted,
  `codec-v1`),
  `I2cTarget` (7-bit address and the bus speed the target allows, served as
  `plans/I2C.md` I1), `InterruptLine` (line, trigger and polarity, served as
  `plans/GPIO.md` G1). A role arrives with its first supplier driver.
- `BusChild` is the case where the supplier is the consumer's parent; it is
  removed and its users (the I2C RTC drivers) move to `I2cTarget` links.

## Resolution and ordering (SL2)

- A supplier is named either by node id (an emitter linking within the nodes
  it emits) or by stable address — a PCI function's segment:bus:device.function
  — so a firmware description can name a controller another emitter
  published. A link whose supplier does not exist yet is pending, and resolves
  when it appears.
- A supplier is ready when it serves, not when its driver is loaded: the
  kernel records on the supplier's node, from the endpoint registry, whether
  each role's endpoint is bound, and bumps the tree's generation when that
  changes. Only the holder of the node's duty may bind it, in every role.
- The device manager holds a consumer while a supplier its links name is
  undecided: in the tree, matched by an installed driver, not serving the
  link's role, and not refused — or not in the tree at all, which a pending
  link and a dead one both are, since no node id is reissued. A supplier no
  installed driver can serve holds nothing: a clock the firmware owns would
  otherwise keep its consumers waiting forever, and the consumer's own driver
  is what learns the link is dead. A link that closes a cycle holds nothing
  either, as Linux's device links relax one: its members load together, each
  still held by any supplier outside the cycle. A supplier may sit later in
  the tree than its consumer, so a pass that decided something is followed by
  another; a supplier that vanishes takes its bound consumers down before
  itself. This is the ordering Linux's device links and deferred probe give,
  decided from the tree rather than by retrying a driver until it succeeds.

## Authority (SL3)

- A link inside the emitter's own subtree needs nothing beyond the emit
  itself, exactly as a child does today.
- A link to a supplier outside it lets a device description open a channel to
  a driver the emitter does not own, so only a platform describer may declare
  one: the architecture port (in-kernel) and the ACPI bus driver, which holds
  `CAP_HW_DESCRIBE`. The capability guards the class of all cross-subtree
  wiring, has its holder and its enforcement point in `hw_emit_node` landing
  together with the ACPI bus driver, and no existing capability expresses it.
- The supplier is the last word on what a selector may do: a GPIO driver
  refuses a line it does not have or that another consumer holds, an I2C
  driver serves exactly the address in the link and refuses every transfer to
  another.

## Device-tree discovery (SL4)

- The interrupt parent is found as Linux's `of_irq_find_parent` finds it;
  `interrupts-extended` names it per specifier. A specifier on the platform's
  root interrupt controller stays an `Irq` resource; one on any other node
  marked `interrupt-controller` becomes an `InterruptLine` link to that node,
  its cells the selector.
- The children of an addressed bus become `I2cTarget` links to their parent.
- A node whose `status` is neither absent nor `okay` is not emitted, and
  neither are its children (D168): a disabled controller binds no driver.

## Device facts (SL5)

A `Property` resource (`HwResource::property`) carries a fact a driver needs
that is neither a handle nor a match key: a closed key (`HwProperty`) and a
64-bit value, which the bound driver reads through `RtDriverHost::property`. A
property confers no authority, so no grant backs it at `hw_emit_node` and the
kernel never reads it.

- `UsbInterface`: the interface number the host-controller driver publishes on
  each interface node (`plans/USB.md` U11).
- `HidDescriptorAddress`: from the device tree's `hid-descr-addr` or from the
  ACPI `_DSM` that states it (`plans/ACPI.md` A4), added with the I2C-HID
  driver that reads it.

## Invariants

- Every cross-device channel is an endpoint discovery names on both nodes; no
  driver names another's endpoint, and no supplier believes a request the
  kernel has not confirmed its caller holds.
- A consumer never runs before its suppliers, nor after one of them.
- Only a platform describer wires across subtrees, and the supplier validates
  every selector.
