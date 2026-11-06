# `tairix-video`

First-party video for untrusted recordings and the desktop's own: the VP8
codec (RFC 6386) and the WebM container a screen recording is kept in
(`plans/CAPTURE.md`). `no_std` + `alloc`, `unsafe`-free, and fail-closed:
untrusted bytes become rows or a typed refusal, never a panic.

## VP8 keyframes

`vp8::decode_rows` decodes a keyframe and hands its opaque RGBA8 rows to a
callback, top first; `vp8::probe` reads the geometry a keyframe declares
without decoding it, and `vp8::frame_peak_bytes` bounds what decoding one
of a given width holds, whatever its height. The decoder takes no limits:
a caller bounds a decode by checking the probed geometry before it begins,
as `lib/image`'s WebP container does. A refusal is a `vp8::Vp8Error`, which
a caller converts into its own error through `From`.

The keyframe decode is complete: the boolean entropy decoder; the
segmentation, loop-filter and quantiser headers with their per-segment and
per-mode deltas; the token-probability updates; the four whole-macroblock
luma modes, the four chroma modes, and all ten subblock modes; the
Walsh-Hadamard and DCT reconstructions; both the normal and the simple loop
filter at macroblock and subblock edges; and the conversion to RGB. A frame
is decoded a macroblock row at a time: each row predicts from the unfiltered
bottom row of the one above, kept aside before the filter moves it, and is
filtered once the row below it is reconstructed, so the decode holds a
window of rows rather than the frame. A reserved colour space is refused
rather than guessed; the clamping-type bit and the two scale fields are read
and not acted on.

## VP8 inter frames

A frame's header is read against what a stream carries from frame to frame:
the token, intra-mode and motion-vector probabilities, the segment features and
the loop-filter deltas. A keyframe resets them to the format's defaults, and a
header replaces what it updates only once it is read whole, so a refused header
leaves them as they were. A frame that does not refresh its probabilities
decodes with its updates and leaves the carried ones untouched; segment
features and deltas persist regardless, and a segment's own filter level is
held to range before the deltas adjust it. An inter frame's header also says
what the golden and alternate buffers take — this frame, a copy of another, or
what they held — which way vectors into them point, and whether the frame
becomes the last; the copy the format reserves is refused.

An inter frame's macroblock modes and motion vectors are read a row at a time
(RFC 6386 §16, §17). A macroblock keeps its segment where the frame leaves the
map be. An intra one reads its modes with the frame's probabilities and none of
a keyframe's context. An inter one names its reference frame, then weighs the
vectors of the macroblocks above, to the left and above-left — two, two and
one, reversing one whose reference points the other way — for the nearest,
near and best vectors and the weights that pick its mode's probabilities. Its
motion is the nearest or near vector, none, a new vector coded against the
best, or a split into halves, quarters or subblocks whose parts each take their
left or above neighbour's vector, none, or a new one. A vector a neighbour
lends is held to a macroblock past the frame's edges before it is used.

## VP8 inter prediction

An inter-coded macroblock is predicted from the planes of a reference frame
through its motion vectors (RFC 6386 §18). A vector is held in eighth-pel
units of the plane it moves: a luma vector is the bitstream's quarter-pel
one doubled, as the format stores it. A chroma block moves by the average of
the four luma subblock vectors it covers, halved and rounded half away from
zero, so a whole macroblock's chroma vector is its luma vector halved.
Version 0 interpolates a fraction with the six-tap filters, and versions 1
and 2 with the bilinear ones, across and then down, rounding and clamping to
pixel range after each pass; version 3 interpolates luma as versions 1 and 2
do and drops chroma's fractions, as the reference decoder does. A reference
is the frame as decoded, whole macroblocks included, and reads past its edges
as its nearest edge sample however far a vector reaches: a block whose
filters stay inside the reference reads it in place, and only one crossing
an edge has its samples gathered first. Prediction allocates nothing.

## VP8 stream decoding

`vp8::Decoder` decodes a stream a frame at a time. Its first frame must be a
keyframe, which sets the stream's size; a later keyframe may change it, and
an inter frame before any keyframe is refused. Each decode answers a
`vp8::Decoded`, which says whether the stream shows the frame and lends its
picture until the next decode: `rgba_rows` converts a range of its rows to
opaque RGBA8 straight into the caller's buffer at the caller's stride, by
the conversion the keyframe decoder uses.

