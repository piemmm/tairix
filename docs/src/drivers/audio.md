# Audio drivers

An audio device is one that presents **sinks** and **sources**: PCM endpoints
the machine clocks samples out of or into. `HwDeviceClass::Audio` names the
class, drivers live under `drivers/audio/<leaf>/`, and every one of them runs
in user space, bound by discovery-match, holding only the grants its matched
node requested — a register window, a DMA constraint, an interrupt line, or
the links to the DMA controller and clock it is fed by. None is in the
bootstrap floor: nothing about reaching the driver store needs sound.

The staged design is `plans/SOUND.md`; this page is the driver-class view. The
client half a program plays through is
[`audio-v1`](../abi/audio.md).

## One path, and where a driver sits in it

```
program ── audio-v1 ──▶ audiod ── audiochan-v1 ──▶ driver ──▶ hardware
              (PCM ring)          (PCM ring)
```

A driver never speaks to a program, and a program never speaks to a driver.
The mixer service is the single client of every audio device, and it is the
sole holder of the audio-device capability, so the kernel refuses at dispatch
every other caller of a driver's endpoint. A driver therefore never
re-checks: authority was settled before its code ran.

## The PCM vocabulary

`tairix_abi::driver::audio` is the vocabulary every layer shares, defined once
because four of them must agree on it exactly — the decoder that produces
samples, the engine that mixes them, the device channel that carries them, and
the driver that clocks them out.

| Type | What it fixes |
|---|---|
| `SampleFormat` | the six encodings the stack converts between, and each one's silence byte |
| `ChannelMap` | the interleave order, positions unique, `Mono` only alone |
| `Rate` | a sample rate inside the range a real converter runs at |
| `RateSupport` | what a device can be clocked at: a discrete list, or a continuous range |
| `Frames` | a monotone position from the start of a stream |
| `GainRange` | a hardware gain control in hundredths of a decibel |
| `AudioEndpointFacts` | everything the mixer needs to configure one sink or source |

Two details are load-bearing rather than decorative. Unsigned eight-bit PCM's
silence is `0x80`, not zero, so `SampleFormat::silence_byte` exists and a gap
filled without it would click. And `RateSupport` models both a crystal-driven
codec's handful of discrete rates *and* a USB Audio Class 2 clock source's
continuous range, because modelling only the first would force a continuous
device to publish an invented list.

There is no period or buffer *setting* in the facts — only the bounds the
hardware imposes. The depth is derived from those bounds and the client's
latency target, so no `const` period size exists anywhere in the stack.

## The PCM ring

`tairix_abi::driver::audio_ring` is one structure serving both hops, because
it is the same job twice: one producer appends interleaved frames, one
consumer takes them, and the two run concurrently in different address spaces.

The header carries two free-running `u64` **frame** positions — total produced
and total consumed — and they never wrap: at 192 kHz a `u64` runs for about
three million years. Occupancy is their plain difference and the slot index is
a mask of the low bits, so the class of wrap-around bugs that byte-indexed
rings spend their lives fixing does not arise. It also means a position on the
wire and a position in the ring are the same number, which is what makes
"start at frame N" and "we lost frames N..M" exact arithmetic.

The ordering discipline is the usual one and is stated in the crate: the
producer writes a frame's bytes then **releases** its position; the consumer
**acquires** that position before reading those bytes and releases its own only
once it has finished with them. The two positions sit in separate cache lines,
because in one line every publish would invalidate the peer's read of the
other. `lib/abi/tests/audio_ring_spsc.rs` drives both sides concurrently and
asserts every frame crosses exactly once, in order, intact.

Both positions live in memory the *peer* can write, so every operation
snapshots them once, refuses a backwards or over-full pair as
`Errno::OutOfRange`, and works from that snapshot. Even the publish arithmetic
is checked: a peer that parks the producer position at the top of the counter
cannot make a write overflow it.

## `audiochan-v1` — the device channel

`tairix_abi::driver::audio_channel` is the control plane, shaped from
`netchan-v1` with one deliberate difference: **configuration comes before
attachment**.

