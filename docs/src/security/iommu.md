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
`PermissionDenied` for any window reaching them, whether or not the unit was
brought up), and `hw_emit_node` refuses a published `Iommu` node or
`IommuReserved` window. A bus driver may pass its own stream on to a child for
the same device; the coverage check holds it to its own range.

A device behind a unit must use it. A virtio function that does not offer
`VIRTIO_F_ACCESS_PLATFORM` declares that it reaches memory by physical address,
past the unit, so discovery does not publish it: no driver is loaded for a
device the kernel could not confine, and the refusal is audited (`4087`). Run
such a device with `iommu_platform=on`. Devices on a platform with no unit are
unaffected.

Firmware may name a reserved window twice, or two that overlap; a domain maps
their union once.

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

Nothing is reused before the unit confirms the device lost it. An unmap the
unit did not confirm keeps its IOVA and its frame out of reuse until a later
confirmed sync covers it; a map or attach whose flush failed takes back what it
installed, and one that cannot is kept as unconfirmed. A domain whose end the
unit did not confirm stays recorded for good: its carves leak, and the node
takes no further driver's carves. The translation is itself the custody of
translated carves, and it never frees what reaches it.

## Bus mastering follows ownership

A function issues DMA, and the writes that deliver its MSIs, only while its
Bus Master Enable is set. TAIRiX sets it only as a function is handed to an
owner, and clears it as that owner ends. No routing or mapping step sets it.

- **One owner of configuration space.** On x86_64 the kernel owns the
  configuration space of every function its boot probe enumerates. It
  reaches them through one PCI host, one access at a time, and keeps a
  record of what it handed over, built from its own emission. No process
  can be granted the configuration ports.
- **Nothing masters at take-over.** Before any unit is taken over, the
  probe stops mastering:
  - every function behind a unit, except a bridge or a function whose
    stream firmware keeps a window for;
  - every virtio function.

  As each unit is enabled, the kernel reads every recorded function behind
  it and reports any still mastering (`masters` on the unit's record).
- **Translated.** A function masters once its owner's domain is attached,
  and stops before that domain is destroyed, so its device is quiet before
  its streams are blocked. A stream firmware keeps a window for is handed
  back to firmware still mastering.
- **Untranslated.** A function masters from its owner's first carve until
  its owner ends. This narrows the window but confines nothing; the
  quarantine stays.
- **In owner order.** Every change is made for an owner's generation, and
  the kernel ignores one from an owner older than the last to change that
  function, so an owner that ends late never stops its successor's device.
- **Raspberry Pi 4.** The PCIe bus driver owns the VL805's configuration
  space. It makes the function a bus master as it publishes it, and stops
  it if the publish is refused. The root port's own bit forwards for the
  whole subtree and stays on for the bridge's life. The platform has no
  unit.

## Faults

Each unit's fault interrupt — a message-signalled interrupt the kernel takes
for itself and hands to no process — wakes one kernel task, which drains the
unit's fault records and parks again. A record names its stream, the page, the
access and the reason. It is recorded against the node whose owner holds the
stream (an owner revoked but still recorded included), or against the unit for
a stream no owner holds.

Faults are budgeted per window of one second: each stream may have four
recorded and the unit thirty-two, the rest only counted and reported with the
next record. A stream that raises 512 in a window is silenced — blocked, its
faults no longer recorded — and its node marked `Offline`, recorded once. A
silenced stream takes an owner again at its node's next driver. A unit is
drained at most 1024 times a window; a storm past that waits out the window,
so a device cannot hold a CPU or the interrupt line.

The live path is proven on QEMU q35 behind an `intel-iommu` by
`tairix-test-dma-fault-qemu-x86-64`: a misbehaving in-kernel virtio-blk driver
carves through its node's domain, confirms a mapped read, then points a device
write at an unmapped address. The unit refuses it and raises its fault-event
MSI, which the per-unit fault service drains into `DmaTranslationFault` against
the device's node, with a canary page the write never reached. The budget's
storm/silence path is proven against the register-level VT-d model, not live: a
QEMU virtio device breaks after one refused DMA, so a 512-fault window would
need hundreds of device resets, a load-dependent test.

## Audit

| Event | Id | When |
|---|---|---|
| `DmaTranslationBypass` | 4087 | discovery refused a function behind a unit that would not use it; `address`, `unit` |
| `DmaTranslationFault` | 4088 | a unit refused an access; `unit`, `stream`, `iova`, `access`, `reason`, `suppressed`, and `node` where an owner holds the stream |
| `DmaTranslationStorm` | 4089 | a stream stormed: silenced, its node `Offline`; the fault's fields and `outcome` |
| `DmaTranslationUnit` | 4094 | boot brought a unit up (`outcome=translating`, with `masters`, the functions behind it found mastering as it took over without a firmware window) or left it untranslated (`unmatched`, `no_registers`, `exhausted`, `unconfirmed`, `hardware`, `refused`); `faults_unrouted` with a `reason` when its faults cannot be served |
| `DmaTranslationUnconfirmed` | 4095 | a unit could not confirm a translation ended — a driver's domain, a removed node's, or one carve's; `node`, `generation` |
| `DmaBusMaster` | 4147 | the kernel turned a function's bus mastering on as its owner began or off as it ended; `node`, `master` (`on`/`off`), `outcome` (`applied`, or `refused` where the function reads back otherwise) |

A malformed DMAR, or unit nodes that could not be emitted, is logged at boot
(`4103`): every device's DMA is then unconfined.

## What is staged

Isolation groups for devices that share a requester id, interrupt remapping,
AMD-Vi, SMMUv3, the RISC-V IOMMU and virtio-iommu, multi-segment discovery,
and the administrator's view are ledger items in `plans/IOMMU.md`.
