# tairix-fat

The FAT file system's on-disk format — FAT12, FAT16 and FAT32, with the VFAT
long-name extension — as Microsoft's FAT specification defines it. The FAT32
driver, the volume probe, the partition writer's FAT types, the metadata
registry's Atari attributes and the test images built for them all read and
write FAT through it, so the format has one definition.

- `boot` — the boot sector: its parameter block parsed and checked, the kind
  told by the block's shape (FAT32) or the clusters it counts (FAT12, FAT16),
  and where each region lies. A volume's clusters are bounded by its sectors,
  its table and its kind, and `clusters_within` bounds them by a device too.
- `table` — allocation-table entries of 12, 16 or 28 bits, read and written
  through the bytes they span so a caller holding a sector of the table
  reaches them as well as one holding all of it.
- `dir` — short entries and their field map, long-name entries, the checksum
  that binds them, a long name reassembled whole or not at all, and the
  UTF-16 a long name is spelt in.
- `name` — 8.3 names, the `~N` aliases a long name is given, and labels.
- `stamp` — an entry's creation, access and write stamps over `lib/abi`'s
  `DosTime`, read and written as UTC.
- `fsinfo` — FAT32's free-space hint sector.
- `format` — a new volume's parameters (kind by size, cluster size, a table
  sized exactly), its boot sector, and what each sector of its system area
  holds, the boot sector meant to be written last.

`no_std`, allocation-free, with no `unsafe`. Every decoder is total: a
structure that breaks the format is refused, never trusted. The decoders have
a fuzz target (`tests/fuzz_fat.rs`).

## Stability

Tier: `experimental`. The format is fixed; the Rust API may change while
TAIRiX is pre-release.
