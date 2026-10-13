# Display drivers

Display drivers present a single linear pixel surface to the
compositor (`userland/gui/wm`). They implement
[`tairix_abi::driver::display::Display`](../abi/driver_traits.md) and
are loaded as user-space drivers; compositing, damage tracking, and
GPU acceleration live above this trait, not inside it.

## Class trait

`Display` is intentionally minimal:

| Method           | Purpose                                  | Capability gate          |
|------------------|------------------------------------------|--------------------------|
| `output`         | report the `DisplayMode { width_px, height_px, stride_bytes, format }` the output runs, or `None` with no sink | `DriverHandle` ownership |
| `service_events` | collect what woke the display's event sources and move `output` to match | `DriverHandle` ownership |
| `device_report`  | report the device's own memory and compositor, for the monitor | `DriverHandle` ownership |
| `present`        | copy a fully-rendered frame to the surface | `DriverHandle` ownership |
| `present_rects`  | present a full frame of which only a list of `DamageRect`s changed | `DriverHandle` ownership |
| `set_power`      | switch the display `On` or `Off` (`DisplayPower`) | `DriverHandle` ownership |

Pixel encodings are `DisplayFormat::Rgba8888` and
`DisplayFormat::Bgra8888` (4 bytes per pixel). Per `AGENTS.md` §2.9 the
trait never panics: a frame shorter than `stride_bytes * height_px`
maps to `DriverError::BufferTooSmall`.

`present_rects` is the damage-aware present (`plans/DISPLAY.md` D7b):
`frame` still carries the whole surface, and `damage` names the
rectangles that changed since the previous present — one to
`MAX_DAMAGE_RECTS`, kept disjoint by the caller so no pixel is blitted
twice. The **whole list** is validated against the active mode
(`DamageRect::validate_list` — non-empty, within the bound, each
rectangle non-empty and wholly on-surface, overflow-checked) before any
pixel access, so one bad rectangle refuses the present rather than
leaving the ones before it on screen.

One call carries a whole frame's damage. A frame that changed two
far-apart places — a menu and the title band its owner repainted — costs
one dispatch and two rectangle-sized blits, never a blit of the box
spanning them and never a dispatch each. The trait
supplies a full-blit default so every driver stays correct unchanged;
a driver whose scan-out path is a copy overrides it to blit only the
touched scanline spans (the framebuffer driver does). The WM
compositor threads its composited damage bounds through this method,
so a small screen update costs a small copy end to end.

### The present right follows the live seat lease

Every present/flip path is additionally gated on the presenting
client's *current* seat lease (`plans/DISPLAY.md` D4): at `open` a
driver captures its host's `DriverHost::seat_gate()` — a
`tairix_abi::driver::display::SeatGate` the kernel bound to the
client's `SeatLease` — and consults it at the **top** of `present` (and
`present_layers`), before any validation or surface access. A client
whose lease was revoked is refused with the distinct
`DriverError::SeatRevoked` even though its framebuffer mapping
(`CAP_MMIO_MAP`) still exists; any other dead handle is
`DriverError::PermissionDenied`, and the refused frame never touches
scan-out. A host with no seat wired (headless, boot bring-up, unit
seams) exposes no gate and the present proceeds ungated — there is no
lease to derive the right from. See
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
VBE mode — and the presenter then keeps its screen black and still instead,
so it spends nothing on the screen either way. A driver that can reach the
panel overrides it: the Pi 4's display engine brings its outputs down, so a
sink sees no signal and sleeps.

A power-down never outlives the configuration that asked for it.
`DisplayServer` tracks the display as unverified, on, or off: its release of a
configuration — the lease that made it ended, or a newer owner configured —
switches a dark display back on, and a service that has just started knows
nothing of the display's state, so its first configuration switches it on
too. The service learns that a lease ended from the kernel's `DisplayLease`
system notice (`docs/src/abi/notice.md`), so a session that crashes behind a
dark screen leaves the machine lit for whoever comes next. Only a lease's end
or a newer `Configure` releases a configuration: a refused request — malformed,
from a caller without the lease, or for a seat nobody configured — changes
nothing.