The pictures are held in a pool of four, and the last, golden and alternate
references each name one by its slot, so a refresh or a copy moves a slot
and never a plane. Once a frame has decoded whole, the alternate takes its
copy, then the golden — reading the alternate as just copied — then each
reference the frame refreshes takes the frame, and the last does where the
frame says: the reference decoder's order. A keyframe refreshes all three.
A reference holds its frame as reconstructed and loop-filtered, whole
macroblocks included.

An inter frame's intra macroblocks predict from the frame being decoded,
with the keyframe predictors and the edges they invent; its inter
macroblocks predict from the reference each names. The frame is
reconstructed a macroblock row at a time against the unfiltered rows above
it, and the row above is filtered once it is, which is the loop filter
acting on the frame as a whole. A macroblock's filter level is its
segment's, then the reference's and the mode's deltas — subblocks, no
motion, one vector, split motion — held to 0–63; an intra macroblock
predicted whole takes no mode delta, and one that coded nothing skips its
inner edges unless it is predicted a subblock at a time. An inter frame
counts fewer edges as sharp features to keep than a keyframe does. A
block's token context, and whether a macroblock coded anything, is whether
its tokens went past their first position, as the reference decoder has it.

Decoding is transactional. A frame decodes into a picture no reference
holds, and its references, segment map and carried probabilities become the
stream's only once it has decoded whole: a truncated or malformed frame, a
reserved value or a refused allocation leaves the stream as it was, and the
next frame decodes as though the refused one never came.
`vp8::stream_peak_bytes` is every byte a decoder of a size holds at its
peak — the four pictures, the contexts of two macroblock rows, the two
segment maps and a frame's partition readers — and a keyframe that changes
the size holds no more than the new size's beyond what the old stream held,
until it decodes whole. The decoder takes no limits: a caller bounds a
stream by probing each keyframe's size before decoding it. `fuzz_vp8`
decodes streams of arbitrary, drawn and mutated frames, refusing their
memory at random, beside a twin stream given only the frames the first
accepted, and holds each decode to its stream's forecast.

## VP8 encoding

Behind the `encode` feature, `vp8::Encoder` codes a stream of opaque RGBA8
pictures of one size, as the screen recorder makes them. `Encoder::new`
refuses a size a keyframe cannot declare and takes the most cores, its
*lanes*, a frame is coded on; `encode` takes a picture at any stride, the
rectangles that changed since the last one, whether to code a keyframe and the
`tairix_parallel::JobRunner` to code it across, and leaves one `VP8 ` frame in
the caller's buffer — the same bytes on any runner and any number of lanes.
The first frame is a keyframe whatever is asked, and a rectangle reaching past
the picture is held to it.

The picture becomes luma and 4:2:0 chroma by the exact inverse of the
decoder's BT.601 conversion, its multipliers derived from the decoder's own.
Each macroblock takes the prediction whose reconstruction error and bits,
weighed at a worth the quantiser step sets, cost least: in a keyframe, one of
the four whole-macroblock modes, or the ten subblock modes where they cost
less; in an inter frame, the last frame through no motion, a whole-pixel
vector searched in steps of four, two and one pixels from no motion and the
vectors its neighbours lend, or one of the whole-macroblock intra modes where
it could cost less. An inter frame never weighs the subblock modes: they cost
it most of its time and save it little, which a recording coded as the screen
changes cannot afford. A search ends at a vector that matches exactly, and a
motion that leaves no residual is taken without weighing intra prediction.
The golden and alternate frames are neither refreshed nor read. A macroblock
no changed rectangle touches is not examined: it is skipped with no motion,
and an inter frame's mode delta for no motion takes any loop-filter level to
0, so static content is copied exactly and never smears; a row of nothing
else is copied from the last frame whole, examining nothing. Every other
macroblock filters at half the quantiser index.

`Quality` picks the quantiser index: `Compact` 48, `Standard` 28 and `Fine`
12, whose AC steps of 52, 32 and 16 keep text legible, keep text and flat
colour clean, and keep anti-aliased gradations.

