# DMA translation: confining what a device can reach

A device that masters DMA reaches memory by address, below the MMU. Without a
translation unit between it and RAM, whoever programs the device can read and
write all of memory, and a dead driver's device keeps whatever addresses it was
handed. A DMA translation unit (an IOMMU: Intel VT-d on x86_64) confines each
device to the memory its driver was given, and TAIRiX drives it as part of the
memory-isolation TCB. The staged design and the ledger of what remains is
`plans/IOMMU.md`.

## Topology in the hardware tree

Discovery states three facts, and only discovery states them:

| Fact | On | Says |
|---|---|---|
| `HwDeviceClass::Iommu` | a unit | a translation unit, its register window, keyed by its programming model (`compatible "intel,vtd"`) |
| `HwResourceKind::IommuStream` | a DMA master | it masters DMA through the unit at node `unit`, as streams `[first, first + count)` |
| `HwResourceKind::IommuReserved` | a unit | stream `stream` keeps an identity window firmware still uses (a VT-d RMRR) |

On x86_64 the boot parses the ACPI DMAR, emits one `Iommu` node per DRHD
(carrying its RMRR windows), and gives each PCI function it probes the stream
its unit knows it by — its requester id, resolved through the DRHD device
scopes and bridge paths. A function names a stream only if its unit's node was
emitted.

No process can state a translation fact. The kernel loads no driver for an
`Iommu` node, maps a unit's registers for no process (`mmio_map` answers
`PermissionDenied` for any window reaching them), and `hw_emit_node` refuses a
published `Iommu` node or `IommuReserved` window. A bus driver may pass its own
stream on to a child for the same device; the coverage check holds it to its
own range.

## Domains and ownership

A node is **translated** when its stream names a unit the kernel brought up.
Its driver instance, admitted with a generation, gets a domain at its first
carve: an I/O page table holding the node's streams and the firmware windows
they keep. Every carve maps there, at an IOVA below the grant's addressing
limit; the frames themselves may lie anywhere. `dma_alloc` and
`shm_create_dma` hand the driver that IOVA, never a physical address.

- **One owner per stream.** A later generation's first carve ends the earlier
  owner's domain first; a second live owner of a stream is refused.
- **The kernel's floor disk** carves as `KERNEL_OWNER`, and no driver can take
  its node (`DmaError::KernelOwned`, `Busy`).
- **A bus viewport does not stack on a domain**: a translated driver granted a
  translating `Dma` window is refused with `NotSupported`.
- A stream no owner holds keeps only its firmware windows, in a firmware
  domain of its own. Every other access faults.

## Revocation replaces the quarantine

A translated driver's end — its process reclaimed, or devmgr unloading it —
blocks its streams and destroys its domain with a confirmed invalidation. Every
carve is then unreachable, so its frames return to the allocator at once: the
DMA quarantine, which exists because nothing but a reset can prove an
untranslated device quiet, holds nothing for a translated node. A removed node
is forgotten the same way.

Nothing is reused before the unit confirms the device lost it. An IOVA, a table
frame or a DMA frame whose invalidation the unit did not confirm (its wait
expired) stays out of reuse for good: the owner stays recorded, its carves
leak, and the node takes no further driver's carves. The translation is itself
the custody of translated carves, and it never frees what reaches it.

## Audit

| Event | Id | When |
|---|---|---|
| `DmaTranslationUnit` | 4094 | boot brought a unit up (`outcome=translating`) or left it untranslated (`unmatched`, `no_registers`, `exhausted`, `unconfirmed`, `hardware`, `refused`) |
| `DmaTranslationUnconfirmed` | 4095 | a unit could not confirm a driver's domain ended; `node`, `generation` |

A malformed DMAR, or unit nodes that could not be emitted, is logged at boot
(`4103`): every device's DMA is then unconfined.

## What is staged

Fault reporting through the unit's interrupt, isolation groups for devices
that share a requester id, interrupt remapping, AMD-Vi, SMMUv3, the RISC-V
IOMMU and virtio-iommu, closing the window before the kernel takes a unit over,
and the administrator's view are ledger items in `plans/IOMMU.md`.
