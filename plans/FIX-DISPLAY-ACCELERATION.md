# FIX-DISPLAY-ACCELERATION — Hardware-assisted scan-out, end to end

| # | Item | Status |
|---|---|---|
| F2 | `plans/FINISH-x86_64.md` F2, delivered here as this plan's prerequisite: one shared framebuffer console and one console/seat layout for every port, D152 fixed | done |
| K | The kernel text console on a surface a display driver hosts: the console layout chosen by discovery, `console_adopt`/`console_relinquish`, damage signalled on a `ConsoleDamage` wait source | done |
| A | Driver-allocated scan-out memory: the display driver allocates every buffer its hardware reads and grants it to the presenter; the `Display` class trait presents buffers by id; the compositor presents through `tairix_display::Presenter` | done |
| B | The `DISPLAY_ENDPOINT` protocol for it: `Configure` answering with frames, `AllocBuffer`/`FreeBuffer`, `QueryPlanes`, `PresentLayers`, the cursor requests, and the `Presented` event; `RemoteDisplay` presenting layers; the session reading the device's planes once | done |
| C | `gpu_virtio`: kernel discovery of virtio-gpu, the 2D driver (zero-copy primary plane, host-side damage, power, EDID and hotplug, the hosted console, interrupt completion), and its QEMU verticals on aarch64, riscv64 and x86_64 | done |
| P | The hardware cursor plane: cursor caps and requests, `gpu_virtio`'s cursor queue, the HVS cursor plane, the WM moving a plain pointer without a composite | done |
| H | `bcm2711_hvs` as an `AcceleratedDisplay`: frames and layers scanned out of the buffers it allocated, one display-list plane per layer, the end-of-frame interrupt | done |
| D | The WM's retained hardware layers: per-window layer buffers re-baked by damage, flattened layers for frosted and translucent surfaces, a layered present consuming the damage it carried | done |
| E | Flips signalled by completion and paced off it, `Busy` coalesced rather than fatal, and hardware scaling of the scaled desktop | done |
| R | The output's refresh rate: `Display::refresh`, the `Query` reply's rate, `gpu_virtio` reading it from the EDID and the HVS from the timing its surface is shaped for | done |
| P.2 | The cursor plane moved by the frame's present while a gesture carries a window | done |

No item is optional and none defers work as "future". Items land together;
the whole-project gate runs once on the final tree.

Binding under `AGENTS.md`. Read first (§15.18): `plans/DISPLAY.md` (seats,
`DISPLAY_ENDPOINT`, D8's surface handover, D12's moving output),
`plans/FINISH-x86_64.md` (F2, delivered here), `plans/FIX-DESKTOP-SPEEDUP.md`
(the software path, which stays mandatory), `plans/COMPOSITOR-WORK.md` and
`plans/GUI-CONTROLS-DESIGN.md` (what the WM composites), `plans/IOMMU.md`
(IOM4/IOM19: device-reachable shared memory), `plans/PI.md` and
`plans/SOUND.md` §Native HDMI on the Pi 4 (`bcm2711_hvs`), `plans/GPU.md`
(layered on this plan), `docs/src/drivers/display.md`.

---

## 0. Binding decisions

Settled by the User on 2026-10-19. A change that contradicts one stops and
asks (§15.7).

1. **The display driver allocates every buffer its hardware reads.** It is the
   one component that knows its device's reach, contiguity, coherence and
   translation domain. A DMA-capable engine carves with `shm_create_dma` under
   its own node's `Dma` grant (`CAP_MEM_DMA`, the device's `DmaReach`, the
   IOMMU domain where a unit exists — which is how the `VideoCore` 1 GiB alias
   is honoured); a CPU-blit framebuffer driver creates plain shared memory.
   The server grants each buffer to the presenting client with
   `shm_grant_peer`, and the client maps it as a `RegionGrant`. No new
   capability, no new shared-memory class. Apps keep their ordinary,
   app-owned window frames: the session's decode of a frame is the one copy,
   and the session's per-window copy stays the stable snapshot the compositor
   reads. The one exception is a picture the session itself makes opaque and
   that covers the output, a screensaver's: its one copy is laid straight into
   a frame this driver allocated (`Presenter::present_drawn`), never the
   other way about.
