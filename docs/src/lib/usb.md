# `tairix-usb`

`lib/usb` is the **bus-agnostic xHCI USB host-controller protocol**: the
host-provable, controller-agnostic layers of an xHCI stack, with no PCI or
board coupling. It is the USB analogue of `lib/virtio` — the protocol lives in
`lib/` so more than one crate can consume it (`AGENTS.md` §2.2 / §6 / §17.4),
and it depends only on `lib/*` crates (`lib/abi`, `lib/dma-barrier`,
`lib/inline`), so it builds for every Tier-1 target and is
identical on `aarch64`, `x86_64`, and `riscv64` (the USB protocol does not vary
by architecture).

## Why it exists

The USB host-controller protocol used to live inside `drivers/bus/usb`. But the
§17.4 layering forbids a `drivers/*` (or `userland/*`) crate from depending on
another `drivers/*` crate, so a class driver could not share the URB transport
while it sat in the bus driver. In `lib/usb`, the host-controller driver
(`drivers/bus/usb`, which adds the PCI discovery/BAR/DMA wiring and the §8
`register` entry) and the class drivers build on the *same* protocol without
depending on each other — the split `lib/virtio` ↔ `drivers/bus/virtio` uses.

## What it provides

- `RegisterBlock` (from `tairix_abi`) — the register seam every controller
  access goes through.
  On metal it is a capability-gated `RegisterWindow` whose base the hardware
  tree discovered (PCI BAR assignment, never a compiled-in constant, §18.1); in
  host tests it is a register-level mock controller.
- `Xhci` — the controller engine. `open` validates the capability block and
  runs the xHCI 1.2 §4.2 prologue (halt, clear latched status, Host Controller
  Reset, wait ready); `start` programs the DMA structures (`DCBAAP`, command
  ring, interrupter-0 event ring) and runs the controller; `begin_port_reset` /
  `clear_port_reset_change` / `set_port_power` / `ring_doorbell` / `ack_event`
  drive the root hub and rings. Awaiting a reset it started is deliberately
  *not* this layer's job — that needs a clock and the controller's interrupt,
  which `UsbDevice` owns (`await_root_port_reset_complete`).