| Operation | What it does |
|---|---|
| `Facts` | what the device is, and how many endpoints it presents |
| `EndpointFacts` | what one sink or source can do |
| `Configure` | program rate, format, channel layout and period — answered with what the device could actually meet |
| `Attach` | hand over the granted sample region and name the notify port |
| `Start` / `Stop` / `Drain` | transport, at exact frame positions |
| `Service` | the doorbell: move one period, and report the clock pair |
| `Gain` | hardware gain and mute |
| `Detach` | release the channel |

A device answers `Configure` with a `ConfigureGrant` — the rate it *will* run
at rather than a refusal — and `ConfigureGrant::geometry` is the one place both
sides derive the shared region's shape from, so they cannot disagree about how
large it is.

Notifications run the other way: `PeriodElapsed` carries the `(position,
sampled_at)` pair the mixer's per-device clock fit is built from, `Xrun` says
exactly which frames were lost, `Drained` that a drain played out,
`JackChanged` reports a connector, and `Faulted` that the driver could not go
on serving an endpoint — its device faulted while being serviced, or a
transfer the hardware ended could not be started again. The endpoint raises
no period after that, so without `Faulted` nothing would ever tell the mixer;
`audiod` takes the device as lost and every stream on it holds its position
as `DeviceLost`. A frame a notification's kind does not define must be zero,
so a second meaning cannot be smuggled into the fixed frame.

### Discovery and the endpoint block

A driver claims the first free id in the reserved
`AUDIO_CHANNEL_ENDPOINT_BASE` block (spelled `"ACHAN"`), so two audio drivers
never collide without a central allocator. Binding a reserved id requires
`CAP_IPC_BIND_PRIVILEGED`, so an unprivileged squatter cannot impersonate a
driver; the driver additionally binds it restricted-sender on the audio-device
capability, so only the mixer can reach it.

The driver then publishes a hardware-tree node carrying
`AUDIOCHAN_NODE_COMPATIBLE` (`tairix,audiochan`), which the device manager
recognises as a bound audio device's channel and hands to the mixer. The key
is defined beside the endpoint block so the key emitted and the key looked for
cannot drift.

With the channel the device manager hands over its **location**: a keyed hash
of the device's place in the hardware tree — each ancestor's class, bus
address, first register window and rank among its siblings — so the same
hardware in the same port answers the same location on every boot, whatever
order it was found in. A channel whose node leaves the tree is retired from the
mixer (`UnbindDriver`) and its streams are told `DeviceLost`; one that returns
is handed over again as a new device. Once the root volume is mounted the
manager also delivers the machine's baseline from `system.conf`
(`audio.output`, `audio.input`, `audio.level`). All three operations are the
manager's alone, under `CAP_DRV_LOAD`.

### The driver copies, on purpose

Once per period a driver copies between the shared ring and its own DMA
buffer. A zero-copy arrangement would mean publishing the driver's DMA window
to another process; the driver owning that window absolutely is worth more
than the copy costs, which at 48 kHz stereo 32-bit and a five-millisecond
period is under 400 KiB/s.

### Nothing spins

Between doorbells a driver parks on its device's event sources: its interrupt
line, the ports a bus driver reports the device's progress on, or the answer
to the period wait it posted to a DMA controller (`tairix_audiochan::Wake`). When a period elapses it wakes the mixer, and the
mixer — parked on that port in its wait set — issues the next `Service`. The
device's own period events are the only timer in the stack.

## Fail closed

Every decode on this surface is total and validates whole. An unknown magic,
version or operation byte, a dirty reserved field, an endpoint index past the
device's own bound, a channel map with a repeated position, a ring depth the
index arithmetic could not serve, or a notification carrying a field its kind
does not define refuses with one typed `Errno` rather than guessing.
`lib/abi/tests/fuzz_audio.rs` drives every decoder on this page with mutated
and pure-noise frames, and drives the ring over positions a hostile peer could
have written.

## The class trait

