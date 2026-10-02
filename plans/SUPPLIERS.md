# SUPPLIERS — links between hardware-tree nodes

Binding under `AGENTS.md`. How a device that depends on another device's
service is wired to it: the I2C target behind a controller, the GPIO line an
interrupt arrives on, the controller a firmware description names as a
device's bus. One kernel-mediated mechanism serves every such dependency,
whoever emitted the two nodes.

## Ledger

| Id | Item | Status |
|---|---|---|
| SL1 | The link: a consumer node names a supplier node, a role and a selector; the kernel mints one endpoint, grants it to the consumer and places the matching client record on the supplier's node; replaces `BusChild` | planned |
| SL2 | Resolution and ordering: a supplier named by node id or by stable address (PCI segment:bus:device.function) resolves when it appears; the device manager binds a consumer only once every supplier it names is bound, and unbinds it when one goes | planned |
| SL3 | Authority: an emitter links within its own subtree; only a platform describer — the architecture port, or the ACPI bus driver under `CAP_HW_DESCRIBE` — links across subtrees; the supplier's driver validates every selector it serves | planned |
| SL4 | Device-tree discovery: `interrupt-parent` and `interrupts-extended` resolved to any interrupt controller — a GIC (or PLIC) line stays an `Irq`, any other controller's specifier becomes an interrupt-line link to it; I2C children become I2C-target links; the `status` property honoured (D168) | planned |
| SL5 | Device facts: a node property carrying a closed key and a 64-bit value; the USB interface number is the first key, and the HID descriptor register (`hid-descr-addr`, or the ACPI `_DSM` that states it) lands with the driver that reads it (`plans/HID.md` H6) | in progress |

## The link (SL1)

- **On the consumer** a `Supplier` resource: the endpoint the consumer's
  driver calls, the role, and the selector. **On the supplier** a `Client`
  resource: the same endpoint, role and selector, which the supplier's driver
  creates and serves. Both are minted by the kernel from the one link the
  consumer's emitter declared, so neither side can name an endpoint the other
  did not get.
- **Roles** are a closed set, each a protocol the supplier serves and a
  meaning for the selector: `I2cTarget` (selector: 7-bit address and the
  bus speed the target allows, served as `plans/I2C.md` I1), `InterruptLine`
  (selector: line, trigger and polarity, served as `plans/GPIO.md` G1).
- `BusChild` is the case where the supplier is the consumer's parent; it is
  removed and its users (the I2C RTC drivers) move to `I2cTarget` links.

## Resolution and ordering (SL2)

- A supplier is named either by node id (an emitter linking within the nodes
  it emits) or by stable address — a PCI function's segment:bus:device.function
  — so a firmware description can name a controller another emitter
  published. A link whose supplier does not exist yet is pending, and resolves
  when it appears.
- The device manager binds a consumer only when every supplier its links name
  is bound, re-deciding on each tree generation; a supplier that unbinds or
  vanishes unbinds its consumers first. This is the ordering Linux's device
  links and deferred probe give, decided from the tree rather than by retrying
  a driver until it succeeds.

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

- Every cross-device channel is a kernel-minted endpoint on both nodes; no
  driver names another's endpoint.
- A consumer never runs before its suppliers, nor after one of them.
- Only a platform describer wires across subtrees, and the supplier validates
  every selector.
