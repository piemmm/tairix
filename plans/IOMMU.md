# IOMMU.md — DMA remapping: every device reaches only what it was given

Binding under `AGENTS.md`. This plan owns how TAIRiX confines a bus-mastering
device to the memory the kernel mapped for it: the translation topology the
hardware tree carries, the kernel subsystem that drives every translation unit,
the domains a device's DMA is translated through, the lifetime of a mapping,
translation faults, interrupt remapping, and the per-family hardware support.
It does not own PCI enumeration beyond the identity and isolation facts a unit
needs, nor the DMA-engine seam (`plans/SOUND.md`), nor a driver's own
descriptor validation (`tests/SECURITY.md` §3.6).

Read first (§15.18): `plans/OPEN-DEFECTS.md` D167, D225, D241 (the quarantine
this plan confines), `plans/SOUND.md` SND5 (the DMA-engine seam and its bus
addresses), `plans/GPU.md` decision 8 (the GPU behind the unit),
`plans/ARCHSUPPORT.md` and `plans/FINISH-x86_64.md` (the x86_64 boot and PCI
floor), `plans/PI.md` (a platform with no unit), `plans/WIRING.md` (Arch HAL
parity), `plans/USB.md` (the HCD's bounce model), `AGENTS.md` §4 (hardware
isolation, no ambient authority), §5.4 (fail closed), §17.2 and §17.4 (HAL and
layering), §18 (discovery and the floor), §19 (threat model), §24 and §26
(scalability and the operating floor), §27 (complete primitives).

## Ledger

| # | Item | Status |
|---|---|---|
| IOM0 | This plan, the jump-sheet row, the §3 map entries, the `PLAN.md` row, the README corrections | done |
| IOM1 | Translation topology in the hardware tree: the `Iommu` class, the `IommuStream` and `IommuReserved` facts and their coverage rules, the refusal to load a driver for a unit or publish one; ACPI DMAR discovery on x86_64 | done |
| IOM2 | `kernel/iommu/api`: the unit contract, domains, the IOVA space, the generic radix I/O page-table engine with superpage leaves, the fault record, the conformance suite | done |
| IOM3 | `kernel/iommu/vtd`: Intel VT-d — legacy root and context tables, second-level tables at every supported depth, queued invalidation, caching mode, walk coherency, the PMEN hand-off, RMRR identity windows, fault recording and the fault event | done |
| IOM4 | DMA through domains: the device-DMA facility; `dma_alloc`, `shm_create_dma` and the kernel floor pools map each carve into its node's domain and return a device address; `VIRTIO_F_ACCESS_PLATFORM`; device addresses named as such in the ABI | done |
| IOM5 | Revocation instead of quarantine on translated nodes: a driver's death blocks its streams and frees every carve at once; an orderly removal frees (D241); the quarantine confined to untranslated nodes | done |
| IOM6 | Faults: drained in thread context from each unit's interrupt, stable audit events, a per-unit budget, a storm silences the stream and marks the node `Offline`. Host-proven against the register-level VT-d model, and a live QEMU vertical provokes one real fault delivered through the fault-event MSI, attributed to the device's node, canary untouched (§12). The storm stays host-proven — not live (§12) | done |
| IOM7 | Default-deny from the first bus-master enable: units enabled before TAIRiX sets Bus Master Enable on any function; bus mastering follows ownership | done |
| IOM8 | Isolation groups: requester-ID aliasing, ACS turned on and read on the upstream path and a switch's internal bus, multi-function devices without ACS, the extended-capability walk ACS lives in; the group is the unit of ownership | done |
| IOM9 | PCI identity: every function a node carrying its segment:BDF, the ATS, PRI, PASID and SR-IOV capabilities decoded on IOM8's extended-capability walk, segment-aware ECAM. Includes discriminating `PciFunction::admit`'s `VIRTIO_F_ACCESS_PLATFORM` gate to virtio functions by vendor id, so a non-virtio translated master is published with its stream rather than refused — latent until non-virtio PCI discovery exists (`plans/OPEN-DEFECTS.md` D507) | planned |
| IOM10 | ATS, PRI and PASID policy: ATS off at the device and refused at the unit; untrusted external-facing ports | planned |
| IOM11 | x86_64 interrupt remapping: VT-d IR (IRTEs, remappable MSI and IO-APIC entries, source-id validation) and x2APIC under EIM | planned |
| IOM12 | `kernel/iommu/amdvi`: AMD-Vi — IVRS discovery, the device table, command buffer, event log, its page tables and interrupt remapping tables | planned |
| IOM13 | The generic PCIe ECAM host bridge on aarch64 and riscv64 `virt` (`pci-host-ecam-generic`) — the prerequisite for every translated vertical off x86 | planned |
| IOM14 | FDT translation topology: `#iommu-cells`, `iommus`, `iommu-map` and `iommu-map-mask` in `lib/fdt` and the shared walk, naming one group for platform masters that share a stream id | planned |
| IOM15 | `kernel/iommu/smmuv3`: Arm SMMUv3 — the stream table, stage 2 (stage 1 where stage 2 is absent), the command queue with `CMD_SYNC`, the event queue, `GERROR`, `GBPA` abort | planned |
| IOM16 | `kernel/iommu/riscv`: the RISC-V IOMMU — the device directory, second-stage (first-stage where absent) tables, the command queue with `IOFENCE.C`, the fault queue | planned |
| IOM17 | `kernel/iommu/virtio`: virtio-iommu — attach, map, unmap and probe over the request queue, faults on the event queue, bypass off; ACPI VIOT and FDT topology | planned |
| IOM18 | MSI isolation off x86: the GICv3 ITS with its doorbell mapped per domain; the RISC-V IMSIC through the IOMMU's MSI page tables | planned |
| IOM19 | Scatter-gather carves: IOVA-contiguous over scattered frames on translated nodes, lifting the 32 MiB carve bound; DMA into shared-memory objects | planned |
| IOM20 | Throughput: invalidation batching with range-versus-domain selection, a deferred-free flush queue for streaming mappings, and per-descriptor wait status so a unit's queue lock is not held across its waits — measured | planned |
| IOM21 | System Information: units, groups, per-node translation state and fault counters behind `CAP_SYSINFO_HW` | planned |