`tairix_abi::driver::audio::Audio` is what every `drivers/audio/*` engine
implements and what `lib/audiochan`'s serve loop is written once over, so the
whole control plane exists in one place rather than per device. It is
deliberately the device's own vocabulary and nothing above it: report the
device's facts and each endpoint's, `configure` an endpoint and answer what
the hardware will actually run at, `start`/`stop`/`drain` it, move one period
between the caller's ring view and the device with `service`, `set_gain`,
`release`, and — for the driver process's interrupt path — `take_interrupt`
and `set_event_interrupts`.

There is no mixing, no conversion and no routing here, because those belong to
the one engine in `lib/audio`; a driver that did any of them would be a second
one.

A playback endpoint is **primed**: the mixer services it before starting it,
the driver takes only whole periods then and holds them, and the start begins
the device on them, so the first thing heard is the mixer's first frame rather
than a gap. A start with nothing primed begins on one period of silence,
counted lost, because a device with nothing in flight finishes nothing and so
never raises the period that would have it serviced. A service's
`transferred` counts frames that moved through the ring, never silence the
driver supplied, and the loss tally counts from the configuration, so a
stop and restart keep it.

`AudioInterrupt` is what an endpoint's interrupt had to say, as three bitmaps
over endpoint index — period elapsed, under/over-run, jack changed. A bitmap
rather than a list because a device with several streams running signals them
together and the serve loop must not allocate on the interrupt path.

## The first driver: `drivers/audio/virtio_snd`

Virtio sound (virtio 1.2 §5.14, device type 25) over the bus-agnostic
split-virtqueue transport, on either bus: the single-aperture virtio-MMIO
device a `-M virt` machine presents and the scattered virtio-PCI device a PC
presents. One signed bundle binds both. It is the cheapest complete driver and
the one that gives an end-to-end QEMU vertical on every port QEMU emulates
(x86_64, aarch64 and riscv64), so the whole path is proved against it before a
second driver exists.

What it reports is what the device said. Bring-up reads the device's own
configuration and enumerates each jack, stream and channel map with an
information request; there is no table keyed on what the device claims to be.
Where the device publishes nothing the driver says so rather than inventing an
answer: no jacks means every endpoint reports `JackState::Unknown`, and no
channel maps means the conventional layout for the reported channel count — or
a refusal, for a count with no conventional reading. A device that advertises
a count and then declines the query reaches the same undescribed state: jacks
and channel maps are descriptive, so a refusal costs their description and not
the device. A refused *stream* description stays fatal, because a stream that
cannot be described cannot be driven.

A rate or encoding the device cannot do is **substituted** with the nearest it
can, so the mixer adapts and owns the conversion the difference implies.

### The position never lies

A running playback device must be fed every period. When the mixer's ring is
short *and the device has nothing left in flight*, the driver submits silence
for exactly the frames it is missing and adds them to the stream's loss tally.
Padding a period the device has not yet asked for would manufacture a glitch
out of frames that were merely going to arrive in time, so it does not; a
drain's tail goes as a short transfer rather than a padded one, because
nothing was lost. A capture period the mixer's ring could not hold is over-run,
and it is counted too.

The reported position is what the device has *clocked out*, not what it was
handed: the transfer status' `latency_bytes` is subtracted from the submitted
total.

### Nothing is freed under the device

Every stream of a direction shares one transfer queue, sized when the device
comes up for every period all its streams keep in flight at once — every
stream in both directions, since a stream's direction is known only once it
is enumerated — and a device whose queue cannot hold them is refused then,
before it is given the ring. Each chain a transfer queue holds is recorded
under its descriptor head, so a completion reaches whichever stream posted
it, whichever stream's service collected it, without a search. Each period's
status word is zeroed before it is posted, so a completion that wrote no
status is refused rather than read as the buffer's last one; a lent period
is kept in that record, room carved at bring-up, so letting a stream go never
allocates; and a period is filled from the mixer's ring only when its queue
has room to post it, so the frames of a period lent chains leave no room for
stay in the ring. Each event slot is zeroed before it is reposted, and a
drain of the event queue takes at most a ring's worth of events.
A period the device still holds when its stream is released or reconfigured is
lent until the device hands it back — only an acknowledged `PCM_RELEASE`
promises that, and it is followed by collecting everything the device
returned — and is never freed before; a control request the device leaves
unanswered holds back the next until it is answered. The device resets when
the driver drops it, and one that will not reset keeps every ring, pool and
period for the kernel's DMA quarantine.

