# `tairix-sound` — sound files

`lib/sound` turns an untrusted sound file into interleaved PCM a block at a
time, or a typed `DecodeError` — never a panic, and never more memory than the
caller's limits allow. Players reach it only through the parser sandbox
([`tairix-sandbox`](./sandbox.md)'s `audiodecode`), so no player parses a byte
of a sound file in its own process. Moving and mixing samples is
[`tairix-audio`](./audio.md)'s job; this crate knows files and nothing of
devices.

Stability tier: **experimental**.

## Shape

- `sniff` names the format a file's first `SNIFF_LEN` bytes open with;
  `PcmSource::open` dispatches on it, and `open_as` reads a named format,
  whose own reader still checks the bytes. An `ID3v2` tag other software put
  ahead of a FLAC stream is stepped over; nothing else may precede a file.
- `SoundInfo` states the stream: format, `Encoding`, rate, channel map, the
  `SampleFormat` blocks are written in, the frame count where the file says,
  whether it can be entered at any frame, and a `DataLength` where the header
  declares more sound than the file holds (the file wins, and the
  disagreement is reported rather than hidden).
- `next_block` writes whole frames and answers how many: as many as its
  buffer holds, unless the stream ends first or a format read a frame at a
  time (FLAC) meets bytes not yet at hand once it has written some; none once
  the stream has ended. `seek` enters a seekable stream at any frame, and a
  stream with no seek structure says so (`SeekUnsupported`) rather than
  scanning.
- `probe` reads the header alone and keeps no metadata.

### Streaming

A decoder reads its file through `SoundInput` at offsets of its own choosing,
never more than `MAX_READ` (64 KiB) at once, and holds at most one block's
bytes. An input whose bytes are not at hand answers `InputError::Unavailable`,
and **the call that met it changes nothing the caller can see**: position,
metadata and output are as they were, so the caller supplies the bytes and
asks again. The sandbox worker decodes a file it never holds whole this way.
`max_working_set(limits, block_frames)` states the most of the file one
`next_block` call reads for any stream within the limits — a FLAC stream's
largest admissible frame, at most — which is what the worker sizes its page
cache from. A seek reads a search's probes beyond that, a few kibibytes each.

### Bounds

`DecodeLimits` bounds what a file can make a decoder keep:

- **channels**, refused past the limit (`ChannelsExceedLimit`);
- **metadata bytes**: a kept tag costs its text plus a fixed 80-byte charge
  for its entry (its kind, key and text's handle), so a file of many tiny tags
  is held to the budget by what keeping them costs, not by their text alone;
- **markers**: cues and sampler loops between them.

Past the metadata limits a tag or marker is left out and `Metadata::omitted`
is set. `Metadata::within` applies the same rule to metadata decoded
elsewhere, which is how the sandbox's client checks a worker's answer. Every
size a file declares is weighed against the bytes it holds before it is used,
and all arithmetic over a declared value is checked.

## Formats

A format claimed is claimed completely; a variant that would be half-read is
refused by name.

- **AU** (Sun/NeXT `.au`, `.snd`): μ-law and A-law; linear 8, 16, 24 and 32
  bits; IEEE float 32 and 64 (written as `F32`); fixed point 8, 16, 24 and 32
  bits; ITU-T G.721, G.722, and G.723 at 24 and 40 kbit/s. Header-declared and
  unknown-length streams; the annotation is kept as a comment tag. Refused by
  name: fragmented sample data, nested sounds, DSP programs and music-kit DSP
  commands, display data (none of them samples); the squelched, emphasised and
  compressed variants, whose processing the format never specifies; ADPCM in
  more than one channel, whose interleave it never specifies; G.722 at any
  rate but 16 kHz. An ADPCM stream cannot be entered except from its start.
- **WAV** (RIFF, RF64, BW64): PCM 8-bit unsigned and 16, 24 and 32-bit
  signed, at any valid width a container states; IEEE float 32 and 64; A-law
  and μ-law; Microsoft ADPCM and IMA/DVI ADPCM; `WAVE_FORMAT_EXTENSIBLE` with
  its channel mask and subformat; the `fact`, `cue `, `smpl` and `LIST INFO`
  chunks; chunks in any order and odd-length padding; and a `data` chunk
  longer than the file. Refused by name: MPEG audio and GSM 6.10 inside WAV,
  each another codec's decoder's job.
- **FLAC** (RFC 9639), native and in Ogg: constant, verbatim, fixed (orders
  0–4) and linear-predictor (orders 1–32) subframes; Rice partitioning at
  both parameter widths, the escape partition, and wasted bits; all four
  stereo decorrelations; every block size and sample width the format allows,
  4 to 32 bits, a 32-bit stream's 33-bit side channel included; fixed and
  variable blocking; every frame-header code; both CRCs checked on every
  frame, and each frame's place in the sequence. The `STREAMINFO`, seek
  table, Vorbis comment (its fields as tags, `WAVEFORMATEXTENSIBLE_CHANNEL_MASK`
  as the channel layout), cuesheet (a cue a track at its index 1), picture and
  application blocks are read and checked; padding and reserved types are
  stepped over, and the forbidden type 127 is refused. A native stream's
  picture is reported as `Metadata::cover`, the byte range its image lies at
  in the file — the front cover when there is one, else the first picture —
  so a player reads and decodes it itself; a link (MIME `-->`) or an empty
  image is no cover, and an Ogg stream's pictures, split across pages, have
  no such range and are not reported. Samples are written
  left-justified in the narrowest container that holds them (`U8`, `S16`,
  `S24`, `S32`), as WAV places them. A stream decoded whole and in order is
  checked against the MD5 its `STREAMINFO` states: a mismatch ends the stream
  in `FlacDigestMismatch` rather than passing the audio off as sound. A frame
  larger than twice its samples stored verbatim is refused: verbatim is
  always open to an encoder, so such a frame holds more unary padding than
  sound, and holding it would hold the decode to no bound. Seven channels
  place a back centre the PCM vocabulary has no position for, and are refused
  as `ChannelLayoutUnsupported`.

  The native stream seeks by bisecting its own frame headers, narrowed by the
  seek table where there is one (each point checked against the frame it
  names); a sync met inside a frame's data is caught by the frame's CRCs and
  searched past. Trailing `APEv2` and `ID3v1` tags end the frames.
- **Ogg** (RFC 3533): pages and their CRC, packets assembled across pages,
  every page's place in its stream's sequence, and a physical stream that
  interleaves several logical ones, read past the others' pages by serial.
  Carried here: FLAC, by its mapping (RFC 9639, section 10.1). Seeking
  bisects the pages on the frame header the first packet starting on each
  states, so a granule position is never trusted to place a sample. A stream
  chained after the FLAC one is refused where the FLAC one ends, as
  `OggChained`, rather than quietly ending the sound there; reading on into
  the next link is part of the full container, staged with Vorbis.

## The FLAC encoder

Behind the off-by-default `encode` feature, so no binary that runs on the
machine carries it. `flac_encode::Writer` is a core that emits exactly the
constructs it is given — a frame's stereo coding, each subframe's predictor,
wasted bits and residual partitioning, the header's codes — so tests and the
fuzz generator can make constructs a chooser would never pick;
`flac_encode::choose` and `flac_encode::encode` are the chooser over it:
exact Rice-cost partitioning, every fixed predictor, Levinson-Durbin linear
predictors over a Tukey-windowed autocorrelation quantised as the reference
encoder quantises them, and the cheapest stereo coding. `finish` writes the
native stream, `finish_ogg` the Ogg mapping. Both halves share one format
model — the predictors, the residual fold, the header code tables, the CRCs
and the digest — so they cannot drift apart by construction.

## Testing

Every test input but FLAC's is synthesised in test code. FLAC carries its
own oracle: a stream states the digest of its samples, so a stream another
encoder wrote verifies itself. RFC 9639's three example streams (Appendix D,
written by the reference encoder) are kept as test vectors and decode to the
samples the RFC lists and their stated digests. The encoder round trip is the
breadth check — every width, channel count, subframe kind, partition shape,
header code and stereo coding, seeking with and without a seek table, and a
decode through an input that answers "not yet" — and proves only that the two
halves agree, which a shared misreading of the format would satisfy too.

The ADPCM codecs are checked
against oracles outside the tree: the G.72x decoders bit-exactly against
Sun's reference implementation and G.722 against the public-domain reference
decoder, both over fixed vectors; IMA against Python's `audioop`; and
Microsoft ADPCM against an encoder written in the test from the format's own
equations, whose reconstruction the decode must equal. `tests/allocation.rs`
holds reading a file's tags to one allocation each. The registered
`fuzz_sound` harness drives arbitrary bytes and structure-aware AU, WAV and
FLAC generators — the last through the encoder's core, every construct drawn
at random, native and in Ogg — asserting that nothing panics, no stream writes
past the frames it states, peak memory stays within the block and limits, the
kept metadata is within the limits, a decode through an input that answers
"not yet" at random equals a direct one, and an unmutated FLAC stream decodes
to exactly the samples it was written from.
