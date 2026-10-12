# `tairix-drv-display-bcm2711-hdmi` — Raspberry Pi 4 HDMI transmitters

Each of the BCM2711's two HDMI blocks as an `encoder-v1` encoder and an
`audiochan-v1` audio device, driven as Linux's `vc4_hdmi` drives it: the
display engine (`bcm2711_hvs`) takes the encoder through a mode set's steps,
the mixer (`audiod`) drives the audio endpoint, and this driver finds the
sink, chooses the mode, programs the block and plays the sink's audio
(`plans/SOUND.md` SND19n, SND19o). Stability tier: `experimental`.

The crate has two targets:

- `src/lib.rs` — `Hdmi`, the encoder and audio device over the block's
  register windows and its links' clients, host-tested against a
  register-level model of the block, its clocks, interrupt lines and DMA
  channel, and a sink answering on its DDC bus.
- `src/main.rs` — the `Run` binary `devmgr` autoloads against an HDMI node,
  serving the encoder link and the audio channel from one thread.

## Supported hardware

`brcm,bcm2711-hdmi0` and `brcm,bcm2711-hdmi1`. The node carries the windows
`hdmi`, `dvp`, `phy`, `rm`, `packet`, `csc` and `hd` (the last shared by both
blocks, each reaching only its own video control and MAI), clock links
`hdmi` (the state machine clock), `bvb` (the pixel path's) and `audio` (the
MAI's, the DVP's gate), the `ddc` link to its DDC bus, the binding's
`hpd-connected` and `hpd-removed` interrupt lines (its fifth and sixth, by
position: their names are longer than a link carries), the `audio-rx` DMA
link feeding the MAI, and its encoder duty. HDMI0 reaches 600 MHz; HDMI1
340 MHz, its lanes wired swapped.

## What it does

- **Finds the sink.** Each hotplug line wakes it; it reads the connector and
  the sink's EDID a block at a time over E-DDC, its segment pointer reaching
  past the first two, retrying a sink that does not answer yet. A sink whose
  EDID cannot be read is given 1024x768 at 60 Hz as DVI.
- **Chooses the mode.** The sink's preferred timing when the transmitter, the
  link and the engine's pipe all carry it, else the largest they do: a DVI
  sink is held to a single link's 165 MHz, an HDMI one to its stated rate,
  past 340 MHz only with SCDC. Timings with an odd part of a line or repeated
  pixels are passed over. On a board sharing the 2.4 GHz band, a link inside
  its first channel is pulled down to 238.56 MHz.
- **Sets the mode.** The state machine clock at least one percent over the
  pixel clock, the pixel path's clock for the rate, the PHY's PLL, drive and
  lanes, the timing, the colour conversion — limited range for a consumer
  timing to an HDMI sink — and, for an HDMI sink, the AVI `InfoFrame`, with
  the vendor frame naming one of the four 4K timings by its HDMI code.
- **Scrambles HDMI 2.0 rates.** Above 340 MHz the sink is told over SCDC to
  expect a quarter-rate clock and a scrambled link, and is asked a second
  later, on a one-shot deadline, whether it sees it — told again until it
  does. A sink replugged under a scrambled link has forgotten it: the
  connector counts a lost link and the engine sets the output up again.
- **Holds its clock only while it needs it.** No register is reached unless
  the state machine clock runs, so it is held while an output is up, while a
  stream is configured, and for each read of the connector, and let go
  otherwise. A configure or an enable the hardware refuses is undone, leaving
  the block as the step found it.
- **Plays the sink's audio.** While a connected HDMI sink declares audio the
  endpoint's jack is present, and it offers the widest layout the sink's
  speakers place without a gap (stereo, 5.1, 7.1) at the rates the sink's
  descriptors name for that many channels, stating the widest sample it takes
  as the endpoint's resolution for the mixer to dither to; a layout whose
  speakers the sink lacks is refused. A stream is fed to the MAI by DMA as
  IEC 60958 subframes carrying the rate and the sink's word length, the bits
  below it sent as zero, the MAI clocked from the audio clock so the stream
  outlives an output brought down; while the link sends,
  N and CTS follow its TMDS clock and the audio `InfoFrame` places the
  channels. A sink leaving keeps the offer and reports the jack absent; a new
  sink's offer is reported as a jack change for the mixer to re-read.

## Limitations

- QEMU models no HDMI block, so the driver is proven on the host against the
  model; the metal run is `plans/SOUND.md` SND19q.
- RGB at eight bits a channel only; no YCbCr, deep colour or HDR.
- Linear PCM audio only: no compressed passthrough or high-bit-rate audio,
  no gain of its own (the mixer applies it), and no capture.
- One CTS value: the registers that would alternate two to make a
  fractional CTS exact are undocumented on the BCM2711, so at a 44.1 kHz
  family rate a TMDS clock that is not a whole multiple of 900 Hz has its CTS
  rounded.

## Required capabilities

- `CAP_MMIO_MAP` — the block's windows.
- `CAP_SHM` — the DDC channel's buffer and the audio DMA buffer.
- `CAP_IPC_ENDPOINT` — the calls to its clocks, its DDC bus, its interrupt
  controller and its DMA channel.
- `CAP_IPC_BIND_PRIVILEGED` — its encoder and audio channel endpoints.
- `CAP_HW_EMIT` — the `tairix,audiochan` node by which the mixer finds the
  audio channel.
- `CAP_LOG_EMIT` — the decisions it records.

It runs in user space and never requests `CAP_DRV_KERNEL`.

## Events

| Event | Id |
|---|---|
| Bring-up failed | 24910 |
| The sink's EDID could not be read | 24911 |
| The sink lost its link's scrambling | 24912 |
| The sink has not seen the link scrambled | 24913 |

The encoder server's own decisions are `lib/encoder`'s, 24800 to 24804, and
the audio channel's are `lib/audiochan`'s, 4210 to 4212.
