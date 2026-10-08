# DMA translation: confining what a device can reach

A device that masters DMA reaches memory by address, below the MMU. Without a
translation unit between it and RAM, whoever programs the device can read and
write all of memory, and a dead driver's device keeps whatever addresses it was
handed. A DMA translation unit (an IOMMU: Intel VT-d or AMD-Vi on x86_64, an
Arm SMMUv3 on aarch64, the RISC-V IOMMU on riscv64, and on all three the
virtio-iommu a hypervisor offers) confines each device to the memory its
driver was given, and TAIRiX drives it as part of the
memory-isolation TCB. The staged design and the ledger of what remains is
`plans/IOMMU.md`.

## Topology in the hardware tree

Discovery states these facts, and only discovery states them:

| Fact | On | Says |
|---|---|---|
| `HwDeviceClass::Iommu` | a unit | a translation unit, its register window, keyed by its programming model (`compatible "intel,vtd"` or `"amd,iommu"`, or a device tree's own, `"arm,smmu-v3"` or `"riscv,iommu"`; a virtio-iommu `"virtio,pci-iommu"` when it is a PCI function, its windows the function's configuration windows, and `"virtio,mmio"` in a virtio-mmio slot) |
| `HwResourceKind::IommuStream` | a DMA master | it masters DMA through the unit at node `unit`, as streams `[first, first + count)` |
| `HwResourceKind::IommuAlias` | a DMA master | the fabric also delivers its DMA as these streams: a bridge re-tagged it |
| `HwResourceKind::IommuGroup` | a DMA master | its DMA cannot be kept apart from that of every other node naming the same `(unit, id)`; `id` is the members' least stream, unique on the unit |
| `HwResourceKind::IommuReserved` | a unit | stream `stream` keeps an identity window firmware still uses (a VT-d RMRR, an AMD-Vi IVMD), mapped only for the access firmware allows there |
| `HwResourceKind::Dma` | a unit | how its own DMA — its tables, queues and records — meets the CPU's caches (`HwResource::dma_coherence`), from `dma-coherent` / `dma-noncoherent` and the architecture's convention, and snooped for every x86 unit; a unit whose node does not state that it snoops is never taken over (`unsnooped`), and the devices behind it master no DMA |

On x86_64 the boot walks each PCI segment once (`tairix_pci::topology`):
every segment the ACPI MCFG describes through ECAM, over every region it
gives that segment, else segment 0 through mechanism #1. It parses the DMAR,
or on an AMD platform the IVRS, or under a hypervisor offering a virtio-iommu
the VIOT, emits one `Iommu` node per DRHD, IVHD or VIOT unit (carrying its
RMRR or IVMD windows), and gives each PCI function it publishes the stream its
unit knows it by — its requester id, resolved through the DRHD device scopes,
the IVHD device entries and the walk's bridge paths, or the endpoint id a
VIOT range numbers it as — with its aliases, those an IVHD names included,
and its group. An IVRS alias
that would put a function's DMA under another group's requester id leaves it,
and every function arriving under that id, unconfined. A function names a
stream only if its unit's node was emitted.

