# Display drivers

Display drivers scan out what the compositor (`userland/gui/wm`) draws.
They implement
[`tairix_abi::driver::display::Display`](../abi/driver_traits.md) and are
loaded as user-space drivers; compositing policy and damage tracking live
above this trait, not inside it.

## Class trait

A display driver allocates every buffer its hardware reads, and a present
names one by id:

| Method            | Purpose |
|-------------------|---------|
| `output`          | the `DisplayMode { width_px, height_px, stride_bytes, format }` the output runs, or `None` with no sink |
| `frame_count`     | how many output frames a presenter rotates through: 1 where a present copies or transfers synchronously, 2 where the engine reads the frame while it shows |
| `alloc_buffer`    | a buffer of a size, in memory the device reaches: its `BufferId`, the shared region holding it and its stride |
| `free_buffer`     | give a buffer back; one the device may still read is kept until it no longer can |
| `present`         | show an output-sized buffer, of which only a list of `DamageRect`s changed, answering `Presented::Shown` or `Presented::Queued` |
| `service_events`  | collect what woke the display's event sources: the output moving, and a queued present latching (`Serviced { latched }`) |
| `memory`          | the device's own memory, for the monitor |
| `released`        | the seat moved on: show the text console, or black for a handover |
| `set_power`       | switch the display `On` or `Off` (`DisplayPower`) |
| `as_accelerated`  | the hardware layer compositor, where the device has one |
| `hardware_cursor` | the cursor plane, where the device has one |

Pixel encodings are `DisplayFormat::Rgba8888` and `DisplayFormat::Bgra8888`
(4 bytes per pixel), premultiplied. The trait never panics: every refusal is
a typed `DriverError`.

### Buffers belong to the driver

The driver is the one component that knows its device's reach, contiguity,
coherence and translation domain, so it carves every buffer itself: a
DMA-capable engine with `shm_create_dma` under its own node's `Dma` grant (in
the device's IOMMU domain where it has one), a CPU-blit framebuffer with plain
shared memory. The display service grants each to the presenting client with
`shm_grant_peer`, and the client maps it. A buffer belongs to the
configuration that allocated it and dies with it; a stale id is `NotFound`.
Each driver bounds what one configuration may hold by a budget derived from
the memory its device reaches, and a request past it fails closed (`NoSpace`).

A buffer is never given back while the device may still read it: unmapping a
DMA region tells the kernel the device has stopped mastering it, so a driver
whose device scans memory in place keeps a freed buffer until no list it
latched names it.

### One present in flight

`present` validates the whole damage list (`DamageRect::validate_list` —
non-empty, within `MAX_DAMAGE_RECTS`, each rectangle non-empty and on the
surface) before touching anything. A driver that copies or transfers the
frame answers `Shown`: every buffer it replaced is free on return. One whose
device reads the frame where it lies answers `Queued`: the buffer it replaces
is read until the device latches the new one at its next frame start, which
`service_events` reports. The display service posts the latch to the
configuration's event port as `DisplayEvent::Presented { seq }`; a present
before it is refused `Busy`, by the client without a call and by the service
if one arrives. A driver whose pipe stops completes a queued present at once,
and one the hardware never latched completes after a bounded deadline,
recorded.

### The present right follows the live seat lease

Every request that acts for a seat is refused before any state is read
unless the caller holds that seat's live lease (`plans/DISPLAY.md` D4). The
framebuffer engine also captures its host's `DriverHost::seat_gate()` — a
`SeatGate` the kernel bound to the client's `SeatLease` — and consults it at
the top of `present`, so a client whose lease was revoked is refused with the
distinct `DriverError::SeatRevoked` before the surface is touched. A host
with no seat wired (headless, bring-up, unit seams) exposes no gate. See
[the seat model](../desktop/seat.md) for the ownership side.

### Switching the display off

