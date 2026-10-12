# `tairix-drv-display-bcm2711-hvs` — Raspberry Pi 4 display engine

The seat's display on a Raspberry Pi 4, driven natively as Linux's full-KMS
`vc4` drives it: the HVS composites the display's surface into the pixel
valves feeding the HDMI outputs, and the encoder behind each valve is driven
over `encoder-v1` by its own driver (`plans/SOUND.md` SND19m). Stability
tier: `experimental`.

The crate has two targets:

- `src/lib.rs` — `Engine`, the display over the HVS's and the valves' register
  windows and its links' clients, host-tested against a register-level model
  whose encoders are answered by the real `encoder-v1` server.
- `src/main.rs` — the `Run` binary `devmgr` autoloads against the HVS node.

## Supported hardware

The BCM2711's fifth-generation HVS (`brcm,bcm2711-hvs`) and its two
HDMI-feeding pixel valves: pixel valve 2 on HVS output 4 feeding HDMI0, and
pixel valve 4 on output 5 feeding HDMI1. The node carries the HVS window as
`hvs`, the valves' windows as `valve2` and `valve4` with `Encoder` links
`hdmi0` and `hdmi1` to the blocks they feed (`tairix_vcmailbox::ENGINE_OUTPUTS`,
the wiring the aarch64 port states), the core clock's link, the DMA windows
its scan-out reads memory through, and, where the boot lit a framebuffer
console, the `BootConsole` fact.

## What it does

- **Takes the display over.** It first has the kernel move the boot console
  off the firmware's memory onto memory the kernel owns and the HVS reaches
  (`console_relocate`); a console that will not move ends the driver with the
  firmware still told nothing, so the screen stays as it was. Then it tells
  the firmware the display is the ARM's (`NOTIFY_DISPLAY_DONE`), records the
  firmware's answer, and takes the display over whatever it is, as Linux's
  `vc4` does: it brings down any pipe the firmware left running in the order
  the hardware needs — the valve's video, the encoder, the valve, the HVS
  channel, the encoder's power — and sets every mode itself.
- **One surface, every output.** The display's surface takes the mode of the
  first output with a sink it can drive; another connected output shows the
  same surface scaled to its own mode, its aspect kept. An output whose sink
  lost its link is set up again, the surface kept.
- **Presents never tear or wait.** A present copies into a frame no channel
  composites or latches next, and each channel latches it at its next frame
  start; a frame brought up to date copies only the damage it lacks. Three
  frames serve one output and four serve two.
- **The screen goes where the seat does.** While the seat shows text the
  outputs show the boot console's surface, scaled to fit; while a presenter
  takes over they show black; a departed presenter's frames are never shown
  again.
- **Every pipe within the core clock.** Each encoder is told what its pipe
  carries at the core clock's ceiling, so 4K at 60 Hz is chosen only where the
  firmware lets the core run fast enough, and the core clock is floored at
  what the running outputs need.

## Limitations

- QEMU models no HVS, so the engine is proven on the host against the model;
  the metal run is `plans/SOUND.md` SND19q.
- One plane a channel: the desktop composites in software and presents one
  frame.

## Required capabilities

- `CAP_MMIO_MAP` — the HVS's and the valves' windows.
- `CAP_MEM_DMA` — the scan-out frames, and the console's move onto memory
  the HVS reads.
- `CAP_SHM` — the client frames a present names.
- `CAP_IPC_ENDPOINT` — the calls to its encoders and its core clock.
- `CAP_IPC_BIND_PRIVILEGED` — the seat's display endpoint.
- `CAP_MAILBOX` — the takeover notice, which the mailbox service admits only
  for the HVS node's driver.
- `CAP_LOG_EMIT` — the decisions it records.

It runs in user space and never requests `CAP_DRV_KERNEL`.

## Events

| Event | Id |
|---|---|
| The firmware's pipe brought down | 24900 |
| An output up | 24901 |
| An output down | 24902 |
| An output left dark until its connector moves | 24903 |
| An encoder's driver ended | 24904 |
| A valve outran its budget stopping | 24905 |
| The core clock refused a floor | 24906 |
| Bring-up failed | 24907 |
| The firmware gave the display up | 24908 |
| The firmware did not confirm giving the display up | 24909 |
