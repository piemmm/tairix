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
| `HwResourceKind::IommuAlias` | a DMA master | the fabric also delivers its DMA as these streams: a bridge re-tagged it |
| `HwResourceKind::IommuGroup` | a DMA master | its DMA cannot be kept apart from that of every other node naming the same `(unit, id)` |
| `HwResourceKind::IommuReserved` | a unit | stream `stream` keeps an identity window firmware still uses (a VT-d RMRR) |

On x86_64 the boot walks the PCI hierarchy once (`tairix_pci::topology`),
parses the ACPI DMAR, emits one `Iommu` node per DRHD (carrying its RMRR
windows), and gives each PCI function it publishes the stream its unit knows
it by — its requester id, resolved through the DRHD device scopes and the
walk's bridge paths — with its aliases and its group. A function names a
stream only if its unit's node was emitted.

No process can state a translation fact. The kernel loads no driver for an
`Iommu` node, maps a unit's registers for no process (`mmio_map` answers
`PermissionDenied` for any window reaching them, whether or not the unit was
brought up), and `hw_emit_node` refuses a published `Iommu` node or
`IommuReserved` window. A bus driver may pass its own streams, aliases and
group on to a child for the same device; the coverage check holds it to its
own range, never lets an alias become a stream, and lets a group pass only
unchanged.

A device behind a unit must use it. A virtio function that does not offer
`VIRTIO_F_ACCESS_PLATFORM` declares that it reaches memory by physical address,
past the unit, so discovery does not publish it: no driver is loaded for a
device the kernel could not confine, and the refusal is audited (`4087`). Run
such a device with `iommu_platform=on`. Devices on a platform with no unit are
unaffected.

Firmware may name a reserved window twice, or two that overlap; a domain maps
their union once.

## Isolation groups

Two functions the fabric cannot keep apart share a group:

- **Aliases.** A bridge to conventional PCI takes ownership of the requests
  below it and tags them with its secondary bus and function `00.0`; a
  conventional bridge, or one from conventional PCI, with its own id. The
  unit sees the alias, never the device, so each function's aliases are
  translated with its own stream, and a firmware window is kept on every
  alias of its function.
- **ACS.** Where a unit covers the segment, the walk turns on the ACS
  controls each function offers (Source Validation, Request and Completion
  Redirect, Upstream Forwarding) and reads back what stayed on. A function
  joins the furthest device its DMA cannot be told apart from: the topmost
  bridge that tags it, then every bridge above whose path lacks ACS. A slot
  whose functions lack ACS is one group. A bus below the root complex holding
  a port without ACS, or an endpoint beside a port, is open, and everything
  below it is one group. Extended configuration space is reached through
  ECAM only, so through mechanism #1 no function has ACS.
- **One owner per group.** A second node's driver is refused its first
  carve (`Busy`, audited `4148`) while the group's holder lives. The holder's
  domain translates its own node's streams only; the group's other members
  stay blocked, or in their firmware domains.
- **Unconfinable.** A function whose group spans units, or whose aliases
  outnumber what a node can name, is published to no driver (`4087`,
  `reason=unconfinable`).

Grouping cannot stop a device below a port without Source Validation from
presenting another's requester id, and peer traffic between root ports is
taken on the root complex's word: the PCI Express specification requires ACS
on a root port that routes it.

## Domains and ownership

A node is **translated** when its stream names a unit the kernel brought up.
Its driver instance, admitted with a generation, gets a domain at its first
carve: an I/O page table holding the node's streams and the firmware windows
they keep. Every carve maps there, at an IOVA below the grant's addressing
limit; the frames themselves may lie anywhere. `dma_alloc` and
`shm_create_dma` hand the driver that IOVA, never a physical address.

- **One owner per group.** A later generation's first carve ends the earlier
  owner's domain first; a live owner of another node in the group refuses
  the carve, and the unit refuses a second domain any one stream.
- **The kernel's floor disk** carves as `KERNEL_OWNER`; the floor bring-up
  claims its node, so no driver is admitted for it, and none can take its
  group (`DmaError::KernelOwned`, `Busy`).
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
  - every function behind a unit, except one that masters nothing of its
    own — a bridge with a type-1 or type-2 header, or a host bridge, whose
    Bus Master Enable chipsets commonly hardwire on — or a function whose
    stream firmware keeps a window for. An LPC bridge is stopped;
  - every virtio function.

  As each unit is enabled, the kernel stops every recorded function behind
  it still mastering without a firmware window, and reports how many it
  stopped and how many would not stop (`stopped`, `refused`).