## `drivers/audio/usb_uac` — USB Audio Class 1.0 and 2.0

One driver per audio function. It binds the function's control interface
(class `0x01_01_00` or `0x01_01_20`), claims the streaming interfaces the
function groups with it — the 1.0 header's list, or the 2.0 interface
association — and serves each as one endpoint of the device. It holds no
register, DMA or interrupt: control requests and isochronous streams go
through the URB transport its host controller serves.

What it reports is what the function's descriptors say:

* **Topology.** The control interface's entity graph — terminals, mixer,
  selector, feature, processing, extension and effect units, and 2.0's clock
  sources, selectors and multipliers. An endpoint is named for the terminal at
  the outside world's end of its signal path ("Speaker", "Headset",
  "Microphone"), and its gain is the first feature unit on that path with a
  volume the host may set. Every walk visits an entity once, so a cyclic graph
  ends rather than spins.
* **Formats.** Every alternate setting's encoding, channel cluster, rates and
  endpoints. A sample is left-justified in its subslot, so a 24-bit sample in
  four bytes is `S32`. A setting the vocabulary cannot carry exactly — signed
  8-bit, A-law, a position with no name, a rate outside `Rate`'s bounds — is
  left out rather than approximated.
* **Rates.** 1.0 lists them per setting and sets them on the data endpoint
  where it states a frequency control. 2.0 reads them from each route to a
  clock source — every pin of a programmable selector, a fixed one's current
  pin, a multiplier's ratio — as the standard rates (`STANDARD_RATES`) its
  ranges admit, and sets them on the source: selector pins first, then the
  frequency, read back, and the clock's validity checked. A clock another
  endpoint runs from is never retuned under it — neither a source it shares
  run at another frequency nor a selector it runs through steered to another
  pin (`Busy`), refused before the bus changes at all.

Configuring an endpoint selects the setting carrying the asked-for channel
count, rate and encoding nearest, and sizes its stream: slots of whole service
intervals covering the period, enough of them to stay sixteen milliseconds
ahead of the bus, within one controller ring. The grant states the period the
slots actually carry.

### Feedback

* **Synchronous and adaptive** endpoints carry the nominal rate, spread over
  intervals exactly (`PacketPacer`): 44.1 kHz over 1 ms frames is 44 frames
  nine times and 45 the tenth.
* **Explicit feedback** runs a stream on the feedback endpoint and follows
  each report it reads (`FeedbackDecoder`), from the next interval on.
* **Implicit feedback** paces an asynchronous OUT endpoint with no feedback
  endpoint of its own from its function's capture endpoint — one stating
  implicit-feedback usage, or the one its `bSynchAddress` names. When nothing
  is capturing, the driver selects a setting on the capture interface at the
  playback rate and runs its stream for the rate alone.

### The position never lies

A position is the device's own timeline, each value stamped with the moment
the host controller finished the slot that carried it. Frames a slot carried
advance it; so do intervals the bus skipped between slots, and frames a missed
or failed interval carried — both counted as lost. A prime sets the stream up
and holds the whole slots the ring fills, unqueued, so nothing clocks before
the start queues them. Once clocking, a playback slot the ring cannot fill
waits while others are in flight and is padded with counted silence only when
the device would otherwise run dry; a drain sends what is left and stops. A
capture slot delivers whole frames, and what the ring cannot hold is counted
as over-run.

A notice is believed only from the process that delegated its stream's
region, and only when it names the stream's number, so a notice a stopped
stream left behind is never read as one about its successor on the same
endpoint.

Every stream start re-establishes its interface from the driver's own record —
claimed, its setting selected, its rate and the mixer's gain set — because a
controller reset forgets all of it, an idle endpoint's included. A data or
feedback stream the host controller ends under a device that is still there is
started that way again, the frames it held counted lost; one whose device went,
or one that cannot be started again, faults the endpoint, every later service
answers the fault until the mixer stops or starts it, and the mixer is told at
once (`Faulted`). A configuration that fails part-way puts the previous setting
and rate back, and where that fails too leaves the endpoint unconfigured, so
nothing streams in a setting its grant does not describe.