### Optional hardware acceleration

A driver whose hardware can composite a stack of planes itself also
implements `AcceleratedDisplay: Display` — the GPU-accelerated path
(`AGENTS.md` §10):

| Method           | Purpose                                            |
|------------------|----------------------------------------------------|
| `accel_caps`     | report `AccelCaps { max_layers, max_width_px, max_height_px, per_layer_opacity }` |
| `present_layers` | composite a back-to-front `AccelLayer` stack and scan it out |

Each `AccelLayer` carries premultiplied-alpha source pixels in the
display's active format plus a destination origin and a per-layer
opacity; the engine blends them and scans the result out, so the host
never composites the whole screen in software. The software
`Display::present` path is always the mandatory fallback: the
compositor (`Compositor::present_accelerated`) drops back to it when the
scene exceeds the reported `AccelCaps` (too many layers, or a layer
larger than the engine can source), so a hardware frame is never
partial (§2.9).

## Shipped drivers

| Driver       | Crate                                | Surface source                            | Stage 4 status        |
|--------------|--------------------------------------|-------------------------------------------|------------------------|
| framebuffer  | `tairix-drv-display-framebuffer` (Run process over `tairix_display::Framebuffer`) | firmware linear framebuffer (GOP / Pi mailbox / `ramfb`) | host-side tests + riscv64 & aarch64 `ramfb` QEMU verticals + wasm32 browser-canvas vertical |
| vesa         | `tairix-drv-display-vesa`            | x86_64 VBE linear framebuffer (`ModeInfoBlock`) | host-side tests + x86_64 `ramfb` QEMU vertical |
| bcm2711_hvs  | `tairix-drv-display-bcm2711-hvs`     | the Pi 4's HVS and HDMI pixel valves, driven natively, its encoders over `encoder-v1` | host-side tests against a register-level model |
| bcm2711_hdmi | `tairix-drv-display-bcm2711-hdmi`    | not a surface: each Pi 4 HDMI block, an `encoder-v1` encoder of the display engine and an `audiochan-v1` audio device | host-side tests against a register-level model |

