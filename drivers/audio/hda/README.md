# tairix-drv-audio-hda

TAIRiX Intel High Definition Audio driver (`plans/SOUND.md` SND11). Stability
tier: **experimental** — it tracks the unfrozen `abi-v1` `Audio` class trait
and the `audiochan-v1` contract.

## Supported hardware

Any HD Audio controller (PCI class `0x04_03_00`, any vendor) and every codec
on its link: the audio on PC motherboards since 2004, HDMI and DisplayPort
audio on a graphics card or integrated GPU, and QEMU's `intel-hda` with its
`hda-output`, `hda-duplex` and `hda-micro` codecs.

What it presents is what each codec states, read through its widgets'
capabilities, connection lists and pin configuration defaults — there is no
table of boards or codecs:

* Each output connector is routed back to a converter of its own; the
  analogue pins of one association become one output of up to eight
  channels, a converter per pair in sequence order (front, centre and LFE,
  rear, side). A pin left without a converter of its own plays the front pair
  of an output it can reach, and plugged headphones silence the speakers they
  share a converter with.
* Each input connector is routed to a converter that can capture it; inputs
  sharing a converter cannot run together (`Busy`).
* An HDMI or DisplayPort pin is named for its monitor from the display's ELD,
  is present only while that ELD is valid, and is told its channel count by
  an audio infoframe.
* Gain and mute are the first adjustable and the first mutable amplifier from
  each converter outwards; an endpoint with neither leaves its level to the
  mixer, and one that cannot mute switches its pins off instead.

## Limitations

* **Encodings** are 16-bit, and 20-, 24- or 32-bit samples in a 32-bit
  container (`S16`, `S32`); 8-bit and float streams are not carried.
* **Display outputs** carry the front pair: the infoframe states no speaker
  allocation past it.
* **Jack detection** follows a pin's unsolicited responses only where the pin
  can sense presence and its configuration does not say presence detection
  is unwired; any other jack is `JackState::Unknown`, a fixed device always
  present.
* **A board wired contrary to its codec's own defaults** gets the answer its
  codec gave; per-machine corrections have no place in a driver that names no
  board.

## Capabilities

`CAP_MMIO_MAP` (the controller's register window), `CAP_MEM_DMA` (its rings,
position buffer and stream buffers), `CAP_IRQ_BIND` (its interrupt),
`CAP_SHM` (the mixer's regions), `CAP_IPC_ENDPOINT` and
`CAP_IPC_BIND_PRIVILEGED` (the reserved device-channel endpoint),
`CAP_HW_EMIT` (its `audiochan` node) and `CAP_LOG_EMIT`.

## Loading

Loadable and unloadable at runtime. Bring-up stops anything a previous
instance left running and resets the controller before touching its memory;
a controller that will not stop has its memory withheld rather than reused.

## Tests

Host tests run the format words, the graph read, the endpoint planning and
the engine against a register-level model of the controller — its command and
response rings, stream descriptors, buffer descriptor lists and position
buffer — and modelled codecs: QEMU's `hda-output` exactly, a desktop codec
with a seven-point-one association, a headphone jack left without a
converter, a cyclic loopback mixer, S/PDIF and three inputs behind selectors,
a laptop's speaker and headphones on one converter, and an HDMI connector
with and without a monitor. The end-to-end QEMU verticals
(`audio_qemu_{aarch64,riscv64,x86_64}` with the HD Audio disk) play the audio
fixture through `intel-hda` and assert the capture sample for sample.