Items are built in ledger order within a milestone. An item is complete —
tests, docs, and a green whole-project gate — before the milestone that needs
it closes.

### Milestones

| Milestone | Items | Exit criterion |
|---|---|---|
| **MI0 — a device reaches only what it was given** | IOM1–IOM6 | On QEMU q35 with `intel-iommu`, the production kernel boots from a virtio-blk root whose every DMA is translated; a device handed a raw physical address writes nothing there and the fault is recorded against its stream; a dead driver's carves are freed at once rather than quarantined. |
| **MI1 — no window, no forgery** | IOM7–IOM11 | No function is a bus master before its unit is enabled and its owner's domain attached; two functions the fabric cannot isolate share one owner or neither is loaded; a device cannot present a pre-translated address or forge an MSI. |
| **MI2 — AMD** | IOM12 | MI0's criterion on QEMU q35 with `amd-iommu,dma-remap=on`. |
| **MI3 — every native Tier-1 arch** | IOM13–IOM18 | MI0's criterion on aarch64 `virt` behind SMMUv3 and on riscv64 `virt` behind the RISC-V IOMMU, and on all three behind virtio-iommu. |
| **MI4 — fast at scale** | IOM19–IOM21 | A carve larger than 32 MiB on a translated node; a streaming workload's mapping cost measured and bounded; the translation state visible to an administrator. |

## 0. Binding decisions

These are settled. A change that contradicts one stops and asks (§15.7).

1. **A translation unit is part of the memory-isolation TCB, and the kernel
   alone drives it.** Whoever programs a unit can point any device at any
   memory, so no process maps a unit's registers, learns the physical address
   of memory mapped through one, or names a stream it does not own. The kernel
   refuses to load a driver for an `Iommu` node and refuses a published
   `Iommu` child; a unit's registers are never a grant. This is the MMU's
   position (§4), for the device side.

2. **The home is `kernel/iommu/`: `api/` plus one sibling crate per hardware
   family.** `api/` holds the unit contract, domains, the IOVA space, the
   generic I/O page-table engine, the fault record and the conformance suite;
   `vtd/`, `amdvi/`, `smmuv3/`, `riscv/` and `virtio/` each implement the
   contract. It is not `kernel/arch/<target>/`: a unit is an MMIO device
   specified independently of any instruction set, so by the charter's own
   test its code differs by the unit's specification, not by the ISA, and a
   platform may pair any CPU with any unit. It is not `drivers/`: a unit is
   the enforcement point for every driver's DMA and cannot be one of them.
   Parallel families are the deliberate shape of a modularity contract, not
   duplication. Units are matched to families at runtime by discovery through
   `lib/devmatch`, the same way the bootstrap floor binds; no image names a
   family.

3. **Discovery stays in the architecture port.** DMAR, IVRS and VIOT are ACPI
   and are parsed in `kernel/arch/x86_64/`; `iommus` and `iommu-map` are FDT
   and are read by the shared walk. What leaves the port is the normalised
   tree (§1), never a table.

4. **Default deny.** A unit is enabled with every stream blocked. A stream is
   attached to a domain only when its node's owner first carves DMA, and is
   blocked again when that owner ends. Firmware reserved windows (RMRR, IVMD)
   are the one exception: the streams they name keep an identity mapping of
   exactly those windows, installed before translation is enabled, because
   firmware still masters them (the scan-out of a firmware framebuffer, USB
   legacy emulation).

5. **A device address is not a physical address.** On a translated node the
   address a driver programs is an IOVA in its node's domain. The ABI calls it
   a device address everywhere; no field that carries one is named `phys`.
   A driver cannot tell a translated node from an untranslated one and must
   not care.

6. **A carve is mapped once, when it is carved, and unmapped before its
   frames are freed.** A mapping's memory returns to the allocator only after
   the unit has confirmed that no cached translation of it survives. There is
   no lazy mode in which a device can reach memory the kernel has freed;
   batching (IOM20) defers the *free* with the invalidation, never the
   invalidation past the free.

7. **Revocation replaces quarantine on translated nodes.** A driver's death
   blocks its streams, destroys its domain with a confirmed invalidation, and
   frees every carve at once. The quarantine (D167) stays for untranslated
   nodes, where nothing but a reset can prove a device quiet.

