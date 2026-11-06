# tairix-video

Stability tier: **experimental**.

First-party TAIRiX video: the VP8 codec (RFC 6386) — keyframes, which a lossy
WebP picture is one of, and inter frames — and the WebM container a screen
recording is kept in. Every decoder is `no_std` + `alloc`, `unsafe`-free and
fail-closed: untrusted bytes become frames or a typed refusal, never a panic,
and a stream's declared geometry states what decoding it holds before anything
is reserved, so a caller bounds a decode it has not begun.

## Modules

- `vp8`: the keyframe decoder, row by row; the stream decoder, which decodes
  keyframes and inter frames against the reference frames it holds; an inter
  frame's header and macroblock syntax; inter prediction from a reference
  frame's planes; the encoder behind the `encode` feature, which reconstructs
  each frame with the decoder's own code; the boolean encoder and syntax
  writers it and the tests write with; and the bitstream fixture behind the
  `fixture` feature.
- `webm`: the muxer a recording is written with as it is made, and the
  demuxer an untrusted file is read through a frame at a time.

## Consumers

- `lib/image` decodes a lossy WebP picture through `vp8`.
- The desktop's recorder encodes a recording through `vp8::Encoder`, with the
  `encode` feature, and the viewer plays one back, through `vp8` and the WebM
  container (`plans/CAPTURE.md`).