A VIOT range numbers its functions as endpoints from its first, each segment's
sixteen bits apart, and a virtio-iommu that is a PCI function translates the
range's functions but its own. A VIOT naming one function in two ranges, one
platform device twice, or one unit twice is refused whole, as is a range
running backwards, one whose endpoints overflow, or one whose unit is no unit
node. A virtio-iommu holds no firmware window: the regions it keeps out of a
domain it reports when an endpoint is probed, and each is a hole in the IOVA
space of every domain holding that endpoint. One that cannot be probed
(`VIRTIO_IOMMU_F_PROBE` not offered, or no room for a probe's properties) is
stranded, since nothing would then name the window its devices' interrupt
messages land at.

On aarch64 and riscv64 the device tree states the topology, read by the walk
every FDT port shares and by the kernel's generic PCI host bring-up:

- A node with `#iommu-cells` is a unit. A master's `iommus` names the
  streams it masters DMA as — a one-cell specifier is the stream id, the one
  form every family reads — and makes it a DMA master, carrying its bus's
  windows. A master whose `iommus` names only units that are not usable or
  whose specifiers carry no stream id masters untranslated. One whose list
  does not frame, that names a provider which is no describable unit, or
  whose DMA would reach memory both through a unit and around one, is given
  no DMA authority at all (`4087`, `reason=undescribed`).
- Platform masters sharing a stream are one group; one sharing a stream with
  a host's devices, or mastering through two units, is unconfined.
- A device the boot probes in a `virtio,mmio` slot masters DMA as the slot's
  node says: through the slot's streams, joining the slot's group; around
  every unit; or, for a slot whose DMA the walk could not describe, not at all
  — the device is not published. A slot whose node is not operational holds
  no device.
- A host's `iommu-map`, under its `iommu-map-mask`, maps each requester id
  to a unit and stream. An id the map leaves out is untranslated, as QEMU's
  virtio-iommu and riscv-iommu-pci leave their own functions; a map that
  does not decode, or names a target that is no unit, leaves the segment
  undescribed and publishes nothing.
- A master's `memory-region` regions with `iommu-addresses` are firmware
  windows: one mapping the region at its own address is kept on every stream
  of the master, on its unit; one the domain cannot keep — a window
  elsewhere, an I/O range only kept out of use, or past the room the unit's
  node has — leaves the master unconfined (`4087`, `reason=unkept_window`).

Only nodes whose `status` lets them be used are walked: a disabled, reserved
or failed node and everything below it is spliced out, so a unit owned by
another agent is never taken over and its masters are untranslated through
it. Function nodes are numbered per owned segment, and a node's address
is `(segment << 16) | requester id`; the walk finds alternative-routing (ARI)
functions past function 7.

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
such a device with `iommu_platform=on`. The kernel reads what a function offers
from its common configuration, mapped for the read; one that is not decoding
answers all ones and is refused alike. Devices on a platform with no unit are
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
  alias of its function. A virtio-iommu answering that it has no endpoint
  for a requester id leaves that id unattached; a node it has no endpoint
  for at all is not behind it, so its carves are refused rather than handed
  addresses the device would take as physical.
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
- **Unconfinable.** A function whose group spans units, whose aliases
  outnumber what a node can name, or that masters DMA as a stream a function
  of another group, or a master off its segment, uses too, is published to no
  driver (`4087`, `reason=unconfinable`).

Grouping cannot stop a device below a port without Source Validation from
presenting another's requester id, and peer traffic between root ports is
taken on the root complex's word: the PCI Express specification requires ACS
on a root port that routes it.

## External-facing ports

A port that can take a device plugged in later — a slot whose Slot
Capabilities say Hot-Plug Capable — is external-facing, and every function
below it untrusted: firmware's reserved windows are never kept for it, and it
is published only if the port validates requester ids (ACS Source
Validation), so a device there cannot pass as another. One that cannot be
told apart is refused (`4087`, `reason=untrusted`).

## Address translation services

A device with an address translation service presents addresses the unit
does not translate. The walk turns off each confined function's ATS, page
request and PASID, and its SR-IOV virtual functions, and the unit is told to
refuse translated requests (VT-d context entries translate untranslated
requests only; an AMD-Vi device entry leaves its IOTLB off), so a device that
ignores the write faults (`reason` `translated`). A device left with any of them on is logged.

## Interrupt remapping

An MSI is a DMA write: without remapping, a device that can write memory
raises any vector at any CPU. Where every translating unit can remap and
firmware says the platform supports it (the DMAR's `INTR_REMAP`; AMD-Vi remaps
wherever it translates), the kernel
gives every interrupt source the boot set up an entry on the unit that sees
its messages, then turns remapping on, before any interrupt is taken:

- each IO-APIC pin, raisable only by the IO-APIC's requester id from the DMAR
  scope that names it, keeping its trigger mode and vector;
- each PCI function the probe routed, raisable only by the requester ids its
  messages reach the unit as: its own, or a bridge's bus range below a
  PCIe-to-PCI bridge;
- every later route — the floor disk's, a vertical's — through the same
  entry-or-compatibility choice: an entry where a remapping unit sees the
  function, a compatibility message where none does.

From then on a compatibility-format interrupt, one from a source its entry
does not admit, and one naming no entry are refused and reported as faults.
A unit's own fault interrupt is never remapped.

An AMD-Vi unit finds an interrupt's entry by the requester id it arrives as
alone, through that id's device table entry, so each source has a table of
its own and every id it covers points at it: a bridge's buses share one, and
any other id's interrupts are refused. Its message data names the entry. An
IO-APIC pin keeps a compatibility-format redirection entry naming its entry
in the vector field. A remapped interrupt reaches its CPU edge-triggered, so
no end of interrupt returns to the pin: the IO-APIC clears a level pin's
remote IRR through its EOI register (by passing the pin through edge
triggering, before version 0x20) when the pin is re-armed. Only fixed
interrupts pass a unit: the INIT, ExtINT, NMI, LINT and system-management
pass-through IVRS offers a device is never applied, so no device can reset,
stall or interrupt a CPU outside its own vectors.

The machine remaps whole or not at all. An IO-APIC no unit names, a unit that
cannot remap, or a unit that refuses to turn remapping on leaves every source
in compatibility format, every unit that had turned it on turning it back off;
a unit that cannot is reported `stranded`. A function whose requester ids are
unknown, or whose unit refuses it an entry, is left unrouted and counted.

A virtio-iommu remaps nothing, so a machine translating through one keeps
compatibility delivery and says so (`unremapped`); its message window is a
region it reports reserved, never mapped into a domain.

The local APICs enter x2APIC mode only alongside remapping in extended mode,
which names their 32-bit destinations: where the CPU has it, the DMAR does not
opt out, and every unit supports extended interrupt mode (VT-d's EIM, or
AMD-Vi's XT through 128-bit entries). An APIC firmware
left in x2APIC mode stays there. A CPU is found by its dense id, never by its
APIC id's width, so in x2APIC mode a CPU may have any id but the broadcast
one; a compatibility-format interrupt still names only the eight bits xAPIC
does, so a CPU past them is reached through remapping or not at all.

## MSI isolation through a GICv3 ITS

On aarch64 a GICv3's interrupt translation service does what remapping does on
x86_64. Each interrupt-driven PCI function the boot probe takes raises its
MSI-X as an LPI of its own, through the service its host's `msi-map` (else
`msi-parent`) names: its DeviceID is that map's image of the requester id its
messages carry, its EventID the next its DeviceID has, so functions sharing a
DeviceID behind a bridge take its events in turn. The function's MSI-X entry
is written with the service's `GITS_TRANSLATER` and its EventID while it
cannot yet master. A function no service is named for, or whose MSI-X refuses
the write, raises its INTx line instead.

Once the GIC is up and frames exist, the boot CPU's redistributor is given LPI
tables covering every route, and each service its command queue, device and
collection tables and an interrupt translation table per device; every route
is mapped at once, before any driver is admitted, and the outcome is the
`InterruptRemapping` record below. The service raises an LPI only for the
DeviceID and EventID it was mapped for, and the DeviceID is the fabric's, so a
device writing an event it was not given — another device's included — raises
nothing.

Behind a translation unit the message is a write the unit translates like any
other. Each such function's node therefore carries its doorbell
(`HwResourceKind::MsiDoorbell`: the translation frame's first page, never the
control frame), and its domain maps it at its own address, write-only where
the unit's tables can grant a write alone (`UnitProfile::write_only`: SMMUv3
stage 2, a virtio-iommu) and readable too where they cannot (SMMUv3 stage 1),
the page holding no readable register. No bus driver can publish a doorbell.

