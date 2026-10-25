# tairix-vcmailbox

The single **BCM2711 `VideoCore` firmware mailbox property-channel client**
(`AGENTS.md` §2.2 — `plans/PI.md` P7/P7b).

On a Raspberry Pi the GPU firmware owns the display pipeline; the ARM side
talks to it over the mailbox property channel — a 16-byte-aligned buffer of
little-endian `u32` tag words posted through the doorbell registers. This
crate owns that protocol once:

- the **pure framing layer**: `FramebufferRequest::encode` /
  `decode_framebuffer_response` (allocate a scan-out surface) and
  `encode_display_size_query` / `decode_display_size_response` (probe the
  attached display's EDID-derived geometry; `0×0` means no display), and
  `encode_display_done` (the display engine's driver telling the firmware
  the display is its own), whose answer `decode_display_done_response`
  confirms only for a tag the firmware processed and otherwise reports in
  the firmware's own codes (`Unconfirmed`). Every firmware answer is
  validated fail-closed — the firmware is an external input (`AGENTS.md`
  §5.4).
- the **bus ↔ ARM-physical translation** (`bus_to_arm_physical`,
  `arm_physical_to_bus`, `DEFAULT_BUS_ALIAS`) over the 30-bit `VideoCore`
  SDRAM aperture, failing closed on anything outside it.
- the **transport seam**: `MailboxTransport` with `MmioMailbox` as the metal
  doorbell implementation over two capability-gated `RegisterWindow`s, whose
  reply waits spin within a poll budget each (the pre-MMU boot path has no
  scheduler to park on), and `DmaMailbox` as the same over a carved property
  buffer it owns, which it withholds rather than frees while the firmware owes
  it a reply, and whose reply waits park on the inbox interrupt
  (`InboxInterrupt`) until a deadline of their own. QEMU
  does not model the firmware, so host tests drive the seam with a
  protocol-faithful mock and the doorbell is the on-metal acceptance item
  (`AGENTS.md` §2.1).
- the **firmware clocks and GPIO expander**: `query_clock_rate` /
  `set_clock_rate` over `FirmwareClock` (the ARM clock the frequency driver
  moves, the EMMC2 base clock the SD host divides), and `set_gpio_state` for
  the expander lines only the firmware drives (`FIRMWARE_GPIO_COMPATIBLE`;
  on a Pi 4 the SD card's power and I/O-voltage rails).
- the **real-time clock registers** (`RtcRegister`, `read_rtc_register` /
  `write_rtc_register`): the Pi 5's clock is inside the board's PMIC and is not
  memory-mapped, so the property channel is the only route to it
  (`plans/TIMESYNC.md` TS-4). Both directions require the per-tag response bit
  and check the register selector the firmware echoed, so a firmware that
  stamps success without processing the tag, or answers about a different
  register, is a fault rather than a plausible wall time.
- the **admission policy** (`admit_request`): the one table of requests the
  user-space mailbox service forwards, and the kernel's own channel to the
  firmware admits for its storage floor. Each is one tag, pinned word for
  word to the shape its driver sends, and admitted only from the driver the
  kernel attests is loaded for that tag's device. A GPIO switch is the SD
  host's alone, and only on a line its node's rails name (`HwNode::io_rail`,
  `HwNode::power_rail`, which only discovery states). Every other tag — code
  execution, GPU memory, voltage, one-time-programmable bits, any other GPIO
  line, framebuffer allocation, even the revision probe — is refused, and a
  malformed buffer is refused before any tag in it is trusted.
- the **client devices**: `DISPLAY_ENGINE_KEY`, `RPI_RTC_KEY`,
  `FIRMWARE_CLOCKS_KEY`, `SD_HOST_KEY` (from `SD_HOST_COMPATIBLE`, which the
  aarch64 port's discovery keys the SD host's node by too) and `VL805_KEY`
  (with `VL805_FIRMWARE_DEV_ADDR`, the one address the xHCI reset may name).
  Each driver's bind table is built from its key, so the device it binds is
  the device its requests are judged against. `SERVICE_CAPS` beside
  `BIND_KEYS` is the service's own capability set, the single source its
  bundle's manifest is authored from.
- the **display engine's wiring**: `ENGINE_OUTPUTS`, each HDMI output's pixel
  valve and encoder bindings and the names its valve's window and encoder
  link carry on the engine's node — what the aarch64 port states and the
  engine's driver finds by, defined once for both.

## Why it lives in `lib/` (the §2.20 / §2.22 carve-out, legitimately)

This is single-device support — it knows the BCM2711 `VideoCore` — yet it
**stays** in `lib/*`, unlike the VL805 / BCM2711-PCIe device logic, which was
collapsed into its driver crate (`AGENTS.md` §2.22). The difference is the
second consumer: independent consumers speak this protocol — the aarch64
port's framebuffer boot console (`kernel/arch/aarch64`, P7b), the display
engine (`drivers/display/bcm2711_hvs`, `plans/SOUND.md` SND19m), the firmware
clocks (`drivers/clock/rpi_firmware`), the VL805 firmware reload
(`drivers/bus/usb/vl805`, P10), the PMIC clock (`drivers/rtc/rpi`), the SD
host's driver (`drivers/storage/emmc2`, P8), and the aarch64 port's channel
the storage floor lends that driver (`kernel/arch/aarch64::firmware`). The
boot console is a
**charter-legal non-driver** consumer (a genuine early-boot need, not a
removable scaffold), so the §2.20 carve-out applies and the shared definition
belongs in `lib/*` (`AGENTS.md` §2.22 / §6 / §2.2); a driver crate may not be
a kernel dependency (`AGENTS.md` §17.4). By contrast the VL805 / PCIe device
logic had only a `drivers/*` consumer (its illegitimate second consumer was a
removed in-kernel scaffold), so it has no `lib/*` home. This crate depends
only on `lib/abi` (the `DisplayFormat` / `RegisterWindow` / `DmaSlab` /
`DriverError` vocabulary).

## Stability tier

`experimental` — the Raspberry Pi bring-up firmware seam. It is `no_std`,
with no `unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9). Its
one `unsafe` block builds the window `DmaMailbox` holds over its own slab, and
the crate runs under `cargo xtask miri`.