8. **One owner per isolation group, attached lazily.** A group belongs to at
   most one live domain, which translates the streams of its owner's own node
   alone. A parent that publishes a child for the same device (a bus driver
   and the controller it exposes) never carves, so the child's driver claims
   the group at its first carve; a second live owner of the group is refused
   and its carve fails. The unit refuses a second domain any one stream as
   well, so a tree that misnames a group still cannot share a stream.

9. **Faults are security events.** A translation fault is drained in thread
   context from the unit's interrupt — never polled — recorded with a stable
   event id, and charged to its stream's budget; a stream that exhausts it is
   blocked and its node marked `Offline`. The driver sees DMA fail; the kernel
   never stops for a device's fault.

10. **ATS is denied and PASID is not offered.** A device with ATS presents
    addresses the unit does not translate, so the unit is told to refuse
    translated requests and the device's ATS stays disabled (IOM10). Shared
    virtual addressing is out of scope until an item states its threat model.

11. **Interrupt remapping is part of the unit's job wherever the silicon has
    it.** An MSI is a DMA write; without remapping a device forges any vector
    to any CPU. IOM11 and IOM18 close it per architecture.

12. **A platform without a unit says so.** On such a platform (the Pi 4's
    BCM2711; a QEMU machine without one) a device reaches all of RAM; carves
    stay capability-bounded, control blocks stay controller-only, and the
    quarantine stays. The README's security matrix and the platform page state
    the limitation; nothing claims it closed.

13. **Nothing is sized by a constant a large machine outgrows.** Domain ids,
    IOVA spaces and stream tables are derived from the unit's reported
    capabilities and the discovered topology, and grow where the hardware
    allows (§24). Fault budgets are containment bounds, not capacities: they
    cap what one unit's faults may cost whatever the machine, and sit above
    what the family's architecture lets a unit hold (§24.4).

## 0a. The security position, stated honestly

| Attack | Without a unit | With a unit (after MI0) | After MI1 |
|---|---|---|---|
| A compromised driver points its device at another process's memory | open | closed: the device reaches only its node's domain | closed |
| A malicious device DMAs where it likes (Thunderclap, CWE-1257) | open | closed for translated streams | closed |
| A dead driver's device keeps writing into freed memory | held off by quarantine | closed: revoked before free | closed |
| DMA before the unit is enabled | open | closed (IOM7): no function TAIRiX takes from firmware masters before its owner's domain is attached | closed |
| Two functions behind one non-ACS switch reach each other peer-to-peer | open | open | closed (IOM8) |
| A device forges an MSI | open | open | closed on x86_64 (IOM11); IOM18 elsewhere |
| A device presents a pre-translated address (ATS) | n/a | refused: ATS never enabled | closed (IOM10) |

Residual, and named: a bug in a unit's family code (the TCB grew by it); a
unit erratum a family must work around; a platform with no unit, and one
whose DMAR is malformed, which is refused whole and treated as having none
(`plans/OPEN-DEFECTS.md` D655); the moments
between firmware's hand-off and the boot probe, which only firmware's own
protected memory regions cover; a device below a port without ACS source
validation presenting another's requester id, which grouping cannot stop;
peer traffic between root ports, taken on the root complex's word (the PCI
Express specification requires ACS on a root port that routes it); physical
attacks (§19.9).

## 1. IOM1 — topology in the hardware tree

The tree (§18.1) is the only inventory, so translation topology is three facts
in it.

- **`HwDeviceClass::Iommu`** — a translation unit. Its resources are its
  register window (`Mmio`) and its firmware reserved windows. Its match keys
  name its programming model: `compatible "intel,vtd"` from DMAR,
  `"arm,smmu-v3"` and `"riscv,iommu"` from FDT, a PCI class key for a unit
  that is itself a PCI function (AMD-Vi, `riscv-iommu-pci`), and the virtio
  device id for virtio-iommu. Admission refuses a driver for this class and
  `hw_emit_node` refuses a child of it.
- **`HwResourceKind::IommuStream`** on a DMA master: "this node masters DMA
  through the unit at node `base`, as stream ids `[xlate, xlate + len)`". It is
  a fact, not authority — no capability — and its coverage rule is containment
  within the same unit, so a driver can only pass its own stream range on to a
  child for the same device.
- **`HwResourceKind::IommuReserved`** on a unit: "stream `xlate` keeps an
  identity mapping of `[base, base + len)`". A reserved window names a stream,
  not a node, because firmware's masters are often functions no driver owns.

A node with no `IommuStream` is untranslated: its carves take physical (or
`dma-ranges`-translated) device addresses and the quarantine. A node behind a
unit whose stream the tree does not name has its DMA blocked — the failure is
closed, never an untranslated escape.

**x86_64 (DMAR).** `kernel/arch/x86_64` locates and validates the DMAR, decodes
every remapping structure, and emits one `Iommu` node per DRHD. A DRHD's
register window is sized from its `Size` field. Device scopes are resolved to
source ids by walking each scope's path through the bridges' secondary bus
numbers; an `INCLUDE_PCI_ALL` unit covers every function of its segment no
other unit claims. Each RMRR becomes an `IommuReserved` per scoped endpoint on
the unit that covers it. Each kernel-probed PCI function gains the
`IommuStream` of the unit covering its source id. A malformed table is refused
whole, never half-applied.