An LPI line has nothing to mask: the message is one edge, and the waiter
taking its fire is the interlock, as for an x86_64 MSI.


## MSI isolation through interrupt files

On riscv64 a message is a write of an identity to a hart's IMSIC file, and a
device that could write the file could raise any identity in it. The RISC-V
IOMMU's MSI page tables stop it: each PCI function whose host sends its
messages to the boot hart's file, behind a unit with MSI page tables, file-mode
entries and a second stage, is given a 512-byte memory-resident interrupt file
of its own (`IommuUnit::message_files`). Its MSI-X entry writes identity 1 to
the hart's file page; the unit takes that write as a message, sets the identity
in the function's own file, and then raises the file's notice — an identity of
the hart's file the AIA controller gave it — in the hart's file. Whatever
identity the function writes lands in its own file, so it raises its own
vector or nothing.

The doorbell page is kept clear of carves in the function's domain and mapped
in none, the unit intercepting the write; the hart's file is outside every
domain. A vector is enabled in its file only while its line is armed. A unit
that cannot confine messages — no second stage, or no file-mode entries — gives
its functions their wired lines alone, the probe offering them no message
route. A unit that sets a file's bits without atomics, QEMU's among them, can
bring back a bit the kernel cleared: a repeated interrupt, never a lost one.