2. **`gpu_virtio` is an honest 2D driver and reports no layer compositor.**
   virtio-gpu's scanouts are separate monitors, not overlay planes: a device
   has one primary plane per scanout and one cursor. Its zero-copy path is the
   primary plane — each frame the driver allocated is one resource's backing,
   so presenting copies no pixel in the guest. Blob resources
   (`RESOURCE_CREATE_BLOB`, `SET_SCANOUT_BLOB`) are **not** part of this plan:
   they buy only a host-side copy once attach-backing already removes the guest
   one, and QEMU cannot realise `blob=on` without a host `/dev/udmabuf` the
   runner cannot open, so no vertical could exercise them.
3. **The display seam carries a hardware cursor plane.** Moving a plain
   pointer is a plane move, not a composite.
4. **The kernel text console appears on a display a driver hosts it on.** A
   machine whose only display is virtio-gpu keeps a visible text login, and a
   dead desktop returns to text on screen. This needs one shared framebuffer
   console and one console/seat layout on every port, so `plans/FINISH-x86_64.md`
   F2 is delivered here (F2 item).

## 1. What it guarantees

- A display driver's device reads only buffers that driver allocated, and a
  present names one by id: no frame byte crosses the IPC, and no engine copies
  a frame it can scan where it lies.
- The layered path crosses the process boundary: the session presents through
  `Presenter`/`LayerPresenter`/`CursorPlane`, which `RemoteDisplay` implements
  over `DISPLAY_ENDPOINT`, and the WM keeps retained layers that re-bake only
  their damage, so an undamaged scene presents nothing.
- Every QEMU target has a native display engine (`gpu_virtio`), every port a
  framebuffer console a display hosts, and a present is paced by its latch,
  never refused fatally.

## 2. Invariants (bind every item)

- **Compositing policy stays in user space.** The kernel issues framebuffer,
  MMIO, DMA and interrupt grants, and hosts its own text console; every engine
  is a user-space driver on `lib/drvrt`.
- **Generic code is board-neutral** (§2.20). `gpu_virtio`, `lib/display`,
  `lib/virtio`, `lib/fbcon`, `lib/abi` and the WM name no board.
  `bcm2711_hvs` knows its silicon because it is that device's leaf driver.
- **One raster/blend path** (§2.2). The software composite stays `lib/raster`;
  a baked or flattened layer is that same composite written into a layer
  buffer.
- **The engine is never handed a blend that would band**
  (`plans/FIX-DESKTOP-SPEEDUP.md` B.5). A window-wide translucency or a
  backdrop blur is flattened: composited in software, with the backdrop
  beneath it and the ordered dither, into an opaque layer. Only a layer's own
  antialiased edge is blended by the engine.
- **Fail closed, lease first, no ambient authority** (§5.4). Every request
  that acts for a seat checks the caller's live lease before any state;
  buffers belong to the configuration that allocated them and die with it; a
  layer stack the engine cannot take falls back to the software frame, never a
  partial hardware frame.
- **No busy-poll** (§2.23). Completion, latching, hotplug and console damage
  arrive as interrupts or wait-set wakes.
- **Evolve in place** (§2.13). `Display`, `AccelLayer`, `AccelCaps` and the
  wire protocol change directly with every caller, the generated C header and
  `abi-check`.
- **Capacities scale, bounds stay fixed** (§24). Buffer counts and bytes per
  configuration are budgets derived from the device; `MAX_ACCEL_LAYERS` and
  `MAX_DAMAGE_RECTS` are wire-format bounds.
- **The software path stays the always-available fallback** on every target
  (§17.3), and headless stays first-class.

---

## F2 — one framebuffer console, one console/seat layout

Delivered exactly as `plans/FINISH-x86_64.md` F2 specifies; that section is
binding. The arch-neutral half of `kernel/arch/aarch64/src/video.rs` — the
surface slot and its placement, the render lock, attach, both write paths,
the grid and extent queries, `set_surface`/`reclaim_surface`/`purge`,
adoption and `paint` — moves into `lib/fbcon`. A port supplies only its
surface source, the extent reachability check, the direct-map lookup, the
post-write cache maintenance and its interrupt-mask type. The console/seat
layout moves into one `kernel/tairix-kernel` module every port calls.

