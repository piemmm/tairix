# `tairix-audiochan`

`lib/audiochan` is the **driver side of the `audiochan-v1` audio
device-channel contract** (`plans/SOUND.md` SND4): everything an audio driver
process must do around an opened device to serve the mixer, written once so
every audio driver shares one control plane.

## Why it exists

The mixer service and an audio driver run as separate processes. The mixer
owns the shared PCM regions and is the channel's *client*; the driver owns the
device (MMIO/DMA/IRQ) and is its *server*. The wire codecs for that contract
live in `lib/abi::driver::audio_channel`, but the *server behaviour* is not a
wire type — it is per-endpoint configuration and attach state, geometry
validation, and a wait-set loop.

It is separate from `lib/audio` for the reason `lib/netchan` is separate from
`lib/net`: **a driver process must not link the mixer.** The engine that
decides what samples come out is one crate and the device-channel server is
another, so a sound card's driver carries no mixing code at all.

## Three layers

`AudioChannelServer<A: Audio>` is the pure, host-testable per-request handler.
It performs no I/O: the caller receives the request, maps the granted regions,
and sends the reply this server produces.

`Dispatcher<A, I: ChannelIo>` is the serve loop's work without its syscalls:
one call answered, or one device event serviced, over an injected `ChannelIo`
that receives calls, maps and unmaps the mixer's regions, and sends its
notifications. The whole control plane — attach and detach, the interrupt
path's refills and notifications — is host-tested against a mock device and a
mock channel.

`serve` is the freestanding shell, compiled only for the bare-metal targets a
driver binary is built for. It claims a reserved device-channel endpoint bound
**restricted-sender on `CAP_AUDIO_DEVICE`**, publishes the `tairix,audiochan`
hardware-tree node the device manager hands to the mixer, and parks on a wait
set over `{call endpoint, device interrupt}`, handing each wake to the
dispatcher.

## The cyclic engine

A device a DMA controller feeds from a looping buffer — the Raspberry Pi's
headphone jack and its I²S interface — streams the same way whatever its FIFO
wants, so `cyclic::CyclicPlayback` is that stream once, over two seams:
`DmaPort`, the channel (`cyclic::LinkDma` in a driver process, the device's
link to its [DMA controller](../drivers/dma.md) through
[`tairix-linkclient`](linkclient.md)), and `FrameCodec`, how a frame sits in
the buffer.

- **Four periods, one written ahead.** A boundary is the answer to a posted
  wait, which the serve loop wakes on (`Wake::CallReply`). Each frees the
  period just played and the next period of the ring goes into it. The period
  after the one playing is always written: as silence counted lost when the
  mixer has not supplied it, since a period left alone replays a lap-old
  sound.
- **A frame is copied where it lies.** A buffer frame is as many bytes as a
  ring frame of two channels in the codec's format, and must be whole 32-bit
  FIFO words, so a period is read from the ring straight into the buffer and
  encoded in place.
- **The position is the channel's**: its boundary count, stamped with the
  time the controller serviced the boundary.
- **A stream ends parked.** Stopping, a drain playing out and a release fill
  the buffer with the codec's parked frame and stop the channel at its next
  boundary, once that frame has reached the device. `is_clocking` says whether
  it still runs, and `halt_parking` stops a parking channel at once.
- **Bounded.** A period holds at most 16384 frames and the buffer four of
  them; a boundary not answered within twice the buffer's span ends the stream
  as a fault rather than leave it waiting on a stalled channel.

The modelled channel every such driver's tests drive the engine over is
`cyclic::mock`, behind the `mock-dma` feature, so none carries a private copy.

## State is per endpoint

A device presents several sinks and sources and each is driven independently,
so one channel carries several endpoints' state rather than one channel's.

- An endpoint starts **unconfigured**: `Facts` and `EndpointFacts` answer and
  every transport call refuses with `NotConnected`.
- `Configure` programs the hardware and records the grant — the device's own
  answer about what it will actually run at.
- `Attach` validates the offered ring against *that recorded grant*, through
  `ConfigureGrant::geometry`: the single derivation both sides size the region
  from, so they cannot disagree about how large it is. A refused attach leaves
  no state and no mapping, and a refused re-attach leaves the endpoint
  detached: its old region was let go before the new one was offered.
- A reconfiguration drops the attached region rather than leaving it the wrong
  shape, and the serve loop unmaps it in the same step.
- `Detach` releases the endpoint's device-side stream. A release the hardware
  refused still forgets the channel state, because the process is about to
  unmap the region either way.

A grant whose period rounds up past its own ring ceiling admits no
power-of-two ring at all, so `ConfigureGrant::validate` refuses it as a device
fault at `Configure` rather than at every later `Attach`. The same validation
runs on the mixer's side of the wire, so a grant that reached the mixer is one
the mixer can size a region from.

## The interrupt path services, and that is deliberate

A period interrupt *means* "the device has consumed a period; refill it". The
region is already mapped in the driver, so making the mixer ask for the refill
with a blocking call would cost two extra process switches per period on the
one path in the system whose whole job is not to have jitter — and would put
the refill deadline behind the mixer's scheduling latency rather than the
driver's.

So the driver moves the period itself and then sends one notify carrying the
`(position, sampled_at)` pair the mixer's linear clock fit is built from. The
ring's atomic counters are what make that safe, and its release/acquire edge
carries a `loom` model in `lib/abi`.

An under- or over-run is reported as the **delta** since the previous service,
because the wire report already carries the running total and the notify is
about what just happened.

The device's causes are read after every call as well as on every event wake.
A call that waited on the device — a codec verb, a control request — may have
consumed the very wake an elapsed period raised, and a stream whose last
period is never serviced never reports its drain.

## Nothing spins

Between events the process parks on its wait set. The device's event sources
are masked whenever nothing is attached — so a device left clocking cannot
storm a driver with nowhere to put frames — and released again on the mixer's
next `Service`. The device's own period interrupt is the only timer in the
stack.

## Fail closed

Every reply is a fully-encoded `audiochan-v1` frame carrying a typed `Errno`:
an endpoint index the device does not present, a transport call before attach,
a region that does not match the agreed geometry or is not aligned for the ring
counters, or any device fault. Never a panic, never a partially-applied
action. Set-up refusals in `serve` return a reserved `exit` code — the same
numbers `lib/netchan` uses, so one supervisor table reads both classes — so a
driver that cannot serve ends with a diagnosable reason rather than degrading
into a busy re-poll.

## Stability

**experimental** — it tracks the unfrozen `abi-v1` `audiochan-v1` contract.