## Domains and ownership

A node is **translated** when its stream names a unit the kernel brought up.
Its driver instance, admitted with a generation, gets a domain at its first
carve: an I/O page table holding the node's streams and the firmware windows
they keep. Every carve maps there, at an IOVA below the grant's addressing
limit narrowed to the reach the driver states for its device; the frames
themselves may lie anywhere. A carve is its pages, drawn as the largest free
blocks first and mapped end to end in one naturally aligned IOVA block, so
each block's leaves are as large as its alignment allows and a carve is
bounded by RAM and the DMA window rather than by the largest buddy block.
`dma_alloc` and `shm_create_dma` hand the driver that IOVA, never a physical
address. No IOVA lies where a PCI segment decodes memory: a root complex may
route a device's request there to a peer before its unit sees it. At boot the
probe reads each segment's root-bus memory BARs and what its root-bus
bridges forward (`PciTopology::decoded_windows`), and every domain keeps
those addresses out, widened to whole pages. This is the same rule Linux
applies to a host bridge's windows, read from the segment itself, so no port
needs firmware's description of them.

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

A carve's free confirms its own range rather than the domain
(`IommuUnit::sync_range`): VT-d page-selectively where one address mask covers
it, AMD-Vi with one span command, an SMMUv3 and a RISC-V IOMMU a page at a
time up to half a command ring, and the domain whole past that or where no
selective form reaches a removed table. Every invalidation that may free a
table reaches the walk caches above the range; a range sync releases only
the retired tables whose span touches its range, and a domain-wide one every
table retired, so the domain's other carves keep their cached translations. Until
it is released a retired table is still reachable from a walk cache through the
entry it was unlinked from, so a map needing a table there relinks it, and no
large leaf is laid over that entry. A unit that
caches the entries it found absent — VT-d in caching mode, AMD-Vi where its
capability header leaves `NpCache` set or cannot be read — is flushed once a
carve, however many blocks it spans, and an AMD-Vi attach flushes the
device's entry alone.

Frees are batched behind one invalidation. A carve its live owner frees
leaves its domain's tables at once, but keeps its IOVA, and its frames wait
with the carves freed near it until one invalidation of each domain holding
them confirms them all gone; only then are they scrubbed and freed. The batch
is confirmed once it holds 64 carves or its first has waited 10 ms, and holds
back at most 1/256 of memory. A carve is given room in the batch before it
leaves its tables, so its caller never unmaps an IOVA another carve may hold
by then; one with no room is confirmed alone. One kernel task confirms
batches, asleep until the batch's window closes or the batch fills; until it
has proved it can park, every free is confirmed alone, and a task the
scheduler refused or that cannot park is audited (`DmaFreesUnbatched`).

A unit's invalidations for different domains overlap. A batch is planned
under its domain's lock, then handed to the unit's command queue and waited on
holding neither that lock nor the unit's map of domains: only the queue's own
lock is held, and only while commands are written, so one device's syncs wait
on another's only for room in the shared ring, and never on a stream being
attached. A command the unit rejects fails the batch that held it and no
other, and a table an unmap emptied is freed only once the batch confirming it
is done. A virtio-iommu is the exception: its device answers one request at a
time, as Linux drives it too, so every map and unmap on the unit waits its
turn for the request in flight.

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
- **Bridges open at a grant.** A bridge the kernel finds closed forwards
  nothing upstream until a function below it is first granted mastering,
  when every bridge above that function opens; on a host whose resources the
  kernel assigns, every bridge starts closed. A bridge that refuses leaves the
  function's DMA short of memory; none is closed again, the function's own
  bit being what stops it.
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