## 2. IOM2 — `kernel/iommu/api`

- **The unit contract.** `IommuUnit` (object-safe, one per hardware unit):
  its profile (input and output address widths, leaf sizes, domain-id space,
  walk coherency, feature flags), `create_domain`, `attach(stream, domain)`,
  `block(stream)`, and fault draining. A domain's operations are `map`,
  `unmap` and `sync`: `unmap` removes translations, and `sync` returns only
  when the unit confirms that no translation removed since the previous sync
  survives in any cache. Table-based families implement them over the generic
  engine; virtio-iommu implements them as requests.
- **The generic I/O page-table engine.** One radix walker, parameterised by a
  PTE format: levels (up to six), the index bits per level, the leaf sizes a
  level may hold, and the encoding of a table pointer, a leaf and its
  permissions. It maps a naturally aligned range with the largest leaves both
  alignments allow, unmaps exactly what was mapped (a split is refused, never
  improvised), and returns the tables an unmap emptied so they are freed only
  after the next confirmed sync. Table frames come from the HAL's
  `PageTableFrames`; a non-coherent walker is served by writing each touched
  line back through the shared DMA-visibility primitive.
- **The IOVA space.** A buddy allocator over naturally aligned power-of-two
  blocks, top-down below the device's reach, with the page at IOVA 0, the x86
  interrupt window, and every reserved window carved out before first use.
  Each order's free blocks are one sorted list: a search is logarithmic, and a
  change reserves its room first, so running out of memory is a value that
  leaves the space as it was.
- **Domains.** A domain owns its IOVA space, its engine (or its family's
  equivalent) and the ledger of what it mapped, so its destruction unmaps
  everything it holds with one confirmed sync.
- **Conformance.** A host-run suite every family passes against its register
  model: blocked-by-default, attach then translate, unmap then sync then the
  translation is gone, a fault is reported against the right stream, and a
  destroyed domain leaves nothing reachable.

## 3. IOM3 — Intel VT-d

Legacy translation mode: a root table, a context table per bus a covered
stream sits on, second-level tables at the depth the unit's `SAGAW` and the
IOVA reach call for (3, 4 or 5 levels), 2 MiB and 1 GiB leaves where `SLLPS`
allows. Invalidation is queued (`ECAP.QI`), with register-based invalidation
only for the bring-up that precedes the queue; every sync is an invalidation
wait descriptor with a status write, waited for within a bounded budget that
fails closed. Caching mode (`CAP.CM`) invalidates on every not-present to
present transition. A non-coherent walker (`ECAP.C` clear) has every table
line written back before the unit may walk it. Firmware's protected memory
regions are disabled once translation is on. Faults are read from the fault
recording registers, cleared, and signalled through the fault event MSI.
Scalable mode (for units that lack legacy mode, and for PASID) is IOM10's.

## 4. IOM4 and IOM5 — DMA through domains, and its lifetime

- **The device-DMA facility.** One kernel facility answers, per node, how its
  device reaches a carve: an untranslated node gets its physical address
  through its `Dma` window as today; a translated node gets an IOVA in the
  domain of its owner's generation. `kernel/mem`'s carve calls it after the
  frames are scrubbed and mapped for the CPU, and before anything is returned;
  `dma_free` and a space's teardown call it before any frame is freed.
- **Reach.** On a translated node the frame allocation takes no physical
  ceiling: the device's reach constrains the IOVA, not the frame. A translated
  `dma-ranges` window composed with a unit is refused until an item defines
  the composition.
- **Kernel floor pools.** The in-kernel floor drivers carve through the same
  facility for their own node, so the root disk is translated like any driver.
- **virtio.** Every virtio driver accepts `lib/virtio`'s one
  `TRANSPORT_FEATURES` wherever offered — `VIRTIO_F_ACCESS_PLATFORM`, since
  declining it asks the device to bypass the platform's translation, and
  `VIRTIO_F_VERSION_1`.
- **Lifetime.** A domain is created at its node's owner's first carve and
  attached then; the owner's death (its process reclaimed, or devmgr unloading
  it) blocks its streams and destroys the domain with one confirmed sync, after
  which every carve of that generation is freed as its space lets go. Whether
  a driver instance is translated is recorded on its load record at admission,
  so the answer cannot change under it. A successor's first carve ends an owner
  never revoked, then gets a fresh domain; a revoked generation carves nothing
  more. A surprise or orderly removal of a translated node revokes the same way
  and forgets the node, so D241's held memory is freed.
- **Unconfirmed is final.** An end the unit cannot confirm proves nothing
  about what the device still reaches, and nothing later can: the owner stays
  recorded for good, its IOVAs, tables and frames are never reused, and the
  node takes no successor's carves. The facility is the custody of translated
  carves for the same reason, and never frees what reaches it.
- **The kernel's floor.** A floor driver carves as `KERNEL_OWNER`
  (generation 0), below every driver generation; no driver can take its node.