- **D152** closes here: a fatal report never waits on the render lock. Once
  every other CPU has stopped it is broken, so a report
  raised with the lock held still renders; while a peer that would not stop
  may be painting, a write renders only where the lock is free. Regression
  tests: a panic with the lock held reaches the sink; the lock is broken only
  when every peer stopped.
- `TextConsole` owns its grids generically (`G: AsMut<[Cell]>`), and gains
  `resized`, which keeps the bottom-most rows, the cursor and the parser state:
  a console attached to a surface of another size, or following its output to
  a new mode, keeps what it showed.
- aarch64 is rewired with no behaviour change; its framebuffer, greeter and
  desktop verticals are the evidence. x86_64 and riscv64 gain the layout: a
  serial console with a seat when no display can host the console, the
  framebuffer console otherwise (K).

## K — the console on a hosted surface

- **The layout is chosen by discovery at boot.** A boot framebuffer, or a
  discovered display-class node that hosts the console when no boot
  framebuffer exists, makes the framebuffer console console 0; otherwise the
  serial console is. The hosting node carries `HwProperty::ConsoleHost`, set
  by the kernel at discovery. With both a boot framebuffer and a hosting
  engine the engine takes the screen over, as on the Pi: no boot-display node
  is published and the console moves to the engine.
- **Before a surface exists the console interprets output into its grid and
  paints nothing**, so what the boot wrote is on screen once a driver hosts it.
- **`console_adopt`** (`CAP_MEM_DMA`, the caller loaded for the
  `ConsoleHost` node): the kernel allocates a zeroed, physically contiguous
  kernel-owned block for the requested mode below the node's reach, maps it
  into the node's translation domain where one exists, attaches the console to
  it at that geometry (`resized`), and answers the block's device address and
  stride. An adoption at another mode is refused `Busy` until the block is
  relinquished, so the console's memory is never moved under a device still
  reading it. The console keeps the block for the kernel's life; a driver's death detaches the
  console's device mapping, never the console.
- **Damage**: an adopted console accumulates the rows it painted. The
  `WaitSourceKind::ConsoleDamage` member is admitted for the console-host
  node's driver, before it hosts as well as after, so a display that finds
  its sink later is still woken; it is ready while rows are owed to the
  instance the console is hosted for, and `console_damage` takes them. A
  driver that scans memory directly (the HVS) never asks.
- **Relinquish**: `console_relinquish` gives the block back on the hosting
  driver's word that its device stopped reading it; the console keeps its
  text and paints nothing until adopted again, which is how a console follows
  its output to a new mode.
- The console's disposition still follows the seat (D8). A driver shows its
  console when the server reports `ReleaseSurface::Text`, and its client's
  frames otherwise. A panic report reaches an adopted console's memory; a
  virtio-gpu screen shows it only if its driver still runs (documented).

## A — driver-allocated scan-out memory

- **The `Display` class trait presents buffers the driver allocated.**
  `alloc_buffer(width, height)` answers an `AllocatedBuffer` (driver id,
  shared-region id, length, stride); `free_buffer(id)`; `frame_count()` is how
  many output frames a presenter rotates through (1 where a present copies or
  transfers synchronously, 2 where the engine reads the frame while it shows);
  `present(id, damage)` answers `Presented::Shown` or `Presented::Queued`, and
  `service_events` reports a queued present latching. The hosting loop calls
  `service_events` on every event wake, and after every request of a driver
  whose requests wait on its device (`requests_take_wakes`), since such a
  request may have taken a source's wake. `memory()` states the device's own memory; `as_accelerated()` and
  `hardware_cursor()` reach the layer compositor and the cursor plane, so the
  server holds one `&mut dyn Display`, and `DisplayDeviceReport` gathers the
  three for the monitor.
- **`AccelLayer` names a buffer.** `buffer: BufferId`, `src: DamageRect` (the
  part of the buffer shown), `dst_x`, `dst_y`, `dst_width`, `dst_height`,
  `opacity`. `AccelCaps` keeps `max_layers` (≤ `MAX_ACCEL_LAYERS`),
  `max_width_px`, `max_height_px`, `per_layer_opacity`, and gains `hw_scaling`
  (E). No per-layer damage field: both engines scan driver-allocated buffers
  directly, so no driver has an upload to scope.
