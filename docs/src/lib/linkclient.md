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
its answer beside its other sources (`Wake::CallReply` in
[`tairix-audiochan`](audiochan.md)), and `reap_wait` collects it when the loop
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

## Transport

`LinkCall` is the seam every client reaches its supplier through, and
`RtLinkCall` the production one over `ipc_call`, `call_post` and `call_reap`,
which keeps the clients host-tested against suppliers that decode every frame
they are sent. A blocking request is framed, sent and its reply decoded by one
path all three share.
