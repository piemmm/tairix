# `kernel/iommu/amdvi` — AMD-Vi

One AMD I/O virtualization unit: a device table over all 65536 device ids,
each entry blocking until a domain is attached; four-level v1 host page tables
with 2 MiB and 1 GiB leaves; the command buffer with bounded, fail-closed
completion waits, and every map flushed over the smallest aligned span holding
it; the event log drained in batches outside the unit's lock, restarted after
an overflow, a record the unit has not landed waited for and then skipped;
faults raised through the unit's own PCI function's MSI; and interrupt
remapping: a table per source, every requester id the source's interrupts can
arrive as pointed at it, 32-bit entries or 128-bit ones naming x2APIC
destinations, and IO-APIC pins raising their entry by index. Bound by
discovery to nodes keyed `compatible "amd,iommu"` (`COMPATIBLE`). The design
is `plans/IOMMU.md` IOM12.

The device table covers every id rather than the largest IVRS names: a
request from an id past a shorter table is not one the format promises to
refuse. At take-over firmware's exclusion range is cleared, so no DMA passes
untranslated, and the unit is told to snoop the CPU's caches. Commands wait
for translation to be on, as QEMU runs none before it; the flush that turns it
on drops whatever the unit cached.

## Stability tier

**experimental**.

## Hardware

AMD IOMMU rev. 3.08 units with host translation. Extended (x2APIC) interrupt
mode needs `XTSup` and `GASup`; a unit without `IASup` is flushed device by
device. Guest translation, ATS, PPR and the guest virtual APIC are not used.

## Tests

Host tests against a register-level model of the unit (device table, v1 walk,
the command buffer and its caches, the event log, interrupt remapping),
including the shared conformance and interrupt suites, with and without
whole-unit invalidation, a unit caching misses, and one running commands only
once translating. On QEMU, `tairix-test-dma-translation-amd-qemu-x86-64` and
`tairix-test-dma-fault-amd-qemu-x86-64` run the production kernel behind an
`amd-iommu` with `dma-remap=on`, remapping interrupts in extended mode.