- **Budgets.** A driver bounds what one configuration may allocate by a budget
  derived from its device; a request past it fails closed (`NoSpace`) and the
  WM keeps that window in the software composite. A display with no layers —
  the linear framebuffer, `gpu_virtio`, whose DMA grant names no window —
  admits the frames a presenter could rotate through (`frames_budget`); the
  HVS admits a share of its DMA window.
- **One memory seam.** `tairix_display::BufferMemory`/`SharedBuffer` for every
  engine, `DeviceBuffer` for one whose device reads a buffer where it lies,
  and `RtDeviceMemory`, the one `shm_create_dma`-backed allocator the HVS and
  `gpu_virtio` share.
- **The compositor presents through `tairix_display::Presenter`**: the mode,
  `present(frame, damage)`, `set_power`, `layers()` → `LayerPresenter`
  (caps, buffer allocation and the mapped pixels, `present_layers`), and
  `cursor()` → `CursorPlane` (P). `RemoteDisplay` implements all three;
  `Compositor` names no driver trait.
- **Every driver is rewired.** `drivers/display/framebuffer` allocates one
  plain shared frame and blits it; `bcm2711_hvs` scans its frames out of the
  carves it allocated (H); `gpu_virtio` attaches them as backing (C). The
  framebuffer and vesa QEMU verticals drive the engines through buffers.

## B — the protocol

Every request but `QueryStats` names a seat and is refused before any state is
read unless the caller holds that seat's live lease.

| Request | Answer |
|---|---|
| `Query` | the output's mode and the rate it scans at, or none |
| `Configure { mode, events port }` | the frame count, the stride, one `RegionGrant` per frame, the grantor; the port must be the caller's (`call_peer_port`) and admit this endpoint |
| `QueryPlanes` | `AccelCaps` or none, `CursorCaps` or none |
| `AllocBuffer { width, height }` / `FreeBuffer { id }` | the buffer's id, stride and grant / status |
| `Present { buffer, damage }` | status, and whether it is shown or queued |
| `PresentLayers { count, layers }` | as `Present`; `count ≤ max_layers ≤ MAX_ACCEL_LAYERS` |
| `SetCursor { buffer, hot spot }` / `HideCursor` / `MoveCursor { x, y }` | status; the image is the buffer's size |
| `SetPower`, `QueryStats` | as before; stats carry the device report: its memory, layer caps and cursor plane |

- **One present in flight.** A queued present's latch is posted to the
  configuration's port as `DisplayEvent::Presented { seq }`; a present before
  it is refused `Busy` by the client without a call, and by the server if one
  arrives. A driver whose pipe stops completes a queued present at once, and
  completes one the hardware never latched after a bounded deadline, recorded.
- **Buffers belong to the configuration**, are checked on every reference, and
  are freed with it; a stale id is `NotFound`.
- **The session reads the planes once.** At bring-up it reads `QueryPlanes`;
  `Compositor::present` then takes the layered path frame by frame wherever
  the presenter reports a layer engine, and the pointer goes to the cursor
  plane wherever the presenter has one and the pointer is one sprite that
  fits it.
- The decoders get structured fuzz seeds in `lib/abi/tests/fuzz_decode.rs`
  (the `DSP1` magic otherwise starves the random input).

## C — `gpu_virtio`

- **Discovery.** The kernel's virtio MMIO and PCI walks publish device type 16
  (`tairix_abi::driver::virtio::VIRTIO_GPU_DEVICE_ID`) as a `Display` node
  with its windows, DMA grant and interrupt; the first is the console's host
  (`boot_display::virtio_gpu`) on every port, beside the aarch64 port's HVS,
  and x86_64 and riscv64 attach the framebuffer console when one is hosted.
  `BIND_KEYS` match `HwMatchKey::virtio(16)` on both transports.
- **The driver** follows `drivers/audio/virtio_snd`: one crate, its engine,
  `register`, `BIND_KEYS`, `REQUIRED_CAPABILITIES` and a `Run` binary. It
  negotiates `VERSION_1`, `ACCESS_PLATFORM` and `EDID`. The output is the first
  enabled scanout at the size its display information states, else the
  preferred timing of its EDID; a `VIRTIO_GPU_EVENT_DISPLAY` config change
  re-reads it in `service_events`.
