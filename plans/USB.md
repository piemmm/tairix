# USB.md — Modular USB stack and device hot-removal

This is the staged build plan for TAIRiX's **modular USB stack**: a clean
split between the host-controller driver, the per-device class drivers, and
the device manager's hot-removal reaction. It is the continuation of the
Design-D device-manager work in `plans/PI.md` (the user-space autoload chain
and the `hw_emit_node` / `hw_remove_node` syscalls) and the layering the §17
modularity contracts require.

`AGENTS.md` is binding — read it and `PLAN.md` first. Every rule in this file
is binding too.

**Note:** `abi-v1` is *not* frozen, despite what `AGENTS.md` / `PLAN.md` say —
the standing task direction supersedes that language. Changing a `lib/abi`
type today is allowed; it requires regenerating the C header
(`cargo xtask c-header --write`), which the drift guard enforces.

## Ledger

| Item | What it is | Status |
|---|---|---|
| U1 | Kernel driver-unload mechanism and the `devmgr` unload reaction | done |
| U2 | URB transport ABI and the `lib/usb` transport server/client | done |
| U3a | Per-endpoint URB-transport grant mechanism | done |
| U3a2 | Cross-process shared-memory primitive | done |
| U3a3 | Wait-set multi-event wait primitive | done |
| U3b | The xHCI HCD process | done |
| U4 | Class drivers bind interface nodes and reach their device only over the URB transport | done |
| U5 | Event-driven hot-plug, re-enumeration, and a controller-fault recovery that keeps the devices that come back | done |
| U6 | Broken-device isolation: one device's failure never costs another its service | done |
| U7 | Composite (multi-interface) devices | done |
| U8 | EP0 max-packet discovery and exact-length descriptor reads | done |
| U9 | Multi-tier hubs (a hub plugged into a hub) | done |
| U10 | Each node gets a fresh shared buffer; a reused endpoint refuses the previous node's driver | done |
| U11 | The host controller is class-neutral: raw interrupt-IN reports sized by the class driver, a one-page control data stage, a control-request policy scoped to the node's own interface, and the interface number on the node (`plans/HID.md` H4) | done |
| U12 | A failed endpoint recovery — a device's interrupt-IN endpoint, or a hub's status-change watch — is decided by its settled port: recovered in place where the device is only disturbed, freed where the port is vacant, and the port reset and enumerated afresh where neither holds | done |
| U13 | A bulk transfer is bounded and its pipe always recovered: every bulk URB carries a deadline the HCD abandons it at, every halting completion is recovered in place, and a class driver clears a pipe's halt through `ClearHalt` with both data toggles reset | done |
| U14 | The engine never waits: every control transfer, command, port reset, enumeration and recovery is an operation driven by routed events and one-shot timers, so one device's slow pipe holds up no other | done |
| UM | Live metal acceptance of attach, detach, re-attach, cold boot unplugged, a controller reset under a live keyboard, a transaction-translator glitch ridden out in place, and a stick whose transfer fails mid-command recovering in place, with `usb_hid` serving a keyboard, a mouse and a touchpad | blocked: the operator's Pi 4 run (QEMU models no Pi USB) |

---

## 0. Why the controller and the class are separate processes

A driver that binds the `usb,xhci` controller node **directly** and performs
the *whole* job folds two distinct responsibilities into one process:

1. maps the controller's register BAR, owns its DMA rings and root-hub ports,
   brings the xHCI controller up, and enumerates the attached device; **and**
2. decodes HID boot-keyboard reports and injects keystrokes.

Step 1 is a **host-controller driver** (HCD) responsibility — it is per
*controller*, identical for every device behind that controller. Step 2 is a
**class driver** responsibility — it is per *device*, and a keyboard, a mouse,
and a mass-storage device each need a different one. Fusing them into one
process means:

- **No modularity (§17).** A second USB device behind the same controller has
  nowhere to bind: the single process already owns the controller. Supporting
  a USB mouse alongside the keyboard would mean a second process re-opening
  the same controller registers — two masters on one xHCI, which has no
  defined ownership model and is unsafe.
- **Hot-removal has no clean shape.** When the keyboard is unplugged the
  controller node is still present (the controller did not vanish), so there is
  no per-device node for a watcher to remove, and removing the controller node
  would tear down the controller driver itself.
- **A bus↔class hardwiring risk.** The keyboard driver knows it sits behind an
  xHCI controller. A class driver must not know *which* controller (or bus —
  the same device class can appear behind a different host controller, or on a
  non-PCIe transport), and a controller driver must not know *which* class
  drivers exist. TAIRiX may carry several controller drivers and several class
  drivers; none may be wired to a specific sibling.

`drivers/bus/usb/xhci` is that host-controller driver, and every class driver
binds an interface node it publishes.

---

## 1. Target architecture (binding)

Three independent layers, each reached only through discovery and the public
ABI — never by naming a sibling crate (§17.4, §2.20):

```
            hardware tree (lib/abi hwtree)            URB transport IPC (lib/abi)
                  │  emits / removes nodes                  │  submit / complete
  ┌───────────────┴───────────────┐         ┌──────────────┴───────────────┐
  │  Bus driver(s)                 │         │  USB host-controller driver   │
  │  drivers/bus/pcie_brcm, …      │  emits  │  (HCD) drivers/bus/usb/xhci   │
  │  trains the link, enumerates   │ ──────▶ │  owns ONE controller:         │
  │  the controller function,      │ usb,xhci│   registers, DMA rings,       │
  │  emits the controller node     │  node   │   root-hub ports, enumeration │
  │  with its BAR + DMA + IRQ      │         │  emits ONE node per attached  │
  │  grants. Knows nothing of USB. │         │  USB interface; serves URB    │
  └────────────────────────────────┘         │  IPC; watches PORTSC.         │
                                              └──────────────┬───────────────┘
                                                             │ emits / removes
                                                             │ per-interface node
                                              ┌──────────────┴───────────────┐
                                              │  USB class driver(s)          │
                                              │  drivers/input/usb_hid,       │
                                              │  drivers/storage/usb_msd, …   │
                                              │  binds an emitted interface   │
                                              │  node, submits/reaps URBs over│
                                              │  the transport IPC. Touches   │
                                              │  NO controller register, knows│
                                              │  no bus/controller identity.  │
                                              └────────────────────────────────┘
```

### 1.1 The host-controller driver (HCD)

- Lives in `drivers/bus/usb/xhci` as a user-space `Run` binary (it is a driver,
  not a `lib/*` crate; the bus-agnostic xHCI *protocol* engine stays in
  `lib/usb`, §2.22). It binds the `usb,xhci` node a bus driver emits by its
  `compatible`, and a function the kernel discovered on a PCI host it owns by
  the xHCI class code, below any driver naming the exact part.
- It is the **sole owner** of one controller: its register BAR, its DMA command
  / event / transfer rings, and its root-hub ports. No other process maps that
  BAR. This is the ownership model that makes "two processes touching one
  xHCI" a non-problem: only the HCD ever does.
- On attach it enumerates the device (address, descriptors, configuration) and
  **emits one hardware-tree node per USB interface** through `hw_emit_node`,
  carrying USB match keys (`vid:pid:class:subclass:protocol`) so the device
  manager can match a class driver — and carrying **no** controller register
  grant. The interface node's only "resource" is the right to talk to the HCD's
  URB transport endpoint for that interface (see §1.3).
- It **watches the root-hub PORTSC** event-driven (xHCI Port Status Change
  events on its event ring; never a busy-poll, §2.23). On a disconnect it calls
  `hw_remove_node` on the affected interface node(s); the device manager reacts
  by unloading the bound class driver (§1.4). On a fresh connect it enumerates
  and emits, and the device manager autoloads the matching class driver — so
  re-plug works with no reboot (§18.4).
- It knows nothing of PCIe, the VL805, or any board: it received its register
  window and DMA constraint as grants on its matched node.
- **It is a single asynchronous event loop, never a per-URB blocking server
  (§2.23).** The HCD services two independent event streams — URB-submit
  IPC calls on the per-interface endpoints it serves, and its controller's
  interrupt — for arbitrarily many interfaces (the USB-mouse-alongside-keyboard
  case, §0). It parks on a kernel **wait-set** (`U3a3`) holding both, and every
  wake runs one service step of the engine (U14): every event the controller
  posted is routed, and every operation an event or a due timer moved runs.
  A call is answered at once only when it finishes at once; otherwise it is
  held and answered when the engine finishes it. The park is bounded by the
  earliest deadline an operation, a held URB or a retry owes, and is
  unbounded when none is; an idle controller burns nothing.

### 1.2 The class driver

- A class driver binds an emitted USB-interface node by its class key — never
  the controller node. Every HID interface is served by the one HID class
  driver, `drivers/input/usb_hid`, which reads and parses its own report
  descriptor and serves every application on the interface (`plans/HID.md`).
- It owns **no** controller register and carves **no** controller DMA. It
  submits control and interrupt-IN transfers (URBs) for its interface and
  reaps completions over the URB transport IPC (§1.3), injecting what it
  decodes through the seat's inject syscalls.
- It knows neither the controller type nor the bus: the same binary would work
  unchanged behind a different host controller, because it speaks only the
  bus-agnostic URB transport ABI.

### 1.3 The URB transport IPC (the seam)

- A versioned, capability-checked ABI in `lib/abi/src/usb_urb.rs` (held to
  the syscall-table discipline, §9). A request is one of:
  - a **URB** (USB request block) — endpoint address, transfer type
    (control/interrupt/bulk), direction and length, its data always in the
    node's shared buffer — answered with a typed completion: status, bytes
    transferred. Every bulk and control URB carries the class driver's
    deadline; the HCD holds a control URB no longer than
    `USB_CONTROL_TIMEOUT_MAX_MS` (5 s, USB 2.0 §9.2.6.4) whatever it asks, and
    an interrupt URB may carry none, to wait for the next report;
  - an **interface operation** — select an alternate setting, which reserves
    bus bandwidth and so is the HCD's alone, clear a pipe's halt, or claim a
    sibling interface the device publishes no node for (a streaming interface
    a control interface governs). One the engine runs over several commands
    is held as a URB is;
  - an **isochronous stream** operation — start, queue a slot, stop — over a
    region of its own the HCD grants the caller, each finished slot reported
    to the notify port the caller's attested pid names (`plans/SOUND.md`
    SND6).

  None carries a controller detail.
- The HCD serves a URB transport **call endpoint** per interface it emits
  (`lib/usb` gains the controller-side "transport server" and the class-side
  "transport client" over this ABI; the multi-device enumeration
  engine is owned by the HCD, not the class driver). The interface node the
  HCD emits names the endpoint id the class driver connects to as its sole
  "resource" — so the capability to submit URBs for an interface is minted
  kernel-side from the matched node, never ambient (§4, §5.4).
- Data buffers are shared-memory IPC objects (§4): the class driver writes/reads
  its own buffer; the HCD maps it for the DMA transfer. No class driver ever
  sees a controller register or another interface's buffer.
- Every URB is validated by the HCD before it touches a ring (endpoint belongs
  to this interface, length within the buffer, direction legal); it fails
  closed (§5.4). Parsing the standard descriptors during enumeration
  (device, configuration, interface, endpoint, hub) is the HCD's job and is
  bounded/fail-closed (it already is, in `lib/usb`); a class-specific
  descriptor, such as a HID report descriptor, is its class driver's.

### 1.4 Device manager hot-removal reaction

- `devmgr` records, for each driver it loads, the `node_id` it bound to (it
  already learns this when it issues `StoreRequest::Load { bundle_id, node_id }`).
- On a hardware-tree generation bump (`hw_tree_wait` wakes it), it diffs the
  live tree against its bound set. A bound node that has **disappeared** means
  the device is gone; the manager unloads that driver through a new kernel
  **driver-unload mechanism** (§1.5), drops the binding record, and logs the
  unbind with a stable event id (mirror of the load log, §19.4).
