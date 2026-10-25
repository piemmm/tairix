# `tairix-drv-bus-usb` — xHCI USB host-controller driver (HCD)

`plans/USB.md` U3b. The loadable, autoloaded **host-controller driver**: the
sole owner of one xHCI controller. It maps the controller's register BAR, owns
its DMA rings and root-hub ports, brings it up, enumerates the attached device,
publishes one hardware-tree node per USB interface once a device is present,
and **serves that interface's transfers** over the bus-agnostic URB transport to
an autoloaded **class** driver (`drivers/input/usb_hid`,
`drivers/storage/usb_msd`). Each node names its interface number
(`HwProperty::UsbInterface`), and a class driver's control requests may reach
no other interface. It names no class driver, no board, and no bus (`AGENTS.md`
§2.20 / §17.4). A device absent at
boot is a first-class state: the controller comes up and waits for the first
hot-plug connect (the onboard hub's status-change watch, or a root-port
connect), so a cold boot with the keyboard unplugged works.

The crate is a `lib` (host-testable logic) **and** a `Run` binary (the process).

The bus-agnostic xHCI **protocol** (the `RegisterBlock` register seam, the `Xhci`
controller engine, the TRB/ring vocabulary, the `UsbDevice` enumeration engine,
and the URB transport `drive_urb`/`UrbEngine`) lives in
[`tairix-usb`](../../../../lib/usb) (`lib/usb`), so this driver and the class
drivers build on the same engine without depending on each other (`drivers/* →
lib/*` only; the USB analogue of `lib/virtio` ↔ `drivers/bus/virtio`).

## What lives here

- `BIND_KEYS` — the bind table: the `compatible(usb,xhci)` key a bus
  driver's emitted node carries, and the xHCI PCI class code (`0x0C0330`), so
  it also autoloads against a function the kernel discovered on a PCI host it
  owns. The class key binds below a driver naming the exact part, which brings
  that function up first.
- `bringup` — `derive_controller_resources` + `bring_up_controller[_diagnostic]`:
  derive the BAR/DMA bounds from the granted resources, carve+aperture-check
  the DMA region (fail-closed `OutOfRange`, §5.4), map the BAR, bring the
  controller up via `tairix_usb::Xhci::open` + `UsbHost::start` (which also
  enables the completion interrupter the event loop parks on), power the
  ports and start their walk (`UsbHost::bring_up`), returning the `UsbHost`
  whose serve loop runs it.
- `serve` — `UrbService`, the per-interface state holding at most one
  outstanding call — a URB, or an interface operation the engine runs over
  several commands (`SetInterface`, `ClearHalt`, `IsoStop`); a second
  concurrent submit fails closed `AlreadyExists`. A call that does not finish
  at once is held and driven again on each service step through
  `tairix_usb::drive_urb` / `frame_completion`, and a URB is abandoned at its
  deadline, a control URB's no later than `USB_CONTROL_TIMEOUT_MAX_MS`. A
  retracted node's held call is answered `NotFound`, as is any submit on an
  endpoint carrying no node. `UrbReply` frames each answer: a URB's
  completion, an operation's status, or a stream's grant. While the
  controller recovers every call is held without touching it, and answered
  once the controller is back: reissuable where the engine had been asked for
  it, driven afresh where it had not (`reissue_after_reset`).
  `attach_transport_grants` adds the URB endpoint + shared-buffer grants onto
  the `describe_device` interface node.
- `interfaces` — `Interfaces`, the published nodes and the transports they
  ride, driven over the `Seam` trait (the live engine and syscalls in the `Run`
  binary, a mock in the tests). `reconcile` keeps a node while the device it
  was built from (`tairix_usb::device::DeviceIdentity`: bus position,
  descriptor identity, serial number) is served, following it to whatever
  index a controller reset gave it; retracts the rest; then publishes the
  unclaimed devices, each on a drained endpoint and a shared buffer created for
  that node alone, since the kernel retires a removed node's regions. A serial
  that no longer reads counts as another device. `serve_submit`, `drive_busy`,
  `recover` and `finish_recovery` hold the call and recovery sequencing: a URB
  that faults is held while the engine looks at whether its device left
  (`Seam::confirm_departure`), answered `NotFound` with the node retracted if
  it did, its own fault otherwise. `serve_submit` decodes each request — a
  malformed frame is answered fail-closed before anything else runs. A started stream rides a region of its own, created for
  it and delegated (`shm_grant_peer`) to the caller that started it, and its
  notifications go to the port the caller's attested pid names, so a class
  driver can neither map another's region nor aim the HCD's wakes at another
  process. Each stream takes a number of its own, which its grant and every
  notification carry, so a notification a stopped stream left behind is never
  read as one about its successor. `deliver_streams` runs after every
  controller interrupt and every
  served request: each finished slot becomes an `IsoNotify::SlotDone`, and a
  stream that halted, or whose notification its port refused, is stopped and
  told why. A departing device's streams end `NotFound` before its node is
  retracted; a controller reset ends every stream with the reissuable
  `WouldBlock`, and the node governs its own interface alone again.
- `domain` — `ControllerHealth`, the controller's interior fault domain (the
  recovery grace window) and the one deferred re-attach owed a port the
  bring-up walk skipped.