- `device::UsbDevice` — the multi-device enumeration engine: per device,
  Enable Slot → Address Device → an 8-byte `GET_DESCRIPTOR` prefix whose
  validated `bMaxPacketSize0` drives an Evaluate Context EP0 fix-up when
  it differs from the speed's assumed worst case (a full-speed receiver's
  8-byte EP0) → the full `GET_DESCRIPTOR` reads (the configuration at its
  exact advertised `wTotalLength`, up to `CTRL_DATA_LEN`) → Configure
  Endpoint → `SET_CONFIGURATION`. It knows no device class: a class driver
  reads its own interface's descriptors, sends its own class requests, and
  receives its interrupt-IN reports exactly as the device sent them. Every
  interface with an interrupt-IN endpoint or a bulk pair is served, whatever
  its class, and so is one driven over the control endpoint alone — one
  setting that brings no endpoint (`alternate::is_control_only`), a USB Audio
  1.0 control interface — while an interface with settings to choose between
  is left for a served sibling to claim. The interrupt-IN transfer is armed only once the class driver's
  first report request names the longest report it expects, to that or to one
  service interval's payload (`PeriodicShape::payload`: the packet times the
  high-speed transactions or `SuperSpeed` burst) if longer, and never more than
  a report needs: a full/low-speed endpoint behind a high-speed hub's
  transaction translator faults with a Split Transaction Error when a transfer
  outruns the interval budget the TT scheduled. Captured reports are queued
  per device in memory sized to that length (`REPORT_QUEUE_CAP` deep, the
  oldest dropped and counted when a consumer stalls). An interrupt endpoint's
  context carries its own interval, Max Burst Size, Max ESIT Payload and a
  Max Packet Size held to its speed's maximum, so no transfer outruns its
  buffer. A `SuperSpeed` bulk endpoint's carries its companion's burst
  (`BulkEndpoint::burst`), so a USB 3 storage device moves up to sixteen
  packets a burst.
  Enumerated interfaces go into a growable table of concurrently served **interfaces**, each with its own
  demand-allocated DMA region (EP0 / interrupt / bulk rings and buffers)
  claimed on attach and released on detach — the only concurrency bounds
  are the controller's reported slot count and genuine memory exhaustion,
  never a compile-time budget. A composite
  device — a wireless keyboard+mouse receiver — occupies one entry per
  served interface, the siblings sharing its slot and EP0
  (`InterfaceInfo::decode_all` decodes every default-alternate interface).
  `next_report(index, …)` arms one
  interrupt-IN transfer for the class URB device `index` is currently serving,
  and `engine_for(index)` is the per-device `UrbEngine` view the HCD's URB
  service drives — one interface's transfers can never reach another
  device's endpoints. Endpoint DCIs, packet sizes, and intervals are read
  from each device's descriptors (never hard-coded); an endpoint descriptor
  naming endpoint zero, or an endpoint the configuration already named, is
  skipped, so no endpoint is configured over another's context or the
  default control endpoint's, and a second default setting of an interface
  number already taken is skipped with its endpoints. A *successful*
  zero-length completion (a ZLP — an idle or composite HID interface, e.g. a
  wireless MMO mouse's extra collection, completing an armed transfer with no
  data) is not a report and not a fault: `next_report` re-arms the endpoint
  and returns `Ok(None)`, so the URB stays parked and a ZLP costs one
  controller interrupt rather than a reply-and-resubmit spin; a genuine
  per-report fault still surfaces after the ring is retired.
  `bring_up` is the arch-neutral bring-up orchestration the host-controller
  driver runs once: it powers all root ports, parks through the connect
  debounce, and attaches **every** connected root port (`attach_root_port`).
  A root device that is itself a hub (the Pi 4B's onboard USB2 hub) is
  installed, descended — every connected downstream port, nested tiers
  included — and watched; a directly-attached device (the Pi 4B's USB3 side
  of each jack is wired straight to a root port) is served beside it (settle
  windows supplied by the `tairix_abi::Delay` seam) — a keyboard and a
  storage stick plugged in together are both served, neither displacing the
  other. A transaction fault while a device's address is assigned or its
  device and configuration descriptors are read re-drives it on a fresh slot,
  up to `ENUM_ATTEMPTS` times, each after resetting its port: a device that
  took its address answers no fresh slot's `SET_ADDRESS` until a reset returns
  it to Default state. A port whose device fails enumeration is skipped with
  its slot released, never allowed to cost the other devices their service —
  including when *every* connected port fails, which is a controller serving
  nothing rather than a bring-up error, so one unservable device can never
  take the controller and its watches down with it. `retry_skipped_ports`
  re-drives every connected-but-unserved port (a served port is left
  untouched: the re-drive resets the port, which would tear a working device
  down), so the HCD can owe a skipped port one deferred re-attach rather than
  wait for a physical re-plug. A device
  absent at bring-up is a first-class state, not a failure: the controller
  comes up watched (each hub's status-change endpoint, and the root ports'
  latched connect changes serviced by `next_root_change`, with no controller
  reset), so a cold boot with nothing plugged in works and each device
  autoloads when plugged in. The `PORTSC` walk runs only when a latched
  source has armed it — a Port Status Change Event drained by any ring
  consumer, or the `USBSTS.PCD` summary the interrupt acknowledgement reads —
  so a device streaming reports never touches a port register, while neither
  trigger can lose a plug. Both are xHCI-mandated latches, and the arming is
  recorded at the one point every ring consumer funnels through, so a plug
  whose event was swallowed by a synchronous engine wait is still scanned for.
  `device_identity(index)` names each served interface by its bus position
  (root port and Route String) and descriptor identity (vendor, product,
  `bcdDevice`, device class triple, and the interface's number and class
  triple) — a `DeviceIdentity` that `reset_and_reenumerate` reproduces for a
  device still on the same port, at whatever index the fresh walk gives it, so
  the host-controller driver matches it across indices and keeps what it
  published for that device across a controller reset; whether a device the
  reset's walk served is the one a node was published for is
  `DeviceIdentity::recognises`'s to decide, where within one enumeration an
  identity is its device's exactly when equal. A device serving a
  mass-storage interface also carries its serial number (`SerialNumber`: the
  string's UTF-16 code units, exactly), which is what tells two sticks of one
  model apart: a storage interface without one is never recognised after a
  re-enumeration, since its driver bound to another medium corrupts it, while
  a device of another class, whose serial is never read, is recognised by model
  and position. The serial is read when `iSerialNumber` is non-zero — the
  LANGID table, then the string in the first language listed, each descriptor
  header first and then at exactly the `bLength` it claims — and is optional
  identity: a refusal, any fault the control endpoint is taken back from, or a
  malformed or empty answer leaves the identity without one and the
  enumeration goes on, never re-driven. What the answers may be is decided by
  pure decoders alone — `StringHeader`, `first_langid`,
  `SerialNumber::decode` — which the transfer path calls. `describe_device`
  gives each interface node the device's bus position as its address, so a
  node kept across a reset still agrees with a sibling published after it, and
  no device published beside it shares its address.
  The event ring is up to four page-sized segments, as many as the
  controller's `ERST Max` takes (QEMU takes one, the VL805 eight): an
  isochronous stream posts an event per service interval, and a page is the
  most one segment can be and still never cross a 64 KiB boundary.
- Alternate settings and isochronous streams, on the same engine. A node governs
  its own interface — when it carries none of the engine's own pipes — and
  any sibling it claims (`claim_interface`: an interface of the same device
  no node serves). `set_interface` reserves the setting's isochronous
  endpoints with the controller first — one Configure Endpoint dropping the
  old setting's endpoints and adding the new, a Bandwidth Error answered
  `NoBandwidth` with nothing changed — and only then sends `SET_INTERFACE`;
  a device that refuses gets its old setting back. A setting bringing a
  non-isochronous endpoint is `Unsupported`.
  A stream (`iso_start`) is a fixed set of slots, each spanning a number of
  service intervals. A queued slot's TDs all go onto the endpoint's one-page
  ring at once, each placed at the frame it is due in: a fresh stream, or one
  queued too late, restarts on the first frame — and service interval — it
  can still make past the controller's scheduling threshold and a frame's
  lead, and the intervals it jumped are the slot's `skipped`; a controller
  without CFC runs a busy ring back to back whatever its Frame IDs say, so
  there a late slot follows on and the controller reports what it misses.
  Every TD interrupts on completion and all but a slot's last block the
  interrupt, so each interval is accounted for exactly — moved (an IN
  interval with its received length), missed (a Missed Service Error, an
  underrun, or a TD the controller passed without its own event) or failed —
  while a slot raises one interrupt. A TRB-level error halts the stream. A
  layout must fit one ring and the controller's 895-frame window. Microframes
  are counted past `MFINDEX`'s 2.048 s wrap against the monotonic clock. A
  stopped stream's buffers return once the controller confirms the stop; a
  departing device's settings and streams go with its slot.
- `periodic` — what an endpoint descriptor says (`EndpointDescriptor`, every
  field read, the audio-class nine-byte form included), the `ServiceInterval`
  and `PeriodicBudget` it means at a bus speed, and the arithmetic a stream
  runs on: `FeedbackDecoder` reads explicit feedback (10.14 per frame at full
  speed, 16.16 per microframe above), fixing its format — the specification's
  or one of a few shifts devices in the field send — on the first report
  within an eighth of the nominal rate and refusing any later one outside it;
  `FeedbackDecoder::implicit` reads the rate an implicit-feedback device's own
  data packets carried, held to the same window;
  `PacketPacer` spreads a rate over intervals in whole frames with an integer
  remainder, so an hour of 44.1 kHz ends on exactly the frame it should, and
  `advance` sums any number of intervals in one step for a gap's accounting.
- `alternate` — a configuration's alternate settings (`alternate_setting`:
  every endpoint and `SuperSpeed` companion a setting brings, a forged
  setting refused), its interface numbers, `is_control_only`, and interface
  associations.
- `regs` / `trb` / `ring` — the register, TRB, and ring-state vocabularies; the
  ring state machines (`ProducerRing`, `EventRingCursor`) hold no memory of
  their own, so the owner publishes every write through the `device::DmaBank`
  seam. A TD that continues past a ring's wrap carries its Chain bit through
  the Link TRB.
- `SlabBank` — the production `device::DmaBank`: a growable bank of owned DMA
  chunks minted from the host's `DmaHost` seam. The engine's first chunk holds
  the controller-shared structures, sized exactly to the reported geometry
  (`MaxSlots`, context size, the VL805's 31-page scratchpad); every served
  device's rings/buffers live in a chunk grown on attach and released on
  detach, and each allocation is verified against the controller's inbound
  DMA aperture, failing closed on a chunk the silicon could not reach (§2.2,
  §24.1). A chunk the controller may still reach — a slot whose Disable Slot
  went unconfirmed — is withheld, and returned by that command's late
  confirmation or by a confirmed controller reset.
- `XHCI_COMPATIBLE` — the `compatible` identity (`usb,xhci`) a discovered xHCI
  controller node carries (§18.1). An xHCI-protocol identity (not a board or
  vendor name), so it lives here as the single definition the emitting bus
  driver (`drivers/bus/usb/vl805`, which publishes the controller node under
  it) and the binding host-controller driver (`drivers/bus/usb/xhci`'s
  `BIND_KEYS`) share (§2.2 / §2.20).

- `transport` — the **bus-agnostic URB transport seam** the modular USB stack
  (`plans/USB.md`) is built on. The wire contract is `tairix_abi::usb_urb`: a
  `UsbRequest`, which is a URB (`UrbRequest`: endpoint, transfer type,
  direction, length, control SETUP — its data always moves through the node's
  shared buffer), an interface operation (select a setting, claim a sibling),
  or a stream operation (start, queue a slot, stop); a URB is answered with a
  status-framed completion (bytes transferred, or an in-band `Errno`), an
  operation with a status or a stream's grant. `transport` adds the two ends
  both sides share:
  - `UrbEngine` — the controller-side operation seam the HCD's live engine
    performs (`UsbDevice` implements it: `control_in` over the EP0 control
    transfer — targeting the enumerated *device*, switching a hub-downstream
    device's EP0 ring active for the transfer — `control_no_data` over the
    same path for a SETUP-only class request (the BOT Mass Storage Reset,
    `plans/DEVICES.md` D2), `control_out` for a class request carrying an
    OUT data stage (the CBI ADSC command channel, `plans/DEVICES.md` D5),
    `interrupt_in` over the report queue — a HID report endpoint or a CBI
    interface's completion endpoint alike, read only when the URB names it —
    and
    `bulk_in` / `bulk_out` over the interface's configured bulk endpoints:
    the IN/OUT pair a BOT/CBI interface carries, or the two pairs a UAS
    interface's four pipes need (`plans/DEVICES.md` D1/D5), addressed by
    endpoint number and routed to the matching per-pipe ring), and the
    interface and stream operations above (`set_interface`,
    `claim_interface`, `iso_start` / `iso_queue` / `iso_stop` / `iso_take`),
    which an engine serving no periodic endpoint refuses `Unsupported`.
  - `drive_urb` — the controller-side server transformation: decode a URB,
    validate it fail-closed against the interface (control ⇒ endpoint 0,
    served as IN, the zero-length no-data OUT, or the data-stage OUT
    carrying the shared buffer's bytes; interrupt/bulk ⇒ one of the
    interface's own endpoints; an oversize length or a malformed frame is
    refused **before** the engine is touched), drive the engine over the
    shared buffer, and frame the completion in band. A control request must
    stay inside the node's `UrbScope` (`control_permitted`) — its own
    interface and those it claimed, and their endpoints in their current
    settings, so a class request to a streaming endpoint (UAC1's sampling
    frequency) is the node's to send: a class
    driver may read the device's descriptors and status and do anything to
    its own interfaces and their endpoints, but never set the
    configuration, the address, an alternate setting, a halt or a power
    feature, which reach every interface of the device — those are refused
    `PermissionDenied`, and the HCD logs each refusal. A not-yet-arrived interrupt-IN report — or a bulk
    transfer still in flight — leaves the HCD's IPC ticket outstanding until
    the controller event arrives, so the class driver parks instead of
    retrying.
  - `UrbCall` / `UrbClient` — the class-side client: a class driver implements
    `UrbCall` over the kernel `ipc_call` surface (a host test routes the bytes
    straight to `drive_urb`), and
    `UrbClient::{control_in, control_no_data, control_out, interrupt_in,
    bulk_in, bulk_out}` build the URB,
    submit it, and decode the completion;
    `UrbClient::{set_interface, claim_interface, iso_start, iso_queue,
    iso_stop}` send the operations. A class driver speaks only
    this ABI, so the same binary works behind any controller that serves it —
    it touches no controller register and no other interface's buffer (§5.4,
    `plans/USB.md` §1.3). An interrupt-IN completion may carry more than the
    request named, up to one service interval's payload, so the report lands
    anywhere in the shared buffer.
  - `UrbLink` — a class driver's link to its interface: the client and its
    mapping of the shared buffer, moving each transfer's bytes in and out,
    splitting bulk transfers into buffer-sized URBs, and refusing an
    interrupt report too long for the caller's buffer rather than truncating
    it. The mass-storage and HID class drivers both drive their interface
    through it.
  - `descriptor::descriptors` — the one walk over a configuration descriptor
    stream every reader of one shares (this crate's decoder, the
    mass-storage, HID and USB Audio class drivers): each descriptor its
    `bLength` bytes, a trailing fragment ending the walk, a malformed one
    refused. `ConfigurationHeader` is the one reading of the header that
    opens it: a `bLength` shorter than the header, or a `wTotalLength`
    shorter than the header it includes, is malformed.
  - `transport::read_configuration` — a class driver's read of its device's
    whole configuration descriptor: the header for the stated total, then
    exactly that many bytes. A stream longer than one data stage is refused
    rather than read cut short, since a truncated one ends mid-descriptor.
  - Bulk endpoints are served through per-pipe transfer rings with
    per-slot staging buffers (several TDs may be outstanding per pipe,
    completing in order; a UAS interface's second pair shares the
    direction's staging buffers — the URB service holds one URB in flight
    per interface, so the pipes never race on them), short packets report
    the honest byte count, and a device STALL is recovered in place —
    Reset Endpoint → Set TR Dequeue Pointer →
    `CLEAR_FEATURE(ENDPOINT_HALT)` on the device's own EP0 — with
    every abandoned TD answered and the stall surfaced as the distinct
    `EndpointStalled`, so a storage class driver can run its own recovery.
    A *control* transfer that does not complete is likewise taken back in
    place before its failure returns — Reset Endpoint from a halt (a STALL,
    babble, or transaction error), Stop Endpoint from a TD the device left
    unanswered past its wait, then Set TR Dequeue Pointer onto a rebuilt EP0
    ring; the device side starts over at the next SETUP — with the observed
    completion code preserved for the diagnostics. A STALL surfaces as
    `EndpointStalled`, the CBI "command not accepted" answer.

## Design

- `no_std` + `alloc`, `#![forbid(unsafe_op_in_unsafe_fn)]`, `lib/*`-only.
- Every controller and DMA access is mediated by the `RegisterBlock` /
  `device::DmaBank` seams, so the bring-up, enumeration, and ring state
  machines are proven host-side against a register-level mock plus an in-memory
  ring/DMA model (§2.2); the doorbell below them is the on-metal acceptance
  item (no QEMU `raspi*` USB vertical exists, §0.4).
- Synchronous completion waits **park** on the caller-supplied
  `device::EventWait` seam (on metal: the HCD's `irq_wait` on the
  controller's bound interrupt line, which the caller binds before
  `UsbDevice::start` — start enables the completion interrupter itself, at the
  1 ms xHCI reset moderation default `regs::IMODI_DEFAULT`, never `IMOD = 0`,
  so an interrupt-IN endpoint that streams a report every service interval — a
  mouse polling every microframe — is coalesced to ≤~1000 interrupts/s rather
  than storming a core with one interrupt per report) and
  are bounded by wall-clock budgets (the USB 2.0 §9.2.6 request ceiling for
  a completion, the power-on-good + attach-debounce window for the boot
  connect scan). Only the brief register handshakes (`Xhci` open/start/
  reset readiness) keep the bounded iteration poll budget.
- Every endpoint is polled at the interval its descriptor states; a device
  that has nothing new to report is quiet because its class driver set its
  idle rate, not because the host slowed it.
- Fail-closed (§2.9): an implausible capability block, an out-of-range port or
  doorbell target, a malformed descriptor, or an exhausted wait budget is a
  typed `DriverError`, never a panic or an unbounded spin (§2.1). The device,
  configuration, hub, string, alternate-setting and endpoint descriptor
  decoders, the isochronous budget and the feedback decoder are fuzzed
  (`fuzz_descriptors`, run by `cargo xtask fuzz`) against a naive model of
  what each may accept.
- The crate holds **no** capability of its own — authority is the consuming
  driver's (`CAP_MMIO_MAP` for the register window, `CAP_MEM_DMA` for the DMA
  carve), checked in the wiring that mints them.

## Stability

Tier: `experimental`.
