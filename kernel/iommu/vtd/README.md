# `kernel/iommu/vtd` — Intel VT-d

One DMA remapping hardware unit in legacy translation mode: root and context
tables, second-level tables at the depth `SAGAW` allows with 2 MiB and 1 GiB
leaves, queued invalidation with bounded, fail-closed waits, caching mode,
table write-back for a walker that does not snoop, the protected-memory
hand-off, and fault recording drained in batches outside the unit's lock.
Bound by discovery to nodes keyed `compatible "intel,vtd"` (`COMPATIBLE`). The
design is `plans/IOMMU.md` IOM3.

## Stability tier

**experimental**.

## Hardware

Intel VT-d rev. 4.1 units with queued invalidation (`ECAP.QI`); a unit without
it, or one needing write-back the port cannot provide, is refused. Scalable
mode, interrupt remapping and ATS are staged in `plans/IOMMU.md`.

## Tests

Host tests against a register-level model of the unit (root and context walk,
second-level walk, the invalidation queue, fault recording), including the
shared conformance suite; enrolled in `cargo xtask miri`. On QEMU,
`tairix-test-dma-translation-qemu-x86-64` runs the production kernel behind an
`intel-iommu`.