- `main.rs` — the freestanding `Run` program: `from_grants_query` → `irq_bind`
  (the controller's interrupt line, **before** the controller is touched, its
  interrupter enabled as part of `UsbHost::start`) → bring-up (a failure logs
  the **whole** diagnostic breadcrumb: phase, error, open stage,
  `USBCMD`/`USBSTS`) → an **asynchronous wait-set event loop** that parks on
  the URB endpoints **and** the controller IRQ, bounded only by the earliest
  deadline an operation, a held URB or a retry owes. Every wake runs one
  engine service step (`UsbHost::service`), which routes every posted event
  and runs every operation it moved: enumerations, port resets, hub service,
  recoveries. Its topology report publishes a node per served interface — a
  grant-restricted `call_create` endpoint (reused across nodes) and a fresh
  `shm_create` buffer, emitted with both grants (`hw_emit_node`) — and
  retracts what left; each settled walk logs a topology summary and warns
  about connected devices that failed enumeration and were skipped. Held calls
  are then driven against what the step did. Root-port and hub-port
  connects/disconnects are each reconciled by their port's own operation,
  with no controller reset, so sibling ports' devices are untouched; a slot
  teardown frees the local slot state at once whether or not the controller
  confirms its Disable Slot, so a re-plug always re-enumerates. Every
  (re)attach publishes a fresh node with a buffer no other node carried, so
  `devmgr` autoloads a class driver that sees nothing of the previous device.
  It never busy-polls (`AGENTS.md` §2.23).
- **Controller recovery keeps the devices that come back.** A controller that
  latches `USBSTS.HSE`/`HCHalted` is reset and re-enumerated with its interface
  nodes left published, and after the walk each node is matched to its device
  by identity wherever the walk placed it (`Interfaces::reconcile`), so an
  unchanged device keeps its node, its id, its buffer, and its bound class
  driver even when an earlier unplug's hole moved it to another index. Node
  ids are never reissued, so this is the only way a device survives the reset
  with its driver. The reset's walk runs in the serve loop, and held calls are
  answered once their device's fate is known, when it settles. A reset that
  fails leaves the nodes published and serves nothing through the controller
  while its grace window runs, every call held; the window's one-shot retries
  the reset.
  If the window elapses first, every interface node is retracted, so `devmgr`
  unloads their drivers, and the HCD exits (code 85) with the reason logged,
  handing the controller's memory to the kernel to quarantine. A node is
  published only after any URB still queued on its endpoint has been answered
  `NotFound`: with no node on it, only a previous node's driver can have posted
  it.

## Least privilege (`AGENTS.md` §5.4)

`CAP_MMIO_MAP` (register BAR), `CAP_MEM_DMA` (controller DMA ring), `CAP_IRQ_BIND`
(completion interrupt), `CAP_SHM` (the URB data buffer and each stream's
region), `CAP_IPC_BIND_PRIVILEGED`
(the restricted-sender URB endpoint), `CAP_HW_EMIT` (publish the interface
node), `CAP_LOG_EMIT` (one-shot diagnostic). It runs in user space and does not
request `CAP_DRV_KERNEL`. The class driver it serves holds **none** of these —
only the right to submit requests on its one interface and map its one buffer
and the stream regions delegated to it.

## Supported hardware

| Platform | Controller                    | Status |
|----------|-------------------------------|--------|
| QEMU (x86_64, aarch64, riscv64) | `qemu-xhci` on the kernel-owned PCI host | enumerates and serves `usb-audio` end to end (`tests/integration/audio_qemu_*`) |

The register window and DMA constraint arrive as grants on the matched node —
never a compiled-in base (`AGENTS.md` §18.1). On a kernel-owned PCI host the
window stops short of the MSI-X table and pending bits, which only the kernel
programs (`PciBus::driver_window`), and the function's DMA grant states no
constraint (`HwResource::dma(0, 0, …)`). A controller QEMU does not model
stays a metal checklist (`plans/PI.md` §0.4).

## Limitations

- Concurrently served devices are bounded by the controller's reported
  slot count (`HCSPARAMS1.MaxSlots`, the same bound the silicon imposes on
  any host) and genuine memory exhaustion: each device's DMA region and
  its URB transport are allocated when it attaches and released when it
  detaches, never a fixed table. Event-driven hot-plug — hub-downstream
  connect/disconnect on any tier, directly-attached connect/disconnect,
  fresh re-enumeration, and cold boot with no device attached — is built
  and host-proven (`plans/USB.md` U5/U9); live attach/detach/cold-boot
  acceptance is metal-only (QEMU models no Pi USB).
- Hubs are descended recursively (a hub plugged into a hub, up to the xHCI
  route string's five tiers): each tier is installed, marked, and watched on
  its own status-change endpoint, an unplugged hub cascades the teardown of
  everything behind it, and a hot-plugged hub is descended in place
  (`plans/USB.md` U9).

## Test surface

The xHCI protocol layers (bring-up, port/doorbell decode, ring state machines,
DMA programming, enumeration, the report path, and the URB transport with its
control-request scope) are tested in `lib/usb` (`cargo test -p tairix-usb`).

`cargo test -p tairix-drv-bus-usb` exercises the pieces here: the `bringup`
fail-closed paths up to the controller hand-off (the inert mock window faults —
the on-metal boundary); the `serve` `UrbService` state machine over a mock
engine (interrupt-IN, bulk and control URBs held until a later event finishes
them; a control URB held no longer than the protocol ceiling; an interface
operation held until the engine finishes it; second-submit `AlreadyExists`;
illegal URBs refused before the engine; every call held without touching the
controller while it recovers, then driven or answered reissuable) plus the
interface-node grant builder; the `interfaces` table over a mock seam that
journals every kernel step and retires a removed node's region (a device a
reset moved to another index, or reordered, keeps its node and buffer; a
moving node never takes an index another node serves; a re-plug at the same
index is published on a fresh region; publication only after the endpoint is
drained and any retraction; every node decided when memory runs out; a
refused node releases its region; an unwatched endpoint watched again, never
re-bound; any identity fact changing replaces the node; serial numbers telling
twins apart; a faulted URB held until its device's departure is settled; a
controller that misses its grace window retracting everything and never being
reset again); and the `domain` grace-window machine.
