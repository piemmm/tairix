# tairix-encoder

The `encoder-v1` driver side (`plans/SOUND.md` SND19l): what every display
encoder driver runs around its `tairix_abi::driver::encoder::Encoder`.
Stability tier: `experimental`.

- `EncoderServer` serves one encoder's endpoint. A request is believed only
  once the kernel attests the caller holds the encoder link it quotes. Any
  holder may read the connector; the mode set's steps and the connector wait
  are the holder's, the first engine to ask, until that instance ends — the
  kernel's refusal to watch a holder says it has, so a restarted engine takes
  over at once. The phase it leaves stands: the next engine finds the output
  as it was and brings it down in order.
- The steps are held to a mode set's order from the phase the hardware was
  found in, so an engine taking over a firmware's running pipe must disable
  and power it down before it sets a mode. A configure for a mode that is not
  the connector's is `Stale`.
- A posted wait naming the moves its holder has seen is answered at once when
  the count differs — the connector moved since, or the encoder restarted —
  and otherwise when it next moves.
- `serve` binds the endpoint under the node's encoder duty and parks on it,
  on peer exits and on the encoder's own event sources, ending the driver
  with its reason when it cannot. `serve_with_audio` does the same for an
  encoder whose connector carries audio, with the audio device channel's
  endpoint and event sources in the one wait set.

The protocol's rules know no device, so every encoder driver serves them from
here rather than writing the attestation, holder and phase rules again.
