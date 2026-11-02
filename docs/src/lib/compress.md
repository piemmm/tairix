# `tairix-compress`

The one home for lossless compression in TAIRiX. Its codecs do not compete:
the first-party `RLZ1` codec is what TAIRiX's own storage uses, and the
foreign codecs — RFC 1951 / RFC 1950 DEFLATE and zlib, bzip2, and the ARC
family's methods — are what other software already speaks.

The crate depends on nothing — not even `alloc` — and allocates nothing. Its
consumers reach from the kernel heap's compressed-page store down through a
filesystem driver to a desktop image decoder, so a heap requirement would be
one imposed on all of them by the least constrained.

## Why more than one codec

`RLZ1` exists because `AGENTS.md` §2.12 bars an external `zstd`/`lz4`
dependency and ARXFS needs a low-CPU codec on its record path
(`docs/src/filesystem/arxfs-spec.md` §10). It is a single greedy LZ77 pass
over a small hash table, with no entropy stage: fast and predictable rather
than dense.

DEFLATE exists because two foreign contracts are written in it and neither
is negotiable. A PNG's `IDAT` stream is zlib-wrapped DEFLATE, so `lib/image`
must *read* it. SSH's `zlib@openssh.com` compresses a session's traffic as
one zlib stream per direction, so `sshd` and `ssh` must *write* one a stock
OpenSSH peer reads (`plans/SSH.md`). Neither codec is a fallback for the
other: a TAIRiX-native format never stores DEFLATE, and a foreign format is
never handed `RLZ1`.

bzip2 and the ARC methods exist for the same reason: archives were written
in them long before TAIRiX. The archive driver (`docs/src/filesystem/archive.md`)
reads `.tar.bz2` and `.bz2` through bzip2, and ARC, Spark and ArcFS members
through the ARC methods. It writes both formats back, because changing an
archive means compressing what changed the way the archive's own tools
would.

## The two shapes, and why both exist

| Shape | Entry point | For |
|---|---|---|
| Whole stream | `compress` / `decompress`, `inflate::inflate_into`, `inflate::inflate_prefix`, `zlib::decompress_into` | data that is present in one buffer: a filesystem record, a PNG `IDAT`, the header of a compressed archive member |
| Streaming | `deflate::Deflate`, `inflate::Inflater`, `zlib::Encoder`, `zlib::Decoder`, `bzip2::Decoder`, `bzip2::Encoder`, `arc::Decoder`, `arc::Encoder` | a conversation, one stream per direction flushed per message; or an archive member read from, or written to, the middle of a file larger than memory |

A one-shot function cannot express what a protocol needs. `zlib@openssh.com`
keeps a single zlib stream alive for the whole session and flushes it at each
packet boundary, so packet ten still back-references packet three. Restarting
the codec per packet would lose most of the ratio the feature exists for; and
on the receiving side, a fragment of a continuous stream is not a stream at
all — it has no final block and its back-references point behind its own
first byte.

So the streaming shape is a value the caller owns:

```rust,ignore
let mut encoder = Box::new(zlib::Encoder::new());
let mut wire = vec![0u8; encoder.bound(payload.len())];
let written = encoder.compress(payload, &mut wire, zlib::Flush::Sync)?;
```

`Flush::Sync` ends the current block and appends an empty stored block, so
the peer can decode every byte fed so far. `Flush::Finish` ends the stream.
`Flush::None` buffers, which is what bulk data wants.

The bzip2 and ARC codecs stream the same way, but through the crate-root
`Input` and `Output` traits rather than slices. A bzip2 decoder must hold a
whole block before it can emit the block's first byte, so reading takes a
byte at a time from the caller's source rather than a slice the caller
re-presents.

## Seeking into a long stream

A stream with no index can only be read from its start, which turns a read
at the end of a 10 GB `.tar.gz` into a 10 GB decode. Both block formats can
resume a stream partway through:

- `inflate::Inflater::inflate_by_block` stops at each DEFLATE block, where
  `checkpoint` records an `inflate::Checkpoint`: the bits held and the
  `WINDOW_SIZE` of history a later block may reach back into. A decoder
  `resume`d from one decodes the rest exactly as the original would.
  `Checkpoint::after_sync` builds one without decoding, for the
  byte-aligned block an encoder begins after `Flush::Sync`, so a writer
  indexes its own output as it writes it.
- A bzip2 block depends on nothing before it. `bzip2::Decoder::next_block`
  reports the bit each block began at, and `resume` starts at such a bit.
  `bzip2::Encoder` reports each block's start through `BlockIndex`.

Choosing where to keep checkpoints, and how many, is the caller's
business. The archive driver's policy is in its own page.

## Memory, and where it goes

A decoder carries the 32 KiB of history a back-reference may reach into
(~34 KiB all told). An encoder additionally carries the match finder: a
64 KiB buffer the 32 KiB window slides inside, the hash head and chain
tables, and one block's tokens — around 220 KiB.

