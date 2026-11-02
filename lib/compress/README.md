# tairix-compress

The single shared **compression codec** for TAIRiX (`AGENTS.md` §6, §16.4 —
compression is a curated shared-library class). It holds the first-party
`RLZ1` LZ77 codec TAIRiX's own storage uses, and the foreign codecs other
software needs, each in both directions: RFC 1951 DEFLATE / RFC 1950 zlib,
bzip2, and the ARC family's methods. PNG's `IDAT` stream (`lib/image`) is
read with DEFLATE and SSH's `zlib@openssh.com` (`plans/SSH.md`) written with
it; the archive driver (`drivers/filesystem/archive`) reads and writes gzip,
bzip2, zip, Spark and ArcFS members with all three.

- `compress(src, dst) -> Result<usize, Error>` — a single greedy LZ77 pass
  over a small hash table of recent 4-byte sequences. Writes into a
  caller-provided slice and returns the compressed length.
- `decompress(src, dst) -> Result<usize, Error>` — a tight literal-copy /
  match-copy loop. The declared output length is bounds-checked against `dst`
  before any byte is produced, and every back-reference is validated against
  the bytes produced so far.
- `max_compressed_len(input_len) -> usize` — a true upper bound on the
  compressed size, so a caller can size a scratch buffer that never provokes a
  spurious `Error::TooSmall`.

The wire frame is `"RLZ1"` magic, a little-endian `u32` uncompressed length,
then LZ4-style token sequences (a literal-run/match-length nibble token,
0xFF-continuation length extensions, the literal bytes, and a `u16`
back-reference offset). There is no entropy-coding stage, so the codec is fast
and predictable rather than maximally dense.

## Foreign-format interoperability: `deflate`, `inflate`, and `zlib`

Three further modules speak the format the rest of the world already
speaks. They never replace the `RLZ1` codec above; they exist so TAIRiX can
read what a foreign encoder wrote and write what a foreign decoder reads.

A stream that is present whole goes through a plain function:

- `inflate::inflate_into(src, dst) -> Result<usize, Error>` — a complete RFC
  1951 DEFLATE decompressor: stored, fixed-Huffman, and dynamic-Huffman
  blocks, canonical Huffman tables built by the reference count/offset walk
  (a real table-driven decode, not a linear scan over every code), and
  overlapping back-references copied byte-by-byte.
- `inflate::inflate_prefix(src, dst) -> Result<usize, Error>` — the opening
  `dst.len()` bytes of a stream, decoding no further: a header read out of a
  compressed member costs the header, not the member.
- `zlib::decompress_into(src, dst) -> Result<usize, Error>` — the RFC 1950
  envelope: header validation (compression method, window size, header
  check, refusing a preset dictionary), the wrapped `inflate` body, and the
  trailing Adler-32 verified over exactly the bytes produced.

A stream that arrives or leaves in pieces goes through a caller-owned state
value, because a protocol keeps one stream per direction for a whole session
and flushes it per packet — later packets still back-reference earlier ones:

- `deflate::Deflate` / `zlib::Encoder` — `deflate(src, dst, flush)` and
  `compress(src, dst, flush)`, with `Flush::Sync` making everything fed so
  far readable by the peer and `Flush::Finish` ending the stream. Size `dst`
  with the matching `bound(input_len)`; a shorter one is refused before any
  state changes. Each block is emitted stored, fixed-Huffman, or
  dynamic-Huffman — whichever is smallest — so incompressible input is never
  expanded by more than a block header.
- `inflate::Inflater` / `zlib::Decoder` — resumable at any bit, which a peer
  flushing with zlib's `Z_PARTIAL_FLUSH` requires, and absorbing every byte
  handed to them so a caller never carries a remainder forward.
- `inflate::Checkpoint` — where an `Inflater` stood at a block's start: its
  held bits and its `WINDOW_SIZE` of history. `inflate_by_block` stops at
  each block so `checkpoint` can record one, `resume` stands there again,
  and `after_sync` records the block an encoder begins after `Flush::Sync`,
  so a reader seeks into a long stream without decoding it from the start.

The state values are large by nature: a decoder carries the 32 KiB history a
back-reference may reach into (~34 KiB), and an encoder additionally carries
the match finder (~220 KiB). Heap-own one; that is why there is no one-shot
*encode* function to put one on a stack by accident.

## Archive formats: `bzip2` and `arc`

- `bzip2::Decoder` / `bzip2::Encoder` — the bzip2 stream, levels 1 to 9.
  Every block's CRC and the stream's combined CRC are checked. The decoder
  reports the bit each block began at (`Next::Block { at }`) and can
  `resume` at one; the encoder reports each block's start through
  `BlockIndex`. A reader or writer can therefore index a stream and later
  seek into it.
- `arc::Decoder` — every method ARC, Spark and ArcFS members use: RLE90
  packing, squeezing, ARC 4's hashed 12-bit crunching (methods 5 to 7), and
  the compress-style dynamic LZW of 9 to 16 bits. `arc::Encoder` writes the
  dynamic LZW, with a dictionary `encoder_work` sizes to its input.

Both stream through the crate-root `Input` (a byte at a time) and `Output`
traits, over workspace the caller places. That workspace is large: a bzip2
block decodes through four bytes a symbol (3.6 MB at level 9) and encodes
through twenty (18 MB), and an ARC table is up to 256 KiB to decode and
1 MiB to encode. None of it belongs on a stack.

Every module keeps the crate's zero-`unsafe`, no-panic, fail-closed
discipline and allocates nothing of its own; see their rustdoc for the full
error taxonomy.

## Why it lives in `lib/`

ARXFS compresses every file-data record before sealing it
(`docs/src/filesystem/arxfs-spec.md` §6, §10), and `AGENTS.md` §16.4 lists
compression among the curated OS-provided shared-library classes, so the codec
belongs in `lib/*` (§6) rather than buried in the filesystem driver. It is
written first-party because `AGENTS.md` §2.12 — *roll your own; do not trust
external code* — bars an external `zstd`/`lz4`/compression dependency. This is
**not** the crypto carve-out (§2.12): cryptography uses audited `lib/crypto`
primitives, but compression is ours.

The crate has no dependencies and sits at the bottom of the §17.4 layering: it
is depended on, never depends.

## Stability tier

`experimental` — the Stage 6 ARXFS compression seam
(`docs/src/filesystem/arxfs-spec.md` §15.6, §18). It is `no_std`, performs no
allocation (it works through caller-provided slices, caller-owned state
values and workspace, and fixed-size internal tables, in every module), and
has no dependencies. No `unsafe`,
and no `unwrap`/`expect`/`panic!` in production paths: every entry point is
`Result`-based and total, and malformed compressed input returns a typed
error rather than panicking (`AGENTS.md` §2.9). A future on-disk format
version may switch the codec globally; the `RLZ1` frame magic carries the
version.