`set_power` is how energy saving reaches the panel: the desktop's
screensaver display-off wait runs out, or the login screen sits untouched for
thirty minutes, and the presenter sends `SetPower` over `DISPLAY_ENDPOINT`; the
first input afterwards sends it back on. Both presenters sleep their display
through the one `tairix_display::DisplaySleep` state machine, so which
refusal means what and which display is switched back on have one
definition. The trait's default refuses `Unsupported`, which is the truth for
a surface with no power control of its own — a firmware linear framebuffer, a
VBE mode — and the presenter then keeps its screen black and still instead.
A driver that can reach the panel overrides it: the Pi 4's display engine
brings its outputs down, and virtio-gpu scans out nothing.

A power-down never outlives the configuration that asked for it.
`DisplayServer` switches a dark display back on when it releases a
configuration — the lease that made it ended, or a newer owner configured —
and a service that has just started knows nothing of the display's state, so
its first configuration switches it on too. The service learns that a lease
ended from the kernel's `DisplayLease` system notice
(`docs/src/abi/notice.md`), so a session that crashes behind a dark screen
leaves the machine lit for whoever comes next.

### Hardware layers

A driver whose hardware composites a stack of planes also implements
`AcceleratedDisplay`:

| Method           | Purpose |
|------------------|---------|
| `accel_caps`     | `AccelCaps { max_layers, max_width_px, max_height_px, per_layer_opacity, hw_scaling }` |
| `present_layers` | show a bottom-first `AccelLayer` stack, as `present` answers |

An `AccelLayer` names a buffer the driver allocated: the part of it shown
(`src`), where it lands (`dst_x`, `dst_y`, which may be negative) and at what
size (`src`'s unless `hw_scaling`), and a constant `opacity` (`255` unless
`per_layer_opacity`). `AccelLayer::validate` checks it against the caps and
its buffer's extent. A stack the engine cannot take is refused whole; the
compositor then presents the frame in software instead, never a partial
hardware frame.

### The cursor plane

A driver whose hardware draws the pointer itself implements
`HardwareCursor`: `cursor_caps` bounds the image, `set_cursor` shows a buffer
as the pointer with its hot spot, `move_cursor` puts the hot spot somewhere,
and `hide_cursor` takes it off. The device has taken the image when
`set_cursor` returns, so the presenter may draw the next into the same
buffer at once. Moving a pointer on the plane presents no frame.

### The presenter side

