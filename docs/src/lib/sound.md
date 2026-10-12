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
  ahead of a FLAC or MPEG audio stream is stepped over; nothing else may
  precede a file.
- `SoundInfo` states the stream: format, `Encoding`, rate, channel map, the
  `SampleFormat` blocks are written in, the frame count where the file says,
  whether it can be entered at any frame, and a `DataLength` where the header
  declares more sound than the file holds (the file wins, and the
  disagreement is reported rather than hidden).
- `next_block` writes whole frames and answers how many: as many as its
  buffer holds, unless the link ends first or a format read a packet or
  frame at a time (FLAC, Vorbis, Opus, MPEG audio) meets bytes not yet at hand once
  it has written some; none once the link has ended. `seek` enters a seekable
  link at any frame, and a stream with no seek structure says so
  (`SeekUnsupported`) rather than scanning.
- A file is read a **link** at a time. An Ogg file may chain streams one after
  another, each with its own codec, rate, channels and tags; `link` names the
  one being read (its place in the chain and the byte it begins at),
  `next_link` moves on once it has ended — its `SoundInfo`, `Metadata` and
  positions are then the next link's, from its first frame — and `open_link`
  opens a file at a link already met, which is how a replacement worker
  returns to one. A file that is no chain is its own one link.
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
largest admissible frame, the Vorbis packets the block's frames can take at
the most a packet can consume, an Opus seek's pre-roll and block in packets of
the mapping's largest, or the window an MPEG audio stream that lost its sync is
searched again in — which is what the worker sizes its page cache from. A seek reads a search's probes beyond that, a few kibibytes each.

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
  its subformat and its channel mask, which places any count it names; the `fact`, `cue `, `smpl` and `LIST INFO`
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
  no such range and are not reported. A comment's picture field
  (`METADATA_BLOCK_PICTURE`, and its legacy `COVERART` forms) is a base64
  payload, so it is stepped over rather than kept as a tag. Samples are written
  left-justified in the narrowest container that holds them (`U8`, `S16`,
  `S24`, `S32`), as WAV places them. A stream decoded whole and in order is
  checked against the MD5 its `STREAMINFO` states: a mismatch ends the stream
  in `FlacDigestMismatch` rather than passing the audio off as sound, and
  `PcmSource::verified` says afterwards whether the check ran and held — a
  stream stating an all-zero digest states none, so nothing proves it. A frame
  larger than twice its samples stored verbatim is refused: verbatim is
  always open to an encoder, so such a frame holds more unary padding than
  sound, and holding it would hold the decode to no bound. A stream naming no
  mask takes the specification's layout for its count, 6.1's back centre
  among them.

  The native stream seeks by bisecting its own frame headers, narrowed by the
  seek table where there is one (each point checked against the frame it
  names); a sync met inside a frame's data is caught by the frame's CRCs and
  searched past. Trailing `APEv2` and `ID3v1` tags end the frames.
- **Ogg** (RFC 3533), the container in full: pages and their CRC, packets
  assembled across pages, every page's place in its stream's sequence,
  logical streams multiplexed in one link, and links chained one after
  another. A link opens its streams together — each a page flagged first, of
  a serial of its own (`OggBadLink` where not; past 64, `OggTooManyStreams`)
  — and the first whose codec is read here is decoded (`OggNoCodec` when
  none is); the others' pages are stepped over. Each link's serials are its
  own, so where a link ends is found by bisecting the file on whether a page
  belongs to it, its last page checked first so an unchained file costs one
  probe; bytes trailing the last link's last page, such as an `ID3v1` tag,
  end the file rather than being taken for a link. A Vorbis or Opus stream's
  length is its last page's granule, sought back from the link's end within
  1 MiB; a stream ending further back states no length and still plays and
  seeks, and one whose ending page puts its end before its first sample is
  refused as it opens (`OggGranuleMismatch`). That page is the only one the
  stream may end on: an ending page
  before it, or the stream's opening page met again, shows a later link
  carrying the serial, which the format forbids (`OggBadLink`), and a stream
  whose pages run out short of it — another stream's damaged page stating a
  length that steps over them — has lost them (`OggPageLost`). Carried here:
  FLAC, by its mapping (RFC 9639, section 10.1), seeking by bisecting the
  pages on the frame header the first packet starting on each states; and
  Vorbis and Opus, below, seeking by bisecting the pages on their granules.
