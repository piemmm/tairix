# `tairix-drv-display-rpi-fb` — Raspberry Pi firmware-framebuffer display service

The display service for the scan-out surface the Raspberry Pi's VideoCore
firmware allocates. The surface is plain linear memory, so its pixels go
through the one linear-surface engine (`tairix_display::Framebuffer`) and the
one service loop (`tairix_display::service`) exactly as the generic
framebuffer service's do. What this driver adds is the one control a generic
framebuffer cannot reach: switching the display off, through the firmware's
blank request (`RPI_FIRMWARE_FRAMEBUFFER_BLANK`, tag `0x0004_0002`) over the
mailbox service. The desktop asks for it when the screensaver's energy-saving
wait runs out, and asks again for the display back at the first input.

The crate has two targets:

- `src/lib.rs` — `FirmwareDisplay`, the surface wrapped with its power switch,
  plus the canonical `BIND_KEYS` and `REQUIRED_CAPABILITIES` the signed
  manifest is built from. Host-tested against the protocol-faithful mock
  firmware in `lib/vcmailbox`.
- `src/main.rs` — the `Run` binary `devmgr` autoloads. It builds the driver
  host from the kernel's grants, maps the granted surface, and serves it.

## Supported hardware

The boot display the aarch64 port brings up through the VideoCore firmware on
a Raspberry Pi. The port publishes that display node with the firmware
framebuffer's own binding, `brcm,bcm2708-fb`, ahead of the generic
`simple-framebuffer` key, so this driver binds at priority 20 over the generic
service's 10. The surface's base, geometry and pixel format come from the
node's `Framebuffer` resource, never from a board constant.

## Limitations

- Whether a blanked HDMI output also drops its signal, so the monitor itself
  sleeps, is the firmware's `hdmi_blanking` setting. Either way the panel
  shows nothing and the desktop draws nothing for it.
- QEMU models no VideoCore firmware, so the power switch is proven on the host
  against the mock firmware; on metal it is part of the `plans/PI.md`
  acceptance run.

## Required capabilities

- `CAP_MMIO_MAP` — mapping the scan-out window the kernel granted.
- `CAP_SHM` — mapping the client's granted frame region at `Configure`.
- `CAP_IPC_BIND_PRIVILEGED` — binding the reserved `DISPLAY_ENDPOINT`.
- `CAP_LOG_EMIT` — the one-shot first-present record.
- `CAP_MAILBOX` — the firmware property exchange the power switch is.

The service runs in user space and never requests `CAP_DRV_KERNEL`. Every
request, the power switch included, is gated on the caller's live seat lease,
and the display is switched back on whenever the lease that switched it off
ends, so no presenter can leave the machine dark behind it.

## Failure behaviour

Bring-up failures exit with the display service's reserved codes (no host: 80,
no surface grant: 81, surface map failed: 82, endpoint bind failed: 83,
wait-set failed: 84). A firmware that refuses or garbles the blank request is
answered as a typed error to the session, which keeps its screensaver black
instead and says so once.

## Runtime load and unload

Loadable and unloadable like every display service: dropping the service
releases the surface window, and loading it again maps it afresh.