QEMU's `usb-audio` device carries the end-to-end vertical on x86_64, aarch64
and riscv64 (`tests/integration/audio_qemu_*`): the kernel discovers the
`qemu-xhci` function, `devmgr` autoloads the host-controller driver and then
this driver, and the capture must hold the fixture's signal sample for sample.
QEMU records that capture with its mixing engine off, because its emulated
volume would otherwise scale every sample; the file then carries the device's
own stream unconverted, under the default rate QEMU labels it with.


## `drivers/audio/hda` — Intel High Definition Audio

The audio on PC motherboards, on graphics cards' HDMI and DisplayPort
outputs, and in QEMU as `intel-hda`. One driver per controller: it binds the
PCI class (`0x04_03_00`), resets the controller, starts its command and
response rings and its DMA position buffer, and walks every codec that
announces itself on the link.

**A codec is read, never looked up.** Each audio function's widgets are read
for their capabilities, connection lists and pin configuration defaults, and
planned into endpoints from that alone:

* Output connectors are routed back to converters breadth-first, nearest
  first, each widget visited once, so a cyclic or deep graph costs one visit
  per widget. The analogue pins of one association, ordered by sequence,
  become one output of up to eight channels, a converter per pair in HDA's
  sequence order — front, centre and LFE, rear, side. A pin left without a
  converter of its own plays the front pair of an output it can reach, and
  plugged headphones silence the speakers they share a converter with.
* Input connectors are routed to converters that can capture them,
  preferring one no other input has; inputs that share one refuse to run
  together.
* A route's selectors are pointed along it and its amplifiers opened at
  0 dB, a mixer's other inputs shut. The endpoint's gain is the first
  adjustable amplifier from its converter outwards, and its mute the first
  mutable one; with no mute, its pins stop driving.
* HDMI and DisplayPort pins are named for their monitor from the display's
  ELD and are present while it is valid; configuring one sends the audio
  infoframe.

**Codec commands wait on the response ring's interrupt.** A command parks on
the controller's line until its answer lands; a stream's period that ends
during the park is cleared in the controller, so a message-signalled line
raises again, and kept for the next read of the causes. A command that times
out restarts both rings, discarding everything in flight, since an answer
arriving after its deadline could not be told from the next command's.

**Streams** run over a cyclic buffer of four periods, one buffer descriptor
each, interrupting at every period's end. As with the cyclic engine, the
period after the one playing is always written, as silence counted lost when
the mixer has supplied nothing. Positions are read from the DMA position
buffer the controller writes, which costs a memory read rather than a
register read; a descriptor's reset zeroes its entry, because the controller
writes it again only once the descriptor moves.

## `drivers/audio/bcm2711_pwm` — the Raspberry Pi 4's headphone jack

The board wires two channels of the BCM2711's PWM block to its 3.5 mm jack
through an RC filter. The driver binds the block the image's overlay names
`tairix,bcm2711-pwm-audio` — a PWM block is a general part, and only the
board knows which one drives its jack — and is a consumer of two links:
the PWM clock (`clock-v1`, the [clock manager](clock.md)) and a cyclic DMA
channel (`dmaengine-v1`, the [DMA engines](dma.md)), both through
[`tairix-linkclient`](../lib/linkclient.md).

* **The jack runs at 375 kHz**, each PWM period 250 cycles of a 93.75 MHz
  clock the clock manager makes from PLLD by a whole divisor, so without MASH
  jitter. It offers that one rate; the mixer's resampler converts to it.
* **Each sample is noise-shaped onto the 250 levels** by third-order error
  feedback with triangular dither inside the loop. Measured by the crate's
  tests, the 20 Hz – 20 kHz noise of the duty stream is −90.8 dBFS under a
  −6 dBFS tone and −91.0 dBFS under a −60 dBFS one, 30 dB below rounding onto
  the same levels. The PWM pad and the board's analogue stage bound what the
  jack plays, and are measured on metal.