- **Vorbis I** (the Xiph.Org specification), in Ogg: the three headers;
  every codebook form — ordered and unordered lengths, sparse books, the
  errata's single-entry book, both vector lookups, sequence accumulation;
  floor 0 (line spectral pairs on the Bark scale, evaluated in the mixed
  precision libvorbis evaluates them in) and floor 1; residues 0, 1 and 2;
  any number of submaps and coupling steps; both block sizes, the windows
  chosen by the blocks' actual sizes as libvorbis chooses them, and the
  inverse MDCT over the shared transform. Written as `F32` in the order the
  specification lays out channels, 6.1's rear centre among them. The comment
  header's fields are tags.

  The mapping's granules place the stream: a first page short of the samples
  its packets return trims them from the start, one past them starts the
  stream later, and the last page trims its final packet's excess. Every other
  page's granule is held to the samples decoded before it, and a packet among
  the audio that is not one is refused (`VorbisBadPacket`); either would part
  the samples' positions from the stream's timeline, and a seek, which
  bisects the pages on their granules and decodes from a packet whose
  position its page fixes (that packet priming the overlap), must land where
  decoding from the start does (`OggGranuleMismatch` names the first). An
  empty packet carries nothing and is passed over. A file cut part way through
  a page plays to the cut and reports `OggPageTruncated`.

  A setup is held to 2^18 codebook entries and 2^19 vector values in all,
  and 256 KiB — many times what any encoder writes (libvorbis's 5.1 setup is
  11 KiB and twenty thousand entries) — so a hostile setup costs a bounded
  amount of memory (`VorbisSetupTooLarge`). An audio packet is held to the
  most its decode can consume, by its block size (`OggPacketTooLarge`). A
  packet whose samples overflow a float is refused rather than passed on
  (`VorbisPacketInvalid`). An error in the stream stays with it: decoding on
  from the same place meets it again, and only a seek leaves it behind.
- **Opus** (RFC 6716, with RFC 8251's corrections), in Ogg (RFC 7845): SILK,
  CELT and hybrid frames, every bandwidth and frame size from 2.5 to 60 ms in
  packets of up to 120 ms, mono and stereo, the switches between modes with
  and without a redundant CELT frame, and each layer's concealment for a frame
  a packet leaves empty, as discontinuous transmission does; SILK in the
  reference's fixed point, bit for bit, and CELT as libopus's float build
  computes it. Channel mapping families 0 and 1 — mono to 7.1, 6.1 included,
  each output channel taken from any stream's or silent — are read, their
  packets' streams self-delimited as appendix B frames them; other families
  are `ChannelLayoutUnsupported`. Written as `F32` at 48 kHz in Vorbis's
  channel order, the header's output gain applied. Redundancy for loss is
  stepped over, since a file loses no packets.

  The identification header stands alone on its page and the comment header
  ends its own (`OpusBadIdentification`, `OpusBadComment`); a version past
  0.x is `OpusUnsupportedVersion`. The pre-skip's samples are decoded and
  dropped. The first page's granule, less the samples its packets decode to,
  is where the stream starts, and one short of them is refused unless that
  page ends the stream; the last page's trims the end, and every other page's
  is held to the samples decoded before it (`OggGranuleMismatch` for each). A
  packet breaking RFC 6716's framing requirements (section 3.4), an empty one
  among them, or whose streams disagree on its duration is
  `OpusPacketInvalid`, as is a decode producing a sample no float holds; one
  past RFC 7845's 61,440 bytes is `OggPacketTooLarge`. No length, padding step or frame is read out of a
  packet's padding.

  A seek bisects the pages on their granules and decodes from the packet
  after the last that completes on a page at least 160 ms before the target,
  the decoder reset: twice the mapping's least pre-roll, since a seek into
  SILK can still be a sixteenth of full scale off after 80 ms. The seek lands
  on its frame exactly; the samples converge onto the decode from the start
  rather than repeat it, as a codec whose state no packet resets allows —
  within a hundredth of full scale over the first 20 ms and half that past
  100 ms on libopus's streams. A target close enough ahead is decoded on to,
  and that is exact. An error stays with the stream as Vorbis's does.
- **MPEG audio** (ISO/IEC 11172-3, ISO/IEC 13818-3, and MPEG-2.5 for 8 to
  12 kHz): Layers I, II and III, mono, stereo, dual channel and joint stereo
  — Layers I and II's intensity bound, Layer III's mid/side and intensity
  stereo at every rate — written as `F32`. A stream's first frame is believed
  only where it and the two after it agree in family, layer, rate and free
  format, each starting where the last ends, or fewer end the file exactly;
  a sync word with nothing agreeing after it is searched past, as are bytes
  between frames, and a stream found again that disagrees with the first is
  refused (`MpegFrameMismatch`). A free-format stream's frame length is
  measured from the next agreeing frame. A protected frame's CRC is checked
  (`MpegFrameCrc`). The first frame fixes the channels: a mono frame in a
  stereo stream sounds in both, and a stereo frame in a mono stream is
  refused rather than folded into one. A first frame carrying Xing's tag
  (`Xing`/`Info`) or Fraunhofer's (`VBRI`) in place of audio states the frame
  count, and a stream ending before it is refused (`MpegTruncated`); LAME's
  extension, where its own CRC vouches for it, states the encoder's delay and
  padding, and those samples and the decoder's own 529 are trimmed, so an
  album plays gapless. Trailing `APEv2` and `ID3v1` tags end the frames.

  Layer III reads its bit reservoir across frames; a frame whose main data
  reaches back past what the reservoir holds — met only entering a stream
  part-way — plays as silence rather than as bytes it does not have. A seek
  is exact: frame headers are walked from the nearest frame already passed
  (one is remembered every 32), and enough frames before the target are
  decoded and dropped that the target decodes as it would have from the
  start — two for Layers I and II, whose synthesis remembers sixteen slots;
  for Layer III, those whose spectra the target's filterbank depends on (one
  frame at MPEG-1, two at the lower rates, whose frames hold one granule) and
  as many before them as fill the reservoir.