- **Reach and viewports.** A translated driver's carve takes no physical
  ceiling; the grant's limit bounds the IOVA. A translated driver granted a
  translating `Dma` window is refused (`NotSupported`).

## 5. IOM6 — faults

- **The interrupt.** Each unit's fault event is a message-signalled interrupt
  from the port's kernel-only producer (`KernelArch::kernel_msi_facility`),
  never the `msi_alloc` pool a driver draws from. It is bound in the IRQ table
  to `FAULT_OWNER`, an identity below the task-id draw, so no process is given
  it and no exit releases the binding. The family routes and unmasks it
  (`IommuUnit::route_faults`); a unit whose faults cannot be served still
  translates, and says why (`DmaTranslationUnit`, `faults_unrouted`).
- **The task.** One kernel task per unit parks on that interrupt and drains.
  A drain takes at most one ring's worth and says whether records remain; the
  task drains again while they do, because a unit raises no interrupt for
  records it already holds (VT-d signals only when PPF sets). It never polls:
  between drains it parks, with no CPU-halt fallback, since a dispatched task
  always parks.
- **Attribution.** A record names its stream, page, access and reason. It is
  laid against the node whose recorded owner holds the stream — a revoked
  owner still recorded included — through the facility's stream index, and
  against the unit for a stream no owner holds (a firmware leftover, a device
  that lies about its requester id).
- **The budget.** Per one-second window each stream may have four records and
  the unit thirty-two; the rest are counted and reported with the next record,
  so neither one device nor a requester-ID sprayer floods the log. A stream
  raising 512 in a window — above the most records any VT-d unit holds, so an
  earlier owner's leftovers cannot storm it — is silenced, its node's fault
  health set `Offline`, and the storm recorded once; it stays counted until
  the window ends. A unit is drained at most 1024 times a window; past that
  the task waits the window out, so a storm can hold neither a CPU nor the
  interrupt line. The stream table's room is taken when the task starts, so
  the drain path never allocates.
- **Silenced streams** point at an always-empty table under a domain id of
  its own (caching mode reserves id 0), with fault processing disabled. A
  silenced stream takes an owner again at its node's next driver.
- **Stale status.** A family clears the fault status its firmware left before
  its first command: a standing status both raises no fault event for the
  next fault and would be blamed on the first invalidation.

## 6. IOM7 — no window

No routing or mapping helper makes a function a bus master: `route_msix`
turns memory decoding on, `route_msi` neither. Only the owner of a function's
configuration space sets Bus Master Enable, as it hands the function to an
owner, and clears it as it takes the function back.

- **The kernel's PCI host.** Where the kernel enumerates PCI itself (x86_64),
  one owner (`kernel/tairix-kernel/src/pci_host.rs`, published by the boot
  probe) holds the probe's own bus behind one lock: ECAM on the MCFG segment,
  else mechanism #1. It also holds the record of the functions it handed
  over, built from the probe's own emission and never from a node's
  descriptive address, which any publisher sets. Every kernel configuration
  access goes through it. The port-I/O gate refuses mechanism #1's ports to
  every process (`PortIoFacility::kernel_owned`).