* **It streams through the shared [cyclic engine](../lib/audiochan.md#the-cyclic-engine)**,
  its frames the shaped duty words: four periods, a boundary the answer to a
  posted DMA wait, and the period after the one playing always written, as
  silence counted lost when the mixer has not supplied it.
* **Between streams the jack is parked at silence**: the buffer is filled with
  silence and the channel stopped once silence has reached the FIFO, so the
  PWM, which repeats its last word when its FIFO runs dry, holds silence with
  nothing running. At bring-up the jack ramps from the PWM's idle low to
  silence over about 44 ms, so no start or stop pops.

## Codecs: `codec-v1`

A codec is a device of its own, driven by its own driver, while the audio
channel is the digital audio interface's: the interface's driver serves the
mixer and calls the codec its link names (`tairix_abi::driver::codec`), through
[`tairix-linkclient`](../lib/linkclient.md). Discovery reads the link from the
board's generic `simple-audio-card`: the framing, which side drives the bit
and frame clocks, which runs inverted, and the interface on each side ride in
the link's selector,
so the codec's driver reads them from the attested link
([`tairix-codec`](../lib/codec.md)).

| Operation | Answer |
|---|---|
| `Describe` | the rates, sample widths and framings the codec takes, whether it can drive the clocks, and its gain range if it has one |
| `Configure { rate, width }` | the interface set up in the link's framing, clock sides and inversions, a frame two slots of `width` bits |
| `Gain { millibel, mute }` | the gain set, the step at or above the one asked |
| `Start`, `Stop` | the output brought up, or taken down |

* **`drivers/audio/pcm5102a`** has no control port and binds with nothing: it
  answers with what the part takes — 8 kHz to 384 kHz, 16-, 24- or 32-bit, I²S
  or left-justified as its pin is strapped — and no gain, and refuses a link
  asking it to drive the clocks or to invert one.
* **`drivers/audio/pcm5122`** reaches its part over the I²C transfer endpoint
  its node's grant names. It sets the part to follow the interface's clocks,
  its PLL fed from the bit clock, programs the framing, the word length and
  the bit clock's polarity — the part has no frame clock polarity, so a link
  inverting that is refused — and serves the part's digital volume, 24 dB to
  −103 dB in half-decibel steps, as the stream's gain. A stop waits up to 10 ms for the soft mute to settle
  before standby, so the clocks stop under a silent part.

## `drivers/audio/bcm2711_i2s` — the PCM/I²S block

The BCM2711's PCM block is a digital audio interface. The driver binds
`brcm,bcm2835-i2s`, which a HAT's overlay enables beside the
`simple-audio-card` linking it to the HAT's codec, and is a consumer of three
links: a cyclic DMA channel on its `tx` request line, the PCM clock, and the
codec. Two drivers compose over one stream: the interface serves the mixer,
and the codec's driver its part.

* **The endpoint is the codec's.** It offers the widest sample the codec takes
  whose FIFO word is a ring's own — 32-bit, 24-bit in 32, or two 16-bit
  samples to a word — so a frame is copied as it is and any narrowing is the
  mixer's, dithered; the codec's rates within the block's 8 kHz to 384 kHz;
  and the codec's gain.
* **Each sample has a slot as wide as itself, two slots a frame**, in the
  link's framing as Linux's `bcm2835-i2s` programs it. Where this side drives
  the bit clock the clock manager runs it at the rate times the frame's bits,
  and a clock made more than 100 ppm off is refused; where the codec drives
  it, the block follows.
* **A stream starts from a cleared FIFO with transmit off.** The block takes
  words for its two channels in turn, so one word left over would swap them.
  The DMA channel fills the FIFO, transmit goes on, then the codec comes up —
  first instead where it drives the bit clock, since a clear completes only on
  a running one.
* **A stream ends with its last frames heard.** A stop mutes the codec first;
  the [cyclic engine](../lib/audiochan.md#the-cyclic-engine) parks the buffer,
  and transmit goes off and the codec down only once the channel has halted,
  so a drain's tail plays out.
* **One codec, playing.** A card linking several codecs to the interface would
  need time slots its link does not describe, and is refused; `codec-v1`
  describes converters that play, so the block's receive side is unused.
