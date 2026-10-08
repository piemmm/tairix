# `kernel/iommu/riscv` — the RISC-V IOMMU

One RISC-V IOMMU: a device directory one, two or three levels deep, the
deepest the unit takes, of contexts that block a device until a domain is
attached; second-stage translation wherever the unit walks `Sv39x4`, `Sv48x4`
or `Sv57x4`, the 16 KiB root resolving two bits more, and first-stage
translation tagged by PSCID otherwise, both at a 4 KiB granule with 2 MiB and
1 GiB leaves, in the shallowest mode wide enough for an identity window
anywhere the unit reaches; the command queue, every removal confirmed by an
`IOFENCE.C` before it is reported done, a carve's free that changed leaves
alone invalidated a page at a time up to half a ring and its domain otherwise,
and a rejected command replaced by a fence so the queue consumes on; and the fault queue, drained in batches
outside the unit's lock, an overflow acknowledged. Bound by discovery to
nodes keyed `compatible "riscv,iommu"` (`COMPATIBLE`). The design is
`plans/IOMMU.md` IOM16.

The unit is taken over with its directory off, so from then on nothing
inbound is allowed until the kernel enables a directory of invalid contexts;
firmware's cached contexts and translations are invalidated first. A valid
context is never rewritten in place: it is made invalid and forgotten before
its replacement is written, since its words cannot change together. A
silenced device walks empty tables with its translation faults unrecorded.

## Stability tier

**experimental**.

## Hardware

RISC-V IOMMU version 1.0 units walking little-endian. A page fault is told
apart into nothing mapped and a mapping that refuses the access by the
device's own domain, which the record does not say. Faults are raised on the
first wired line the unit's node names, where every cause's vector is set, or
by message, chosen as the unit is taken over, while nothing is live; its
performance counters are stopped. A unit that keeps big-endian access or
`GXL` set is refused. A second-stage unit with memory-resident interrupt files
confines each device's messages to a file of its own through an MSI page
table; a stream translating through a domain is confined as every other
stream of that domain is. Page requests, ATS, process contexts, flat MSI
translation and nested translation are not used.

## Tests

Host tests against a register-level model of the unit (the directory at each
depth and context format, both stages' walks, the command queue with its
error recovery, the fault queue with its overflow), including the shared
conformance suite at each stage; enrolled in `cargo xtask miri`. On QEMU,
`tairix-test-dma-translation-qemu-riscv64` runs the production kernel behind
the `virt` board's `iommu-sys` at the second stage and
`tairix-test-dma-translation-stage1-qemu-riscv64` at the first.