- **Before take-over.** The probe stops these functions mastering:
  - every function behind a unit, whose stream the unit will block anyway,
    except one that masters nothing of its own — a bridge with a type-1 or
    type-2 header, which forwards, or a host bridge, the root complex's own
    function, whose Bus Master Enable chipsets commonly hardwire on, so take
    over would report it refused on every boot — or one a unit keeps a
    firmware window for (decision 4). An LPC bridge is type 0 and is stopped;
  - every virtio function it publishes or refuses.

  A function behind a unit is stopped whether or not its unit then comes up:
  one firmware keeps no window for has no claim on DMA after the hand-off. A
  function behind no unit that TAIRiX does not drive is left as firmware left
  it (decision 12). At each unit's enable, the facility stops every recorded
  function behind it still mastering without a firmware window, and reports
  how many it stopped and how many would not stop (`stopped`, `refused` on
  the unit's `DmaTranslationUnit` record).
- **What cannot be confined.** A segment whose walk forms no tree, whose
  units the tree has no room for, or whose functions' DMA identities or
  stops fail publishes nothing. A flat scan (`PciTopology::quiesce`) then
  stops, on a segment a unit covers, every function mastering DMA of its
  own, firmware windows included, and every bridge — a root port with Bus
  Master Enable clear refuses the upstream requests of its whole subtree,
  however its buses were numbered — and elsewhere every virtio function, and
  logs how many it stopped and how many read back mastering. Where only the
  tree is missing, every unit is still given its node, keeping no firmware
  window, so it comes up blocking every stream (decision 4): a device that
  ignores its own Bus Master Enable reaches nothing either.
- **Translated owners.** The facility grants bus mastering once an owner's
  domain is attached, at its first carve.
  - It names the owner's streams, so a bus-published child of a device
    masters through its parent's function (decision 8).
  - It withdraws bus mastering in the retirement's live arm, before the
    domain is destroyed, so a device stops before its revocation instead of
    being faulted by it.
  - The exception is a stream firmware keeps a window for, which firmware
    masters again.
  - Only the live arm withdraws, so a second retirer cannot stop what a
    successor was granted since.
  - An unconfirmed end stays withdrawn: no successor attaches to grant it
    again.
- **Every death path ends first.** A translated space's teardown ends its
  owner before it releases a block (`DeviceTranslation::end`). Whichever
  context drops the space, the order is: withdraw, block, one confirmed
  invalidation, free.
- **Changes are ordered by when owners began.** The host hands out an epoch
  as each owner begins — an untranslated driver at its first carve, a
  translated owner as it is published — keeps, for each function, the epoch
  of the latest owner that changed it, and ignores a change from one that
  began earlier. A node's successor retires its predecessor first, and a
  group's next owner waits for its holder's end, so an owner whose end lands
  late never stops its successor, whatever generations they were admitted
  with: a parent and the child it published for one device share a function.
- **Untranslated owners.** On x86_64 with no unit, the kernel still owns
  configuration space. A driver's function masters from its first carve
  (`dma_alloc`, `shm_create_dma`; the floor disk at its hand-over) until the
  driver ends; a driver never handed its function takes nothing back. The
  bit is written under the address-space registry's lock, where a concurrent
  carve of the same driver waits, so none is answered before its function
  masters. That narrows the window but confines nothing; the quarantine
  stays (decisions 7, 12).
- **The kernel's own device.** The floor bring-up claims the floor disk's
  node for the kernel before it touches the device
  (`InitSpawnCtx::claim_for_kernel`), so no process is admitted as its driver,
  translated or not: none can reach the device or take its function back.
- **The Pi.** `drivers/bus/pcie_brcm` owns the VL805's configuration space.
  - It makes the function a bus master just before it publishes it: the
    driver may run the moment its node is published, and the halted
    controller issues no DMA before its driver runs it. A refused publish
    stops it again, and the refusal is reported whether or not the function
    stops.
  - Nothing tells it when the xHCI driver ends, so the function keeps
    mastering past its driver (`plans/OPEN-DEFECTS.md` D587); the platform
    has no unit (decision 12).
  - The root port's own Bus Master Enable forwards for the whole subtree and
    originates no DMA. It must be set after the link trains, and it stays on
    for the bridge's life.
- **Never on a live device.** A virtio function written with Bus Master
  Enable clear disables itself and drops DRIVER_OK. So the grant lands
  before a driver sets DRIVER_OK (its first carve precedes it), and a
  command bit that already holds the asked value is never written.
- **Audit.** Each change the kernel makes is recorded with its owner's node
  and generation and what the function reads back (`DmaBusMaster`, 4147),
  under the host's lock, so the records keep the order the changes landed
  in. A reach for the configuration ports is refused and recorded
  (`PortIoRefused`, 4158).

## 7. IOM8 — isolation groups

Two functions the fabric cannot keep apart form one group, and one owner
holds a group at a time.

- **In the tree.** Each translated node carries its own `IommuStream`, an
  `IommuAlias` for each further stream the fabric tags its DMA with, and one
  `IommuGroup` — `(unit, id)`, the id the least requester id among the
  group's members. A group covers only itself and an alias only aliases
  inside it, and neither covers a requester stream: a child for the same
  device keeps its parent's group, and no driver can turn an alias into a
  function it may master. Delegating a finer grouping to a bus driver that
  enumerates a subtree of its own is IOM13's, with the first such driver.
- **Aliases.** Every bridge between a function and its root bus that takes
  ownership of its requests tags them: one to conventional PCI with its
  secondary bus and function `00.0`, a conventional one or one from
  conventional PCI with its own id; a PCI Express port passes them on (PCI
  Express to PCI/PCI-X Bridge rev. 1.0 §2.3). The owner's domain translates
  every alias with the node's own stream, and a firmware window is kept on
  every alias of the function it names.
- **Grouping** (`lib/pci::topology`, from one walk of the hierarchy). A
  function joins the furthest device its DMA cannot be told apart from — the
  topmost bridge that tags it, then every bridge above whose path to the root
  lacks ACS — and that device, sharing a slot without ACS, joins its siblings
  that lack it too: Linux's `pci_device_group`, without device-specific
  exceptions. A port isolates when Source Validation, Request and Completion
  Redirect and Upstream Forwarding, each it implements, are on. Beyond
  Linux, a bus below the root complex holding a bridge that does not
  isolate, or an endpoint beside a bridge, is open — a request reaches every
  function on it without passing a port that redirects it — and everything
  below an open bus is one group; grouping by the path alone would let one
  downstream port without ACS reach its siblings' devices.
- **ACS.** Where a unit covers the segment, the walk turns on every
  isolating control each function offers and reads back what the hardware
  kept. The controls live in extended configuration space, so a segment
  reached through mechanism #1 groups as if no function had ACS.
- **Ownership.** A second node's owner is refused its first carve
  (`DmaError::GroupBusy`, answering `Busy`, audited `DmaGroupRefused`, 4148)
  while the group's holder lives; a holder whose end the unit could not
  confirm keeps the group from every successor, and the kernel's floor disk
  keeps its group from every driver. The domain translates only the owner's
  own node's streams: the group's other members stay blocked, or in their
  firmware domains, so a function the owner was not handed cannot master
  into its domain and a firmware-mastered sibling never shares it.
- **Unconfinable.** A function whose group spans units, or that is tagged
  with more streams than a node can name, is published to no driver and
  audited (`DmaTranslationBypass`, `reason=unconfinable`).

## 7a. IOM9–IOM11 — identity, ATS, interrupt remapping

- **ATS.** Disabled in the device's ATS capability and refused at the unit
  (VT-d context `TT` untranslated-only; SMMUv3 `EATS` zero; AMD-Vi `IOTLB`
  zero; RISC-V `EN_ATS` zero).
- **Interrupt remapping.** VT-d IR with source-id verification, remappable
  MSI and IO-APIC entries, and x2APIC under extended interrupt mode, so a
  device can raise only the vectors its owner was given.

## 8. IOM12–IOM18 — the other families

- **AMD-Vi.** The unit is a PCI function whose register base IVRS names; the
  device table is sized to the largest device id IVRS covers; the command
  buffer's `COMPLETION_WAIT` is the sync; IVMD unity ranges are reserved
  windows; IVRS special entries give the IO-APIC and HPET ids for remapping.
- **SMMUv3.** Stage 2 wherever `IDR0.S2P` is set, so no context descriptors
  are needed; stage 1 with one context descriptor per stream otherwise.
  `GBPA.ABORT` holds while the unit is disabled; `CMD_SYNC` is the sync; the
  event queue is the fault source. On QEMU, `-global arm-smmuv3.stage=2`.
- **RISC-V IOMMU.** Second-stage (`iohgatp`, Sv39x4/Sv48x4/Sv57x4 with the
  16 KiB root) wherever `g-stage` is present; `IOFENCE.C` is the sync; the
  fault queue is the fault source.
- **virtio-iommu.** `bypass` is cleared so an unattached endpoint is
  blocked; `PROBE` reserved regions become reserved windows; the event queue
  carries faults.
- **MSI isolation off x86.** GICv3 with the ITS (the ITS validates each MSI's
  DeviceID) and its doorbell mapped into each domain; AIA with the IMSIC
  reached through the RISC-V IOMMU's MSI page tables.

## 9. Performance

Mapping happens at carve time, so a driver's steady-state rings cost nothing
per I/O. Discovery walks the hierarchy once and every observer reads that
walk; grouping is one pass, a union over each function's path to its root. Leaves are the largest the alignments allow, because a buddy carve is
naturally aligned and its IOVA is allocated at its own alignment. Teardown is
one confirmed sync per domain, not one per carve, in every death path: a
space's teardown ends its owner before it releases a block. Bus mastering costs
one configuration read-modify-write and a read-back at an owner's attach and
its end, off every hot path; an untranslated function is handed over once, at
its owner's first carve, so later carves touch no configuration space. Domain
lookup per carve is a
hash probe under a per-facility lock, off every hot path. Neither
facility-wide lock is held across a wait on a unit: an owner's first carve
retires a predecessor and adopts its streams under that owner's own state,
which it holds from before the owner is published, so only carves for the
same node wait for it; a group's next owner waits for its holder's end under
that holder's state alone; firmware domains are taken out of their table
before they are destroyed. A unit serialises its own queue, and every wait on it is
bounded by the family's command budget. Domain ids are handed out fresh first,
then in the order they were freed, at constant cost. IOM20 adds
measurement-backed batching for streaming mappings, per-descriptor wait status
so a unit's queue lock is not held across its waits, and an IOVA free list
whose updates are logarithmic rather than a sorted vector's linear moves.

## 10. Refused by name

- **Passthrough (identity) domains for convenience.** An identity domain is
  no isolation; only firmware reserved windows get identity mappings, and only
  for the streams firmware names.
- **Lazy invalidation that lets freed memory be reached.** See decision 6.
- **A user-space IOMMU driver.** See decision 1.
- **Bypass for unattached streams.** A stream no domain owns is blocked.
- **Per-image family selection.** Families are matched by discovery.

## 11. Charter amendments this work requires

- §3 lists `kernel/iommu/` and its crates (IOM0).
- §15.18 gains this plan's jump-sheet row (IOM0).
- §19's claim that the design forecloses unbounded DMA holds only where the
  platform has a unit; it is qualified to say so (IOM0).

## 12. Verification

- **IOM1:** DMAR fixtures built byte-for-byte, including malformed lengths,
  overlapping units, scopes through bridges, RMRRs naming several endpoints,
  and a table that claims more than it holds; coverage-rule and admission
  refusal tests in `lib/abi` and `kernel/core`.
- **IOM2:** the engine against a host frame source at every depth and leaf
  size, including maps that straddle a table boundary and unmaps that empty a
  table; the IOVA space's aligned, top-down, reach-bounded placement and its
  exhaustion as a value; the conformance suite over a model unit.
- **IOM3:** a register-level VT-d model (root/context walk, second-level walk,
  the invalidation queue, fault recording) the family is host-proven against;
  QEMU q35 with `intel-iommu` and `iommu_platform=on` on every virtio-pci
  device.
- **IOM4–IOM6:** kernel-core tests of the facility's untranslated and
  translated paths, the free ordering, revocation at death, the fault budget,
  and the fault service's routing, attribution, storm action and unrouted
  fallback over the register-level VT-d model; the model records a fault
  exactly as Intel VT-d rev. 4.1 §7.2.1 does, so a refused access, its
  decode, its charge and its attribution are proven end to end in host tests.
  `tairix-test-dma-translation-qemu-x86-64` boots the production kernel on q35
  behind an `intel-iommu`, every virtio function `iommu_platform=on`, and
  passes only on a key the autoloaded virtio-input driver delivered after the
  unit reported `translating` — the floor disk and the driver both reached
  memory through their domains, and the per-unit fault service is live
  throughout. `tairix-test-dma-fault-qemu-x86-64` closes the live-fault gap: on
  the same machine a bin-local PID 1 seam admits a misbehaving in-kernel
  virtio-blk driver that carves through its node's domain, confirms a mapped
  read, then points a device write at an unmapped address; the unit refuses it
  and raises its fault-event MSI, which the per-unit fault service drains into a
  `DmaTranslationFault` attributed to the device's node, with a canary page the
  write never reached — the MSI delivery the host model cannot exercise.
- **IOM7:**
  - **lib/pci host tests:** routing turns decoding on and leaves bus
    mastering as it was; a refused route writes nothing; an unchanged bit
    is not written; each helper changes only its own bit.
  - **Facility tests** over the reference model unit, with a fake port that
    records the stream's attachment at each change. They cover:
    - a grant only once the domain holds the stream;
    - a withdraw while it still does, before the block;
    - a predecessor's withdraw before its successor's grant;
    - `forget`, an unconfirmed end, a failed adoption and a firmware window;
    - a node removed while its first carve is made;
    - the stragglers stopped at take-over;
    - owners of one shared function ordered by when they began, not by
      generation.
  - **kernel/mem:** teardown ends a translated owner once, first.
  - **Syscall-layer tests:** an untranslated driver masters from its carve to
    its end, the bit written under the registry's lock; a driver never handed
    its function takes nothing back; a refused carve grants nothing; a
    translated one follows its domain; a kernel-owned port opens to no grant,
    and the refusal is recorded; a node the kernel drives admits no driver.
  - **The PCI host** (epochs, the record made under its lock, the take-over
    stop) and **the probe's stop rule** (an LPC bridge stopped, no host bridge
    and no type-1 bridge, and on a segment that cannot be confined every
    bridge) are host-tested, as are the flat scan's tally of what it stopped
    and what would not stop, and a unit given its node with no hierarchy and
    no firmware window.
  - **Live:** `tairix-test-dma-translation-qemu-x86-64` fails on a unit
    taking over a mastering function, or on a function granted before
    translation. It passes only on a key that arrives after the keyboard's
    node was granted, since QEMU delivers no MSI from a function that is not
    a bus master.
  - **Live:** `tairix-test-dma-fault-qemu-x86-64` ends its owner after the
    canary and passes only once the function reads back not mastering.
  - **The untranslated grant** is exercised by every untranslated x86_64
    vertical whose driver is autoloaded (`autoload_input`,
    `netstack_autoload`): without it the device can neither DMA nor
    interrupt. The harness verticals set the bit themselves (D579).
