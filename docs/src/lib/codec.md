# `tairix-codec`

The driver side of `codec-v1`, the seam a digital audio interface's driver
speaks to the codec its link names ([audio drivers](../drivers/audio.md)).

A codec is reached only through the link its interface's node holds, so the
server believes a request once the kernel attests the caller holds the quoted
codec link, and reads the framing, which side drives the bit and frame
clocks and which runs inverted from that attested link rather than from the
request. Any holder of a link may ask what the codec accepts. Configuring,
gain, start and stop belong to the holder: the first interface driver to
configure the codec, until that instance ends, when the codec is stopped. A holder whose end has not yet
reached the server is found out by the kernel's refusal to watch it, so a
restarted driver takes the codec over at once rather than being refused.

A refusal from the codec's driver travels as `codec-v1`'s `refusal_reason`
states — a framing it does not take as `NotSupported`, a held codec as
`Busy`, a missing gain as `NotImplemented` — and the interface's client reads
it back with `refusal` as the driver's own error.

`serve` binds the codec node's endpoint under its codec duty and parks on it
and on peer exits; a codec driver's binary is its bring-up and one call to it.
