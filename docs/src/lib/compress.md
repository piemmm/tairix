# `tairix-compress`

The one home for lossless compression in TAIRiX. Its codecs do not compete:
the first-party `RLZ1` codec is what TAIRiX's own storage uses, and the
foreign codecs — RFC 1951 / RFC 1950 DEFLATE and zlib, the RFC 1952 gzip
member read over DEFLATE, bzip2, the ARC family's methods, and RISC OS's
Squash — are what other software already speaks.

The crate needs no `alloc` and allocates nothing, and its one dependency,
`lib/crc32`, the checksum gzip's trailer is held to, needs neither. Its
consumers reach from the kernel heap's compressed-page store down through a
filesystem driver to a desktop image decoder, so a heap requirement would be
one imposed on all of them by the least constrained.

## Why more than one codec

`RLZ1` exists because `AGENTS.md` §2.12 bars an external `zstd`/`lz4`
dependency and ARXFS needs a low-CPU codec on its record path
(`docs/src/filesystem/arxfs-spec.md` §10). It is a single greedy LZ77 pass
over a small hash table, with no entropy stage: fast and predictable rather
than dense. A frame decodes only if it ends exactly where its encoder ended
it, and `decompressed_len` refuses a declared length its bytes could not
expand to, so a decoder of a stranger's frame sizes its buffer before
allocating it.

DEFLATE exists because two foreign contracts are written in it and neither
is negotiable. A PNG's `IDAT` stream is zlib-wrapped DEFLATE, so `lib/image`
must *read* it. SSH's `zlib@openssh.com` compresses a session's traffic as
one zlib stream per direction, so `sshd` and `ssh` must *write* one a stock
OpenSSH peer reads (`plans/SSH.md`). Neither codec is a fallback for the
other: a TAIRiX-native format never stores DEFLATE, and a foreign format is
never handed `RLZ1`.

bzip2, the ARC methods and Squash exist for the same reason: archives were
written in them long before TAIRiX. The archive driver
(`docs/src/filesystem/archive.md`) reads `.tar.bz2` and `.bz2` through bzip2,
ARC, Spark and ArcFS members through the ARC methods, and a Squash file
through Squash. It writes them all back, because changing an archive means
compressing what changed the way the archive's own tools would. TAIRiX
Bridge's Squash module ([TAIRiX Bridge](../desktop/tairix-bridge.md)) answers
a RISC OS program's `Squash_Compress` and `Squash_Decompress` through the
same Squash, so a stream either writes the other reads.

## The two shapes, and why both exist

| Shape | Entry point | For |
|---|---|---|
| Whole stream | `compress` / `decompress` / `decompressed_len`, `inflate::inflate_into`, `inflate::inflate_prefix`, `zlib::decompress_into`, `gzip::Header::parse`, `gzip::decompress` | data that is present in one buffer: a filesystem record, a PNG `IDAT`, the header of a compressed archive member, a gzip stream of one member or several |
| Streaming | `deflate::Deflate`, `inflate::Inflater`, `zlib::Encoder`, `zlib::Decoder`, `gzip::Member`, `bzip2::Decoder`, `bzip2::Encoder`, `arc::Decoder`, `arc::Encoder`, `squash::compress`, `squash::decompress` | a conversation, one stream per direction flushed per message; or an archive member read from, or written to, the middle of a file larger than memory |

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

Squash streams over slices with nothing held anywhere but the caller's
workspace, because that is the contract of the RISC OS calls it answers: a
program hands `Squash_Compress` a workspace, input and room, and calls again
with the same workspace to carry on, so the workspace is the whole of a
stream's state. Each call saves the coder's few bytes of state there beside
its tables and restores them at the next, and the state and tables are held
to what a coder could have left as they are read, since a program may have
written over them between calls: a workspace a stranger wrote is refused,
never followed.

## One LZW, two sets of rules

The ARC family's dynamic LZW and Squash are one coding, Unix `compress`'s,
and one coder (`lzw`) writes and reads both. They differ in two rules, which
the coder is told rather than written twice: when a full table is cleared —
`compress`'s once a check finds it compressing worse, Squash's on a fixed
schedule — and whether a stream's last code may be read one bit short, as
Squash's decoder reads one. Squash itself has two compressors, and which one
a stream gets is the first call's to decide: a whole input with room for all
it can make is compressed at once, clearing the table as its last code would
be assigned; anything else clears a full table every 32 KiB of input. A
stream's codes do not depend on how its calls split it, so both are
reproduced byte for byte, call by call.

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
- A Squash stream read on from where its table was cleared needs nothing
  before it. `squash::compress` and `squash::decompress` tell their caller
  each such place as they pass it, and `start_decompression_at_place` starts
  a decoder there.

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
| ARC methods | for 16-bit LZW, 255 KiB of table and string stack (`DECODER_STACK`); ARC 4's crunching, its 32 KiB table (`DECODER_TABLE`); narrower codes need less, and a squeezed member's tree 2 KiB (`decoder_workspace` says which) | `encoder_work(max_bits, len)`: just enough for every string `len` bytes can make, up to 447 KiB for 16-bit codes |
| Squash | `DECOMPRESS_WORK`, 17 KiB | `COMPRESS_WORK`, 31 KiB |

An LZW dictionary is cleared whole each time an encoder starts. Sizing it to
the member keeps that cost proportional to the member, so compressing a
thousand small files does not cost a thousand full-size clears. Squash's
workspaces are the sizes the Squash module asks for, so a RISC OS program's
workspace holds what TAIRiX's coder keeps.

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
traffic, an archive from a download, a RISC OS program's workspace. Each
carries a fuzz harness in `cargo xtask fuzz`: `fuzz_inflate`, `fuzz_zlib`,
`fuzz_bzip2`, `fuzz_arc` and `fuzz_squash`, alongside the `RLZ1` decoder's
`fuzz_compress`. The bzip2, ARC and Squash harnesses also round-trip what
their encoders write, and Squash's scribbles over a workspace between calls.

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

Squash is pinned against RISC OS 5.30's Squash module, observed from outside
under an emulator: the streams both its compressors wrote, by length and
CRC-32, every one of 1,307 recorded calls' input taken, output made and
status, and its answers at the edges. The inputs are regenerated in the
tests; nothing the module wrote but those figures is committed.

## Stability tier

`experimental`. See the crate's `README.md`.
