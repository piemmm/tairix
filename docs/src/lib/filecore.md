# `tairix-filecore`

`lib/filecore` is RISC OS FileCore's disc format. The [ADFS
driver](../filesystem/adfs.md), the volume probe (`lib/fsprobe`) and the [TAIRiX
Bridge](../desktop/tairix-bridge.md)'s FileCore and RamFS reach FileCore
discs here rather than each carrying a copy. Every access goes through the
`Disc` trait, whose failures pass through as `Error::Disc`; what the format
refuses is a `Fault`, one for each condition RISC OS reports apart.

| Module | What it is |
|---|---|
| `record` | The disc record (`DiscRecord`), its zones (`fit_zones`), the boot block, the checksums, the least fragment and the whole sectors an object is allocated (`allocation_bits`). |
| `oldmap`, `newmap` | Allocation in the old map's free-space list and the new map's zones and fragments. |
| `dir`, `bigdir` | The fixed and the big directories. |
| `format` | A new disc's layout (`new_map_record`, `lay_out`) for each `Variant`, the RAM disc's among them. |
| `identify` | Where a disc's structures lie (`Layout`). |
| `volume` | A disc's objects and directories (`Volume`), attached to a disc for one operation and detached between them (`Mount`). |

## Where objects go

What `volume` does with an object is what RISC OS 5's FileCore does, recorded
on the RAM disc:

- **Allocation** is in whole sectors, no fewer than a fragment takes: a
  2,100-byte file on a disc of 1K sectors takes 3K.
- **Sharing.** An object whose whole share granules come short of the least
  fragment shares one (`place_object`): room in a fragment the directory's
  objects share, else a new fragment of the least size from its start. An
  empty object has no fragment and is addressed `EMPTY_OBJECT`.
- **Growing an open file** (`growth_base`, `grow_object`): past its
  allocation, it takes every byte the disc has free. One with a fragment of
  its own, or alone in a shared fragment from its start, grows within it; one
  sharing with others moves to a fragment of its own.
- **Settling a closed file** (`settle_object`): an empty one holds nothing; a
  small one moves into the directory's shared room, else keeps the least of
  its own fragment shared from its start; a larger one is cut to its extent;
  one already shared stays.
- **Moving** a file to another directory (`copy_for_move`): a file sharing its
  fragment with others takes a new fragment of its own, since FileCore shares
  a fragment within one directory only; a file alone in its fragment takes
  the fragment along.

## Layouts

`format` lays out each disc FileCore makes, floppy and hard disc, as RISC OS
lays them out. The RAM disc's variant is RISC OS 5's RamFS's, bit for bit at
every size recorded: 128-byte map bits and 1K sectors, one zone up to 512K
with the map at the disc's start, and from 1M a hard disc's layout, its
record in the boot block and its map in the middle zone.
