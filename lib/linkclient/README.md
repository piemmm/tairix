# tairix-linkclient

The consumer halves of the supplier links (`plans/SUPPLIERS.md`): what a
driver whose node names another driver's service speaks to it. Stability
tier: `experimental`.

A supplier believes a link request only once the kernel attests the caller
holds it, so a client quotes the request discovery gave its node and nothing
of its own choosing.

- `DmaClient` (`dmaengine-v1`) opens a channel on the node's DMA request line,
  has the controller carve the cyclic buffer — mapped by the caller with
  `tairix_rt::shm::MappedGrant` — and runs the channel. A period boundary is
  the answer to a wait it posts with a deadline, which the driver's serve loop
  wakes on (`tairix_audiochan::serve::Wake::CallReply` for an audio driver)
  and `reap_wait` collects. One wait is outstanding at a time, and one the
  transport refuses is spent.
- `ClockClient` (`clock-v1`) describes, runs and releases the clock a block
  runs from. A clock another process holds runs at the rate it already has.
- `CodecClient` (`codec-v1`) describes, configures, gains, starts and stops
  the codec on the far side of a digital audio interface, reading the
  framing and clock sides from the link's selector. Its refusals are decoded
  by `codec-v1`'s own `refusal`, so a codec with no gain is told apart from
  one that failed.

`LinkCall` is the seam a client reaches its supplier through; `RtLinkCall` is
the production one, over `ipc_call` and the posted `call_post` and `call_reap`
pair, and every blocking request is framed, sent and decoded by one shared
path. The PWM and I²S drivers link this crate rather than re-deriving the
frames, so they cannot drift on them. Everything a supplier answers is decoded
by the protocol's own fail-closed decoders before a caller sees it.
