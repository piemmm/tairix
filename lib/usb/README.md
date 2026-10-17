# tairix-usb

Bus-agnostic xHCI USB host-controller protocol for TAIRiX (`lib/usb`,
`AGENTS.md` §6 / §2.2 — `plans/PI.md` P10).

The USB protocol is identical on every architecture, so the host-provable,
controller-agnostic xHCI layers live here once, in `lib/`, rather than inside
any one driver. This is the USB analogue of `lib/virtio`: a concrete
host-controller driver (`drivers/bus/usb`, which adds the PCI
discovery/BAR/DMA wiring and the §8 `register` entry) and an arch-neutral
user-space keyboard driver both consume this crate without depending on each
other (§17.4 — `drivers/* → lib/*` only).

## API

- `RegisterBlock` (from `tairix_abi`) — the register seam every controller access goes through
  (metal: a capability-gated `RegisterWindow`; tests: a register-level mock).
- `Xhci` — the controller engine: `open` runs the §4.2 prologue (halt, reset,
  wait ready) and parses the capability block; `start` programs the DMA
  structures and runs the controller; `begin_port_reset` / `set_port_power` /
  `ring_doorbell` / `ack_event` / `abort_command_ring` drive the root hub and
  rings.
- `device::UsbHost` — the engine and the operations it runs: every procedure
  that spans a controller round trip (a control transfer, a command, a port
  reset, an enumeration, a recovery) is an operation with an owner, cancelled
  by owner, its guards giving back what it holds. `service(now)` routes every
  posted event, fires due timers and command deadlines, runs every operation
  that moved, and reports what the topology did, whether a walk settled, and
  the earliest deadline. A command the controller does not complete within its
  budget is aborted, failing only its owner; a device that leaves mid-attach
  ends its attach at once. `engine_for(index)` is the per-interface
  `transport::UrbEngine` a host driver's URB service drives, answering `None`
  while the engine runs a call.
- `device::UsbDevice` — the multi-device enumeration engine (Enable Slot →
  Address Device → descriptors → Configure Endpoint → `SET_CONFIGURATION`),
  knowing no device class: each interface's class driver reads its own
  descriptors, sends its own class requests inside its interface's
  `transport::UrbScope`, and receives its reports as the device sent them.
  Each slot's control pipe — its output and input contexts, EP0 ring, and
  control data buffer, in its own region — runs one TD at a time, so a device
  that answers a timed-out control transfer late writes only that region. A
  control transfer that does not complete — refused, ended by any error, or
  left unanswered past its deadline — has its endpoint taken back before the
  next runs (Reset Endpoint from a halt, Stop Endpoint from a TD still armed,
  then Set TR Dequeue Pointer). Every slot the
  engine gives up — a detach, a failed attach, or a device with no interface it
  serves (refused `Unsupported` before anything is configured) — is disabled
  before its region goes. A re-driven enumeration resets the port first: a
  device that took its address answers no fresh slot's `SET_ADDRESS` until a
  reset returns it to Default state. `device_identity` names each served
  interface by its bus position (root port + Route String), its descriptor
  identity, and — for a device serving a mass-storage interface — its serial
  number (`device::DeviceIdentity`), reproduced by `UsbHost::reset_and_reenumerate` for a
  device still where it was. `DeviceIdentity::recognises` is what a host
  driver keeps a node on across the reset, and it never recognises a storage
  interface without a serial; within one enumeration an identity is its
  device's exactly when equal. The serial is read in the first language the
  device lists, each string descriptor at exactly its advertised length; a
  refused, faulted, or malformed read leaves the identity without one. A
  node's device address (`describe_device`) is the device's bus position, which
  the reset keeps where the slot it reassigns would not.
  It also runs alternate settings and isochronous streams on the interfaces a
  node governs — its own and the siblings it claims: a setting's isochronous
  endpoints are reserved with the controller before the device is told, and a
  stream's slots are scheduled onto the endpoint's ring at the frame each
  service interval is due in, every interval accounted moved, missed or
  failed (`plans/SOUND.md` SND6).
- `periodic` — endpoint descriptors, service intervals, isochronous budgets,
  explicit-feedback decoding and exact packet pacing.
- `alternate` — a configuration's alternate settings, interface numbers,
  control-only interfaces and interface associations.
- `regs` / `trb` / `ring` — the register, TRB, and ring-state vocabularies.
- `SlabBank` — the production `device::DmaBank`: a growable bank of owned
  DMA chunks minted from the host's `DmaHost` seam, aperture-checked per
  allocation and freed on release, so per-device memory is allocated on
  attach and returned on detach — never a fixed carve. A chunk the controller
  may still reach — a slot whose Disable Slot went unconfirmed, which also
  keeps its DCBAA entry — is withheld instead, and returned when that command's
  late confirmation arrives or after a confirmed controller reset; the engine
  resets its controller whenever it is dropped, keeping every chunk if the
  controller will not reset.

## Design

- `no_std` + `alloc`, `#![forbid(unsafe_op_in_unsafe_fn)]`, depends only on
  `lib/*` crates (`lib/abi`, `lib/dma-barrier`, `lib/inline`), so it
  builds for every Tier-1 target. The enumeration engine's
  tables and DMA chunks grow with the devices actually served, through the
  fallible allocation paths (exhaustion is a typed error, never a panic):
  the only concurrency bounds are the controller's reported slot count and
  genuine memory exhaustion, exactly as on other hosts — never a
  compile-time budget.
- Every access is mediated by the `RegisterBlock` / `device::DmaBank` seams, so
  the bring-up, enumeration, and ring state machines are proven host-side
  against a register-level mock plus an in-memory ring/DMA model (§2.2); the
  doorbell below them is the on-metal acceptance item.
- Nothing waits inside the engine: every procedure that spans a controller
  round trip is an operation `device::UsbHost` runs, waiting only on a routed
  completion or a one-shot timer, each bounded by a wall-clock budget (the
  per-command budget, the USB 2.0 §9.2.6 request ceiling, the power-on-good +
  attach-debounce window). The caller parks on the controller's interrupt
  line, bound before `UsbHost::start` — start enables the completion
  interrupter itself — until the earliest deadline its service step reports.
  Only the brief register handshakes (`Xhci` open/start/reset readiness) keep
  the bounded iteration poll budget.
- Fail-closed (§2.9): an implausible capability block, an out-of-range port or
  doorbell target, a malformed descriptor, or an exhausted wait budget is a
  typed `DriverError`, never a panic or an unbounded spin (§2.1).
- Fuzzed: `tests/fuzz_descriptors.rs` (registered with `cargo xtask fuzz`)
  holds the pure descriptor decoders — `DeviceDescriptor::decode`,
  `InterfaceInfo::decode_all`, `HubDescriptor::decode`, `StringHeader`,
  `first_langid`, `SerialNumber::decode`, `alternate_setting`,
  `is_control_only`, `interface_numbers`, `EndpointDescriptor::decode`,
  `PeriodicBudget::isochronous` and `FeedbackDecoder::decode` — to a naive
  model of what they may accept. The transfer path validates through those
  same decoders only.
- The crate holds **no** capability of its own — authority is the consuming
  driver's (`CAP_MMIO_MAP` for the register window, `CAP_MEM_DMA` for the DMA
  carve), checked in the wiring that mints them.

## Stability

Tier: `experimental`.