- This reaction is generic: it fires for *any* vanished bound node, whatever
  emitted it (the HCD's port-watch, a future bus hot-remove, manual teardown).
  It does not know about USB.

### 1.5 Kernel driver-unload mechanism

- The driver-store server (`StoreRequest`) gains an `Unload { handle }` opcode
  (the symmetric partner of `Load`). The kernel tears the driver process down:
  reclaims its grants (MMIO maps, DMA regions, IRQ bindings, served endpoints),
  removes it from the spawn/loader registry, and emits the unbind audit event.
  Teardown fails closed and is idempotent (unloading an already-gone handle is a
  benign `NotFound`, never a panic, §2.9).
- Capability-gated exactly as load is (`CAP_DRV_LOAD`); the device manager owns
  *policy* (which handle, when), the kernel owns the *mechanism*. No ambient
  authority (§4).

---

## 2. Why this is secure, modular, and reviewable

- **Least privilege per layer (§5.4).** The class driver holds no controller
  register grant and no DMA grant — only the right to submit URBs on its one
  interface. A compromised keyboard driver cannot reprogram the controller,
  reach another device's buffers, or touch the bus.
- **One owner per controller.** Exactly one process maps a given xHCI BAR, so
  there is never a defined-ownership gap or a DMA-ring race between processes.
- **No sibling hardwiring (§2.20, §17.4).** The HCD names no class driver and
  no bus; class drivers name no controller and no bus; bus drivers name no USB.
  Every edge is discovery (`hw_emit_node`) + match (`lib/devmatch`) + the public
  URB ABI. Adding a USB device class is a new class-driver bundle and zero
  edits elsewhere; supporting USB behind a different controller is a new HCD
  bundle.
- **Hotplug is structural, not bolted on.** Connect → enumerate → emit →
  autoload; disconnect → `hw_remove_node` → devmgr unload. Both directions use
  the same generation-bump path the device manager already runs (§18.4).
- **Fail-closed throughout (§5.4, §2.9).** URB validation, descriptor parsing,
  unload teardown, and the port-watch all reject/deny on error; none panics.

---

## 3. Staged increments

Each increment ends green on the **whole-project** validation gate (§7). Live
USB attach/detach is metal-only on the Pi 4 (QEMU cannot model the VL805,
`plans/PI.md` §0.4), so the increments are ordered so that everything *except*
the live controller behaviour is host- and CI-proven first. QEMU's own
`qemu-xhci` runs this driver end to end (the `plans/SOUND.md` SND7 audio
verticals), which proves enumeration and serving on a generic controller but
nothing of the VL805's own behaviour.

- **U1 — kernel driver-unload mechanism + devmgr unload reaction.**
  `StoreRequest::Unload { handle }` (+ status-only reply) lives in
  `lib/abi/src/driver_store.rs`; the endpoint request cap is now
  `MAX_REQUEST_LEN`. The kernel teardown is the symmetric partner of
  `spawn_driver_process`: `InitSpawnCtx::terminate_driver_process(handle)`
  (kernel/core, implemented by `KernelInitSpawner`, which gained the IRQ-table
  borrow) reaps the driver's scheduler task (`scheduler.exit` drops its
  control block → kernel stack, live address space, page-table frames), then
  withdraws its address-space-registry entry (grants/streams/limits/matched
  node), destroys its served endpoints (+`call_wake`), releases its IRQ
  bindings, and drops its capability record — idempotent, fail-closed
  (`NotFound` for an already-gone handle), audited (`AuditEvent::DriverUnloaded`,
  4033). The bin reaches it through `DriverProcessSpawn::terminate_driver` and
  the driver-store server serves `StoreRequest::Unload` via `unload_reply`. In
  `devmgr`, `AutoloadState` gained a `node_id → NodeDriver{bundle_id,handle}`
  binding map (recorded in `match_and_load`); `service::react_once` runs
  `autoload::unload_vanished` after each match pass, tearing down (via
  `store::unload_driver`) any bound node absent from the new snapshot — only
  when its driver's *last* bound node has vanished — and purging the
  loaded-bundle cache so a re-attach reloads (`NODE_UNLOADED`, 13_008). Covered
  by host unit tests (ABI round-trip/fail-closed; kernel teardown against the
  real `KernelState` registries; server serve; store unload round-trip;
  `unload_vanished` vanish/present/shared-last-node/fail-soft; service-level
  vanish→unload, no-vanish→none, vanish-then-reattach→reload) and the
  `tests/integration/driver_unload_qemu_aarch64` `-M virt` vertical
  (autoload → `terminate_driver_process` → assert live-task count 1→0 + caps
  /aspace reclaimed + idempotent `NotFound`). Whole gate green.
- **U2 — the transport ABI and `lib/usb`'s transport server and client.**
  `lib/abi/src/usb_urb.rs` carries `UsbRequest` — a URB, an interface
  operation or an isochronous stream operation, each decoded only from its
  exact canonical frame — with the status-framed completion, a stream's
  `IsoGrant` and its `IsoNotify`. A driver-to-driver format, so not part of
  the C-header surface. `lib/usb::transport` holds the controller-side
  `UrbEngine` seam and `drive_urb` (validate fail-closed against the node's
  `UrbScope`, drive the engine, frame the completion), and the class-side
  `UrbCall` / `UrbClient` / `UrbLink`. Host-tested end to end; the decoders
  are in the `lib/abi` fuzz harness.
- **U3a — per-endpoint URB-transport grant mechanism.** §1.3
  requires the right to submit URBs for an interface to be "minted kernel-side
  from the matched node, never ambient" — a mechanism that did not exist. It
  now does, modelled on `msi_alloc`'s allocate-then-grant pattern and reusing
  the existing region-scoped grant machinery, so no `kernel/ipc` change was
  needed:
  - `CapabilityId::IPC_ENDPOINT` (28) is the generic "participate in
    per-endpoint-granted call IPC" capability; `HwResourceKind::Endpoint` (5)
    + `HwResource::endpoint(id)` is the per-endpoint grant (its `base` is the
    call-endpoint id, `covers` is exact-id containment like an IRQ line, its
    required capability is `IPC_ENDPOINT`). The C header regenerated
    (`TAIRIX_CAP_IPC_ENDPOINT`).
  - A *grant-restricted* endpoint is one whose required send caps include
    `CAP_IPC_ENDPOINT`. `call_create` mints the creator the matching
    `HwResource::endpoint(id)` grant when it binds such an endpoint, so the
    server may forward the endpoint onto a node it emits (`hw_emit_node`'s
    existing coverage check admits it unchanged) and the autoloaded class
    driver inherits the grant from its matched node like any MMIO/DMA/IRQ
    resource. `ipc_call` denies a grant-restricted endpoint fail-closed unless
    the caller's grants cover the endpoint id — so two class drivers behind
    one controller cannot reach each other's transport endpoint even though
    both hold the class capability. Covered by host unit tests (ABI
    kind/`covers`/round-trip; kernel `call_create` grant-mint, `ipc_call`
    denied-without-grant, and round-trips-with-grant).
- **U3a2 — cross-process shared-memory primitive.** The URB data
  path needs a shared-memory buffer the class driver owns and the HCD maps
  (§1.3); TAIRiX had only per-process `mem_map`. The kernel now provides a
  generic, capability- and grant-scoped shared-memory primitive (Option B:
  the buffer is plain cacheable RAM with **no** DMA properties, so a class
  driver holds zero DMA authority and the HCD will bounce-copy into its own
  DMA ring — smallest attack surface, no IOMMU coupling):
  - `CapabilityId::SHM` (29) gates participation; `HwResourceKind::Shared`
    (6) + `HwResource::shared(id)` is the per-region grant (exact-id `covers`
    like the endpoint grant, required capability `SHM`). Three syscalls
    `shm_create` (40) / `shm_map` (41) / `shm_unmap` (42); C header
    regenerated (`TAIRIX_CAP_SHM`, `tairix_sys_shm_*`).
  - `shm_create` allocates a physically-contiguous, **zeroed** kernel-owned
    region, maps it cacheable `RW`/non-exec/guard-bracketed into the caller's
    own live space (a fourth per-task window beside MMIO/anon/DMA, reusing the
    guarded-window mechanism with cacheable flags — `MmioWindowMap::
    map_cacheable_into`), records it against the owner, and mints the owner
    `HwResource::shared(id)` so it can forward the region onto an emitted node
    (`hw_emit_node` coverage unchanged). `shm_map` resolves the inherited
    grant against the calling task (forgery/wrong-kind fail closed), maps the
    *same* frames into the grantee, and refcounts. The region's frames are
    **scrubbed and freed only at the last reference** (`shm_unmap`, exit, or
    the U1 driver-unload teardown), so two class drivers behind one controller
    cannot reach each other's buffer and a hot-removed driver's frames are
    zero-on-freed even though the teardown runs in the device manager's
    context (the facility scrubs through the arch direct map, new
    `KernelArch::direct_phys_map`). Mechanism: `kernel/core` `SharedRegion`
    registry (`sharedreg`, facility-as-parameter — no global) + the
    `SharedMemFacility` seam over `LiveSharedMem`; per-arch direct-map wiring
    in all three ports. Covered by host unit tests (ABI kind/`covers`/round-
    trip; `LiveSpace::map_shared`/`unmap_shared`; sharedreg refcount + last-
    ref free + reclaim + fail-closed; shm handler grant-mint/id-write,
    forged/wrong-kind, no-facility).
- **U3a3 — wait-set multi-event wait primitive.** The HCD is a
  single async event loop (§1.1) that must wake on *either* an incoming URB
  IPC call *or* its controller interrupt, for arbitrarily many interfaces.
  TAIRiX had no multi-source wait (`call_recv` parks on one endpoint,
  `irq_wait` on one line), and after the split the class driver holds no
  controller IRQ to park on — so without this it could only busy-poll
  (forbidden, §2.23) or block one interface behind another. The kernel now
  provides a growable, caller-owned **wait-set** (the scalable `epoll`/`kqueue`
  analogue — membership registered once, no fixed source ceiling, §24.1):
  - `SyscallNumber::WAITSET_CREATE` (43, no args → handle) /
    `WAITSET_CTL` (44, set/op/kind/id/token → errno) /
    `WAITSET_WAIT` (45, set/timeout/token_out → errno). `lib/abi/src/waitset.rs`
    holds the two scalar enums `WaitSetOp` (`Add`/`Del`) and `WaitSourceKind`
    (`Endpoint`/`Irq`); no packed wire format (the values cross as registers),
    so no C-header surface beyond the three syscall stubs.
  - A member names a resource the caller **already holds** — an IPC endpoint it
    serves or an `IrqHandle` it bound. `WAITSET_CTL(Add)` resolves and
    **owner-checks that resource against the kernel-trusted caller before
    recording it** (no ambient authority): an unowned endpoint / unbound IRQ
    fails closed `NotFound`. `WAITSET_WAIT` parks the caller on the shared
    `SERVE_WAITQ` + `IRQ_WAITQ` with the timeout as the deadline (woken by an
    endpoint post, a member line firing, or the timed sweep — never a spin),
    re-arms each IRQ member's line before parking, re-checks each member
    against the caller as it scans, writes the ready member's token through the
    validated user boundary **before** consuming the IRQ edge (a faulting
    `token_out` drops no interrupt), and reports first-ready. Needs no
    capability of its own. Wait-sets are reclaimed on task exit and by the U1
    driver-unload teardown.
  - Mechanism: `kernel/core/src/waitset.rs` (growable, handle-keyed registry,
    owner-checked, global pure-data behind a `SpinLock` like `callreg`);
    `CallEndpoint::has_pending` (non-consuming readiness peek, drained by
    `call_recv`); reuses `IrqTable::line_for`/`ready_for`/`try_wait_step`.
    Covered by host unit tests (registry create/add/remove/duplicate/owner-
    check/release; handler owner-checked ctl, timeout, fired-IRQ readiness +
    edge-consume + token-write, pending-endpoint readiness drained by recv).
    `lib/rt`/`lib/drvrt` wrappers + a QEMU vertical land with U3b (the first
    consumer); not added speculatively (§2.4).
