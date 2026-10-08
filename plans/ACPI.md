# ACPI — the namespace bus driver and its AML interpreter

Binding under `AGENTS.md`. How the devices an x86 machine's firmware describes
in its ACPI namespace reach the hardware tree: the node the architecture port
publishes for the namespace, the user-space bus driver that interprets its AML,
the authority that driver holds, and what it publishes. The port keeps the
static tables it needs to boot (RSDP, XSDT, MADT, MCFG, DMAR, FADT); only AML
leaves it.

## Ledger

| Id | Item | Status |
|---|---|---|
| A1 | The namespace node: the x86_64 port publishes the ACPI namespace as a node carrying the DSDT and every SSDT as read-only table grants, the FADT's facts, and the region authority | planned |
| A2 | Region access: the bus driver reaches an operation region only through a kernel-checked map — system memory that is not usable RAM and not the kernel's own, system I/O outside the ports the kernel keeps, the configuration space of PCI functions in the tree — a refused region failing the method that touched it, logged | planned |
| A3 | The AML interpreter (`drivers/bus/acpi`, host-testable `lib` target): the term parser, the namespace (scopes, devices, methods, names, fields, regions, mutexes, events, aliases, externals, processors, power resources, thermal zones), method evaluation with the full expression opcode set, field access with every access type and update rule, `_OSI`/`_OS`/`_REV`, and bounds on loops, recursion, objects and time | planned |
| A4 | Device publication: every present device (`_STA`) with a `_HID` or `_CID` emitted with ACPI match keys and the resources its `_CRS` declares — memory and I/O, interrupts as GSIs, GPIO interrupts and I2C connections as supplier links, `_DSM`-derived properties | planned |
| A5 | PCI interrupt routing from `_PRT`, for a function without MSI — the x86_64 virtio-iommu's fault line among them, which the kernel binds once the interpreter publishes it (`plans/IOMMU.md` §8) | planned |
| A6 | Power-off: `\_S5` through the FADT's PM1 control registers (`plans/ARCHSUPPORT.md` A7) | planned |
| A7 | Events: the SCI, GPE blocks, the `_Lxx`/`_Exx` methods and `Notify`, so the power button reaches the session | planned |
| A8 | The QEMU vertical: the interpreter on QEMU q35's own tables — power-off, the PS/2 controller (`PNP0303`), the power button | planned |

## Where it runs, and why

AML is firmware bytecode: untrusted input that, run in the kernel, could reach
everything the kernel can. Here it runs in a bus driver, as any bus driver
does, holding exactly its node's grants: the tables to read, and the regions
the kernel agrees to map. A fault in it costs the devices it would have
published, never the kernel.

## Authority (A1, A2)

- The namespace node is published by the port with each table's physical range
  as a read-only grant and the FADT's register blocks (PM1, PM timer, GPE) as
  port or memory grants.
- An operation region is mapped on demand — AML computes a region's address at
  run time — through one request the kernel checks against the boot memory map
  and the tree: usable RAM, kernel memory, and the ports the kernel drives
  itself are refused. A region inside another node's resources is shared with
  that node's driver only where the namespace itself declares the resource for
  the device, as firmware does for its own devices' registers.
- The driver publishes under its own node with `CAP_HW_EMIT`, and wires a
  device to a supplier it did not emit (an I2C controller found on PCI) under
  `CAP_HW_DESCRIBE` (`plans/SUPPLIERS.md` SL3).

## The interpreter (A3)

- The DSDT, then each SSDT in table order, is parsed into the namespace;
  `Load`/`LoadTable` of further tables are refused, as is `Unload`.
- Evaluation is complete for the opcode set — integers of the revision's
  width, strings, buffers, packages, buffer fields, references, the arithmetic,
  logical, conversion, `Concatenate`/`Mid`/`Index`/`Match`/`SizeOf`/
  `ObjectType`/`CopyObject` operators, `If`/`Else`/`While`/`Break`/`Continue`,
  `Mutex`/`Acquire`/`Release`, `Event`/`Signal`/`Wait`/`Reset`, `Sleep` and
  `Stall` — so a real machine's methods run as written.
- `_OSI` answers the Windows strings the firmware expects of a current
  operating system, so it takes the paths it was tested on.
- Bounds: a `While` loop's iterations, the call depth, the namespace's and a
  method's object counts, and a method's run time are bounded; exceeding one
  aborts the method, logged, and fails what depended on it.

## Publication (A4, A5)

- A device is published when `_STA` says present and functioning (absent
  `_STA` meaning both), with `acpi:` match keys for its `_HID` and each `_CID`.
- `_CRS` resources: `Memory32Fixed`/`QWordMemory`/`DWordMemory` as memory
  grants, `IO`/`FixedIO` as port grants, `Interrupt`/`IRQ` as GSIs the kernel
  routes, `GpioInt` as an `InterruptLine` link to its GPIO controller,
  `I2cSerialBusV2` as an `I2cTarget` link to its controller (`plans/I2C.md`
  I7) — a controller with an `_ADR` under a PCI root named by its PCI address.
- `_DSM` is evaluated for the properties a driver needs: the HID-over-I2C
  function of `3CDFF6F7-4267-4555-AD05-B30A3D8938DE` gives
  `HidDescriptorAddress`.
- `_PRT` routes a PCI function's INTx pin to a GSI where it has no MSI.

## Power and events (A6, A7)

- Power-off evaluates `\_PTS(5)`, reads `\_S5`'s package, and writes SLP_TYP
  and SLP_EN to the FADT's PM1 control blocks.
- The SCI is the FADT's interrupt; GPE status is read from its blocks and each
  set bit runs its method; `Notify` on the power-button device reaches the
  session as a power request.

## Invariants

- No AML runs in the kernel.
- The interpreter reaches hardware only through kernel-checked maps, never
  usable RAM or the kernel's own memory.
- Every method has a bounded cost; a firmware bug ends a method, not the
  driver.
