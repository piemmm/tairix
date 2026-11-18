# tairix-dfs

The Acorn DFS on-disc format, as DFS 1.20 writes it. The volume probe, the
archive driver that serves a DFS disc image as a volume, and the Open Micro
emulator's DFS ROM and blank discs all read and write DFS through it, so the
format has one definition.

- `catalogue` — the two sectors that open each side: the title, the cycle
  number, the boot option, the side's sector count and its 31 entries, each a
  name, a directory, a lock and three 18-bit fields (load, execution, length)
  with a 10-bit start sector. `Catalogue::read` believes a catalogue only
  where every field is one a DFS disc could hold.
- `name` — the bytes a name DFS makes may hold, and a name's padded spelling.
- `image` — where a side's bytes lie in an image: an SSD's tracks one after
  another, a DSD's two sides alternating track by track.

`no_std`, allocation-free, with no `unsafe`. Every decoder is total: a
catalogue that breaks the format is refused, never trusted. The decoders have a
fuzz target (`tests/fuzz_dfs.rs`).

## Stability

Tier: `experimental`. The format is fixed; the Rust API may change while
TAIRiX is pre-release.