- **IOM8:**
  - **lib/pci topology tests:** aliasing through bridges to and from
    conventional PCI and nested ones; ACS on root and downstream ports; a
    switch with one port lacking ACS grouped whole, and an endpoint beside a
    switch's ports; slots with and without ACS; reserved port types;
    held-back and unassigned buses; every bus-number tree that is no tree
    refused. The walk over a writable mock turns ACS on and reads it back,
    and finds none through a port that ignores the write, a mechanism with
    no extended space, or a malformed extended list.
  - **ABI:** the alias and group facts round-trip, refuse non-canonical
    encodings, and cover only what the rules above allow.
  - **DMAR:** a firmware window is kept for every alias of its function,
    once.
  - **Facility:** aliases attached and blocked with the rest; one owner per
    group, the refusal audited; a forgotten owner freeing its group; the
    kernel's group; an unconfirmed end keeping its group; a node naming no
    group, two, or a stream on another unit carving nothing; a fault on an
    alias laid against its owner; mastering named by requester streams
    only; a refused adoption leaving attribution intact.
  - **Probe:** each function's stream, aliases and group; unconfinable groups
    (two units, more aliases than a node names).
  - **Live:** `tairix-test-dma-translation-qemu-x86-64` hangs its keyboard
    behind a `pcie-pci-bridge`: QEMU delivers its DMA under the bridge's
    alias, so the key arrives only if the alias is translated, and any
    translation fault fails the run. Group exclusion between two live
    drivers stays host-proven: which of two drivers carves first is a race,
    and a vertical that passes on whichever wins proves nothing.
- **The storm stays host-proven, not live.** QEMU's virtio device calls
  `virtio_error` and breaks on the first refused DMA, and the storm threshold
  sits above any VT-d fault ring, so a live storm would need ~512 device resets
  per one-second window — a load-dependent, flaky mechanism the charter forbids.
  The storm/silence/`Offline` path is proven against the register-level model
  (IOM4–IOM6 above); the live vertical proves the single MSI-delivered fault,
  which is MI0's exit criterion.
- **miri** enrols `kernel/iommu/api` and every family crate with an `unsafe`
  core.
