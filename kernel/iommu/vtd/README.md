# `kernel/iommu/vtd` — Intel VT-d

One DMA remapping hardware unit in legacy translation mode: root and context
tables, second-level tables at the depth `SAGAW` allows with 2 MiB and 1 GiB
leaves, context entries that translate untranslated requests only, queued
invalidation with bounded, fail-closed waits, a carve's free confirmed
page-selectively where the unit has PSI and one mask covers it, caching mode, table write-back
for a walker that does not snoop, the protected-memory hand-off, fault
recording drained in batches outside the unit's lock, and interrupt
remapping: a table sized to the machine, IRTEs that admit only their source's
requester ids, remappable MSI and IO-APIC entries, extended (x2APIC) mode,
compatibility interrupts blocked once on, and the way back off.
Bound by discovery to nodes keyed `compatible "intel,vtd"` (`COMPATIBLE`). The
design is `plans/IOMMU.md` IOM3.

## Stability tier

**experimental**.

## Hardware

Intel VT-d rev. 4.1 units with queued invalidation (`ECAP.QI`); a unit without
it, or one needing write-back the port cannot provide, is refused. Scalable
mode is not used.

## Tests

Host tests against a register-level model of the unit (root and context walk,
second-level walk, the invalidation queue, fault recording, interrupt
remapping), including the shared conformance and interrupt suites; enrolled in
`cargo xtask miri`. On QEMU, `tairix-test-dma-translation-qemu-x86-64` runs the
production kernel behind an `intel-iommu` remapping interrupts in extended
mode, and `tairix-test-dma-fault-qemu-x86-64` has a device write refused and
its record delivered through the fault-event interrupt.