A frame is coded in two passes. The first decides, quantises and reconstructs
each macroblock, pricing each candidate's tokens from a table of what every
token costs under the default probabilities, and counts the token branches
the frame takes; the second
writes the header — each token probability replaced where the counts make the
update cheaper, and the skip and intra odds as counted — then the modes and
the tokens, through the boolean encoder the test fixture writes with too.
Reconstruction runs the decoder's own predictors, inverse transforms and loop
filter, its filter in an order equivalent to the decoder's, so the encoder's
reference is, to the bit, the picture every decoder of the stream holds; the
tests decode every frame and compare every plane's picture. A first partition
longer than a frame's tag can name is coded again without subblock
prediction, then with the cheapest modes, which fit any picture the format
carries.

The first pass is a wavefront over `tairix_parallel::for_each_ordered`: each
lane codes a macroblock row at a time in a band of its own — the row's sixteen
luma rows under the row above's bottom ones — two macroblocks behind the row
above, and filters it a macroblock behind itself. A row publishes, through a
`tairix_parallel::Published` slot per macroblock, its unfiltered bottom samples
with its decisions and contexts, which the macroblocks below predict from,
and then its filtered bottom samples, which their top edges read and move; a
row waits on nothing else, so no row's work depends on which lane runs it. The
lanes' token counts are summed and the frame is written settled row by row
into the reference. A frame coding at least 1024 macroblocks over eight or
more rows writes its tokens as eight partitions, each at once beside the
first partition, into room made for it with the encoder: an eighth of the
picture's raw size, which a partition outgrowing it is written past anew in
the frame itself. On one core a frame costs what it did before the passes were
spread — at 1080p a keyframe about 165 ms and a whole-screen change about
70 ms — and on four lanes about 60 ms and 24 ms, on eight about 35 ms and 13 ms.

`Encoder::peak_bytes(width, height, lanes)` is every byte an encoder holds: its
two pictures, each macroblock's decisions, quantised residual, contexts and
published edges, each lane's band and contexts, the token partitions' room
and the frame's branch counts, all made with it. A frame allocates nothing
but growth of the caller's buffer, and nothing per macroblock; an encode
refused that growth answers `Vp8Error::OutOfMemory` and leaves the encoder as
it was, so the next frame is coded against the same reference.

## WebM

`webm::Muxer` writes a recording as it is made, through positional writes
(`tairix_fscopy::Target`) to a file it never reads back. The Segment and each
Cluster open with an unknown size that closing them patches, so the file of a
writer that died plays as far as it got. The video and audio frames are
interleaved by timestamp; a Cluster opens at every video keyframe and wherever
a block's offset from its Cluster's timestamp would pass a signed 16-bit
tick. The seek index is written as the recording finishes into space reserved
at the start, so finishing needs no new space and a recording a full disk
stopped still gets one; an index that outgrows the reservation follows the
last Cluster, and one that fits nowhere else is thinned to every n-th
keyframe. A full disk ends the file at its last block written whole.

`webm::Demuxer` reads an untrusted file a frame at a time through
`tairix_sound::SoundInput`, the random-access input the sandbox's page cache
serves, answering each frame's track, timestamp, keyframe flag and where its
payload lies, and seeks to the latest keyframe at or before a target. It reads
element headers and the few small elements it interprets, never a payload.
Every element ID and size is held to EBML's rules and to the element holding
it; only the Segment and a Cluster may leave their size unknown; nothing is
recursed into. A frame holds at most `MAX_FRAME_BYTES`, a file declares at
most `MAX_TRACKS` tracks, and an index keeps at most `MAX_INDEX_POINTS`. A
call meeting bytes the input does not hold yet answers
`DemuxError::InputUnavailable` and, asked again once they are there, answers
as it would have. `fuzz_webm` drives the demuxer over arbitrary bytes and
mutated recordings the muxer wrote.

## Test fixtures

The `fixture` feature exposes `vp8::fixture`, the bitstreams the VP8 tests
are written from, written with the encoder's own boolean encoder and syntax
writers — the format specification's encoder rather than the decoder's
inverse — so the tests of a container that carries VP8 draw their frames
from the same writer. `inter_frame` writes an
inter frame from each macroblock's prediction, vector and residual, coding
every vector against those its neighbours lend through the writer the
decoder keeps beside its macroblock reader, and `drawn_inter_frame` one
whose macroblocks are drawn bits, as `drawn_keyframe`'s are. It is test
scaffolding, enabled only from a `[dev-dependencies]` entry.