## The shared transform

One mixed-radix complex FFT (radices 2, 3, 4 and 5, as KISS FFT decomposes
them) and the cosine transforms over it: the DCT-IV on a half-length FFT, the
MDCT pair over the DCT-IV, and the DCT-II on a half-length FFT of its real
input. Vorbis's inverse MDCT runs on it at every block size, and MPEG audio's
filterbanks moved onto it — Layer III's 36- and 12-point IMDCTs and the
synthesis's 32-point matrixing — each held to the direct form it replaced.
Every constant is worked in double precision and rounded once.

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

Every test input but FLAC's, Vorbis's and Opus's is synthesised in test code. FLAC
carries its own oracle: a stream states the digest of its samples, so a
stream another encoder wrote verifies itself. Vorbis has no exact output to
state, so its oracle is the reference decoder's own: `tests/vectors/` keeps
three streams libvorbis 1.3.7 encoded — stereo with its block switching, mono
at 8 kHz, and 5.1 — each beside libvorbis's decode of it, quantised to 16
bits and stored losslessly as FLAC, and each decodes within one 16-bit step of
it. libvorbis's encoder writes floor 1 and residues 1 and 2 only, so a fourth
reference is libvorbis's decode of a stream written in the tests — floor 0 at
both orders, residues 0 and 1, a listing lookup and a single-entry book over
two submaps — which decodes within the same step. Beside them, streams written
from chosen values in every construct, each packet's floors, residues,
coupling and blocks, are checked against the specification's formulas
evaluated directly in double precision — the inverse MDCT by its definition,
the windows by their slopes, the overlap summed at each block's absolute
place — and against every header refusal; and Ogg streams written in the
tests cover the granules' trimming at both ends, chains of links of different
codecs and shapes, multiplexing, a stream ending far before its link, packets
crossing pages, exact seeks, a cut file and a timeline at odds with its
samples. Opus's oracle is libopus 1.4: `tests/vectors/` keeps nine streams it
encoded from signals synthesised for them, every packet's settings forced so
that between them they reach each mode, bandwidth and frame size, mono and
stereo packets, the switches between modes with and without a redundant frame,
redundancy for loss, discontinuous transmission, constant bitrate's padding,
packets of many frames, 5.1, 6.1 and 7.1. Each decodes within one 16-bit step
of libopus's float decode of it, stored losslessly beside it, and ends every
packet in the range coder's state the encoder recorded, which proves the parse
bit for bit. Ogg Opus streams written in the tests cover the pre-skip, the
granules at both ends and their refusals, the headers' placement, packets out
of form, the output gain, chains, multiplexing, links sharing a serial, pages
lost behind another stream's damaged page, a cut file and seeks; the packet
parser is held to RFC 6716's framing requirements one by one. Conformance was
established against RFC 8251's test vectors, which the tree does not carry:
every packet of all twelve ends in the encoder's range state, the stereo
decode is within one 16-bit step of each reference (three of them exactly),
and both channel counts are within 2e-7 of libopus 1.4's float decoder. RFC
9639's three example streams (Appendix D,
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
`fuzz_sound` harness drives arbitrary bytes and structure-aware AU, WAV,
MPEG audio, FLAC, Vorbis and Opus generators — FLAC through the encoder's
core, every construct drawn at random, native and in Ogg; Vorbis and Opus from
libvorbis's and libopus's streams, mostly under serials of their own, chained,
interleaved, Opus links among Vorbis ones, a page dropped, and corrupted
beneath resealed page CRCs so the damage reaches the codec — asserting, for
every link of a chain, that nothing panics, no stream writes past the frames
it states, peak memory stays within the block and limits, the kept metadata
is within the limits, a decode through an input that answers "not yet" at
random equals a direct one, a block entered by a seek equals the decode from
the start (for Opus, lands where it does), and an unmutated FLAC stream
decodes to exactly the samples it was written from.

MPEG audio's pieces are each checked against the standard's own formulas
evaluated directly in double precision: the synthesis filterbank, Layer III's
hybrid filterbank over every block shape, its requantisation, stereo
processing and scalefactor partitions, and Layers I and II's requantisation
against the standard's two-step form; every Huffman table is checked to be a
complete prefix code. End to end, streams written in the tests — Layer I, and
Layer III leaning on its reservoir — exercise sync and junk, the tags, the
CRC, free format, a change of mode, the encoder's trim, truncation, exact
seeks and an input answering "not yet". Conformance was established against
the ISO/IEC 11172-4 compliance streams, which the tree does not carry: each
decodes within one 16-bit step of its reference output, inside the standard's
accuracy limits.