- **Zero-copy primary plane.** One frame (`frame_count() == 1`): each buffer is
  the backing of a 2D resource made at its first present, which transfers it
  whole. A present is one batch on the control queue — a transfer per damage
  rectangle, the scan-out switch, a flush per rectangle — published with one
  notify and answered on the used-ring interrupt (`RequestQueue` carries one
  batch at a time), so the frame is free when the call returns
  (`Presented::Shown`). The batch relies on the device processing its control
  queue in order, which QEMU's device does and virtio 1.2 does not require
  (§2.6, §5.7.6). A frame given back while shown keeps its resource until
  replaced; a
  backing whose detach the device never answered is never unmapped, and a
  resource an unanswered batch may have made (a frame's, the console's, the
  pointer's) is unreferenced before its memory is reused or given back.
- **Power**: `SET_SCANOUT` naming resource 0 switches the output off; switching
  on scans out what is shown again, a present taken while off included.
- **The hosted console**: shown on `ReleaseSurface::Text`, its rows
  transferred on its damage wake, re-hosted at a new output mode
  (`console_relinquish`, then `console_adopt`); one the device refused goes
  back to the kernel and is hosted again when the output moves.
- **Verticals**: on aarch64 `virt`, riscv64 `virt` and x86_64 `q35`, each with a
  virtio GPU and no other display, `devmgr` autoloads the driver, the console
  is dumped from the device's screen once the driver records it shown (the
  passphrase is held until then), and the guest passes once the process the
  kernel names as serving the display endpoint reports the first frame its
  display took, which a refused present never produces. aarch64 also runs the desktop over it, whose dumps
  hold the composited frame and then the damaged region a window opened. The
  runner's display knob (`tairix_qemu::Display`) attaches the GPU at the
  ramfb console's mode, removes the x86_64 VGA on `q35`, and names the device
  in a screendump.

## P — the cursor plane

- `CursorCaps { max_width_px, max_height_px }` (`gpu_virtio`: 64 × 64, the
  device's cursor size; HVS: its cursor plane's bound). The image is
  premultiplied in the output's format, drawn by the client into a buffer it
  allocated.
- `gpu_virtio`: the cursor buffer is a 2D resource's backing; `SetCursor`
  transfers it and issues `UPDATE_CURSOR` on the cursor queue, `MoveCursor`
  issues `MOVE_CURSOR`.
- HVS: a top display-list plane; `MoveCursor` rewrites its position words in
  the live list, so moving the pointer presents nothing.
- The WM gives the plane the pointer whenever the pointer is one sprite that
  fits it; a trail or the halo composites in software. A pointer that moves
  while on the plane damages nothing. The session moves the cursor once per
  drained input batch.

### P.2 — the cursor moves with the frame during a gesture

While a gesture carries a window the plane is moved by the frame's present,
after the frame is handed over, so the pointer and what it drags reach the
scan-out together; `plans/FIX-DESKTOP-SPEEDUP.md` E.6 states the rule and its
edge cases. `CursorPlane` gains nothing: the change is when the compositor
calls `move_to`.

## R — the output's refresh rate

- `Display::refresh` answers the rate the output scans at as a
  `RefreshRate` (millihertz, `MIN_MHZ..=MAX_MHZ`), or none; it moves across
  `service_events` with `output`, and the server's `DisplayOutput` notice
  announces a change of either.
- **Not part of `DisplayMode`.** The mode is a frame layout, compared whole to
  decide whether every buffer must be rebuilt; a rate that changes alone
  rebuilds nothing.
- `Query` answers the rate beside the mode; a decoded rate outside the bound is
  refused, not clamped.
- `gpu_virtio` takes the rate from the EDID timing whose active size is the
  scan-out's, preferring the preferred timing, and re-reads it on every
  display-change event. The HVS answers the timing of the first connected
  output, the one its surface is shaped for. The framebuffer and VESA drivers
  answer none.
- Consumers: `plans/FIX-DESKTOP-SPEEDUP.md` E.5.

## H — `bcm2711_hvs` as an `AcceleratedDisplay`

- Frames and layer buffers are `shm_create_dma` carves below the HVS's reach,
  scanned out where they lie: a present writes no pixel.