A compositor names none of these traits. It presents through
`tairix_display::Presenter` — the mode, whether a present may go now
(`ready`), `present(frame, damage)`, `set_power` — and, where the device has
them, `LayerPresenter` (the caps, layer buffers it bakes into, and
`present_layers` over the output frame as the bottom layer) and
`CursorPlane`. `RemoteDisplay` implements all three over `DISPLAY_ENDPOINT`:
it maps the frames the driver granted at `Configure`, copies each present's
damage into the frame the device does not read, together with whatever that
frame missed while others were shown, and names it by id. How the desktop
uses the layers and the plane is [the window manager's](../desktop/wm.md).

## Shipped drivers

| Driver       | Crate                                | Surface source                            | Stage 4 status        |
|--------------|--------------------------------------|-------------------------------------------|------------------------|
| framebuffer  | `tairix-drv-display-framebuffer` (Run process over `tairix_display::Framebuffer`) | firmware linear framebuffer (GOP / Pi mailbox / `ramfb`) | host-side tests + riscv64 & aarch64 `ramfb` QEMU verticals + wasm32 browser-canvas vertical |
| vesa         | `tairix-drv-display-vesa`            | x86_64 VBE linear framebuffer (`ModeInfoBlock`) | host-side tests + x86_64 `ramfb` QEMU vertical |
| gpu_virtio   | `tairix-drv-display-gpu-virtio`      | a virtio GPU's 2D device on virtio-mmio or virtio-PCI: frames are resource backings, the pointer its cursor, the console hosted on it | host-side tests against a model device + aarch64, riscv64 and x86_64 QEMU verticals |
| bcm2711_hvs  | `tairix-drv-display-bcm2711-hvs`     | the Pi 4's HVS and HDMI pixel valves, driven natively, its encoders over `encoder-v1` | host-side tests against a register-level model |
| bcm2711_hdmi | `tairix-drv-display-bcm2711-hdmi`    | not a surface: each Pi 4 HDMI block, an `encoder-v1` encoder of the display engine and an `audiochan-v1` audio device | host-side tests against a register-level model |

The two linear-surface drivers are deliberate siblings, not duplicates:
`vesa` owns the VBE-specific decode, while the framebuffer path consumes an
already-parsed geometry record.

### `tairix-drv-display-framebuffer`

The framebuffer display service copies a presented frame into a
firmware-provided linear surface. Its crate is the `Run` entry point of
the `/System/Drivers/` bundle `devmgr` autoloads when a display node
carrying a `HwResourceKind::Framebuffer` resource is discovered, plus the
lib target holding its canonical bind table, and it holds no device logic
of its own: the linear-surface engine is `tairix_display::Framebuffer`,
the protocol engine is `tairix_display::DisplayServer`, and the loop that
hosts them is `tairix_display::service` (`lib/display`, one shared
definition, `plans/DISPLAY.md` D7b). None of them programs a display
controller; the service resolves its surface's `(phys_base, mode)`
fail-closed from its kernel-issued device-resource grants
(`sole_framebuffer`) and never scans out a guessed geometry.

`Framebuffer::open` validates the geometry and maps exactly
`stride_bytes * height_px` bytes through the host's `MmioMapper`,
which enforces `CAP_MMIO_MAP`. The framebuffer is therefore reached
only through a kernel-installed mapping, never through a pointer the
service synthesises. Its buffers are plain shared memory, none larger than
the surface, and one configuration's buffers hold no more than
`DISPLAY_MAX_FRAMES` surfaces together. A present copies only the validated
damage out of the buffer it names into the mapped window — one span per
rectangle row, or one span for whole rows of matching stride — bounds-checked
at every write, and answers `Shown`.

The hardware-tree framebuffer resource also carries its discovered CPU
memory policy. QEMU `ramfb` scans ordinary coherent guest RAM and therefore
stays write-back cacheable. A true CPU-written display aperture requests the
separate write-combining page attribute; a port that cannot encode WC refuses
that mapping rather than silently substituting Device or WB memory. On
aarch64, WC is Normal Non-Cacheable and remains distinct from bidirectional
coherent DMA in the software PTE metadata. The HVS software fallback and its
plane uploads use one bounds-checked bulk transfer rather than millions of
individually checked volatile word stores.

No current backend advertises Linux-style y-wrap. Linux enables y-wrap only
when the driver explicitly reports hardware wrap and the visible and virtual
heights align to text rows. QEMU `ramfb` has no wrap primitive, and the Pi
firmware mailbox exposes virtual panning but does not prove wrap-at-end
semantics; treating either as wrap could scan beyond the allocation. The
retained console grid therefore keeps its correct coalesced damage repaint
until a discovered backend can guarantee bounded pan or true wrap.

The service binds the reserved `DISPLAY_ENDPOINT` (its manifest's
`CAP_IPC_BIND_PRIVILEGED` — a squatter cannot intercept presents) and
serves the `lib/display` protocol from a wait-set park that also holds the
kernel's display-lease notice: every request that acts *for a seat* —
`Query` included — is gated on the in-flight caller's live seat lease
through `call_peer_seat`. `Configure` allocates the configuration's output
frames and `AllocBuffer` one more buffer, each granted to the calling process
alone (`shm_grant_peer`); a `Present` names a buffer by id, so no frame byte
ever crosses the IPC.

`QueryStats` is the one operation that acts for no seat: it describes the
*device* this service drives, for the monitor that draws graphics
utilisation (`GPU_DEVICE_STATS`, served by `sysinfod` over this endpoint).
A lease is the wrong question about a device read — a monitor holds none
and never will — so it is gated instead on the caller's kernel-attested
`CAP_SYSINFO_HW` through `call_peer_origin`, the authority the hardware
inventory it details is already read under. It answers the cumulative
`busy_ns` / `idle_ns` the engine measured *around the driver's own present
call* — which is also where the window they partition opens, so a refused
present adds nothing to either and no request an unauthorised caller can send
moves the epoch —
what the device is (`DisplayDeviceReport`: its own memory, from
`Display::memory`, and the caps of its layer compositor and cursor plane),
and the mode being scanned out, or none while there is no output. A firmware
framebuffer honestly reports no memory of its own, no compositor and no
plane.

#### The display follows its output

The output is the driver's: `Display::output` is the mode it runs for the
sink it has, or `None` with none (`plans/DISPLAY.md` D12). A driver whose
output can move hands `serve` its event sources (`DeviceWake`: an interrupt
line, a port, a posted call's reply); the loop parks on them beside the
endpoint, calls `Display::service_events` when one fires, and announces a
move on the counted `NoticeTopic::DisplayOutput` before it serves another
request — and once when it starts, so a client of an earlier instance
converges onto this one. `Query` answers the output: a mode, or none. A
`Configure` whose geometry is not the output's mode, and a `Present` of frames
shaped for another, are refused `Stale`; the configuration is not dropped, so
the frames serve again the moment the output runs their mode, and it still
ends only with its lease. A linear framebuffer's output never moves, so it
names no source.

Clients configure one way, `tairix_display::establish_display` (feature
`rt`): the event port bound and admitted to the endpoint, the configure, and
the `RtDisplay` that maps the granted frames and owns the port.

Lifecycle: the signed-bundle load gate clears `CAP_DRV_LOAD`;
`Framebuffer::open` maps the surface; dropping the `Framebuffer`
releases the window and its buffers (unload); calling `open` again reloads. The engine
and its unit tests live in `lib/display`
(`lib/display/tests/framebuffer.rs`); the QEMU verticals below drive
that same shared engine.

#### The client side of a present

`lib/display` also owns the three pixel-exact decisions every program
that presents has to get right, in `tairix_display::scanout`: how many
bytes a frame is for a given mode (`scanout_len`, refusing a zero
extent, a stride too short for one scanline, or a size that overflows),
which byte order the mode's format wants (`ChannelOrder::for_format`,
refusing a format it has no software encoding for rather than guessing
and rendering the screen in false colour), and whether a damage
rectangle is a sub-region or the whole frame (`sub_screen_damage`,
falling back to a full present rather than presenting a wrong region).

They live here because getting any of them wrong is visible on the
whole screen, and two programs need them: the compositing window
manager, and the graphical login screen — which may not depend on the
window manager. What is deliberately *not* shared is the composition
itself: a compositor blending many windows through a back buffer and a
login screen blitting one surface are different loops, and both encode
their result through the one `ChannelOrder::encode`.

A composite loop encodes a whole row span at once through
`ChannelOrder::encode_run` — that same per-pixel encoder walked four
output bytes at a time, not a second spelling of the channel order. It
writes `min(pixels.len(), out.len() / 4)` pixels and returns that count,
so a short frame slice truncates rather than panicking or overrunning, a
longer one keeps its tail, and a trailing group of fewer than four bytes
is never half-written; the window manager passes one row span of its
back buffer with the frame bytes for that same span and checks the count
against the pixels it offered. There is no bulk-copy shortcut for the
order that already matches the pixel in memory: `tairix_raster::Pixel`
carries no layout guarantee to copy through, and a matching order is
already a four-byte move per pixel with no shuffle left to remove.

#### QEMU integration vertical

`tests/integration/framebuffer_display_qemu_riscv64`
(`tairix-test-framebuffer-display-qemu-riscv64`, enrolled in `cargo
xtask test --qemu`) drives the driver against a **real** emulated
framebuffer on the riscv64 `virt` board, closing the `load → use →
unload → reload` loop for a display driver.

The boot hand-off is synthesised the way QEMU exposes one: the test
harness attaches a QEMU `ramfb` device (`-device ramfb`) and programs
a static, page-aligned guest-RAM scan-out surface into it over the
`fw_cfg` MMIO DMA interface (find `etc/ramfb` in the file directory,
DMA-write the big-endian `RAMFBCfg`). The resulting geometry is the
`FramebufferConfig` boot hand-off. The harness then loads the signed
framebuffer `.rxe` through `tairix_drvhost::Host` (the §8 load gate)
and, for the "use" step, maps the surface through the
capability-gated `tairix_kernel_virtio::KernelMmioMapper` — the same
real kernel MMIO-map facility the bus drivers use — and calls
`present`. A second window mapped over the same physical range reads
the pixels back to confirm they reached the scan-out memory QEMU
consumes, before and after the reload. The `ramfb`/`fw_cfg` bring-up
is test-harness code, mirroring how the virtio verticals own their
PLIC/trap bring-up rather than placing it in the production kernel.
The `fw_cfg` DMA protocol itself lives once in the shared
`lib/fwcfg` crate (`tairix-fwcfg`, which also serves the aarch64
framebuffer boot console's ramfb path); this vertical supplies only
the riscv64 MMIO transport (`AGENTS.md` §2.2).

`tests/integration/framebuffer_display_qemu_aarch64`
(`tairix-test-framebuffer-display-qemu-aarch64`, enrolled in `cargo
xtask test --qemu`) is the aarch64 `virt`-board sibling of the riscv64
vertical, driving the same driver against a **real** emulated `ramfb`
framebuffer over the EL1/GICv2 path. It reuses the shared aarch64
bring-up (`tairix-test-virtio-qemu-support`'s FP-enable + 2 GiB identity
MMU + EL1 vectors) and the **same** shared `fw_cfg` MMIO transport
(`tairix-fwcfg`'s `MmioDma`) the riscv64 vertical uses — the two
`virt` boards expose `fw_cfg` identically, so there is one transport,
not two (`AGENTS.md` §2.2). Because QEMU's aarch64 `-kernel <ELF>` path
passes no DTB pointer, the vertical embeds the canonical `virt` device
tree (dumped at build time) to discover the `fw_cfg` base. The driver
lifecycle, `ramfb` programming, and pixel read-back are otherwise the
riscv64 scenario's siblings.

`tests/integration/framebuffer_display_wasm32`
(`tairix-test-framebuffer-display-wasm32`, enrolled in `cargo xtask test
--wasm`) is the wasm32 sibling: it drives the **same** framebuffer driver
in a real headless browser, against a static RGBA8888 surface in WASM
linear memory. Because wasm32 has no MMU, the surface is mapped through a
capability-checked `WasmMmioMapper` — a bounds- and `CAP_MMIO_MAP`-gated
view of the one in-memory surface, the MMU-less analogue of
`KernelMmioMapper`. Each presented frame is read back two ways: through a
second independently-mapped window (the bytes reached linear memory) and
through the new `tairix_host_present_framebuffer` host import, which
paints the surface onto an HTML `<canvas>` and returns the count of
pixels that survived the canvas round-trip — so the vertical proves the
pixels reach a genuine display surface, not just memory. The signed
`.rxe` load gate and the `load → use → unload → reload` lifecycle are the
bare-metal verticals' siblings (`AGENTS.md` §2.2); the driver itself is
byte-for-byte the same. See `docs/src/platform/wasm32.md` for the host
loader and harness.

### `tairix-drv-display-vesa`

The VESA driver presents the linear framebuffer a VESA BIOS Extensions
(VBE) mode exposes on a legacy x86_64 PC. Because the kernel cannot
re-enter real mode to issue VBE BIOS calls, mode selection happens in
the bootloader; the boot stub captures the 256-byte VBE `ModeInfoBlock`
(VBE function `0x4F01`) for a 32-bpp direct-colour linear-framebuffer
mode and hands it to the driver host as a boot capability.

`parse_mode_info` decodes and validates that block, accepting only a
supported mode whose linear-framebuffer attribute is set
(`ModeAttributes` bits 0 and 7), the direct-colour memory model
(`MemoryModel == 6`), 32 bits per pixel with 8-bit channel masks, and a
channel layout that is either `Bgra8888` (red at bit 16) or `Rgba8888`
(red at bit 0). A zero `PhysBasePtr`, a stride too small for one
scanline, or any other layout fails closed (`DeviceFault` /
`LengthOutOfRange` / `Unsupported`), so the byte-preserving `present`
copy never mis-renders.

`open` then hands the decoded geometry to the shared linear-framebuffer
engine, `tairix_display::Framebuffer`, which maps exactly
`stride_bytes * height_px` bytes at the reported `PhysBasePtr` through the
host's `MmioMapper` (which enforces `CAP_MMIO_MAP`); the framebuffer is
reached only through a kernel-installed mapping, never through a pointer the
driver synthesises from the block. Buffers, presents and lifecycle are the
framebuffer driver's: `register` clears `CAP_DRV_LOAD`, dropping the engine
releases the window (unload), and calling `open` again reloads.

#### QEMU integration vertical

`tests/integration/vesa_display_qemu_x86_64`
(`tairix-test-vesa-qemu-x86-64`, enrolled in `cargo xtask test --qemu`)
is the x86_64 sibling of the framebuffer vertical: it drives the driver
against a **real** emulated framebuffer, closing the `load → use →
unload → reload` loop on x86_64.

The harness attaches a QEMU `ramfb` device (`-device ramfb`) and
programs a static, page-aligned guest-RAM scan-out surface into it over
the `fw_cfg` **IOport** DMA interface (registers `0x514`/`0x518`). It
then synthesises the bootloader-captured VBE `ModeInfoBlock` describing
that surface — the shape a real VBE mode query (`0x4F01`) would produce
— as the boot hand-off, loads the signed vesa `.rxe` through
`tairix_drvhost::Host` (the §8 load gate), and for the "use" step decodes
the block with `open`, maps the surface through the
capability-gated `tairix_kernel_virtio::KernelMmioMapper`, and calls
`present`. A second window mapped over the same physical range reads the
pixels back to confirm they reached the scan-out memory, before and
after the reload.

The `fw_cfg` DMA protocol lives once in the shared `lib/fwcfg` crate
(`tairix-fwcfg`); this vertical supplies only the x86_64 IOport
transport, the deliberate sibling of the riscv64 MMIO transport
(`AGENTS.md` §2.2).

### `tairix-drv-display-gpu-virtio`

The 2D device of a virtio GPU (virtio 1.2 §5.7) as the seat's display, on
either transport; one signed bundle binds both. Kernel discovery publishes
device type 16 as a `Display` node, the first marked `ConsoleHost`.

- **The output** is the first scanout the device reports enabled, at the size
  its display information states, else the preferred timing of the EDID it
  offers. A `VIRTIO_GPU_EVENT_DISPLAY` configuration change re-reads it.
- **Presents.** Each frame is the backing of a 2D resource made at its first
  present, which transfers it whole. A present is one batch on the control
  queue — a transfer per damage rectangle, the scan-out switch, a flush per
  rectangle — published with one notify (`RequestQueue::submit_all_and_wait`)
  and answered on the device interrupt, so it answers `Shown`. The batch
  relies on the device processing its control queue in order, as every
  virtio-gpu device does and Linux's driver relies on.
- **The cursor plane.** A pointer image is copied into the driver's own
  64 × 64 image, transferred, and shown on the cursor queue at its hot spot.
- **The console.** It is hosted at the output's mode under the node's DMA
  grant, shown as a resource of its own while the seat shows text, its rows
  transferred on its damage wake, and re-hosted when the output moves —
  after a refusal too, its memory having gone back to the kernel.
- **Power.** `SET_SCANOUT` naming resource 0 switches the output off.
- **Memory.** A buffer the device may still read is never unmapped: a shown
  frame given back keeps its resource until replaced, a backing whose detach
  the device never answered is held, and a resource an unanswered batch may
  have made — a frame's, the console's, the pointer's — is unreferenced
  before its memory is used again or given back.

### `tairix-drv-display-bcm2711-hvs`

The Raspberry Pi 4's display engine, driven natively as Linux's full-KMS
`vc4` drives it: the fifth-generation HVS composites the seat's surface into
the pixel valves feeding HDMI0 and HDMI1, and each HDMI block is an encoder
driven over [`encoder-v1`](../lib/encoder.md) by its own driver.

- **Its node.** The aarch64 port states the engine's fixed wiring on the HVS
  node, which the image's display overlay enables: the HVS window `hvs`, the
  pixel valves' windows `valve2` and `valve4` with `Encoder` links `hdmi0`
  and `hdmi1` to the blocks they feed (`tairix_vcmailbox::ENGINE_OUTPUTS`),
  the core clock's link, its bus's DMA windows, and the discovery-only
  `ConsoleHost` fact in place of a boot display node, so no generic
  framebuffer service binds the firmware's surface ([aarch64
  platform](../platform/aarch64.md)).
- **Takeover.** The engine first asks the firmware the size of the screen it
  lit (`GET_PHYSICAL_WH`, tag `0x0004_0003`, which the mailbox service admits
  only for the HVS node's driver) and has the kernel host the console on a
  block it owns below the HVS's DMA window from the bottom of memory, at that
  size (`console_adopt`), so nothing writes or scans out memory the firmware
  may take back; a console that cannot be hosted ends the driver with the
  firmware told nothing. The call waits until the boot has handed the console
  to `login`, so the takeover's mode set, which blanks a sink while it
  re-locks, never lands under the Supervisor window or a passphrase prompt.
  The engine then tells the firmware the display is the ARM's
  (`NOTIFY_DISPLAY_DONE`, tag `0x0003_0066`, which the mailbox service admits
  only for the HVS node's driver) and records the answer: the message header
  is all the firmware answers for a tag with no values, as Linux reads it, so
  a refused message is logged with the firmware's codes, and the display is
  taken over regardless, as Linux's `vc4` does. It brings down whatever pipe
  the firmware left running — the valve's video stopped and its pixels
  drained, the encoder disabled, the valve reset, the HVS channel the
  output's mux names stopped, the encoder powered down — before it sets the
  HVS up and every mode itself.
- **The mode set.** Up: the channel's lists written and the output muxed to
  it, the channel started, the encoder configured, the valve configured and
  enabled, the encoder prepared, the valve's video started, the encoder
  enabled. Down is the reverse. The encoder server holds the engine to that
  order, and the engine's tests run against it. An output whose sink lost
  what its link set it up with — the encoder's connector counting a lost
  link — is brought down and up again, the surface and its frames kept.
- **The surface.** The display takes the mode of the first output, in valve
  order, with a sink it can drive. Another connected output shows the same
  surface scaled to its mode with its aspect kept — the polyphase filter
  enlarging or shrinking by up to a third, the trapezoidal one shrinking
  further, through the Mitchell–Netravali kernel in display-list memory.
- **Presents write no pixel.** The engine carves every buffer its presenter
  fills in memory the HVS reaches, under the node's DMA window, and the
  display service grants it. A present writes each running channel a list
  naming the buffers — the output frame or one plane per layer, bottom first,
  cut where a layer runs off the surface and scaled by the HVS's filters, and
  the pointer on a plane of its own above — into the stack slot the channel
  neither composites nor latches next, and is `Busy` while no slot is free.
  Every running output's list is built before any is written: a stack one
  output cannot composite, its scaled layers outrunning that output's share of
  the line buffer, is refused `OutOfRange` with nothing moved. It answers
  `Queued` until every running channel has latched the list at a frame start,
  which the end-of-frame interrupt reports, so a present never tears and the
  presenter draws its next frame into the buffer no list names. An output
  brought down or switched off latches what it waited for at once, and a
  present the display service abandons at its deadline is withdrawn, leaving
  the stack the outputs latched before it. An output coming up that cannot
  show the stack shows black until the presenter's next one.
- **The cursor plane.** A pointer image is copied into one of the engine's
  own, and the pointer's position, size and image words are rewritten in the
  lists the channels read, so moving the pointer presents nothing; a changed
  size is narrowed first, so no state between the writes reads past an image.
  An image a list stopped naming is drawn into again only once two frames
  have ended on every running channel.
- **Memory a list names is never given back.** A buffer freed while a list a
  channel composites or latches next names it is kept until none does:
  unmapping it tells the kernel the HVS has stopped reading it. The
  end-of-frame interrupt is raised only while something waits on it.
- **Where the screen goes.** While the seat shows text the outputs show the
  boot console's surface scaled to fit; while another presenter takes over
  they show black, and a departed presenter's buffers are never shown
  again.
- **The core clock.** Each encoder is told what its pipe carries at the core
  clock's ceiling (`clock-v1`'s highest rate): a deep-FIFO valve composites a
  line's active pixels with an 8 MHz margin and no less than nine tenths of
  the pixel clock, a shallow one keeps pace with the clock. The core clock is
  raised to at least 500 MHz for a mode set and settles at what the running
  outputs need: raised before a channel can latch a larger stack, and lowered
  only once none still composites one. A rate the clock refused is not asked
  again until another is needed.

QEMU models no HVS; the register-level model carries the host tests and the
metal run is `plans/SOUND.md` SND19q.

### `tairix-drv-display-bcm2711-hdmi`

Each of the Pi 4's HDMI blocks as an [`encoder-v1`](../lib/encoder.md)
encoder and an [`audiochan-v1`](../lib/audiochan.md) audio device, driven as
Linux's `vc4_hdmi` drives it, its sink found and its mode chosen by the driver
while the display engine takes it through the mode set and the mixer plays
through it. One thread serves both (`tairix_encoder::serve_with_audio`).

- **The connector.** Each hotplug line wakes it to read the connector and the
  sink's EDID over E-DDC on its DDC bus, a block at a time, retrying a sink
  still powering up. A sink whose EDID cannot be read is given 1024x768 at
  60 Hz as DVI.
- **The mode.** The sink's preferred timing when the transmitter, the link
  and the engine's pipe all carry it, else the largest they do. A DVI sink is
  held to a single link's 165 MHz, an HDMI sink to the rate its EDID states,
  and rates past 340 MHz need SCDC. A timing with an odd part of a line is
  passed over: the pixel valve clocks two pixels at a time. Where the board
  shares the 2.4 GHz band (the node's `wifi-2.4ghz-coexistence`), a link
  inside its first channel is pulled down to 238.56 MHz.
- **The mode set.** Configure floors the state machine clock at one percent
  over the pixel clock and the pixel path's clock for the rate, brings the
  PHY's PLL up and sets the timing; prepare sets the colour conversion —
  limited range for a consumer timing to an HDMI sink — and the FIFO; enable
  starts the video, switches the scheduler to HDMI, sends the AVI `InfoFrame`
  (the vendor frame too for one of the four 4K timings named by its HDMI
  code), and recentres the FIFO. A configure or an enable the hardware
  refuses is undone.
- **Scrambling.** Above 340 MHz the sink is told over SCDC to expect a
  quarter-rate clock and a scrambled link, then asked on a one-shot deadline,
  a second later, whether it sees it, and told again until it does. A sink
  replugged under a scrambled link has forgotten it, so the connector counts
  a lost link and the engine sets the output up again.
- **The state machine clock.** A register reached without it stalls the bus,
  so it is held while an output is up, while a stream is configured, and for
  each read of the connector, and let go otherwise; an output the firmware
  left running keeps the rate the firmware gave it until it is brought down.
- **Audio.** The jack is present while a connected HDMI sink declares audio.
  The endpoint offers the widest layout the sink's speakers place without a
  gap — stereo, 5.1 or 7.1 — at the rates its descriptors name for that many
  channels, as Linux's ELD constraints read them, and refuses a layout whose
  speakers the sink lacks, and states the widest sample the sink takes as its
  resolution, which the mixer dithers to. A stream is fed to the MAI by a
  DMA4 channel as IEC 60958 subframes carrying the rate and the sink's word
  length, the bits below it sent as zero, and the MAI is clocked from the
  DVP's audio clock, so a stream outlives an output brought down. While the link sends, N is the HDMI specification's
  recommended value and CTS the nearest whole count of the TMDS clock, and the
  audio `InfoFrame` names the channels' speaker allocation. A sink leaving
  keeps what the endpoint offers; a new sink's offer is a jack change, which
  the mixer re-reads.
- **Sound is the video's companion, never its precondition.** A node that
  links no audio clock or audio DMA line, or whose audio line opens no channel,
  is stated (`hdmi: the transmitter carries no sound`, with its cause) and its
  encoder served alone: the display comes up, and only HDMI audio is absent.

QEMU models no HDMI block; the register-level model carries the host tests
and the metal run is `plans/SOUND.md` SND19q.