- **U3b — xHCI HCD process** (live path metal-only).
  `drivers/bus/usb/xhci` (`tairix-drv-bus-usb`) is now a `lib`+`Run`-binary
  crate: it binds `usb,xhci` and the xHCI PCI class (`BIND_KEYS`), owns
  the controller, enumerates,
  emits one per-interface node, and serves the URB transport. The host-testable
  logic is in the `lib` target:
  - `bringup` (moved from the deleted `lib/hid::service`, returning the raw
    `UsbDevice` rather than a decoded `BootKeyboard`): `derive_controller_
    resources` + `bring_up_controller[_diagnostic]`, host-tested over mocks up
    to the controller hand-off (`Xhci::open` fails closed on an inert window —
    the metal boundary).
  - `serve`: the alloc-free `UrbService` (≤1 outstanding interrupt-IN URB,
    second concurrent submit → `AlreadyExists`, driven on submit/IRQ via
    `lib/usb::drive_urb`/`frame_completion`, rejecting submits with `NotFound`
    while no interface node is live) + `attach_transport_grants` (adds the
    endpoint + shm grants onto the `describe_device` node). Host-tested.
  - `main.rs` (freestanding): `from_grants_query` → bring-up →
    grant-restricted `call_create` (one protocol-sized endpoint-id block
    claimed by its base id; per-index ids and `shm_create` buffers are
    bound lazily as device-table indices first serve) →
    `describe_device`/`attach_transport_grants`/`hw_emit_node` (now returns the
    assigned node id) → enable interrupter + `irq_bind` → **async wait-set
    loop** (URB endpoint + controller IRQ): recv → `UrbService::on_submit`
    (reply now or hold); IRQ → ack + hotplug service first (hub status-change
    or root-port `CCS`; disconnect aborts the parked URB with `NotFound`) →
    `on_event` only when no disconnect was handled. If a hub-downstream unplug
    appears first as the watched device's failed interrupt transfer, the HCD
    confirms the hub port is disconnected before retracting the node and
    replying `NotFound`. It leaves the hub connection-change latch for the
    status endpoint to report, so an already-stashed or delayed disconnect
    notification drains/re-arms the watch before the later reconnect; live-device
    report faults are not reclassified.
    Bounce-copy is internal to the engine's `interrupt_in` writing the report
    into the HCD's shm mapping; the class driver holds no DMA grant. `lib/rt`
    `shm_*`/`waitset_*` wrappers and `lib/drvrt` `urb_endpoint()`/`map_shared()`
    landed as the consumers.
  - `hw_emit_node` was evolved to **return the kernel-assigned node id** (the
    emitter cannot choose it but needs it to `hw_remove_node` on disconnect);
    `drvrt`/existing bus drivers treat `≥0` as success unchanged.
- **U4 — class drivers over the transport.** A class driver is a `lib`+`Run`
  crate binding an **interface** key in its own `lib` `BIND_KEYS`, holding no
  MMIO, DMA or IRQ grant. It takes the node's shared buffer
  (`RtDriverHost::shared_buffer`) and submits **blocking** URBs over
  `UrbLink` (`ipc_call`), parking in the kernel between reports. `NotFound`
  from the transport is terminal: the process exits, so a re-emitted node
  loads a fresh instance rather than retrying a vanished interface. The image
  ships the `xhci` HCD bundle (`bus_usb/xhci`) and each class driver's bundle.