- **What cannot be confined publishes nothing.** A segment whose walk forms
  no tree, whose units the hardware tree has no room for, or whose functions'
  DMA identities or stops fail publishes no function. A flat scan then stops,
  where a unit covers the segment, every function mastering DMA of its own,
  firmware windows included, and every bridge, so a root port refuses what
  lies below it however its buses were numbered; elsewhere, every virtio
  function. The boot log states how many it stopped and how many still read
  back mastering. Where only the tree is missing, every unit still comes up,
  keeping no firmware window, so it blocks every stream: a device that
  ignores its own Bus Master Enable reaches nothing.
- **Translated.** A function masters once its owner's domain is attached,
  and stops before that domain is destroyed, so its device is quiet before
  its streams are blocked. A stream firmware keeps a window for is handed
  back to firmware still mastering.
- **Untranslated.** A function masters from its owner's first carve until
  its owner ends, the bit written before any carve is answered; an owner
  never handed its function takes nothing back. The floor disk is handed
  over at its bring-up. This narrows the window but confines nothing; the
  quarantine stays.
- **In owner order.** Each owner takes an epoch as it begins, and the kernel
  ignores a change from an owner that began before the last to change that
  function, so an owner that ends late never stops its successor's device —
  a parent and the child it published for one device included.
- **Raspberry Pi 4.** The PCIe bus driver owns the VL805's configuration
  space. It makes the function a bus master as it publishes it, and stops
  it if the publish is refused. Nothing tells it when the xHCI driver ends,
  so the function keeps mastering past it. The root port's own bit forwards
  for the whole subtree and stays on for the bridge's life. The platform has
  no unit.

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
| `DmaTranslationBypass` | 4087 | discovery refused a function behind a unit that it would not use (`reason=bypasses_unit`) or that could not confine it (`reason=unconfinable`); `address`, `unit` |
| `DmaTranslationFault` | 4088 | a unit refused an access; `unit`, `stream`, `iova`, `access`, `reason`, `suppressed`, and `node` where an owner holds the stream |
| `DmaTranslationStorm` | 4089 | a stream stormed: silenced, its node `Offline`; the fault's fields and `outcome` |
| `DmaTranslationUnit` | 4094 | boot brought a unit up (`outcome=translating`, with `stopped` and `refused`, the functions behind it found mastering as it took over without a firmware window that were stopped and that would not stop) or left it untranslated (`unmatched`, `no_registers`, `exhausted`, `unconfirmed`, `hardware`, `refused`); `faults_unrouted` with a `reason` when its faults cannot be served |
| `DmaTranslationUnconfirmed` | 4095 | a unit could not confirm a translation ended — a driver's domain, a removed node's, or one carve's; `node`, `generation` |
| `DmaBusMaster` | 4147 | the kernel turned a function's bus mastering on as its owner began or off as it ended, recorded in the order the changes landed; `node`, `generation`, `master` (`on`/`off`), `outcome` (`applied`, or `refused` where the function reads back otherwise) |
| `DmaGroupRefused` | 4148 | a driver's first carve was refused: another node's live owner holds its isolation group; `node`, `generation`, `group`, `holder` |
| `PortIoRefused` | 4158 | a driver's port access reached the ports the kernel keeps (PCI configuration mechanism #1); `port`, `width`, `task` |

A malformed DMAR is logged at boot (`4103`) and refused whole, and the
machine is then treated as having no unit: every device's DMA is unconfined
(`plans/OPEN-DEFECTS.md` D655). A segment that cannot be confined — unit
nodes that could not be emitted, a walk that forms no tree — is logged under
the same id, with the flat scan's `stopped` and `refused`, and publishes
nothing.

## What is staged

Interrupt remapping, ATS policy, AMD-Vi, SMMUv3, the RISC-V IOMMU and
virtio-iommu, multi-segment discovery, and the administrator's view are
ledger items in `plans/IOMMU.md`.
