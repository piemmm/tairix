# `kernel/iommu/smmuv3` — Arm SMMUv3

One System MMU: a stream table, two-level where the unit has it, of entries
that block a stream until a domain is attached; stage 2 translation wherever
`IDR0.S2P` is set, and stage 1 with one context descriptor per domain
otherwise, both over 4 KiB-granule AArch64 tables with 2 MiB and 1 GiB leaves,
a stage-2 walk on a unit whose output is narrower than 44 bits starting at
level 1 over up to sixteen concatenated tables; the command queue, every removal confirmed by a `CMD_SYNC` before it is
reported done and a rejected command replaced by one so the queue consumes on;
and the event queue, drained in batches outside the unit's lock, an overflow
acknowledged. Bound by discovery to nodes keyed `compatible "arm,smmu-v3"`
(`COMPATIBLE`). The design is `plans/IOMMU.md` IOM15.

The unit is taken over with `GBPA.ABORT` set, so from then on a transaction
arriving while it is disabled is aborted, and firmware's cached configuration
and translations are invalidated before any stream is attached. A blocked
stream's transactions are aborted and recorded (`C_BAD_STE`); a stream silenced
after a fault storm is aborted without a record.

## Stability tier

**experimental**.

## Hardware

SMMUv3 units with AArch64 little-endian table walks, the 4 KiB granule and
coherent table and queue access. A unit that is not coherent, forces stalls,
fixes its table or queue bases, offers a command queue smaller than the one
the family runs, or reports a service failure (`GERROR.SFM`) is refused. A unit with MSIs signals `CMD_SYNC` completions by
message; one without completes a sync when the queue is consumed past it.
Faults are raised on the wired line the unit's node names, edge-triggered,
else by message where the unit has MSIs. Stalls, ATS, PRI,
substreams and nested translation are not used.

## Tests

Host tests against a register-level model of the unit (stream table, both
stages, the command queue with its error recovery, the event queue with its
overflow), including the shared conformance suite; enrolled in
`cargo xtask miri`. On QEMU, `tairix-test-dma-translation-qemu-aarch64` runs
the production kernel behind an `arm-smmuv3` at stage 2 and
`tairix-test-dma-translation-stage1-qemu-aarch64` at stage 1.
