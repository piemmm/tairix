# `tairix-encoder`

The driver side of `encoder-v1`, the seam a display engine speaks to the
encoder its link names — an HDMI transmitter, driven by a driver of its own
while the scan-out is the engine's ([display drivers](../drivers/display.md)).

## The connector

The connector is the encoder's to know: whether a sink is connected, and the
mode it runs for that sink, its own choice within the sink's limits and its
own. An encoder with a sink it can drive none of the modes of reports it
connected with no mode. Any holder of an encoder link may read it with
`Describe`.

The engine states what its pipe carries with `Limit`: the fastest pixel
clock its timing generator takes, and the fastest it composites a line's
active pixels (the clock scaled by the active share of the line). The
connector's mode is the best one that fits, so a mode the pipe cannot carry —
4K at 60 Hz behind a core clock the firmware caps — gives way to the next,
and the change is a move.

The connector also counts the times a sink lost what its running link set it
up with — the scrambling an HDMI 2.0 sink forgets when it is replugged faster
than its hotplug is read. An output brought up under an older count is
brought down and up again, as DRM's link status asks of a compositor; the
loss is a move like any other.

Each move of the connector is counted. A `Wait` names the count its holder
has seen and is answered at once when the count differs — the connector moved
since, or the encoder restarted and counts afresh — and otherwise when the
encoder's event source next moves it; one wait is posted at a time, and a
second is `Busy`. The engine posts it with no deadline and
parks on its reply, so a sink arriving days later wakes it, and nothing polls.

## The mode set

A mode set goes up as `Configure` (for the connector's mode), `Prepare` and
`Enable`, interleaved with the engine's own timing generator, and comes down
as `Disable` and `PowerDown`. The server holds the steps to that order:

| From | Admits |
|---|---|
| `Off` | `Configure`; `PowerDown`, which changes nothing |
| `Configured` | `Prepare`; `PowerDown`, abandoning the set |
| `Prepared` | `Enable`; `PowerDown`, abandoning the set |
| `Enabled` | `Disable` |
| `Disabled` | `PowerDown` |

A step out of order is `OutOfRange` and reaches no hardware. A `Configure`
for a mode that is not the connector's — or with no sink — is `Stale`: the
engine re-reads the connector and follows it. A device's refusal leaves the
phase where it was.

The server starts from the phase the hardware is in, which the encoder reads
once: a firmware that left its pipe running leaves the output `Enabled`, and
an engine taking over disables and powers it down before it sets a mode of
its own.

## The holder

The steps and the wait are the holder's: the first engine to ask, until that
instance ends. A holder whose end has not reached the server yet is found out
by the kernel's refusal to watch it, so a restarted engine takes over at
once. A holder that ends leaves the phase standing, and its posted wait goes
with it.

A refusal from the encoder's driver travels as `encoder-v1`'s
`refusal_reason` states — a mode it cannot set as `NotSupported`, a held
encoder as `Busy` — and the engine's client reads it back with `refusal`.
`NotFound` is the transport's alone: the encoder is gone. A device's own
`NotFound` travels as a fault.

## The serve loop

`serve` binds the encoder node's endpoint under its encoder duty and parks on
it, on peer exits and on the encoder's own event sources (its hotplug line,
typically), which it hands to `Encoder::service_event` by index. A check the
hardware raises no interrupt for — whether a sink sees its link scrambled —
is an `Encoder::deadline_us`: the loop waits no longer than it and calls
`Encoder::expired` when it comes, so the core sleeps between checks. An event
source that faults ends the driver with `SERVE_FAILED` and its reason logged.

An encoder whose connector also carries audio serves both from one thread
with `serve_with_audio`: the audio device channel
([`tairix-audiochan`](audiochan.md)'s `Channel`) has its endpoint — a
`DeviceWake::Calls` source, a second endpoint the driver serves — and its own
event sources in the same wait set, and reads its device's causes after
everything the encoder does, since a hotplug or a mode set may have changed
what the audio endpoint offers.

Every decision is logged with a stable event id:

| Event | Id |
|---|---|
| Held by an engine | 24800 |
| Its holder ended | 24801 |
| The connector moved | 24802 |
| A request refused | 24803 |
| The service ended | 24804 |

The engine's half is [`tairix-linkclient`](linkclient.md)'s `EncoderClient`.
