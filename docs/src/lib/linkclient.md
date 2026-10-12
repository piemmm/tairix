# `tairix-linkclient`

The consumer halves of the supplier links (`plans/SUPPLIERS.md`): the one way
a driver speaks to the drivers its node names. A supplier believes a request
only once the kernel attests the caller holds it, so a client quotes what
discovery gave its node and nothing of its own choosing.

## DMA channels

A `DmaClient` holds one channel of a [DMA controller](../drivers/dma.md),
opened on the request line its driver's node holds; the controller builds
every control block from attested facts. The client asks the controller to
carve the cyclic buffer and maps the grant it is handed through
`tairix_rt::shm::MappedGrant`, so no consumer builds a slice over a raw
address.

A period boundary arrives as the answer to a posted `Wait`, never a blocking
call: the client posts it with a deadline, the driver's serve loop waits on
its answer beside its other sources (a `DeviceWake::CallReply` handed to
[`tairix-audiochan`](audiochan.md)'s `serve`), and `reap_wait` collects it when the loop
wakes. One wait is outstanding at a time. A wait the transport refuses — a
controller that let the deadline pass — is spent and reported, so the driver
stops rather than waits on a channel that will never answer.

## Clocks

A `ClockClient` describes, runs and releases the clock its block runs from,
through the [clock controller](../drivers/clock.md) its node's clock link
names. A clock another process holds runs only at the rate it already has.

## Codecs

A `CodecClient` drives the [codec](../drivers/audio.md#codecs-codec-v1) a
digital audio interface's node links it to: what it accepts, its set-up for a
rate and sample width in the link's framing, its gain and its output. It
reads the framing, clock sides and inversions from the link's own selector,
so the interface frames its side exactly as the codec is told to. A refusal
comes back as the codec driver's own (`codec-v1`'s `refusal`): a framing it
does not take is `Unsupported`, a codec another interface holds `Busy`, and
one with no gain `NotImplemented`, which is how the interface leaves the gain
to the mixer.

## Encoders

An `EncoderClient` takes the [encoder](./encoder.md) a display engine's node
links it to through a mode set: `limit` for what the engine's pipe carries,
`configure` for the connector's mode within it, then
`prepare` and `enable` around the engine's own timing generator, and
`disable` and `power_down` on the way back. Each step blocks for the
encoder's answer. A mode the connector no longer runs comes back `Stale`, a
step out of order `OutOfRange`, and an encoder another engine holds `Busy`.

The connector's moves arrive as the answer to a posted `Wait` naming the
count of moves the engine has seen. It is posted with no deadline, since a
sink may not arrive for days: it is answered by a move, or ends with the
encoder, which comes back `NotFound` — the transport's alone, so an encoder
that ended is told from one that failed. The engine's serve loop parks on
the reply as a `DeviceWake::CallReply` naming the link's endpoint.

## I²C

An `I2cClient` holds a channel on an [I²C controller](./i2cbus.md), opened
over the I²C link its node holds: the bus's facts, and the buffer the
controller made for the channel, mapped through `MappedGrant` and refused if
shorter than the protocol's. A transaction names its messages by offset and
length into that buffer: write bytes are staged in it before, read bytes
found in it after. A bus link reaches every address of a bus wired to its
consumer alone, as a display port's DDC bus is.

An `I2cTarget` is a channel over a target link, the one address it names: the
`I2cPort` a register-addressed chip is driven through
([`tairix-i2c`](./i2c.md)). A transfer is the one write-then-read transaction
`I2cTransaction::write_then_read` shapes. A target refuses at open a bus that
cannot issue the repeated START between a register write and its read, so a
chip driver refuses the bus at bind with its reason rather than at its first
read. A target that does not answer its first message is `NotFound`; one that
answered and then refused, and any other fault, is the device's.

## Transport

`LinkCall` is the seam every client reaches its supplier through, and
`RtLinkCall` the production one over `ipc_call`, `call_post` and `call_reap`,
which keeps the clients host-tested against suppliers that decode every frame
they are sent. A blocking request is framed, sent and its reply decoded by one
path every client shares.