The two display drivers are deliberate siblings (`AGENTS.md` §2.2
carve-out), not duplicates: `vesa` owns the VBE-specific decode, while
the framebuffer path consumes an already-parsed geometry record.

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
service synthesises (`AGENTS.md` §4 — no ambient authority). `present`
is byte-preserving: it copies the caller's frame verbatim into the
mapped window, bounds-checked at every write; `present_rects` blits
only the validated damage rectangles, and refuses the whole list
rather than write part of it.

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
kernel's display-lease notice: every request that
acts *for a seat* — `Query` included — is gated on the in-flight caller's
live seat lease through `call_peer_seat`, a `Configure` maps the client's
`shm_grant`ed frame region once (sized from the kernel's own record of
the region length via `shm_map_from`'s `len_out`, never the client's
claimed geometry, and mapped only as the attested caller's own delegation),
and a `Present` blits by frame index — zero frame
bytes ever cross the IPC.

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
the memory and compositor capabilities the driver reports through
`Display::device_report`, and the mode being scanned out, or none while
there is no output. The default
`device_report` is honest for a firmware framebuffer — no memory of its
own, no hardware compositor — and an `AcceleratedDisplay` driver overrides
it with its real `AccelCaps`.

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

Clients establish frames one way, `tairix_display::establish_frames` (feature
`rt`): the shared region, its grant to the service, the configure, and the
ring that owns the region and unmaps it when dropped.

Lifecycle: the signed-bundle load gate clears `CAP_DRV_LOAD`;
`Framebuffer::open` maps the surface; dropping the `Framebuffer`
releases the window (unload); calling `open` again reloads. The engine
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

`VbeModeInfo::parse` decodes and validates that block, accepting only a
supported mode whose linear-framebuffer attribute is set
(`ModeAttributes` bits 0 and 7), the direct-colour memory model
(`MemoryModel == 6`), 32 bits per pixel with 8-bit channel masks, and a
channel layout that is either `Bgra8888` (red at bit 16) or `Rgba8888`
(red at bit 0). A zero `PhysBasePtr`, a stride too small for one
scanline, or any other layout fails closed (`DeviceFault` /
`LengthOutOfRange` / `Unsupported`), so the byte-preserving `present`
copy never mis-renders.

`VesaFramebuffer::open` then maps exactly `stride_bytes * height_px`
bytes at the reported `PhysBasePtr` through the host's `MmioMapper`
(which enforces `CAP_MMIO_MAP`); the framebuffer is reached only through
a kernel-installed mapping, never through a pointer the driver
synthesises from the block (`AGENTS.md` §4). Lifecycle and `present`
semantics match the framebuffer driver: `register` clears
`CAP_DRV_LOAD`, dropping the `VesaFramebuffer` releases the window
(unload), and calling `open` again reloads.

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
the block with `VesaFramebuffer::open`, maps the surface through the
capability-gated `tairix_kernel_virtio::KernelMmioMapper`, and calls
`present`. A second window mapped over the same physical range reads the
pixels back to confirm they reached the scan-out memory, before and
after the reload.

The `fw_cfg` DMA protocol lives once in the shared `lib/fwcfg` crate
(`tairix-fwcfg`); this vertical supplies only the x86_64 IOport
transport, the deliberate sibling of the riscv64 MMIO transport
(`AGENTS.md` §2.2).

### `tairix-drv-display-bcm2711-hvs`

The Raspberry Pi 4's display engine, driven natively as Linux's full-KMS
`vc4` drives it: the fifth-generation HVS composites the seat's surface into
the pixel valves feeding HDMI0 and HDMI1, and each HDMI block is an encoder
driven over [`encoder-v1`](../lib/encoder.md) by its own driver.

- **Its node.** The aarch64 port states the engine's fixed wiring on the HVS
  node, which the image's display overlay enables: the HVS window `hvs`, the
  pixel valves' windows `valve2` and `valve4` with `Encoder` links `hdmi0`
  and `hdmi1` to the blocks they feed (`tairix_vcmailbox::ENGINE_OUTPUTS`),
  the core clock's link, its bus's DMA windows, and, where the boot lit a
  framebuffer console, the discovery-only `BootConsole` fact in place of a
  boot display node, so no generic framebuffer service binds the firmware's
  surface ([aarch64 platform](../platform/aarch64.md)).
- **Takeover.** The kernel first moves the boot console off the firmware's
  memory onto a block it owns below the HVS's DMA reach (`console_relocate`),
  so nothing writes or scans out memory the firmware may take back; a console
  that will not move ends the driver with the firmware told nothing. The call
  waits until the boot has handed the console to `login`, so the takeover's
  mode set, which blanks a sink while it re-locks, never lands under the
  Supervisor window or a passphrase prompt. The
  engine then tells the firmware the display is the ARM's
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
- **Presents.** Every display list is written before its channel can read it.
  A present copies into a frame no running channel composites or latches next,
  and each channel latches it at its next frame start, so a present never
  tears and never waits; with two frames per channel in flight and one being
  written, three frames serve one output and four serve two. A frame is
  brought up to date with only the damage presented since it was last written.
- **Where the screen goes.** While the seat shows text the outputs show the
  boot console's surface scaled to fit; while another presenter takes over
  they show black. A departed presenter's frames are forgotten, so the next
  presenter's first present rewrites a frame whole before any channel shows
  it.
- **The core clock.** Each encoder is told what its pipe carries at the core
  clock's ceiling (`clock-v1`'s highest rate): a deep-FIFO valve composites a
  line's active pixels with an 8 MHz margin and no less than nine tenths of
  the pixel clock, a shallow one keeps pace with the clock. The core clock is
  raised to at least 500 MHz for a mode set and settles at what the running
  outputs need.

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