The 32 KiB window is the format's own, fixed by RFC 1951 and by what a peer
expects (`AGENTS.md` §24.4). The hash and token table sizes are tuning, and
are documented on their constants.

That memory is deliberately *not* hidden. There is no one-shot encode
function, because one would put 220 KiB on a caller's stack without saying
so; the caller allocates the state and decides where it lives. The one-shot
*decode* functions remain, because the machine they run needs no window at
all when the whole output stays addressable in the caller's buffer — so a
PNG decode pays neither the window nor a copy into it.

The archive codecs take their working tables as workspace the caller hands
in, because their sizes depend on the stream:

| Codec | Decode | Encode |
|---|---|---|
| bzip2 | one `u32` a block symbol: up to 3.6 MB at level 9 (`decoder_work`), or for a stream known to decode to `len` bytes `block_symbols(len)`, five quarters of it; a block past the workspace is refused | five `u32` a symbol plus a byte block: up to 18.9 MB at level 9 |
| ARC methods | for 16-bit LZW, a 256 KiB table and a 64 KiB string stack (`DECODER_TABLE`, `DECODER_STACK`); narrower codes need less, and a squeezed member's tree 2 KiB (`SQUEEZE_TABLE`) | `encoder_work(len)`: just enough for every string `len` bytes can make, up to 1 MiB |

An ARC dictionary is cleared whole each time an encoder starts. Sizing it to
the member keeps that cost proportional to the member, so compressing a
thousand small files does not cost a thousand 1 MiB clears.

## Output sizing, and why it is a bound rather than a retry

The encoder writes straight into the caller's slice and cannot rewind, so
the caller sizes the destination with `bound(input_len)` and a shorter one is
refused before any state changes. This is the discipline
`max_compressed_len` already set for `RLZ1`, applied to a stream.

The bound holds because a block is never emitted larger than the bytes it
covers plus its header. A block's input span is capped below what a stored
block can carry, so the stored form is always available as a floor, and each
block goes out as whichever of stored, fixed-Huffman, and dynamic-Huffman is
smallest. Incompressible input therefore costs a header per block rather
than the eighth a Huffman-only encoder would add.

## Where it stands against zlib

Measured on one corpus against the reference implementation, so the ratios
rather than the absolute rates are the point.

The encoder produces zlib-level-6 output — within 0.2% on the same corpus —
at rather better than level 6's speed, which is where a full-window match
finder with a bounded chain walk and lazy matching should land.

The decoder is the other way round: roughly seven times slower than zlib's,
because it walks a canonical Huffman code one bit at a time where zlib reads
a multi-level lookup table. That is the strategy this module has always used
and it is what keeps every alphabet a plain fixed-size array. Closing it
means adding a derived lookup table for short codes, which would speed bulk
decoding several-fold and *slow* the small-block case a packetised protocol
lives in, since the table is rebuilt per block. Which of those matters is a
question for a profile of a real consumer, not for a guess, so the number is
recorded here rather than acted on.

## Robustness

Every entry point is `Result`-based and total. The crate is `no_std`, has no
dependencies, contains no `unsafe`, and has no `unwrap`/`expect`/`panic!` on
a production path: a malformed, truncated, or adversarial stream returns a
typed error. Output bounds are checked before any byte is produced.

Every decoder is attacker-reachable: a PNG from anywhere, an SSH peer's
traffic, an archive from a download. Each carries a fuzz harness in
`cargo xtask fuzz`: `fuzz_inflate`, `fuzz_zlib`, `fuzz_bzip2` and
`fuzz_arc`, alongside the `RLZ1` decoder's `fuzz_compress`. The bzip2 and
ARC harnesses also round-trip what their encoders write.

## Proven against the real thing

A codec tested only against itself agrees with its own bugs, so the DEFLATE
tests are pinned to streams a real zlib produced — fixed-Huffman,
dynamic-Huffman, stored, a 20 KiB-distance match, and an OpenSSH-shaped
sequence of `Z_PARTIAL_FLUSH` frames that each end mid-byte. The encode
direction was likewise verified by inflating its output under a real zlib.
The permanent in-tree gate is the fixture set plus the round-trip harnesses;
the end-to-end check against a pinned OpenSSH is `plans/SSH.md` S13.

The bzip2 tests decode streams the reference `bzip2` 1.0.8 wrote. Its
encoder's output passes `bzip2 -t`.

The ARC family has no reference encoder anyone still runs, so its tests
build their vectors with independent encoders written into the tests: RLE90
packing, a squeezer, and ARC 4's hashed crunching under both hashes. Out of
tree, the decoders were also run over more than ten thousand members of real
ARC, Spark and ArcFS archives. Every member they refused was refused by an
independent decoder too. That corpus is other people's work, so it is not
committed.

## Stability tier

`experimental`. See the crate's `README.md`.
