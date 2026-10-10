# tairix-drv-audio-usb-uac

TAIRiX USB Audio Class driver (`plans/SOUND.md` SND7). Stability tier:
**experimental** — it tracks the unfrozen `abi-v1` `Audio` class trait, the
`audiochan-v1` contract and the URB transport's stream operations.

## Supported hardware

Any USB Audio Class 1.0 or 2.0 function — headsets, DACs, interfaces,
speakers — at full, high or SuperSpeed, through whichever host controller
publishes its control interface. It binds by class alone (`0x01_01_00`,
`0x01_01_20`); a driver naming an exact device outranks it.

Each streaming interface is one endpoint of the device, its direction, name,
formats, rates and gain read from the function's own descriptors and
controls: there is no table of devices. Synchronous, adaptive and
asynchronous endpoints are served, asynchronous ones paced by their explicit
feedback endpoint or, where they have none, by their function's capture
endpoint (implicit feedback).

## Limitations

* **Encodings** are the Type I family the audio vocabulary carries: PCM in 2,
  3 or 4-byte subslots, unsigned 8-bit PCM8, and 32-bit float. Signed 8-bit
  PCM, A-law, µ-law and the Type II/III compressed formats are left out.
* **Channel positions** are the 7.1 set the vocabulary names; a setting
  placing a channel elsewhere (left of centre, top) is left out.
* **Rates** set through a 2.0 clock are the standard rates its ranges admit.
* **Jack detection** is reported as `JackState::Unknown`: neither revision
  states a connector in a way this driver reads.
* **Controls** used are a feature unit's volume and mute; tone, gain and
  processing controls are not.
* **USB Audio 3.0** and MIDI functions are other drivers' work.

## Capabilities

`CAP_SHM` (its interface's shared buffer and its streams' regions),
`CAP_IPC_ENDPOINT` (its URB endpoint and its stream ports),
`CAP_IPC_BIND_PRIVILEGED` (the reserved device-channel endpoint), `CAP_HW_EMIT`
(its `audiochan` node) and `CAP_LOG_EMIT`. No register window, DMA or
interrupt: everything it does to the hardware it asks its host controller for.

## Loading

Loadable and unloadable at runtime. A function that goes is retracted with its
interface node, which unloads the driver; one whose host controller resets
under it has its streams restarted.

## Tests

Host tests run the topology, format, request and stream code against QEMU's
own `usb-audio` descriptors and version 2.0 functions in real devices' shapes,
and the engine against a mock of the host controller's streams: sample order
and exact packet sizes, slots held by a prime and queued by the start, a
start with nothing held, padding and drains, skipped and missed intervals,
explicit and implicit feedback, notices from a stranger, halts and restarts,
streams that cannot be set out whole, and clock contention. The end-to-end QEMU verticals
(`audio_qemu_{aarch64,riscv64,x86_64}` with the USB disk) play the audio
fixture through QEMU's `usb-audio` behind `qemu-xhci` and assert the capture
sample for sample.
