# tairix-codec

The `codec-v1` driver side (`plans/SOUND.md` SND8f): what every audio codec
driver runs around its `tairix_abi::driver::codec::Codec`. Stability tier:
`experimental`.

- `CodecServer` serves one codec's endpoint. A request is believed only once
  the kernel attests the caller holds the codec link it quotes, and the
  framing and clock sides come from that attested link, never from the frame.
  Anyone holding a link may ask what the codec accepts; configuring, gain,
  start and stop are the holder's, the first interface driver to configure
  it, until that instance ends — the kernel's refusal to watch a holder says
  it has, so a restarted driver takes over at once — when the codec is
  stopped.
- `serve` binds the endpoint under the node's codec duty and parks on it and
  on peer exits, ending the driver with the audio class's shared exit code
  and reason when it cannot.

Two drivers link it, `drivers/audio/pcm5102a` and `drivers/audio/pcm5122`,
so the attestation and the holder rule are written once.
