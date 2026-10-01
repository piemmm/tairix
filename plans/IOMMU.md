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
| IOM6 | Faults: drained in thread context from each unit's interrupt, stable audit events, a per-stream budget, a storm blocks the stream and marks the node `Offline` — the unit's recording, its batched drain and the budget are built; the fault event's MSI, the draining thread and the storm action remain | in progress |
| IOM7 | Default-deny from the first bus-master enable: units enabled before TAIRiX sets Bus Master Enable on any function; bus mastering follows ownership | planned |
| IOM8 | Isolation groups: requester-ID aliasing, ACS on the upstream path, multi-function devices without ACS, shared platform stream ids; the group is the unit of domain ownership | planned |
| IOM9 | PCI identity and extended configuration space: every function a node carrying its segment:BDF, the 0x100+ capability walk (ACS, ATS, PRI, PASID, SR-IOV), segment-aware ECAM | planned |
| IOM10 | ATS, PRI and PASID policy: ATS off at the device and refused at the unit; untrusted external-facing ports | planned |
| IOM11 | x86_64 interrupt remapping: VT-d IR (IRTEs, remappable MSI and IO-APIC entries, source-id validation) and x2APIC under EIM | planned |
| IOM12 | `kernel/iommu/amdvi`: AMD-Vi — IVRS discovery, the device table, command buffer, event log, its page tables and interrupt remapping tables | planned |
| IOM13 | The generic PCIe ECAM host bridge on aarch64 and riscv64 `virt` (`pci-host-ecam-generic`) — the prerequisite for every translated vertical off x86 | planned |
| IOM14 | FDT translation topology: `#iommu-cells`, `iommus`, `iommu-map` and `iommu-map-mask` in `lib/fdt` and the shared walk | planned |
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

8. **One owner per stream, attached lazily.** A stream belongs to at most one
   live domain. A parent that publishes a child for the same device (a bus
   driver and the controller it exposes) never carves, so the child's driver
   attaches the stream at its first carve; a second live owner is refused and
   its carve fails. IOM8 widens "stream" to "isolation group".

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
    IOVA spaces, stream tables and fault budgets are derived from the unit's
    reported capabilities and the discovered topology, and grow where the
    hardware allows (§24).

## 0a. The security position, stated honestly

| Attack | Without a unit | With a unit (after MI0) | After MI1 |
|---|---|---|---|
| A compromised driver points its device at another process's memory | open | closed: the device reaches only its node's domain | closed |
| A malicious device DMAs where it likes (Thunderclap, CWE-1257) | open | closed for translated streams | closed |
| A dead driver's device keeps writing into freed memory | held off by quarantine | closed: revoked before free | closed |
| DMA before the unit is enabled | open | open for functions TAIRiX made bus masters at boot | closed (IOM7) |
| Two functions behind one non-ACS switch reach each other peer-to-peer | open | open | closed (IOM8) |
| A device forges an MSI | open | open | closed on x86_64 (IOM11); IOM18 elsewhere |
| A device presents a pre-translated address (ATS) | n/a | refused: ATS never enabled | closed (IOM10) |

Residual, and named: a bug in a unit's family code (the TCB grew by it); a
unit erratum a family must work around; a platform with no unit; physical
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

Each unit's fault interrupt wakes one kernel thread that drains the unit's
records. A drain takes at most one ring's worth and says whether records
remain; the thread drains again while they do, because a unit raises no
interrupt for records it already holds (VT-d signals only when PPF sets). A
record names its stream, IOVA, access and reason; it is attributed
to the stream's node and recorded with a stable event id, rate-limited so a
device cannot flood the log. A stream over its budget is blocked, its node's
fault health set `Offline`, and the event recorded once. A fault on a stream
no domain owns (a firmware leftover, a device that lies about its requester
id) is recorded against the unit.

## 6. IOM7 — no window

The unit comes up before the first function TAIRiX makes a bus master, and
Bus Master Enable follows ownership: set when a node's owner attaches its
domain, cleared when the owner ends. The PCI routing helpers stop enabling bus
mastering as a side effect; the owner of a function's configuration space
enables it explicitly when it hands the function over.

## 7. IOM8–IOM11 — isolation groups, identity, ATS, interrupt remapping

- **Groups.** Two streams the fabric cannot keep apart — a conventional PCI
  device behind a PCIe-to-PCI bridge (the bridge's alias), a multi-function
  device without ACS, functions behind a switch port without ACS source
  validation and peer-to-peer redirect, or platform masters sharing an id —
  form one group, and the group is what one owner attaches. The bus driver
  that enumerates a subtree is trusted for that subtree's isolation facts,
  because it can rewrite them in configuration space; the kernel bounds it to
  the subtree its grant covers.
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
per I/O. Leaves are the largest the alignments allow, because a buddy carve is
naturally aligned and its IOVA is allocated at its own alignment. Teardown is
one confirmed sync per domain, not one per carve. Domain lookup per carve is a
hash probe under a per-facility lock, off every hot path; that lock spans a
unit's work only for an owner's first carve, where it serialises adoption and
ends a predecessor never revoked, while a death's or a removal's revocation
runs outside it. A unit serialises its own queue, and
every wait on it is bounded by the family's command budget. IOM20 adds
measurement-backed batching for streaming mappings.

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
  translated paths, the free ordering, revocation at death, and the fault
  budget; `tairix-test-dma-translation-qemu-x86-64` boots the production
  kernel on q35 behind an `intel-iommu`, every virtio function
  `iommu_platform=on`, and passes only on a key the autoloaded virtio-input
  driver delivered after the unit reported `translating` — the floor disk and
  the driver both reached memory through their domains. IOM6 adds the raw
  physical address that reaches nothing and the fault recorded against its
  stream.
- **miri** enrols `kernel/iommu/api` and every family crate with an `unsafe`
  core.