- **U5 — event-driven hot-plug + re-enumeration** (live metal acceptance is
  UM). Both staged refinements are built in
  `lib/usb`, and the HCD services them:
  - **Hub-downstream hot-plug is event-driven, not polled.** The `UsbDevice`
    engine, when it descends through a hub, configures and **arms the
    hub's interrupt-IN status-change endpoint** (USB 2.0 §11.12.3) and keeps
    the hub addressed concurrently with the downstream device, each on its
    own control pipe. The shared xHCI event ring is **routed per endpoint**
    (`hub_async_index`/`report_async_index` + the per-device and per-hub
    parking slots), so a status-change report and a keyboard report never
    collide. A report wakes each port it names, whose operation reads it: a
    disconnect frees the device slot (Disable Slot) → `HubEvent::Detached`; a
    connect enumerates a **brand-new** device → `HubEvent::Attached`. No
    busy-poll, no spin (§2.23).
  - **Every port-change latch is drained, not just connect.** Resetting a
    downstream port during enumeration latches the hub's Reset-change (and may
    latch Enable-change) in `wPortChange` alongside Connect-change; a hub keeps
    its status-change endpoint asserting a report for that port until **all**
    its latched changes are cleared. So `clear_hub_port_changes` issues one
    `CLEAR_FEATURE` per set change bit (connection/enable/suspend/over-current/
    reset), and enumeration drains the whole set before the watch is armed.
    Clearing only Connect-change left the Reset-change latched, so the
    freshly-armed watch fired immediately and forever on a stale change — on
    metal that derailed the keyboard (the hub status-change service faulted
    `DeviceFault` and the keyboard's reports errored out). `process_hub_change`
    acts only on a genuine connect/disconnect transition and drains every
    latch, so the watch goes quiet until the next real hot-plug.
  - **Re-attach is a fresh device, never reused state** (the operator's
    requirement): on connect the engine resets the downstream port and
    enumerates on a fresh slot; the HCD re-emits a **new** interface node
    carrying the index's endpoint and a fresh shared buffer, so `devmgr`
    re-autoloads its class driver onto it and input resumes to the same seat.
    Re-attach zeroes the reused EP0/interrupt ring regions first (stale TRBs at
    the producer cycle would otherwise be consumed past the new enqueue
    pointer — a real-hardware correctness fix). A disconnect also aborts any
    parked interrupt-IN URB with `NotFound` before draining a stale faulting
    transfer completion from the vanished device. If the fault arrives before a
    hub status-change completion, `UsbDevice::detach_if_watched_device_gone`
    reads the watched hub port and only then converts the stale transfer into
    interface retraction + `NotFound`, deliberately leaving the hub latch for
    the status endpoint. If the report path had already stashed the hub
    status-change completion, or if it arrives just after retraction, the HCD
    drains it to re-arm the status endpoint before the physical reconnect.
    Ordinary live-device report faults stay visible as transfer errors.
    Old-driver submits that race in after retraction while no interface is live
    are rejected, so the reloaded class driver cannot be blocked by a stale
    ticket from the vanished instance. A freed device slot is retained as
    `UsbDevice::freed_slot` so a **trailing** transfer completion the controller
    still posts for the gone slot (a dropped in-flight transfer, or a Disable
    Slot side-effect) is dropped by the service step's routing rather than
    faulted — without
    this such an event matched no live endpoint and faulted the hub watch,
    silencing it so a later re-plug went unseen. The tolerance is cleared once a
    fresh device enumerates.
  - **A zero-length interrupt-IN completion parks; it is not a report and not
    a fault.** An idle or composite HID interface (a wireless MMO mouse's
    extra collection) can complete an armed interrupt-IN transfer with no data
    (a ZLP — a residual equal to the whole request). `decode_transfer_report`
    returns `Ok(None)` for it and `capture_report_event` re-arms the endpoint
    without buffering a report, so `next_report` finds the FIFO empty and holds
    the URB outstanding (`Ok(None)` → `Held`) rather than replying. So the class driver's blocking
    `ipc_call` stays parked and a ZLP costs one controller interrupt, never a
    reply-and-resubmit spin. Treating the ZLP as a `DeviceFault` (the prior
    behaviour) made the HCD reply an error, the class driver exit fail-closed
    after a few pump faults, and `devmgr` reload it — a hot reload storm that
    pegged a core at boot behind a ZLP-streaming device. Regression:
    `a_zero_length_completion_parks_and_rearms_rather_than_faulting`.
  - **Interrupt-IN report capture is driven off the controller interrupt,
    fully decoupled from the class driver, and the HCD runs in the
    strict-priority real-time class — it cannot be starved, ever.** A boot
    keyboard (or mouse, or any interrupt-IN device) reports only on a state
    change, at its polling interval, on the device's own schedule. The class
    driver and the HCD are ordinary user-space tasks, so under CPU load
    (`stress --cpu N`) either could be slow to run; the report path must not
    depend on how promptly they are scheduled. The mechanism has three parts —
    the first two decouple capture from the *class driver*, the third removes
    the residual dependency on the *HCD* being scheduled:
    - **Continuous depth-arming.** The engine keeps `INT_ARM_DEPTH` interrupt
      transfers armed on every report endpoint (`UsbDevice::ensure_reports_armed`),
      so the controller always has a landing TRB for the next interval's report
      and the xHCI event ring queues the completions. Bounded by the ring (one
      slot stays free); in-flight-but-unretired transfers count against the
      depth, so retiring a completion is what re-opens a slot.
    - **Consumer-independent capture in every service step.** The service
      step the HCD runs on *every* wake (`UsbHost::service`), **before** it
      drives any held class-driver URB, arms every served interrupt-IN
      endpoint and routes every posted completion off the shared event ring
      (`capture_report_event`): each report is
      **copied out of its DMA slot into a bounded per-device software FIFO**
      (`DeviceState::reports`, `REPORT_QUEUE_CAP`) and the slot is re-armed at
      once. Capture therefore happens the instant the controller posts a
      completion, regardless of whether the class driver has a URB in flight or
      is running at all. `next_report` then simply pops the FIFO when the class
      driver next submits — its consume rate no longer gates hardware polling.
      This is the analogue of Linux `usbhid`'s IRQ-context URB resubmission,
      adapted to the microkernel's user-space HCD: the re-arm is on the
      controller's interrupt, not on a starvable per-URB path.
    - **The HCD runs in the strict-priority real-time scheduling class.** The
      capture above removes the dependency on the *class driver* being
      scheduled, but the capture itself runs in the HCD task, which is woken by the
      controller IRQ (`irq_wait`) and must be *dispatched* to run. Under
      `stress --cpu N` a fair-scheduled HCD would merely compete with the
      CPU-bound tasks, so the wake-to-run latency could stretch until the
      armed transfer ring drained and reports were lost at the hardware — the
      starvation point simply moved from the class driver up to the HCD. The
      HCD therefore enters the real-time class at start-up
      (`tairix_rt::sched_set_realtime(true)`, gated by `CAP_SCHED_REALTIME`
      in its manifest): a real-time task is dispatched ahead of *every*
      time-shared task on its CPU and is never preempted by one, and the wake
      already sends the placement IPI, so the IRQ-woken HCD preempts the
      `stress` tasks and runs within interrupt-return + context-switch latency
      — microseconds, which the `INT_ARM_DEPTH`-deep armed ring covers many
      times over. This is the microkernel analogue of Linux's threaded-IRQ /
      `SCHED_FIFO` context: the report path no longer depends on how busy
      userland is at any layer. The scheduling class is a general kernel
      facility (`SchedClass::Realtime`, honoured by every policy — CFQ, EEVDF,
      MLFQ — and exercised by the `kernel/sched/api` conformance suite), not a
      USB special case; any interrupt-driven driver that must service its
      device before a hardware ring drains uses it the same way.

    The previous design re-armed only from inside `next_report`, so once a
    starved class driver fell more than `INT_ARM_DEPTH` reports behind the
    endpoint went unarmed and every later report was dropped at the hardware —
    the on-metal "missed keypresses under load" defect. With the pump the
    endpoint is re-armed every interrupt independent of the consumer, and with
    the HCD in the real-time class the pump itself runs the instant the IRQ
    fires however busy userland is, so a
    transiently-starved class driver loses nothing. Because the FIFO is bounded,
    a *permanently* stalled consumer cannot make the engine hold unbounded
    memory: the oldest buffered report is dropped so the newest device state is
    kept, and the loss is counted (`dropped_report_total`) and logged by the
    HCD — never silent. The single `pending_report` slot the old parking path
    used is replaced by this FIFO: with several transfers armed, more than one
    report completion can arrive in one service step, which the single slot
    would have faulted.
    Regressions: `the_interrupt_pump_keeps_reports_flowing_past_the_armed_depth_with_no_class_urb`,
    `the_interrupt_pump_captures_a_mouse_the_same_way_as_a_keyboard`,
    `a_permanently_stalled_consumer_bounds_the_buffer_and_counts_dropped_reports`,
    `a_report_arriving_in_the_class_driver_resubmit_gap_is_not_lost`,
    `a_burst_of_reports_before_the_class_driver_drains_any_is_kept_up_to_depth`.
  - **A halting interrupt-IN completion is only *flagged* where it is observed;
    the endpoint is recovered later, at a top-level, non-re-entrant point.
    Neither the completion code nor a failed recovery is a verdict on the
    device: its settled port is.** Any error completion code — a STALL, babble, data-buffer or
    TRB error, **and a USB or split transaction error** — leaves the endpoint
    **halted**: the xHCI controller runs no further transfers on it until it is
    reset (§4.8.3), so re-arming a halted endpoint re-faults every transfer. A
    transaction error (`UsbTransactionError` / `SplitTransactionError`) is *not*
    conclusive of a hot-removal on its own: CRC / timeout / bad-PID is exactly
    what a device that is present but momentarily disturbed produces — for
    instance a keyboard or mouse hammered with input *while its endpoint is
    still being brought up*, the reported on-metal defect (typing during boot
    killed the keyboard, and a transient mouse fault stormed a core).
    - **Recovery must not run from `capture_report_event`.** Report capture
      runs inside the service step's routing. Recovery (Reset Endpoint →
      rebuild the transfer ring at its base → Set TR Dequeue Pointer →
      device-side `CLEAR_FEATURE(ENDPOINT_HALT)`) waits on routed completions,
      during which another interrupt fault for the same endpoint (a held key,
      a moving mouse) lands back in capture — so recovering there would
      re-enter recovery and scramble the ring it is rebuilding. On metal that
      recursion made recovery fault, which recorded a fatal report fault, which
      killed the class driver after a few consecutive pump errors (the
      "type during boot and the keyboard stops working" symptom), leaving the
      halted endpoint's later recovered reports with no consumer — the
      dropped-report storm.
    - **So capture only flags** the endpoint and asks for its recovery, which
      runs as an operation of its own (`recover::recover_reports`, one per
      device owner), never inside the routing. An interrupt completion arriving
      during the recovery's own waits only re-flags the endpoint, so recovery
      can never re-enter itself. Recovered, the endpoint is
      re-armed and the URB stays parked (`Ok(None)`), so a transient fault
      never reaches — and kills — the class driver.
    - **A failed recovery owes deferred retries that read the settled
      port.** A present device answers its own `CLEAR_FEATURE` handshake and
      keeps serving (the captured fault code is cleared). One that does not
      may be gone or only disturbed: a hub's transaction translator glitching
      for a moment fails the handshake as surely as an unplug does — the
      on-metal capture where a keyboard and a mouse that were never touched
      were dropped. So a failed recovery leaves the device served, its URB
      parked and its endpoint unarmed, and owes `RECOVERY_RETRIES` (2)
      retries, which the HCD runs `RECOVERY_SETTLE_NS` (500 ms) apart
      (`UsbDevice::retry_owed_recoveries`, armed off one `OneShot`). Each
      reads the port the device hangs off: a vacant port frees the device; a
      port its hub disabled is reset and enumerated afresh; on an enabled one
      the endpoint is recovered again in place, so a device that was only
      disturbed keeps its slot, index, node and class driver. A recovery
      still failing once the retries are spent frees the device, and its port
      is reset and enumerated in the next round — which a vacant port never
      answers — so the HCD retracts the device's node before whatever answers
      there is published, and no node is carried across a reset on an equal
      identity it has not proved (a storage device's serial). The ladder is
      one pure state machine (`device_recovery.rs`,
      `Ladder`) shared with the hub watch: an incident opens at a halt and
      closes only when a transfer next completes, so a recovery the device
      answers but that halts again before any report counts as failed, and a
      device the reset produced that fails the same way is given up, its port
      left unserved until a re-plug, rather than reset forever.
      `UsbHost::confirm_departure` leaves a device whose recovery is owed to
      the retry, and otherwise frees one only on a port read that comes back
      vacant. Every endpoint kind stops through the one idempotent
      `stop_endpoint` (the command its likely state needs first, the other on
      Context State Error) and restarts through `restart_ring`, so a recovery
      that failed part-way runs again. A malformed mapping over a
      *successful* completion (a forged residual or TRB pointer) still fails
      closed without recovery — the endpoint is not halted. Regressions:
    `report_source_recovers_a_halted_endpoint_without_faulting_the_class_driver`,
    `report_source_recovers_a_babble_halt_the_same_way`,
    `a_transient_transaction_error_during_bringup_recovers_and_keeps_the_keyboard`,
    `a_keystroke_landing_during_recovery_is_deferred_and_never_recurses`,
    `a_glitch_failing_the_hub_watch_and_the_keyboard_recovers_both_in_place`,
    `a_gone_device_is_freed_by_its_owed_retry_once_its_port_reads_vacant`,
    `a_device_still_failing_once_its_retries_are_spent_is_enumerated_afresh`,
    `a_disabled_port_is_enumerated_afresh_without_spending_its_retries`,
    `a_device_whose_endpoint_halts_however_often_it_is_recovered_is_reset_then_given_up`,
    `an_owed_re_enumeration_leaves_a_port_attached_meanwhile_alone`,
    `an_owed_re_enumeration_leaves_with_the_hub_its_port_was_on`,
    `live_downstream_report_fault_recovers_the_endpoint_and_keeps_the_device`,
    `forged_report_residual_fails_closed`. (QEMU models no Pi USB, so the exact
    on-metal ring corruption from the old recursion is not host-reproducible;
    the mock exercises the deferred-recovery contract, and the non-re-entrant
    structure removes the recursion by construction.)
  - **A transaction fault *while a device's control pipe is being
    established* is a disturbed device, not a broken one: enumeration re-drives
    a fresh slot, bounded.** Address Device and the descriptor reads
    (`EnumStage::is_pipe_bringup`: `EnableSlot`/`AddressDevice`/
    `GetDeviceDescriptor`/`GetConfigDescriptor`) for a full/low-speed device
    split through the hub's transaction translator, and a device hammered with
    input *while it is still being addressed* — a keyboard typed on during
    boot, before USB bring-up — makes that split complete with a
    `SplitTransactionError` (the on-metal `enum_stage=3 completion=36`). A
    fault on Address Device leaves the device in Default state, one on a
    descriptor read leaves it holding the address it was just given; either
    way enumeration disables the slot, resets the port — returning the device
    to Default state, where a fresh slot's `SET_ADDRESS` reaches it — and
    re-drives a fresh Enable Slot + Address Device up to `ENUM_ATTEMPTS` (4)
    times, as Linux `hub_port_init` resets the port on each of its
    `PORT_INIT_TRIES`. Before this, one transient split fault failed the
    attach; because the Pi 4's only connected root port is its onboard hub,
    that took the *whole* controller down (`bring_up` attached nothing →
    errored → the HCD task exited nonzero). The retry is scoped to the
    pipe-bring-up phase and to a *transaction* fault
    (`CompletionCode::indicates_device_unreachable`): a device that *answers*
    with an error (STALL/babble), a forged descriptor, or a fault on
    `SET_CONFIGURATION`/`CONFIGURE_ENDPOINT`/an optional HID class request is
    surfaced on the first attempt, never re-driven — a bounded recovery, never
    a retry-until-it-works. Regressions:
    `a_transient_split_fault_during_enumeration_retries_and_serves_the_device`,
    `an_active_device_error_during_enumeration_is_not_retried` (QEMU models no
    Pi USB, so the register-level mock injects the one-shot Address Device
    split fault the metal produced).
  - **Interrupt moderation is left at the 1 ms xHCI reset default, never
    disabled.** `enable_interrupter` programs `IR_IMOD` to
    `regs::IMODI_DEFAULT` (4000 × 250 ns = 1 ms). Disabling moderation
    (`IMOD = 0`) raises one interrupt the instant every transfer completes;
    that is fine for a report-on-change endpoint but an interrupt-IN endpoint
    that streams a report every service interval — a mouse polling every
    microframe posts thousands per second — then floods the CPU with one
    interrupt per report, pegging the host-controller and class drivers even
    while the pointer is idle (an on-metal CPU storm). The 1 ms default coalesces such a stream to ≤~1000 interrupts/s
    (each drain retiring every report the interval accumulated) and adds no
    perceptible latency to genuine, sparse input, exactly as production xHCI
    drivers run. Regression: `start_enables_the_interrupter` asserts
    `IR_IMOD == IMODI_DEFAULT`.
  - **Root-port hot-plug is a per-port CSC scan, uniform for every port.**
    A connect or disconnect on any root port latches `PORTSC.CSC` (and
    posts the Port Status Change Event that raises the interrupt); the
    service step's root-port scan hands each changed port to its own
    operation (`port::serve_port`), which consumes the latches
    (`Xhci::clear_port_changes`, RW1C-masked), re-reads the live connect
    state, and reconciles: a new connect on an unserved port is attached in
    place (a hub tier installed, descended, and watched, or a leaf served),
    and a disconnect detaches exactly what that port carried (a hub tier
    cascades with everything behind it). The controller is **never** reset for routine
    hot-plug, so sibling ports' devices are untouched; the latch scan is
    register-confirmed, so no plug is lost to a coalesced event. `reset_and_reenumerate` (full
    HCRST + re-program + full re-walk) remains only as the latched
    controller-fault recovery. Fail-soft per port, mirroring the hub scan:
    a failed attach consumes the latch (no re-fire storm), records its
    `AttachFault` from the operation's own breadcrumb (with the root port
    number; `port_status` is `0` — a root port has no hub-format
    `wPortStatus`), and the first failure is taken by the HCD's log
    (`UsbDevice::take_attach_fault`).
  - **The `PORTSC` walk is armed by a latched event, never run
    unconditionally.** The scan returns immediately unless
    `root_change_pending` is set, because on a PCIe controller each `PORTSC`
    read is a non-posted round trip — the most expensive operation on the
    report path — and a mouse in motion makes that path run ~1000 times a
    second. Two independent xHCI-mandated latches arm it: a **Port Status
    Change Event** recorded in `poll_event`, the one point the service step
    reads the ring through, and the **`USBSTS.PCD`** summary, read
    and cleared by the interrupt acknowledgement that had to write that
    register anyway. It starts armed and is re-armed by
    `reset_and_reenumerate`, so bring-up and post-reset re-enumeration scan
    ports the firmware may already have left connected. A scan visits every
    port and hands each changed one to its operation. Regressions:
    `a_drained_port_status_change_event_arms_the_root_scan`,
    `the_usbsts_port_change_summary_arms_the_root_scan`,
    `an_unarmed_root_scan_touches_no_port_register`,
    `every_root_port_changed_under_one_arming_is_served`,
    `a_controller_reset_rearms_the_root_scan`.
  - **The event ring is drained once per interrupt, by one classifier, reading
    one TRB per probe.** The interrupt is acknowledged, then the service step
    makes the single pass that sorts every posted event into its consumer
    (reports into per-device FIFOs with the endpoint re-armed, each watched
    hub's status report into its parked slot, bulk into its FIFO, a command
    completion to its poster by TRB address, a Port Status Change Event into
    the scan arming), and hot-plug and the URB replies then read those. The hub watch takes
    what that drain parked rather than walking the ring with a second copy of
    the same dispatch decision, and `poll_event` reads the single 16-byte entry
    at the dequeue point rather than the whole segment out of non-cacheable
    memory. Servicing loops until every parked hub report is drained, not just
    the first: a hub's status endpoint is re-armed only once its report is
    serviced, so a second reporting hub left for "the next interrupt" may never
    get one. Measured per 4-byte report: **1 register read** (was 6) and **100
    bytes of DMA over 7 reads** (was 1796 over 8), allocating nothing.
    Regressions: `a_report_interrupt_costs_one_register_read_and_one_trb_per_ring_probe`,
    `a_report_interrupt_allocates_nothing`,
    `a_hub_status_change_is_serviced_from_the_slot_the_shared_drain_parked_it_in`,
    `two_hubs_reporting_at_once_are_both_serviced_without_a_further_interrupt`.
  - **Metal acceptance item (open).** The scan arming and the reordered
    interrupt service are mock-verified only — QEMU models no Pi USB. On a
    Pi 4, confirm plug and unplug of a mouse, a keyboard and a hub assembly on
    both a root port and a downstream hub port, and confirm the CPU drop while
    moving a mouse.
  - **Every reachable device is served concurrently — on every root
    port.** `UsbHost::bring_up` powers all root ports, waits out the
    connect-debounce window on a timer, then hands **every** root port to its
    own operation (fail-soft per port): a hub tier (the Pi 4's
    onboard USB2 hub) is installed, descended — every connected downstream
    port, nested tiers included — and watched, and a directly-attached
    device (the Pi 4's USB3 side of each jack is wired straight to a root
    port) is served beside it. Every attachment is enumerated on its own
    claimed device-table entry and demand-allocated DMA region — the
    single-root special case (the shared-chunk `enumerate_hid` path and
    the engine-level `root_port`/`root_speed`) is deleted; a hub records
    its root port in its `HubState` (children inherit it into their slot
    contexts) and a leaf in its `DeviceState`, so a *direct* device's
    removal is confirmed on its root port's live `CCS`. Each slot has a
    control pipe of its own (U14). Devices
    fill a
    growable table bounded only by the
    controller's reported slot count and genuine memory exhaustion — a
    keyboard and a storage stick plugged in together are both served, neither
    displacing the other (the Pi 4 boot defect where a plugged-in stick won
    the single device slot and the keyboard never enumerated). Each device
    owns its own demand-allocated DMA region (EP0/interrupt/bulk rings and
    buffers), grown on attach and released on detach, and is
    driven through the per-device `engine_for(index)` `UrbEngine` view; the
    HCD serves one URB transport (endpoint + shared buffer + node) per device
    index, so one interface's transfers can never reach another device's
    endpoints. A port whose device fails enumeration is skipped with its slot
    released, never allowed to cost the other ports their service.
  - **A device absent at initial bring-up is a first-class state, not a
    failure.** The controller is always brought up and left serving. When the
    root device is the onboard hub but no downstream device is connected yet,
    the hub's status-change watch is armed; when no root device is present at
    all, the controller waits for the first root-port connect, which the
    root port's operation attaches with no controller reset. The HCD
    publishes an interface node per device actually enumerated
    (`device_live(index)`), so a cold boot with nothing plugged in works:
    plug a device in afterwards and it autoloads. `reset_and_reenumerate`
    re-runs the same bring-up walk, so devices that vanished before
    re-enumeration leave the controller awaiting them rather than faulting.
    A hub claims its own device-table entry's region for its contexts
    (exactly as a nested hub always did), so the leaf devices behind the
    Pi 4's onboard hub occupy indices above it — index 0 is the hub's.
  - Host/CI-proven over the register-level mock (the mock gained a hub
    status-change endpoint, per-slot EP0-ring tracking, Disable Slot, HCRST
    state reset, `PORTSC.CSC` write-1-to-clear, and a root-port-keyed
    fixture model — the hub on root port 1, a leaf on any other — with the
    root hub's live slot tracked for the TT check; `SET_FEATURE(PORT_RESET)`
    latches the Reset-change bit and
    `CLEAR_FEATURE` clears only the selected change, mirroring real hardware):
    `hub_watch_arms…`, `hub_watch_retracts_a_disconnected…`,
    `hub_watch_reenumerates_a_reattached_device_on_a_fresh_slot`,
    `reset_and_reenumerate_brings_up_a_directly_attached_device_as_new`,
    `enumeration_drains_every_port_change_latch_so_the_hub_watch_stays_quiet`,
    `trailing_freed_slot_transfer_event_is_drained_not_faulted`,
    `hub_assembly_unplug_at_root_port_tears_down_and_replug_reenumerates`,
    the multi-root cases (the Pi 4 silent-insertion defect)
    `a_device_plugged_into_a_second_root_port_is_served_while_the_hub_stays_watched`
    and `bring_up_serves_a_hub_tier_and_a_direct_root_device_together`,
    the cold-boot-no-device cases
    `bring_up_keyboard_arms_the_hub_watch_when_no_downstream_device_is_present`,
    `bring_up_keyboard_then_a_downstream_connect_enumerates_a_fresh_keyboard`,
    `bring_up_keyboard_comes_up_awaiting_a_connect_when_no_device_is_attached`,
    and the concurrent-device cases (the mock scripts a keyboard and a
    mass-storage stick on separate hub ports at once, with per-endpoint slot
    attribution)
    `bring_up_serves_a_keyboard_and_a_storage_stick_behind_the_hub_together`
    and `unplugging_the_keyboard_leaves_the_storage_stick_served`.
  - **Whole-hub-assembly unplug (hub directly on a root port) is detected at
    the root port.** When a hub sits directly on a root port and is itself
    pulled, the unplug surfaces as that root port clearing its connect bit, not
    as a downstream hub-port status-change (the hub is gone, so it answers
    neither its status-change endpoint nor a `GET_PORT_STATUS`). The root
    port's operation sees the latched change, cascades the tier down
    (`detach_hub`: every device and deeper tier behind it), and the HCD
    reconciles its nodes; the later re-plug is attached afresh by the same
    operation — hub reinstalled, descended, watched — with no controller reset,
    so sibling ports' devices are untouched.
  - **Downstream keyboard unplug behind a *persistent* hub.** The Pi 4
    keyboard hangs off a hub that stays plugged in, so on unplug the root
    port keeps reading connected (`root_conn=1`, the root-port check above
    is correctly inert) and the disconnect surfaces as the keyboard's own
    interrupt-IN transfer faulting — completion code `0x24` (xHCI Split
    Transaction Error) on the metal capture — while the hub's
    `GET_PORT_STATUS` straight after can time out (`reject_hex=4`) or go on
    reading connected. No single early read is trusted: the hub's
    status-change report of the vacant port frees the device, and failing
    that its owed recovery retries do, bounded — a vacant port frees it, and
    once the retries are spent a port reset finds the port vacant. The
    completion code stays captured in `last_report_fault_code` for the
    diagnostics.
  - **The teardown is best-effort and never blocks on the controller.** The
    metal capture showed a teardown that waited for its Disable Slot
    timing out: the gone device's hub does not let the controller post that
    completion in time. A teardown therefore posts the Disable Slot and
    **frees the local slot state at once**, whether or not the controller
    confirms; a command it never completes is aborted at its deadline (U14),
    so the ring stays consistent for the next enumeration. The completion,
    whenever it lands, is settled by its routing
    (`UsbDevice::settle_awaited_disable`): a confirmation clears the slot's DCBAA entry
    and returns the region withheld behind it, a refusal leaves it withheld
    until a controller reset. The hub's connect change then re-enumerates a
    fresh keyboard. Host regressions:
    `a_gone_device_whose_port_still_reads_connected_is_freed_once_its_retries_are_spent`,
    `a_gone_device_is_freed_even_when_its_disable_slot_is_never_confirmed`,
    `rejected_report_records_its_completion_code_surviving_a_later_control_transfer`.
  - **A failed status-change service never silences the watch.** The
    service step re-arms the hub's status-change interrupt-IN endpoint (a
    fresh transfer + doorbell) **before** the ports the report names are
    served, so no failure serving them can leave it unarmed. Right after a downstream
    disconnect the gone device's transaction translator can briefly fail a hub
    class control transfer (the `reject_hex=4` timeout), so servicing that
    report faults; previously the re-arm was skipped on that error, leaving the
    status-change endpoint with no outstanding transfer — the hub could then
    never post another report and the **re-plug produced no interrupt at all**
    (the engine never woke again — the captured "reconnect not detected"
    symptom). Re-arming first keeps the watch live so the next genuine report
    (the reconnect) still wakes the loop. Host regression:
    `a_failed_status_change_service_re_arms_the_watch_so_a_replug_is_still_seen`.
  - **A failed status-change *transfer* recovers the watch.** A status
    transfer that completes with an error — a USB Transaction Error in the
    metal capture — halts the hub's status-change endpoint. Taken for a
    report and re-armed, it never completed again, so no later unplug or
    re-plug behind the hub was seen, which is how a keyboard dropped by a
    glitch stayed gone even when re-plugged. The service step asks for the
    endpoint's recovery instead (`recover::recover_hub_watch`) — stopped, its
    ring restarted, the hub's own
    `CLEAR_FEATURE(ENDPOINT_HALT)`, a fresh transfer armed — and reconciles
    every downstream port, since the lost report may have carried any change
    (`HubEvent::HubRecovered`). A hub that cannot be reached is owed the same
    deferred retries as a device, on the same ladder; one still failing once
    they are spent is reset as a whole tier — freed with everything behind
    it, its upstream port reset, and enumerated afresh — and a tier that
    reset brings up failing the same way is given up. Host regressions:
    `a_failed_status_report_recovers_the_hub_watch_so_later_changes_are_seen`,
    `a_recovered_hub_watch_reattaches_a_device_its_port_still_carries`,
    `a_hub_watch_that_cannot_be_recovered_at_once_is_owed_a_retry`,
    `a_hub_watch_still_failing_once_its_retries_are_spent_resets_the_hub_tier`,
    `a_hub_whose_status_transfers_keep_failing_is_reset_then_given_up`,
    `a_stashed_hub_report_frees_a_gone_device_owed_a_recovery`.
  - **Every engine wait is an operation's, and downstream hot-plug is
    interrupt-driven only — no periodic port sweep.** An operation waits on a
    routed completion or a one-shot timer, each bounded by **wall-clock**
    budgets (the per-command budget, the USB 2.0 §9.2.6 request ceiling,
    `CONNECT_WINDOW_US` for power-on-good + attach debounce), never an
    iteration-count spin; the HCD parks on the controller's interrupt line,
    bound **before** the controller is started (`UsbHost::start` enables the
    completion interrupter, so cold boot and every post-reset re-program share
    one definition). The hub's status-change interrupt-IN watch is the
    sole downstream hot-plug wake source; the HCD event loop's wait is
    unbounded with no periodic wakes. Every changed port still goes through
    the one `reconcile_hub_port` decision, keyed on the port's *live*
    `GET_PORT_STATUS` state against the tracking tables (attach the
    untracked connected, free the tracked disconnected — hubs cascade — and
    drain stale latches). The earlier "the hub raises no interrupt for a
    downstream connect" metal observation is treated as **undiagnosed**
    until re-captured: a bring-up failure logs the whole breadcrumb (phase,
    error, open stage, `USBCMD`/`USBSTS`), each settled walk logs a topology
    summary (devices served, hub watch), and a connected device that fails
    enumeration and is
    skipped logs a warning with the skip count
    (`UsbDevice::skipped_port_count`) instead of looking like an empty
    port. Host regressions:
    `a_command_that_never_completes_is_aborted_and_a_controller_that_will_not_abort_is_dead`,
    `an_empty_root_hub_parks_through_the_connect_window_and_serves_nothing`,
    `start_enables_the_interrupter`.
  - **Both port tiers run the one port-reset protocol step.** The reset
    figures live once (`PORT_RESET_POLL_US` 20 ms, `PORT_RESET_POLLS` 40 →
    800 ms, `PORT_RESET_SETTLE_US` = `TRSTRCY` 10 ms) and both tiers spend
    them the same way: wait for the port's change, re-reading it at the poll
    interval at the latest, until the reset signalling is done **and** the
    port is enabled; consume the reset's own change latches; then settle
    `TRSTRCY` on a timer before the device is addressed. A fast port costs one
    read; a slow external hub, which legitimately takes hundreds of
    milliseconds, is not refused as a `DeviceFault`; a port that reads
    disconnected ends the reset at once.
    - A downstream port's reset completes on the hub's status-change report
      (the watch is armed throughout, and its `C_PORT_RESET` is cleared the
      moment the reset is seen done), the poll interval bounding it.
    - A root port's reset completes on its Port Status Change Event, the poll
      interval bounding it, and `Xhci::clear_port_reset_change` consumes
      `PRC`/`PEC`. A `SuperSpeed` port, which trains and enables without a
      reset, still gets the settle.
    Landing the polled-reset fix on the hub tier alone was the Pi 4B defect:
    the root port returned the instant `PORTSC.PR` read clear and addressed
    the device inside its recovery interval, which the VL805 answers with a
    Context State Error (`enum_stage=3 completion=19`).
  - **A failed attach keeps its evidence.** Hot-plugging an (empty) external
    hub into the integrated hub faulted the attach (`hub status-change
    service failed err_hex=a`) with nothing in the log to say why.
    - A failed attach snapshots its diagnostics before the best-effort
      latch drain overwrites the live state
      (`AttachFault`: port, error, enumeration stage, the completion code
      and event type the failing step ended with or that its deadline did,
      the port's final observed `wPortStatus`; the first failure of a walk
      kept, taken by `UsbDevice::take_attach_fault`). The HCD's skipped-port
      warning logs that snapshot, and every error URB reply is logged with
      its errno and the endpoint's latched raw completion code, so a
      collateral class-driver fault (the keyboard's warning beside the
      hub plug) is attributable to the controller's actual verdict.
    Host regressions:
    `a_slow_hub_port_reset_is_polled_until_it_completes`,
    `a_port_that_never_enables_records_its_stage_port_and_final_status`,
    `a_root_port_reset_consumes_its_change_latches_and_settles_reset_recovery`,
    `a_root_port_that_never_leaves_reset_fails_closed_within_the_poll_bound`.
  - **The controller answers with the whole architected completion-code set,
    and a rejected command is retried.** `CompletionCode` covers all of xHCI
    1.2 table 6-90 (only reserved/vendor values still fail closed): a code
    the decoder cannot name reaches a diagnostic as "undecodable", which is
    how the VL805's `Context State Error` read as a driver decode failure
    (`error=5 reject=3`) and, worse, fell outside the retry classification.
    Enumeration now re-drives a fresh slot — after a port reset, whose
    `PORT_RESET_SETTLE_US` recovery interval also settles `TRSTRCY` — for
    **any** pipe-bring-up fault the device did not answer: a USB/split
    transaction error (it could not answer) or a command the controller
    rejected on its own slot/port state
    (`CompletionCode::indicates_state_disagreement`). A device that *answers*
    wrong (STALL, babble, a forged descriptor) still fails on the first
    attempt. Bounded by `ENUM_ATTEMPTS` (4, Linux's `PORT_INIT_TRIES`), never
    an unbounded retry. A command that completes with a non-Success code also
    retires its command-ring slot now — the controller consumed the TRB — so
    the ring no longer leaks a slot per rejection.
    Host regressions:
    `an_address_device_rejected_for_context_state_is_retried_on_a_fresh_slot`,
    `the_completion_code_decoder_names_the_whole_architected_set`,
    `a_rejected_command_retires_its_command_ring_slot`.
  - **A device that will not enumerate never takes the controller down, and
    is owed one deferred re-attach.** `bring_up` fails only on a fault of the
    **controller itself**; every per-port enumeration failure is a counted
    skip, including the case where *every* connected port fails, which used
    to error the walk and exit the HCD (code 82) — discarding a healthy,
    started controller with its watches armed, so one unservable device took
    the keyboard, the mouse, and every later hot-plug with it. The skip
    warning now carries the failing port's own snapshot rather than the live
    breadcrumb (which after a multi-port walk describes whichever port ran
    last). Because the walk consumed the skipped port's connect latch,
    nothing would ever wake it again, so the serve loop arms one one-shot
    (`SkippedPortRetry`, ~1 s) and spends it on
    `UsbDevice::retry_skipped_ports`, which re-drives every
    connected-but-unserved port on every tier and leaves an already-served
    port untouched (the re-drive resets the port, which would tear a working
    device down). It fires **once ever**, so a genuinely broken device cannot
    loop; after that the port waits for a physical re-plug, as Linux's does.
    Host regressions:
    `a_skipped_root_port_is_re_attached_by_the_deferred_retry`,
    `the_deferred_retry_never_resets_an_already_served_port`,
    `the_skipped_port_retry_fires_exactly_once_ever`,
    `an_unarmed_skipped_port_retry_bounds_no_park_and_is_never_due`,
    `arming_the_skipped_port_retry_twice_keeps_the_first_deadline`,
    `an_overdue_skipped_port_retry_bounds_the_park_at_zero`.
  - **A controller-fault recovery can no longer be wedged by a dead class
    driver's leftover URB submit.** Mid-typing (no plug/unplug) the VL805
    latched a controller fault; the HCD's reset/re-enumerate recovery
    worked, but the *reloaded* keyboard driver's every submit was refused
    `AlreadyExists` and it exited fail-closed. Cause: the old driver's
    final `ipc_call` was still queued in the kernel when `devmgr` killed
    it; the kernel kept a dead caller's calls, so after recovery the HCD
    received the corpse's URB, held it in the single outstanding slot,
    and the transport was wedged. Fixes, at the correct layers:
    - Kernel: task reclamation now cancels every call the dead task
      posted (`CallEndpoint::cancel_posted_by`, walked over the registry
      from `reclaim_task_resources`; audit event 3051) — a server never
      receives a dead caller's request (`docs/src/architecture/ipc.md`).
    - ABI/runtime: `call_recv` gained fail-closed `CallRecvFlags`
      (`NON_BLOCKING` answers an empty queue with `WouldBlock`); every
      wait-set-driven event loop — the HCD, `usb_msd` (one endpoint per
      LUN), the display service, login's elevation broker — receives
      non-blocking, so a readiness peek whose call was cancelled can
      never park the loop and starve its other sources.
    - Diagnostics: the controller-reset warning (id 27027, `usb-hcd:
      resetting the controller to recover it`) carries live
      `USBSTS`/`USBCMD`, so a metal capture names which fault bit
      (HSE/HCE/HCHalted) latched — the spontaneous mid-typing fault itself
      remains to be diagnosed from that evidence.
    Regressions: kernel/ipc `cancel_posted_by_*` (three-state scrub,
    other-poster isolation, silent no-op), kernel/core
    `reclaim_scrubs_a_dead_posters_queued_call`,
    `nonblocking_call_recv_answers_an_empty_queue_with_would_block`.
  - **A stray controller event never silences the hub watch (the
    "controller goes quiet after the first report" fix).** The Pi 4's black
    USB2 sockets sit behind an *integrated* hub (the engine reports
    `hub_watch=1` even with no external hub and the keyboard in a black port),
    so the keyboard is always a downstream-hub device and the hub-watch path is
    correct. The decisive metal capture showed the hub status-change service
    failing with diagnostics left **stale** from the last successful command —
    proving the fault issued no control/command transfer and so came from the
    hub watch's own ring poll, not the `GET_PORT_STATUS` transfer earlier
    hypotheses blamed. That poll
    failed closed on an event it did not model **before** the status-change
    endpoint was re-armed — leaving it with no outstanding transfer, so the
    hub could never post another report and every later
    keystroke/unplug/replug produced no interrupt (the controller "went
    silent"). The service step's routing, the only ring access, **drops** any
    event nothing owns — an informational controller event (device
    notification, host-controller event, MFINDEX wrap), a trailing freed-slot
    completion — and keeps routing, never faulting (the shared event ring is
    not a security boundary; a genuine fault still reaches the operation whose
    command or transfer it ends). Host regression:
    `a_stray_controller_event_during_a_hub_poll_never_silences_the_watch`.
  - **The event-ring drain is the *only* writer of `ERDP`; never a standalone
    Event-Handler-Busy clear (the "weird storm as soon as a key is pressed"
    fix).** xHCI Event Handler Busy (`ERDP.EHB`): the controller sets EHB when
    it asserts the interrupt and re-asserts `IMAN.IP` for a later event only
    once software writes `ERDP` (with the EHB bit) to a position **equal to**
    the controller's internal enqueue. `UsbDevice::poll_event` does exactly
    that per event it dequeues (`ack_event`), so EHB is released precisely when
    the ring is genuinely caught up. A *standalone* `ERDP` write performed on
    every interrupt service — including a wake that dequeued **nothing** —
    was the storm: on the non-coherent VL805/PCIe path the MSI can arrive
    before the event TRB's DMA write is visible to the PE, so the drain sees an
    empty ring while the controller's enqueue is already ahead; writing `ERDP`
    there points the controller at a dequeue *behind* its own enqueue, so it
    re-asserts immediately and the loop spins (the capture: `controller IRQ
    woke URB loop` + `hub change serviced, no event` thousands of times at one
    millisecond, `foreign_drained` frozen, only the first keystroke delivered).
    Writing `ERDP` at the *start* of servicing has the same effect; the timing
    was never the cure. The fix removes the standalone clear entirely:
    `UsbDevice::acknowledge_interrupt` clears `IMAN.IP` **only** (called at the
    start, so a completion posted during the drain re-asserts and is not lost),
    and the per-event `ack_event` the drain performs is the sole `ERDP`/EHB
    writer — matching the metal-confirmed bring-up model, which never wrote
    `ERDP` except per consumed event. A genuinely undelivered event is then
    picked up when its DMA lands and the drain consumes it, not by a
    speculative write. Host regression:
    `acknowledge_clears_ip_only_and_a_zero_event_wake_never_writes_erdp`.
    (Metal-only acceptance still required — QEMU models no Pi USB, §0.4.)
  - **The event-ring drain never consumes a cycle-owned but not-yet-landed
    TRB (the "first key then silent" fix).** With only the keyboard attached
    (the two hubs in the log are the VL805's own integrated USB2/USB3 hubs,
    keyboard on USB2), the **first** keystroke worked end to end and then the
    controller went silent — continuously working only while an *external* hub
    injected extra interrupts. The decisive metal capture's end-of-wake
    **interrupter snapshot** showed `erdp_ehb=1` (Event Handler Busy **stuck
    set**) with `IMAN.IP=0`, and `usb-hcd: last drained foreign event` showed
    `trbtype=0` (an all-zero TRB) drained on the first keypress (`foreign_drained`
    0→1). Root cause: `UsbDevice::poll_event` gated consumption on the cycle bit
    alone. On the non-coherent BCM2711/VL805 PCIe path the VL805's 16-byte event
    TRB write does not reach RAM atomically, so the announcing cycle bit can be
    visible to the PE while the body is still the zeroed initial state — the
    `dma_rmb` orders *this PE's* reads (body after cycle) but cannot order the
    *controller's* posted writes into RAM. The drain then popped that phantom
    zero TRB, advanced the dequeue **past** the controller's enqueue, wrote
    `ERDP` there, and permanently desynchronised the consumer cycle: the
    controller next set EHB for a real event whose cycle the (over-advanced)
    cursor no longer recognised, so `owned()` stayed false, EHB stayed set, and
    no further completion interrupt was raised. The external hub's unrelated
    MSIs were the only thing that incidentally re-woke the HCD. (This is the
    same defect class as the earlier storm — the zero-event-wake `ERDP` write —
    here triggered by the *body* lagging the cycle rather than the MSI lagging
    the event.) Fix: after the barrier and a re-confirmed `owned()`, `poll_event`
    refuses to consume an entry whose `trb_type_raw() == 0` (a real event TRB is
    never type 0): it leaves the entry un-consumed, writes no `ERDP`, and the
    next wake re-reads it once the body has landed — no poll, no spin, no stale
    pointer. Host regression:
    `a_cycle_owned_but_not_yet_landed_event_is_not_consumed_until_its_body_arrives`
    (the harness models a cycle-visible/body-zeroed entry via
    `MockXhci::unland_last_event`/`land_last_event`; it faults+over-consumes
    without the guard and is left alone with it).
    Metal-confirmed: with this fix, boot-time typing works end to end and every
    keystroke wake reads a healthy interrupter, exactly as predicted. The
    TEMPORARY metal diagnostics used to localise these faults (the BCM2711
    `brcm-msi` vector diag; the HCD `URB submit received` / `URB held` / hotplug
    / interrupter-snapshot / foreign-event Info logs, and the `UsbDevice`
    drain-count / foreign-event / disable-confirmed accessors that fed them)
    have been removed now that the chain is complete; the load-bearing tolerance
    behaviour they observed remains.
  - **A controller that latches a fatal error or halts is reset, and keeps
    the devices that come back.** The VL805 halts itself (`USBSTS.HSE` +
    `HCHalted`) during a downstream hot-removal teardown, after that device's
    Disable Slot completed, and a halted controller raises no further
    interrupt, so its hub watch would never see the re-plug. A Host System
    Error clears only with a Host Controller Reset, so the HCD checks
    `UsbDevice::controller_faulted` (`USBSTS & (HSE|HCHalted)`) on each
    interrupt and after each teardown, and recovers through
    `UsbDevice::reset_and_reenumerate`, the full reset and fresh walk a cold
    boot performs (routine hot-plug never resets the controller).
    The reset does **not** remove the controller's children — the Linux USB
    core's `usb_reset_and_verify_device` model:
    - A held URB is answered once its device's fate is known: with the URB
      protocol's reissuable `WouldBlock` where the device came back (the reset
      discarded whatever transfer it had armed), which every class driver
      treats as retry-later — `usb_msd` as `Busy`, the HID drivers as one fault
      they ride out — and `NotFound` where its node was retracted. Never before
      the reset: the resubmission it would prompt could arrive after a
      replacing device's node is published and reach that device.
    - After a successful reset each node is matched by identity to the index
      now serving its device (`interfaces::Interfaces::reconcile` over
      `UsbDevice::device_identity`): the device's root port and Route String
      plus its vendor, product, `bcdDevice`, device class triple,
      serial-number string, and the served interface's number and class
      triple. A device that came back keeps its node, its id, its buffer and
      its bound class driver even at another index; a vanished one is
      retracted and a new one published, and no two nodes ever claim one
      index. Node ids are never reissued, so keeping the node is the only way
      a device survives the reset with its driver; `devmgr` holds nothing. The
      serial is read, for a storage device only, in its first listed
      language; one with none, or whose read is refused, faults, times out or
      answers malformed, carries no serial and is still served. Non-storage
      twins swapped between two positions compare equal; a storage interface
      without a serial is never recognised across the reset
      (`DeviceIdentity::recognises`), so its node is retracted and
      republished.
    - A reset that fails leaves the children published while the
      controller's grace window runs (`plans/FIX-IO.md` IO4), serving nothing
      through the controller: a report poll, held or new, waits for the next
      attempt, and any other transfer is answered `WouldBlock` so its class
      driver's own recovery paces the retry. The window's one-shot retries the
      reset; if the window elapses first the subtree fails closed, every
      interface node is retracted, so `devmgr` unloads their drivers, and the
      HCD exits (85) with the reason logged; one whose wait-set fails does the
      same (86).
    Host regressions: `controller_faulted_reports_hse_and_halt_and_recovery_clears_it`,
    `a_controller_reset_serves_an_unchanged_topology_under_the_same_identities`,
    `a_device_found_on_another_port_after_a_reset_has_another_identity`, the
    `interfaces` tests (`a_device_a_reset_moved_to_another_index_keeps_its_node_and_buffer`,
    `devices_a_reset_reordered_keep_their_nodes`,
    `a_moving_node_never_takes_an_index_another_node_serves`,
    `a_device_replugged_where_one_left_is_published_on_a_region_no_node_carried`,
    `a_node_is_published_only_once_its_endpoint_is_drained`,
    `a_device_leaving_on_the_submit_path_recovers_the_controller_it_halted`,
    `a_controller_that_misses_its_grace_window_retracts_every_node_and_is_never_reset_again`
    and `stopping_retracts_every_node_and_answers_its_held_urb`), the
    recovering `UrbService` tests
    (`a_report_poll_during_recovery_waits_for_the_reset_without_touching_the_controller`,
    `a_transfer_during_recovery_is_held_without_touching_the_controller`,
    `a_reset_answers_a_transfer_it_discarded_reissuably`),
    and `devmgr`'s
    `a_vanished_child_is_unloaded_at_once_even_while_its_owner_is_recovering`.
    The sequencing is host-tested in `interfaces`; the freestanding serve loop
    only drives it over the live engine.
  - **A re-plugged device reloads its class driver (the "unplug seen, re-plug
    never reloads" fix).** With the controller recovery above, the HCD
    correctly retracts the interface node on unplug and re-emits it on
    re-plug, but the re-plugged keyboard produced no input because `devmgr`
    never reloaded the class driver. Root cause was in the kernel driver-load
    path, not USB: the user-space load mechanism reported the **driver host's
    per-instance handle counter** (always `1`, since a fresh `Host` is built
    per load) instead of the spawned process id. `devmgr`'s hot-removal diff
    (`unload_vanished`) keys teardown on that handle and skips a driver while
    *any other* bound node shares its handle — so with every driver reporting
    handle `1`, the vanished keyboard's driver was never torn down and its
    loaded-bundle cache never purged, so the re-emitted node reused the stale
    handle and was never re-spawned. (The same handle was also wrong for the
    teardown seam, which resolves it as a PID.) `SpawnDriverLoader::load` now
    returns the unique spawned PID as the driver handle. Host regression: the
    store-server `a_load_spawns_the_matched_signed_driver_with_the_nodes_resources`
    test asserts the reported handle is the spawned PID, not the host counter.
  - **Metal acceptance (UM):** attach → keystroke, detach → `usb_hid`
    unloaded (controller stays up), re-attach → autoloads again, cold boot
    with the keyboard unplugged then plugged in, and a controller reset under
    a live keyboard that keeps typing with its driver unreloaded — inherently
    metal-only (QEMU models no Pi USB, §0.4). Update the `README.md` matrix on
    metal sign-off.

- **U6 — broken-device isolation** (live path metal-only).
  - **A failed downstream attach drains the port's latched changes.**
    `attach_hub_port` and `attach_downstream_device` clear the port's whole
    latch set (best-effort) on the failed path too, so the hub's
    status-change endpoint never re-reports a stale change and a broken device
    costs one surfaced error per genuine change, never a multi-second
    enumeration loop starving every other port.
  - **The hot-plug scan is per-port fail-soft.** `process_hub_change` drains
    and skips a failing port, services the remaining changed ports, and
    surfaces the first failure only when no actionable event was found, so a
    broken device's connect never starves a healthy device's event in the same
    report.
  Host regressions (a scripted boot mouse on its own port with a second,
  slot-attributed interrupt endpoint, and a port-never-enables knob):
  `bring_up_serves_a_keyboard_and_a_mouse_behind_the_hub_together`,
  `a_failing_port_at_bring_up_never_costs_the_keyboard_its_service`,
  `a_failed_hot_plug_attach_drains_the_port_latches_so_the_watch_stays_quiet`.

- **U7 — composite (multi-interface) devices** (live path metal-only). One
  physical USB device may carry several functions — the
  motivating hardware is a wireless keyboard+mouse receiver whose single
  configuration holds a boot-keyboard interface *and* a boot-mouse
  interface. Before this, `InterfaceInfo` decoded only the first interface
  (the second function was structurally invisible), the 64-byte
  configuration read truncated a two/three-interface configuration
  mid-descriptor, and one device-table entry existed per device — on metal
  the receiver's attach faulted (`hub status-change service failed
  err_hex=a`) and booting with it connected cost the keyboard its service.
  The engine now realises §1.1's "one node per interface" for composite
  devices:
  - `InterfaceInfo::decode_all` decodes **every** default-alternate
    interface (bounded by `MAX_INTERFACES`, alternate settings skipped, an
    interface with neither an interrupt-IN endpoint nor a bulk pair dropped so
    a servable sibling is still served; an endpoint descriptor for endpoint
    0, an endpoint named twice, and a second default setting of an interface
    number already taken are skipped, the last with its endpoints); the
    control-data buffer (`CTRL_DATA_LEN`, one page) holds a composite
    device's whole configuration in one read.
  - `finish_enumeration` plans one device-table entry per servable
    interface: the primary at the caller's index, each sibling at its own
    free index and ring region while **sharing the device's slot and its
    control pipe** by reference, never by copy. Every Configure Endpoint
    carries Context Entries covering the highest served DCI, and
    `SET_CONFIGURATION` runs once; the (slot, DCI) event routing and the
    per-index report/bulk paths serve each sibling as their own.
  - `detach_device` frees **every** entry sharing the vanished device's
    slot, so one physical unplug retracts all of its interfaces; the HCD
    reconciles all published nodes against the live table by identity
    (`interfaces::Interfaces::reconcile`, each node matched to the index now
    serving its device) after any hub
    event, fault detach, or reset/re-enumeration instead of touching a single
    index.
  - Host regressions (the mock gained a composite fixture whose 75-byte
    configuration exceeds the old 64-byte read plus an alternate-setting
    decoy, and a `composite_downstream_port` knob capturing the same slot's
    second interrupt endpoint):
    `interface_info_decodes_every_interface_of_a_composite_device`,
    `an_interface_with_nothing_to_serve_leaves_its_sibling_served`,
    `interface_info_bounds_the_decoded_interface_set`,
    `bring_up_serves_both_interfaces_of_a_composite_receiver`,
    `unplugging_a_composite_receiver_frees_both_interfaces_and_a_replug_reserves_them`,
    `a_composite_receiver_beside_the_keyboard_costs_it_nothing`.

- **U8 — EP0 max-packet discovery + exact-length descriptor reads** (live
  path metal-only). Enumeration assumed the speed's
  worst-case EP0 max packet (full speed → 64) for every EP0 transfer, but
  a full-speed device may legally use 8/16/32 — the real wireless
  receiver reports `bMaxPacketSize0` = 8, so its 18-byte device-descriptor
  read terminated short at the first 8-byte packet and the attach faulted
  (`hub status-change service failed err_hex=a` at hot-plug; at boot the
  failed walk left no device served). The engine now performs the
  standard fix-up every production stack does:
  - `finish_enumeration` first reads the 8-byte descriptor prefix (one
    packet at the smallest legal EP0 size, so it completes at any real
    size), validates `bMaxPacketSize0` against the speed
    (`ep0_max_packet_from_descriptor`: low 8, full 8/16/32/64, high 64,
    SuperSpeed exponent 9; anything else `BadMagic`, fail-closed), and
    issues an **Evaluate Context** (`TrbType::EvaluateContext`, A1-only
    input context, xHCI §4.6.7) whenever the honest size differs from the
    Address Device assumption — only then the full 18-byte read.
  - The configuration descriptor is read at its exact advertised length:
    a 9-byte header read for `wTotalLength`, then precisely that many
    bytes (clamped to `CTRL_DATA_LEN`) — never an over-long request a
    buggy device might mishandle.
  - The mock models the physics (per-slot programmed EP0 max captured at
    Address Device / Evaluate Context; a mismatched descriptor read
    delivers one device-sized packet), the composite fixture is a
    full-speed device with `bMaxPacketSize0` = 8 like the real receiver,
    and regressions pin the behaviour:
    `bring_up_serves_both_interfaces_of_a_composite_receiver` /
    `a_composite_receiver_beside_the_keyboard_costs_it_nothing` (exactly
    one Evaluate Context for the receiver, none for the 64-byte keyboard),
    `a_forged_ep0_max_packet_fails_closed_without_costing_the_keyboard`,
    `ep0_max_packet_validation_follows_the_speed_rules`.

- **U9 — multi-tier hubs (a hub plugged into a hub)** (live path
  metal-only). The engine tracks every hub in a growable table
  (`lib/usb::device::HubState`, bounded only by the controller's slot
  count, `MAX_HUB_DEPTH` = 5 route-string tiers — the protocol-fixed
  Route String depth), not a single implicit tier:
  - **Topology is per hub, never assumed.** Each hub records its parent
    hub + port, its Route String, depth, speed, and the TT coordinates its
    slot context carries. A child extends its parent's route by one nibble
    (`route_for_child`, fail-closed on port 0/>15 and on exceeding the five
    tiers), and a full/low-speed child splits through the TT of the nearest
    **high-speed** ancestor: the parent hub itself when high-speed, else
    the coordinates the parent inherited (§6.2.2/§8.9).
  - **A downstream device that enumerates as a hub is installed, marked,
    and descended** (`install_hub` → `descend_hub`): its ports are powered
    and scanned (per-port fail-soft, exactly like the bring-up walk — and
    a tier whose descent fails is torn down whole, never left
    half-installed), and its own interrupt-IN status-change endpoint is
    configured and armed on its own layout region
    (`Layout::hub_regions`). Its slot contexts stay on the device region it
    was enumerated on, which it claims for its lifetime
    (`HubState::device_region`), never handed to another device.
  - **Every tier is watched concurrently.** The event-ring routing sends
    each status-change completion to its hub (`hub_async_index`, per-hub
    parking slots); the service step serves every hub that reported and
    re-arms its watch first. Hub-class requests go on the hub's own control
    pipe.
  - **Unplugging a hub cascades.** The parent's port disconnect tears down
    the hub and everything behind it — served devices and deeper hub tiers
    recursively (`detach_hub`) — as `HubEvent::HubDetached`; a hot-plugged
    hub is installed and descended in place as `HubEvent::HubAttached`.
    The HCD reconciles its published interface nodes against the live
    device table for both, so class drivers autoload/unload per device
    exactly as for a leaf hot-plug.
  - Host-proven over the register-level mock, which gained a **nested-hub
    bank** (`with_nested_hub`: a high-speed hub on the root hub's port 3
    with a full-speed keyboard on its port 2; per-EP0-slot routing of
    hub-class requests, nested marking/status-endpoint capture, nested TT
    validation at Address Device, `post_nested_hub_status_change`):
    `bring_up_serves_a_keyboard_behind_a_nested_hub` (route string `0x23`
    + nested TT + end-to-end report),
    `hot_plug_on_a_nested_hubs_port_attaches_and_detaches_through_its_own_watch`,
    `unplugging_a_nested_hub_cascades_and_a_replug_rebuilds_the_tier`,
    `route_for_child_extends_one_nibble_per_tier_and_fails_closed`.

- **U10 — each node gets a fresh shared buffer; a reused endpoint refuses the
  previous node's driver.** Every interface node the HCD publishes carries a
  shared buffer created for it and carried by no other: `hw_remove_node`
  revokes the node's grants, from its driver and anything it delegated them
  to, and retires its regions (`plans/OPEN-DEFECTS.md` D230), and the HCD
  unmaps its own copy, so the previous driver keeps only a buffer that
  carries nothing of the new device. The device-table index's call endpoint
  is reused: the previous driver's grant for it is revoked before the
  removal returns, and the HCD answers the held URB and every queued one
  `NotFound` before it publishes another node on it, so nothing the previous
  driver posted, before or after the removal, reaches the new device.

- **U11 — the host controller is class-neutral** (`plans/HID.md` H4). The HCD
  serves any interface with an interrupt-IN endpoint or a bulk pair and holds
  no knowledge of the class above it; HID is parsed and configured in
  `drivers/input/usb_hid` (`plans/HID.md` H5).
  - **Interrupt-IN reports are raw and sized by their class driver.** The
    transfer length is fixed at the interface's first interrupt-IN URB, as the
    larger of that request and one service interval's payload, and a later URB
    naming a different length is refused; nothing is armed before that first
    request. A report up to `INT_TRANSFER_MAX` (one page) arrives whole, and a
    full packet never babbles. The endpoint context carries the payload its
    speed and descriptors allow (`PeriodicShape::payload`): `wMaxPacketSize`
    at full and low speed, times the high-bandwidth transaction count at high
    speed, and the SuperSpeed companion's burst and `wBytesPerInterval`.
  - **Control requests are scoped to the node's own interface**
    (`transport::control_permitted`): standard device reads, standard
    interface reads naming the interface, `GET_STATUS` on its own endpoints,
    and class or vendor requests addressed to the interface or one of its
    endpoints. Anything else — a `SET_CONFIGURATION`, a `SET_ADDRESS`, a
    request naming a sibling — is refused `PermissionDenied` and logged
    (event 27149), so a class driver cannot reconfigure the device its
    siblings share. `wLength` must equal the URB length and the direction must
    match. The control data stage is one page (`CTRL_DATA_LEN`).
  - **The interface number is a node property** (`HwProperty::UsbInterface`),
    a fact the kernel carries without a grant, read by the class driver
    through `RtDriverHost::property`.
  - **No transfer buffer crosses a page**, so none crosses the 64 KiB boundary
    a TRB may not span (xHCI 1.2 §4.11.7.1).
  - **A `SuperSpeed` bulk endpoint bursts what its companion states**
    (`BulkEndpoint::burst`, at most sixteen packets); a companion completes
    only the endpoint descriptor it immediately follows.
  - Regressions:
    `a_superspeed_bulk_endpoint_bursts_what_its_companion_states`,
    `a_companion_completes_only_the_endpoint_it_follows`,
    `the_first_report_request_fixes_the_transfer_length`,
    `nothing_is_armed_before_the_class_driver_asks`,
    `a_report_longer_than_a_packet_arrives_whole`,
    `a_periodic_endpoint_moves_what_its_speed_and_descriptors_allow`,
    `a_mouse_is_polled_at_its_own_interval_with_its_own_payload`,
    `no_transfer_buffer_crosses_a_page_so_none_crosses_64_kib`,
    `a_class_driver_reaches_its_own_interface_and_never_the_device`,
    `a_configuration_longer_than_512_bytes_is_read_whole`.
- **U13 — a bulk transfer is bounded, and its pipe always recovered.**
  - **Every bulk URB carries a deadline** (`UrbRequest::timeout_ms`, refused at
    decode when absent, as a control transfer's is (U14); a report poll may
    carry one). The HCD holds a URB no longer than it (`UrbService::expire`):
    its pipe is stopped and restarted empty (`recover::abort_bulk`), and the
    URB answered `TimedOut`, logged (event 27153) with the EP State its
    endpoint had at the deadline. Where the controller stood against the TD
    is read only once the stop has made the context's dequeue pointer the
    controller's own, before the ring is rebuilt
    (`device::AbandonedTransfer`, event 27154): still on its first TRB or a
    later one — the device was working on it — on the TRB after it — the
    host lost its completion — or elsewhere. A control transfer abandoned at
    its deadline is read the same way. The serve loop's park includes the soonest deadline, and a
    timer wake drives every held URB before expiring any, so a completion the
    controller already has is delivered rather than abandoned.
  - **Every halting completion is recovered in place**, not only a STALL: a
    transaction error or babble halts the endpoint just the same, and a pipe
    left halted runs nothing again. A STALL surfaces as `EndpointStalled`,
    anything else as `DeviceFault`, on a pipe that works.
  - **`ClearHalt`** (`UsbRequest`, op 7) clears the halt of one of the
    interface's own bulk endpoints on the device and in the controller; the
    controller's toggle of an endpoint that is not halted resets only by a
    Configure Endpoint that drops and re-adds it (xHCI §4.6.8). A raw
    `CLEAR_FEATURE` stays refused: it would reset the device's toggle alone.
  - **`DiscardReports`** (op 8) drops what the interface's interrupt-IN
    endpoint has captured and not yet delivered, the controller's pending
    events included. The endpoint stays armed whatever URB is held, so a CBI
    completion, which carries no tag, raised late by an earlier command would
    otherwise answer the next command's poll; CBI discards before each ADSC.
  - **Abandoned TDs never complete into a later transfer.** A Stop Endpoint's
    Stopped completion is dropped as the abandoned TD's, parked completions
    are dropped only once the stop is confirmed (the rebuilt ring reuses the
    old one's addresses), and a completion the ring cannot account for
    restarts the pipe and fails its TD.
  - **Diagnostics name the endpoint that failed**: event 27153 carries the
    completion code of the transfer's own endpoint, and a transfer refused at
    once is logged as a held one would be.
  - The class side: every mass-storage exchange is bounded, and a broken one
    runs its transport's own reset recovery (`plans/DEVICES.md` D2).
  - Regressions:
    `a_bulk_transaction_error_recovers_the_pipe_in_place_so_the_next_transfer_runs`,
    `a_bulk_transfer_abandoned_at_its_deadline_frees_the_pipe_for_the_next`,
    `clearing_the_halt_of_a_running_pipe_restarts_both_data_toggles`,
    `a_completion_the_ring_cannot_account_for_restarts_the_pipe_and_fails_its_td`,
    `a_bulk_poll_hands_another_consumers_event_on_rather_than_failing`,
    `a_held_bulk_urb_is_abandoned_at_its_deadline_and_answered_timed_out`,
    `a_held_urb_the_controller_finished_in_time_is_delivered_not_abandoned`,
    `a_clear_halt_or_report_discard_reaches_the_engine_unless_a_urb_is_held`,
    `discarding_reports_drops_those_captured_or_still_on_the_ring_but_none_sent_later`,
    `a_completion_an_earlier_command_raised_late_is_not_taken_for_the_next_ones`.

The modular USB stack — bus driver → user-space HCD owning one controller and
serving the URB transport → per-interface class drivers (HID, mass storage),
with event-driven hub hot-plug on every tier, recursive multi-tier
hub descent with cascade teardown, fresh re-enumeration, controller-fault
recovery that keeps the devices that come back, per-port failure isolation,
composite (multi-interface) devices served one node per interface, and EP0
max-packet discovery for full-speed devices — is host- and CI-proven; its live
attach/detach/re-attach behaviour is metal-only (QEMU models no Pi USB, §0.4).

---

- **U14 — the engine never waits.** Every procedure that spans a controller
  round trip is an operation, so no device's slow pipe, hung command or
  recovery delays another's report, bulk completion or URB deadline.
  - **Operations.** Each multi-step procedure — a control transfer, a
    command, a port reset, an enumeration, a hub descent, a pipe or endpoint
    recovery, `SetInterface`, `ClearHalt`, a stream stop — is an `async`
    procedure in `lib/usb` (`device_ops`, `device_port`, `device_recover`,
    `device_iso`, `device_urb`) that `UsbHost` runs. Each has an owner (a
    port, a slot's pipe, a device's task, a hub, the walk) and is cancelled by
    owner; its guards give back what it holds — a slot's Disable Slot is
    posted with its regions withheld until the controller confirms, a held
    URB is answered. The engine is one `Rc<RefCell<UsbDevice>>` an operation
    borrows only between waits (`clippy::await_holding_refcell_ref` denies the
    rest), and a borrow that would overlap fails closed with `Busy`.
  - **The service step** (`UsbHost::service`) routes every posted event,
    fires due timers and command deadlines, hands each changed root port and
    each hub-reported port to its operation (re-arming the hub's watch first),
    polls every operation that moved, and repeats while one did, a bounded
    number of rounds. It reports the topology changes, a walk's settle and the
    earliest deadline, which bounds the HCD's park.
  - **Routing, never awaiting.** Transfer events are routed by (slot, DCI),
    command completions by command-TRB address to the posted-command table.
    An event nothing owns is dropped.
  - **Per-slot control pipes.** Each slot's EP0 ring, data buffer and input
    context live in its own region; composite siblings share the slot's pipe
    by reference. The pipe is a FIFO with one TD in flight (a message pipe,
    USB 2.0 §5.3.2.2); a TD that fails is taken back (Reset or Stop Endpoint,
    then Set TR Dequeue) before the next runs, and an operation's transfer is
    bounded by the request ceiling.
  - **Commands posted.** A command the controller has not completed within
    its 5 s budget is aborted by Command Ring Abort (xHCI 1.2 §4.6.1.2),
    failing only its owner; the abort writes the next command's address and
    cycle state whole, so a ring that stops before the upper dword lands keeps
    its place. A ring that does not stop within its abort budget is a
    controller fault. The bus's default address and each slot's input context
    are ticket locks, granted in the order asked.
  - **A disconnect ends an attach at any step.** A port's attach watches for
    its device leaving: a root port's connect change, or a hub port's status
    read showing it gone, ends the attach with `DriverError::DeviceOffline`,
    and the port's next pass reconciles what it carries. A reset on a port
    that reads disconnected ends at once, and a hub abandoned mid-descent is
    torn down rather than left half-walked.
  - **Ring reuse is fenced.** A ring is rebuilt only after the controller has
    confirmed its endpoint stopped or reset, so no completion for an old TD
    lands in a slot of the new ring.
  - **Calls are held, never answered inline.** A control URB carries its
    class driver's deadline, held no longer than
    `USB_CONTROL_TIMEOUT_MAX_MS` (`lib/usb` `CONTROL_BUDGET_MAX` for the
    class side); at its deadline its TD is taken back and the pipe serves the
    next. Each class driver states its budget: `usb_hid` and `usb_uac` the
    ceiling, `usb_msd` what its command exchange has left. Interface
    operations the engine runs are held as URBs are; a URB that faults is
    held while the engine looks at whether its device left. While a
    controller reset's walk runs every call is held, and answered once it
    settles: reissuable where the engine had been asked for it, driven
    afresh where it had not.
  - Bounded hardware handshakes inside a controller reset (`HCRST`, `CNR`)
    remain bounded MMIO polls that fail closed.
  - Host regressions in `lib/usb` `tests/operations.rs`:
    `a_control_urb_its_device_never_answers_holds_up_nothing_else`,
    `one_device_stalled_mid_enumeration_holds_up_none_beside_it`,
    `an_unplug_at_every_enumeration_step_leaks_nothing`,
    `an_unplugged_hub_at_every_step_leaves_no_tier_behind`,
    `a_hub_whose_connection_bounces_at_every_step_comes_back_whole`,
    `an_unplug_behind_a_hub_ends_the_attach_at_once`,
    `a_hung_command_is_aborted_failing_only_its_owner`,
    `a_controller_reset_mid_enumeration_ends_every_operation_and_leaks_nothing`,
    `a_late_answer_to_an_abandoned_control_td_never_completes_the_next`,
    `an_unplugged_device_leaves_no_answer_behind_for_its_held_urb`; and in the
    HCD, `a_control_urb_is_held_no_longer_than_any_device_may_take`,
    `an_interface_operation_is_held_until_the_engine_finishes_it`,
    `an_interface_operation_the_engine_runs_is_held_until_it_finishes`.

## 4. Out of scope (explicitly)

- HID applications the seat has no channel for (consumer and system controls,
  gamepads, vendor collections): `plans/HID.md` non-goals.
- A second host-controller driver (a non-xHCI controller): the architecture
  admits it (it binds a different controller node and serves the same URB ABI),
  but none is planned here.

**Isochronous transfers are in scope, specified by `plans/SOUND.md` (SND6),
whose USB audio driver is their first consumer.** They extend this plan's seam
rather than depart from it — the transport carries the interface and stream
operations beside URBs — and are shaped for a periodic endpoint in general, so
the next consumer (a camera) reuses them. Read that plan before touching the
transfer path.