Each unit's fault interrupt — one the kernel takes for itself and hands to no
process: a VT-d unit's fault event, an AMD-Vi unit's own PCI function's MSI, an
SMMUv3's wired line its node names (`combined`, else `eventq`), set
edge-triggered once it is bound to the kernel alone, so no other owner's
trigger changes under it, else its message, and a RISC-V IOMMU's first
wired line, every cause's vector set to it and its counters stopped, else its
message, chosen as the unit is taken over, and a virtio-iommu's event queue
interrupt — its function's INTx where the host's bring-up resolves the line,
else its MSI-X, or its slot's line — wakes one kernel task, which drains the unit's fault records (an AMD-Vi unit's
event log, an SMMUv3's event queue, a RISC-V IOMMU's fault queue, a
virtio-iommu's fault reports) and parks again. A
RISC-V record does not say whether nothing was mapped or the mapping refused
the access; the device's domain tells them apart. A unit nothing can hear —
a virtio-iommu function with no MSI-X on x86_64, whose INTx only ACPI's `_PRT`
routes (`plans/ACPI.md` A5) — translates with its faults unrouted
(`faults_unrouted`, `reason=no_line`). A record names its stream, the page, the
access and the reason. It is recorded against the node whose owner holds the
stream (an owner revoked but still recorded included), or against the unit for
a stream no owner holds.

Faults are budgeted per window of one second: each stream may have four
recorded and the unit thirty-two, the rest only counted and reported with the
next record. A stream that raises 1024 in a window, twice the deepest fault
queue a family keeps, is silenced — blocked, its faults no longer recorded —
and its node marked `Offline`. The storm's record is charged to the unit's
thirty-two like any other, so a unit whose share is spent contains the storm
without recording it. A silenced stream takes an owner again at its node's
next driver. A unit is drained at most 1024 times a window; a storm past that
waits out the window, so a device cannot hold a CPU or the interrupt line.
The fault interrupt is the kernel's alone: no process may bind it, nor share
it as a wired line. A unit whose service cannot start, or can no longer wait
on its interrupt, is told to stop raising it, and only then is its vector
returned to the port's pool. One that will not stop keeps its vector, since the
vector's next owner would hear the unit. A virtio-mmio slot's line cannot be
stopped at all.

Silencing is each family's strongest suppression: an AMD-Vi device table
entry suppresses only page faults, so a silenced device's other refusals
still reach the log, inside the same budget; a virtio-iommu cannot be told to
stop reporting, so its family drops a silenced endpoint's reports.

The live path is proven on QEMU q35 behind an `intel-iommu` and an
`amd-iommu` by `tairix-test-dma-fault-qemu-x86-64` and its AMD binary: a
misbehaving in-kernel virtio-blk driver carves through its node's domain,
reads the sector the runner planted back through it, then points a device
write at an unmapped address. The unit refuses it and raises its fault
interrupt, which the per-unit fault service drains into `DmaTranslationFault`
against the device's node, with a canary page the write never reached. QEMU's
AMD-Vi model writes neither the access direction nor the address where the
specification puts them, so that run's fault reads as a read of address 0.
The budget's storm/silence path is proven against the register-level models,
not live: QEMU bounces a mapping its unit refuses and completes the request,
so a 1024-fault window would need as many request round trips inside one
second, a load-dependent test.

## Audit

| Event | Id | When |
|---|---|---|
| `DmaTranslationBypass` | 4087 | discovery refused a function behind a unit that it would not use (`reason=bypasses_unit`) or that could not confine it (`reason=unconfinable`), with `address` and `unit`; or a platform master no group confines (`unconfinable`, `unkept_window`) or that was given no DMA authority (`undescribed`), with `node` and `unit` |
| `DmaTranslationFault` | 4088 | a unit refused an access; `unit`, `stream`, `iova`, `access`, `reason`, `suppressed`, and `node` where an owner holds the stream |
| `DmaTranslationStorm` | 4089 | a stream stormed: silenced, its node `Offline`; the fault's fields and `outcome`, where the unit's share of the window holds a record |
| `DmaTranslationUnit` | 4094 | boot brought a unit up (`outcome=translating`, with its `stage` — `second`, `first`, or `kept` for a unit keeping its domains' translations itself — and `stopped` and `refused`, the functions behind it found mastering as it took over without a firmware window that were stopped and that would not stop) or stranded it, translating nothing and every function behind it told to stop (`unmatched`, `no_registers`, `unsnooped`, `exhausted`, `unconfirmed`, `hardware`, `refused`, with `stopped` and `refused`); `faults_unrouted` with a `reason` when its faults cannot be served (`unactivated`: the interrupt controller has nothing to give its wired line; `untriggerable`: it cannot sense the line as the tree states; `no_line`: no line the platform describes and no message the unit can raise) |
| `DmaUnitsMalformed` | 4104 | the units' firmware table was malformed; `family` (`vt-d`, `amd-vi`, `virtio-iommu`) and `outcome`: `withheld` (no PCI function published) or `unconfined` (published untranslated, as `iommu.malformed=unconfined` asked) |
| `DmaTranslationUnconfirmed` | 4095 | a unit could not confirm a translation ended — a driver's domain, a removed node's, or one carve's; `node`, `generation` |
| `DmaBusMaster` | 4147 | the kernel turned a function's bus mastering on as its owner began or off as it ended, recorded in the order the changes landed; `node`, `generation`, `master` (`on`/`off`), `outcome` (`applied`, or `refused` where the function reads back otherwise) |
| `DmaGroupRefused` | 4148 | a driver's first carve was refused: another node's live owner holds its isolation group; `node`, `generation`, `group`, `holder` |
| `DmaFreesUnbatched` | 4250 | no task confirms freed carves in batches, so each free waits for an invalidation of its own; `cause` (`no_room`, `not_admitted`, `cannot_park`) |
| `PortIoRefused` | 4158 | a driver's port access reached the ports the kernel keeps (PCI configuration mechanism #1); `port`, `width`, `task` |
| `InterruptRemapping` | 4159 | the port routed the interrupt sources it set up at boot: `outcome` `remapped` (every source raises only the entry, LPI mapping or interrupt file the kernel wrote), `unremapped` (no unit, or one that cannot remap: a device can forge any interrupt), `refused`, `stranded` (a unit could not turn remapping back off and refuses its devices' interrupts), or `unrouted` with the `unrouted` count |

A hardware tree the kernel cannot read, or units it cannot record, fail the
boot (`phase = "irq"`, `cause = "dma_translation_unbuilt"`) rather than leave
every device untranslated. A unit whose registers overlap RAM is refused
(`no_registers`), so the kernel never drives frames it hands out as a unit.
A malformed DMAR, IVRS or VIOT is logged at boot (`4103`) and refused whole.
The machine has units, and none of them can be read, so any segment may sit
behind one: every segment is stranded. Each function that masters DMA, and
each bridge, is told to stop, and none is published. The boot setting
`iommu.malformed=unconfined` on the kernel command line publishes them
untranslated instead, as on a machine with no unit. Either way the outcome is
audited (`4104`) and `sysinfo dma` lists the table's family as `withheld` or
`unconfined`. A segment that cannot be confined (unit nodes that could not be
emitted, or a walk that forms no tree) is logged under `4103`, with the flat
scan's `stopped` and `refused`, and publishes nothing.

## The administrator's view

`sysinfo dma` lists the translation state through the System Information
API, behind `CAP_SYSINFO_HW` and audited: each discovered unit — its family,
whether it translates or why not, how it raises its faults, where its
translations live, the owners and firmware streams it holds, and its faults
recorded, dropped past its budget, and the streams a storm silenced since
boot — then each isolation group an owner holds, and each node a unit
translates for an owner with its owner's generation and standing and what
its domain maps (`DMA_UNITS`, `DMA_GROUPS`, `DMA_NODES`; [the sysinfo
ABI](../abi/sysinfo.md)). The kernel reads each owner's state under that
owner's lock alone, one owner at a time, so a listing holds nothing an
adoption or a carve needs and waits only on the owner it is reading.

## What remains

Taking over a VT-d or AMD-Vi unit that firmware left translating turns its
translation off until the kernel enables it again, so DMA passes untranslated
in between (`plans/OPEN-DEFECTS.md`). A virtio-iommu answers one request at a
time, so a run of maps costs a round trip each rather than one a batch. A
unit no
family drives is left as firmware set it (`4094`, `outcome=unmatched`) and
strands the devices behind it, which master nothing ([Constructing the
real-hardware bus](../drivers/bus.md#constructing-the-real-hardware-bus)). An
SMMUv3's and a RISC-V IOMMU's live fault delivery are proven against their
register models, not yet on QEMU.