- `present_layers` writes a display list of one plane per layer — position,
  source crop, scaled size where `src ≠ dst`, per-plane alpha (premultiplied,
  with the constant `opacity` where `per_layer_opacity`) — into a free list
  slot and latches it. The background fill is the screen beneath the first
  layer. `accel_caps` states what one stack slot holds; a stack whose scaled
  layers outrun an output's share of the line buffer is refused `OutOfRange`
  with every output's list built before any is written, so a refusal moves
  nothing on screen.
- **Latching is interrupt-driven.** The driver binds the HVS node's interrupt
  (`CAP_IRQ_BIND`) and enables the end-of-frame source; a wake that finds the
  queued list active completes the present.
- The device report states the engine's caps; its memory is none (it scans
  system RAM).

## D — retained hardware layers in the WM

- **A window on a hardware layer owns a buffer ring** (two buffers, allocated
  through `LayerPresenter`) holding its whole footprint — content, furniture,
  corner coverage, shadow — baked once and re-baked only where damaged; each
  buffer catches up on what changed while the other was shown. Moving a window
  is a new destination, not a composite.
- **The stack**: a base layer (the output frame, holding the desktop and every
  window below the first hardware-layer window, composited in software), then
  one layer per window above it, then the pointer if it is not on the cursor
  plane. The split moves up when the stack exceeds `max_layers`.
- **Flattened layers**: a frosted or translucent window's layer is the
  software composite of its footprint over everything beneath it, opaque, so
  the engine blends nothing that could band. It is re-baked where anything
  beneath it changes.
- **A layered present consumes the damage it carried** into a software-stale
  region outside `has_damage`, which the software path drains when it next
  composites; an undamaged scene presents nothing.
- A window that cannot be given buffers, or the lowest window of a stack the
  engine cannot composite at once, joins the base with everything beneath it
  and the frame is planned again in the same present, the layers above kept;
  the refusal stands until that window's size changes. A stack refused
  outright falls back to the software frame through the damage-scoped
  `present`.

## E — flips and hardware scaling

- `FramePacer` holds a frame while a present is queued and admits it on the
  `Presented` event, replacing the fixed period with the flip where the
  presenter reports one (`plans/FIX-DESKTOP-SPEEDUP.md` E.3 keeps its other
  invariants). `Busy` is coalesced: damage stays, the next admitted frame
  carries it; it never ends the session.
- **Hardware scaling**: with `AccelCaps::hw_scaling`, the scaled desktop
  (`set_desktop_size`, `plans/DISPLAY.md` D11) is composited into a
  desktop-sized layer buffer and presented as one scaled layer — the HVS
  scaler replaces the software resample. Without it, the software resample
  stays.

---

## Per-target delivery

| Target | Accelerated path |
|---|---|
| aarch64 / riscv64 / x86_64 (QEMU) | `gpu_virtio`: zero-copy primary plane, host-side damage, power, EDID and hotplug, the cursor plane, the hosted console, interrupt completion. One driver, no per-arch code. |
| Raspberry Pi 4 | `bcm2711_hvs`: zero-copy frames and layers, one plane per layer, the cursor plane, end-of-frame completion, the scaler. |
| wasm32, framebuffer, VESA | The software path; one synchronously blitted frame. |

## Refused by name

- Blob resources and multi-scanout "layers" on virtio-gpu (decision 2).
- A virgl/3D path: `plans/GPU.md` owns rendering, layered on this plan.
- A per-layer damage field with no engine to consume it.
- A second blend path, a GPU-only path with no software equivalent, or a
  partial hardware frame.

## Definition of done

Every item's code, tests and docs land together; `gpu_virtio` is a real
driver; the software path is the fallback everywhere and the lease gates every
path. §23 self-review applied. Whole-project gate green once on the final
tree: `cargo fmt --all`, `cargo xtask ci`, `cargo xtask fuzz --secs 5`,
`tools/ci/soak.sh both --secs 20`. New `unsafe` (the shared console slot, the
driver DMA paths) is enrolled in miri. The queued-present handoff and the
console damage readiness add no ordering pairing of their own: the damage is
written and read under the console's render lock (`lib/sync`'s locks, loom
modelled), its wake is the wait queue's deferred request drained into the
park/unpark handshake (`kernel/sched/api`, loom modelled), and a queued present
is held and released by single-threaded loops on either side of the IPC.
