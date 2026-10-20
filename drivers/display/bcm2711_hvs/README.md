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
its scan-out reads memory through, and the `ConsoleHost` fact.

## What it does

- **Takes the display over.** It first asks the firmware the size of the
  screen it lit and has the kernel host the console at that size on memory
  the kernel owns and the HVS reaches (`console_adopt`); a console that
  cannot be hosted ends the driver with the firmware still told nothing, so
  the screen stays as it was. Then it tells
  the firmware the display is the ARM's (`NOTIFY_DISPLAY_DONE`), records the
  firmware's answer, and takes the display over whatever it is, as Linux's
  `vc4` does: it brings down any pipe the firmware left running in the order
  the hardware needs — the valve's video, the encoder, the valve, the HVS
  channel, the encoder's power — and sets every mode itself.
- **One surface, every output.** The display's surface takes the mode of the
  first output with a sink it can drive; another connected output shows the
  same surface scaled to its own mode, its aspect kept. An output whose sink
  lost its link is set up again, the surface kept.
- **A present writes no pixel.** The engine carves its presenter's buffers
  in memory the HVS reaches, under the node's DMA window, and grants them;
  a present hands each running channel a list naming them, one plane per
  layer, bottom first, the pointer on a plane of its own above, in a stack
  slot neither the list the channel composites nor the one it latches next
  names — `Busy` until a frame start frees one. Every running output's list
  is built before any is written, so a stack one output cannot take is
  refused whole. The present is queued until every channel latches the list
  at a frame start, which the end-of-frame interrupt reports, and is latched
  at once for an output brought down or switched off meanwhile; the
  presenter draws into its other frame meanwhile. A present the display
  service gives up on at its deadline is withdrawn, and the outputs go on
  showing the stack they latched before it.
- **Layers are cut and scaled in the HVS.** A layer partly off the surface
  is read from its first visible pixel, and one shown at another size is
  scaled by the HVS's filters, within each channel's share of the line
  buffer; a stack outrunning it is refused `OutOfRange`. Each layer blends
  by its own premultiplied alpha, scaled by a constant opacity. An output
  coming up that cannot show the stack shows black until the presenter's
  next stack, which is built for it with the others.
- **The pointer moves without a present.** A pointer image is copied into
  one of the engine's own, and the pointer's position, size and pointer
  words are rewritten in the lists the channels read, as Linux's `vc4`
  updates a cursor, a changed size narrowed first so no word in between
  reads past an image; an image a list stopped naming is drawn into again
  only once two frames have ended on every running channel.
- **Memory a list names is never given back.** A buffer its presenter frees
  while a list a channel composites or latches next still names it is kept
  until none does: giving it back tells the kernel the HVS has stopped
  reading it. The frame-end interrupt is raised only while a present, a
  pointer image or such a buffer waits on it.
- **The screen goes where the seat does.** While the seat shows text the
  outputs show the boot console's surface, scaled to fit; while a presenter
  takes over they show black; a departed presenter's frames are never shown
  again.
- **Every pipe within the core clock.** Each encoder is told what its pipe
  carries at the core clock's ceiling, so 4K at 60 Hz is chosen only where the
  firmware lets the core run fast enough, and the core clock is floored at
  what the running outputs need: raised before a channel can latch a larger
  stack, lowered only once none still composites one. A floor the clock
  refused is not asked again until another is needed.

## Limitations

- QEMU models no HVS, so the engine is proven on the host against the model;
  the metal run is `plans/SOUND.md` SND19q.
- The end-of-frame interrupt reports a latch up to a frame late; a present
  is still shown at the frame start it latched at.

## Required capabilities

- `CAP_MMIO_MAP` — the HVS's and the valves' windows.
- `CAP_MEM_DMA` — the presenter's buffers and the pointer's images, and the
  console's memory the HVS reads.
- `CAP_SHM` — granting the buffers to the presenter.
- `CAP_IRQ_BIND` — the HVS's interrupt, which reports each frame's end;
  without it a present completes at the display service's deadline.
- `CAP_IPC_ENDPOINT` — the calls to its encoders and its core clock.
- `CAP_IPC_BIND_PRIVILEGED` — the seat's display endpoint.
- `CAP_MAILBOX` — the lit screen's size and the takeover notice, which the
  mailbox service admits only for the HVS node's driver.
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
| The HVS's interrupt could not be bound | 24907 |
| The firmware accepted the hand-over | 24908 |
| The firmware refused the hand-over | 24909 |

A driver's ending is the driver runtime's record (`tairix_drvrt::exit`).
